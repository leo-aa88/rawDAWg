//! One polyphonic voice: layered oscillators → stereo pan → amp envelope → filter.

use crate::patch::{Generator, MAX_LAYERS, Patch};
use crate::registry::NoteKey;
use rawdawg_dsp::envelope::{Adsr, AdsrParams, EnvStage};
use rawdawg_dsp::filter::Svf;
use rawdawg_dsp::oscillator::{BasicOscillator, BasicTables, BasicWave};
use rawdawg_dsp::smooth::Smoother;
use rawdawg_dsp::wavetable::MipTable;

/// Control-rate interval in samples for pitch/filter coefficient updates.
pub const CONTROL_INTERVAL: usize = 16;

/// Prepared per-layer engine data shared by all voices (owned by the synth).
#[derive(Debug)]
pub struct LayerEngine {
    pub enabled: bool,
    pub kind: LayerKind,
    pub gain: f32,
    pub pan_l: f32,
    pub pan_r: f32,
    pub semitone_offset: f64,
    pub reset_phase: bool,
    /// Previous table during a crossfade (retired when the fade completes).
    pub prev_table: Option<Box<MipTable>>,
    /// Crossfade position 0..1 (1 = fully on the new table) and per-sample step.
    pub xfade: f32,
    pub xfade_step: f32,
}

#[derive(Debug)]
pub enum LayerKind {
    Basic {
        wave: BasicWave,
        pulse_width: f64,
    },
    /// Prepared table built from the layer's editable cycle.
    Table(Box<MipTable>),
}

/// Table swap crossfade length (DSP.md: 5–10 ms).
pub const TABLE_XFADE_S: f32 = 0.008;

impl LayerEngine {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            kind: LayerKind::Basic { wave: BasicWave::Sine, pulse_width: 0.5 },
            gain: 0.0,
            pan_l: 0.0,
            pan_r: 0.0,
            semitone_offset: 0.0,
            reset_phase: true,
            prev_table: None,
            xfade: 1.0,
            xfade_step: 0.0,
        }
    }

    /// Non-RT: build engine data for a patch layer (allocates for tables).
    pub fn build(layer: &crate::patch::Layer) -> Self {
        let kind = match &layer.generator {
            Generator::Basic { wave, pulse_width } => LayerKind::Basic { wave: *wave, pulse_width: f64::from(*pulse_width) },
            Generator::Wavetable { cycle, remove_dc } => {
                let mut b = rawdawg_dsp::wavetable::TableBuilder::new();
                LayerKind::Table(Box::new(b.build(cycle, *remove_dc)))
            }
        };
        let mut e = Self {
            enabled: layer.enabled,
            kind,
            gain: 0.0,
            pan_l: 0.0,
            pan_r: 0.0,
            semitone_offset: 0.0,
            reset_phase: layer.reset_phase,
            prev_table: None,
            xfade: 1.0,
            xfade_step: 0.0,
        };
        e.update_mix(layer);
        e
    }

    /// RT: install a new table, starting a crossfade from the current one. Returns a
    /// table that must be retired now (a previous crossfade that was still running).
    pub fn install_table(&mut self, table: Box<MipTable>, sample_rate: f32) -> Option<Box<MipTable>> {
        let leftover = self.prev_table.take();
        let old = std::mem::replace(&mut self.kind, LayerKind::Table(table));
        if let LayerKind::Table(t) = old {
            self.prev_table = Some(t);
            self.xfade = 0.0;
            self.xfade_step = 1.0 / (TABLE_XFADE_S * sample_rate).max(1.0);
        } else {
            self.xfade = 1.0;
            self.xfade_step = 0.0;
        }
        leftover
    }

    /// RT: advance the crossfade by one sample; returns a finished previous table.
    #[inline]
    pub fn tick_xfade(&mut self) -> Option<Box<MipTable>> {
        if self.prev_table.is_some() {
            self.xfade += self.xfade_step;
            if self.xfade >= 1.0 {
                self.xfade = 1.0;
                return self.prev_table.take();
            }
        }
        None
    }

    /// RT: read the layer's table(s) at phase, honoring a running crossfade.
    #[inline]
    pub fn read_table(&self, level: usize, phase: f64) -> f32 {
        match (&self.kind, &self.prev_table) {
            (LayerKind::Table(t), Some(p)) if self.xfade < 1.0 => {
                let a = p.read(level, phase);
                let b = t.read(level, phase);
                a + (b - a) * self.xfade
            }
            (LayerKind::Table(t), _) => t.read(level, phase),
            _ => 0.0,
        }
    }

    /// RT-safe: refresh gain/pan/tuning from the layer settings.
    pub fn update_mix(&mut self, layer: &crate::patch::Layer) {
        self.enabled = layer.enabled;
        self.gain = rawdawg_dsp::math::db_to_lin(layer.level_db);
        let (l, r) = rawdawg_types::units::Pan(f64::from(layer.pan)).equal_power_gains();
        self.pan_l = l;
        self.pan_r = r;
        self.semitone_offset = f64::from(layer.coarse_semitones) + f64::from(layer.fine_cents) / 100.0;
        self.reset_phase = layer.reset_phase;
        if let (LayerKind::Basic { wave, pulse_width }, Generator::Basic { wave: w, pulse_width: pw }) = (&mut self.kind, &layer.generator) {
            *wave = *w;
            *pulse_width = f64::from(*pw);
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct LayerVoice {
    osc: BasicOscillator,
    level: usize,
}

/// A voice slot.
#[derive(Clone, Debug)]
pub struct Voice {
    pub key: Option<NoteKey>,
    /// Monotonic allocation stamp (older = smaller).
    pub age: u64,
    pub released: bool,
    /// Note-off arrived while sustain was down; release when pedal lifts.
    pub sustained: bool,
    pub velocity: f32,
    layers: [LayerVoice; MAX_LAYERS],
    amp: Adsr,
    fenv: Adsr,
    filter_l: Svf,
    filter_r: Svf,
    /// Current (gliding) pitch in MIDI semitones including transpose/detune.
    pitch: Smoother,
    bend_semitones: f32,
    sample_rate: f64,
    control_counter: usize,
}

impl Voice {
    pub fn new(sample_rate: f64, patch: &Patch) -> Self {
        let sr = sample_rate as f32;
        let lv = LayerVoice { osc: BasicOscillator::new(sample_rate), level: 0 };
        Self {
            key: None,
            age: 0,
            released: false,
            sustained: false,
            velocity: 0.0,
            layers: [lv; MAX_LAYERS],
            amp: Adsr::new(sr, patch.amp_env),
            fenv: Adsr::new(sr, patch.filter_env),
            filter_l: Svf::new(sr),
            filter_r: Svf::new(sr),
            pitch: Smoother::new(60.0, 0.0, sr),
            bend_semitones: 0.0,
            sample_rate,
            control_counter: 0,
        }
    }

    pub fn is_active(&self) -> bool {
        self.key.is_some()
    }
    pub fn is_fading(&self) -> bool {
        self.amp.stage() == EnvStage::Fade
    }
    pub fn amp_level(&self) -> f32 {
        self.amp.level()
    }

    pub fn set_envelopes(&mut self, amp: AdsrParams, fenv: AdsrParams) {
        self.amp.set_params(amp);
        self.fenv.set_params(fenv);
    }

    /// Start a note. With `retrigger == false` the current pitch glides to the target (legato).
    #[allow(clippy::too_many_arguments)]
    pub fn note_on(&mut self, key: NoteKey, velocity: f32, age: u64, target_pitch: f32, portamento_s: f32, retrigger: bool, layers: &[LayerEngine; MAX_LAYERS]) {
        self.key = Some(key);
        self.age = age;
        self.released = false;
        self.sustained = false;
        self.velocity = velocity;
        let sr = self.sample_rate as f32;
        if retrigger {
            if portamento_s > 0.0 && self.amp.is_active() {
                self.pitch.set_time(portamento_s / 3.0, sr);
                self.pitch.set_target(target_pitch);
            } else {
                self.pitch.snap(target_pitch);
            }
            self.amp.note_on();
            self.fenv.note_on();
            for (lv, le) in self.layers.iter_mut().zip(layers.iter()) {
                if le.reset_phase {
                    lv.osc.reset_phase(0.0);
                }
            }
        } else {
            // legato: glide without retriggering envelopes
            self.pitch.set_time(portamento_s / 3.0, sr);
            self.pitch.set_target(target_pitch);
            if !self.amp.is_active() {
                self.amp.note_on();
                self.fenv.note_on();
            }
        }
        self.control_counter = 0;
    }

    pub fn note_off(&mut self) {
        self.released = true;
        self.sustained = false;
        self.amp.note_off();
        self.fenv.note_off();
    }

    /// Fast fade for stealing.
    pub fn steal(&mut self) {
        self.released = true;
        self.sustained = false;
        self.amp.fade_out();
        self.fenv.fade_out();
    }

    pub fn kill(&mut self) {
        self.key = None;
        self.released = false;
        self.sustained = false;
        self.amp.reset();
        self.fenv.reset();
        self.filter_l.reset();
        self.filter_r.reset();
    }

    pub fn set_bend(&mut self, semitones: f32) {
        self.bend_semitones = semitones;
    }

    pub fn set_sample_rate(&mut self, sr: f64) {
        self.sample_rate = sr;
        self.amp.set_sample_rate(sr as f32);
        self.fenv.set_sample_rate(sr as f32);
        self.filter_l.set_sample_rate(sr as f32);
        self.filter_r.set_sample_rate(sr as f32);
        for l in &mut self.layers {
            l.osc.set_sample_rate(sr);
        }
    }

    fn update_control(&mut self, patch: &Patch, layers: &[LayerEngine; MAX_LAYERS]) {
        let p = f64::from(self.pitch.current()) + f64::from(self.bend_semitones);
        let a4 = f64::from(patch.tuning.a4_hz);
        for (lv, le) in self.layers.iter_mut().zip(layers.iter()) {
            if !le.enabled {
                continue;
            }
            let hz = a4 * 2f64.powf((p + le.semitone_offset - 69.0) / 12.0);
            lv.osc.set_frequency(hz);
            lv.level = MipTable::level_for_increment(lv.osc.increment());
            if let LayerKind::Basic { wave, pulse_width } = &le.kind {
                lv.osc.set_wave(*wave);
                lv.osc.set_pulse_width(*pulse_width);
            }
        }
        if patch.filter.enabled {
            let f = &patch.filter;
            let env = self.fenv.level() * f.env_octaves;
            let key = f.key_track * (self.pitch.current() - 60.0) / 12.0;
            let cutoff = f.cutoff_hz * 2f32.powf(env + key);
            self.filter_l.set_mode(f.mode);
            self.filter_r.set_mode(f.mode);
            self.filter_l.set(cutoff, f.resonance);
            self.filter_r.set(cutoff, f.resonance);
        }
    }

    /// Render `out_l.len()` samples, **adding** into the outputs. Returns `false`
    /// when the voice finished (envelope idle) and the slot can be freed.
    #[inline(never)]
    pub fn render_add(&mut self, out_l: &mut [f32], out_r: &mut [f32], patch: &Patch, layers: &[LayerEngine; MAX_LAYERS], tables: &BasicTables) -> bool {
        if self.key.is_none() {
            return false;
        }
        let vel_gain = 1.0 - patch.velocity_sensitivity * (1.0 - self.velocity);
        let use_filter = patch.filter.enabled;
        for i in 0..out_l.len() {
            if self.control_counter == 0 {
                self.update_control(patch, layers);
            }
            self.control_counter += 1;
            if self.control_counter >= CONTROL_INTERVAL {
                self.control_counter = 0;
            }
            self.pitch.next();
            let env = self.amp.next();
            self.fenv.next();
            if !self.amp.is_active() {
                self.kill();
                return false;
            }
            let mut l = 0.0f32;
            let mut r = 0.0f32;
            for (lv, le) in self.layers.iter_mut().zip(layers.iter()) {
                if !le.enabled {
                    continue;
                }
                let s = match &le.kind {
                    LayerKind::Basic { .. } => lv.osc.next(tables),
                    LayerKind::Table(_) => {
                        let v = le.read_table(lv.level, lv.osc.phase());
                        lv.osc.advance();
                        v
                    }
                } * le.gain;
                l += s * le.pan_l;
                r += s * le.pan_r;
            }
            if use_filter {
                l = self.filter_l.process(l);
                r = self.filter_r.process(r);
            }
            let g = env * vel_gain;
            out_l[i] += l * g;
            out_r[i] += r * g;
        }
        true
    }
}

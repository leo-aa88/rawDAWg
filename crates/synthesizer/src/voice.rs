//! One polyphonic voice: per-layer generators (with unison) → stereo pan →
//! amp envelope → filter, with control-rate modulation.

use crate::generators::{AdditiveState, FmState, GranularState, PluckState, read_frames, unison_detune, unison_gain, unison_pan};
use crate::graph::{CompiledGraph, GraphState, Sources, Workspace};
use crate::modulation::{LfoState, ModInputs, ModOutputs};
use crate::patch::{Generator, Lfo, MAX_LAYERS, MAX_LFOS, MAX_UNISON, ModTarget, Patch};
use crate::registry::NoteKey;
use rawdawg_dsp::envelope::{Adsr, AdsrParams, EnvStage};
use rawdawg_dsp::filter::Svf;
use rawdawg_dsp::oscillator::{BasicOscillator, BasicTables, BasicWave};
use rawdawg_dsp::rng::DeterministicRng;
use rawdawg_dsp::smooth::Smoother;
use rawdawg_dsp::wavetable::{MipTable, TableBuilder};
use rawdawg_types::events::NoteExpression;

/// Control-rate interval in samples for pitch/filter/modulation updates.
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
    pub unison: usize,
    pub unison_detune_cents: f32,
    pub unison_spread: f32,
    /// Previous frame table during a crossfade (capacity 1, allocated once; drained
    /// into the retire queue when the fade completes).
    pub prev_frames: Vec<Box<MipTable>>,
    pub fading: bool,
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
    /// Motion frames built from the layer's editable cycles.
    Table {
        frames: Vec<Box<MipTable>>,
        position: f32,
    },
    Fm(crate::patch::FmSettings),
    /// Harmonic mode renders through `table`; detuned mode uses the per-voice bank.
    Additive {
        settings: crate::patch::AdditiveSettings,
        table: Box<MipTable>,
    },
    Granular {
        settings: crate::patch::GranularSettings,
        source: Box<MipTable>,
    },
    Pluck(crate::patch::PluckSettings),
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
            unison: 1,
            unison_detune_cents: 0.0,
            unison_spread: 0.0,
            prev_frames: Vec::with_capacity(1),
            fading: false,
            xfade: 1.0,
            xfade_step: 0.0,
        }
    }

    /// Non-RT: build engine data for a patch layer (allocates for tables).
    pub fn build(layer: &crate::patch::Layer) -> Self {
        let mut b = TableBuilder::new();
        let kind = match &layer.generator {
            Generator::Basic { wave, pulse_width } => LayerKind::Basic { wave: *wave, pulse_width: f64::from(*pulse_width) },
            Generator::Wavetable { remove_dc, position, .. } => {
                let frames = layer.generator.frames().iter().map(|c| Box::new(b.build(&normalized_cycle(c), *remove_dc))).collect();
                LayerKind::Table { frames, position: *position }
            }
            Generator::Fm(s) => LayerKind::Fm(*s),
            Generator::Additive(s) => {
                let cycle = crate::sculpt::synthesize(&crate::sculpt::Harmonics { magnitude: pad_dc(&s.amplitudes), phase: pad_dc(&s.phases) });
                LayerKind::Additive { settings: s.clone(), table: Box::new(b.build(&normalized_cycle(&cycle), true)) }
            }
            Generator::Granular { settings, source } => LayerKind::Granular { settings: *settings, source: Box::new(b.build(&normalized_cycle(source), true)) },
            Generator::Pluck(s) => LayerKind::Pluck(*s),
        };
        let mut e = Self {
            enabled: layer.enabled,
            kind,
            gain: 0.0,
            pan_l: 0.0,
            pan_r: 0.0,
            semitone_offset: 0.0,
            reset_phase: layer.reset_phase,
            unison: 1,
            unison_detune_cents: 0.0,
            unison_spread: 0.0,
            prev_frames: Vec::with_capacity(1),
            fading: false,
            xfade: 1.0,
            xfade_step: 0.0,
        };
        e.update_mix(layer);
        e
    }

    /// RT-safe: refresh gain/pan/tuning/unison/generator scalars from the layer settings.
    pub fn update_mix(&mut self, layer: &crate::patch::Layer) {
        self.enabled = layer.enabled;
        self.gain = rawdawg_dsp::math::db_to_lin(layer.level_db);
        let (l, r) = rawdawg_types::units::Pan(f64::from(layer.pan)).equal_power_gains();
        self.pan_l = l;
        self.pan_r = r;
        self.semitone_offset = f64::from(layer.coarse_semitones) + f64::from(layer.fine_cents) / 100.0;
        self.reset_phase = layer.reset_phase;
        self.unison = usize::from(layer.unison).clamp(1, MAX_UNISON);
        self.unison_detune_cents = layer.unison_detune_cents;
        self.unison_spread = layer.unison_spread;
        match (&mut self.kind, &layer.generator) {
            (LayerKind::Basic { wave, pulse_width }, Generator::Basic { wave: w, pulse_width: pw }) => {
                *wave = *w;
                *pulse_width = f64::from(*pw);
            }
            (LayerKind::Table { position, .. }, Generator::Wavetable { position: p, .. }) => *position = *p,
            (LayerKind::Fm(s), Generator::Fm(p)) => *s = *p,
            (LayerKind::Additive { settings, .. }, Generator::Additive(p)) => {
                settings.stretch = p.stretch;
                settings.detuned = p.detuned;
            }
            (LayerKind::Granular { settings, .. }, Generator::Granular { settings: p, .. }) => *settings = *p,
            (LayerKind::Pluck(s), Generator::Pluck(p)) => *s = *p,
            _ => {}
        }
    }

    /// RT: install a new table for `frame`, starting a crossfade. Returns tables that
    /// must be retired now (a previous crossfade still running, or a replaced table
    /// when the layer is not a wavetable).
    pub fn install_table(&mut self, frame: usize, table: Box<MipTable>, sample_rate: f32, retire: &mut dyn FnMut(Box<MipTable>)) {
        match &mut self.kind {
            LayerKind::Table { frames, .. } => {
                // a fade still running: retire its source first (capacity stays allocated)
                while let Some(t) = self.prev_frames.pop() {
                    retire(t);
                }
                if frames.is_empty() {
                    retire(table);
                    return;
                }
                let f = frame.min(frames.len() - 1);
                // swap the edited frame in place (no allocation) and keep the old one for the fade
                let replaced = std::mem::replace(&mut frames[f], table);
                if self.prev_frames.capacity() == 0 {
                    retire(replaced); // cannot hold it without allocating: hard switch
                    self.fading = false;
                    return;
                }
                self.prev_frames.push(replaced);
                self.fading = true;
                self.xfade = 0.0;
                self.xfade_step = 1.0 / (TABLE_XFADE_S * sample_rate).max(1.0);
            }
            LayerKind::Additive { table: t, .. } | LayerKind::Granular { source: t, .. } => {
                let old = std::mem::replace(t, table);
                retire(old);
            }
            _ => retire(table),
        }
    }

    /// RT: advance the crossfade by `samples`; finished sources go to `retire`.
    #[inline]
    pub fn tick_xfade(&mut self, samples: usize, retire: &mut dyn FnMut(Box<MipTable>)) {
        if self.fading {
            self.xfade += self.xfade_step * samples as f32;
            if self.xfade >= 1.0 {
                self.xfade = 1.0;
                self.fading = false;
                while let Some(t) = self.prev_frames.pop() {
                    retire(t);
                }
            }
        }
    }

    /// RT: read the layer's frames at phase/position, honoring a running crossfade.
    #[inline]
    pub fn read_table(&self, level: usize, phase: f64, position: f32) -> f32 {
        let LayerKind::Table { frames, .. } = &self.kind else { return 0.0 };
        let v = read_frames(frames, level, phase, position);
        if self.fading
            && let Some(prev) = self.prev_frames.first()
        {
            let a = prev.read(level, phase);
            a + (v - a) * self.xfade
        } else {
            v
        }
    }
}

fn pad_dc(v: &[f32]) -> Vec<f32> {
    let mut out = Vec::with_capacity(rawdawg_dsp::wavetable::CYCLE_LEN / 2 + 1);
    out.push(0.0);
    out.extend_from_slice(v);
    out.resize(rawdawg_dsp::wavetable::CYCLE_LEN / 2 + 1, 0.0);
    out
}

/// Peak guard: cycles above full scale are scaled down for playback; the stored
/// cycle stays editable (reversible representation).
fn normalized_cycle(c: &[f32]) -> Vec<f32> {
    let peak = rawdawg_dsp::math::peak(c);
    if peak > 1.0 { c.iter().map(|s| s / peak).collect() } else { c.to_vec() }
}

#[derive(Clone, Debug)]
struct LayerVoice {
    osc: [BasicOscillator; MAX_UNISON],
    level: usize,
    fm: [FmState; MAX_UNISON],
    additive: AdditiveState,
    granular: GranularState,
    pluck: PluckState,
}

/// Rate and full depth of the vibrato a note's vibrato expression adds.
const VIBRATO_HZ: f32 = 5.5;
const VIBRATO_SEMITONES: f32 = 0.5;
/// Time a full-scale change of a note's volume, expression or pan takes.
const EXPRESSION_GLIDE_S: f32 = 0.005;
/// How far a brightness of 0 or 1 moves the filter cutoff.
const BRIGHTNESS_OCTAVES: f32 = 2.0;

/// One note's expression, CLAP ranges ([`NoteExpression`]); neutral by default.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoteExpr {
    /// Linear gain 0..4 (1 = unchanged).
    pub volume: f32,
    /// 0 = left, 0.5 = centre, 1 = right.
    pub pan: f32,
    /// Semitones added to the note's pitch.
    pub tuning: f32,
    /// Depth of the built-in vibrato, 0..1.
    pub vibrato: f32,
    /// Gain 0..1 (like MIDI CC 11).
    pub expression: f32,
    /// Filter brightness 0..1 (0.5 neutral); `None` follows the channel's CC 74.
    pub brightness: Option<f32>,
    /// Pressure 0..1; `None` follows channel pressure.
    pub pressure: Option<f32>,
}

impl Default for NoteExpr {
    fn default() -> Self {
        Self { volume: 1.0, pan: 0.5, tuning: 0.0, vibrato: 0.0, expression: 1.0, brightness: None, pressure: None }
    }
}

impl NoteExpr {
    /// Output gains (left, right): volume × expression with an equal-power pan that is unity at
    /// the centre.
    fn gains(&self) -> [f32; 2] {
        let (l, r) = rawdawg_types::units::Pan(f64::from(self.pan * 2.0 - 1.0)).equal_power_gains();
        let g = self.volume * self.expression * std::f32::consts::SQRT_2;
        [l * g, r * g]
    }
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
    layers: Vec<LayerVoice>,
    amp: Adsr,
    fenv: Adsr,
    filter_l: Svf,
    filter_r: Svf,
    /// Current (gliding) pitch in MIDI semitones including transpose/detune.
    pitch: Smoother,
    bend_semitones: f32,
    mod_wheel: f32,
    aftertouch: f32,
    /// MIDI CC 74 ("timbre"/brightness) of the voice's channel, 0..1 (0.5 neutral).
    timbre: f32,
    /// This note's own expression (reset at every note-on).
    expr: NoteExpr,
    /// Phase (cycles) of the built-in vibrato driven by the note's vibrato expression.
    vib_phase: f32,
    /// Output gains (left, right) reached at the end of the last sub-block; ramps follow them.
    out_gain: [f32; 2],
    random: f32,
    lfo: [LfoState; MAX_LFOS],
    rng: DeterministicRng,
    sample_rate: f64,
    control_counter: usize,
    mods: ModOutputs,
    seed: u64,
    /// Per-voice modular graph state (None when the voice graph is empty).
    graph: Option<GraphState>,
    /// Force a gate rising edge on the next sub-block (retrigger).
    gate_pulse: bool,
}

impl Voice {
    pub fn new(sample_rate: f64, patch: &Patch, index: usize, graph: Option<&CompiledGraph>) -> Self {
        let sr = sample_rate as f32;
        let seed = 0x0B00_B1E5 + index as u64;
        let layers = (0..MAX_LAYERS)
            .map(|l| LayerVoice {
                osc: [BasicOscillator::new(sample_rate); MAX_UNISON],
                level: 0,
                fm: [FmState::new(sample_rate); MAX_UNISON],
                additive: AdditiveState::new(sample_rate),
                granular: GranularState::new(sample_rate, seed ^ (l as u64 + 1)),
                pluck: PluckState::new(sample_rate, seed ^ (0x10 + l as u64)),
            })
            .collect();
        Self {
            key: None,
            age: 0,
            released: false,
            sustained: false,
            velocity: 0.0,
            layers,
            amp: Adsr::new(sr, patch.amp_env),
            fenv: Adsr::new(sr, patch.filter_env),
            filter_l: Svf::new(sr),
            filter_r: Svf::new(sr),
            pitch: Smoother::new(60.0, 0.0, sr),
            bend_semitones: 0.0,
            mod_wheel: 0.0,
            aftertouch: 0.0,
            timbre: 0.5,
            expr: NoteExpr::default(),
            vib_phase: 0.0,
            out_gain: [1.0, 1.0],
            random: 0.0,
            lfo: [LfoState::default(); MAX_LFOS],
            rng: DeterministicRng::new(seed, 0x77),
            sample_rate,
            control_counter: 0,
            mods: ModOutputs::default(),
            seed,
            graph: graph.map(|g| GraphState::new(g, sr, seed)),
            gate_pulse: false,
        }
    }

    /// Non-RT: (re)build the per-voice graph state for a new compiled graph.
    pub fn set_graph(&mut self, graph: Option<&CompiledGraph>) {
        self.graph = graph.map(|g| GraphState::new(g, self.sample_rate as f32, self.seed));
    }

    /// RT: exchange graph states with another voice slot (bounded move, no allocation).
    pub fn swap_graph_state(&mut self, other: &mut Voice) {
        std::mem::swap(&mut self.graph, &mut other.graph);
    }

    pub fn has_graph_state(&self) -> bool {
        self.graph.is_some()
    }

    pub fn expr_diagnostics(&self) -> crate::expr::ExprDiagnostics {
        self.graph.as_ref().map(|g| g.expr_diagnostics()).unwrap_or_default()
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
    pub fn set_controllers(&mut self, mod_wheel: f32, aftertouch: f32) {
        self.mod_wheel = mod_wheel;
        self.aftertouch = aftertouch;
    }

    /// Start a note. With `retrigger == false` the current pitch glides to the target (legato).
    #[allow(clippy::too_many_arguments)]
    pub fn note_on(&mut self, key: NoteKey, velocity: f32, age: u64, target_pitch: f32, portamento_s: f32, retrigger: bool, layers: &[LayerEngine; MAX_LAYERS]) {
        self.key = Some(key);
        self.age = age;
        self.released = false;
        self.sustained = false;
        self.velocity = velocity;
        self.expr = NoteExpr::default();
        let sr = self.sample_rate as f32;
        self.rng = DeterministicRng::new(self.seed ^ u64::from(key.note_id), age);
        self.random = self.rng.bipolar_f32();
        if retrigger {
            if portamento_s > 0.0 && self.amp.is_active() {
                self.pitch.set_time(portamento_s / 3.0, sr);
                self.pitch.set_target(target_pitch);
            } else {
                self.pitch.snap(target_pitch);
            }
            self.amp.note_on();
            self.fenv.note_on();
            self.gate_pulse = true;
            self.vib_phase = 0.0;
            self.out_gain = self.expr.gains();
            if let Some(g) = &mut self.graph {
                g.reset();
            }
            for l in &mut self.lfo {
                l.reset();
            }
            for (li, (lv, le)) in self.layers.iter_mut().zip(layers.iter()).enumerate() {
                if le.reset_phase {
                    for o in &mut lv.osc {
                        o.reset_phase(0.0);
                    }
                    for f in &mut lv.fm {
                        f.reset();
                    }
                }
                match &le.kind {
                    LayerKind::Additive { settings, .. } => lv.additive.reset(settings),
                    LayerKind::Granular { .. } => lv.granular.reset(self.seed ^ (age << 2) ^ li as u64),
                    LayerKind::Pluck(s) => {
                        lv.pluck.reset(self.seed ^ age ^ (li as u64) << 8);
                        let f = rawdawg_types::units::Pitch(key.pitch).frequency_hz(440.0, 0.0);
                        lv.pluck.pluck(s, f, velocity);
                    }
                    _ => {}
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

    /// The channel's CC 74 (0..1); the note's own brightness expression takes precedence.
    pub fn set_timbre(&mut self, value: f32) {
        self.timbre = value.clamp(0.0, 1.0);
    }

    /// Polyphonic (per-note) pressure, 0..1: replaces channel pressure as this note's
    /// aftertouch modulation source.
    pub fn set_note_pressure(&mut self, value: f32) {
        self.expr.pressure = Some(value.clamp(0.0, 1.0));
    }

    /// One per-note expression in its documented range ([`NoteExpression`]).
    pub fn set_expression(&mut self, expression: NoteExpression, value: f32) {
        let e = &mut self.expr;
        match expression {
            NoteExpression::Volume => e.volume = value.clamp(0.0, 4.0),
            NoteExpression::Pan => e.pan = value.clamp(0.0, 1.0),
            NoteExpression::Tuning => e.tuning = value.clamp(-120.0, 120.0),
            NoteExpression::Vibrato => e.vibrato = value.clamp(0.0, 1.0),
            NoteExpression::Expression => e.expression = value.clamp(0.0, 1.0),
            NoteExpression::Brightness => e.brightness = Some(value.clamp(0.0, 1.0)),
            NoteExpression::Pressure => e.pressure = Some(value.clamp(0.0, 1.0)),
        }
    }

    /// This note's expression (tests and diagnostics).
    pub fn expression(&self) -> NoteExpr {
        self.expr
    }

    pub fn set_sample_rate(&mut self, sr: f64) {
        self.sample_rate = sr;
        self.amp.set_sample_rate(sr as f32);
        self.fenv.set_sample_rate(sr as f32);
        self.filter_l.set_sample_rate(sr as f32);
        self.filter_r.set_sample_rate(sr as f32);
        for l in &mut self.layers {
            for o in &mut l.osc {
                o.set_sample_rate(sr);
            }
            l.fm = [FmState::new(sr); MAX_UNISON];
            l.additive = AdditiveState::new(sr);
            l.granular = GranularState::new(sr, self.seed);
            l.pluck = PluckState::new(sr, self.seed);
        }
    }

    /// Control-rate update: modulation, pitch, filter coefficients.
    fn update_control(&mut self, patch: &Patch, layers: &[LayerEngine; MAX_LAYERS], global_lfo: &[f32; MAX_LFOS], tempo_bpm: f64) {
        let Some(key) = self.key else { return };
        // per-voice LFOs advance by one control interval
        let mut lfo_vals = *global_lfo;
        for (i, lfo) in patch.lfos.iter().enumerate().take(MAX_LFOS) {
            if lfo.per_voice {
                lfo_vals[i] = self.lfo[i].advance(lfo, CONTROL_INTERVAL, self.sample_rate, tempo_bpm, &mut self.rng);
            }
        }
        let inputs = ModInputs {
            lfo: lfo_vals,
            amp_env: self.amp.level(),
            filter_env: self.fenv.level(),
            velocity: self.velocity,
            key: (f32::from(key.pitch) - 60.0) / 60.0,
            mod_wheel: self.mod_wheel,
            aftertouch: self.expr.pressure.unwrap_or(self.aftertouch),
            random: self.random,
        };
        self.mods = ModOutputs::evaluate(&patch.mod_routes, &inputs);
        // the note's vibrato expression: a 5.5 Hz sine of up to ±0.5 semitone
        self.vib_phase = (self.vib_phase + VIBRATO_HZ * CONTROL_INTERVAL as f32 / self.sample_rate as f32).fract();
        let vibrato = (self.vib_phase * std::f32::consts::TAU).sin() * self.expr.vibrato * VIBRATO_SEMITONES;
        let p = f64::from(self.pitch.current()) + f64::from(self.bend_semitones) + f64::from(self.expr.tuning + vibrato) + f64::from(self.mods.get(ModTarget::Pitch) * 24.0);
        let a4 = f64::from(patch.tuning.a4_hz);
        let pw_mod = f64::from(self.mods.get(ModTarget::PulseWidth) * 0.45);
        for (lv, le) in self.layers.iter_mut().zip(layers.iter()) {
            if !le.enabled {
                continue;
            }
            let n = le.unison;
            for u in 0..n {
                let det = unison_detune(u, n, le.unison_detune_cents);
                let hz = a4 * 2f64.powf((p + le.semitone_offset + det - 69.0) / 12.0);
                lv.osc[u].set_frequency(hz);
            }
            lv.level = MipTable::level_for_increment(lv.osc[0].increment());
            if let LayerKind::Basic { wave, pulse_width } = &le.kind {
                for o in lv.osc.iter_mut().take(n) {
                    o.set_wave(*wave);
                    o.set_pulse_width(pulse_width + pw_mod);
                }
            }
        }
        if patch.filter.enabled {
            let f = &patch.filter;
            let env = self.fenv.level() * f.env_octaves;
            let key_track = f.key_track * (self.pitch.current() - 60.0) / 12.0;
            // brightness (note expression or the channel's CC 74): ±2 octaves around 0.5
            let bright = (self.expr.brightness.unwrap_or(self.timbre) - 0.5) * BRIGHTNESS_OCTAVES * 2.0;
            let cutoff = f.cutoff_hz * 2f32.powf(env + key_track + bright + self.mods.get(ModTarget::Cutoff) * 4.0);
            let res = (f.resonance * 2f32.powf(self.mods.get(ModTarget::Resonance) * 2.0)).clamp(0.5, 20.0);
            self.filter_l.set_mode(f.mode);
            self.filter_r.set_mode(f.mode);
            self.filter_l.set(cutoff, res);
            self.filter_r.set(cutoff, res);
        }
    }

    /// Render `out_l.len()` samples (≤ CONTROL_INTERVAL), **adding** into the outputs.
    /// Returns `false` when the voice finished and the slot can be freed.
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    pub fn render_add(
        &mut self,
        out_l: &mut [f32],
        out_r: &mut [f32],
        patch: &Patch,
        layers: &[LayerEngine; MAX_LAYERS],
        tables: &BasicTables,
        global_lfo: &[f32; MAX_LFOS],
        tempo_bpm: f64,
        graph: Option<&CompiledGraph>,
        ws: &mut Workspace,
    ) -> bool {
        if self.key.is_none() {
            return false;
        }
        let n = out_l.len().min(CONTROL_INTERVAL);
        if self.control_counter == 0 {
            self.update_control(patch, layers, global_lfo, tempo_bpm);
        }
        self.control_counter += n;
        if self.control_counter >= CONTROL_INTERVAL {
            self.control_counter = 0;
        }
        let vel_gain = (1.0 - patch.velocity_sensitivity * (1.0 - self.velocity)) * (1.0 + self.mods.get(ModTarget::Amp)).max(0.0);
        let use_filter = patch.filter.enabled;
        let pan_mod = self.mods.get(ModTarget::Pan);
        let wt_mod = self.mods.get(ModTarget::WavePosition);
        let fm_scale = (1.0 + self.mods.get(ModTarget::FmIndex)).max(0.0);
        let grain_mod = self.mods.get(ModTarget::GrainPosition);
        let level_mods =
            [self.mods.get(ModTarget::LayerLevel1), self.mods.get(ModTarget::LayerLevel2), self.mods.get(ModTarget::LayerLevel3), self.mods.get(ModTarget::LayerLevel4)];
        // Envelopes and pitch glide first; `alive` samples remain before the amp envelope ends.
        let mut env = [0.0f32; CONTROL_INTERVAL];
        let mut alive = n;
        for (i, e) in env.iter_mut().enumerate().take(n) {
            self.pitch.next();
            *e = self.amp.next();
            self.fenv.next();
            if !self.amp.is_active() {
                alive = i;
                break;
            }
        }
        let mut sl = [0.0f32; CONTROL_INTERVAL];
        let mut sr_buf = [0.0f32; CONTROL_INTERVAL];
        for i in 0..alive {
            let mut l = 0.0f32;
            let mut r = 0.0f32;
            for (li, (lv, le)) in self.layers.iter_mut().zip(layers.iter()).enumerate() {
                if !le.enabled {
                    continue;
                }
                let gain = le.gain * (1.0 + level_mods[li]).max(0.0);
                match &le.kind {
                    LayerKind::Basic { .. } | LayerKind::Table { .. } | LayerKind::Fm(_) => {
                        let n = le.unison;
                        let ug = unison_gain(n);
                        for u in 0..n {
                            let s = match &le.kind {
                                LayerKind::Basic { .. } => lv.osc[u].next(tables),
                                LayerKind::Table { position, .. } => {
                                    let v = le.read_table(lv.level, lv.osc[u].phase(), (position + wt_mod).clamp(0.0, 1.0));
                                    lv.osc[u].advance();
                                    v
                                }
                                LayerKind::Fm(s) => {
                                    let hz = lv.osc[u].increment() * self.sample_rate;
                                    lv.fm[u].next(s, hz, fm_scale)
                                }
                                _ => 0.0,
                            } * gain
                                * ug;
                            let up = unison_pan(u, n, le.unison_spread);
                            let (pl, pr) = rawdawg_types::units::Pan(f64::from((up + pan_mod).clamp(-1.0, 1.0))).equal_power_gains();
                            // combine layer pan with unison pan (both equal-power)
                            l += s * le.pan_l * pl * std::f32::consts::SQRT_2;
                            r += s * le.pan_r * pr * std::f32::consts::SQRT_2;
                        }
                    }
                    LayerKind::Additive { settings, .. } => {
                        let hz = lv.osc[0].increment() * self.sample_rate;
                        let s = if settings.detuned {
                            lv.additive.next(settings, hz)
                        } else {
                            // harmonic mode: the prepared table
                            let v = match &le.kind {
                                LayerKind::Additive { table, .. } => table.read(lv.level, lv.osc[0].phase()),
                                _ => 0.0,
                            };
                            lv.osc[0].advance();
                            v
                        } * gain;
                        l += s * le.pan_l;
                        r += s * le.pan_r;
                    }
                    LayerKind::Granular { settings, source } => {
                        let hz = lv.osc[0].increment() * self.sample_rate;
                        let pos = (grain_mod + 0.5).clamp(0.0, 1.0);
                        let s = lv.granular.next(settings, source, hz, pos) * gain;
                        l += s * le.pan_l;
                        r += s * le.pan_r;
                    }
                    LayerKind::Pluck(s) => {
                        let hz = lv.osc[0].increment() * self.sample_rate;
                        // gain-matched to the other generators (a noise-excited loop sits ~4 dB lower)
                        let v = lv.pluck.next(s, hz, self.velocity) * gain * 1.6;
                        l += v * le.pan_l;
                        r += v * le.pan_r;
                    }
                }
            }
            sl[i] = l;
            sr_buf[i] = r;
        }
        // Per-voice modular graph between the generators and the filter/amp stage.
        if let (Some(g), Some(state)) = (graph, self.graph.as_mut()) {
            let mut gate = [0.0f32; CONTROL_INTERVAL];
            let held = if self.released { 0.0 } else { 1.0 };
            gate[..alive].fill(held);
            if self.gate_pulse && alive > 0 {
                gate[0] = 0.0;
                self.gate_pulse = false;
            }
            let mut lfo_vals = *global_lfo;
            for (i, lfo) in patch.lfos.iter().enumerate().take(MAX_LFOS) {
                if lfo.per_voice {
                    lfo_vals[i] = self.lfo[i].value();
                }
            }
            let src = Sources {
                mix_l: &sl,
                mix_r: &sr_buf,
                pitch_norm: (self.pitch.current() + self.bend_semitones + self.expr.tuning) / 127.0,
                gate: &gate,
                velocity: self.velocity,
                amp_env: self.amp.level(),
                filter_env: self.fenv.level(),
                lfo: lfo_vals,
                a4_hz: patch.tuning.a4_hz,
                tables,
            };
            let mut gl = [0.0f32; CONTROL_INTERVAL];
            let mut gr = [0.0f32; CONTROL_INTERVAL];
            g.run(state, ws, &src, &mut gl[..alive], &mut gr[..alive]);
            sl[..alive].copy_from_slice(&gl[..alive]);
            sr_buf[..alive].copy_from_slice(&gr[..alive]);
        }
        // the note's volume, expression and pan glide to their new values (no clicks)
        let [mut gl, mut gr] = self.out_gain;
        let [to_l, to_r] = self.expr.gains();
        let step = 1.0 / (EXPRESSION_GLIDE_S * self.sample_rate as f32);
        for i in 0..alive {
            let (mut l, mut r) = (sl[i], sr_buf[i]);
            if use_filter {
                l = self.filter_l.process(l);
                r = self.filter_r.process(r);
            }
            gl += (to_l - gl).clamp(-step, step);
            gr += (to_r - gr).clamp(-step, step);
            let g = env[i] * vel_gain;
            out_l[i] += l * g * gl;
            out_r[i] += r * g * gr;
        }
        self.out_gain = [gl, gr];
        if alive < n {
            self.kill();
            return false;
        }
        true
    }
}

/// Resolve the LFO definitions for the global clock (used by the synth).
pub fn lfo_defs(patch: &Patch) -> [Lfo; MAX_LFOS] {
    let mut out = [Lfo::default(); MAX_LFOS];
    for (i, l) in patch.lfos.iter().enumerate().take(MAX_LFOS) {
        out[i] = *l;
    }
    out
}

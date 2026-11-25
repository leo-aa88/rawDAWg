//! The `native.synth` processor.

use crate::params::{self, DESCRIPTORS};
use crate::patch::{Generator, Layer, MAX_LAYERS, Patch, VoiceMode};
use crate::registry::{HeldNotes, NoteKey, SustainTable, source_slot};
use crate::voice::{LayerEngine, Voice};
use rawdawg_dsp::math::sanitize;
use rawdawg_dsp::oscillator::{BasicTables, BasicWave};
use rawdawg_dsp::processor::{PrepareError, PrepareSpec, ProcessContext, Processor, Tail, find_descriptor};
use rawdawg_dsp::smooth::Smoother;
use rawdawg_types::events::{EventKind, TimedEvent};
use rawdawg_types::{ParamId, ParamMapping};

/// Extra voice slots reserved so stolen voices can fade out instead of clicking.
pub const SPARE_VOICES: usize = 8;
const CC_SUSTAIN: u8 = 64;
const CC_ALL_SOUND_OFF: u8 = 120;
const CC_ALL_NOTES_OFF: u8 = 123;

/// Health counters readable off-thread after `process` (copied, not shared).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SynthStats {
    pub active_voices: u32,
    pub stolen_voices: u64,
    pub nonfinite_samples: u64,
    pub rejected_events: u64,
}

pub struct Synth {
    patch: Patch,
    layers: [LayerEngine; MAX_LAYERS],
    tables: Option<BasicTables>,
    voices: Vec<Voice>,
    sustain: SustainTable,
    held: HeldNotes,
    bend: [f32; 16],
    master: Smoother,
    age: u64,
    sample_rate: f64,
    prepared: bool,
    stats: SynthStats,
    /// Mono-mode note id of the voice currently sounding (slot 0).
    mono_active: Option<NoteKey>,
}

impl Default for Synth {
    fn default() -> Self {
        Self::new(Patch::default())
    }
}

impl Synth {
    pub fn new(patch: Patch) -> Self {
        let layers = std::array::from_fn(|_| LayerEngine::disabled());
        Self {
            patch,
            layers,
            tables: None,
            voices: Vec::new(),
            sustain: SustainTable::new(),
            held: HeldNotes::new(),
            bend: [0.0; 16],
            master: Smoother::new(0.0, 0.0, 48_000.0),
            age: 0,
            sample_rate: 48_000.0,
            prepared: false,
            stats: SynthStats::default(),
            mono_active: None,
        }
    }

    pub fn patch(&self) -> &Patch {
        &self.patch
    }

    pub fn stats(&self) -> SynthStats {
        let mut s = self.stats;
        s.active_voices = self.voices.iter().filter(|v| v.is_active()).count() as u32;
        s
    }

    /// Non-RT: replace the whole patch (rebuilds tables when prepared).
    pub fn set_patch(&mut self, patch: Patch) -> Result<(), String> {
        patch.validate()?;
        self.patch = patch;
        if self.prepared {
            self.rebuild_engines();
        }
        Ok(())
    }

    /// Non-RT: rebuild layer engines/tables and voice pool for the current patch.
    fn rebuild_engines(&mut self) {
        for (i, le) in self.layers.iter_mut().enumerate() {
            *le = match self.patch.layers.get(i) {
                Some(layer) => LayerEngine::build(layer),
                None => LayerEngine::disabled(),
            };
        }
        if self.tables.is_none() {
            self.tables = Some(BasicTables::build());
        }
        let wanted = self.patch.max_voices as usize + SPARE_VOICES;
        if self.voices.len() != wanted {
            self.voices = (0..wanted).map(|_| Voice::new(self.sample_rate, &self.patch)).collect();
        } else {
            for v in &mut self.voices {
                v.kill();
                v.set_sample_rate(self.sample_rate);
                v.set_envelopes(self.patch.amp_env, self.patch.filter_env);
            }
        }
        self.master.snap(rawdawg_dsp::math::db_to_lin(self.patch.master_db));
        self.master.set_time(0.01, self.sample_rate as f32);
        self.mono_active = None;
        self.held.clear();
    }

    fn logical_voice_count(&self) -> usize {
        self.patch.max_voices as usize
    }

    /// Choose a slot for a new note. Deterministic: idle → oldest released → oldest active.
    fn allocate_voice(&mut self) -> usize {
        let n = self.voices.len();
        let active_logical = self.voices.iter().filter(|v| v.is_active() && !v.is_fading()).count();
        if active_logical < self.logical_voice_count()
            && let Some(i) = self.voices.iter().position(|v| !v.is_active())
        {
            return i;
        }
        // Must steal. Pick oldest released (not fading) else oldest active.
        let mut best: Option<(usize, u64)> = None;
        for (i, v) in self.voices.iter().enumerate() {
            if v.is_active() && v.released && !v.is_fading() && best.is_none_or(|(_, age)| v.age < age) {
                best = Some((i, v.age));
            }
        }
        if best.is_none() {
            for (i, v) in self.voices.iter().enumerate() {
                if v.is_active() && !v.is_fading() && best.is_none_or(|(_, age)| v.age < age) {
                    best = Some((i, v.age));
                }
            }
        }
        let victim = best.map(|(i, _)| i).unwrap_or(0);
        self.stats.stolen_voices += 1;
        self.voices[victim].steal();
        // The stolen voice keeps fading in its slot; use a free slot for the new
        // note if any remains, otherwise hard-cut the oldest fading voice.
        if let Some(i) = self.voices.iter().position(|v| !v.is_active()) {
            return i;
        }
        let mut oldest_fading: Option<(usize, u64)> = None;
        for (i, v) in self.voices.iter().enumerate() {
            if v.is_fading() && oldest_fading.is_none_or(|(_, age)| v.age < age) {
                oldest_fading = Some((i, v.age));
            }
        }
        let i = oldest_fading.map(|(i, _)| i).unwrap_or(victim.min(n - 1));
        self.voices[i].kill();
        i
    }

    fn target_pitch(&self, pitch: u8) -> f32 {
        f32::from(pitch) + self.patch.tuning.transpose_semitones as f32 + self.patch.tuning.detune_cents / 100.0
    }

    fn handle_note_on(&mut self, key: NoteKey, velocity: f32) {
        self.age += 1;
        let age = self.age;
        let target = self.target_pitch(key.pitch);
        let bend = self.bend[usize::from(key.channel) % 16] * self.patch.tuning.bend_range_semitones;
        match self.patch.voice_mode {
            VoiceMode::Poly => {
                // Re-trigger of the exact same note id (same source) restarts it.
                let slot = match self.voices.iter().position(|v| v.key == Some(key)) {
                    Some(i) => i,
                    None => self.allocate_voice(),
                };
                let layers = &self.layers;
                let v = &mut self.voices[slot];
                v.set_bend(bend);
                v.note_on(key, velocity, age, target, self.patch.portamento_s, true, layers);
            }
            VoiceMode::MonoRetrigger | VoiceMode::MonoLegato => {
                let was_sounding = self.mono_active.is_some_and(|k| self.voices[0].key == Some(k) && !self.voices[0].released);
                self.held.push(key, velocity);
                let legato = self.patch.voice_mode == VoiceMode::MonoLegato && was_sounding;
                let layers = &self.layers;
                let v = &mut self.voices[0];
                v.set_bend(bend);
                v.note_on(key, velocity, age, target, self.patch.portamento_s, !legato, layers);
                self.mono_active = Some(key);
            }
        }
    }

    fn handle_note_off(&mut self, key: NoteKey) {
        let pedal = self.sustain.is_down(key.source, key.channel);
        match self.patch.voice_mode {
            VoiceMode::Poly => {
                for v in self.voices.iter_mut() {
                    if v.key == Some(key) && !v.released {
                        if pedal {
                            v.sustained = true;
                        } else {
                            v.note_off();
                        }
                    }
                }
            }
            _ => {
                self.held.remove(key);
                if self.mono_active == Some(key) {
                    if let Some((prev, vel)) = self.held.top() {
                        // return to the previously held note
                        let age = self.age;
                        let target = self.target_pitch(prev.pitch);
                        let legato = self.patch.voice_mode == VoiceMode::MonoLegato;
                        let layers = &self.layers;
                        self.voices[0].note_on(prev, vel, age, target, self.patch.portamento_s, !legato, layers);
                        self.mono_active = Some(prev);
                    } else if pedal {
                        self.voices[0].sustained = true;
                    } else {
                        self.voices[0].note_off();
                        self.mono_active = None;
                    }
                }
            }
        }
    }

    fn release_sustained(&mut self, source: u8, channel: u8) {
        for v in self.voices.iter_mut() {
            if let Some(k) = v.key
                && v.sustained
                && k.source == source
                && k.channel == channel
            {
                v.note_off();
            }
        }
        if self.mono_active.is_some_and(|k| k.source == source && k.channel == channel) && self.voices[0].sustained {
            self.voices[0].note_off();
            self.mono_active = None;
        }
    }

    fn all_notes_off(&mut self, source: Option<u8>) {
        for v in self.voices.iter_mut() {
            if let Some(k) = v.key
                && source.is_none_or(|s| s == k.source)
            {
                v.note_off();
            }
        }
        match source {
            Some(s) => self.held.remove_source(s),
            None => self.held.clear(),
        }
        if self.mono_active.is_some_and(|k| source.is_none_or(|s| s == k.source)) {
            self.mono_active = None;
        }
    }

    fn hard_reset(&mut self) {
        for v in self.voices.iter_mut() {
            v.kill();
        }
        self.held.clear();
        self.sustain.clear();
        self.bend = [0.0; 16];
        self.mono_active = None;
    }

    fn handle_event(&mut self, ev: &TimedEvent) {
        if !ev.kind.is_valid() || ev.channel > 15 {
            self.stats.rejected_events += 1;
            return;
        }
        let source = source_slot(ev.source);
        match ev.kind {
            EventKind::NoteOn { note_id, pitch, velocity } => {
                self.handle_note_on(NoteKey { source, channel: ev.channel, note_id, pitch }, velocity);
            }
            EventKind::NoteOff { note_id, pitch } => {
                self.handle_note_off(NoteKey { source, channel: ev.channel, note_id, pitch });
            }
            EventKind::Control { cc, value } => match cc {
                CC_SUSTAIN => {
                    let down = value >= 0.5;
                    self.sustain.set(source, ev.channel, down);
                    if !down {
                        self.release_sustained(source, ev.channel);
                    }
                }
                CC_ALL_NOTES_OFF => self.all_notes_off(Some(source)),
                CC_ALL_SOUND_OFF => self.hard_reset(),
                _ => {}
            },
            EventKind::PitchBend { value } => {
                self.bend[usize::from(ev.channel)] = value;
                let semis = value * self.patch.tuning.bend_range_semitones;
                for v in self.voices.iter_mut() {
                    if let Some(k) = v.key
                        && k.channel == ev.channel
                    {
                        v.set_bend(semis);
                    }
                }
            }
            EventKind::AllNotesOff { source: src } => self.all_notes_off(src.map(source_slot)),
            EventKind::Reset => self.hard_reset(),
            EventKind::Param { id, value } => {
                self.apply_param(id, value);
            }
            EventKind::ParamMod { .. } | EventKind::ChannelPressure { .. } | EventKind::NotePressure { .. } | EventKind::NoteExpression { .. } => {}
        }
    }

    /// RT-safe parameter application (no allocation; wavetable cycles are not params).
    fn apply_param(&mut self, id: ParamId, normalized: f32) -> bool {
        let Some(d) = find_descriptor(&DESCRIPTORS, id) else { return false };
        let phys = d.to_physical(normalized);
        let p = &mut self.patch;
        let mut env_changed = false;
        let mut mode_changed = false;
        match id.0 {
            params::MASTER_GAIN => {
                p.master_db = phys as f32;
                self.master.set_target(rawdawg_dsp::math::db_to_lin(p.master_db));
            }
            params::VELOCITY_SENS => p.velocity_sensitivity = phys as f32,
            params::AMP_ATTACK => {
                p.amp_env.attack_s = phys as f32;
                env_changed = true;
            }
            params::AMP_DECAY => {
                p.amp_env.decay_s = phys as f32;
                env_changed = true;
            }
            params::AMP_SUSTAIN => {
                p.amp_env.sustain = phys as f32;
                env_changed = true;
            }
            params::AMP_RELEASE => {
                p.amp_env.release_s = phys as f32;
                env_changed = true;
            }
            params::FILTER_CUTOFF => p.filter.cutoff_hz = phys as f32,
            params::FILTER_RESONANCE => p.filter.resonance = phys as f32,
            params::FILTER_MODE => p.filter.mode = rawdawg_dsp::filter::FilterMode::from_index(phys as usize),
            params::FILTER_ENV_AMOUNT => p.filter.env_octaves = phys as f32,
            params::FILTER_KEYTRACK => p.filter.key_track = phys as f32,
            params::FILTER_ENABLED => p.filter.enabled = phys >= 0.5,
            params::FENV_ATTACK => {
                p.filter_env.attack_s = phys as f32;
                env_changed = true;
            }
            params::FENV_DECAY => {
                p.filter_env.decay_s = phys as f32;
                env_changed = true;
            }
            params::FENV_SUSTAIN => {
                p.filter_env.sustain = phys as f32;
                env_changed = true;
            }
            params::FENV_RELEASE => {
                p.filter_env.release_s = phys as f32;
                env_changed = true;
            }
            params::VOICE_MODE => {
                let m = VoiceMode::from_index(phys as usize);
                if m != p.voice_mode {
                    p.voice_mode = m;
                    mode_changed = true;
                }
            }
            params::PORTAMENTO => p.portamento_s = phys as f32,
            params::BEND_RANGE => p.tuning.bend_range_semitones = phys as f32,
            params::MAX_VOICES => {
                // Changing the pool size requires re-preparation (allocation); clamp to pool.
                let want = (phys as u32 + 1).clamp(1, 64);
                let pool_logical = (self.voices.len().saturating_sub(SPARE_VOICES)).max(1) as u32;
                p.max_voices = want.min(pool_logical);
            }
            params::TRANSPOSE => p.tuning.transpose_semitones = phys as i32 - 48,
            params::DETUNE => p.tuning.detune_cents = phys as f32,
            _ => {
                let Some((layer_idx, offset)) = params::decode_layer_param(id.0) else { return false };
                if layer_idx >= p.layers.len() {
                    // Layers beyond the patch's count can only be enabled via set_patch (allocation).
                    return false;
                }
                let layer: &mut Layer = &mut p.layers[layer_idx];
                match offset {
                    params::L_ENABLED => layer.enabled = phys >= 0.5,
                    params::L_WAVE => {
                        let idx = phys as usize;
                        if idx < 4 {
                            // switching to a basic wave never allocates
                            match &mut layer.generator {
                                Generator::Basic { wave, .. } => *wave = BasicWave::from_index(idx),
                                Generator::Wavetable { .. } => {
                                    // keep the table engine but mark basic playback via engine kind swap
                                    // (table data stays resident; no allocation)
                                    self.layers[layer_idx].kind = crate::voice::LayerKind::Basic { wave: BasicWave::from_index(idx), pulse_width: 0.5 };
                                    self.layers[layer_idx].update_mix(layer);
                                    return true;
                                }
                            }
                        } else {
                            // index 4 = wavetable; requires a prepared cycle (set_patch); ignore if none
                            return false;
                        }
                    }
                    params::L_PULSE_WIDTH => {
                        if let Generator::Basic { pulse_width, .. } = &mut layer.generator {
                            *pulse_width = phys as f32;
                        }
                    }
                    params::L_LEVEL => layer.level_db = phys as f32,
                    params::L_PAN => layer.pan = phys as f32,
                    params::L_COARSE => layer.coarse_semitones = phys as i32 - 48,
                    params::L_FINE => layer.fine_cents = phys as f32,
                    params::L_RESET_PHASE => layer.reset_phase = phys >= 0.5,
                    _ => return false,
                }
                self.layers[layer_idx].update_mix(layer);
                return true;
            }
        }
        if env_changed {
            let (a, f) = (p.amp_env.clamped(), p.filter_env.clamped());
            p.amp_env = a;
            p.filter_env = f;
            for v in &mut self.voices {
                v.set_envelopes(a, f);
            }
        }
        if mode_changed {
            self.all_notes_off(None);
        }
        true
    }

    fn render_span(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        let Some(tables) = self.tables.as_ref() else { return };
        for v in self.voices.iter_mut() {
            if v.is_active() {
                v.render_add(out_l, out_r, &self.patch, &self.layers, tables);
            }
        }
    }
}

impl Processor for Synth {
    fn type_id(&self) -> &'static str {
        crate::TYPE_ID
    }

    fn descriptors(&self) -> &'static [rawdawg_types::ParamDescriptor] {
        &DESCRIPTORS
    }

    fn prepare(&mut self, spec: &PrepareSpec) -> Result<(), PrepareError> {
        if !spec.is_valid() {
            return Err(PrepareError::UnsupportedSampleRate);
        }
        if spec.layout != rawdawg_dsp::BusLayout::Stereo {
            return Err(PrepareError::UnsupportedLayout);
        }
        self.patch.validate().map_err(PrepareError::InvalidState)?;
        self.sample_rate = spec.sample_rate;
        self.prepared = true;
        self.voices.clear();
        self.rebuild_engines();
        Ok(())
    }

    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let frames = ctx.audio.frames;
        ctx.audio.clear_outputs();
        if !self.prepared || ctx.audio.outputs.len() < 2 {
            return;
        }
        let (first, rest) = ctx.audio.outputs.split_at_mut(1);
        let out_l: &mut [f32] = &mut first[0][..frames];
        let out_r: &mut [f32] = &mut rest[0][..frames];
        let mut pos = 0usize;
        for ev in ctx.events {
            let at = (ev.frame as usize).min(frames);
            if at > pos {
                self.render_span(&mut out_l[pos..at], &mut out_r[pos..at]);
                pos = at;
            }
            self.handle_event(ev);
        }
        if pos < frames {
            self.render_span(&mut out_l[pos..], &mut out_r[pos..]);
        }
        for i in 0..frames {
            let g = self.master.next();
            out_l[i] *= g;
            out_r[i] *= g;
        }
        let bad = sanitize(out_l) + sanitize(out_r);
        if bad > 0 {
            self.stats.nonfinite_samples += u64::from(bad);
            self.hard_reset();
        }
    }

    fn reset(&mut self) {
        self.hard_reset();
    }

    fn tail(&self) -> Tail {
        // release time of the amp envelope bounds the tail after note-off
        Tail::Frames((self.patch.amp_env.release_s * self.sample_rate as f32).ceil() as u32 + 64)
    }

    fn active_voices(&self) -> u32 {
        self.voices.iter().filter(|v| v.is_active()).count() as u32
    }

    fn param(&self, id: ParamId) -> Option<f32> {
        let d = find_descriptor(&DESCRIPTORS, id)?;
        let p = &self.patch;
        let phys: f64 = match id.0 {
            params::MASTER_GAIN => p.master_db.into(),
            params::VELOCITY_SENS => p.velocity_sensitivity.into(),
            params::AMP_ATTACK => p.amp_env.attack_s.into(),
            params::AMP_DECAY => p.amp_env.decay_s.into(),
            params::AMP_SUSTAIN => p.amp_env.sustain.into(),
            params::AMP_RELEASE => p.amp_env.release_s.into(),
            params::FILTER_CUTOFF => p.filter.cutoff_hz.into(),
            params::FILTER_RESONANCE => p.filter.resonance.into(),
            params::FILTER_MODE => p.filter.mode.index() as f64,
            params::FILTER_ENV_AMOUNT => p.filter.env_octaves.into(),
            params::FILTER_KEYTRACK => p.filter.key_track.into(),
            params::FILTER_ENABLED => f64::from(u8::from(p.filter.enabled)),
            params::FENV_ATTACK => p.filter_env.attack_s.into(),
            params::FENV_DECAY => p.filter_env.decay_s.into(),
            params::FENV_SUSTAIN => p.filter_env.sustain.into(),
            params::FENV_RELEASE => p.filter_env.release_s.into(),
            params::VOICE_MODE => p.voice_mode.index() as f64,
            params::PORTAMENTO => p.portamento_s.into(),
            params::BEND_RANGE => p.tuning.bend_range_semitones.into(),
            params::MAX_VOICES => f64::from(p.max_voices - 1),
            params::TRANSPOSE => f64::from(p.tuning.transpose_semitones + 48),
            params::DETUNE => p.tuning.detune_cents.into(),
            _ => {
                let (li, off) = params::decode_layer_param(id.0)?;
                let layer = p.layers.get(li)?;
                match off {
                    params::L_ENABLED => f64::from(u8::from(layer.enabled)),
                    params::L_WAVE => match &layer.generator {
                        Generator::Basic { wave, .. } => wave.index() as f64,
                        Generator::Wavetable { .. } => 4.0,
                    },
                    params::L_PULSE_WIDTH => match &layer.generator {
                        Generator::Basic { pulse_width, .. } => f64::from(*pulse_width),
                        _ => 0.5,
                    },
                    params::L_LEVEL => layer.level_db.into(),
                    params::L_PAN => layer.pan.into(),
                    params::L_COARSE => f64::from(layer.coarse_semitones + 48),
                    params::L_FINE => layer.fine_cents.into(),
                    params::L_RESET_PHASE => f64::from(u8::from(layer.reset_phase)),
                    _ => return None,
                }
            }
        };
        let n = d.to_normalized(phys);
        // Discrete mappings: to_normalized gives phys/(steps-1), matching to_physical's rounding.
        let _ = ParamMapping::Toggle;
        Some(n)
    }

    fn set_param(&mut self, id: ParamId, normalized: f32) -> bool {
        if !normalized.is_finite() {
            return false;
        }
        self.apply_param(id, normalized.clamp(0.0, 1.0))
    }

    fn save_state(&self) -> Vec<u8> {
        self.patch.to_json()
    }

    fn load_state(&mut self, bytes: &[u8]) -> Result<(), String> {
        let patch = Patch::from_json(bytes)?;
        self.set_patch(patch)
    }
}

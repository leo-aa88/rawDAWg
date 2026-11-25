//! The `native.synth` processor.

use crate::graph::{self, CompiledGraph, GraphState, Workspace};
use crate::modulation::LfoState;
use crate::params::{self, DESCRIPTORS, ParamBlock};
use crate::patch::{FmAlgorithm, Generator, GrainWindow, Layer, LfoShape, MAX_LAYERS, MAX_LFOS, MAX_MOD_ROUTES, ModRoute, ModSource, ModTarget, Patch, VoiceMode};
use crate::registry::{HeldNotes, NoteKey, SustainTable, source_slot};
use crate::tables::TableInbox;
use crate::voice::{CONTROL_INTERVAL, LayerEngine, Voice, lfo_defs};
use rawdawg_dsp::math::sanitize;
use rawdawg_dsp::oscillator::{BasicTables, BasicWave};
use rawdawg_dsp::processor::{PrepareError, PrepareSpec, ProcessContext, Processor, Tail, find_descriptor};
use rawdawg_dsp::rng::DeterministicRng;
use rawdawg_dsp::smooth::Smoother;
use rawdawg_dsp::wavetable::MipTable;
use rawdawg_types::ParamId;
use rawdawg_types::events::{EventKind, TimedEvent};

/// Extra voice slots reserved so stolen voices can fade out instead of clicking.
pub const SPARE_VOICES: usize = 8;
const CC_MOD_WHEEL: u8 = 1;
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
    inbox: Option<TableInbox>,
    /// Boxed on purpose: unboxing would free the `Box` allocation on the audio thread.
    #[allow(clippy::vec_box)]
    pending_retire: Vec<Box<MipTable>>,
    tables: Option<BasicTables>,
    voices: Vec<Voice>,
    sustain: SustainTable,
    held: HeldNotes,
    bend: [f32; 16],
    mod_wheel: [f32; 16],
    aftertouch: [f32; 16],
    master: Smoother,
    age: u64,
    sample_rate: f64,
    prepared: bool,
    stats: SynthStats,
    /// Mono-mode note id of the voice currently sounding (slot 0).
    mono_active: Option<NoteKey>,
    global_lfo: [LfoState; MAX_LFOS],
    global_lfo_values: [f32; MAX_LFOS],
    lfo_rng: DeterministicRng,
    tempo_bpm: f64,
    /// Compiled modular graphs (None = pass-through) and their scratch workspaces.
    voice_graph: Option<CompiledGraph>,
    global_graph: Option<CompiledGraph>,
    voice_ws: Workspace,
    global_ws: Workspace,
    global_state: Option<GraphState>,
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
            inbox: None,
            pending_retire: Vec::with_capacity(crate::tables::TABLE_QUEUE_DEPTH * 4),
            tables: None,
            voices: Vec::new(),
            sustain: SustainTable::new(),
            held: HeldNotes::new(),
            bend: [0.0; 16],
            mod_wheel: [0.0; 16],
            aftertouch: [0.0; 16],
            master: Smoother::new(0.0, 0.0, 48_000.0),
            age: 0,
            sample_rate: 48_000.0,
            prepared: false,
            stats: SynthStats::default(),
            mono_active: None,
            global_lfo: [LfoState::default(); MAX_LFOS],
            global_lfo_values: [0.0; MAX_LFOS],
            lfo_rng: DeterministicRng::new(0x1F0, 0),
            tempo_bpm: 120.0,
            voice_graph: None,
            global_graph: None,
            voice_ws: Workspace::default(),
            global_ws: Workspace::default(),
            global_state: None,
        }
    }

    /// Compiled voice/global graphs (for tests and UI diagnostics).
    pub fn compiled_graphs(&self) -> (Option<&CompiledGraph>, Option<&CompiledGraph>) {
        (self.voice_graph.as_ref(), self.global_graph.as_ref())
    }

    /// Expression diagnostics summed over all voices and the global graph.
    pub fn expr_diagnostics(&self) -> crate::expr::ExprDiagnostics {
        let mut d = self.global_state.as_ref().map(|g| g.expr_diagnostics()).unwrap_or_default();
        for v in &self.voices {
            let x = v.expr_diagnostics();
            d.division_by_zero += x.division_by_zero;
            d.domain_errors += x.domain_errors;
            d.nonfinite += x.nonfinite;
        }
        d
    }

    pub fn patch(&self) -> &Patch {
        &self.patch
    }

    /// Attach the RT side of a live wavetable channel (non-RT, before/after prepare).
    pub fn set_table_inbox(&mut self, inbox: TableInbox) {
        self.inbox = Some(inbox);
    }

    /// Per-layer crossfade progress (1.0 = settled), for tests/UI.
    pub fn layer_xfade(&self, layer: usize) -> f32 {
        self.layers.get(layer).map(|l| l.xfade).unwrap_or(1.0)
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
        // Graphs were validated with the patch; an empty subgraph is a pass-through.
        self.voice_graph = if self.patch.graph.voice.is_empty() { None } else { graph::compile(&self.patch.graph.voice, graph::Scope::Voice).ok() };
        self.global_graph = if self.patch.graph.global.is_empty() { None } else { graph::compile(&self.patch.graph.global, graph::Scope::Global).ok() };
        self.voice_ws = self.voice_graph.as_ref().map(Workspace::new).unwrap_or_default();
        self.global_ws = self.global_graph.as_ref().map(Workspace::new).unwrap_or_default();
        self.global_state = self.global_graph.as_ref().map(|g| GraphState::new(g, self.sample_rate as f32, 0x6107));
        let wanted = self.patch.max_voices as usize + SPARE_VOICES;
        if self.voices.len() != wanted {
            self.voices = (0..wanted).map(|i| Voice::new(self.sample_rate, &self.patch, i, self.voice_graph.as_ref())).collect();
        } else {
            for v in &mut self.voices {
                v.kill();
                v.set_sample_rate(self.sample_rate);
                v.set_envelopes(self.patch.amp_env, self.patch.filter_env);
                v.set_graph(self.voice_graph.as_ref());
            }
        }
        self.master.snap(rawdawg_dsp::math::db_to_lin(self.patch.master_db));
        self.master.set_time(0.01, self.sample_rate as f32);
        self.mono_active = None;
        self.held.clear();
        for l in &mut self.global_lfo {
            l.reset();
        }
    }

    /// RT: apply queued table updates and park finished crossfades for retirement.
    fn poll_tables(&mut self) {
        let Some(inbox) = self.inbox.as_mut() else { return };
        while let Some(t) = self.pending_retire.pop() {
            if let Err(t) = inbox.retire(t) {
                self.pending_retire.push(t);
                break;
            }
        }
        let sr = self.sample_rate as f32;
        let layers = &mut self.layers;
        let pending = &mut self.pending_retire;
        for _ in 0..crate::tables::TABLE_QUEUE_DEPTH {
            if !inbox.can_retire() {
                return;
            }
            let Some(update) = inbox.pop() else { return };
            if update.layer >= layers.len() {
                let _ = inbox.retire(update.table);
                continue;
            }
            let mut retire = |t: Box<MipTable>| {
                if let Err(t) = inbox.retire(t) {
                    pending.push(t);
                }
            };
            layers[update.layer].install_table(update.frame, update.table, sr, &mut retire);
        }
    }

    /// RT: advance crossfades for every layer by `samples`, parking finished tables.
    #[inline]
    fn tick_xfades(&mut self, samples: usize) {
        let pending = &mut self.pending_retire;
        let mut retire = |t: Box<MipTable>| pending.push(t);
        for l in self.layers.iter_mut() {
            l.tick_xfade(samples, &mut retire);
        }
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
        let ch = usize::from(key.channel) % 16;
        let bend = self.bend[ch] * self.patch.tuning.bend_range_semitones;
        let (mw, at) = (self.mod_wheel[ch], self.aftertouch[ch]);
        match self.patch.voice_mode {
            VoiceMode::Poly => {
                let slot = match self.voices.iter().position(|v| v.key == Some(key)) {
                    Some(i) => i,
                    None => self.allocate_voice(),
                };
                let layers = &self.layers;
                let v = &mut self.voices[slot];
                v.set_bend(bend);
                v.set_controllers(mw, at);
                v.note_on(key, velocity, age, target, self.patch.portamento_s, true, layers);
            }
            VoiceMode::MonoRetrigger | VoiceMode::MonoLegato => {
                let was_sounding = self.mono_active.is_some_and(|k| self.voices[0].key == Some(k) && !self.voices[0].released);
                self.held.push(key, velocity);
                let legato = self.patch.voice_mode == VoiceMode::MonoLegato && was_sounding;
                let layers = &self.layers;
                let v = &mut self.voices[0];
                v.set_bend(bend);
                v.set_controllers(mw, at);
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
        if let Some(g) = &mut self.global_state {
            g.reset();
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
        let ch = usize::from(ev.channel);
        match ev.kind {
            EventKind::NoteOn { note_id, pitch, velocity } => {
                self.handle_note_on(NoteKey { source, channel: ev.channel, note_id, pitch }, velocity);
            }
            EventKind::NoteOff { note_id, pitch } => {
                self.handle_note_off(NoteKey { source, channel: ev.channel, note_id, pitch });
            }
            EventKind::Control { cc, value } => match cc {
                CC_MOD_WHEEL => {
                    self.mod_wheel[ch] = value;
                    let at = self.aftertouch[ch];
                    for v in self.voices.iter_mut() {
                        if v.key.is_some_and(|k| k.channel == ev.channel) {
                            v.set_controllers(value, at);
                        }
                    }
                }
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
            EventKind::ChannelPressure { value } => {
                self.aftertouch[ch] = value;
                let mw = self.mod_wheel[ch];
                for v in self.voices.iter_mut() {
                    if v.key.is_some_and(|k| k.channel == ev.channel) {
                        v.set_controllers(mw, value);
                    }
                }
            }
            EventKind::PitchBend { value } => {
                self.bend[ch] = value;
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
            EventKind::ParamMod { .. } | EventKind::NotePressure { .. } | EventKind::NoteExpression { .. } => {}
        }
    }

    /// RT-safe parameter application (no allocation; structure changes are not params).
    fn apply_param(&mut self, id: ParamId, normalized: f32) -> bool {
        let Some(d) = find_descriptor(&DESCRIPTORS, id) else { return false };
        if d.flags.read_only {
            return false;
        }
        let phys = d.to_physical(normalized);
        let Some(block) = params::decode(id.0) else { return false };
        let mut env_changed = false;
        let mut mode_changed = false;
        match block {
            ParamBlock::Global => {
                let p = &mut self.patch;
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
                        let want = (phys as u32 + 1).clamp(1, 64);
                        let pool_logical = (self.voices.len().saturating_sub(SPARE_VOICES)).max(1) as u32;
                        p.max_voices = want.min(pool_logical);
                    }
                    params::TRANSPOSE => p.tuning.transpose_semitones = phys as i32 - 48,
                    params::DETUNE => p.tuning.detune_cents = phys as f32,
                    _ => return false,
                }
            }
            ParamBlock::Layer { layer: li, offset } => {
                let Some(layer) = self.patch.layers.get_mut(li) else { return false };
                match offset {
                    params::L_ENABLED => layer.enabled = phys >= 0.5,
                    params::L_WAVE => {
                        if let Generator::Basic { wave, .. } = &mut layer.generator {
                            *wave = BasicWave::from_index(phys as usize);
                        } else {
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
                    params::L_UNISON => layer.unison = (phys as u8 + 1).clamp(1, 8),
                    params::L_UNISON_DETUNE => layer.unison_detune_cents = phys as f32,
                    params::L_UNISON_SPREAD => layer.unison_spread = phys as f32,
                    params::L_WT_POSITION => {
                        if let Generator::Wavetable { position, .. } = &mut layer.generator {
                            *position = phys as f32;
                        }
                    }
                    _ => return false,
                }
                self.layers[li].update_mix(layer);
                return true;
            }
            ParamBlock::Gen { layer: li, offset } => {
                let Some(layer) = self.patch.layers.get_mut(li) else { return false };
                let ok = match (&mut layer.generator, offset) {
                    (Generator::Fm(s), o) if o <= params::G_FM_ALGORITHM => {
                        match o {
                            params::G_FM_RATIO1..=params::G_FM_RATIO4 => s.operators[(o - params::G_FM_RATIO1) as usize].ratio = phys as f32,
                            params::G_FM_LEVEL1..=params::G_FM_LEVEL4 => s.operators[(o - params::G_FM_LEVEL1) as usize].level = phys as f32,
                            params::G_FM_FEEDBACK => s.feedback = phys as f32,
                            _ => s.algorithm = FmAlgorithm::from_index(phys as usize),
                        }
                        true
                    }
                    (Generator::Additive(s), params::G_ADD_STRETCH) => {
                        s.stretch = phys as f32;
                        true
                    }
                    (Generator::Additive(s), params::G_ADD_DETUNED) => {
                        s.detuned = phys >= 0.5;
                        true
                    }
                    (Generator::Granular { settings, .. }, o) if (params::G_GRAIN_SIZE..=params::G_GRAIN_WINDOW).contains(&o) => {
                        match o {
                            params::G_GRAIN_SIZE => settings.size_ms = phys as f32,
                            params::G_GRAIN_DENSITY => settings.density_hz = phys as f32,
                            params::G_GRAIN_JITTER => settings.jitter = phys as f32,
                            params::G_GRAIN_SPREAD => settings.pitch_spread_semitones = phys as f32,
                            _ => settings.window = [GrainWindow::Hann, GrainWindow::Triangle, GrainWindow::Rect][(phys as usize).min(2)],
                        }
                        true
                    }
                    (Generator::Pluck(s), o) if o >= params::G_PLUCK_DAMPING => {
                        match o {
                            params::G_PLUCK_DAMPING => s.damping = phys as f32,
                            params::G_PLUCK_BRIGHTNESS => s.brightness = phys as f32,
                            _ => s.excitation = [crate::patch::Excitation::Noise, crate::patch::Excitation::Impulse, crate::patch::Excitation::Pick][(phys as usize).min(2)],
                        }
                        true
                    }
                    _ => false,
                };
                if ok {
                    self.layers[li].update_mix(layer);
                }
                return ok;
            }
            ParamBlock::Lfo { lfo, offset } => {
                let Some(l) = self.patch.lfos.get_mut(lfo) else { return false };
                match offset {
                    params::LFO_SHAPE => l.shape = LfoShape::from_index(phys as usize),
                    params::LFO_RATE => l.rate_hz = phys as f32,
                    params::LFO_SYNC => l.sync_beats = phys as f32,
                    params::LFO_DEPTH => l.depth = phys as f32,
                    params::LFO_PER_VOICE => l.per_voice = phys >= 0.5,
                    _ => return false,
                }
                return true;
            }
            ParamBlock::Mod { route, offset } => {
                // routes beyond the current count become enabled defaults (no allocation
                // beyond MAX_MOD_ROUTES: the Vec was sized at prepare)
                while self.patch.mod_routes.len() <= route && self.patch.mod_routes.len() < MAX_MOD_ROUTES {
                    if self.patch.mod_routes.capacity() == self.patch.mod_routes.len() {
                        return false; // would allocate on the RT thread
                    }
                    self.patch.mod_routes.push(ModRoute { source: ModSource::Lfo1, target: ModTarget::Pitch, depth: 0.0, enabled: false });
                }
                let Some(r) = self.patch.mod_routes.get_mut(route) else { return false };
                match offset {
                    params::MOD_SOURCE => r.source = ModSource::from_index(phys as usize),
                    params::MOD_TARGET => r.target = ModTarget::from_index(phys as usize),
                    params::MOD_DEPTH => r.depth = phys as f32,
                    params::MOD_ENABLED => r.enabled = phys >= 0.5,
                    _ => return false,
                }
                return true;
            }
        }
        if env_changed {
            let (a, f) = (self.patch.amp_env.clamped(), self.patch.filter_env.clamped());
            self.patch.amp_env = a;
            self.patch.filter_env = f;
            for v in &mut self.voices {
                v.set_envelopes(a, f);
            }
        }
        if mode_changed {
            self.all_notes_off(None);
        }
        true
    }

    /// Render a span in control-interval chunks: global LFOs and crossfades advance per chunk.
    fn render_span(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        if self.tables.is_none() {
            return;
        }
        let defs = lfo_defs(&self.patch);
        let n = out_l.len();
        let mut pos = 0;
        while pos < n {
            let end = (pos + CONTROL_INTERVAL).min(n);
            let chunk = end - pos;
            for (i, lfo) in defs.iter().enumerate() {
                if !lfo.per_voice {
                    self.global_lfo_values[i] = self.global_lfo[i].advance(lfo, chunk, self.sample_rate, self.tempo_bpm, &mut self.lfo_rng);
                }
            }
            {
                let tables = self.tables.as_ref().expect("checked above");
                let lfo_vals = self.global_lfo_values;
                let graph = self.voice_graph.as_ref();
                for v in self.voices.iter_mut() {
                    if v.is_active() {
                        v.render_add(&mut out_l[pos..end], &mut out_r[pos..end], &self.patch, &self.layers, tables, &lfo_vals, self.tempo_bpm, graph, &mut self.voice_ws);
                    }
                }
            }
            if let (Some(g), Some(state)) = (self.global_graph.as_ref(), self.global_state.as_mut()) {
                let tables = self.tables.as_ref().expect("checked above");
                let mut in_l = [0.0f32; CONTROL_INTERVAL];
                let mut in_r = [0.0f32; CONTROL_INTERVAL];
                in_l[..chunk].copy_from_slice(&out_l[pos..end]);
                in_r[..chunk].copy_from_slice(&out_r[pos..end]);
                let src = graph::Sources {
                    mix_l: &in_l,
                    mix_r: &in_r,
                    pitch_norm: 69.0 / 127.0,
                    gate: &in_l,
                    velocity: 1.0,
                    amp_env: 1.0,
                    filter_env: 1.0,
                    lfo: self.global_lfo_values,
                    a4_hz: self.patch.tuning.a4_hz,
                    tables,
                };
                g.run(state, &mut self.global_ws, &src, &mut out_l[pos..end], &mut out_r[pos..end]);
            }
            self.tick_xfades(chunk);
            pos = end;
        }
    }
}

impl Processor for Synth {
    fn type_id(&self) -> &'static str {
        crate::TYPE_ID
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    /// Take over the live voices, note bookkeeping, tables and the table inbox from
    /// the previously running instance. Bounded moves only; `old` ends up with this
    /// instance's fresh (silent) state and is retired by the worker.
    fn migrate_from(&mut self, old: &mut dyn Processor) {
        let Some(old) = old.as_any_mut().downcast_mut::<Synth>() else { return };
        if !old.prepared || !self.prepared || old.sample_rate != self.sample_rate || old.voices.len() != self.voices.len() {
            return;
        }
        // Voice graph state: carry over when the node layout is unchanged, otherwise move the
        // fresh (silent) states of this instance into the live voices (bounded swaps, no allocation).
        let carry = match (&self.voice_graph, &old.voice_graph) {
            (Some(a), Some(b)) => a.state_compatible(b),
            (None, None) => true,
            _ => false,
        };
        if !carry {
            for (n, o) in self.voices.iter_mut().zip(old.voices.iter_mut()) {
                n.swap_graph_state(o);
            }
        }
        let carry_global = match (&self.global_graph, &old.global_graph) {
            (Some(a), Some(b)) => a.state_compatible(b),
            _ => false,
        };
        if carry_global {
            std::mem::swap(&mut self.global_state, &mut old.global_state);
        }
        std::mem::swap(&mut self.voices, &mut old.voices);
        std::mem::swap(&mut self.sustain, &mut old.sustain);
        std::mem::swap(&mut self.held, &mut old.held);
        std::mem::swap(&mut self.bend, &mut old.bend);
        std::mem::swap(&mut self.mod_wheel, &mut old.mod_wheel);
        std::mem::swap(&mut self.aftertouch, &mut old.aftertouch);
        std::mem::swap(&mut self.age, &mut old.age);
        std::mem::swap(&mut self.mono_active, &mut old.mono_active);
        std::mem::swap(&mut self.layers, &mut old.layers);
        std::mem::swap(&mut self.inbox, &mut old.inbox);
        std::mem::swap(&mut self.pending_retire, &mut old.pending_retire);
        std::mem::swap(&mut self.global_lfo, &mut old.global_lfo);
        self.master.snap(old.master.current());
        let (a, f) = (self.patch.amp_env, self.patch.filter_env);
        for v in &mut self.voices {
            v.set_envelopes(a, f);
        }
        for (i, le) in self.layers.iter_mut().enumerate() {
            if let Some(layer) = self.patch.layers.get(i) {
                le.update_mix(layer);
            }
        }
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
        // reserve the modulation route capacity so RT param events never allocate
        self.patch.mod_routes.reserve(MAX_MOD_ROUTES.saturating_sub(self.patch.mod_routes.len()));
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
        if ctx.transport.tempo_bpm.is_finite() && ctx.transport.tempo_bpm > 0.0 {
            self.tempo_bpm = ctx.transport.tempo_bpm;
        }
        self.poll_tables();
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
        Tail::Frames((self.patch.amp_env.release_s * self.sample_rate as f32).ceil() as u32 + 64)
    }

    fn active_voices(&self) -> u32 {
        self.voices.iter().filter(|v| v.is_active()).count() as u32
    }

    fn param(&self, id: ParamId) -> Option<f32> {
        let d = find_descriptor(&DESCRIPTORS, id)?;
        let p = &self.patch;
        let phys: f64 = match params::decode(id.0)? {
            ParamBlock::Global => match id.0 {
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
                _ => return None,
            },
            ParamBlock::Layer { layer: li, offset } => {
                let layer = p.layers.get(li)?;
                match offset {
                    params::L_ENABLED => f64::from(u8::from(layer.enabled)),
                    params::L_WAVE => match &layer.generator {
                        Generator::Basic { wave, .. } => wave.index() as f64,
                        _ => 0.0,
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
                    params::L_UNISON => f64::from(layer.unison.saturating_sub(1)),
                    params::L_UNISON_DETUNE => layer.unison_detune_cents.into(),
                    params::L_UNISON_SPREAD => layer.unison_spread.into(),
                    params::L_WT_POSITION => match &layer.generator {
                        Generator::Wavetable { position, .. } => f64::from(*position),
                        _ => 0.0,
                    },
                    params::L_KIND => layer.generator.kind_index() as f64,
                    _ => return None,
                }
            }
            ParamBlock::Gen { layer: li, offset } => {
                let layer = p.layers.get(li)?;
                match (&layer.generator, offset) {
                    (Generator::Fm(s), o) if o <= params::G_FM_RATIO4 => s.operators[o as usize].ratio.into(),
                    (Generator::Fm(s), o) if o <= params::G_FM_LEVEL4 => s.operators[(o - params::G_FM_LEVEL1) as usize].level.into(),
                    (Generator::Fm(s), params::G_FM_FEEDBACK) => s.feedback.into(),
                    (Generator::Fm(s), params::G_FM_ALGORITHM) => s.algorithm.index() as f64,
                    (Generator::Additive(s), params::G_ADD_STRETCH) => s.stretch.into(),
                    (Generator::Additive(s), params::G_ADD_DETUNED) => f64::from(u8::from(s.detuned)),
                    (Generator::Granular { settings, .. }, params::G_GRAIN_SIZE) => settings.size_ms.into(),
                    (Generator::Granular { settings, .. }, params::G_GRAIN_DENSITY) => settings.density_hz.into(),
                    (Generator::Granular { settings, .. }, params::G_GRAIN_JITTER) => settings.jitter.into(),
                    (Generator::Granular { settings, .. }, params::G_GRAIN_SPREAD) => settings.pitch_spread_semitones.into(),
                    (Generator::Granular { settings, .. }, params::G_GRAIN_WINDOW) => {
                        [GrainWindow::Hann, GrainWindow::Triangle, GrainWindow::Rect].iter().position(|w| *w == settings.window).unwrap_or(0) as f64
                    }
                    (Generator::Pluck(s), params::G_PLUCK_DAMPING) => s.damping.into(),
                    (Generator::Pluck(s), params::G_PLUCK_BRIGHTNESS) => s.brightness.into(),
                    (Generator::Pluck(s), params::G_PLUCK_EXCITATION) => {
                        [crate::patch::Excitation::Noise, crate::patch::Excitation::Impulse, crate::patch::Excitation::Pick].iter().position(|e| *e == s.excitation).unwrap_or(0)
                            as f64
                    }
                    _ => return None,
                }
            }
            ParamBlock::Lfo { lfo, offset } => {
                let l = p.lfos.get(lfo)?;
                match offset {
                    params::LFO_SHAPE => l.shape.index() as f64,
                    params::LFO_RATE => l.rate_hz.into(),
                    params::LFO_SYNC => l.sync_beats.into(),
                    params::LFO_DEPTH => l.depth.into(),
                    params::LFO_PER_VOICE => f64::from(u8::from(l.per_voice)),
                    _ => return None,
                }
            }
            ParamBlock::Mod { route, offset } => {
                let r = p.mod_routes.get(route).copied().unwrap_or(ModRoute { source: ModSource::Lfo1, target: ModTarget::Pitch, depth: 0.0, enabled: false });
                match offset {
                    params::MOD_SOURCE => r.source.index() as f64,
                    params::MOD_TARGET => r.target.index() as f64,
                    params::MOD_DEPTH => r.depth.into(),
                    params::MOD_ENABLED => f64::from(u8::from(r.enabled)),
                    _ => return None,
                }
            }
        };
        Some(d.to_normalized(phys))
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

/// Keep the unused-import check honest for `Layer` (used by callers via patch).
#[allow(dead_code)]
fn _layer_marker(_: &Layer) {}

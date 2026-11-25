//! Patch model: the complete serializable sound.

use rawdawg_dsp::envelope::AdsrParams;
use rawdawg_dsp::filter::FilterMode;
use rawdawg_dsp::oscillator::BasicWave;
use rawdawg_dsp::wavetable::{CYCLE_LEN, cycles};
use serde::{Deserialize, Serialize};

/// Current patch schema version.
pub const PATCH_VERSION: u32 = 1;
pub const MAX_LAYERS: usize = 4;
pub const MAX_UNISON: usize = 8;
pub const MAX_FRAMES: usize = 16;
pub const MAX_LFOS: usize = 4;
pub const MAX_MOD_ROUTES: usize = 16;
pub const MAX_PARTIALS: usize = 128;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VoiceMode {
    #[default]
    Poly,
    MonoRetrigger,
    MonoLegato,
}

impl VoiceMode {
    pub const ALL: [VoiceMode; 3] = [VoiceMode::Poly, VoiceMode::MonoRetrigger, VoiceMode::MonoLegato];
    pub fn index(self) -> usize {
        match self {
            VoiceMode::Poly => 0,
            VoiceMode::MonoRetrigger => 1,
            VoiceMode::MonoLegato => 2,
        }
    }
    pub fn from_index(i: usize) -> Self {
        Self::ALL[i.min(2)]
    }
    pub fn label(self) -> &'static str {
        match self {
            VoiceMode::Poly => "Poly",
            VoiceMode::MonoRetrigger => "Mono",
            VoiceMode::MonoLegato => "Legato",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FmAlgorithm {
    /// 4 → 3 → 2 → 1 (one carrier).
    #[default]
    Serial,
    /// (2 → 1) + (4 → 3) (two carriers).
    Pairs,
    /// 2, 3, 4 → 1 (one carrier, three modulators).
    Fan,
    /// All four are carriers (additive).
    Parallel,
}

impl FmAlgorithm {
    pub const ALL: [FmAlgorithm; 4] = [FmAlgorithm::Serial, FmAlgorithm::Pairs, FmAlgorithm::Fan, FmAlgorithm::Parallel];
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|a| *a == self).unwrap_or(0)
    }
    pub fn from_index(i: usize) -> Self {
        Self::ALL[i.min(3)]
    }
    pub fn label(self) -> &'static str {
        match self {
            FmAlgorithm::Serial => "Serial 4>3>2>1",
            FmAlgorithm::Pairs => "Pairs (2>1)+(4>3)",
            FmAlgorithm::Fan => "Fan 2,3,4>1",
            FmAlgorithm::Parallel => "Parallel",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct FmOperator {
    /// Frequency ratio to the note (0.25..16).
    pub ratio: f32,
    /// Modulation level / carrier level 0..1.
    pub level: f32,
    pub detune_cents: f32,
}

impl Default for FmOperator {
    fn default() -> Self {
        Self { ratio: 1.0, level: 0.5, detune_cents: 0.0 }
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct FmSettings {
    pub operators: [FmOperator; 4],
    pub algorithm: FmAlgorithm,
    /// Feedback of operator 4 into itself (one-sample delay), 0..1.
    pub feedback: f32,
}

impl Default for FmSettings {
    fn default() -> Self {
        Self {
            operators: [
                FmOperator { ratio: 1.0, level: 1.0, detune_cents: 0.0 },
                FmOperator { ratio: 2.0, level: 0.35, detune_cents: 0.0 },
                FmOperator { ratio: 1.0, level: 0.0, detune_cents: 0.0 },
                FmOperator { ratio: 1.0, level: 0.0, detune_cents: 0.0 },
            ],
            algorithm: FmAlgorithm::Serial,
            feedback: 0.0,
        }
    }
}

impl FmSettings {
    pub fn is_valid(&self) -> bool {
        self.feedback.is_finite()
            && (0.0..=1.0).contains(&self.feedback)
            && self.operators.iter().all(|o| {
                o.ratio.is_finite()
                    && (0.25..=16.0).contains(&o.ratio)
                    && o.level.is_finite()
                    && (0.0..=1.0).contains(&o.level)
                    && o.detune_cents.is_finite()
                    && (-100.0..=100.0).contains(&o.detune_cents)
            })
    }
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct AdditiveSettings {
    /// Partial amplitudes 0..1 (index 0 = fundamental), up to `MAX_PARTIALS`.
    pub amplitudes: Vec<f32>,
    /// Partial phases in radians.
    pub phases: Vec<f32>,
    /// Inharmonic stretch: partial k frequency × (1 + stretch·k).
    pub stretch: f32,
    /// `true` = real-time detuned bank (≤32 partials); `false` = harmonic table.
    pub detuned: bool,
}

impl Default for AdditiveSettings {
    fn default() -> Self {
        Self { amplitudes: (0..16).map(|k| 1.0 / (k as f32 + 1.0)).collect(), phases: vec![0.0; 16], stretch: 0.0, detuned: false }
    }
}

impl AdditiveSettings {
    pub fn is_valid(&self) -> bool {
        !self.amplitudes.is_empty()
            && self.amplitudes.len() <= MAX_PARTIALS
            && self.phases.len() == self.amplitudes.len()
            && self.amplitudes.iter().all(|a| a.is_finite() && (0.0..=1.0).contains(a))
            && self.phases.iter().all(|p| p.is_finite())
            && self.stretch.is_finite()
            && (-0.05..=0.05).contains(&self.stretch)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GrainWindow {
    #[default]
    Hann,
    Triangle,
    Rect,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct GranularSettings {
    pub size_ms: f32,
    pub density_hz: f32,
    /// Random start offset as a fraction of the source (0..1).
    pub jitter: f32,
    pub pitch_spread_semitones: f32,
    pub window: GrainWindow,
}

impl Default for GranularSettings {
    fn default() -> Self {
        Self { size_ms: 60.0, density_hz: 40.0, jitter: 0.1, pitch_spread_semitones: 0.0, window: GrainWindow::Hann }
    }
}

impl GranularSettings {
    pub fn is_valid(&self) -> bool {
        self.size_ms.is_finite()
            && (5.0..=500.0).contains(&self.size_ms)
            && self.density_hz.is_finite()
            && (1.0..=400.0).contains(&self.density_hz)
            && self.jitter.is_finite()
            && (0.0..=1.0).contains(&self.jitter)
            && self.pitch_spread_semitones.is_finite()
            && (0.0..=24.0).contains(&self.pitch_spread_semitones)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Excitation {
    #[default]
    Noise,
    Impulse,
    Pick,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct PluckSettings {
    /// 0 = long sustain, 1 = fast decay.
    pub damping: f32,
    /// 0 = dark, 1 = bright.
    pub brightness: f32,
    pub excitation: Excitation,
}

impl Default for PluckSettings {
    fn default() -> Self {
        Self { damping: 0.2, brightness: 0.6, excitation: Excitation::Noise }
    }
}

impl PluckSettings {
    pub fn is_valid(&self) -> bool {
        self.damping.is_finite() && (0.0..=1.0).contains(&self.damping) && self.brightness.is_finite() && (0.0..=1.0).contains(&self.brightness)
    }
}

/// Sound generator of one layer.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Generator {
    /// Bandlimited basic wave.
    Basic {
        wave: BasicWave,
        pulse_width: f32,
    },
    /// Editable single-cycle wavetable (`CYCLE_LEN` samples, [-1,1]) with optional
    /// motion frames; `cycle` is the first frame when `frames` is empty.
    Wavetable {
        cycle: Vec<f32>,
        remove_dc: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        frames: Vec<Vec<f32>>,
        #[serde(default)]
        position: f32,
    },
    Fm(FmSettings),
    Additive(AdditiveSettings),
    /// Grains read from the layer's `source` cycle.
    Granular {
        settings: GranularSettings,
        source: Vec<f32>,
    },
    Pluck(PluckSettings),
}

impl Generator {
    pub const KINDS: [&'static str; 6] = ["Basic", "Wavetable", "FM", "Additive", "Granular", "Pluck"];

    pub fn kind_index(&self) -> usize {
        match self {
            Generator::Basic { .. } => 0,
            Generator::Wavetable { .. } => 1,
            Generator::Fm(_) => 2,
            Generator::Additive(_) => 3,
            Generator::Granular { .. } => 4,
            Generator::Pluck(_) => 5,
        }
    }

    /// Default generator of a kind (non-RT).
    pub fn default_of_kind(kind: usize) -> Self {
        match kind {
            0 => Generator::Basic { wave: BasicWave::Saw, pulse_width: 0.5 },
            1 => Generator::Wavetable { cycle: cycles::soft_saw(), remove_dc: true, frames: vec![], position: 0.0 },
            2 => Generator::Fm(FmSettings::default()),
            3 => Generator::Additive(AdditiveSettings::default()),
            4 => Generator::Granular { settings: GranularSettings::default(), source: cycles::soft_saw() },
            _ => Generator::Pluck(PluckSettings::default()),
        }
    }

    /// All motion frames (the single cycle when there are none).
    pub fn frames(&self) -> Vec<&[f32]> {
        match self {
            Generator::Wavetable { cycle, frames, .. } => {
                if frames.is_empty() {
                    vec![cycle.as_slice()]
                } else {
                    frames.iter().map(|f| f.as_slice()).collect()
                }
            }
            _ => vec![],
        }
    }

    pub fn is_valid(&self) -> bool {
        let cycle_ok = |c: &[f32]| c.len() == CYCLE_LEN && c.iter().all(|s| s.is_finite() && s.abs() <= 4.0);
        match self {
            Generator::Basic { pulse_width, .. } => pulse_width.is_finite() && (0.05..=0.95).contains(pulse_width),
            Generator::Wavetable { cycle, frames, position, .. } => {
                cycle_ok(cycle) && frames.len() <= MAX_FRAMES && frames.iter().all(|f| cycle_ok(f)) && position.is_finite() && (0.0..=1.0).contains(position)
            }
            Generator::Fm(s) => s.is_valid(),
            Generator::Additive(s) => s.is_valid(),
            Generator::Granular { settings, source } => settings.is_valid() && cycle_ok(source),
            Generator::Pluck(s) => s.is_valid(),
        }
    }
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Layer {
    pub enabled: bool,
    pub generator: Generator,
    pub level_db: f32,
    pub pan: f32,
    pub coarse_semitones: i32,
    pub fine_cents: f32,
    /// `true` = deterministic phase 0 at note start, `false` = free-running.
    pub reset_phase: bool,
    /// Unison voices 1..=8 (basic, wavetable and FM layers).
    #[serde(default = "one_u8")]
    pub unison: u8,
    /// Unison detune spread in cents (±).
    #[serde(default = "default_unison_detune")]
    pub unison_detune_cents: f32,
    /// Unison stereo spread 0..1.
    #[serde(default = "half")]
    pub unison_spread: f32,
}

fn one_u8() -> u8 {
    1
}
fn default_unison_detune() -> f32 {
    12.0
}
fn half() -> f32 {
    0.5
}

impl Layer {
    pub fn with_generator(generator: Generator) -> Self {
        Self {
            enabled: true,
            generator,
            level_db: 0.0,
            pan: 0.0,
            coarse_semitones: 0,
            fine_cents: 0.0,
            reset_phase: true,
            unison: 1,
            unison_detune_cents: 12.0,
            unison_spread: 0.5,
        }
    }
    pub fn default_soft_saw() -> Self {
        Self::with_generator(Generator::Wavetable { cycle: cycles::soft_saw(), remove_dc: true, frames: vec![], position: 0.0 })
    }
    pub fn basic(wave: BasicWave) -> Self {
        Self::with_generator(Generator::Basic { wave, pulse_width: 0.5 })
    }
    pub fn disabled() -> Self {
        Self { enabled: false, ..Self::basic(BasicWave::Sine) }
    }
    pub fn is_valid(&self) -> bool {
        self.generator.is_valid()
            && self.level_db.is_finite()
            && (-60.0..=12.0).contains(&self.level_db)
            && self.pan.is_finite()
            && (-1.0..=1.0).contains(&self.pan)
            && (-48..=48).contains(&self.coarse_semitones)
            && self.fine_cents.is_finite()
            && (-100.0..=100.0).contains(&self.fine_cents)
            && (1..=MAX_UNISON as u8).contains(&self.unison)
            && self.unison_detune_cents.is_finite()
            && (0.0..=100.0).contains(&self.unison_detune_cents)
            && self.unison_spread.is_finite()
            && (0.0..=1.0).contains(&self.unison_spread)
    }
}

/// LFO shape.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LfoShape {
    #[default]
    Sine,
    Triangle,
    Saw,
    Square,
    SampleHold,
}

impl LfoShape {
    pub const ALL: [LfoShape; 5] = [LfoShape::Sine, LfoShape::Triangle, LfoShape::Saw, LfoShape::Square, LfoShape::SampleHold];
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }
    pub fn from_index(i: usize) -> Self {
        Self::ALL[i.min(4)]
    }
    pub fn label(self) -> &'static str {
        match self {
            LfoShape::Sine => "Sine",
            LfoShape::Triangle => "Triangle",
            LfoShape::Saw => "Saw",
            LfoShape::Square => "Square",
            LfoShape::SampleHold => "Sample & hold",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Lfo {
    pub shape: LfoShape,
    /// Rate in Hz when not synced (0.01..50).
    pub rate_hz: f32,
    /// Beat-synced rate in cycles per beat (0 = free-running Hz).
    pub sync_beats: f32,
    pub depth: f32,
    /// Retrigger phase on note-on (per-voice LFO) or run globally.
    pub per_voice: bool,
}

impl Default for Lfo {
    fn default() -> Self {
        Self { shape: LfoShape::Sine, rate_hz: 2.0, sync_beats: 0.0, depth: 0.0, per_voice: false }
    }
}

impl Lfo {
    pub fn is_valid(&self) -> bool {
        self.rate_hz.is_finite()
            && (0.01..=50.0).contains(&self.rate_hz)
            && self.sync_beats.is_finite()
            && (0.0..=16.0).contains(&self.sync_beats)
            && self.depth.is_finite()
            && (0.0..=1.0).contains(&self.depth)
    }
}

/// Modulation sources.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModSource {
    Lfo1,
    Lfo2,
    Lfo3,
    Lfo4,
    AmpEnv,
    FilterEnv,
    Velocity,
    KeyTrack,
    ModWheel,
    Aftertouch,
    /// Per-voice random value fixed at note-on.
    Random,
}

impl ModSource {
    pub const ALL: [ModSource; 11] = [
        ModSource::Lfo1,
        ModSource::Lfo2,
        ModSource::Lfo3,
        ModSource::Lfo4,
        ModSource::AmpEnv,
        ModSource::FilterEnv,
        ModSource::Velocity,
        ModSource::KeyTrack,
        ModSource::ModWheel,
        ModSource::Aftertouch,
        ModSource::Random,
    ];
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }
    pub fn from_index(i: usize) -> Self {
        Self::ALL[i.min(Self::ALL.len() - 1)]
    }
    pub fn label(self) -> &'static str {
        match self {
            ModSource::Lfo1 => "LFO 1",
            ModSource::Lfo2 => "LFO 2",
            ModSource::Lfo3 => "LFO 3",
            ModSource::Lfo4 => "LFO 4",
            ModSource::AmpEnv => "Amp env",
            ModSource::FilterEnv => "Filter env",
            ModSource::Velocity => "Velocity",
            ModSource::KeyTrack => "Key",
            ModSource::ModWheel => "Mod wheel",
            ModSource::Aftertouch => "Aftertouch",
            ModSource::Random => "Random",
        }
    }
}

/// Modulation targets (continuous, applied at control rate per voice).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModTarget {
    /// Semitones (±24 at full depth).
    Pitch,
    /// Octaves (±4 at full depth).
    Cutoff,
    Resonance,
    /// Linear gain (±1 at full depth, clamped ≥ 0).
    Amp,
    Pan,
    /// Wavetable motion position (±1).
    WavePosition,
    PulseWidth,
    /// FM index scale (±1).
    FmIndex,
    GrainPosition,
    LayerLevel1,
    LayerLevel2,
    LayerLevel3,
    LayerLevel4,
}

impl ModTarget {
    pub const ALL: [ModTarget; 13] = [
        ModTarget::Pitch,
        ModTarget::Cutoff,
        ModTarget::Resonance,
        ModTarget::Amp,
        ModTarget::Pan,
        ModTarget::WavePosition,
        ModTarget::PulseWidth,
        ModTarget::FmIndex,
        ModTarget::GrainPosition,
        ModTarget::LayerLevel1,
        ModTarget::LayerLevel2,
        ModTarget::LayerLevel3,
        ModTarget::LayerLevel4,
    ];
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }
    pub fn from_index(i: usize) -> Self {
        Self::ALL[i.min(Self::ALL.len() - 1)]
    }
    pub fn label(self) -> &'static str {
        match self {
            ModTarget::Pitch => "Pitch",
            ModTarget::Cutoff => "Cutoff",
            ModTarget::Resonance => "Resonance",
            ModTarget::Amp => "Volume",
            ModTarget::Pan => "Pan",
            ModTarget::WavePosition => "Wave motion",
            ModTarget::PulseWidth => "Pulse width",
            ModTarget::FmIndex => "FM amount",
            ModTarget::GrainPosition => "Grain position",
            ModTarget::LayerLevel1 => "Layer 1 level",
            ModTarget::LayerLevel2 => "Layer 2 level",
            ModTarget::LayerLevel3 => "Layer 3 level",
            ModTarget::LayerLevel4 => "Layer 4 level",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct ModRoute {
    pub source: ModSource,
    pub target: ModTarget,
    /// Depth -1..1 (sign = polarity).
    pub depth: f32,
    pub enabled: bool,
}

impl ModRoute {
    pub fn is_valid(&self) -> bool {
        self.depth.is_finite() && (-1.0..=1.0).contains(&self.depth)
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct FilterSettings {
    pub mode: FilterMode,
    pub cutoff_hz: f32,
    /// Resonance Q 0.5..20.
    pub resonance: f32,
    /// Filter envelope amount in octaves [-8, 8].
    pub env_octaves: f32,
    /// Key tracking 0..1 (1 = cutoff follows pitch fully).
    pub key_track: f32,
    pub enabled: bool,
}

impl Default for FilterSettings {
    fn default() -> Self {
        // Open low-pass per spec default patch.
        Self { mode: FilterMode::LowPass, cutoff_hz: 20_000.0, resonance: 0.707, env_octaves: 0.0, key_track: 0.0, enabled: true }
    }
}

impl FilterSettings {
    pub fn is_valid(&self) -> bool {
        self.cutoff_hz.is_finite()
            && (20.0..=20_000.0).contains(&self.cutoff_hz)
            && self.resonance.is_finite()
            && (0.5..=20.0).contains(&self.resonance)
            && self.env_octaves.is_finite()
            && (-8.0..=8.0).contains(&self.env_octaves)
            && self.key_track.is_finite()
            && (0.0..=1.0).contains(&self.key_track)
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Tuning {
    pub a4_hz: f32,
    pub transpose_semitones: i32,
    pub detune_cents: f32,
    pub bend_range_semitones: f32,
}

impl Default for Tuning {
    fn default() -> Self {
        Self { a4_hz: 440.0, transpose_semitones: 0, detune_cents: 0.0, bend_range_semitones: 2.0 }
    }
}

impl Tuning {
    pub fn is_valid(&self) -> bool {
        self.a4_hz.is_finite()
            && (400.0..=480.0).contains(&self.a4_hz)
            && (-48..=48).contains(&self.transpose_semitones)
            && self.detune_cents.is_finite()
            && (-100.0..=100.0).contains(&self.detune_cents)
            && self.bend_range_semitones.is_finite()
            && (0.0..=48.0).contains(&self.bend_range_semitones)
    }
}

/// The complete synth patch.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Patch {
    pub version: u32,
    pub name: String,
    pub layers: Vec<Layer>,
    pub amp_env: AdsrParams,
    pub filter: FilterSettings,
    pub filter_env: AdsrParams,
    pub voice_mode: VoiceMode,
    pub portamento_s: f32,
    pub max_voices: u32,
    pub tuning: Tuning,
    /// Output gain in dB applied after voice summation.
    pub master_db: f32,
    /// Velocity → amplitude sensitivity 0..1 (0 = ignore velocity).
    pub velocity_sensitivity: f32,
    #[serde(default = "default_lfos")]
    pub lfos: Vec<Lfo>,
    #[serde(default)]
    pub mod_routes: Vec<ModRoute>,
    /// Randomization provenance and locks (persisted with the sound).
    #[serde(default)]
    pub mutation: MutationInfo,
    /// Optional modular graph (SYN-07); empty = pass-through.
    #[serde(default)]
    pub graph: crate::graph::Graph,
}

/// Randomization bookkeeping stored in the patch (PROJECT_FORMAT.md §4).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct MutationInfo {
    pub algorithm_version: u32,
    pub seed: u64,
    /// Chaos 0..100 used for the last mutation.
    pub chaos: f32,
    #[serde(default)]
    pub locks: Locks,
}

impl Default for MutationInfo {
    fn default() -> Self {
        Self { algorithm_version: crate::mutate::ALGORITHM_VERSION, seed: 0, chaos: 35.0, locks: Locks::default() }
    }
}

/// What randomization must not touch.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Locks {
    /// Locked semantic categories (see `ParamCategory`); stored as stable strings.
    pub categories: Vec<String>,
    /// Locked individual parameter ids.
    pub params: Vec<u32>,
}

impl Default for Locks {
    fn default() -> Self {
        // DSP.md §4: master output safety, tuning/root pitch and routing locked by default.
        Self { categories: vec!["safety".into(), "routing".into(), "pitch".into()], params: vec![] }
    }
}

impl Locks {
    pub fn category_key(c: rawdawg_types::params::ParamCategory) -> &'static str {
        use rawdawg_types::params::ParamCategory::*;
        match c {
            Wave => "wave",
            Pitch => "pitch",
            Envelope => "envelope",
            Filter => "filter",
            Modulation => "modulation",
            Effects => "effects",
            Topology => "topology",
            Mix => "mix",
            Safety => "safety",
            Routing => "routing",
        }
    }
    pub fn is_category_locked(&self, c: rawdawg_types::params::ParamCategory) -> bool {
        self.categories.iter().any(|k| k == Self::category_key(c))
    }
    pub fn is_param_locked(&self, d: &rawdawg_types::ParamDescriptor) -> bool {
        self.params.contains(&d.id.0) || self.is_category_locked(d.category)
    }
    pub fn set_category(&mut self, c: rawdawg_types::params::ParamCategory, locked: bool) {
        let k = Self::category_key(c);
        self.categories.retain(|x| x != k);
        if locked {
            self.categories.push(k.to_owned());
        }
    }
    pub fn set_param(&mut self, id: u32, locked: bool) {
        self.params.retain(|x| *x != id);
        if locked {
            self.params.push(id);
        }
    }
}

fn default_lfos() -> Vec<Lfo> {
    vec![Lfo::default(); MAX_LFOS]
}

impl Default for Patch {
    fn default() -> Self {
        Self {
            version: PATCH_VERSION,
            name: "Init".to_owned(),
            layers: vec![Layer::default_soft_saw()],
            amp_env: AdsrParams::DEFAULT,
            filter: FilterSettings::default(),
            filter_env: AdsrParams { attack_s: 0.005, decay_s: 0.3, sustain: 0.0, release_s: 0.2 },
            voice_mode: VoiceMode::Poly,
            portamento_s: 0.0,
            max_voices: 32,
            tuning: Tuning::default(),
            master_db: -10.0,
            velocity_sensitivity: 0.7,
            lfos: default_lfos(),
            mod_routes: vec![],
            mutation: MutationInfo::default(),
            graph: crate::graph::Graph::default(),
        }
    }
}

impl Patch {
    /// Validate ranges and structure.
    pub fn validate(&self) -> Result<(), String> {
        if self.version == 0 || self.version > PATCH_VERSION {
            return Err(format!("unsupported patch version {}", self.version));
        }
        if self.layers.is_empty() || self.layers.len() > MAX_LAYERS {
            return Err(format!("patch must have 1..={MAX_LAYERS} layers"));
        }
        for (i, l) in self.layers.iter().enumerate() {
            if !l.is_valid() {
                return Err(format!("layer {i} has invalid values"));
            }
        }
        if !self.filter.is_valid() {
            return Err("filter settings out of range".into());
        }
        if !self.tuning.is_valid() {
            return Err("tuning out of range".into());
        }
        if !(1..=64).contains(&self.max_voices) {
            return Err("max_voices must be 1..=64".into());
        }
        if !self.portamento_s.is_finite() || !(0.0..=10.0).contains(&self.portamento_s) {
            return Err("portamento out of range".into());
        }
        if !self.master_db.is_finite() || !(-60.0..=12.0).contains(&self.master_db) {
            return Err("master gain out of range".into());
        }
        self.graph.validate().map_err(|e| format!("graph: {e}"))?;
        if !self.velocity_sensitivity.is_finite() || !(0.0..=1.0).contains(&self.velocity_sensitivity) {
            return Err("velocity sensitivity out of range".into());
        }
        for e in [self.amp_env, self.filter_env] {
            if e != e.clamped() {
                return Err("envelope out of range".into());
            }
        }
        if self.lfos.len() != MAX_LFOS || self.lfos.iter().any(|l| !l.is_valid()) {
            return Err(format!("patch must have exactly {MAX_LFOS} valid LFOs"));
        }
        if self.mod_routes.len() > MAX_MOD_ROUTES || self.mod_routes.iter().any(|r| !r.is_valid()) {
            return Err(format!("at most {MAX_MOD_ROUTES} valid modulation routes"));
        }
        if !self.mutation.chaos.is_finite() || !(0.0..=100.0).contains(&self.mutation.chaos) {
            return Err("chaos out of range".into());
        }
        let osc_budget: u32 = self.layers.iter().filter(|l| l.enabled).map(|l| u32::from(l.unison)).sum::<u32>() * self.max_voices;
        if osc_budget > rawdawg_types::limits::EngineLimits::DEFAULT.max_voice_oscillators * 4 {
            return Err(format!("voice × unison budget exceeded ({osc_budget})"));
        }
        Ok(())
    }

    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).expect("patch serializes")
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > 16 * 1024 * 1024 {
            return Err("patch too large".into());
        }
        let p: Patch = serde_json::from_slice(bytes).map_err(|e| format!("invalid patch JSON: {e}"))?;
        p.validate()?;
        Ok(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_patch_is_valid_and_round_trips() {
        let p = Patch::default();
        p.validate().unwrap();
        let bytes = p.to_json();
        let back = Patch::from_json(&bytes).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn invalid_patches_are_rejected() {
        let p = Patch { max_voices: 0, ..Patch::default() };
        assert!(p.validate().is_err());
        let p = Patch { layers: vec![], ..Patch::default() };
        assert!(p.validate().is_err());
        let p = Patch {
            layers: vec![Layer {
                generator: Generator::Wavetable { cycle: vec![f32::NAN; CYCLE_LEN], remove_dc: true, frames: vec![], position: 0.0 },
                ..Layer::default_soft_saw()
            }],
            ..Patch::default()
        };
        assert!(p.validate().is_err());
        let p = Patch { version: 99, ..Patch::default() };
        assert!(Patch::from_json(&p.to_json()).is_err());
        assert!(Patch::from_json(b"not json").is_err());
    }
}

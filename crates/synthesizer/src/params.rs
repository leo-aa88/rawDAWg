//! Stable parameter descriptors for `native.synth`. IDs are never reused.
//!
//! Layout: globals 1..99; per-layer core block `100 + layer*20 + offset`;
//! per-layer generator block `200 + layer*40 + offset`; LFO block
//! `400 + lfo*10 + offset`; modulation routes `500 + route*4 + offset`.

use rawdawg_types::params::{ParamCategory, ParamDescriptor, ParamMapping, ParamUnit};

pub const MASTER_GAIN: u32 = 1;
pub const VELOCITY_SENS: u32 = 2;
pub const AMP_ATTACK: u32 = 10;
pub const AMP_DECAY: u32 = 11;
pub const AMP_SUSTAIN: u32 = 12;
pub const AMP_RELEASE: u32 = 13;
pub const FILTER_CUTOFF: u32 = 20;
pub const FILTER_RESONANCE: u32 = 21;
pub const FILTER_MODE: u32 = 22;
pub const FILTER_ENV_AMOUNT: u32 = 23;
pub const FILTER_KEYTRACK: u32 = 24;
pub const FILTER_ENABLED: u32 = 25;
pub const FENV_ATTACK: u32 = 30;
pub const FENV_DECAY: u32 = 31;
pub const FENV_SUSTAIN: u32 = 32;
pub const FENV_RELEASE: u32 = 33;
pub const VOICE_MODE: u32 = 40;
pub const PORTAMENTO: u32 = 41;
pub const BEND_RANGE: u32 = 42;
pub const MAX_VOICES: u32 = 43;
pub const TRANSPOSE: u32 = 44;
pub const DETUNE: u32 = 45;

/// Layer core block.
pub const LAYER_BASE: u32 = 100;
pub const LAYER_STRIDE: u32 = 20;
pub const L_ENABLED: u32 = 0;
pub const L_WAVE: u32 = 1;
pub const L_PULSE_WIDTH: u32 = 2;
pub const L_LEVEL: u32 = 3;
pub const L_PAN: u32 = 4;
pub const L_COARSE: u32 = 5;
pub const L_FINE: u32 = 6;
pub const L_RESET_PHASE: u32 = 7;
pub const L_UNISON: u32 = 8;
pub const L_UNISON_DETUNE: u32 = 9;
pub const L_UNISON_SPREAD: u32 = 10;
pub const L_WT_POSITION: u32 = 11;
/// Generator kind (read-only through the RT path: switching allocates).
pub const L_KIND: u32 = 12;
pub const LAYER_CORE_COUNT: usize = 13;

/// Layer generator block.
pub const GEN_BASE: u32 = 200;
pub const GEN_STRIDE: u32 = 40;
pub const G_FM_RATIO1: u32 = 0;
pub const G_FM_RATIO2: u32 = 1;
pub const G_FM_RATIO3: u32 = 2;
pub const G_FM_RATIO4: u32 = 3;
pub const G_FM_LEVEL1: u32 = 4;
pub const G_FM_LEVEL2: u32 = 5;
pub const G_FM_LEVEL3: u32 = 6;
pub const G_FM_LEVEL4: u32 = 7;
pub const G_FM_FEEDBACK: u32 = 8;
pub const G_FM_ALGORITHM: u32 = 9;
pub const G_ADD_STRETCH: u32 = 10;
pub const G_ADD_DETUNED: u32 = 11;
pub const G_GRAIN_SIZE: u32 = 12;
pub const G_GRAIN_DENSITY: u32 = 13;
pub const G_GRAIN_JITTER: u32 = 14;
pub const G_GRAIN_SPREAD: u32 = 15;
pub const G_GRAIN_WINDOW: u32 = 16;
pub const G_PLUCK_DAMPING: u32 = 17;
pub const G_PLUCK_BRIGHTNESS: u32 = 18;
pub const G_PLUCK_EXCITATION: u32 = 19;
pub const GEN_COUNT: usize = 20;

/// LFO block.
pub const LFO_BASE: u32 = 400;
pub const LFO_STRIDE: u32 = 10;
pub const LFO_SHAPE: u32 = 0;
pub const LFO_RATE: u32 = 1;
pub const LFO_SYNC: u32 = 2;
pub const LFO_DEPTH: u32 = 3;
pub const LFO_PER_VOICE: u32 = 4;
pub const LFO_COUNT: usize = 5;

/// Modulation route block.
pub const MOD_BASE: u32 = 500;
pub const MOD_STRIDE: u32 = 4;
pub const MOD_SOURCE: u32 = 0;
pub const MOD_TARGET: u32 = 1;
pub const MOD_DEPTH: u32 = 2;
pub const MOD_ENABLED: u32 = 3;
pub const MOD_COUNT: usize = 4;

pub fn layer_param(layer: u32, offset: u32) -> u32 {
    LAYER_BASE + layer * LAYER_STRIDE + offset
}
pub fn gen_param(layer: u32, offset: u32) -> u32 {
    GEN_BASE + layer * GEN_STRIDE + offset
}
pub fn lfo_param(lfo: u32, offset: u32) -> u32 {
    LFO_BASE + lfo * LFO_STRIDE + offset
}
pub fn mod_param(route: u32, offset: u32) -> u32 {
    MOD_BASE + route * MOD_STRIDE + offset
}

/// Which block an id belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ParamBlock {
    Global,
    Layer { layer: usize, offset: u32 },
    Gen { layer: usize, offset: u32 },
    Lfo { lfo: usize, offset: u32 },
    Mod { route: usize, offset: u32 },
}

pub fn decode(id: u32) -> Option<ParamBlock> {
    if id < LAYER_BASE {
        return Some(ParamBlock::Global);
    }
    if id < GEN_BASE {
        let rel = id - LAYER_BASE;
        let (layer, offset) = ((rel / LAYER_STRIDE) as usize, rel % LAYER_STRIDE);
        return (layer < crate::patch::MAX_LAYERS && (offset as usize) < LAYER_CORE_COUNT).then_some(ParamBlock::Layer { layer, offset });
    }
    if id < LFO_BASE {
        let rel = id - GEN_BASE;
        let (layer, offset) = ((rel / GEN_STRIDE) as usize, rel % GEN_STRIDE);
        return (layer < crate::patch::MAX_LAYERS && (offset as usize) < GEN_COUNT).then_some(ParamBlock::Gen { layer, offset });
    }
    if id < MOD_BASE {
        let rel = id - LFO_BASE;
        let (lfo, offset) = ((rel / LFO_STRIDE) as usize, rel % LFO_STRIDE);
        return (lfo < crate::patch::MAX_LFOS && (offset as usize) < LFO_COUNT).then_some(ParamBlock::Lfo { lfo, offset });
    }
    let rel = id - MOD_BASE;
    let (route, offset) = ((rel / MOD_STRIDE) as usize, rel % MOD_STRIDE);
    (route < crate::patch::MAX_MOD_ROUTES && (offset as usize) < MOD_COUNT).then_some(ParamBlock::Mod { route, offset })
}

/// Decode a layer core parameter id into (layer, offset) (compatibility helper).
pub fn decode_layer_param(id: u32) -> Option<(usize, u32)> {
    match decode(id) {
        Some(ParamBlock::Layer { layer, offset }) => Some((layer, offset)),
        _ => None,
    }
}

const fn time(id: u32, key: &'static str, name: &'static str, default_n: f32, cat: ParamCategory) -> ParamDescriptor {
    ParamDescriptor::continuous(id, key, name, ParamUnit::Seconds, ParamMapping::LogWithZero { min: 0.001, max: 30.0 }, default_n, cat).with_smoothing(0.0)
}

const GLOBAL: [ParamDescriptor; 22] = [
    ParamDescriptor::continuous(MASTER_GAIN, "master.gain", "Master", ParamUnit::Decibel, ParamMapping::Linear { min: -60.0, max: 12.0 }, 50.0 / 72.0, ParamCategory::Mix),
    ParamDescriptor::continuous(VELOCITY_SENS, "velocity.sensitivity", "Velocity", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 1.0 }, 0.7, ParamCategory::Envelope),
    time(AMP_ATTACK, "amp.attack", "Attack", 0.0, ParamCategory::Envelope),
    time(AMP_DECAY, "amp.decay", "Decay", 0.0, ParamCategory::Envelope),
    ParamDescriptor::continuous(AMP_SUSTAIN, "amp.sustain", "Sustain", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 1.0 }, 0.75, ParamCategory::Envelope)
        .with_smoothing(0.0),
    time(AMP_RELEASE, "amp.release", "Release", 0.0, ParamCategory::Envelope),
    ParamDescriptor::continuous(FILTER_CUTOFF, "filter.cutoff", "Cutoff", ParamUnit::Hertz, ParamMapping::Log { min: 20.0, max: 20_000.0 }, 1.0, ParamCategory::Filter),
    ParamDescriptor::continuous(FILTER_RESONANCE, "filter.resonance", "Resonance", ParamUnit::None, ParamMapping::Log { min: 0.5, max: 20.0 }, 0.0, ParamCategory::Filter),
    ParamDescriptor::continuous(FILTER_MODE, "filter.mode", "Filter Type", ParamUnit::None, ParamMapping::Discrete { steps: 4 }, 0.0, ParamCategory::Filter).stepped(),
    ParamDescriptor::continuous(FILTER_ENV_AMOUNT, "filter.env_amount", "Filter Env", ParamUnit::None, ParamMapping::Linear { min: -8.0, max: 8.0 }, 0.5, ParamCategory::Filter),
    ParamDescriptor::continuous(FILTER_KEYTRACK, "filter.key_track", "Key Track", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 1.0 }, 0.0, ParamCategory::Filter),
    ParamDescriptor::continuous(FILTER_ENABLED, "filter.enabled", "Filter On", ParamUnit::None, ParamMapping::Toggle, 1.0, ParamCategory::Filter).stepped(),
    time(FENV_ATTACK, "fenv.attack", "Filter Attack", 0.0, ParamCategory::Filter),
    time(FENV_DECAY, "fenv.decay", "Filter Decay", 0.0, ParamCategory::Filter),
    ParamDescriptor::continuous(FENV_SUSTAIN, "fenv.sustain", "Filter Sustain", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 1.0 }, 0.0, ParamCategory::Filter)
        .with_smoothing(0.0),
    time(FENV_RELEASE, "fenv.release", "Filter Release", 0.0, ParamCategory::Filter),
    ParamDescriptor::continuous(VOICE_MODE, "voice.mode", "Voice Mode", ParamUnit::None, ParamMapping::Discrete { steps: 3 }, 0.0, ParamCategory::Topology).stepped(),
    ParamDescriptor::continuous(PORTAMENTO, "voice.portamento", "Glide", ParamUnit::Seconds, ParamMapping::LogWithZero { min: 0.005, max: 10.0 }, 0.0, ParamCategory::Pitch)
        .with_smoothing(0.0),
    ParamDescriptor::continuous(BEND_RANGE, "tuning.bend_range", "Bend Range", ParamUnit::Semitones, ParamMapping::Discrete { steps: 49 }, 2.0 / 48.0, ParamCategory::Pitch)
        .stepped(),
    ParamDescriptor::continuous(MAX_VOICES, "voice.max", "Voices", ParamUnit::None, ParamMapping::Discrete { steps: 64 }, 31.0 / 63.0, ParamCategory::Topology).stepped(),
    ParamDescriptor::continuous(TRANSPOSE, "tuning.transpose", "Transpose", ParamUnit::Semitones, ParamMapping::Discrete { steps: 97 }, 0.5, ParamCategory::Pitch).stepped(),
    ParamDescriptor::continuous(DETUNE, "tuning.detune", "Detune", ParamUnit::Cents, ParamMapping::Linear { min: -100.0, max: 100.0 }, 0.5, ParamCategory::Pitch),
];

macro_rules! layer_block {
    ($l:literal) => {
        [
            ParamDescriptor::continuous(
                LAYER_BASE + $l * LAYER_STRIDE + L_ENABLED,
                concat!("layer", $l, ".enabled"),
                concat!("L", $l, " On"),
                ParamUnit::None,
                ParamMapping::Toggle,
                if $l == 0 { 1.0 } else { 0.0 },
                ParamCategory::Topology,
            )
            .stepped(),
            ParamDescriptor::continuous(
                LAYER_BASE + $l * LAYER_STRIDE + L_WAVE,
                concat!("layer", $l, ".wave"),
                concat!("L", $l, " Wave"),
                ParamUnit::None,
                ParamMapping::Discrete { steps: 4 },
                0.0,
                ParamCategory::Wave,
            )
            .stepped(),
            ParamDescriptor::continuous(
                LAYER_BASE + $l * LAYER_STRIDE + L_PULSE_WIDTH,
                concat!("layer", $l, ".pulse_width"),
                concat!("L", $l, " Pulse Width"),
                ParamUnit::Percent,
                ParamMapping::Linear { min: 0.05, max: 0.95 },
                0.5,
                ParamCategory::Wave,
            ),
            ParamDescriptor::continuous(
                LAYER_BASE + $l * LAYER_STRIDE + L_LEVEL,
                concat!("layer", $l, ".level"),
                concat!("L", $l, " Level"),
                ParamUnit::Decibel,
                ParamMapping::Linear { min: -60.0, max: 12.0 },
                60.0 / 72.0,
                ParamCategory::Mix,
            ),
            ParamDescriptor::continuous(
                LAYER_BASE + $l * LAYER_STRIDE + L_PAN,
                concat!("layer", $l, ".pan"),
                concat!("L", $l, " Pan"),
                ParamUnit::None,
                ParamMapping::Linear { min: -1.0, max: 1.0 },
                0.5,
                ParamCategory::Mix,
            ),
            ParamDescriptor::continuous(
                LAYER_BASE + $l * LAYER_STRIDE + L_COARSE,
                concat!("layer", $l, ".coarse"),
                concat!("L", $l, " Coarse"),
                ParamUnit::Semitones,
                ParamMapping::Discrete { steps: 97 },
                0.5,
                ParamCategory::Pitch,
            )
            .stepped(),
            ParamDescriptor::continuous(
                LAYER_BASE + $l * LAYER_STRIDE + L_FINE,
                concat!("layer", $l, ".fine"),
                concat!("L", $l, " Fine"),
                ParamUnit::Cents,
                ParamMapping::Linear { min: -100.0, max: 100.0 },
                0.5,
                ParamCategory::Pitch,
            ),
            ParamDescriptor::continuous(
                LAYER_BASE + $l * LAYER_STRIDE + L_RESET_PHASE,
                concat!("layer", $l, ".reset_phase"),
                concat!("L", $l, " Reset Phase"),
                ParamUnit::None,
                ParamMapping::Toggle,
                1.0,
                ParamCategory::Wave,
            )
            .stepped(),
            ParamDescriptor::continuous(
                LAYER_BASE + $l * LAYER_STRIDE + L_UNISON,
                concat!("layer", $l, ".unison"),
                concat!("L", $l, " Unison"),
                ParamUnit::None,
                ParamMapping::Discrete { steps: 8 },
                0.0,
                ParamCategory::Wave,
            )
            .stepped(),
            ParamDescriptor::continuous(
                LAYER_BASE + $l * LAYER_STRIDE + L_UNISON_DETUNE,
                concat!("layer", $l, ".unison_detune"),
                concat!("L", $l, " Unison Detune"),
                ParamUnit::Cents,
                ParamMapping::Linear { min: 0.0, max: 100.0 },
                0.12,
                ParamCategory::Wave,
            ),
            ParamDescriptor::continuous(
                LAYER_BASE + $l * LAYER_STRIDE + L_UNISON_SPREAD,
                concat!("layer", $l, ".unison_spread"),
                concat!("L", $l, " Unison Spread"),
                ParamUnit::Percent,
                ParamMapping::Linear { min: 0.0, max: 1.0 },
                0.5,
                ParamCategory::Mix,
            ),
            ParamDescriptor::continuous(
                LAYER_BASE + $l * LAYER_STRIDE + L_WT_POSITION,
                concat!("layer", $l, ".wt_position"),
                concat!("L", $l, " Motion"),
                ParamUnit::Percent,
                ParamMapping::Linear { min: 0.0, max: 1.0 },
                0.0,
                ParamCategory::Wave,
            ),
            ParamDescriptor::continuous(
                LAYER_BASE + $l * LAYER_STRIDE + L_KIND,
                concat!("layer", $l, ".kind"),
                concat!("L", $l, " Generator"),
                ParamUnit::None,
                ParamMapping::Discrete { steps: 6 },
                0.2,
                ParamCategory::Topology,
            )
            .stepped()
            .read_only(),
        ]
    };
}

macro_rules! gen_block {
    ($l:literal) => {
        [
            ParamDescriptor::continuous(
                GEN_BASE + $l * GEN_STRIDE + G_FM_RATIO1,
                concat!("layer", $l, ".fm.ratio1"),
                concat!("L", $l, " FM Ratio 1"),
                ParamUnit::Ratio,
                ParamMapping::Log { min: 0.25, max: 16.0 },
                1.0 / 3.0,
                ParamCategory::Pitch,
            ),
            ParamDescriptor::continuous(
                GEN_BASE + $l * GEN_STRIDE + G_FM_RATIO2,
                concat!("layer", $l, ".fm.ratio2"),
                concat!("L", $l, " FM Ratio 2"),
                ParamUnit::Ratio,
                ParamMapping::Log { min: 0.25, max: 16.0 },
                0.5,
                ParamCategory::Pitch,
            ),
            ParamDescriptor::continuous(
                GEN_BASE + $l * GEN_STRIDE + G_FM_RATIO3,
                concat!("layer", $l, ".fm.ratio3"),
                concat!("L", $l, " FM Ratio 3"),
                ParamUnit::Ratio,
                ParamMapping::Log { min: 0.25, max: 16.0 },
                1.0 / 3.0,
                ParamCategory::Pitch,
            ),
            ParamDescriptor::continuous(
                GEN_BASE + $l * GEN_STRIDE + G_FM_RATIO4,
                concat!("layer", $l, ".fm.ratio4"),
                concat!("L", $l, " FM Ratio 4"),
                ParamUnit::Ratio,
                ParamMapping::Log { min: 0.25, max: 16.0 },
                1.0 / 3.0,
                ParamCategory::Pitch,
            ),
            ParamDescriptor::continuous(
                GEN_BASE + $l * GEN_STRIDE + G_FM_LEVEL1,
                concat!("layer", $l, ".fm.level1"),
                concat!("L", $l, " FM Level 1"),
                ParamUnit::Percent,
                ParamMapping::Linear { min: 0.0, max: 1.0 },
                1.0,
                ParamCategory::Wave,
            ),
            ParamDescriptor::continuous(
                GEN_BASE + $l * GEN_STRIDE + G_FM_LEVEL2,
                concat!("layer", $l, ".fm.level2"),
                concat!("L", $l, " FM Level 2"),
                ParamUnit::Percent,
                ParamMapping::Linear { min: 0.0, max: 1.0 },
                0.35,
                ParamCategory::Wave,
            ),
            ParamDescriptor::continuous(
                GEN_BASE + $l * GEN_STRIDE + G_FM_LEVEL3,
                concat!("layer", $l, ".fm.level3"),
                concat!("L", $l, " FM Level 3"),
                ParamUnit::Percent,
                ParamMapping::Linear { min: 0.0, max: 1.0 },
                0.0,
                ParamCategory::Wave,
            ),
            ParamDescriptor::continuous(
                GEN_BASE + $l * GEN_STRIDE + G_FM_LEVEL4,
                concat!("layer", $l, ".fm.level4"),
                concat!("L", $l, " FM Level 4"),
                ParamUnit::Percent,
                ParamMapping::Linear { min: 0.0, max: 1.0 },
                0.0,
                ParamCategory::Wave,
            ),
            ParamDescriptor::continuous(
                GEN_BASE + $l * GEN_STRIDE + G_FM_FEEDBACK,
                concat!("layer", $l, ".fm.feedback"),
                concat!("L", $l, " FM Feedback"),
                ParamUnit::Percent,
                ParamMapping::Linear { min: 0.0, max: 1.0 },
                0.0,
                ParamCategory::Wave,
            ),
            ParamDescriptor::continuous(
                GEN_BASE + $l * GEN_STRIDE + G_FM_ALGORITHM,
                concat!("layer", $l, ".fm.algorithm"),
                concat!("L", $l, " FM Algorithm"),
                ParamUnit::None,
                ParamMapping::Discrete { steps: 4 },
                0.0,
                ParamCategory::Topology,
            )
            .stepped(),
            ParamDescriptor::continuous(
                GEN_BASE + $l * GEN_STRIDE + G_ADD_STRETCH,
                concat!("layer", $l, ".add.stretch"),
                concat!("L", $l, " Partial Stretch"),
                ParamUnit::None,
                ParamMapping::Linear { min: -0.05, max: 0.05 },
                0.5,
                ParamCategory::Pitch,
            ),
            ParamDescriptor::continuous(
                GEN_BASE + $l * GEN_STRIDE + G_ADD_DETUNED,
                concat!("layer", $l, ".add.detuned"),
                concat!("L", $l, " Detuned Partials"),
                ParamUnit::None,
                ParamMapping::Toggle,
                0.0,
                ParamCategory::Wave,
            )
            .stepped(),
            ParamDescriptor::continuous(
                GEN_BASE + $l * GEN_STRIDE + G_GRAIN_SIZE,
                concat!("layer", $l, ".grain.size"),
                concat!("L", $l, " Grain Size"),
                ParamUnit::Milliseconds,
                ParamMapping::Log { min: 5.0, max: 500.0 },
                0.54,
                ParamCategory::Wave,
            ),
            ParamDescriptor::continuous(
                GEN_BASE + $l * GEN_STRIDE + G_GRAIN_DENSITY,
                concat!("layer", $l, ".grain.density"),
                concat!("L", $l, " Grain Density"),
                ParamUnit::Hertz,
                ParamMapping::Log { min: 1.0, max: 400.0 },
                0.62,
                ParamCategory::Wave,
            ),
            ParamDescriptor::continuous(
                GEN_BASE + $l * GEN_STRIDE + G_GRAIN_JITTER,
                concat!("layer", $l, ".grain.jitter"),
                concat!("L", $l, " Grain Jitter"),
                ParamUnit::Percent,
                ParamMapping::Linear { min: 0.0, max: 1.0 },
                0.1,
                ParamCategory::Wave,
            ),
            ParamDescriptor::continuous(
                GEN_BASE + $l * GEN_STRIDE + G_GRAIN_SPREAD,
                concat!("layer", $l, ".grain.spread"),
                concat!("L", $l, " Grain Pitch Spread"),
                ParamUnit::Semitones,
                ParamMapping::Linear { min: 0.0, max: 24.0 },
                0.0,
                ParamCategory::Pitch,
            ),
            ParamDescriptor::continuous(
                GEN_BASE + $l * GEN_STRIDE + G_GRAIN_WINDOW,
                concat!("layer", $l, ".grain.window"),
                concat!("L", $l, " Grain Window"),
                ParamUnit::None,
                ParamMapping::Discrete { steps: 3 },
                0.0,
                ParamCategory::Wave,
            )
            .stepped(),
            ParamDescriptor::continuous(
                GEN_BASE + $l * GEN_STRIDE + G_PLUCK_DAMPING,
                concat!("layer", $l, ".pluck.damping"),
                concat!("L", $l, " String Damping"),
                ParamUnit::Percent,
                ParamMapping::Linear { min: 0.0, max: 1.0 },
                0.2,
                ParamCategory::Envelope,
            ),
            ParamDescriptor::continuous(
                GEN_BASE + $l * GEN_STRIDE + G_PLUCK_BRIGHTNESS,
                concat!("layer", $l, ".pluck.brightness"),
                concat!("L", $l, " String Brightness"),
                ParamUnit::Percent,
                ParamMapping::Linear { min: 0.0, max: 1.0 },
                0.6,
                ParamCategory::Filter,
            ),
            ParamDescriptor::continuous(
                GEN_BASE + $l * GEN_STRIDE + G_PLUCK_EXCITATION,
                concat!("layer", $l, ".pluck.excitation"),
                concat!("L", $l, " Pluck Type"),
                ParamUnit::None,
                ParamMapping::Discrete { steps: 3 },
                0.0,
                ParamCategory::Wave,
            )
            .stepped(),
        ]
    };
}

macro_rules! lfo_block {
    ($n:literal) => {
        [
            ParamDescriptor::continuous(
                LFO_BASE + $n * LFO_STRIDE + LFO_SHAPE,
                concat!("lfo", $n, ".shape"),
                concat!("LFO ", $n, " Shape"),
                ParamUnit::None,
                ParamMapping::Discrete { steps: 5 },
                0.0,
                ParamCategory::Modulation,
            )
            .stepped(),
            ParamDescriptor::continuous(
                LFO_BASE + $n * LFO_STRIDE + LFO_RATE,
                concat!("lfo", $n, ".rate"),
                concat!("LFO ", $n, " Rate"),
                ParamUnit::Hertz,
                ParamMapping::Log { min: 0.01, max: 50.0 },
                0.62,
                ParamCategory::Modulation,
            ),
            ParamDescriptor::continuous(
                LFO_BASE + $n * LFO_STRIDE + LFO_SYNC,
                concat!("lfo", $n, ".sync"),
                concat!("LFO ", $n, " Beat Sync"),
                ParamUnit::None,
                ParamMapping::LogWithZero { min: 0.0625, max: 16.0 },
                0.0,
                ParamCategory::Modulation,
            ),
            ParamDescriptor::continuous(
                LFO_BASE + $n * LFO_STRIDE + LFO_DEPTH,
                concat!("lfo", $n, ".depth"),
                concat!("LFO ", $n, " Depth"),
                ParamUnit::Percent,
                ParamMapping::Linear { min: 0.0, max: 1.0 },
                0.0,
                ParamCategory::Modulation,
            ),
            ParamDescriptor::continuous(
                LFO_BASE + $n * LFO_STRIDE + LFO_PER_VOICE,
                concat!("lfo", $n, ".per_voice"),
                concat!("LFO ", $n, " Per Voice"),
                ParamUnit::None,
                ParamMapping::Toggle,
                0.0,
                ParamCategory::Modulation,
            )
            .stepped(),
        ]
    };
}

macro_rules! mod_block {
    ($n:literal) => {
        [
            ParamDescriptor::continuous(
                MOD_BASE + $n * MOD_STRIDE + MOD_SOURCE,
                concat!("mod", $n, ".source"),
                concat!("Mod ", $n, " Source"),
                ParamUnit::None,
                ParamMapping::Discrete { steps: 11 },
                0.0,
                ParamCategory::Modulation,
            )
            .stepped()
            .hidden(),
            ParamDescriptor::continuous(
                MOD_BASE + $n * MOD_STRIDE + MOD_TARGET,
                concat!("mod", $n, ".target"),
                concat!("Mod ", $n, " Target"),
                ParamUnit::None,
                ParamMapping::Discrete { steps: 13 },
                0.0,
                ParamCategory::Modulation,
            )
            .stepped()
            .hidden(),
            ParamDescriptor::continuous(
                MOD_BASE + $n * MOD_STRIDE + MOD_DEPTH,
                concat!("mod", $n, ".depth"),
                concat!("Mod ", $n, " Depth"),
                ParamUnit::Percent,
                ParamMapping::Linear { min: -1.0, max: 1.0 },
                0.5,
                ParamCategory::Modulation,
            ),
            ParamDescriptor::continuous(
                MOD_BASE + $n * MOD_STRIDE + MOD_ENABLED,
                concat!("mod", $n, ".enabled"),
                concat!("Mod ", $n, " On"),
                ParamUnit::None,
                ParamMapping::Toggle,
                0.0,
                ParamCategory::Modulation,
            )
            .stepped()
            .hidden(),
        ]
    };
}

const L0: [ParamDescriptor; LAYER_CORE_COUNT] = layer_block!(0);
const L1: [ParamDescriptor; LAYER_CORE_COUNT] = layer_block!(1);
const L2: [ParamDescriptor; LAYER_CORE_COUNT] = layer_block!(2);
const L3: [ParamDescriptor; LAYER_CORE_COUNT] = layer_block!(3);
const G0: [ParamDescriptor; GEN_COUNT] = gen_block!(0);
const G1: [ParamDescriptor; GEN_COUNT] = gen_block!(1);
const G2: [ParamDescriptor; GEN_COUNT] = gen_block!(2);
const G3: [ParamDescriptor; GEN_COUNT] = gen_block!(3);
const F0: [ParamDescriptor; LFO_COUNT] = lfo_block!(0);
const F1: [ParamDescriptor; LFO_COUNT] = lfo_block!(1);
const F2: [ParamDescriptor; LFO_COUNT] = lfo_block!(2);
const F3: [ParamDescriptor; LFO_COUNT] = lfo_block!(3);
const M: [[ParamDescriptor; MOD_COUNT]; 16] = [
    mod_block!(0),
    mod_block!(1),
    mod_block!(2),
    mod_block!(3),
    mod_block!(4),
    mod_block!(5),
    mod_block!(6),
    mod_block!(7),
    mod_block!(8),
    mod_block!(9),
    mod_block!(10),
    mod_block!(11),
    mod_block!(12),
    mod_block!(13),
    mod_block!(14),
    mod_block!(15),
];

pub const DESCRIPTOR_COUNT: usize = 22 + 4 * LAYER_CORE_COUNT + 4 * GEN_COUNT + 4 * LFO_COUNT + 16 * MOD_COUNT;

const fn concat_all() -> [ParamDescriptor; DESCRIPTOR_COUNT] {
    let mut out = [GLOBAL[0]; DESCRIPTOR_COUNT];
    let mut n = 0;
    let mut i = 0;
    while i < 22 {
        out[n] = GLOBAL[i];
        n += 1;
        i += 1;
    }
    let layers = [&L0, &L1, &L2, &L3];
    let mut l = 0;
    while l < 4 {
        let mut j = 0;
        while j < LAYER_CORE_COUNT {
            out[n] = layers[l][j];
            n += 1;
            j += 1;
        }
        l += 1;
    }
    let gens = [&G0, &G1, &G2, &G3];
    let mut l = 0;
    while l < 4 {
        let mut j = 0;
        while j < GEN_COUNT {
            out[n] = gens[l][j];
            n += 1;
            j += 1;
        }
        l += 1;
    }
    let lfos = [&F0, &F1, &F2, &F3];
    let mut l = 0;
    while l < 4 {
        let mut j = 0;
        while j < LFO_COUNT {
            out[n] = lfos[l][j];
            n += 1;
            j += 1;
        }
        l += 1;
    }
    let mut r = 0;
    while r < 16 {
        let mut j = 0;
        while j < MOD_COUNT {
            out[n] = M[r][j];
            n += 1;
            j += 1;
        }
        r += 1;
    }
    out
}

/// All descriptors (static, no allocation at query time).
pub static DESCRIPTORS: [ParamDescriptor; DESCRIPTOR_COUNT] = concat_all();

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn descriptors_are_unique_and_well_formed() {
        let mut ids = HashSet::new();
        let mut keys = HashSet::new();
        for d in DESCRIPTORS.iter() {
            assert!(d.is_well_formed(), "{}", d.key);
            assert!(ids.insert(d.id.0), "duplicate id {:?}", d.id);
            assert!(keys.insert(d.key), "duplicate key {}", d.key);
            assert!(decode(d.id.0).is_some(), "undecodable id {}", d.id.0);
        }
        assert_eq!(DESCRIPTORS.len(), 238);
        assert_eq!(decode_layer_param(layer_param(2, L_PAN)), Some((2, L_PAN)));
        assert_eq!(decode_layer_param(5), None);
        assert_eq!(decode(layer_param(4, 0)), None);
        assert_eq!(decode(gen_param(1, G_PLUCK_DAMPING)), Some(ParamBlock::Gen { layer: 1, offset: G_PLUCK_DAMPING }));
        assert_eq!(decode(lfo_param(3, LFO_DEPTH)), Some(ParamBlock::Lfo { lfo: 3, offset: LFO_DEPTH }));
        assert_eq!(decode(mod_param(15, MOD_ENABLED)), Some(ParamBlock::Mod { route: 15, offset: MOD_ENABLED }));
        assert_eq!(decode(mod_param(16, 0)), None);
    }
}

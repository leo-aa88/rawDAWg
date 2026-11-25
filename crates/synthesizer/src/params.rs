//! Stable parameter descriptors for `native.synth`. IDs are never reused.

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
/// Layer parameters: `LAYER_BASE + layer * LAYER_STRIDE + offset`.
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

pub fn layer_param(layer: u32, offset: u32) -> u32 {
    LAYER_BASE + layer * LAYER_STRIDE + offset
}

/// Decode a layer parameter id into (layer, offset).
pub fn decode_layer_param(id: u32) -> Option<(usize, u32)> {
    if id < LAYER_BASE {
        return None;
    }
    let rel = id - LAYER_BASE;
    let layer = rel / LAYER_STRIDE;
    if layer as usize >= crate::patch::MAX_LAYERS {
        return None;
    }
    Some((layer as usize, rel % LAYER_STRIDE))
}

const fn time(id: u32, key: &'static str, name: &'static str, default_s: f32, cat: ParamCategory) -> ParamDescriptor {
    // log mapping 1 ms .. 30 s; default supplied in seconds, converted at runtime by caller tests
    ParamDescriptor::continuous(id, key, name, ParamUnit::Seconds, ParamMapping::LogWithZero { min: 0.001, max: 30.0 }, default_s, cat).with_smoothing(0.0)
}

macro_rules! layer_descs {
    ($l:literal) => {
        [
            ParamDescriptor::continuous(
                LAYER_BASE + $l * LAYER_STRIDE + L_ENABLED,
                concat!("layer", $l, ".enabled"),
                concat!("Layer ", $l, " On"),
                ParamUnit::None,
                ParamMapping::Toggle,
                if $l == 0 { 1.0 } else { 0.0 },
                ParamCategory::Topology,
            )
            .stepped(),
            ParamDescriptor::continuous(
                LAYER_BASE + $l * LAYER_STRIDE + L_WAVE,
                concat!("layer", $l, ".wave"),
                concat!("Layer ", $l, " Wave"),
                ParamUnit::None,
                ParamMapping::Discrete { steps: 5 },
                0.0,
                ParamCategory::Wave,
            )
            .stepped(),
            ParamDescriptor::continuous(
                LAYER_BASE + $l * LAYER_STRIDE + L_PULSE_WIDTH,
                concat!("layer", $l, ".pulse_width"),
                concat!("Layer ", $l, " Pulse Width"),
                ParamUnit::Percent,
                ParamMapping::Linear { min: 0.05, max: 0.95 },
                0.5,
                ParamCategory::Wave,
            ),
            ParamDescriptor::continuous(
                LAYER_BASE + $l * LAYER_STRIDE + L_LEVEL,
                concat!("layer", $l, ".level"),
                concat!("Layer ", $l, " Level"),
                ParamUnit::Decibel,
                ParamMapping::Linear { min: -60.0, max: 12.0 },
                60.0 / 72.0,
                ParamCategory::Mix,
            ),
            ParamDescriptor::continuous(
                LAYER_BASE + $l * LAYER_STRIDE + L_PAN,
                concat!("layer", $l, ".pan"),
                concat!("Layer ", $l, " Pan"),
                ParamUnit::None,
                ParamMapping::Linear { min: -1.0, max: 1.0 },
                0.5,
                ParamCategory::Mix,
            ),
            ParamDescriptor::continuous(
                LAYER_BASE + $l * LAYER_STRIDE + L_COARSE,
                concat!("layer", $l, ".coarse"),
                concat!("Layer ", $l, " Coarse"),
                ParamUnit::Semitones,
                ParamMapping::Discrete { steps: 97 },
                0.5,
                ParamCategory::Pitch,
            )
            .stepped(),
            ParamDescriptor::continuous(
                LAYER_BASE + $l * LAYER_STRIDE + L_FINE,
                concat!("layer", $l, ".fine"),
                concat!("Layer ", $l, " Fine"),
                ParamUnit::Cents,
                ParamMapping::Linear { min: -100.0, max: 100.0 },
                0.5,
                ParamCategory::Pitch,
            ),
            ParamDescriptor::continuous(
                LAYER_BASE + $l * LAYER_STRIDE + L_RESET_PHASE,
                concat!("layer", $l, ".reset_phase"),
                concat!("Layer ", $l, " Reset Phase"),
                ParamUnit::None,
                ParamMapping::Toggle,
                1.0,
                ParamCategory::Wave,
            )
            .stepped(),
        ]
    };
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

const L0: [ParamDescriptor; 8] = layer_descs!(0);
const L1: [ParamDescriptor; 8] = layer_descs!(1);
const L2: [ParamDescriptor; 8] = layer_descs!(2);
const L3: [ParamDescriptor; 8] = layer_descs!(3);

const fn concat_all() -> [ParamDescriptor; 22 + 32] {
    let mut out = [GLOBAL[0]; 54];
    let mut i = 0;
    while i < 22 {
        out[i] = GLOBAL[i];
        i += 1;
    }
    let layers = [&L0, &L1, &L2, &L3];
    let mut l = 0;
    while l < 4 {
        let mut j = 0;
        while j < 8 {
            out[22 + l * 8 + j] = layers[l][j];
            j += 1;
        }
        l += 1;
    }
    out
}

/// All descriptors (static, no allocation at query time).
pub static DESCRIPTORS: [ParamDescriptor; 54] = concat_all();

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
        }
        assert_eq!(decode_layer_param(layer_param(2, L_PAN)), Some((2, L_PAN)));
        assert_eq!(decode_layer_param(5), None);
        assert_eq!(decode_layer_param(layer_param(4, 0)), None);
    }
}

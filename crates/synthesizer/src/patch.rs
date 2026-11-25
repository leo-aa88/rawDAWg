//! Patch model: the complete serializable sound.

use rawdawg_dsp::envelope::AdsrParams;
use rawdawg_dsp::filter::FilterMode;
use rawdawg_dsp::oscillator::BasicWave;
use rawdawg_dsp::wavetable::{CYCLE_LEN, cycles};
use serde::{Deserialize, Serialize};

/// Current patch schema version.
pub const PATCH_VERSION: u32 = 1;
pub const MAX_LAYERS: usize = 4;

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

/// Sound generator of one layer.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Generator {
    /// Bandlimited basic wave.
    Basic { wave: BasicWave, pulse_width: f32 },
    /// Editable single-cycle wavetable (`CYCLE_LEN` samples, [-1,1]).
    Wavetable { cycle: Vec<f32>, remove_dc: bool },
}

impl Generator {
    pub fn kind_index(&self) -> usize {
        match self {
            Generator::Basic { .. } => 0,
            Generator::Wavetable { .. } => 1,
        }
    }
    pub fn is_valid(&self) -> bool {
        match self {
            Generator::Basic { pulse_width, .. } => pulse_width.is_finite() && (0.05..=0.95).contains(pulse_width),
            Generator::Wavetable { cycle, .. } => cycle.len() == CYCLE_LEN && cycle.iter().all(|s| s.is_finite() && s.abs() <= 4.0),
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
}

impl Layer {
    pub fn default_soft_saw() -> Self {
        Self {
            enabled: true,
            generator: Generator::Wavetable { cycle: cycles::soft_saw(), remove_dc: true },
            level_db: 0.0,
            pan: 0.0,
            coarse_semitones: 0,
            fine_cents: 0.0,
            reset_phase: true,
        }
    }
    pub fn basic(wave: BasicWave) -> Self {
        Self { enabled: true, generator: Generator::Basic { wave, pulse_width: 0.5 }, level_db: 0.0, pan: 0.0, coarse_semitones: 0, fine_cents: 0.0, reset_phase: true }
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
        if !self.velocity_sensitivity.is_finite() || !(0.0..=1.0).contains(&self.velocity_sensitivity) {
            return Err("velocity sensitivity out of range".into());
        }
        for e in [self.amp_env, self.filter_env] {
            if e != e.clamped() {
                return Err("envelope out of range".into());
            }
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
            layers: vec![Layer { generator: Generator::Wavetable { cycle: vec![f32::NAN; CYCLE_LEN], remove_dc: true }, ..Layer::default_soft_saw() }],
            ..Patch::default()
        };
        assert!(p.validate().is_err());
        let p = Patch { version: 99, ..Patch::default() };
        assert!(Patch::from_json(&p.to_json()).is_err());
        assert!(Patch::from_json(b"not json").is_err());
    }
}

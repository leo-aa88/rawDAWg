//! ADSR envelope with analog-style exponential segments.

use serde::{Deserialize, Serialize};

/// ADSR parameters in seconds / level.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AdsrParams {
    pub attack_s: f32,
    pub decay_s: f32,
    pub sustain: f32,
    pub release_s: f32,
}

impl AdsrParams {
    /// Spec default patch: 5 ms / 120 ms / 0.75 / 180 ms.
    pub const DEFAULT: AdsrParams = AdsrParams { attack_s: 0.005, decay_s: 0.120, sustain: 0.75, release_s: 0.180 };

    pub fn clamped(self) -> Self {
        Self {
            attack_s: if self.attack_s.is_finite() { self.attack_s.clamp(0.0, 30.0) } else { 0.005 },
            decay_s: if self.decay_s.is_finite() { self.decay_s.clamp(0.0, 30.0) } else { 0.12 },
            sustain: if self.sustain.is_finite() { self.sustain.clamp(0.0, 1.0) } else { 0.75 },
            release_s: if self.release_s.is_finite() { self.release_s.clamp(0.0, 30.0) } else { 0.18 },
        }
    }
}

impl Default for AdsrParams {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvStage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
    /// Very fast fade used by voice stealing.
    Fade,
}

/// Envelope generator. Attack is linear-ish (fast exponential overshoot
/// target), decay/release exponential toward target.
#[derive(Clone, Copy, Debug)]
pub struct Adsr {
    stage: EnvStage,
    level: f32,
    sample_rate: f32,
    params: AdsrParams,
    attack_coeff: f32,
    decay_coeff: f32,
    release_coeff: f32,
    fade_coeff: f32,
}

const ATTACK_OVERSHOOT: f32 = 1.3;
const SILENCE: f32 = 1e-4;

impl Adsr {
    pub fn new(sample_rate: f32, params: AdsrParams) -> Self {
        let mut e = Self { stage: EnvStage::Idle, level: 0.0, sample_rate, params: params.clamped(), attack_coeff: 0.0, decay_coeff: 0.0, release_coeff: 0.0, fade_coeff: 0.0 };
        e.recompute();
        e
    }

    fn coeff(time_s: f32, sr: f32) -> f32 {
        // Decay/release time is the time to fall from full level to the silence
        // threshold (1e-4 = -80 dB): ln(1/SILENCE) time constants within time_s.
        if time_s <= 0.0 { 1.0 } else { 1.0 - (-(1.0 / SILENCE).ln() / (time_s * sr)).exp() }
    }

    fn attack_coeff(time_s: f32, sr: f32) -> f32 {
        // With an overshoot target A, the level reaches 1.0 after ln(A/(A-1)) time
        // constants; choose the constant so that happens exactly at `time_s`.
        if time_s <= 0.0 {
            1.0
        } else {
            let k = (ATTACK_OVERSHOOT / (ATTACK_OVERSHOOT - 1.0)).ln();
            1.0 - (-k / (time_s * sr)).exp()
        }
    }

    fn recompute(&mut self) {
        self.attack_coeff = Self::attack_coeff(self.params.attack_s, self.sample_rate);
        self.decay_coeff = Self::coeff(self.params.decay_s, self.sample_rate);
        self.release_coeff = Self::coeff(self.params.release_s, self.sample_rate);
        self.fade_coeff = Self::coeff(0.003, self.sample_rate);
    }

    pub fn set_params(&mut self, params: AdsrParams) {
        self.params = params.clamped();
        self.recompute();
    }
    pub fn params(&self) -> AdsrParams {
        self.params
    }
    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sample_rate = sr;
        self.recompute();
    }
    pub fn stage(&self) -> EnvStage {
        self.stage
    }
    pub fn level(&self) -> f32 {
        self.level
    }
    pub fn is_active(&self) -> bool {
        self.stage != EnvStage::Idle
    }
    pub fn is_releasing(&self) -> bool {
        matches!(self.stage, EnvStage::Release | EnvStage::Fade)
    }

    /// Start (or retrigger from the current level, no click).
    pub fn note_on(&mut self) {
        self.stage = EnvStage::Attack;
        if self.params.attack_s <= 0.0 {
            self.level = 1.0;
            self.stage = EnvStage::Decay;
        }
    }
    pub fn note_off(&mut self) {
        if self.stage != EnvStage::Idle && self.stage != EnvStage::Fade {
            self.stage = EnvStage::Release;
        }
    }
    /// Fast fade for voice stealing (~3 ms).
    pub fn fade_out(&mut self) {
        if self.stage != EnvStage::Idle {
            self.stage = EnvStage::Fade;
        }
    }
    pub fn reset(&mut self) {
        self.stage = EnvStage::Idle;
        self.level = 0.0;
    }

    #[inline]
    pub fn next(&mut self) -> f32 {
        match self.stage {
            EnvStage::Idle => 0.0,
            EnvStage::Attack => {
                self.level += (ATTACK_OVERSHOOT - self.level) * self.attack_coeff;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.stage = EnvStage::Decay;
                }
                self.level
            }
            EnvStage::Decay => {
                let s = self.params.sustain;
                self.level += (s - self.level) * self.decay_coeff;
                if (self.level - s).abs() < SILENCE || self.params.decay_s <= 0.0 {
                    self.level = s;
                    self.stage = EnvStage::Sustain;
                }
                self.level
            }
            EnvStage::Sustain => {
                self.level = self.params.sustain;
                if self.level <= 0.0 {
                    self.stage = EnvStage::Idle;
                }
                self.level
            }
            EnvStage::Release => {
                self.level -= self.level * self.release_coeff;
                if self.level < SILENCE || self.params.release_s <= 0.0 {
                    self.level = 0.0;
                    self.stage = EnvStage::Idle;
                }
                self.level
            }
            EnvStage::Fade => {
                self.level -= self.level * self.fade_coeff;
                if self.level < SILENCE {
                    self.level = 0.0;
                    self.stage = EnvStage::Idle;
                }
                self.level
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_envelope_timing() {
        let sr = 48_000.0;
        let mut e = Adsr::new(sr, AdsrParams::DEFAULT);
        e.note_on();
        let mut peak_at = None;
        for i in 0..48_000 {
            let v = e.next();
            if peak_at.is_none() && v >= 0.999 {
                peak_at = Some(i);
            }
        }
        let p = peak_at.expect("reached peak");
        // attack 5 ms = 240 samples; allow ±40%
        assert!((200..=290).contains(&p), "peak at {p}");
        // after 1 s we must be at sustain
        assert!((e.level() - 0.75).abs() < 0.01);
        assert_eq!(e.stage(), EnvStage::Sustain);
        e.note_off();
        let mut released_at = None;
        for i in 0..48_000 {
            e.next();
            if !e.is_active() {
                released_at = Some(i);
                break;
            }
        }
        let r = released_at.expect("released");
        // release 180 ms ≈ 8640 samples from 0.75 to the -80 dB silence threshold
        // (slightly less because the start level is 0.75, not 1.0)
        assert!((7000..=9000).contains(&r), "released at {r}");
        assert_eq!(e.next(), 0.0);
    }

    #[test]
    fn fade_out_is_fast_and_monotonic() {
        let mut e = Adsr::new(48_000.0, AdsrParams::DEFAULT);
        e.note_on();
        for _ in 0..2000 {
            e.next();
        }
        e.fade_out();
        let mut last = e.level();
        let mut n = 0;
        while e.is_active() {
            let v = e.next();
            assert!(v <= last + 1e-6);
            last = v;
            n += 1;
            assert!(n < 1000, "fade took too long");
        }
        assert!(n <= 400);
    }

    #[test]
    fn zero_sustain_goes_idle_without_note_off() {
        let mut e = Adsr::new(48_000.0, AdsrParams { attack_s: 0.001, decay_s: 0.01, sustain: 0.0, release_s: 0.1 });
        e.note_on();
        for _ in 0..48_000 {
            e.next();
        }
        assert!(!e.is_active());
    }

    #[test]
    fn invalid_params_are_clamped() {
        let p = AdsrParams { attack_s: f32::NAN, decay_s: -1.0, sustain: 2.0, release_s: 1000.0 }.clamped();
        assert_eq!(p.attack_s, 0.005);
        assert_eq!(p.decay_s, 0.0);
        assert_eq!(p.sustain, 1.0);
        assert_eq!(p.release_s, 30.0);
    }
}

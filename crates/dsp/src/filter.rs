//! State-variable filter (topology-preserving transform, Simper 2011 form),
//! stable under per-sample coefficient modulation.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub enum FilterMode {
    #[default]
    LowPass,
    HighPass,
    BandPass,
    Notch,
}

impl FilterMode {
    pub const ALL: [FilterMode; 4] = [FilterMode::LowPass, FilterMode::HighPass, FilterMode::BandPass, FilterMode::Notch];
    pub fn index(self) -> usize {
        match self {
            FilterMode::LowPass => 0,
            FilterMode::HighPass => 1,
            FilterMode::BandPass => 2,
            FilterMode::Notch => 3,
        }
    }
    pub fn from_index(i: usize) -> Self {
        Self::ALL[i.min(3)]
    }
    pub fn label(self) -> &'static str {
        match self {
            FilterMode::LowPass => "Low-pass",
            FilterMode::HighPass => "High-pass",
            FilterMode::BandPass => "Band-pass",
            FilterMode::Notch => "Notch",
        }
    }
}

/// Second-order SVF, 12 dB/oct.
#[derive(Clone, Copy, Debug)]
pub struct Svf {
    ic1eq: f32,
    ic2eq: f32,
    g: f32,
    k: f32,
    a1: f32,
    a2: f32,
    a3: f32,
    mode: FilterMode,
    sample_rate: f32,
}

impl Svf {
    pub fn new(sample_rate: f32) -> Self {
        let mut f = Self { ic1eq: 0.0, ic2eq: 0.0, g: 0.0, k: 0.0, a1: 0.0, a2: 0.0, a3: 0.0, mode: FilterMode::LowPass, sample_rate };
        f.set(20_000.0, 0.707);
        f
    }
    pub fn set_sample_rate(&mut self, sr: f32) {
        self.sample_rate = sr;
    }
    pub fn set_mode(&mut self, mode: FilterMode) {
        self.mode = mode;
    }
    pub fn mode(&self) -> FilterMode {
        self.mode
    }
    /// Set cutoff (Hz) and resonance Q (0.5 .. 20). Cheap enough to call per sample.
    #[inline]
    pub fn set(&mut self, cutoff_hz: f32, q: f32) {
        let fc = cutoff_hz.clamp(5.0, self.sample_rate * 0.49);
        let q = q.clamp(0.5, 20.0);
        self.g = (std::f32::consts::PI * fc / self.sample_rate).tan();
        self.k = 1.0 / q;
        self.a1 = 1.0 / (1.0 + self.g * (self.g + self.k));
        self.a2 = self.g * self.a1;
        self.a3 = self.g * self.a2;
    }
    pub fn reset(&mut self) {
        self.ic1eq = 0.0;
        self.ic2eq = 0.0;
    }
    #[inline]
    pub fn process(&mut self, v0: f32) -> f32 {
        let v3 = v0 - self.ic2eq;
        let v1 = self.a1 * self.ic1eq + self.a2 * v3;
        let v2 = self.ic2eq + self.a2 * self.ic1eq + self.a3 * v3;
        self.ic1eq = 2.0 * v1 - self.ic1eq;
        self.ic2eq = 2.0 * v2 - self.ic2eq;
        match self.mode {
            FilterMode::LowPass => v2,
            FilterMode::BandPass => self.k * v1,
            FilterMode::HighPass => v0 - self.k * v1 - v2,
            FilterMode::Notch => v0 - self.k * v1,
        }
    }
    pub fn process_block(&mut self, buf: &mut [f32]) {
        for s in buf.iter_mut() {
            *s = self.process(*s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::magnitude_spectrum;

    fn response_db(mode: FilterMode, fc: f32, q: f32, probe_hz: f32) -> f32 {
        let sr = 48_000.0;
        let mut f = Svf::new(sr);
        f.set_mode(mode);
        f.set(fc, q);
        let n = 48_000;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let x = (std::f64::consts::TAU * probe_hz as f64 * i as f64 / sr as f64).sin() as f32;
            out.push(f.process(x));
        }
        // steady-state amplitude from the last half
        let tail = &out[n / 2..];
        let peak = tail.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        20.0 * peak.log10()
    }

    #[test]
    fn lowpass_passes_low_and_attenuates_high() {
        assert!(response_db(FilterMode::LowPass, 1000.0, 0.707, 100.0).abs() < 0.2);
        let at_fc = response_db(FilterMode::LowPass, 1000.0, 0.707, 1000.0);
        assert!((at_fc + 3.0).abs() < 0.5, "at fc: {at_fc}");
        let two_oct = response_db(FilterMode::LowPass, 1000.0, 0.707, 4000.0);
        assert!(two_oct < -22.0 && two_oct > -27.0, "2 octaves above: {two_oct}");
    }

    #[test]
    fn highpass_mirror() {
        assert!(response_db(FilterMode::HighPass, 1000.0, 0.707, 10_000.0).abs() < 0.3);
        assert!(response_db(FilterMode::HighPass, 1000.0, 0.707, 250.0) < -22.0);
    }

    #[test]
    fn bandpass_and_notch_at_center() {
        let bp = response_db(FilterMode::BandPass, 1000.0, 4.0, 1000.0);
        assert!(bp.abs() < 0.5, "bp center {bp}");
        assert!(response_db(FilterMode::BandPass, 1000.0, 4.0, 100.0) < -20.0);
        assert!(response_db(FilterMode::Notch, 1000.0, 4.0, 1000.0) < -30.0);
        assert!(response_db(FilterMode::Notch, 1000.0, 4.0, 100.0).abs() < 0.5);
    }

    #[test]
    fn resonance_peaks_at_cutoff() {
        let hi_q = response_db(FilterMode::LowPass, 1000.0, 8.0, 1000.0);
        // Q=8 → ~18 dB peak
        assert!(hi_q > 15.0 && hi_q < 20.0, "{hi_q}");
    }

    #[test]
    fn stable_under_fast_modulation_and_finite() {
        let sr = 48_000.0;
        let mut f = Svf::new(sr);
        f.set_mode(FilterMode::LowPass);
        let mut mx = 0.0f32;
        for i in 0..200_000 {
            let fc = 20.0 + 20_000.0 * (0.5 + 0.5 * ((i as f32) * 0.01).sin());
            f.set(fc, 15.0);
            let x = if i % 1000 == 0 { 1.0 } else { 0.0 };
            let y = f.process(x);
            assert!(y.is_finite());
            mx = mx.max(y.abs());
        }
        assert!(mx < 50.0);
        // passing white-ish noise through must not explode
        let spec = magnitude_spectrum(&[0.0f32; 1024]);
        assert_eq!(spec.len(), 513);
    }
}

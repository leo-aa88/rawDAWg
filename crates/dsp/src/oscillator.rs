//! Oscillators: bandlimited basic waves read from mipmapped tables, and a
//! pure sine. Phase is accumulated in `f64`.
//!
//! Pulse with variable width is synthesized as the difference of two
//! bandlimited saws offset by the pulse width (exactly the DC-free pulse), so
//! it inherits the table's alias suppression.

use crate::wavetable::{MipTable, TableBuilder, cycles};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub enum BasicWave {
    #[default]
    Sine,
    Triangle,
    Saw,
    Pulse,
}

impl BasicWave {
    pub const ALL: [BasicWave; 4] = [BasicWave::Sine, BasicWave::Triangle, BasicWave::Saw, BasicWave::Pulse];
    pub fn index(self) -> usize {
        match self {
            BasicWave::Sine => 0,
            BasicWave::Triangle => 1,
            BasicWave::Saw => 2,
            BasicWave::Pulse => 3,
        }
    }
    pub fn from_index(i: usize) -> Self {
        Self::ALL[i.min(3)]
    }
    pub fn label(self) -> &'static str {
        match self {
            BasicWave::Sine => "Sine",
            BasicWave::Triangle => "Triangle",
            BasicWave::Saw => "Saw",
            BasicWave::Pulse => "Pulse",
        }
    }
}

/// Prepared tables for the basic waves (built once off-thread, shared).
#[derive(Clone, Debug)]
pub struct BasicTables {
    pub saw: MipTable,
    pub triangle: MipTable,
}

impl BasicTables {
    pub fn build() -> Self {
        let mut b = TableBuilder::new();
        Self { saw: b.build(&cycles::saw(), true), triangle: b.build(&cycles::triangle(), true) }
    }
}

/// Bandlimited basic-wave oscillator reading from [`BasicTables`].
#[derive(Clone, Copy, Debug)]
pub struct BasicOscillator {
    phase: f64,
    inc: f64,
    level: usize,
    wave: BasicWave,
    pulse_width: f64,
    sample_rate: f64,
}

impl BasicOscillator {
    pub fn new(sample_rate: f64) -> Self {
        Self { phase: 0.0, inc: 0.0, level: 0, wave: BasicWave::Sine, pulse_width: 0.5, sample_rate }
    }
    pub fn set_sample_rate(&mut self, sr: f64) {
        self.sample_rate = sr;
    }
    pub fn set_wave(&mut self, w: BasicWave) {
        self.wave = w;
    }
    pub fn wave(&self) -> BasicWave {
        self.wave
    }
    /// Pulse width in [0.05, 0.95].
    pub fn set_pulse_width(&mut self, pw: f64) {
        self.pulse_width = if pw.is_finite() { pw.clamp(0.05, 0.95) } else { 0.5 };
    }
    /// Set frequency in Hz (clamped below Nyquist) and pick the mip level.
    #[inline]
    pub fn set_frequency(&mut self, hz: f64) {
        let max = self.sample_rate * 0.49;
        let hz = if hz.is_finite() { hz.clamp(0.0, max) } else { 0.0 };
        self.inc = hz / self.sample_rate;
        self.level = MipTable::level_for_increment(self.inc);
    }
    pub fn reset_phase(&mut self, phase: f64) {
        self.phase = phase.rem_euclid(1.0);
    }
    pub fn phase(&self) -> f64 {
        self.phase
    }
    pub fn increment(&self) -> f64 {
        self.inc
    }
    /// Advance the phase by one sample without producing output.
    #[inline]
    pub fn advance(&mut self) {
        self.phase += self.inc;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
    }

    #[inline]
    pub fn next(&mut self, tables: &BasicTables) -> f32 {
        let t = self.phase;
        let out = match self.wave {
            BasicWave::Sine => (t * std::f64::consts::TAU).sin() as f32,
            BasicWave::Saw => tables.saw.read(self.level, t),
            BasicWave::Triangle => tables.triangle.read(self.level, t),
            BasicWave::Pulse => tables.saw.read(self.level, t) - tables.saw.read(self.level, t + self.pulse_width),
        };
        self.phase += self.inc;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        out
    }
}

/// Pure sine oscillator with f64 phase (metronome, tests, additive partials).
#[derive(Clone, Copy, Debug)]
pub struct SineOscillator {
    phase: f64,
    inc: f64,
    sample_rate: f64,
}

impl SineOscillator {
    pub fn new(sample_rate: f64) -> Self {
        Self { phase: 0.0, inc: 0.0, sample_rate }
    }
    pub fn set_frequency(&mut self, hz: f64) {
        self.inc = if hz.is_finite() { hz.clamp(0.0, self.sample_rate * 0.5) / self.sample_rate } else { 0.0 };
    }
    pub fn reset_phase(&mut self, phase: f64) {
        self.phase = phase.rem_euclid(1.0);
    }
    #[inline]
    pub fn next(&mut self) -> f32 {
        let v = (self.phase * std::f64::consts::TAU).sin();
        self.phase += self.inc;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        v as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{dominant_frequency, harmonic_alias_ratio_db};

    fn render(tables: &BasicTables, wave: BasicWave, hz: f64, sr: f64, n: usize, pw: f64) -> Vec<f32> {
        let mut o = BasicOscillator::new(sr);
        o.set_wave(wave);
        o.set_pulse_width(pw);
        o.set_frequency(hz);
        (0..n).map(|_| o.next(tables)).collect()
    }

    #[test]
    fn a4_frequency_is_accurate_on_2s_render_at_all_rates() {
        let tables = BasicTables::build();
        for sr in [44_100.0, 48_000.0, 88_200.0, 96_000.0, 176_400.0, 192_000.0] {
            for wave in BasicWave::ALL {
                let buf = render(&tables, wave, 440.0, sr, (2.0 * sr) as usize, 0.5);
                let f = dominant_frequency(&buf, sr);
                assert!((f - 440.0).abs() < 0.5, "{wave:?} @ {sr}: {f}");
            }
        }
    }

    #[test]
    fn alias_energy_is_suppressed_at_high_pitch() {
        // C7 (2093 Hz) does not divide 48 kHz so aliases land off-harmonic (T-OSC ≥ 35 dB).
        let tables = BasicTables::build();
        let sr = 48_000.0;
        let f0 = 2093.0;
        for (wave, pw) in [(BasicWave::Saw, 0.5), (BasicWave::Pulse, 0.5), (BasicWave::Pulse, 0.2), (BasicWave::Triangle, 0.5)] {
            let buf = render(&tables, wave, f0, sr, 1 << 16, pw);
            let ratio = harmonic_alias_ratio_db(&buf, f0, sr);
            assert!(ratio >= 35.0, "{wave:?} pw={pw}: alias ratio {ratio:.1} dB");
        }
        // sanity: a naive saw at the same pitch must measure clearly worse
        let naive: Vec<f32> = {
            let mut ph = 0.0f64;
            (0..1 << 16)
                .map(|_| {
                    let v = (2.0 * ph - 1.0) as f32;
                    ph += f0 / sr;
                    if ph >= 1.0 {
                        ph -= 1.0
                    }
                    v
                })
                .collect()
        };
        assert!(harmonic_alias_ratio_db(&naive, f0, sr) < 35.0);
    }

    #[test]
    fn pulse_width_changes_duty_cycle_without_dc() {
        let tables = BasicTables::build();
        let buf = render(&tables, BasicWave::Pulse, 100.0, 48_000.0, 48_000, 0.25);
        let dc = buf.iter().map(|s| *s as f64).sum::<f64>() / buf.len() as f64;
        assert!(dc.abs() < 0.02, "dc {dc}");
        // levels: high ≈ 2(1-pw)=1.5 for 25% of the time, low ≈ -2pw=-0.5
        let high = buf.iter().filter(|s| **s > 1.0).count() as f64 / buf.len() as f64;
        assert!((high - 0.25).abs() < 0.03, "duty {high}");
    }

    #[test]
    fn amplitude_bounded_and_finite() {
        let tables = BasicTables::build();
        for wave in BasicWave::ALL {
            let buf = render(&tables, wave, 55.0, 48_000.0, 48_000, 0.5);
            assert!(buf.iter().all(|s| s.is_finite()));
            let p = crate::math::peak(&buf);
            assert!(p <= 1.3 && p > 0.5, "{wave:?} peak {p}");
        }
    }

    #[test]
    fn dc_is_small() {
        let tables = BasicTables::build();
        for wave in BasicWave::ALL {
            let buf = render(&tables, wave, 100.0, 48_000.0, 48_000, 0.5);
            let dc = buf.iter().map(|s| *s as f64).sum::<f64>() / buf.len() as f64;
            assert!(dc.abs() < 0.01, "{wave:?} dc {dc}");
        }
    }

    #[test]
    fn phase_continuity_across_frequency_change() {
        let tables = BasicTables::build();
        let mut o = BasicOscillator::new(48_000.0);
        o.set_wave(BasicWave::Sine);
        o.set_frequency(440.0);
        let mut last = o.next(&tables);
        for i in 0..2000 {
            if i == 1000 {
                o.set_frequency(660.0);
            }
            let v = o.next(&tables);
            assert!((v - last).abs() < 0.1, "discontinuity at {i}: {last} -> {v}");
            last = v;
        }
    }

    #[test]
    fn frequency_clamped_below_nyquist() {
        let mut o = BasicOscillator::new(48_000.0);
        o.set_frequency(100_000.0);
        assert!(o.increment() < 0.5);
        o.set_frequency(-5.0);
        assert_eq!(o.increment(), 0.0);
        o.set_frequency(f64::NAN);
        assert_eq!(o.increment(), 0.0);
    }
}

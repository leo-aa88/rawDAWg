//! Per-voice generator algorithms (SYN-01). Pure DSP state + per-sample render;
//! no allocation after `prepare`-time construction.

use crate::patch::{AdditiveSettings, FmAlgorithm, FmSettings, GranularSettings, MAX_UNISON, PluckSettings};
use rawdawg_dsp::oscillator::{BasicOscillator, BasicTables};
use rawdawg_dsp::rng::DeterministicRng;
use rawdawg_dsp::wavetable::MipTable;
use std::f64::consts::TAU;

/// Control-rate values a generator needs this block (already modulated).
#[derive(Clone, Copy, Debug)]
pub struct GenControl {
    /// Fundamental frequency in Hz (after bend, tuning, unison detune per voice).
    pub freq_hz: f64,
    /// Wavetable motion position 0..1.
    pub wt_position: f32,
    pub pulse_width: f64,
    /// FM index scale 0..2 (1 = patch value).
    pub fm_index: f32,
    /// Granular position 0..1.
    pub grain_position: f32,
}

// ---------------------------------------------------------------------------
// Basic / wavetable (unison handled by the caller with several phases)
// ---------------------------------------------------------------------------

/// Read a wavetable with motion frames: blend between adjacent frame tables.
#[inline]
pub fn read_frames(frames: &[Box<MipTable>], level: usize, phase: f64, position: f32) -> f32 {
    match frames.len() {
        0 => 0.0,
        1 => frames[0].read(level, phase),
        n => {
            let p = position.clamp(0.0, 1.0) * (n - 1) as f32;
            let i = (p as usize).min(n - 2);
            let f = p - i as f32;
            let a = frames[i].read(level, phase);
            let b = frames[i + 1].read(level, phase);
            a + (b - a) * f
        }
    }
}

// ---------------------------------------------------------------------------
// FM / phase modulation, 4 operators
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct FmState {
    phase: [f64; 4],
    /// Previous output of operator 4 (explicit one-sample feedback delay).
    fb_last: f32,
    sample_rate: f64,
}

impl FmState {
    pub fn new(sample_rate: f64) -> Self {
        Self { phase: [0.0; 4], fb_last: 0.0, sample_rate }
    }
    pub fn reset(&mut self) {
        self.phase = [0.0; 4];
        self.fb_last = 0.0;
    }
    /// Operator frequencies are `freq * ratio * 2^(detune/1200)`, bounded below Nyquist.
    /// Each operator's `level` scales its output (as a carrier or as a modulator);
    /// `index_scale` scales all modulation depths (1 = patch value).
    #[inline]
    pub fn next(&mut self, s: &FmSettings, freq_hz: f64, index_scale: f32) -> f32 {
        let nyq = self.sample_rate * 0.5;
        let index = f64::from(index_scale) * TAU * 2.0; // full level → 4π rad peak deviation
        let mut out = [0.0f32; 4];
        // operators 1..4 → index 0..3; evaluate modulators first (4, 3, 2, 1)
        for k in (0..4).rev() {
            let o = &s.operators[k];
            let f = (freq_hz * f64::from(o.ratio) * 2f64.powf(f64::from(o.detune_cents) / 1200.0)).min(nyq);
            let inc = f / self.sample_rate;
            let modin: f32 = match s.algorithm {
                FmAlgorithm::Serial => match k {
                    3 => self.fb_last * s.feedback,
                    _ => out[k + 1],
                },
                FmAlgorithm::Pairs => match k {
                    3 => self.fb_last * s.feedback,
                    2 => out[3],
                    1 => 0.0,
                    _ => out[1],
                },
                FmAlgorithm::Fan => match k {
                    3 => self.fb_last * s.feedback,
                    0 => out[1] + out[2] + out[3],
                    _ => 0.0,
                },
                FmAlgorithm::Parallel => {
                    if k == 3 {
                        self.fb_last * s.feedback
                    } else {
                        0.0
                    }
                }
            };
            let ph = self.phase[k] * TAU + f64::from(modin) * index;
            out[k] = ph.sin() as f32 * o.level;
            self.phase[k] += inc;
            if self.phase[k] >= 1.0 {
                self.phase[k] -= 1.0;
            }
        }
        self.fb_last = out[3];
        match s.algorithm {
            FmAlgorithm::Serial | FmAlgorithm::Fan => out[0],
            FmAlgorithm::Pairs => (out[0] + out[2]) * 0.5,
            FmAlgorithm::Parallel => (out[0] + out[1] + out[2] + out[3]) * 0.25,
        }
    }
}

// ---------------------------------------------------------------------------
// Additive: detuned partial bank (harmonic mode renders through the wavetable path)
// ---------------------------------------------------------------------------

pub const ADDITIVE_BANK: usize = 32;

#[derive(Clone, Copy, Debug)]
pub struct AdditiveState {
    phase: [f64; ADDITIVE_BANK],
    sample_rate: f64,
}

impl AdditiveState {
    pub fn new(sample_rate: f64) -> Self {
        Self { phase: [0.0; ADDITIVE_BANK], sample_rate }
    }
    pub fn reset(&mut self, s: &AdditiveSettings) {
        for (k, p) in self.phase.iter_mut().enumerate() {
            *p = f64::from(s.phases.get(k).copied().unwrap_or(0.0)) / std::f32::consts::TAU as f64;
        }
    }
    /// Detuned mode: partial k at `freq * (k+1) * (1 + stretch * k)`, limited at Nyquist.
    #[inline]
    pub fn next(&mut self, s: &AdditiveSettings, freq_hz: f64) -> f32 {
        let nyq = self.sample_rate * 0.5;
        let mut out = 0.0f32;
        let n = s.amplitudes.len().min(ADDITIVE_BANK);
        let mut norm = 0.0f32;
        for k in 0..n {
            let a = s.amplitudes[k];
            if a <= 0.0 {
                continue;
            }
            let f = freq_hz * (k as f64 + 1.0) * (1.0 + f64::from(s.stretch) * k as f64);
            if f >= nyq {
                continue;
            }
            out += (self.phase[k] * TAU).sin() as f32 * a;
            norm += a;
            self.phase[k] += f / self.sample_rate;
            if self.phase[k] >= 1.0 {
                self.phase[k] -= 1.0;
            }
        }
        if norm > 1.0 { out / norm } else { out }
    }
}

// ---------------------------------------------------------------------------
// Granular: grains read from a source table (the layer cycle) with windowing
// ---------------------------------------------------------------------------

pub const GRAINS_PER_VOICE: usize = 8;

#[derive(Clone, Copy, Debug, Default)]
struct Grain {
    active: bool,
    /// Read position in source samples (fractional).
    pos: f64,
    /// Position increment per output sample.
    inc: f64,
    /// Age in samples and total length.
    age: u32,
    len: u32,
}

#[derive(Clone, Debug)]
pub struct GranularState {
    grains: [Grain; GRAINS_PER_VOICE],
    /// Samples until the next grain spawns.
    next_spawn: f64,
    /// Phase-coherent playhead through the source (so overlapping grains reinforce the note).
    playhead: f64,
    rng: DeterministicRng,
    sample_rate: f64,
}

impl GranularState {
    pub fn new(sample_rate: f64, seed: u64) -> Self {
        Self { grains: [Grain::default(); GRAINS_PER_VOICE], next_spawn: 0.0, playhead: 0.0, rng: DeterministicRng::new(seed, 0x6A), sample_rate }
    }
    pub fn reset(&mut self, seed: u64) {
        self.grains = [Grain::default(); GRAINS_PER_VOICE];
        self.next_spawn = 0.0;
        self.playhead = 0.0;
        self.rng = DeterministicRng::new(seed, 0x6A);
    }
    /// `source` is a cyclic table level 0 (4096 samples) treated as the grain material.
    #[inline]
    pub fn next(&mut self, s: &GranularSettings, source: &MipTable, freq_hz: f64, position: f32) -> f32 {
        let src = source.level_slice(0);
        let n = src.len() as f64;
        // the source cycle is one period: reading it at `freq * n / sample_rate`
        // samples per sample reproduces the note; the playhead keeps grains phase-coherent
        let base_inc = freq_hz * n / self.sample_rate;
        if self.next_spawn <= 0.0 {
            let interval = self.sample_rate / f64::from(s.density_hz.max(1.0));
            self.next_spawn += interval;
            if let Some(g) = self.grains.iter_mut().find(|g| !g.active) {
                let len = (f64::from(s.size_ms) * 0.001 * self.sample_rate).clamp(32.0, self.sample_rate * 0.5) as u32;
                let jitter = (self.rng.unit_f64() - 0.5) * f64::from(s.jitter) * n;
                let start = (self.playhead + f64::from(position) * n + jitter).rem_euclid(n);
                let spread = 2f64.powf((self.rng.unit_f64() - 0.5) * f64::from(s.pitch_spread_semitones) / 12.0);
                *g = Grain { active: true, pos: start, inc: base_inc * spread, age: 0, len };
            }
        }
        self.next_spawn -= 1.0;
        self.playhead = (self.playhead + base_inc).rem_euclid(n);
        let mut out = 0.0f32;
        let mut active = 0u32;
        for g in self.grains.iter_mut().filter(|g| g.active) {
            let t = g.age as f32 / g.len as f32;
            let w = match s.window {
                crate::patch::GrainWindow::Hann => 0.5 - 0.5 * (std::f32::consts::TAU * t).cos(),
                crate::patch::GrainWindow::Triangle => 1.0 - (2.0 * t - 1.0).abs(),
                crate::patch::GrainWindow::Rect => 1.0,
            };
            let i = g.pos as usize % src.len();
            let frac = (g.pos - g.pos.floor()) as f32;
            let a = src[i];
            let b = src[(i + 1) % src.len()];
            out += (a + (b - a) * frac) * w;
            g.pos = (g.pos + g.inc).rem_euclid(n);
            g.age += 1;
            active += 1;
            if g.age >= g.len {
                g.active = false;
            }
        }
        if active > 1 { out / (active as f32).sqrt() } else { out }
    }
    pub fn active_grains(&self) -> usize {
        self.grains.iter().filter(|g| g.active).count()
    }
}

// ---------------------------------------------------------------------------
// Plucked string: Karplus–Strong / waveguide with fractional delay and damping
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct PluckState {
    delay: Vec<f32>,
    write: usize,
    /// One-pole damping filter state.
    lp: f32,
    excite_remaining: u32,
    rng: DeterministicRng,
    sample_rate: f64,
}

impl PluckState {
    /// Allocates a delay line long enough for 20 Hz at this sample rate.
    pub fn new(sample_rate: f64, seed: u64) -> Self {
        let len = (sample_rate / 20.0).ceil() as usize + 4;
        Self { delay: vec![0.0; len], write: 0, lp: 0.0, excite_remaining: 0, rng: DeterministicRng::new(seed, 0x9C), sample_rate }
    }
    pub fn reset(&mut self, seed: u64) {
        self.delay.fill(0.0);
        self.write = 0;
        self.lp = 0.0;
        self.excite_remaining = 0;
        self.rng = DeterministicRng::new(seed, 0x9C);
    }
    /// Start a pluck: an excitation burst one period long is injected into the loop.
    pub fn pluck(&mut self, _s: &PluckSettings, freq_hz: f64, _velocity: f32) {
        let period = (self.sample_rate / freq_hz.max(20.0)) as u32;
        self.excite_remaining = period.max(2);
    }
    #[inline]
    pub fn next(&mut self, s: &PluckSettings, freq_hz: f64, velocity: f32) -> f32 {
        let n = self.delay.len();
        // loop filter: one-pole low-pass whose DC group delay is (1-c)/c samples;
        // the delay line is shortened by that amount so the pitch stays in tune
        let coeff = (0.02 + 0.9 * s.brightness).clamp(0.02, 0.98);
        let group_delay = (1.0 - coeff) / coeff;
        let period = (self.sample_rate / freq_hz.clamp(20.0, self.sample_rate * 0.45)) as f32 - group_delay;
        let period = period.clamp(2.0, (n - 2) as f32);
        let int_delay = period.floor() as usize;
        let frac = period - int_delay as f32;
        // fractional read `period` samples behind the write head (linear interpolation)
        let i0 = (self.write + n - int_delay) % n;
        let i1 = (i0 + n - 1) % n;
        let x = self.delay[i0] + (self.delay[i1] - self.delay[i0]) * frac;
        self.lp += (x - self.lp) * coeff;
        let loop_gain = (0.9 + 0.0999 * (1.0 - s.damping)).clamp(0.5, 0.9999);
        let mut v = self.lp * loop_gain;
        if self.excite_remaining > 0 {
            self.excite_remaining -= 1;
            let burst = match s.excitation {
                crate::patch::Excitation::Noise => self.rng.bipolar_f32(),
                crate::patch::Excitation::Impulse => {
                    if self.excite_remaining == 0 {
                        1.0
                    } else {
                        0.0
                    }
                }
                crate::patch::Excitation::Pick => (self.rng.bipolar_f32() * 0.5 + 0.5) * if self.excite_remaining.is_multiple_of(2) { 1.0 } else { -1.0 },
            };
            v += burst * velocity;
        }
        // the loop gain is < 1 so the string decays; clamp as a safety net only
        v = v.clamp(-2.0, 2.0);
        self.delay[self.write] = v;
        self.write += 1;
        if self.write >= n {
            self.write = 0;
        }
        v
    }
}

// ---------------------------------------------------------------------------
// Unison helper: per-voice detune/pan offsets for N unison voices.
// ---------------------------------------------------------------------------

/// Detune in semitones for unison voice `i` of `n` with spread `detune_cents` (±).
#[inline]
pub fn unison_detune(i: usize, n: usize, detune_cents: f32) -> f64 {
    if n <= 1 {
        return 0.0;
    }
    let t = i as f64 / (n - 1) as f64 * 2.0 - 1.0; // -1..1
    t * f64::from(detune_cents) / 100.0
}

/// Pan position (-1..1) for unison voice `i` of `n` with stereo `spread` 0..1.
#[inline]
pub fn unison_pan(i: usize, n: usize, spread: f32) -> f32 {
    if n <= 1 {
        return 0.0;
    }
    let t = i as f32 / (n - 1) as f32 * 2.0 - 1.0;
    t * spread
}

/// Gain normalization so unison does not multiply loudness: 1/sqrt(n).
#[inline]
pub fn unison_gain(n: usize) -> f32 {
    1.0 / (n.clamp(1, MAX_UNISON) as f32).sqrt()
}

/// Prepared basic tables for the basic-wave path (re-exported for voices).
pub type Tables = BasicTables;
pub type Osc = BasicOscillator;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::{AdditiveSettings, FmOperator, FmSettings, GrainWindow, GranularSettings, PluckSettings};
    use rawdawg_dsp::analysis::{dominant_frequency, features, fundamental_frequency};
    use rawdawg_dsp::wavetable::{TableBuilder, cycles};

    const SR: f64 = 48_000.0;

    #[test]
    fn fm_serial_produces_sidebands_and_carrier_pitch_is_stable() {
        let mut st = FmState::new(SR);
        let mut s = FmSettings::default();
        s.operators[1].level = 0.0; // pure carrier first
        s.operators[2].level = 0.0;
        s.operators[3].level = 0.0;
        let pure: Vec<f32> = (0..32_768).map(|_| st.next(&s, 440.0, 1.0)).collect();
        assert!((dominant_frequency(&pure, SR) - 440.0).abs() < 0.5);
        let (_, c0, _) = features(&pure, SR);
        st.reset();
        s.operators[1].level = 0.6;
        s.operators[1].ratio = 2.0;
        let modulated: Vec<f32> = (0..32_768).map(|_| st.next(&s, 440.0, 1.0)).collect();
        let (_, c1, _) = features(&modulated, SR);
        assert!(c1 > c0 * 1.5, "modulation adds sidebands: centroid {c0} -> {c1}");
        assert!(modulated.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
        // feedback on op4 with everything routed stays bounded
        let mut s2 = FmSettings { feedback: 1.0, ..FmSettings::default() };
        for op in &mut s2.operators {
            op.level = 1.0;
        }
        let mut st2 = FmState::new(SR);
        let fb: Vec<f32> = (0..48_000).map(|_| st2.next(&s2, 55.0, 2.0)).collect();
        assert!(fb.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
        let _ = FmOperator::default();
    }

    #[test]
    fn additive_bank_limits_partials_at_nyquist_and_stretch_detunes() {
        let s = AdditiveSettings { amplitudes: vec![1.0, 0.5, 0.25, 0.125], phases: vec![0.0; 4], stretch: 0.0, detuned: true };
        let mut st = AdditiveState::new(SR);
        st.reset(&s);
        let buf: Vec<f32> = (0..32_768).map(|_| st.next(&s, 220.0)).collect();
        assert!((dominant_frequency(&buf, SR) - 220.0).abs() < 0.5);
        assert!(buf.iter().all(|v| v.abs() <= 1.0));
        // high fundamental: partials above Nyquist are dropped (no alias), output still finite
        let mut st2 = AdditiveState::new(SR);
        let hi: Vec<f32> = (0..32_768).map(|_| st2.next(&s, 20_000.0)).collect();
        assert!((dominant_frequency(&hi, SR) - 20_000.0).abs() < 2.0);
        let s2 = AdditiveSettings { stretch: 0.02, ..s.clone() };
        let mut st3 = AdditiveState::new(SR);
        let stretched: Vec<f32> = (0..32_768).map(|_| st3.next(&s2, 220.0)).collect();
        let (_, c_a, _) = features(&buf, SR);
        let (_, c_b, _) = features(&stretched, SR);
        assert!((c_a - c_b).abs() > 1.0, "stretch changes the spectrum");
    }

    #[test]
    fn granular_is_bounded_and_follows_pitch() {
        let mut b = TableBuilder::new();
        let src = b.build(&cycles::saw(), true);
        let s = GranularSettings { size_ms: 40.0, density_hz: 60.0, jitter: 0.0, pitch_spread_semitones: 0.0, window: GrainWindow::Hann };
        let mut st = GranularState::new(SR, 1);
        let buf: Vec<f32> = (0..65_536).map(|_| st.next(&s, &src, 220.0, 0.0)).collect();
        assert!(buf.iter().all(|v| v.is_finite() && v.abs() <= 2.0));
        assert!(st.active_grains() <= GRAINS_PER_VOICE);
        assert!(rawdawg_dsp::math::rms(&buf[8192..]) > 0.05);
        // deterministic for the same seed
        let mut st2 = GranularState::new(SR, 1);
        let buf2: Vec<f32> = (0..65_536).map(|_| st2.next(&s, &src, 220.0, 0.0)).collect();
        assert_eq!(buf, buf2);
        let f = dominant_frequency(&buf[8192..], SR);
        assert!((f - 220.0).abs() < 3.0, "grain pitch follows the note: {f}");
    }

    #[test]
    fn pluck_decays_to_silence_and_is_in_tune() {
        let s = PluckSettings::default();
        let mut st = PluckState::new(SR, 3);
        st.pluck(&s, 220.0, 1.0);
        let buf: Vec<f32> = (0..96_000).map(|_| st.next(&s, 220.0, 1.0)).collect();
        assert!(buf.iter().all(|v| v.is_finite()));
        let early = rawdawg_dsp::math::rms(&buf[2000..12_000]);
        let late = rawdawg_dsp::math::rms(&buf[80_000..]);
        assert!(early > 0.05, "string rings: {early}");
        assert!(late < early * 0.1, "string decays: {early} -> {late}");
        let f = fundamental_frequency(&buf[2000..30_000], SR, 0.15);
        assert!((f - 220.0).abs() < 2.0, "pluck pitch {f}");
        // fractional delay keeps tuning at a non-integer period
        let mut st2 = PluckState::new(SR, 3);
        st2.pluck(&s, 441.3, 1.0);
        let b2: Vec<f32> = (0..48_000).map(|_| st2.next(&s, 441.3, 1.0)).collect();
        let f2 = fundamental_frequency(&b2[1000..30_000], SR, 0.15);
        assert!((f2 - 441.3).abs() < 2.0, "fractional tuning {f2}");
    }

    #[test]
    fn unison_helpers() {
        assert_eq!(unison_detune(0, 1, 50.0), 0.0);
        assert!((unison_detune(0, 3, 50.0) + 0.5).abs() < 1e-9);
        assert!((unison_detune(2, 3, 50.0) - 0.5).abs() < 1e-9);
        assert!((unison_detune(1, 3, 50.0)).abs() < 1e-9);
        assert!((unison_gain(4) - 0.5).abs() < 1e-6);
        assert!((unison_pan(3, 4, 1.0) - 1.0).abs() < 1e-6);
    }
}

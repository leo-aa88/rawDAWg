//! Mipmapped wavetables (DSP.md §3 pipeline).
//!
//! The canonical editable cycle has [`CYCLE_LEN`] samples. Playback tables are
//! built off-thread: cycle → optional DC removal → FFT → per-level harmonic
//! truncation → inverse FFT into [`TABLE_LEN`]-sample tables (zero-padded
//! spectrum, so the top harmonic of level 0 still has 8 samples per period) →
//! 4-point Hermite interpolated playback at the highest level whose top
//! harmonic stays below Nyquist for the current frequency.

use realfft::RealFftPlanner;
use realfft::num_complex::Complex;

/// Editable cycle length.
pub const CYCLE_LEN: usize = 2048;
/// Playback table length per mip level.
pub const TABLE_LEN: usize = 4096;
/// Highest harmonic kept at mip level 0 (8 table samples per period).
pub const MAX_HARMONIC: usize = 512;
/// Number of mip levels: 512, 256, ..., 1.
pub const LEVELS: usize = 10;
const GUARD: usize = 4;
const STRIDE: usize = TABLE_LEN + GUARD;

/// Spectrum of a cycle: `CYCLE_LEN/2 + 1` complex bins.
pub type Spectrum = Vec<Complex<f32>>;

/// Non-RT builder: owns FFT plans and scratch.
pub struct TableBuilder {
    planner: RealFftPlanner<f32>,
}

impl Default for TableBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TableBuilder {
    pub fn new() -> Self {
        Self { planner: RealFftPlanner::new() }
    }

    /// Forward transform of a [`CYCLE_LEN`] cycle.
    pub fn analyze(&mut self, cycle: &[f32]) -> Spectrum {
        assert_eq!(cycle.len(), CYCLE_LEN, "cycle must have CYCLE_LEN samples");
        let fft = self.planner.plan_fft_forward(CYCLE_LEN);
        let mut input = cycle.to_vec();
        let mut out = fft.make_output_vec();
        fft.process(&mut input, &mut out).expect("sizes match");
        out
    }

    /// Inverse transform of a [`CYCLE_LEN`] spectrum back into a cycle (normalized).
    pub fn synthesize(&mut self, spectrum: &Spectrum) -> Vec<f32> {
        assert_eq!(spectrum.len(), CYCLE_LEN / 2 + 1);
        let ifft = self.planner.plan_fft_inverse(CYCLE_LEN);
        let mut spec = spectrum.clone();
        // real IFFT requires zero imaginary parts at DC and Nyquist
        spec[0].im = 0.0;
        spec[CYCLE_LEN / 2].im = 0.0;
        let mut out = ifft.make_output_vec();
        ifft.process(&mut spec, &mut out).expect("sizes match");
        let scale = 1.0 / CYCLE_LEN as f32;
        for s in &mut out {
            *s *= scale;
        }
        out
    }

    /// Build mip levels from a cycle. `remove_dc` zeroes bin 0.
    pub fn build(&mut self, cycle: &[f32], remove_dc: bool) -> MipTable {
        let mut spectrum = self.analyze(cycle);
        if remove_dc {
            spectrum[0] = Complex::new(0.0, 0.0);
        }
        self.build_from_spectrum(&spectrum)
    }

    /// Build mip levels from a spectrum (harmonics above `MAX_HARMONIC` are dropped).
    pub fn build_from_spectrum(&mut self, spectrum: &Spectrum) -> MipTable {
        let ifft = self.planner.plan_fft_inverse(TABLE_LEN);
        let mut data = vec![0.0f32; LEVELS * STRIDE];
        let scale = (TABLE_LEN as f32 / CYCLE_LEN as f32) / TABLE_LEN as f32;
        for level in 0..LEVELS {
            let max_h = MAX_HARMONIC >> level;
            let mut spec = ifft.make_input_vec();
            spec[0] = Complex::new(spectrum[0].re, 0.0);
            for (h, bin) in spectrum.iter().enumerate().skip(1) {
                if h > max_h || h >= CYCLE_LEN / 2 {
                    break;
                }
                spec[h] = *bin;
            }
            let mut out = ifft.make_output_vec();
            ifft.process(&mut spec, &mut out).expect("sizes match");
            let base = level * STRIDE;
            for (i, s) in out.iter().enumerate() {
                data[base + i] = s * scale;
            }
            for g in 0..GUARD {
                data[base + TABLE_LEN + g] = data[base + g];
            }
        }
        MipTable { data }
    }
}

/// Prepared mipmapped table (immutable after build, shared by voices).
#[derive(Clone, Debug)]
pub struct MipTable {
    data: Vec<f32>,
}

impl MipTable {
    /// Table of silence.
    pub fn silent() -> Self {
        Self { data: vec![0.0; LEVELS * STRIDE] }
    }

    /// Mip level for a phase increment (cycles per sample): the finest level whose
    /// highest harmonic stays strictly below Nyquist.
    #[inline]
    pub fn level_for_increment(inc: f64) -> usize {
        if inc <= 0.0 {
            return 0;
        }
        let max_h = (0.5 / inc).floor();
        let mut level = 0usize;
        while level + 1 < LEVELS && f64::from((MAX_HARMONIC >> level) as u32) > max_h {
            level += 1;
        }
        level
    }

    /// Read with 4-point Hermite interpolation at `phase` in [0,1) on `level`.
    #[inline]
    pub fn read(&self, level: usize, phase: f64) -> f32 {
        let level = level.min(LEVELS - 1);
        let pos = phase.rem_euclid(1.0) * TABLE_LEN as f64;
        let i = pos as usize;
        let frac = (pos - i as f64) as f32;
        let base = level * STRIDE;
        // guard samples make i+2 safe; i-1 wraps via TABLE_LEN
        let im1 = if i == 0 { base + TABLE_LEN - 1 } else { base + i - 1 };
        let y0 = self.data[im1];
        let y1 = self.data[base + i];
        let y2 = self.data[base + i + 1];
        let y3 = self.data[base + i + 2];
        hermite(y0, y1, y2, y3, frac)
    }

    /// Level-0 table slice (for thumbnails/scope).
    pub fn level_slice(&self, level: usize) -> &[f32] {
        let base = level.min(LEVELS - 1) * STRIDE;
        &self.data[base..base + TABLE_LEN]
    }

    pub fn is_finite(&self) -> bool {
        self.data.iter().all(|s| s.is_finite())
    }
}

#[inline]
pub fn hermite(y0: f32, y1: f32, y2: f32, y3: f32, t: f32) -> f32 {
    let c0 = y1;
    let c1 = 0.5 * (y2 - y0);
    let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
    ((c3 * t + c2) * t + c1) * t + c0
}

/// Generate canonical cycles for the basic waves.
pub mod cycles {
    use super::CYCLE_LEN;
    use std::f64::consts::TAU;

    pub fn sine() -> Vec<f32> {
        (0..CYCLE_LEN).map(|i| (TAU * i as f64 / CYCLE_LEN as f64).sin() as f32).collect()
    }
    /// Rising saw from -1 to +1 (DC-free).
    pub fn saw() -> Vec<f32> {
        (0..CYCLE_LEN).map(|i| (2.0 * i as f64 / CYCLE_LEN as f64 - 1.0) as f32).collect()
    }
    /// Triangle starting at -1, peak +1 at half cycle.
    pub fn triangle() -> Vec<f32> {
        (0..CYCLE_LEN)
            .map(|i| {
                let p = i as f64 / CYCLE_LEN as f64;
                (if p < 0.5 { -1.0 + 4.0 * p } else { 3.0 - 4.0 * p }) as f32
            })
            .collect()
    }
    /// Square (50% pulse) +1 for the first half.
    pub fn square() -> Vec<f32> {
        (0..CYCLE_LEN).map(|i| if i < CYCLE_LEN / 2 { 1.0 } else { -1.0 }).collect()
    }
    /// Soft saw: saw with harmonics rolled off (1/n^1.5), the spec's default patch wave.
    pub fn soft_saw() -> Vec<f32> {
        let mut out = vec![0.0f32; CYCLE_LEN];
        for (i, s) in out.iter_mut().enumerate() {
            let p = i as f64 / CYCLE_LEN as f64;
            let mut acc = 0.0f64;
            for n in 1..=24u32 {
                acc += (TAU * n as f64 * p).sin() / (n as f64).powf(1.5);
            }
            *s = (acc * 0.75) as f32;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{dominant_frequency, harmonic_alias_ratio_db};

    fn render(table: &MipTable, hz: f64, sr: f64, n: usize) -> Vec<f32> {
        let inc = hz / sr;
        let level = MipTable::level_for_increment(inc);
        let mut phase = 0.0f64;
        (0..n)
            .map(|_| {
                let v = table.read(level, phase);
                phase += inc;
                if phase >= 1.0 {
                    phase -= 1.0;
                }
                v
            })
            .collect()
    }

    #[test]
    fn level_selection() {
        // 440 Hz at 48k: inc = 0.00917, max harmonic 54 → level 4 (32 harmonics)
        assert_eq!(MipTable::level_for_increment(440.0 / 48000.0), 4);
        // 20 Hz: 1200 harmonics allowed → level 0
        assert_eq!(MipTable::level_for_increment(20.0 / 48000.0), 0);
        // 20 kHz at 48k: 1 harmonic → level 9
        assert_eq!(MipTable::level_for_increment(20000.0 / 48000.0), 9);
        assert_eq!(MipTable::level_for_increment(0.0), 0);
    }

    #[test]
    fn built_tables_reproduce_fundamental_frequency() {
        let mut b = TableBuilder::new();
        let saw = b.build(&cycles::saw(), true);
        for sr in [44_100.0, 48_000.0, 96_000.0, 192_000.0] {
            let buf = render(&saw, 440.0, sr, (2.0 * sr) as usize);
            let f = dominant_frequency(&buf, sr);
            assert!((f - 440.0).abs() < 0.5, "{sr}: {f}");
        }
    }

    #[test]
    fn alias_suppression_at_high_pitches() {
        let mut b = TableBuilder::new();
        let sr = 48_000.0;
        for (name, cycle) in [("saw", cycles::saw()), ("square", cycles::square()), ("triangle", cycles::triangle())] {
            let t = b.build(&cycle, true);
            for f0 in [2093.0, 4186.0, 8372.0] {
                let buf = render(&t, f0, sr, 1 << 16);
                let ratio = harmonic_alias_ratio_db(&buf, f0, sr);
                assert!(ratio >= 35.0, "{name} @ {f0} Hz: alias ratio {ratio:.1} dB");
            }
        }
    }

    #[test]
    fn low_pitch_saw_is_bright_and_bounded() {
        let mut b = TableBuilder::new();
        let t = b.build(&cycles::saw(), true);
        let buf = render(&t, 55.0, 48_000.0, 48_000);
        assert!(buf.iter().all(|s| s.is_finite()));
        let p = crate::math::peak(&buf);
        assert!(p > 0.9 && p < 1.3, "peak {p}");
        let (_, centroid, _) = crate::analysis::features(&buf, 48_000.0);
        assert!(centroid > 1500.0, "centroid {centroid}");
    }

    #[test]
    fn seam_wraps_continuously() {
        let mut b = TableBuilder::new();
        let t = b.build(&cycles::sine(), true);
        let a = t.read(0, 0.99999);
        let z = t.read(0, 0.0);
        let c = t.read(0, 0.00001);
        assert!((a - z).abs() < 1e-3 && (z - c).abs() < 1e-3);
        // table 0 is a sine of amplitude ~1
        assert!((t.read(0, 0.25) - 1.0).abs() < 1e-3);
        assert!((t.read(0, 0.75) + 1.0).abs() < 1e-3);
    }

    #[test]
    fn dc_removal_zeroes_mean() {
        let mut b = TableBuilder::new();
        let cycle: Vec<f32> = cycles::sine().iter().map(|s| s + 0.3).collect();
        let t = b.build(&cycle, true);
        let mean: f64 = t.level_slice(0).iter().map(|s| *s as f64).sum::<f64>() / TABLE_LEN as f64;
        assert!(mean.abs() < 1e-4);
        let t2 = b.build(&cycle, false);
        let mean2: f64 = t2.level_slice(0).iter().map(|s| *s as f64).sum::<f64>() / TABLE_LEN as f64;
        assert!((mean2 - 0.3).abs() < 1e-3);
    }

    #[test]
    fn analyze_synthesize_round_trip() {
        let mut b = TableBuilder::new();
        let cycle = cycles::soft_saw();
        let spec = b.analyze(&cycle);
        let back = b.synthesize(&spec);
        let err = cycle.iter().zip(&back).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(err < 1e-5, "round trip error {err}");
    }
}

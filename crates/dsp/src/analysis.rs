//! Offline analysis helpers (tests, previews, waveform thumbnails). Not RT-safe:
//! these allocate and are used off the audio thread only.

use realfft::RealFftPlanner;
use realfft::num_complex::Complex;

/// Magnitude spectrum (linear) of `buf` with a 4-term Blackman-Harris window
/// (sidelobes below -92 dB, main lobe ±4 bins). Length `n/2+1`.
pub fn magnitude_spectrum(buf: &[f32]) -> Vec<f32> {
    let n = buf.len().max(2) & !1;
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n);
    let mut input: Vec<f32> = fft.make_input_vec();
    let mut output: Vec<Complex<f32>> = fft.make_output_vec();
    for (i, s) in input.iter_mut().enumerate() {
        let x = std::f32::consts::TAU * i as f32 / n as f32;
        let w = 0.35875 - 0.48829 * x.cos() + 0.14128 * (2.0 * x).cos() - 0.01168 * (3.0 * x).cos();
        *s = buf.get(i).copied().unwrap_or(0.0) * w;
    }
    fft.process(&mut input, &mut output).expect("fft sizes match");
    output.iter().map(|c| c.norm() / n as f32).collect()
}

/// Estimate the dominant frequency in Hz with parabolic peak interpolation.
pub fn dominant_frequency(buf: &[f32], sample_rate: f64) -> f64 {
    let spec = magnitude_spectrum(buf);
    let n = (spec.len() - 1) * 2;
    let (mut k, mut best) = (0usize, 0.0f32);
    for (i, m) in spec.iter().enumerate().skip(1) {
        if *m > best {
            best = *m;
            k = i;
        }
    }
    if k == 0 || k + 1 >= spec.len() {
        return k as f64 * sample_rate / n as f64;
    }
    // parabolic interpolation on log magnitude
    let (a, b, c) = (spec[k - 1].max(1e-12).ln(), spec[k].max(1e-12).ln(), spec[k + 1].max(1e-12).ln());
    let denom = a - 2.0 * b + c;
    let delta = if denom.abs() < 1e-12 { 0.0 } else { 0.5 * (a - c) / denom };
    (k as f64 + f64::from(delta)) * sample_rate / n as f64
}

/// Ratio in dB between the fundamental's magnitude and the strongest spectral
/// peak that is **not** near a harmonic of `f0`. Higher = less aliasing.
pub fn harmonic_alias_ratio_db(buf: &[f32], f0: f64, sample_rate: f64) -> f64 {
    let spec = magnitude_spectrum(buf);
    let n = (spec.len() - 1) * 2;
    let bin_hz = sample_rate / n as f64;
    let fundamental_bin = (f0 / bin_hz).round() as usize;
    let fund = spec[fundamental_bin.saturating_sub(2)..(fundamental_bin + 3).min(spec.len())].iter().cloned().fold(0.0f32, f32::max);
    // Exclude ±6 bins (Blackman-Harris main lobe + leakage) around every harmonic.
    let tolerance_bins = 6.0;
    let mut worst = 0.0f32;
    for (i, m) in spec.iter().enumerate().skip(2) {
        let f = i as f64 * bin_hz;
        let harmonic = (f / f0).round().max(1.0);
        let dist_bins = (f - harmonic * f0).abs() / bin_hz;
        if dist_bins <= tolerance_bins {
            continue;
        }
        if *m > worst {
            worst = *m;
        }
    }
    if worst <= 0.0 {
        return 200.0;
    }
    20.0 * (f64::from(fund) / f64::from(worst)).log10()
}

/// Simple spectral features for perceptual difference tests: (rms, centroid Hz, spectral flatness).
pub fn features(buf: &[f32], sample_rate: f64) -> (f32, f64, f64) {
    let spec = magnitude_spectrum(buf);
    let n = (spec.len() - 1) * 2;
    let bin_hz = sample_rate / n as f64;
    let mut num = 0.0f64;
    let mut den = 0.0f64;
    let mut log_sum = 0.0f64;
    let mut count = 0usize;
    for (i, m) in spec.iter().enumerate().skip(1) {
        let m = f64::from(*m);
        num += i as f64 * bin_hz * m;
        den += m;
        log_sum += (m + 1e-12).ln();
        count += 1;
    }
    let centroid = if den > 0.0 { num / den } else { 0.0 };
    let geo = (log_sum / count.max(1) as f64).exp();
    let arith = den / count.max(1) as f64;
    let flatness = if arith > 0.0 { geo / arith } else { 0.0 };
    (crate::math::rms(buf), centroid, flatness)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dominant_frequency_of_pure_sine() {
        let sr = 48_000.0;
        let buf: Vec<f32> = (0..65536).map(|i| (std::f64::consts::TAU * 1234.5 * i as f64 / sr).sin() as f32).collect();
        let f = dominant_frequency(&buf, sr);
        assert!((f - 1234.5).abs() < 0.3, "{f}");
    }

    #[test]
    fn pure_sine_has_no_alias_energy() {
        let sr = 48_000.0;
        let buf: Vec<f32> = (0..65536).map(|i| (std::f64::consts::TAU * 1000.0 * i as f64 / sr).sin() as f32).collect();
        assert!(harmonic_alias_ratio_db(&buf, 1000.0, sr) > 60.0);
    }
}

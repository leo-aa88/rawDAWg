//! Numeric helpers shared by processors.

use std::f32::consts::PI;

/// Convert decibels to linear gain (f32 fast path).
#[inline]
pub fn db_to_lin(db: f32) -> f32 {
    if db <= -144.0 || !db.is_finite() { 0.0 } else { 10f32.powf(db / 20.0) }
}

/// Convert linear gain to decibels, floored at -144 dB.
#[inline]
pub fn lin_to_db(lin: f32) -> f32 {
    if lin <= 0.0 || !lin.is_finite() { -144.0 } else { (20.0 * lin.log10()).max(-144.0) }
}

/// Replace non-finite samples with zero and flush denormals. Returns the number
/// of samples that were non-finite (for overload/health counters).
#[inline]
pub fn sanitize(buf: &mut [f32]) -> u32 {
    let mut bad = 0u32;
    for s in buf.iter_mut() {
        if !s.is_finite() {
            *s = 0.0;
            bad += 1;
        } else if s.abs() < 1e-20 {
            *s = 0.0;
        }
    }
    bad
}

/// Peak absolute value of a buffer.
#[inline]
pub fn peak(buf: &[f32]) -> f32 {
    buf.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

/// RMS of a buffer.
#[inline]
pub fn rms(buf: &[f32]) -> f32 {
    if buf.is_empty() {
        return 0.0;
    }
    (buf.iter().map(|s| f64::from(*s) * f64::from(*s)).sum::<f64>() / buf.len() as f64).sqrt() as f32
}

/// Padé approximation of tanh, accurate to ~1e-4 on [-3,3], saturating outside.
#[inline]
pub fn fast_tanh(x: f32) -> f32 {
    let x = x.clamp(-4.97, 4.97);
    let x2 = x * x;
    let num = x * (135_135.0 + x2 * (17_325.0 + x2 * (378.0 + x2)));
    let den = 135_135.0 + x2 * (62_370.0 + x2 * (3150.0 + x2 * 28.0));
    (num / den).clamp(-1.0, 1.0)
}

/// Frequency (Hz) → normalized radian frequency at `sample_rate`.
#[inline]
pub fn omega(freq_hz: f32, sample_rate: f32) -> f32 {
    2.0 * PI * freq_hz / sample_rate
}

/// Add `src` into `dst` with gain.
#[inline]
pub fn mix_into(dst: &mut [f32], src: &[f32], gain: f32) {
    for (d, s) in dst.iter_mut().zip(src) {
        *d += s * gain;
    }
}

/// One-pole coefficient for a time constant (reaches ~63% in `time_s`).
#[inline]
pub fn one_pole_coeff(time_s: f32, sample_rate: f32) -> f32 {
    if time_s <= 0.0 { 1.0 } else { 1.0 - (-1.0 / (time_s * sample_rate)).exp() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_conversions() {
        assert!((db_to_lin(0.0) - 1.0).abs() < 1e-6);
        assert!((db_to_lin(-20.0) - 0.1).abs() < 1e-6);
        assert!((lin_to_db(0.5) + 6.0206).abs() < 1e-3);
        assert_eq!(db_to_lin(f32::NAN), 0.0);
        assert_eq!(lin_to_db(0.0), -144.0);
    }

    #[test]
    fn sanitize_removes_nan_and_denormals() {
        let mut b = [1.0, f32::NAN, f32::INFINITY, 1e-30, -0.5];
        assert_eq!(sanitize(&mut b), 2);
        assert_eq!(b, [1.0, 0.0, 0.0, 0.0, -0.5]);
    }

    #[test]
    fn fast_tanh_close_to_std() {
        for i in -300..=300 {
            let x = i as f32 / 100.0;
            assert!((fast_tanh(x) - x.tanh()).abs() < 2e-4, "x={x}");
        }
        assert!((fast_tanh(10.0) - 1.0).abs() < 1e-3);
    }

    #[test]
    fn rms_and_peak() {
        let b = [0.5f32, -0.5, 0.5, -0.5];
        assert!((rms(&b) - 0.5).abs() < 1e-6);
        assert_eq!(peak(&b), 0.5);
        assert_eq!(rms(&[]), 0.0);
    }
}

//! Waveform sculpting (SYN-02): brushes over one cyclic 2048-sample cycle.
//!
//! All operations work on circular sample indices (the seam wraps), use a
//! raised-cosine kernel of the brush radius, and are pure functions so the UI
//! can preview, coalesce and undo them at gesture granularity.

use rawdawg_dsp::rng::DeterministicRng;
use rawdawg_dsp::wavetable::{CYCLE_LEN, Spectrum, TableBuilder};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Brush {
    /// Set local amplitude toward the pointer value.
    #[default]
    Draw,
    /// Shift local samples vertically by the pointer's vertical motion.
    Deform,
    /// Circular low-pass (moving average) under the brush.
    Smooth,
    /// Add bounded high-pass difference (emphasize detail) under the brush.
    Sharpen,
    /// Remap local phase with a monotonic warp (pull samples toward/away from center).
    Stretch,
    /// Localized wavefold/waveshape.
    Distort,
    /// Add seeded noise under the brush.
    Noise,
}

impl Brush {
    pub const ALL: [Brush; 7] = [Brush::Draw, Brush::Deform, Brush::Smooth, Brush::Sharpen, Brush::Stretch, Brush::Distort, Brush::Noise];
    pub fn label(self) -> &'static str {
        match self {
            Brush::Draw => "Draw",
            Brush::Deform => "Deform",
            Brush::Smooth => "Smooth",
            Brush::Sharpen => "Sharpen",
            Brush::Stretch => "Stretch",
            Brush::Distort => "Distort",
            Brush::Noise => "Noise",
        }
    }
    /// One-sentence status tip (UI_UX.md: no DSP jargon).
    pub fn tip(self) -> &'static str {
        match self {
            Brush::Draw => "Drag to draw the shape directly.",
            Brush::Deform => "Drag up or down to push the wave around.",
            Brush::Smooth => "Drag to soften bumps and edges.",
            Brush::Sharpen => "Drag to bring out edges and detail.",
            Brush::Stretch => "Drag left or right to squeeze or widen this part of the wave.",
            Brush::Distort => "Drag to fold and crunch this part of the wave.",
            Brush::Noise => "Drag to sprinkle grit onto the wave.",
        }
    }
}

/// Brush settings. `radius` and `strength` are normalized 0..1.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct BrushSettings {
    pub brush: Brush,
    /// Fraction of the cycle covered by the brush (0.01..0.5).
    pub radius: f32,
    /// Effect strength 0..1.
    pub strength: f32,
}

impl Default for BrushSettings {
    fn default() -> Self {
        Self { brush: Brush::Draw, radius: 0.06, strength: 0.7 }
    }
}

impl BrushSettings {
    pub fn clamped(self) -> Self {
        Self {
            brush: self.brush,
            radius: if self.radius.is_finite() { self.radius.clamp(0.01, 0.5) } else { 0.06 },
            strength: if self.strength.is_finite() { self.strength.clamp(0.0, 1.0) } else { 0.7 },
        }
    }
    pub fn radius_samples(&self) -> usize {
        ((self.radius * CYCLE_LEN as f32) as usize).clamp(2, CYCLE_LEN / 2)
    }
}

/// One pointer sample of a stroke: phase in [0,1), amplitude in [-1,1], and the
/// motion since the previous sample (for Deform/Stretch).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct StrokePoint {
    pub phase: f32,
    pub value: f32,
    pub dphase: f32,
    pub dvalue: f32,
}

#[inline]
fn wrap(i: isize) -> usize {
    i.rem_euclid(CYCLE_LEN as isize) as usize
}

/// Raised-cosine weight for distance `d` (samples) at radius `r`.
#[inline]
fn kernel(d: f32, r: f32) -> f32 {
    if d >= r { 0.0 } else { 0.5 + 0.5 * (std::f32::consts::PI * d / r).cos() }
}

/// Apply one brush dab centered at `point`. Modifies `cycle` in place.
pub fn apply_dab(cycle: &mut [f32], settings: &BrushSettings, point: StrokePoint, seed: u64, dab_index: u64) {
    debug_assert_eq!(cycle.len(), CYCLE_LEN);
    let s = settings.clamped();
    let r = s.radius_samples() as f32;
    let center = (point.phase.rem_euclid(1.0) * CYCLE_LEN as f32) as isize;
    let strength = s.strength;
    let ri = r as isize;
    match s.brush {
        Brush::Draw => {
            let target = point.value.clamp(-1.0, 1.0);
            for o in -ri..=ri {
                let w = kernel(o.abs() as f32, r) * strength;
                let i = wrap(center + o);
                cycle[i] += (target - cycle[i]) * w;
            }
        }
        Brush::Deform => {
            let shift = point.dvalue.clamp(-1.0, 1.0) * strength;
            for o in -ri..=ri {
                let w = kernel(o.abs() as f32, r);
                let i = wrap(center + o);
                cycle[i] = (cycle[i] + shift * w).clamp(-1.5, 1.5);
            }
        }
        Brush::Smooth => {
            let half = ((r * 0.25) as isize).max(1);
            let src: Vec<f32> = cycle.to_vec();
            for o in -ri..=ri {
                let w = kernel(o.abs() as f32, r) * strength;
                let i = wrap(center + o);
                let mut acc = 0.0;
                for k in -half..=half {
                    acc += src[wrap(center + o + k)];
                }
                let avg = acc / (2 * half + 1) as f32;
                cycle[i] += (avg - cycle[i]) * w;
            }
        }
        Brush::Sharpen => {
            // unsharp mask: boost the difference between the sample and a local average
            let src: Vec<f32> = cycle.to_vec();
            let half = ((r * 0.125) as isize).max(2);
            for o in -ri..=ri {
                let w = kernel(o.abs() as f32, r) * strength;
                let i = wrap(center + o);
                let mut acc = 0.0;
                for k in -half..=half {
                    acc += src[wrap(center + o + k)];
                }
                let local = acc / (2 * half + 1) as f32;
                let detail = (src[i] - local).clamp(-0.5, 0.5);
                cycle[i] = (cycle[i] + detail * 3.0 * w).clamp(-1.5, 1.5);
            }
        }
        Brush::Stretch => {
            // monotonic warp: samples inside the radius are resampled from a
            // position pulled toward (dphase<0) or pushed away from the center
            let amount = (point.dphase * 8.0).clamp(-0.9, 0.9) * strength;
            let src: Vec<f32> = cycle.to_vec();
            for o in -ri..=ri {
                let w = kernel(o.abs() as f32, r);
                let i = wrap(center + o);
                // source offset: o * (1 - amount*w) keeps monotonicity for |amount*w| < 1
                let src_off = o as f32 * (1.0 - amount * w);
                let base = src_off.floor();
                let frac = src_off - base;
                let a = src[wrap(center + base as isize)];
                let b = src[wrap(center + base as isize + 1)];
                cycle[i] = a + (b - a) * frac;
            }
        }
        Brush::Distort => {
            let drive = 1.0 + 6.0 * strength;
            for o in -ri..=ri {
                let w = kernel(o.abs() as f32, r);
                let i = wrap(center + o);
                let x = cycle[i] * drive;
                // triangle wavefold into [-1,1]
                let folded = {
                    let t = (x + 1.0).rem_euclid(4.0);
                    if t < 2.0 { t - 1.0 } else { 3.0 - t }
                };
                cycle[i] += (folded - cycle[i]) * w;
            }
        }
        Brush::Noise => {
            let mut rng = DeterministicRng::new(seed, dab_index);
            for o in -ri..=ri {
                let w = kernel(o.abs() as f32, r) * strength * 0.5;
                let i = wrap(center + o);
                cycle[i] = (cycle[i] + rng.bipolar_f32() * w).clamp(-1.5, 1.5);
            }
        }
    }
}

/// Apply a stroke segment from `from` to `to`, interpolating dabs so fast
/// pointer motion leaves no gaps. Returns the number of dabs applied.
pub fn apply_stroke(cycle: &mut [f32], settings: &BrushSettings, from: StrokePoint, to: StrokePoint, seed: u64, dab_counter: &mut u64) -> usize {
    let s = settings.clamped();
    let step = (s.radius_samples() as f32 / 4.0).max(1.0) / CYCLE_LEN as f32;
    // shortest circular distance
    let mut dp = to.phase - from.phase;
    if dp > 0.5 {
        dp -= 1.0;
    } else if dp < -0.5 {
        dp += 1.0;
    }
    let n = ((dp.abs() / step).ceil() as usize).clamp(1, 512);
    for k in 1..=n {
        let t = k as f32 / n as f32;
        let p = StrokePoint {
            phase: (from.phase + dp * t).rem_euclid(1.0),
            value: from.value + (to.value - from.value) * t,
            dphase: dp / n as f32,
            dvalue: (to.value - from.value) / n as f32,
        };
        apply_dab(cycle, &s, p, seed, *dab_counter);
        *dab_counter += 1;
    }
    n
}

/// Spectral view helpers (SYN-06): harmonic magnitudes/phases of a cycle.
pub struct Harmonics {
    /// Magnitude per harmonic index (0 = DC), normalized so a full-scale sine is 1.0.
    pub magnitude: Vec<f32>,
    pub phase: Vec<f32>,
}

pub fn analyze(cycle: &[f32]) -> Harmonics {
    let mut b = TableBuilder::new();
    let spec = b.analyze(cycle);
    let scale = 2.0 / CYCLE_LEN as f32;
    Harmonics { magnitude: spec.iter().map(|c| c.norm() * scale).collect(), phase: spec.iter().map(|c| c.arg()).collect() }
}

/// Rebuild a cycle from harmonics (conjugate symmetry and DC/Nyquist handled by the real IFFT).
pub fn synthesize(h: &Harmonics) -> Vec<f32> {
    let mut b = TableBuilder::new();
    let scale = CYCLE_LEN as f32 / 2.0;
    let spec: Spectrum = h.magnitude.iter().zip(&h.phase).map(|(m, p)| realfft::num_complex::Complex::from_polar(m * scale, *p)).collect();
    b.synthesize(&spec)
}

/// Paint one harmonic's magnitude (linear, 0..1 relative to full-scale sine) and return the new cycle.
pub fn set_harmonic(cycle: &[f32], harmonic: usize, magnitude: f32, keep_phase: bool) -> Vec<f32> {
    let mut h = analyze(cycle);
    if harmonic < h.magnitude.len() {
        h.magnitude[harmonic] = magnitude.clamp(0.0, 2.0);
        if !keep_phase || h.magnitude[harmonic] == 0.0 {
            h.phase[harmonic] = -std::f32::consts::FRAC_PI_2; // sine-like phase
        }
    }
    synthesize(&h)
}

/// Smooth the magnitude envelope around `harmonic` (±width bins).
pub fn smooth_harmonics(cycle: &[f32], harmonic: usize, width: usize, strength: f32) -> Vec<f32> {
    let mut h = analyze(cycle);
    let src = h.magnitude.clone();
    let lo = harmonic.saturating_sub(width).max(1);
    let hi = (harmonic + width).min(src.len() - 2);
    for k in lo..=hi {
        let avg = (src[k - 1] + src[k] + src[k + 1]) / 3.0;
        h.magnitude[k] += (avg - src[k]) * strength.clamp(0.0, 1.0);
    }
    synthesize(&h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawdawg_dsp::wavetable::cycles;

    fn changed_indices(a: &[f32], b: &[f32]) -> Vec<usize> {
        a.iter().zip(b).enumerate().filter(|(_, (x, y))| (*x - *y).abs() > 1e-6).map(|(i, _)| i).collect()
    }

    #[test]
    fn each_brush_changes_only_samples_under_the_brush_and_wraps_at_seam() {
        for brush in Brush::ALL {
            let base = cycles::soft_saw();
            let mut c = base.clone();
            let s = BrushSettings { brush, radius: 0.05, strength: 0.8 };
            // dab at the seam (phase 0.0): changes must wrap to the end of the cycle
            let p = StrokePoint { phase: 0.0, value: 0.9, dphase: 0.01, dvalue: 0.3 };
            apply_dab(&mut c, &s, p, 1, 0);
            let changed = changed_indices(&base, &c);
            assert!(!changed.is_empty(), "{brush:?} changed nothing");
            let r = s.radius_samples();
            assert!(changed.iter().all(|&i| i <= r || i >= CYCLE_LEN - r), "{brush:?} touched samples outside the brush");
            assert!(changed.iter().any(|&i| i >= CYCLE_LEN - r), "{brush:?} did not wrap at the seam");
            assert!(c.iter().all(|v| v.is_finite() && v.abs() <= 1.5));
            // far away untouched
            assert_eq!(c[CYCLE_LEN / 2], base[CYCLE_LEN / 2]);
        }
    }

    #[test]
    fn draw_sets_value_and_noise_is_deterministic() {
        let mut c = cycles::sine();
        let s = BrushSettings { brush: Brush::Draw, radius: 0.03, strength: 1.0 };
        apply_dab(&mut c, &s, StrokePoint { phase: 0.25, value: -0.5, dphase: 0.0, dvalue: 0.0 }, 0, 0);
        assert!((c[CYCLE_LEN / 4] + 0.5).abs() < 1e-5);
        let mut a = cycles::sine();
        let mut b = cycles::sine();
        let n = BrushSettings { brush: Brush::Noise, radius: 0.1, strength: 1.0 };
        apply_dab(&mut a, &n, StrokePoint { phase: 0.5, value: 0.0, dphase: 0.0, dvalue: 0.0 }, 42, 7);
        apply_dab(&mut b, &n, StrokePoint { phase: 0.5, value: 0.0, dphase: 0.0, dvalue: 0.0 }, 42, 7);
        assert_eq!(a, b);
        let mut d = cycles::sine();
        apply_dab(&mut d, &n, StrokePoint { phase: 0.5, value: 0.0, dphase: 0.0, dvalue: 0.0 }, 43, 7);
        assert_ne!(a, d);
    }

    #[test]
    fn smooth_reduces_and_sharpen_increases_local_detail() {
        // maximum slew (largest sample-to-sample step) inside the brushed region is what
        // smoothing lowers and sharpening raises
        let region = (CYCLE_LEN / 2 - CYCLE_LEN / 12)..(CYCLE_LEN / 2 + CYCLE_LEN / 12);
        let max_slew = |c: &[f32]| region.clone().map(|i| (c[i] - c[i - 1]).abs()).fold(0.0f32, f32::max);
        let mut spiky = cycles::square();
        let s = BrushSettings { brush: Brush::Smooth, radius: 0.08, strength: 1.0 };
        let before = max_slew(&spiky);
        for _ in 0..4 {
            apply_dab(&mut spiky, &s, StrokePoint { phase: 0.5, value: 0.0, dphase: 0.0, dvalue: 0.0 }, 0, 0);
        }
        let after = max_slew(&spiky);
        assert!(after < before * 0.6, "smooth: {before} -> {after}");
        let mut soft = cycles::soft_saw();
        let sh = BrushSettings { brush: Brush::Sharpen, radius: 0.08, strength: 1.0 };
        let b2 = max_slew(&soft);
        for _ in 0..4 {
            apply_dab(&mut soft, &sh, StrokePoint { phase: 0.5, value: 0.0, dphase: 0.0, dvalue: 0.0 }, 0, 0);
        }
        let a2 = max_slew(&soft);
        assert!(a2 > b2 * 1.05, "sharpen: {b2} -> {a2}");
    }

    #[test]
    fn stroke_interpolates_across_the_seam_without_gaps() {
        let base = cycles::sine();
        let mut c = base.clone();
        let s = BrushSettings { brush: Brush::Draw, radius: 0.02, strength: 1.0 };
        let mut counter = 0;
        let from = StrokePoint { phase: 0.95, value: 0.0, dphase: 0.0, dvalue: 0.0 };
        let to = StrokePoint { phase: 0.05, value: 0.0, dphase: 0.0, dvalue: 0.0 };
        let n = apply_stroke(&mut c, &s, from, to, 0, &mut counter);
        assert!(n >= 4);
        // everything between 0.95 and 0.05 (through the seam) is now ~0
        for i in (0..CYCLE_LEN / 20).chain(CYCLE_LEN * 19 / 20..CYCLE_LEN) {
            assert!(c[i].abs() < 0.05, "gap at {i}: {}", c[i]);
        }
        // the far side is untouched
        assert_eq!(c[CYCLE_LEN / 2], base[CYCLE_LEN / 2]);
    }

    #[test]
    fn spectral_paint_changes_one_harmonic_by_the_requested_amount() {
        let cycle = cycles::soft_saw();
        let h0 = analyze(&cycle);
        let target = h0.magnitude[3] * 2.0; // +6.02 dB on harmonic 3
        let painted = set_harmonic(&cycle, 3, target, true);
        let h1 = analyze(&painted);
        let db = 20.0 * (h1.magnitude[3] / h0.magnitude[3]).log10();
        assert!((db - 6.02).abs() < 0.5, "harmonic 3 changed by {db:.2} dB");
        for k in [1usize, 2, 4, 5, 8] {
            let d = 20.0 * (h1.magnitude[k] / h0.magnitude[k]).log10();
            assert!(d.abs() < 0.1, "harmonic {k} drifted by {d:.2} dB");
        }
        // erasing a harmonic removes it
        let erased = set_harmonic(&cycle, 1, 0.0, true);
        assert!(analyze(&erased).magnitude[1] < 1e-4);
        // round trip without edits is exact
        let same = synthesize(&h0);
        assert!(cycle.iter().zip(&same).all(|(a, b)| (a - b).abs() < 1e-4));
        let sm = smooth_harmonics(&cycle, 10, 3, 1.0);
        assert!(sm.iter().all(|v| v.is_finite()));
    }
}

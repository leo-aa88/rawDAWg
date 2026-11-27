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

/// The most harmonics the harmonic editor builds a wave from.
pub const MAX_HARMONICS: usize = 64;

/// Phase of a sine-shaped harmonic in [`analyze`]'s convention.
const SINE_PHASE: f32 = -std::f32::consts::FRAC_PI_2;

/// Peak of a cycle built by [`build_harmonics`].
pub const HARMONIC_PEAK: f32 = 1.0;

/// A cycle made of exactly harmonics 1..=`levels.len()` at those levels (0..1, relative to each
/// other; above 1 counts as 1, not finite as 0) with the given phases (a sine's where none is
/// given), scaled to a peak of [`HARMONIC_PEAK`]. All levels at zero give silence.
pub fn build_harmonics(levels: &[f32], phases: &[f32]) -> Vec<f32> {
    let n = levels.len().min(MAX_HARMONICS).min(CYCLE_LEN / 2 - 1);
    let mut h = Harmonics { magnitude: vec![0.0; CYCLE_LEN / 2 + 1], phase: vec![0.0; CYCLE_LEN / 2 + 1] };
    for k in 1..=n {
        h.magnitude[k] = if levels[k - 1].is_finite() { levels[k - 1].clamp(0.0, 1.0) } else { 0.0 };
        h.phase[k] = phases.get(k - 1).copied().filter(|p| p.is_finite()).unwrap_or(SINE_PHASE);
    }
    let mut cycle = synthesize(&h);
    let peak = cycle.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    if peak > 1e-6 {
        let g = HARMONIC_PEAK / peak;
        cycle.iter_mut().for_each(|v| *v *= g);
    } else {
        cycle.iter_mut().for_each(|v| *v = 0.0);
    }
    cycle
}

/// The levels of harmonics 1..=`n` of `cycle` relative to the strongest of them (which reads
/// 1.0), with their phases: what [`build_harmonics`] needs to rebuild the same sound.
pub fn harmonic_levels(cycle: &[f32], n: usize) -> (Vec<f32>, Vec<f32>) {
    let h = analyze(cycle);
    let n = n.min(h.magnitude.len().saturating_sub(1));
    let strongest = h.magnitude[1..=n].iter().fold(0.0f32, |m, v| m.max(*v));
    let levels: Vec<f32> = h.magnitude[1..=n].iter().map(|m| if strongest > 1e-9 { (m / strongest).clamp(0.0, 1.0) } else { 0.0 }).collect();
    // a silent harmonic's phase is rounding noise (and differs between FFT code paths): call it
    // a sine, so raising it gives the same wave everywhere
    let phases = h.phase[1..=n].iter().zip(&levels).map(|(p, l)| if *l < 1e-6 { SINE_PHASE } else { *p }).collect();
    (levels, phases)
}

/// How much of `cycle`'s energy lies above harmonic `n` (0 = none, 1 = all of it).
pub fn energy_above(cycle: &[f32], n: usize) -> f32 {
    let h = analyze(cycle);
    let total: f32 = h.magnitude.iter().skip(1).map(|m| m * m).sum();
    let above: f32 = h.magnitude.iter().skip(n.saturating_add(1)).map(|m| m * m).sum();
    if total > 1e-12 { above / total } else { 0.0 }
}

/// The classic waves as harmonic levels and phases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HarmonicShape {
    Sine,
    Saw,
    Square,
    Triangle,
}

impl HarmonicShape {
    pub const ALL: [HarmonicShape; 4] = [HarmonicShape::Sine, HarmonicShape::Saw, HarmonicShape::Square, HarmonicShape::Triangle];

    pub fn label(self) -> &'static str {
        match self {
            HarmonicShape::Sine => "Sine",
            HarmonicShape::Saw => "Saw",
            HarmonicShape::Square => "Square",
            HarmonicShape::Triangle => "Triangle",
        }
    }

    /// Levels and phases of harmonics 1..=`n`: sine only the first; saw every harmonic at 1/k;
    /// square the odd ones at 1/k; triangle the odd ones at 1/k² with alternating sign.
    pub fn harmonics(self, n: usize) -> (Vec<f32>, Vec<f32>) {
        let n = n.clamp(1, MAX_HARMONICS);
        let flip = SINE_PHASE + std::f32::consts::PI;
        (1..=n)
            .map(|k| {
                let kf = k as f32;
                match self {
                    HarmonicShape::Sine => (if k == 1 { 1.0 } else { 0.0 }, SINE_PHASE),
                    // a rising ramp: alternating signs
                    HarmonicShape::Saw => (1.0 / kf, if k % 2 == 1 { SINE_PHASE } else { flip }),
                    HarmonicShape::Square => (if k % 2 == 1 { 1.0 / kf } else { 0.0 }, SINE_PHASE),
                    HarmonicShape::Triangle => (if k % 2 == 1 { 1.0 / (kf * kf) } else { 0.0 }, if k % 4 == 1 { SINE_PHASE } else { flip }),
                }
            })
            .unzip()
    }
}

/// Random harmonics 1..=`n`: the fundamental always sounds (at least half as loud as the
/// strongest, so the note keeps its pitch), every other harmonic is present or not at random
/// and, when present, at a random level; the strongest reads 1.0. Sine phases. The same `seed`
/// gives the same result.
pub fn random_harmonics(n: usize, seed: u64) -> (Vec<f32>, Vec<f32>) {
    let n = n.clamp(1, MAX_HARMONICS);
    let mut rng = DeterministicRng::new(seed, 0x4A2B_0001);
    let mut levels: Vec<f32> = (1..=n)
        .map(|k| {
            if k == 1 {
                0.5 + 0.5 * rng.unit_f32()
            } else if rng.unit_f32() < 0.5 {
                0.05 + 0.95 * rng.unit_f32()
            } else {
                0.0
            }
        })
        .collect();
    let strongest = levels.iter().fold(0.0f32, |m, l| m.max(*l));
    levels.iter_mut().for_each(|l| *l /= strongest);
    (levels, vec![SINE_PHASE; n])
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

    #[test]
    fn a_wave_built_from_harmonics_has_exactly_those_levels_and_nothing_above() {
        let levels = [1.0, 0.0, 0.5, 0.25, 0.0, 0.1];
        let cycle = build_harmonics(&levels, &[]);
        let peak = cycle.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!((peak - HARMONIC_PEAK).abs() < 1e-5, "peak {peak}");
        let (back, _) = harmonic_levels(&cycle, 8);
        for (k, want) in levels.iter().enumerate() {
            assert!((back[k] - want).abs() < 2e-3, "harmonic {}: {} vs {want}", k + 1, back[k]);
        }
        assert!(back[6] < 1e-3 && back[7] < 1e-3, "nothing above the sixth");
        assert!(energy_above(&cycle, 6) < 1e-6);
        assert!(energy_above(&cycles::saw(), 6) > 0.01, "a saw has more");
        // levels are relative: the same proportions at half the size give the same wave
        let half: Vec<f32> = levels.iter().map(|l| l * 0.5).collect();
        let again = build_harmonics(&half, &[]);
        assert!(cycle.iter().zip(&again).all(|(a, b)| (a - b).abs() < 1e-4));
        // the phases are kept: rebuilding from the analysis gives the same wave
        let (lv, ph) = harmonic_levels(&cycle, 6);
        let rebuilt = build_harmonics(&lv, &ph);
        assert!(cycle.iter().zip(&rebuilt).all(|(a, b)| (a - b).abs() < 2e-3));
        assert!(build_harmonics(&[0.0, 0.0], &[]).iter().all(|v| *v == 0.0), "silence, no NaN");
    }

    #[test]
    fn the_classic_shapes_have_their_harmonic_series() {
        let (sine, _) = HarmonicShape::Sine.harmonics(16);
        assert_eq!(sine.iter().filter(|l| **l > 0.0).count(), 1);
        let (saw, _) = HarmonicShape::Saw.harmonics(16);
        assert!(saw.iter().enumerate().all(|(i, l)| (l - 1.0 / (i + 1) as f32).abs() < 1e-6));
        let (square, _) = HarmonicShape::Square.harmonics(16);
        assert!(square.iter().enumerate().all(|(i, l)| if i % 2 == 0 { (l - 1.0 / (i + 1) as f32).abs() < 1e-6 } else { *l == 0.0 }));
        let (tri, ph) = HarmonicShape::Triangle.harmonics(16);
        assert!((tri[2] - 1.0 / 9.0).abs() < 1e-6 && tri[1] == 0.0);
        // a triangle built from them looks like one: straight ramps, peak at a quarter cycle
        let wave = build_harmonics(&tri, &ph);
        let quarter = wave[CYCLE_LEN / 4].abs();
        assert!(quarter > 0.97, "peak at a quarter cycle: {quarter}");
        assert!(wave[CYCLE_LEN / 8].abs() > 0.45 && wave[CYCLE_LEN / 8].abs() < 0.55, "halfway up the ramp");
        // a square from 16 harmonics is mostly flat at its peak level
        let (sq, ph) = HarmonicShape::Square.harmonics(16);
        let wave = build_harmonics(&sq, &ph);
        let flat = wave[CYCLE_LEN / 8..3 * CYCLE_LEN / 8].iter().filter(|v| v.abs() > 0.8).count();
        assert!(flat > CYCLE_LEN / 5, "{flat}");
    }

    #[test]
    fn random_harmonics_keep_the_fundamental_and_stay_within_the_count() {
        let (a, ph) = random_harmonics(16, 7);
        assert_eq!(a.len(), 16);
        assert_eq!(ph.len(), 16);
        assert!(a[0] >= 0.5, "the fundamental keeps the pitch");
        assert!(a.iter().all(|l| (0.0..=1.0).contains(l)));
        assert!(a.contains(&1.0), "the strongest reads 1.0");
        assert!(a.iter().skip(1).any(|l| *l == 0.0) && a.iter().skip(1).any(|l| *l > 0.0), "some present, some not: {a:?}");
        assert_eq!(random_harmonics(16, 7).0, a, "the same seed, the same harmonics");
        assert_ne!(random_harmonics(16, 8).0, a);
        assert_eq!(random_harmonics(200, 1).0.len(), MAX_HARMONICS);
        let cycle = build_harmonics(&a, &ph);
        assert!(energy_above(&cycle, 16) < 1e-6);
    }

    #[test]
    fn the_shapes_have_the_right_polarity_and_silent_harmonics_read_as_sines() {
        let n = CYCLE_LEN;
        // one harmonic is a rising sine: 0 at the start, +1 a quarter in
        let sine = build_harmonics(&[1.0], &[]);
        assert!(sine[0].abs() < 1e-3 && sine[n / 4] > 0.999 && sine[3 * n / 4] < -0.999);
        // the saw rises through zero at the start and jumps at half a cycle
        let (l, p) = HarmonicShape::Saw.harmonics(32);
        let saw = build_harmonics(&l, &p);
        assert!(saw[0].abs() < 0.02 && saw[n / 8] > 0.15 && saw[3 * n / 8] > saw[n / 8] + 0.3, "rising: {} {} {}", saw[0], saw[n / 8], saw[3 * n / 8]);
        assert!(saw[n / 2 + n / 8] < -0.15, "after the jump it is negative");
        // the square is high in the first half, the triangle peaks at +1 a quarter in
        let (l, p) = HarmonicShape::Square.harmonics(32);
        let sq = build_harmonics(&l, &p);
        assert!(sq[n / 4] > 0.8 && sq[3 * n / 4] < -0.8);
        let (l, p) = HarmonicShape::Triangle.harmonics(32);
        let tri = build_harmonics(&l, &p);
        assert!(tri[n / 4] > 0.97 && tri[3 * n / 4] < -0.97);
        // a harmonic a cycle does not have reads as a sine (not FFT rounding noise)
        let (levels, phases) = harmonic_levels(&rawdawg_dsp::wavetable::cycles::sine(), 8);
        for k in 1..8 {
            assert!(levels[k] < 1e-6);
            assert_eq!(phases[k], SINE_PHASE, "harmonic {}", k + 1);
        }
        // nonsense levels count as silence, not as a silent cycle
        let w = build_harmonics(&[1.0, f32::NAN, f32::INFINITY], &[]);
        assert!(w.iter().all(|v| v.is_finite()) && w[n / 4] > 0.99, "the fundamental still plays");
    }
}

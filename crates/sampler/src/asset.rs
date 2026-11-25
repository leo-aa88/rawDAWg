//! Prepared, immutable sample data shared by sampler voices.

use std::sync::Arc;

/// Decoded audio, planar f32, 1 or 2 channels.
#[derive(Clone, Debug, PartialEq)]
pub struct SampleAsset {
    pub name: String,
    pub sample_rate: u32,
    pub channels: Vec<Vec<f32>>,
}

impl SampleAsset {
    pub fn mono(name: &str, sample_rate: u32, samples: Vec<f32>) -> Arc<Self> {
        Arc::new(Self { name: name.to_owned(), sample_rate, channels: vec![samples] })
    }
    pub fn stereo(name: &str, sample_rate: u32, left: Vec<f32>, right: Vec<f32>) -> Arc<Self> {
        Arc::new(Self { name: name.to_owned(), sample_rate, channels: vec![left, right] })
    }
    pub fn frames(&self) -> usize {
        self.channels.first().map(|c| c.len()).unwrap_or(0)
    }
    pub fn is_stereo(&self) -> bool {
        self.channels.len() >= 2
    }
    pub fn is_valid(&self) -> bool {
        self.sample_rate >= 8000
            && !self.channels.is_empty()
            && self.channels.len() <= 2
            && self.frames() > 0
            && self.channels.iter().all(|c| c.len() == self.frames() && c.iter().all(|s| s.is_finite()))
    }
    /// 4-point Hermite read at fractional frame `pos` for channel `ch` (clamped at the ends).
    #[inline]
    pub fn read(&self, ch: usize, pos: f64) -> f32 {
        let c = &self.channels[ch.min(self.channels.len() - 1)];
        let n = c.len();
        if n == 0 {
            return 0.0;
        }
        let i = pos.floor();
        let frac = (pos - i) as f32;
        let i = i as isize;
        let at = |k: isize| -> f32 { c[(k.clamp(0, n as isize - 1)) as usize] };
        rawdawg_dsp::wavetable::hermite(at(i - 1), at(i), at(i + 1), at(i + 2), frac)
    }
    /// Hermite read inside a forward loop `[start, end)` (wraps for the neighbour taps).
    #[inline]
    pub fn read_looped(&self, ch: usize, pos: f64, start: usize, end: usize) -> f32 {
        let c = &self.channels[ch.min(self.channels.len() - 1)];
        let len = end.saturating_sub(start).max(1);
        let rel = (pos - start as f64).rem_euclid(len as f64);
        let i = rel.floor() as usize;
        let frac = (rel - i as f64) as f32;
        let at = |k: isize| -> f32 { c[start + ((i as isize + k).rem_euclid(len as isize)) as usize] };
        rawdawg_dsp::wavetable::hermite(at(-1), at(0), at(1), at(2), frac)
    }
}

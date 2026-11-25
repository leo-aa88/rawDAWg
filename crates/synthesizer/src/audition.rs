//! Offline audition rendering and candidate validation (non-RT).

use crate::patch::Patch;
use crate::synth::Synth;
use rawdawg_dsp::analysis::features;
use rawdawg_dsp::processor::{PrepareSpec, ProcessContext, Processor, TransportView};
use rawdawg_dsp::{AudioBlock, BusLayout};
use rawdawg_types::events::{EventSource, TimedEvent};

/// Audition settings used for previews and candidate validation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AuditionSpec {
    pub sample_rate: f64,
    pub pitch: u8,
    pub velocity: f32,
    pub hold_seconds: f32,
    pub tail_seconds: f32,
}

impl Default for AuditionSpec {
    fn default() -> Self {
        Self { sample_rate: 48_000.0, pitch: 60, velocity: 0.9, hold_seconds: 0.4, tail_seconds: 0.25 }
    }
}

/// Render a mono (L) audition of `patch` with one held note. Non-RT, allocates.
pub fn render_patch(patch: &Patch, spec: &AuditionSpec) -> Result<Vec<f32>, String> {
    let mut synth = Synth::new(patch.clone());
    let block = 256usize;
    synth.prepare(&PrepareSpec::new(spec.sample_rate, block, BusLayout::Stereo)).map_err(|e| e.to_string())?;
    let hold = (spec.hold_seconds.max(0.01) * spec.sample_rate as f32) as usize;
    let total = hold + (spec.tail_seconds.max(0.0) * spec.sample_rate as f32) as usize;
    let mut out = Vec::with_capacity(total);
    let mut l = vec![0.0f32; block];
    let mut r = vec![0.0f32; block];
    let transport = TransportView::stopped(spec.sample_rate);
    let mut pos = 0usize;
    while pos < total {
        let n = block.min(total - pos);
        let mut events: Vec<TimedEvent> = Vec::new();
        if pos == 0 {
            events.push(TimedEvent::note_on(0, EventSource::Preview, 0, 1, spec.pitch, spec.velocity));
        }
        if hold >= pos && hold < pos + n {
            events.push(TimedEvent::note_off((hold - pos) as u32, EventSource::Preview, 0, 1, spec.pitch));
        }
        let mut outs: [&mut [f32]; 2] = [&mut l[..n], &mut r[..n]];
        let mut ctx = ProcessContext { audio: AudioBlock::output_only(n, &mut outs), events: &events, transport: &transport };
        synth.process(&mut ctx);
        out.extend_from_slice(&l[..n]);
        pos += n;
    }
    Ok(out)
}

/// Perceptual summary used to compare candidates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SoundFeatures {
    pub rms: f32,
    pub peak: f32,
    pub centroid_hz: f64,
    pub flatness: f64,
    /// Fraction of the hold window still above -40 dB (sustain vs. percussive).
    pub sustain: f32,
}

impl SoundFeatures {
    pub fn of(buf: &[f32], sample_rate: f64) -> Self {
        let (rms, centroid, flatness) = features(buf, sample_rate);
        let peak = rawdawg_dsp::math::peak(buf);
        let win = (sample_rate * 0.02) as usize;
        let mut loud = 0usize;
        let mut total = 0usize;
        for chunk in buf.chunks(win.max(1)) {
            total += 1;
            if rawdawg_dsp::math::rms(chunk) > 0.01 {
                loud += 1;
            }
        }
        Self { rms, peak, centroid_hz: centroid, flatness, sustain: if total == 0 { 0.0 } else { loud as f32 / total as f32 } }
    }

    /// Perceptual distance (unitless): dB level, octave centroid, flatness, sustain.
    pub fn distance(&self, other: &Self) -> f32 {
        let db = (rawdawg_dsp::math::lin_to_db(self.rms) - rawdawg_dsp::math::lin_to_db(other.rms)).abs() / 6.0;
        let oct = ((self.centroid_hz.max(20.0) / other.centroid_hz.max(20.0)).log2().abs()) as f32;
        let flat = (self.flatness - other.flatness).abs() as f32 * 4.0;
        let sus = (self.sustain - other.sustain).abs() * 2.0;
        db + oct + flat + sus
    }

    /// Short descriptive tags for evolution cards.
    pub fn tags(&self) -> Vec<&'static str> {
        let mut t = Vec::new();
        t.push(if self.centroid_hz < 600.0 {
            "dark"
        } else if self.centroid_hz < 2500.0 {
            "warm"
        } else {
            "bright"
        });
        if self.flatness > 0.3 {
            t.push("noisy");
        }
        t.push(if self.sustain > 0.8 { "sustained" } else { "plucky" });
        if self.peak > 0.9 {
            t.push("loud");
        } else if self.rms < 0.02 {
            t.push("quiet");
        }
        t
    }
}

/// Why a candidate was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rejection {
    Invalid(String),
    NonFinite,
    Silent,
    TooLoud,
}

/// Validate a candidate by structure and by a short offline audition.
/// Returns the audition features on success.
pub fn validate_candidate(patch: &Patch, spec: &AuditionSpec) -> Result<SoundFeatures, Rejection> {
    patch.validate().map_err(Rejection::Invalid)?;
    let buf = render_patch(patch, spec).map_err(Rejection::Invalid)?;
    if buf.iter().any(|s| !s.is_finite()) {
        return Err(Rejection::NonFinite);
    }
    let f = SoundFeatures::of(&buf, spec.sample_rate);
    if f.rms < 0.003 {
        return Err(Rejection::Silent);
    }
    if f.peak > 2.0 {
        return Err(Rejection::TooLoud);
    }
    Ok(f)
}

/// Cheap, stable hash of a patch's state for preview caches.
pub fn patch_hash(patch: &Patch) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    patch.to_json().hash(&mut h);
    h.finish()
}

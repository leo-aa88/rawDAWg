//! Offline rendering: a separate engine instance driven block by block, no
//! audio hardware. Uses exactly the same `Engine::render_block` as playback.

use crate::commands::{EngineCommand, TransportMode};
use crate::engine::Engine;
use crate::plan::{CompileError, compile};
use rawdawg_project::Project;
use rawdawg_types::limits::EngineLimits;
use rawdawg_types::units::{SampleRate, Tick};
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub struct OfflineOptions {
    pub mode: TransportMode,
    /// Start/end in ticks; `None` end = schedule end (pattern length or song end).
    pub start_tick: Tick,
    pub end_tick: Option<Tick>,
    /// Extra tail rendered after the end (seconds).
    pub tail_seconds: f64,
    pub block_frames: usize,
    /// Override the project sample rate.
    pub sample_rate: Option<SampleRate>,
    /// Bypass master protection (unprotected float export, explicit choice).
    pub unprotected: bool,
    pub metronome: bool,
}

impl Default for OfflineOptions {
    fn default() -> Self {
        Self { mode: TransportMode::Pattern, start_tick: Tick::ZERO, end_tick: None, tail_seconds: 1.0, block_frames: 256, sample_rate: None, unprotected: false, metronome: false }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RenderStats {
    pub frames: usize,
    pub sample_rate: u32,
    pub peak: f32,
    pub clipped_samples: u64,
    pub nonfinite_blocks: u64,
    pub dropped_events: u64,
    pub render_seconds: f64,
    pub realtime_factor: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error(transparent)]
    Compile(#[from] CompileError),
    #[error("nothing to render: empty range")]
    EmptyRange,
    #[error("render too long ({seconds:.0} s, limit {limit:.0} s)")]
    TooLong { seconds: f64, limit: f64 },
    #[error("could not write {path}: {source}")]
    Write { path: String, source: hound::Error },
}

/// Maximum offline duration (24 h) to guard against runaway ranges.
const MAX_SECONDS: f64 = 24.0 * 3600.0;

/// Render to planar stereo buffers. Deterministic for the same inputs.
pub fn render_project(project: &Project, opts: &OfflineOptions) -> Result<(Vec<f32>, Vec<f32>, RenderStats), RenderError> {
    let started = std::time::Instant::now();
    let mut project = project.clone();
    if let Some(sr) = opts.sample_rate {
        project.sample_rate_hz = sr;
    }
    let rate = project.sample_rate_hz;
    let limits = EngineLimits::DEFAULT;
    let block = opts.block_frames.clamp(1, limits.max_block_frames as usize);
    let plan = compile(&project, 1, block, &limits, &std::collections::HashMap::new())?.plan;
    let sched = match opts.mode {
        TransportMode::Pattern => &plan.pattern_schedule,
        TransportMode::Song => &plan.song_schedule,
    };
    let start_frame = plan.tempo.tick_to_frame(opts.start_tick, rate);
    let end_frame = match opts.end_tick {
        Some(t) => plan.tempo.tick_to_frame(t, rate),
        None => sched.loop_end_frame,
    };
    if end_frame <= start_frame {
        return Err(RenderError::EmptyRange);
    }
    let tail_frames = (opts.tail_seconds.max(0.0) * rate.as_f64()).round() as i64;
    let total = (end_frame - start_frame + tail_frames) as usize;
    let seconds = total as f64 / rate.as_f64();
    if seconds > MAX_SECONDS {
        return Err(RenderError::TooLong { seconds, limit: MAX_SECONDS });
    }
    let (mut engine, mut handle) = Engine::new(rate, block, &limits);
    let old = engine.install_plan(plan);
    drop(old);
    handle.send(EngineCommand::SetMode(opts.mode)).ok();
    handle.send(EngineCommand::SetLoopEnabled(false)).ok();
    handle.send(EngineCommand::SetMasterProtection(!opts.unprotected)).ok();
    handle.send(EngineCommand::SetMetronome(opts.metronome)).ok();
    handle.send(EngineCommand::SeekTick(opts.start_tick.0)).ok();
    handle.send(EngineCommand::Play).ok();
    let mut left = vec![0.0f32; total];
    let mut right = vec![0.0f32; total];
    let mut pos = 0usize;
    let mut stopped = false;
    while pos < total {
        let n = block.min(total - pos);
        // stop the transport (release notes) once the musical range is done; the tail continues
        if !stopped && engine.position_frame() >= end_frame {
            handle.send(EngineCommand::Pause).ok();
            stopped = true;
        }
        let (l, r) = (&mut left[pos..pos + n], &mut right[pos..pos + n]);
        engine.render_block(l, r);
        pos += n;
    }
    let st = engine.status();
    let peak = rawdawg_dsp::math::peak(&left).max(rawdawg_dsp::math::peak(&right));
    let clipped = left.iter().chain(right.iter()).filter(|s| s.abs() >= 1.0).count() as u64;
    let elapsed = started.elapsed().as_secs_f64();
    let stats = RenderStats {
        frames: total,
        sample_rate: rate.0,
        peak,
        clipped_samples: clipped,
        nonfinite_blocks: st.nonfinite_blocks.get(),
        dropped_events: st.dropped_events.get(),
        render_seconds: elapsed,
        realtime_factor: if elapsed > 0.0 { seconds / elapsed } else { f64::INFINITY },
    };
    Ok((left, right, stats))
}

/// WAV output format for the headless renderer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WavFormat {
    Pcm16,
    Pcm24,
    Float32,
}

/// Render and write a WAV (no normalization, no hidden processing).
pub fn render_project_to_wav(project: &Project, opts: &OfflineOptions, format: WavFormat, path: &Path) -> Result<RenderStats, RenderError> {
    let (l, r, stats) = render_project(project, opts)?;
    write_wav(&l, &r, stats.sample_rate, format, path).map_err(|e| RenderError::Write { path: path.display().to_string(), source: e })?;
    Ok(stats)
}

/// Write planar stereo to WAV. Float is written as-is; PCM uses TPDF dither for 16-bit
/// and rounding with saturation.
pub fn write_wav(l: &[f32], r: &[f32], sample_rate: u32, format: WavFormat, path: &Path) -> Result<(), hound::Error> {
    let spec = match format {
        WavFormat::Pcm16 => hound::WavSpec { channels: 2, sample_rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int },
        WavFormat::Pcm24 => hound::WavSpec { channels: 2, sample_rate, bits_per_sample: 24, sample_format: hound::SampleFormat::Int },
        WavFormat::Float32 => hound::WavSpec { channels: 2, sample_rate, bits_per_sample: 32, sample_format: hound::SampleFormat::Float },
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    let n = l.len().min(r.len());
    // deterministic TPDF dither for 16-bit
    let mut rng = rawdawg_dsp::rng::DeterministicRng::new(0x5EED_D17E, 16);
    for i in 0..n {
        for s in [l[i], r[i]] {
            match format {
                WavFormat::Float32 => w.write_sample(s)?,
                WavFormat::Pcm24 => {
                    let v = (f64::from(s.clamp(-1.0, 1.0)) * 8_388_607.0).round() as i32;
                    w.write_sample(v)?;
                }
                WavFormat::Pcm16 => {
                    let dither = (rng.unit_f32() + rng.unit_f32() - 1.0) / 32768.0;
                    let v = ((s + dither).clamp(-1.0, 1.0) * 32767.0).round() as i32;
                    w.write_sample(v.clamp(-32768, 32767) as i16)?;
                }
            }
        }
    }
    w.finalize()
}

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
    /// Project folder for resolving assets (samples, SoundFonts); `None` = no assets.
    pub project_dir: Option<std::path::PathBuf>,
}

impl Default for OfflineOptions {
    fn default() -> Self {
        Self {
            mode: TransportMode::Pattern,
            start_tick: Tick::ZERO,
            end_tick: None,
            tail_seconds: 1.0,
            block_frames: 256,
            sample_rate: None,
            unprotected: false,
            metronome: false,
            project_dir: None,
        }
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
    /// Streamed-clip frames that were still missing after waiting (should be 0 offline).
    pub stream_dropouts: u64,
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
    #[error("could not write {path}: {reason}")]
    WriteOther { path: String, reason: String },
    #[error("export cancelled")]
    Cancelled,
}

/// Audio export formats (IO-06). MP3 is import-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Wav(WavFormat),
    Flac16,
    Flac24,
}

impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Wav(_) => "wav",
            ExportFormat::Flac16 | ExportFormat::Flac24 => "flac",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            ExportFormat::Wav(WavFormat::Pcm16) => "WAV 16-bit PCM",
            ExportFormat::Wav(WavFormat::Pcm24) => "WAV 24-bit PCM",
            ExportFormat::Wav(WavFormat::Float32) => "WAV 32-bit float",
            ExportFormat::Flac16 => "FLAC 16-bit",
            ExportFormat::Flac24 => "FLAC 24-bit",
        }
    }
    pub const ALL: [ExportFormat; 5] =
        [ExportFormat::Wav(WavFormat::Float32), ExportFormat::Wav(WavFormat::Pcm24), ExportFormat::Wav(WavFormat::Pcm16), ExportFormat::Flac24, ExportFormat::Flac16];
}

/// Per-channel stem buffers: (mixer channel, left, right) in plan order.
pub type StemBuffers = Vec<(rawdawg_types::MixerChannelId, Vec<f32>, Vec<f32>)>;

/// One exported stem file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StemFile {
    pub name: String,
    pub path: std::path::PathBuf,
    pub mixer_id: rawdawg_types::MixerChannelId,
}

/// Maximum offline duration (24 h) to guard against runaway ranges.
const MAX_SECONDS: f64 = 24.0 * 3600.0;

/// Render to planar stereo buffers. Deterministic for the same inputs.
pub fn render_project(project: &Project, opts: &OfflineOptions) -> Result<(Vec<f32>, Vec<f32>, RenderStats), RenderError> {
    render_project_with(project, opts, &mut |_, _| true, None)
}

/// Render with progress (`done_frames, total_frames` after every block; return `false`
/// to cancel → `RenderError::Cancelled`) and optional per-channel stems: when `stems` is
/// given it receives, in plan order, the post-fader signal of every mixer channel (master
/// included) with exactly the main render's length.
pub fn render_project_with(
    project: &Project,
    opts: &OfflineOptions,
    progress: &mut dyn FnMut(usize, usize) -> bool,
    mut stems: Option<&mut StemBuffers>,
) -> Result<(Vec<f32>, Vec<f32>, RenderStats), RenderError> {
    let started = std::time::Instant::now();
    let mut project = project.clone();
    if let Some(sr) = opts.sample_rate {
        project.sample_rate_hz = sr;
    }
    let rate = project.sample_rate_hz;
    let limits = EngineLimits::DEFAULT;
    let block = opts.block_frames.clamp(1, limits.max_block_frames as usize);
    let mut cache = crate::assets::ProjectAssetCache::new(opts.project_dir.clone());
    cache.sync(opts.project_dir.clone(), &project);
    let compiled = compile(&project, 1, block, &limits, &std::collections::HashMap::new(), &cache)?;
    let _stream_worker = compiled.stream_worker;
    let plan = compiled.plan;
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
    if let Some(stems) = stems.as_deref_mut() {
        engine.enable_stems();
        stems.clear();
        for id in engine.mixer_ids() {
            stems.push((id, vec![0.0f32; total], vec![0.0f32; total]));
        }
    }
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
        // streamed clips: wait (outside the audio path) until their data is in
        let waited = std::time::Instant::now();
        while !engine.streams_ready(n) && waited.elapsed() < std::time::Duration::from_secs(10) {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let (l, r) = (&mut left[pos..pos + n], &mut right[pos..pos + n]);
        engine.render_block(l, r);
        if let Some(stems) = stems.as_deref_mut() {
            for (i, (_, sl, sr)) in stems.iter_mut().enumerate() {
                if let Some((bl, br)) = engine.stem(i, n) {
                    sl[pos..pos + n].copy_from_slice(bl);
                    sr[pos..pos + n].copy_from_slice(br);
                }
            }
        }
        pos += n;
        if !progress(pos, total) {
            return Err(RenderError::Cancelled);
        }
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
        stream_dropouts: st.stream_dropouts.get(),
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

/// Render and write an audio file in `format` (WAV or FLAC), reporting progress.
pub fn export_project(
    project: &Project,
    opts: &OfflineOptions,
    format: ExportFormat,
    path: &Path,
    progress: &mut dyn FnMut(usize, usize) -> bool,
) -> Result<RenderStats, RenderError> {
    let (l, r, stats) = render_project_with(project, opts, progress, None)?;
    write_audio(&l, &r, stats.sample_rate, format, path)?;
    Ok(stats)
}

/// Render once and write one file per mixer channel (post-fader, before its output; the
/// master's own stem is the mix) into `dir`, named `<index>-<channel name>.<ext>` with
/// sanitized unique names. Every stem has the main render's exact length. Stems of
/// channels that share sends or sidechains need not sum to the master (documented).
pub fn export_stems(
    project: &Project,
    opts: &OfflineOptions,
    format: ExportFormat,
    dir: &Path,
    progress: &mut dyn FnMut(usize, usize) -> bool,
) -> Result<(Vec<StemFile>, RenderStats), RenderError> {
    let mut stems: StemBuffers = Vec::new();
    let (_, _, stats) = render_project_with(project, opts, progress, Some(&mut stems))?;
    std::fs::create_dir_all(dir).map_err(|e| RenderError::WriteOther { path: dir.display().to_string(), reason: e.to_string() })?;
    let mut used = std::collections::HashSet::new();
    let mut files = Vec::with_capacity(stems.len());
    for (i, (id, l, r)) in stems.iter().enumerate() {
        let raw = project.mixer.channels.iter().find(|m| m.id == *id).map(|m| m.name.clone()).unwrap_or_else(|| "channel".into());
        let mut base: String = raw.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '_' }).collect::<String>().trim().to_owned();
        if base.is_empty() {
            base = "channel".into();
        }
        let mut name = format!("{:02}-{}", i + 1, base);
        let mut k = 2;
        while !used.insert(name.to_lowercase()) {
            name = format!("{:02}-{}-{k}", i + 1, base);
            k += 1;
        }
        let path = dir.join(format!("{name}.{}", format.extension()));
        write_audio(l, r, stats.sample_rate, format, &path)?;
        files.push(StemFile { name, path, mixer_id: *id });
    }
    Ok((files, stats))
}

/// Write planar stereo in any export format.
pub fn write_audio(l: &[f32], r: &[f32], sample_rate: u32, format: ExportFormat, path: &Path) -> Result<(), RenderError> {
    match format {
        ExportFormat::Wav(f) => write_wav(l, r, sample_rate, f, path).map_err(|e| RenderError::Write { path: path.display().to_string(), source: e }),
        ExportFormat::Flac16 => write_flac(l, r, sample_rate, 16, path),
        ExportFormat::Flac24 => write_flac(l, r, sample_rate, 24, path),
    }
}

/// Write planar stereo to FLAC (16-bit with TPDF dither, 24-bit rounded), lossless after
/// quantisation, no normalization.
pub fn write_flac(l: &[f32], r: &[f32], sample_rate: u32, bits: usize, path: &Path) -> Result<(), RenderError> {
    use flacenc::component::BitRepr;
    use flacenc::error::Verify;
    let n = l.len().min(r.len());
    let full = ((1i64 << (bits - 1)) - 1) as f64;
    let mut rng = rawdawg_dsp::rng::DeterministicRng::new(0x5EED_D17E, bits as u64);
    let mut interleaved = Vec::with_capacity(n * 2);
    for i in 0..n {
        for s in [l[i], r[i]] {
            let v = if bits == 16 {
                let dither = (rng.unit_f32() + rng.unit_f32() - 1.0) / 32768.0;
                ((f64::from((s + dither).clamp(-1.0, 1.0))) * full).round()
            } else {
                (f64::from(s.clamp(-1.0, 1.0)) * full).round()
            };
            interleaved.push(v.clamp(-full - 1.0, full) as i32);
        }
    }
    let err = |reason: String| RenderError::WriteOther { path: path.display().to_string(), reason };
    let config = flacenc::config::Encoder::default().into_verified().map_err(|e| err(format!("{e:?}")))?;
    let source = flacenc::source::MemSource::from_samples(&interleaved, 2, bits, sample_rate as usize);
    let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size).map_err(|e| err(format!("{e:?}")))?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream.write(&mut sink).map_err(|e| err(format!("{e:?}")))?;
    let mut bytes = sink.into_inner();
    // flacenc 0.5 records the (shorter) last block as STREAMINFO min_block_size although it
    // numbers frames by index, which the FLAC spec reserves for fixed-blocksize streams
    // (min == max); decoders such as symphonia reject every frame of such a file. Mark the
    // stream fixed-size: bytes 8..10 (min) := bytes 10..12 (max) of the STREAMINFO block.
    if bytes.len() > 12 && &bytes[..4] == b"fLaC" && bytes[4] & 0x7f == 0 {
        let (min, max) = ((u16::from(bytes[8]) << 8) | u16::from(bytes[9]), (u16::from(bytes[10]) << 8) | u16::from(bytes[11]));
        if min != max {
            bytes[8] = bytes[10];
            bytes[9] = bytes[11];
        }
    }
    std::fs::write(path, &bytes).map_err(|e| err(e.to_string()))
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

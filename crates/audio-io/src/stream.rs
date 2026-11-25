//! Output stream lifecycle: negotiate a format, open, and run a prepared
//! real-time render source. Sample-type conversion happens at the edge.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SizedSample};
use rawdawg_rt::shared::{AtomicF64, Counter};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Per-callback information passed to the render source.
#[derive(Clone, Copy, Debug)]
pub struct CallbackInfo {
    pub sample_rate: u32,
    /// Monotonic count of frames rendered before this call.
    pub stream_frame: u64,
    /// Output latency reported by the backend, if known (seconds).
    pub output_latency_s: Option<f64>,
}

/// Prepared real-time renderer. All methods are called on the audio thread and
/// must obey the RT rules.
pub trait RenderSource: Send + 'static {
    /// Fill `left`/`right` (equal length, already zeroed) for one chunk.
    fn render(&mut self, left: &mut [f32], right: &mut [f32], info: &CallbackInfo);
}

#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct StreamConfigRequest {
    /// Device id from [`crate::DeviceInfo::id`], or `None` for the default output.
    pub device_id: Option<String>,
    pub sample_rate_hz: Option<u32>,
    pub buffer_frames: Option<u32>,
}

#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    #[error("no audio output device is available")]
    NoDevice,
    #[error("audio device {0} was not found")]
    DeviceNotFound(String),
    #[error("device does not support the requested configuration ({0}); nearest supported: {1}")]
    UnsupportedConfig(String, String),
    #[error("could not open the audio stream: {0}")]
    Build(String),
    #[error("could not start the audio stream: {0}")]
    Play(String),
    #[error("the audio stream was lost: {0}")]
    Lost(String),
}

/// Shared status, updated by the audio thread with atomics only.
#[derive(Debug, Default)]
pub struct StreamStatus {
    pub callbacks: Counter,
    pub frames: AtomicU64,
    pub xruns: Counter,
    pub oversized_callbacks: Counter,
    pub last_callback_frames: AtomicU64,
    pub last_callback_ns: AtomicU64,
    pub max_callback_ns: AtomicU64,
    pub output_latency_s: AtomicF64,
    pub failed: AtomicBool,
    pub errors: Counter,
    pub error: Mutex<Option<String>>,
}

impl StreamStatus {
    pub fn error_message(&self) -> Option<String> {
        self.error.lock().ok().and_then(|g| g.clone())
    }
    /// Fraction of the buffer deadline used by the last callback.
    pub fn last_load(&self, sample_rate: u32) -> f64 {
        let frames = self.last_callback_frames.load(Ordering::Relaxed);
        if frames == 0 || sample_rate == 0 {
            return 0.0;
        }
        let deadline_ns = frames as f64 / f64::from(sample_rate) * 1e9;
        self.last_callback_ns.load(Ordering::Relaxed) as f64 / deadline_ns
    }
}

/// Negotiated, actually-opened configuration (requested vs actual is shown in the UI).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActualConfig {
    pub device_id: String,
    pub device_name: String,
    pub backend: crate::AudioBackend,
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub sample_format: String,
    /// Fixed buffer size we asked for, if the backend accepted a fixed size.
    pub requested_buffer_frames: Option<u32>,
}

pub struct OutputStream {
    _stream: cpal::Stream,
    pub status: Arc<StreamStatus>,
    pub config: ActualConfig,
}

/// Maximum frames prepared per chunk; larger callbacks are split.
pub const MAX_CHUNK_FRAMES: usize = 8192;

struct Converter<S: RenderSource> {
    source: S,
    left: Vec<f32>,
    right: Vec<f32>,
    channels: usize,
    sample_rate: u32,
    status: Arc<StreamStatus>,
    stream_frame: u64,
    last_callback: Option<Instant>,
}

impl<S: RenderSource> Converter<S> {
    fn fill<T: SizedSample + FromSample<f32>>(&mut self, data: &mut [T], info: &cpal::OutputCallbackInfo) {
        let start = Instant::now();
        let total_frames = data.len() / self.channels.max(1);
        // xrun heuristic: a gap much longer than the buffer duration between callbacks
        if let Some(prev) = self.last_callback {
            let expected = total_frames as f64 / f64::from(self.sample_rate);
            let gap = start.duration_since(prev).as_secs_f64();
            if gap > expected * 2.5 + 0.002 {
                self.status.xruns.incr();
            }
        }
        self.last_callback = Some(start);
        let latency = {
            let ts = info.timestamp();
            ts.playback.checked_duration_since(ts.callback).map(|d| d.as_secs_f64())
        };
        if let Some(l) = latency {
            self.status.output_latency_s.store(l);
        }
        if total_frames > MAX_CHUNK_FRAMES {
            self.status.oversized_callbacks.incr();
        }
        let mut done = 0usize;
        while done < total_frames {
            let n = (total_frames - done).min(MAX_CHUNK_FRAMES);
            let (l, r) = (&mut self.left[..n], &mut self.right[..n]);
            l.fill(0.0);
            r.fill(0.0);
            let cb = CallbackInfo { sample_rate: self.sample_rate, stream_frame: self.stream_frame, output_latency_s: latency };
            self.source.render(l, r, &cb);
            let frames = &mut data[done * self.channels..(done + n) * self.channels];
            match self.channels {
                1 => {
                    for (i, f) in frames.iter_mut().enumerate() {
                        *f = T::from_sample(0.5 * (l[i] + r[i]));
                    }
                }
                _ => {
                    for (i, frame) in frames.chunks_exact_mut(self.channels).enumerate() {
                        frame[0] = T::from_sample(l[i]);
                        frame[1] = T::from_sample(r[i]);
                        for extra in &mut frame[2..] {
                            *extra = T::from_sample(0.0f32);
                        }
                    }
                }
            }
            self.stream_frame += n as u64;
            done += n;
        }
        let ns = start.elapsed().as_nanos() as u64;
        self.status.callbacks.incr();
        self.status.frames.fetch_add(total_frames as u64, Ordering::Relaxed);
        self.status.last_callback_frames.store(total_frames as u64, Ordering::Relaxed);
        self.status.last_callback_ns.store(ns, Ordering::Relaxed);
        self.status.max_callback_ns.fetch_max(ns, Ordering::Relaxed);
    }
}

fn find_device(req: &StreamConfigRequest) -> Result<(cpal::Host, cpal::Device), StreamError> {
    if let Some(id) = &req.device_id {
        for host_id in cpal::available_hosts() {
            let Ok(host) = cpal::host_from_id(host_id) else { continue };
            let Ok(devices) = host.devices() else { continue };
            for d in devices {
                if d.id().map(|i| i.to_string()).as_deref() == Ok(id.as_str()) {
                    return Ok((host, d));
                }
            }
        }
        return Err(StreamError::DeviceNotFound(id.clone()));
    }
    let host = cpal::default_host();
    let dev = host.default_output_device().ok_or(StreamError::NoDevice)?;
    Ok((host, dev))
}

fn format_rank(f: cpal::SampleFormat) -> u8 {
    match f {
        cpal::SampleFormat::F32 => 0,
        cpal::SampleFormat::I32 => 1,
        cpal::SampleFormat::I16 => 2,
        cpal::SampleFormat::U16 => 3,
        _ => 9,
    }
}

/// A candidate configuration range as offered by a device (backend-independent).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigRange {
    pub channels: u16,
    pub min_rate_hz: u32,
    pub max_rate_hz: u32,
    pub sample_format: cpal::SampleFormat,
}

impl ConfigRange {
    fn contains(&self, rate: u32) -> bool {
        self.min_rate_hz <= rate && rate <= self.max_rate_hz
    }
}

/// Pure selection policy (testable without devices): prefer exactly stereo, then
/// more channels, then mono; prefer f32, then i32/i16/u16. Returns the chosen
/// range, or the list of what the device offers for the error message.
pub fn choose_config(ranges: &[ConfigRange], want_rate: u32) -> Result<ConfigRange, Vec<String>> {
    let mut candidates: Vec<&ConfigRange> = ranges.iter().filter(|r| r.channels >= 1 && r.contains(want_rate)).collect();
    candidates.sort_by_key(|r| {
        let ch_rank = match r.channels {
            2 => 0,
            n if n > 2 => 1,
            _ => 2,
        };
        (ch_rank, format_rank(r.sample_format))
    });
    if let Some(r) = candidates.first() {
        return Ok((*r).clone());
    }
    let mut nearest: Vec<String> = ranges.iter().map(|r| format!("{}ch {}-{} Hz {:?}", r.channels, r.min_rate_hz, r.max_rate_hz, r.sample_format)).collect();
    nearest.sort();
    nearest.dedup();
    Err(nearest)
}

/// Project rates a device can actually open (for the nearest-supported suggestion).
pub fn offered_project_rates(ranges: &[ConfigRange]) -> Vec<u32> {
    rawdawg_types::SampleRate::SUPPORTED.iter().copied().filter(|r| ranges.iter().any(|c| c.contains(*r))).collect()
}

/// Negotiate a stereo (or mono) output config for the request.
fn negotiate(device: &cpal::Device, req: &StreamConfigRequest) -> Result<cpal::SupportedStreamConfig, StreamError> {
    let default = device.default_output_config().map_err(|e| StreamError::Build(e.to_string()))?;
    let want_rate = req.sample_rate_hz.unwrap_or(default.sample_rate());
    let ranges: Vec<cpal::SupportedStreamConfigRange> = device.supported_output_configs().map_err(|e| StreamError::Build(e.to_string()))?.collect();
    let plain: Vec<ConfigRange> = ranges
        .iter()
        .map(|r| ConfigRange { channels: r.channels(), min_rate_hz: r.min_sample_rate(), max_rate_hz: r.max_sample_rate(), sample_format: r.sample_format() })
        .collect();
    match choose_config(&plain, want_rate) {
        Ok(chosen) => {
            let r = ranges
                .iter()
                .find(|r| {
                    r.channels() == chosen.channels
                        && r.min_sample_rate() == chosen.min_rate_hz
                        && r.max_sample_rate() == chosen.max_rate_hz
                        && r.sample_format() == chosen.sample_format
                })
                .expect("chosen range comes from the list");
            Ok((*r).with_sample_rate(want_rate))
        }
        Err(nearest) => {
            let rates = offered_project_rates(&plain);
            let hint = if rates.is_empty() { nearest.join("; ") } else { format!("project rates offered: {rates:?}; {}", nearest.join("; ")) };
            Err(StreamError::UnsupportedConfig(format!("{want_rate} Hz"), hint))
        }
    }
}

impl OutputStream {
    /// Open and start an output stream. Non-RT. `source` must already be prepared
    /// for the negotiated sample rate; use [`Self::negotiate_only`] first to learn it.
    pub fn open<S: RenderSource>(req: &StreamConfigRequest, make_source: impl FnOnce(u32) -> S) -> Result<Self, StreamError> {
        let (host, device) = find_device(req)?;
        let supported = negotiate(&device, req)?;
        let mut config = supported.config();
        let mut requested_buffer = None;
        if let Some(b) = req.buffer_frames {
            let ok = match supported.buffer_size() {
                cpal::SupportedBufferSize::Range { min, max } => (*min..=*max).contains(&b),
                cpal::SupportedBufferSize::Unknown => true,
            };
            if ok {
                config.buffer_size = cpal::BufferSize::Fixed(b);
                requested_buffer = Some(b);
            }
        }
        let status = Arc::new(StreamStatus::default());
        let channels = usize::from(config.channels);
        let sample_rate = config.sample_rate;
        let device_id = device.id().map(|i| i.to_string()).unwrap_or_default();
        let device_name = device.description().map(|d| d.name().to_owned()).unwrap_or_else(|_| device_id.clone());
        let actual = ActualConfig {
            device_id,
            device_name,
            backend: crate::devices::backend_of(host.id()),
            sample_rate_hz: sample_rate,
            channels: config.channels,
            sample_format: format!("{:?}", supported.sample_format()),
            requested_buffer_frames: requested_buffer,
        };
        let converter = Converter {
            source: make_source(sample_rate),
            left: vec![0.0; MAX_CHUNK_FRAMES],
            right: vec![0.0; MAX_CHUNK_FRAMES],
            channels,
            sample_rate,
            status: status.clone(),
            stream_frame: 0,
            last_callback: None,
        };
        let err_status = status.clone();
        let err_cb = move |e: cpal::Error| {
            // Backends can report the same failure on every period: count, log the
            // first occurrence and then every 1000th, never flood.
            let n = err_status.errors.get();
            err_status.errors.incr();
            if n == 0 || n.is_multiple_of(1000) {
                log::error!("audio stream error (#{n}): {e}");
            }
            err_status.failed.store(true, Ordering::SeqCst);
            if n == 0
                && let Ok(mut g) = err_status.error.lock()
            {
                *g = Some(e.to_string());
            }
        };
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => build::<f32, S>(&device, &config, converter, err_cb),
            cpal::SampleFormat::I16 => build::<i16, S>(&device, &config, converter, err_cb),
            cpal::SampleFormat::U16 => build::<u16, S>(&device, &config, converter, err_cb),
            cpal::SampleFormat::I32 => build::<i32, S>(&device, &config, converter, err_cb),
            other => {
                // fall back to i16 conversion request; cpal will refuse if unsupported
                log::warn!("unusual sample format {other:?}; requesting i16");
                build::<i16, S>(&device, &config, converter, err_cb)
            }
        }
        .map_err(|e| StreamError::Build(e.to_string()))?;
        stream.play().map_err(|e| StreamError::Play(e.to_string()))?;
        let out = Self { _stream: stream, status, config: actual };
        // Some backends accept a configuration and only fail once running (e.g. a
        // fixed period size the server rejects). Give the stream a short window to
        // either call back or report an error so callers can fall back explicitly.
        let deadline = Instant::now() + std::time::Duration::from_millis(250);
        while Instant::now() < deadline {
            if out.is_failed() {
                return Err(StreamError::Lost(out.status.error_message().unwrap_or_else(|| "stream failed right after start".into())));
            }
            if out.status.callbacks.get() >= 2 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        Ok(out)
    }

    /// Resolve the sample rate and channel count that `open` would use, without opening.
    pub fn negotiate_only(req: &StreamConfigRequest) -> Result<(u32, u16), StreamError> {
        let (_host, device) = find_device(req)?;
        let s = negotiate(&device, req)?;
        Ok((s.sample_rate(), s.channels()))
    }

    pub fn is_failed(&self) -> bool {
        self.status.failed.load(Ordering::SeqCst)
    }
}

fn build<T, S>(device: &cpal::Device, config: &cpal::StreamConfig, mut conv: Converter<S>, err_cb: impl FnMut(cpal::Error) + Send + 'static) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
    S: RenderSource,
{
    device.build_output_stream::<T, _, _>(*config, move |data: &mut [T], info: &cpal::OutputCallbackInfo| conv.fill(data, info), err_cb, None)
}

/// Convert an f32 sample with saturation (used by tests of the edge conversion).
pub fn to_i16_saturating(v: f32) -> i16 {
    i16::from_sample(v.clamp(-1.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_conversion_saturates() {
        assert_eq!(to_i16_saturating(2.0), i16::MAX);
        assert_eq!(to_i16_saturating(-2.0), i16::MIN);
        assert_eq!(to_i16_saturating(0.0), 0);
        assert!((to_i16_saturating(0.5) as i32 - 16384).abs() <= 1);
    }

    fn fake_device() -> Vec<ConfigRange> {
        vec![
            ConfigRange { channels: 1, min_rate_hz: 8000, max_rate_hz: 48_000, sample_format: cpal::SampleFormat::I16 },
            ConfigRange { channels: 2, min_rate_hz: 44_100, max_rate_hz: 96_000, sample_format: cpal::SampleFormat::I16 },
            ConfigRange { channels: 2, min_rate_hz: 44_100, max_rate_hz: 96_000, sample_format: cpal::SampleFormat::F32 },
            ConfigRange { channels: 6, min_rate_hz: 48_000, max_rate_hz: 192_000, sample_format: cpal::SampleFormat::F32 },
        ]
    }

    #[test]
    fn fake_device_negotiation_prefers_stereo_f32_and_reports_nearest() {
        let dev = fake_device();
        let c = choose_config(&dev, 48_000).unwrap();
        assert_eq!((c.channels, c.sample_format), (2, cpal::SampleFormat::F32));
        // 192 kHz only offered on the 6-channel range: pick it rather than fail
        let c = choose_config(&dev, 192_000).unwrap();
        assert_eq!(c.channels, 6);
        // 22.05 kHz only on mono i16
        let c = choose_config(&dev, 22_050).unwrap();
        assert_eq!((c.channels, c.sample_format), (1, cpal::SampleFormat::I16));
        // unsupported rate → explicit error listing what is offered
        let err = choose_config(&dev, 384_000).unwrap_err();
        assert!(err.iter().any(|s| s.contains("2ch 44100-96000 Hz F32")), "{err:?}");
        assert_eq!(offered_project_rates(&dev), vec![44_100, 48_000, 88_200, 96_000, 176_400, 192_000]);
        assert!(choose_config(&[], 48_000).is_err());
        // mono-only device still works (engine downmixes at the edge)
        let mono = vec![ConfigRange { channels: 1, min_rate_hz: 48_000, max_rate_hz: 48_000, sample_format: cpal::SampleFormat::U16 }];
        assert_eq!(choose_config(&mono, 48_000).unwrap().channels, 1);
    }

    #[test]
    fn negotiate_only_reports_no_device_or_a_rate() {
        // Works both on headless machines (NoDevice/Build) and with a device.
        match OutputStream::negotiate_only(&StreamConfigRequest::default()) {
            Ok((rate, ch)) => {
                assert!(rate >= 8000 && ch >= 1);
            }
            Err(StreamError::NoDevice) | Err(StreamError::Build(_)) | Err(StreamError::UnsupportedConfig(..)) => {}
            Err(e) => panic!("unexpected error {e}"),
        }
        assert!(matches!(
            OutputStream::negotiate_only(&StreamConfigRequest { device_id: Some("definitely-not-a-device".into()), ..Default::default() }),
            Err(StreamError::DeviceNotFound(_))
        ));
    }
}

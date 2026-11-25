//! Audio input capture (IO-04): a CPAL input stream converts the selected physical
//! channels to float, stamps fixed-size blocks with the stream frame, and pushes them
//! through preallocated SPSC queues — one for the disk writer, one for input monitoring
//! in the engine. Nothing in the callback allocates, locks or blocks; a full queue drops
//! the block and counts it (the writer inserts silence for the lost range, flagged).

use crate::stream::{ConfigRange, StreamError, choose_config};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SizedSample};
use rawdawg_rt::queue::{ControlQueue, QueueConsumer, QueueProducer};
use rawdawg_rt::shared::Counter;
use rawdawg_rt::{AtomicF32, AtomicF64};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Frames per capture block.
pub const BLOCK_FRAMES: usize = 256;
/// Blocks held by each queue (≈ 2.7 s at 48 kHz): plenty for a writer thread hiccup.
pub const QUEUE_BLOCKS: usize = 512;

/// One stereo block of captured audio with its position on the input stream clock.
#[derive(Clone, Copy)]
pub struct CaptureBlock {
    /// Stream frame of the block's first sample (monotonic on the input clock).
    pub stream_frame: u64,
    pub frames: u16,
    pub left: [f32; BLOCK_FRAMES],
    pub right: [f32; BLOCK_FRAMES],
}

impl Default for CaptureBlock {
    fn default() -> Self {
        Self { stream_frame: 0, frames: 0, left: [0.0; BLOCK_FRAMES], right: [0.0; BLOCK_FRAMES] }
    }
}

/// Which physical input channels feed the (stereo) capture: mono duplicates one channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputChannels {
    Mono(u16),
    Stereo(u16, u16),
}

impl Default for InputChannels {
    fn default() -> Self {
        InputChannels::Stereo(0, 1)
    }
}

#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct InputConfigRequest {
    /// Device id from [`crate::DeviceInfo::id`], or `None` for the default input.
    pub device_id: Option<String>,
    pub sample_rate_hz: Option<u32>,
    pub buffer_frames: Option<u32>,
    pub channels: InputChannels,
}

#[derive(Debug, Default)]
pub struct CaptureStatus {
    pub callbacks: Counter,
    pub frames: AtomicU64,
    /// Blocks dropped because the writer queue was full.
    pub dropped_blocks: Counter,
    /// Blocks dropped because the monitor queue was full (harmless when monitoring is off).
    pub monitor_dropped: Counter,
    pub peak_l: AtomicF32,
    pub peak_r: AtomicF32,
    pub input_latency_s: AtomicF64,
    pub failed: AtomicBool,
    pub errors: Counter,
    pub error: std::sync::Mutex<Option<String>>,
}

pub struct InputStream {
    _stream: cpal::Stream,
    pub status: Arc<CaptureStatus>,
    pub sample_rate_hz: u32,
    pub device_name: String,
    pub device_channels: u16,
}

/// Consumer ends of a capture: the writer queue and the monitor queue.
pub struct CaptureQueues {
    pub writer: QueueConsumer<CaptureBlock>,
    pub monitor: QueueConsumer<CaptureBlock>,
}

/// Producer side of a capture: used by the input callback, and by tests/mock devices.
pub struct CaptureProducer {
    writer: QueueProducer<CaptureBlock>,
    monitor: QueueProducer<CaptureBlock>,
    pub status: Arc<CaptureStatus>,
    pending: CaptureBlock,
    stream_frame: u64,
    channels: usize,
    map: (usize, usize),
}

impl CaptureProducer {
    /// A producer with its queues (no device): tests and the mock capture path.
    pub fn new(device_channels: usize, map: InputChannels) -> (Self, CaptureQueues) {
        let (wp, wc) = ControlQueue::create::<CaptureBlock>(QUEUE_BLOCKS);
        let (mp, mc) = ControlQueue::create::<CaptureBlock>(QUEUE_BLOCKS);
        let map = match map {
            InputChannels::Mono(c) => (usize::from(c), usize::from(c)),
            InputChannels::Stereo(l, r) => (usize::from(l), usize::from(r)),
        };
        let n = device_channels.max(1);
        (
            Self {
                writer: wp,
                monitor: mp,
                status: Arc::new(CaptureStatus::default()),
                pending: CaptureBlock::default(),
                stream_frame: 0,
                channels: n,
                map: (map.0.min(n - 1), map.1.min(n - 1)),
            },
            CaptureQueues { writer: wc, monitor: mc },
        )
    }

    /// Feed interleaved device samples (any count). RT-safe: fixed blocks, no allocation.
    pub fn push_interleaved<T: SizedSample + cpal::Sample>(&mut self, data: &[T])
    where
        f32: FromSample<T>,
    {
        let ch = self.channels;
        let frames = data.len() / ch;
        let (mut pk_l, mut pk_r) = (0.0f32, 0.0f32);
        for f in 0..frames {
            let l = f32::from_sample(data[f * ch + self.map.0]);
            let r = f32::from_sample(data[f * ch + self.map.1]);
            pk_l = pk_l.max(l.abs());
            pk_r = pk_r.max(r.abs());
            let i = usize::from(self.pending.frames);
            if i == 0 {
                self.pending.stream_frame = self.stream_frame;
            }
            self.pending.left[i] = l;
            self.pending.right[i] = r;
            self.pending.frames += 1;
            self.stream_frame += 1;
            if usize::from(self.pending.frames) == BLOCK_FRAMES {
                self.flush_block();
            }
        }
        self.status.frames.fetch_add(frames as u64, Ordering::Relaxed);
        self.status.peak_l.fetch_max(pk_l);
        self.status.peak_r.fetch_max(pk_r);
        self.status.callbacks.incr();
    }

    /// Push the partial block (end of a take or a test).
    pub fn flush_partial(&mut self) {
        if self.pending.frames > 0 {
            self.flush_block();
        }
    }

    fn flush_block(&mut self) {
        let block = self.pending;
        if self.writer.push(block).is_err() {
            self.status.dropped_blocks.incr();
        }
        if self.monitor.push(block).is_err() {
            self.status.monitor_dropped.incr();
        }
        self.pending.frames = 0;
    }

    pub fn stream_frame(&self) -> u64 {
        self.stream_frame
    }
}

fn find_input_device(req: &InputConfigRequest) -> Result<(cpal::Host, cpal::Device), StreamError> {
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
    let dev = host.default_input_device().ok_or(StreamError::NoDevice)?;
    Ok((host, dev))
}

fn negotiate_input(device: &cpal::Device, req: &InputConfigRequest) -> Result<cpal::SupportedStreamConfig, StreamError> {
    let default = device.default_input_config().map_err(|e| StreamError::Build(e.to_string()))?;
    let want_rate = req.sample_rate_hz.unwrap_or(default.sample_rate());
    let ranges: Vec<cpal::SupportedStreamConfigRange> = device.supported_input_configs().map_err(|e| StreamError::Build(e.to_string()))?.collect();
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
        Err(nearest) => Err(StreamError::UnsupportedConfig(format!("{want_rate} Hz input"), nearest.join("; "))),
    }
}

impl InputStream {
    /// Open and start an input stream. Non-RT. The returned queues are the writer's and
    /// the engine's (monitor) ends.
    pub fn open(req: &InputConfigRequest) -> Result<(Self, CaptureQueues), StreamError> {
        let (_host, device) = find_input_device(req)?;
        let supported = negotiate_input(&device, req)?;
        let mut config = supported.config();
        if let Some(b) = req.buffer_frames {
            let ok = match supported.buffer_size() {
                cpal::SupportedBufferSize::Range { min, max } => (*min..=*max).contains(&b),
                cpal::SupportedBufferSize::Unknown => true,
            };
            if ok {
                config.buffer_size = cpal::BufferSize::Fixed(b);
            }
        }
        let channels = usize::from(config.channels);
        let sample_rate = config.sample_rate;
        let (producer, queues) = CaptureProducer::new(channels, req.channels);
        let status = producer.status.clone();
        let err_status = status.clone();
        let err_cb = move |e: cpal::Error| {
            err_status.errors.incr();
            err_status.failed.store(true, Ordering::Relaxed);
            if let Ok(mut m) = err_status.error.lock() {
                *m = Some(e.to_string());
            }
        };
        let device_name = device.description().map(|d| d.name().to_owned()).unwrap_or_else(|_| "input".into());
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => build::<f32>(&device, &config, producer, err_cb),
            cpal::SampleFormat::I16 => build::<i16>(&device, &config, producer, err_cb),
            cpal::SampleFormat::U16 => build::<u16>(&device, &config, producer, err_cb),
            cpal::SampleFormat::I32 => build::<i32>(&device, &config, producer, err_cb),
            other => return Err(StreamError::Build(format!("unsupported input sample format {other:?}"))),
        }
        .map_err(|e| StreamError::Build(e.to_string()))?;
        stream.play().map_err(|e| StreamError::Play(e.to_string()))?;
        Ok((Self { _stream: stream, status, sample_rate_hz: sample_rate, device_name, device_channels: config.channels }, queues))
    }

    pub fn is_failed(&self) -> bool {
        self.status.failed.load(Ordering::Relaxed)
    }
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut producer: CaptureProducer,
    err_cb: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + cpal::Sample + Send + 'static,
    f32: FromSample<T>,
{
    let status = producer.status.clone();
    device.build_input_stream::<T, _, _>(
        *config,
        move |data: &[T], info: &cpal::InputCallbackInfo| {
            let ts = info.timestamp();
            // cpal 0.18: `duration_since` is saturating (capture precedes the callback)
            let d = ts.callback.duration_since(ts.capture);
            status.input_latency_s.store(d.as_secs_f64());
            producer.push_interleaved(data);
        },
        err_cb,
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn producer_blocks_are_stamped_contiguously_and_overflow_is_counted() {
        let (mut p, mut q) = CaptureProducer::new(4, InputChannels::Stereo(2, 3));
        // 3 callbacks of 300 frames × 4 channels: channel 2 carries a ramp, channel 3 its negative
        for c in 0..3 {
            let data: Vec<f32> = (0..300)
                .flat_map(|f| {
                    let v = (c * 300 + f) as f32 / 1000.0;
                    [0.0, 0.0, v, -v]
                })
                .collect();
            p.push_interleaved(&data);
        }
        p.flush_partial();
        let mut blocks = Vec::new();
        while let Some(b) = q.writer.pop() {
            blocks.push(b);
        }
        assert_eq!(blocks.len(), 4, "900 frames = 3 full blocks + 1 partial");
        assert_eq!(blocks.iter().map(|b| u64::from(b.frames)).sum::<u64>(), 900);
        for w in blocks.windows(2) {
            assert_eq!(w[1].stream_frame, w[0].stream_frame + u64::from(w[0].frames));
        }
        assert_eq!(blocks[1].left[0], 256.0 / 1000.0);
        assert_eq!(blocks[1].right[0], -256.0 / 1000.0);
        assert_eq!(p.status.frames.load(Ordering::Relaxed), 900);
        assert!(p.status.peak_l.load() > 0.89);
        // the monitor queue got the same blocks
        assert_eq!(q.monitor.len(), 4);
        // fill the writer queue completely: further blocks are dropped and counted, stamps keep running
        let big: Vec<f32> = vec![0.1; BLOCK_FRAMES * 4 * (QUEUE_BLOCKS + 10)];
        p.push_interleaved(&big);
        assert!(p.status.dropped_blocks.get() >= 10, "{}", p.status.dropped_blocks.get());
        assert_eq!(p.stream_frame(), 900 + (BLOCK_FRAMES * (QUEUE_BLOCKS + 10)) as u64);
    }
}

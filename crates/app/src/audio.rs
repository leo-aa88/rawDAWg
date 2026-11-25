//! Audio device wiring for the app: owns the output stream and the engine handle.

use anyhow::Context;
use rawdawg_audio_engine::engine::{Engine, EngineHandle};
use rawdawg_audio_io::capture::{CaptureBlock, InputConfigRequest, InputStream};
use rawdawg_audio_io::stream::{CallbackInfo, OutputStream, RenderSource, StreamConfigRequest};
use rawdawg_rt::QueueConsumer;
use rawdawg_rt::alloc_guard::RtScope;
use rawdawg_types::limits::EngineLimits;
use rawdawg_types::units::SampleRate;

/// The engine runs inside the CPAL callback through this adapter.
pub struct EngineSource {
    engine: Engine,
}

impl RenderSource for EngineSource {
    fn render(&mut self, left: &mut [f32], right: &mut [f32], _info: &CallbackInfo) {
        let _rt = RtScope::enter();
        self.engine.render_block(left, right);
    }
}

/// A running audio input: its stream and the writer end of its capture queue (taken by a
/// take's disk writer while recording, handed back afterwards).
pub struct InputSystem {
    pub stream: InputStream,
    pub writer_queue: Option<QueueConsumer<CaptureBlock>>,
}

/// A running audio output with its engine handle.
pub struct AudioSystem {
    pub stream: OutputStream,
    pub handle: EngineHandle,
    pub sample_rate: SampleRate,
    /// Human-readable note when the actual configuration differs from the request.
    pub note: Option<String>,
    /// Input device, when enabled and opened.
    pub input: Option<InputSystem>,
    pub input_error: Option<String>,
}

impl AudioSystem {
    /// Drop captured blocks while no take is recording so the queue never sits full.
    pub fn drain_idle_capture(&mut self) {
        if let Some(input) = self.input.as_mut()
            && let Some(q) = input.writer_queue.as_mut()
        {
            while q.pop().is_some() {}
        }
    }
}

impl AudioSystem {
    fn open_once(req: &StreamConfigRequest, input_req: Option<&InputConfigRequest>, limits: &EngineLimits) -> anyhow::Result<Self> {
        let (rate_hz, _ch) = OutputStream::negotiate_only(req).context("negotiating audio format")?;
        let rate = SampleRate::new(rate_hz).context("device reported a zero sample rate")?;
        let max_frames = rawdawg_audio_io::stream::MAX_CHUNK_FRAMES;
        let (mut engine, handle) = Engine::new(rate, max_frames, limits);
        // the input (if any) is opened at the output's rate; failure never blocks the output
        let (input, input_error) = match input_req {
            Some(ir) => {
                let ir = InputConfigRequest { sample_rate_hz: Some(rate_hz), ..ir.clone() };
                match InputStream::open(&ir) {
                    Ok((stream, queues)) => {
                        engine.set_monitor_queue(Some(queues.monitor));
                        (Some(InputSystem { stream, writer_queue: Some(queues.writer) }), None)
                    }
                    Err(e) => (None, Some(format!("input unavailable: {e}"))),
                }
            }
            None => (None, None),
        };
        let stream = OutputStream::open(req, move |_sr| EngineSource { engine }).context("opening audio stream")?;
        Ok(Self { stream, handle, sample_rate: rate, note: None, input, input_error })
    }

    /// Open the requested device and start the engine with an empty plan.
    ///
    /// If the device accepts the configuration but fails as soon as it runs (seen with
    /// fixed period sizes on some ALSA bridges), retry once with the device's default
    /// buffer size and report that in `note`. Stored preferences are never changed here.
    pub fn start(req: &StreamConfigRequest, input_req: Option<&InputConfigRequest>, limits: &EngineLimits) -> anyhow::Result<Self> {
        match Self::open_once(req, input_req, limits) {
            Ok(s) => Ok(s),
            Err(first) if req.buffer_frames.is_some() => {
                let fallback = StreamConfigRequest { buffer_frames: None, ..req.clone() };
                let mut s =
                    Self::open_once(&fallback, input_req, limits).map_err(|second| anyhow::anyhow!("{first:#}; fallback to device default buffer also failed: {second:#}"))?;
                s.note = Some(format!("Buffer of {} frames was refused by the device ({first:#}); using the device default buffer", req.buffer_frames.unwrap_or(0)));
                Ok(s)
            }
            Err(e) => Err(e),
        }
    }
}

/// `--audio-smoke`: play a 1 s A4 tone from the default synth through the real device.
pub fn smoke_test(device: Option<String>, buffer: Option<u32>) -> anyhow::Result<i32> {
    use rawdawg_audio_engine::commands::EngineCommand;
    use rawdawg_types::events::{EventSource, TimedEvent};
    let req = StreamConfigRequest { device_id: device, sample_rate_hz: None, buffer_frames: buffer };
    let limits = EngineLimits::DEFAULT;
    let mut sys = match AudioSystem::start(&req, None, &limits) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("audio smoke: no usable output device: {e:#}");
            return Ok(4);
        }
    };
    println!(
        "opened {} ({:?}) at {} Hz, {} ch, {} ; requested buffer {:?}",
        sys.stream.config.device_name,
        sys.stream.config.backend,
        sys.stream.config.sample_rate_hz,
        sys.stream.config.channels,
        sys.stream.config.sample_format,
        sys.stream.config.requested_buffer_frames
    );
    let mut project = rawdawg_project::Project::new_default("smoke", serde_json::json!({}));
    project.sample_rate_hz = sys.sample_rate;
    let plan =
        rawdawg_audio_engine::plan::compile(&project, 1, rawdawg_audio_io::stream::MAX_CHUNK_FRAMES, &limits, &std::collections::HashMap::new(), &rawdawg_audio_engine::NoAssets)
            .context("compile")?
            .plan;
    let channel = project.channels[0].id;
    sys.handle.publish_plan(plan).map_err(|_| anyhow::anyhow!("plan handoff full"))?;
    sys.handle.send(EngineCommand::Live { channel, event: TimedEvent::note_on(0, EventSource::Preview, 0, 1, 69, 0.8) }).ok();
    std::thread::sleep(std::time::Duration::from_millis(1000));
    sys.handle.send(EngineCommand::Live { channel, event: TimedEvent::note_off(0, EventSource::Preview, 0, 1, 69) }).ok();
    std::thread::sleep(std::time::Duration::from_millis(400));
    let st = &sys.stream.status;
    let est = &sys.handle.status;
    println!(
        "callbacks {} frames {} xruns {} max callback {:.3} ms, engine blocks {} peak L {:.3} R {:.3} nonfinite {} failed {}",
        st.callbacks.get(),
        st.frames.load(std::sync::atomic::Ordering::Relaxed),
        st.xruns.get(),
        st.max_callback_ns.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e6,
        est.blocks.get(),
        est.master_peak_l.load(),
        est.master_peak_r.load(),
        est.nonfinite_blocks.get(),
        sys.stream.is_failed()
    );
    if let Some(e) = st.error_message() {
        eprintln!("stream error: {e}");
        return Ok(5);
    }
    if st.callbacks.get() == 0 {
        eprintln!("the device never called back; audio is not flowing");
        return Ok(5);
    }
    if est.master_peak_l.load() <= 0.0 {
        eprintln!("engine produced silence; the tone did not render");
        return Ok(5);
    }
    Ok(0)
}

/// `--capture-smoke`: open the default (or chosen) input for one second and report what
/// arrived: callbacks, frames, peaks, dropped blocks, reported latency. Hardware evidence
/// for HW-01 (recording); exits non-zero when no input opens.
pub fn capture_smoke(device: Option<String>, channels: rawdawg_audio_io::capture::InputChannels) -> anyhow::Result<i32> {
    let req = InputConfigRequest { device_id: device, sample_rate_hz: None, buffer_frames: None, channels };
    let (stream, mut queues) = match InputStream::open(&req) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("capture smoke: no input stream: {e}");
            return Ok(2);
        }
    };
    let start = std::time::Instant::now();
    let mut blocks = 0u64;
    while start.elapsed() < std::time::Duration::from_secs(1) {
        while queues.writer.pop().is_some() {
            blocks += 1;
        }
        while queues.monitor.pop().is_some() {}
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let st = &stream.status;
    println!(
        "capture smoke: {} ({} ch) at {} Hz: {} callbacks, {} frames, {} blocks received, {} dropped, peak L {:.3} R {:.3}, reported input latency {:.1} ms, failed: {}",
        stream.device_name,
        stream.device_channels,
        stream.sample_rate_hz,
        st.callbacks.get(),
        st.frames.load(std::sync::atomic::Ordering::Relaxed),
        blocks,
        st.dropped_blocks.get(),
        st.peak_l.load(),
        st.peak_r.load(),
        st.input_latency_s.load() * 1000.0,
        stream.is_failed()
    );
    Ok(if stream.is_failed() || st.frames.load(std::sync::atomic::Ordering::Relaxed) == 0 { 1 } else { 0 })
}

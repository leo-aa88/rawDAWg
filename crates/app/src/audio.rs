//! Audio device wiring for the app: owns the output stream and the engine handle.

use anyhow::Context;
use rawdawg_audio_engine::engine::{Engine, EngineHandle};
use rawdawg_audio_io::stream::{CallbackInfo, OutputStream, RenderSource, StreamConfigRequest};
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

/// A running audio output with its engine handle.
pub struct AudioSystem {
    pub stream: OutputStream,
    pub handle: EngineHandle,
    pub sample_rate: SampleRate,
    /// Human-readable note when the actual configuration differs from the request.
    pub note: Option<String>,
}

impl AudioSystem {
    fn open_once(req: &StreamConfigRequest, limits: &EngineLimits) -> anyhow::Result<Self> {
        let (rate_hz, _ch) = OutputStream::negotiate_only(req).context("negotiating audio format")?;
        let rate = SampleRate::new(rate_hz).context("device reported a zero sample rate")?;
        let max_frames = rawdawg_audio_io::stream::MAX_CHUNK_FRAMES;
        let (engine, handle) = Engine::new(rate, max_frames, limits);
        let stream = OutputStream::open(req, move |_sr| EngineSource { engine }).context("opening audio stream")?;
        Ok(Self { stream, handle, sample_rate: rate, note: None })
    }

    /// Open the requested device and start the engine with an empty plan.
    ///
    /// If the device accepts the configuration but fails as soon as it runs (seen with
    /// fixed period sizes on some ALSA bridges), retry once with the device's default
    /// buffer size and report that in `note`. Stored preferences are never changed here.
    pub fn start(req: &StreamConfigRequest, limits: &EngineLimits) -> anyhow::Result<Self> {
        match Self::open_once(req, limits) {
            Ok(s) => Ok(s),
            Err(first) if req.buffer_frames.is_some() => {
                let fallback = StreamConfigRequest { buffer_frames: None, ..req.clone() };
                let mut s = Self::open_once(&fallback, limits).map_err(|second| anyhow::anyhow!("{first:#}; fallback to device default buffer also failed: {second:#}"))?;
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
    let mut sys = match AudioSystem::start(&req, &limits) {
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
    let plan = rawdawg_audio_engine::plan::compile(&project, 1, rawdawg_audio_io::stream::MAX_CHUNK_FRAMES, &limits, &std::collections::HashMap::new()).context("compile")?.plan;
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

//! VST3 export via the `vst3` (coupler-rs) COM bindings. PLUGIN_API.md §2, PLUG-01.
//!
//! Thin adapter over the format-neutral catalogue in `rawdawg_plugin_api`: every
//! exported plugin (the flagship synth and each effect family) is a pair of
//! classes — a [`Component`] (IComponent + IAudioProcessor, f32 processing with
//! sample-accurate parameter queues, note events, process context, latency and
//! tail) and a [`Controller`] (IEditController over the native descriptors plus
//! the shared `Bypass`/`Mix` wrapper parameters, state hand-off). Class ids are
//! derived from the slots recorded in `rawdawg_plugin_api::exported::VST3_SLOTS`
//! and never change. No DSP lives in this crate.
//!
//! Threading: the host calls `process` on its audio thread and state/activation
//! on the main thread, possibly overlapping. The audio state sits behind a mutex
//! that `process` only `try_lock`s (never blocks): a block that collides with a
//! state save passes the input through instead.
#![allow(non_snake_case, non_upper_case_globals, clippy::missing_safety_doc)]
#![allow(clippy::unnecessary_cast, reason = "The VST3 binding's enum constants are `u32` on Linux and `i32` on MSVC: a cast that one platform needs is redundant on the other")]

use rawdawg_dsp::processor::{PrepareSpec, ProcessContext, Processor, TransportView};
use rawdawg_dsp::{AudioBlock, BusLayout};
use rawdawg_plugin_api::wrap::{EffectWrapper, parse_state, save_envelope, wrapper_descriptors};
use rawdawg_plugin_api::{ExportedPlugin, WRAP_BYPASS, WRAP_MIX};
use rawdawg_types::events::{EventKind, EventSource, NoteExpression, TimedEvent, sort_events};
use rawdawg_types::{ParamDescriptor, ParamId, ParamMapping, ParamUnit};
use std::cell::{Cell, RefCell};
use std::ffi::{CString, c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComRef, ComWrapper, uid};

mod editor;

pub use rawdawg_plugin_api::exported::{URL, VENDOR};
pub use rawdawg_plugin_api::wrap::{WRAP_BYPASS as WRAP_BYPASS_ID, WRAP_MIX as WRAP_MIX_ID};

/// SDK version string reported in class info (the `vst3` bindings track this SDK line).
pub const SDK_VERSION: &str = "VST 3.7.9";
/// Maximum frames per process call we prepare for; larger host blocks are split.
const MAX_FRAMES: usize = 8192;
/// Maximum events accepted per process call (bounded, preallocated).
const MAX_EVENTS: usize = 4096;
/// Hard cap for host-provided state blobs.
const MAX_STATE_BYTES: usize = 16 * 1024 * 1024;

fn catalogue() -> &'static [ExportedPlugin] {
    static ALL: OnceLock<Vec<ExportedPlugin>> = OnceLock::new();
    ALL.get_or_init(rawdawg_plugin_api::exported_plugins)
}

fn tuid(u: rawdawg_plugin_api::exported::Vst3Uid) -> TUID {
    uid(u.0, u.1, u.2, u.3)
}

/// (processor, controller) class ids of an exported plugin.
pub fn class_ids(plugin: &ExportedPlugin) -> (TUID, TUID) {
    let (p, c) = plugin.vst3_class_ids();
    (tuid(p), tuid(c))
}

/// Every exported plugin in factory order.
pub fn exported_plugins() -> &'static [ExportedPlugin] {
    catalogue()
}

fn copy_cstring(src: &str, dst: &mut [c_char]) {
    let c = CString::new(src).unwrap_or_default();
    let bytes = c.as_bytes_with_nul();
    for (s, d) in bytes.iter().zip(dst.iter_mut()) {
        *d = *s as c_char;
    }
    if bytes.len() > dst.len()
        && let Some(last) = dst.last_mut()
    {
        *last = 0;
    }
}

fn copy_wstring(src: &str, dst: &mut [TChar]) {
    let mut len = 0;
    for (s, d) in src.encode_utf16().zip(dst.iter_mut()) {
        *d = s as TChar;
        len += 1;
    }
    if len < dst.len() {
        dst[len] = 0;
    } else if let Some(last) = dst.last_mut() {
        *last = 0;
    }
}

fn read_wstring(src: *const TChar, max: usize) -> String {
    if src.is_null() {
        return String::new();
    }
    let mut len = 0;
    while len < max && unsafe { *src.add(len) } != 0 {
        len += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(src, len) })
}

fn read_stream(stream: &ComRef<IBStream>) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let mut read: int32 = 0;
        let r = unsafe { stream.read(buf.as_mut_ptr() as *mut c_void, buf.len() as int32, &mut read) };
        if r != kResultOk || read <= 0 {
            break;
        }
        out.extend_from_slice(&buf[..read as usize]);
        if out.len() > MAX_STATE_BYTES {
            return None;
        }
        if (read as usize) < buf.len() {
            break;
        }
    }
    Some(out)
}

fn write_stream(stream: &ComRef<IBStream>, bytes: &[u8]) -> bool {
    let mut off = 0usize;
    while off < bytes.len() {
        let chunk = (bytes.len() - off).min(i32::MAX as usize) as int32;
        let mut written: int32 = 0;
        let r = unsafe { stream.write(bytes[off..].as_ptr() as *mut c_void, chunk, &mut written) };
        if r != kResultOk || written <= 0 {
            return false;
        }
        off += written as usize;
    }
    true
}

/// Parameters exposed to the host: visible native descriptors, then the wrapper's.
fn visible_descriptors(plugin: &ExportedPlugin, native: &'static [ParamDescriptor]) -> Vec<&'static ParamDescriptor> {
    native.iter().filter(|d| !d.flags.hidden).chain(wrapper_descriptors(plugin.is_synth(), plugin.has_mix).iter()).collect()
}

fn unit_label(u: ParamUnit) -> &'static str {
    match u {
        ParamUnit::None => "",
        ParamUnit::Percent => "%",
        ParamUnit::Decibel => "dB",
        ParamUnit::Hertz => "Hz",
        ParamUnit::Seconds => "s",
        ParamUnit::Milliseconds => "ms",
        ParamUnit::Semitones => "st",
        ParamUnit::Cents => "ct",
        ParamUnit::Degrees => "°",
        ParamUnit::Ratio => ":1",
        ParamUnit::Bpm => "BPM",
    }
}

/// Audio-thread state of a component (behind a non-blocking mutex).
struct AudioState {
    processor: Box<dyn Processor>,
    wrapper: EffectWrapper,
    events: Vec<TimedEvent>,
    in_l: Vec<f32>,
    in_r: Vec<f32>,
    sc_l: Vec<f32>,
    sc_r: Vec<f32>,
    out_l: Vec<f32>,
    out_r: Vec<f32>,
    sample_rate: f64,
    max_frames: usize,
    prepared: bool,
    /// Pending state from `setState` while inactive (applied on the next prepare).
    steady_frame: i64,
}

/// Audio component + processor for one exported plugin.
pub struct Component {
    plugin: &'static ExportedPlugin,
    audio: Mutex<AudioState>,
    latency: AtomicU32,
    active: AtomicBool,
    processing: AtomicBool,
    /// Mix target as f32 bits (shared with state save while active).
    mix_bits: AtomicU32,
    bypass: AtomicBool,
    /// Blocks skipped because a state operation held the audio state.
    skipped_blocks: AtomicU64,
}

impl Class for Component {
    type Interfaces = (IComponent, IAudioProcessor, IProcessContextRequirements);
}

impl Component {
    pub fn new(plugin: &'static ExportedPlugin) -> Result<Self, String> {
        let mut processor = plugin.make()?;
        let _ = processor.prepare(&PrepareSpec::new(48_000.0, 512, BusLayout::Stereo));
        let latency = processor.latency_frames();
        let state = AudioState {
            processor,
            wrapper: EffectWrapper::new(plugin.has_mix),
            events: Vec::with_capacity(MAX_EVENTS),
            in_l: Vec::new(),
            in_r: Vec::new(),
            sc_l: Vec::new(),
            sc_r: Vec::new(),
            out_l: Vec::new(),
            out_r: Vec::new(),
            sample_rate: 48_000.0,
            max_frames: 512,
            prepared: false,
            steady_frame: 0,
        };
        Ok(Self {
            plugin,
            audio: Mutex::new(state),
            latency: AtomicU32::new(latency),
            active: AtomicBool::new(false),
            processing: AtomicBool::new(false),
            mix_bits: AtomicU32::new(1.0f32.to_bits()),
            bypass: AtomicBool::new(false),
            skipped_blocks: AtomicU64::new(0),
        })
    }

    pub fn skipped_blocks(&self) -> u64 {
        self.skipped_blocks.load(Ordering::Relaxed)
    }

    fn mix(&self) -> f32 {
        f32::from_bits(self.mix_bits.load(Ordering::Relaxed))
    }

    fn audio_bus_count(&self, dir: BusDirection) -> i32 {
        if dir as BusDirections == BusDirections_::kInput { if self.plugin.is_synth() { 0 } else { 1 + i32::from(self.plugin.has_sidechain) } } else { 1 }
    }

    /// Translate the host's events and parameter queues into sorted [`TimedEvent`]s.
    fn collect_events(&self, pd: &ProcessData, st: &mut AudioState, frames: u32) {
        st.events.clear();
        let clamp_t = |t: i32| (t.max(0) as u32).min(frames.saturating_sub(1));
        if let Some(list) = unsafe { ComRef::from_raw(pd.inputEvents) } {
            let n = unsafe { list.getEventCount() };
            for i in 0..n {
                if st.events.len() >= MAX_EVENTS {
                    break;
                }
                let mut e: Event = unsafe { std::mem::zeroed() };
                if unsafe { list.getEvent(i, &mut e) } != kResultOk {
                    continue;
                }
                let t = clamp_t(e.sampleOffset);
                match e.r#type as Event_::EventTypes {
                    Event_::EventTypes_::kNoteOnEvent => {
                        let n = unsafe { e.__field0.noteOn };
                        let pitch = n.pitch.clamp(0, 127) as u8;
                        let note_id = if n.noteId < 0 { u32::from(pitch) | 0x8000_0000 } else { n.noteId as u32 };
                        let velocity = n.velocity.clamp(0.0, 1.0);
                        let ch = n.channel.clamp(0, 15) as u8;
                        if velocity > 0.0 {
                            st.events.push(TimedEvent::note_on(t, EventSource::Host, ch, note_id, pitch, velocity));
                        } else {
                            st.events.push(TimedEvent::note_off(t, EventSource::Host, ch, note_id, pitch));
                        }
                    }
                    Event_::EventTypes_::kNoteOffEvent => {
                        let n = unsafe { e.__field0.noteOff };
                        let pitch = n.pitch.clamp(0, 127) as u8;
                        let note_id = if n.noteId < 0 { u32::from(pitch) | 0x8000_0000 } else { n.noteId as u32 };
                        st.events.push(TimedEvent::note_off(t, EventSource::Host, n.channel.clamp(0, 15) as u8, note_id, pitch));
                    }
                    Event_::EventTypes_::kPolyPressureEvent => {
                        let n = unsafe { e.__field0.polyPressure };
                        let pitch = n.pitch.clamp(0, 127) as u8;
                        let note_id = if n.noteId < 0 { u32::from(pitch) | 0x8000_0000 } else { n.noteId as u32 };
                        st.events.push(TimedEvent::new(t, EventSource::Host, n.channel.clamp(0, 15) as u8, EventKind::NotePressure { note_id, value: n.pressure.clamp(0.0, 1.0) }));
                    }
                    Event_::EventTypes_::kNoteExpressionValueEvent => {
                        let x = unsafe { e.__field0.noteExpressionValue };
                        // VST3 addresses note expressions by note id only
                        if x.noteId >= 0
                            && let Some((expression, value)) = note_expression_from_vst3(x.typeId, x.value)
                        {
                            st.events.push(TimedEvent::new(t, EventSource::Host, 0, EventKind::NoteExpression { note_id: x.noteId as u32, expression, value }));
                        }
                    }
                    _ => {}
                }
            }
        }
        if let Some(changes) = unsafe { ComRef::from_raw(pd.inputParameterChanges) } {
            let n = unsafe { changes.getParameterCount() };
            for i in 0..n {
                let Some(q) = (unsafe { ComRef::from_raw(changes.getParameterData(i)) }) else { continue };
                let id = unsafe { q.getParameterId() };
                let points = unsafe { q.getPointCount() };
                for p in 0..points {
                    if st.events.len() >= MAX_EVENTS {
                        break;
                    }
                    let mut off = 0;
                    let mut v = 0.0;
                    if unsafe { q.getPoint(p, &mut off, &mut v) } == kResultTrue && v.is_finite() {
                        st.events.push(TimedEvent::new(clamp_t(off), EventSource::Host, 0, EventKind::Param { id: ParamId(id), value: (v as f32).clamp(0.0, 1.0) }));
                    }
                }
            }
        }
        sort_events(&mut st.events);
    }

    fn transport(&self, pd: &ProcessData, st: &AudioState) -> TransportView {
        let mut t = TransportView::stopped(st.sample_rate);
        t.block_start_frame = st.steady_frame;
        if let Some(ctx) = unsafe { pd.processContext.as_ref() } {
            let s = ctx.state;
            // the binding's enum constants are `i32` on MSVC and `u32` elsewhere
            use ProcessContext_::StatesAndFlags_ as F;
            t.playing = s & F::kPlaying as u32 != 0;
            if s & F::kTempoValid as u32 != 0 && ctx.tempo.is_finite() && ctx.tempo > 0.0 {
                t.tempo_bpm = ctx.tempo.clamp(1.0, 999.0);
            }
            if s & F::kProjectTimeMusicValid as u32 != 0 && ctx.projectTimeMusic.is_finite() {
                t.position_quarters = ctx.projectTimeMusic.max(0.0);
            }
            if s & F::kTimeSigValid as u32 != 0 && ctx.timeSigNumerator > 0 && ctx.timeSigDenominator > 0 {
                t.meter_numerator = ctx.timeSigNumerator.min(32) as u8;
                t.meter_denominator = ctx.timeSigDenominator.min(32) as u8;
            }
            if ctx.projectTimeSamples >= 0 {
                t.block_start_frame = ctx.projectTimeSamples;
            }
        }
        t
    }
}

impl IPluginBaseTrait for Component {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }
    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IComponentTrait for Component {
    unsafe fn getControllerClassId(&self, class_id: *mut TUID) -> tresult {
        unsafe { *class_id = class_ids(self.plugin).1 };
        kResultOk
    }
    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }
    unsafe fn getBusCount(&self, media_type: MediaType, dir: BusDirection) -> i32 {
        match media_type as MediaTypes {
            MediaTypes_::kAudio => self.audio_bus_count(dir),
            MediaTypes_::kEvent if self.plugin.is_synth() && dir as BusDirections == BusDirections_::kInput => 1,
            _ => 0,
        }
    }
    unsafe fn getBusInfo(&self, media_type: MediaType, dir: BusDirection, index: i32, bus: *mut BusInfo) -> tresult {
        if index < 0 || index >= unsafe { self.getBusCount(media_type, dir) } {
            return kInvalidArgument;
        }
        let bus = unsafe { &mut *bus };
        bus.mediaType = media_type;
        bus.direction = dir;
        bus.flags = BusInfo_::BusFlags_::kDefaultActive as uint32;
        if media_type as MediaTypes == MediaTypes_::kEvent {
            bus.channelCount = 16;
            copy_wstring("Notes", &mut bus.name);
            bus.busType = BusTypes_::kMain as BusType;
            return kResultOk;
        }
        bus.channelCount = 2;
        let is_input = dir as BusDirections == BusDirections_::kInput;
        if is_input && index == 1 {
            copy_wstring("Sidechain", &mut bus.name);
            bus.busType = BusTypes_::kAux as BusType;
            bus.flags = 0;
        } else {
            copy_wstring(if is_input { "Input" } else { "Output" }, &mut bus.name);
            bus.busType = BusTypes_::kMain as BusType;
        }
        kResultOk
    }
    unsafe fn getRoutingInfo(&self, _i: *mut RoutingInfo, _o: *mut RoutingInfo) -> tresult {
        kNotImplemented
    }
    unsafe fn activateBus(&self, _m: MediaType, _d: BusDirection, _i: i32, _s: TBool) -> tresult {
        kResultOk
    }
    unsafe fn setActive(&self, state: TBool) -> tresult {
        let on = state != 0;
        if on {
            let Ok(mut guard) = self.audio.lock() else { return kResultFalse };
            let st = &mut *guard;
            let spec = PrepareSpec::new(st.sample_rate, st.max_frames, BusLayout::Stereo);
            if st.processor.prepare(&spec).is_err() {
                return kResultFalse;
            }
            let latency = st.processor.latency_frames();
            self.latency.store(latency, Ordering::Relaxed);
            let n = st.max_frames;
            for b in [&mut st.in_l, &mut st.in_r, &mut st.sc_l, &mut st.sc_r, &mut st.out_l, &mut st.out_r] {
                b.clear();
                b.resize(n, 0.0);
            }
            if !self.plugin.is_synth() {
                let (mix, sr) = (self.mix(), st.sample_rate);
                st.wrapper.prepare(latency, sr, mix);
                st.wrapper.bypass = self.bypass.load(Ordering::Relaxed);
            }
            st.prepared = true;
        } else if let Ok(mut st) = self.audio.lock() {
            st.processor.reset();
            st.prepared = false;
        }
        self.active.store(on, Ordering::Relaxed);
        kResultOk
    }
    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        let Some(stream) = (unsafe { ComRef::from_raw(state) }) else { return kInvalidArgument };
        let Some(bytes) = read_stream(&stream) else { return kResultFalse };
        let parsed = if self.plugin.is_synth() {
            rawdawg_plugin_api::wrap::ParsedState { processor: bytes, bypass: None, mix: None }
        } else {
            match parse_state(self.plugin.native_type_id, &bytes) {
                Ok(p) => p,
                Err(_) => return kResultFalse,
            }
        };
        // State may arrive while processing: the lock is held briefly; a colliding
        // block passes through (see `process`).
        let Ok(mut st) = self.audio.lock() else { return kResultFalse };
        if st.processor.load_state(&parsed.processor).is_err() {
            return kResultFalse;
        }
        if let Some(b) = parsed.bypass {
            self.bypass.store(b, Ordering::Relaxed);
            st.wrapper.bypass = b;
        }
        if let Some(m) = parsed.mix {
            self.mix_bits.store(m.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
            st.wrapper.set_mix(m);
        }
        kResultOk
    }
    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        let Some(stream) = (unsafe { ComRef::from_raw(state) }) else { return kInvalidArgument };
        let bytes = {
            let Ok(st) = self.audio.lock() else { return kResultFalse };
            let raw = st.processor.save_state();
            if self.plugin.is_synth() {
                raw
            } else {
                match save_envelope(self.plugin.native_type_id, &raw, self.bypass.load(Ordering::Relaxed), self.mix()) {
                    Ok(b) => b,
                    Err(_) => return kResultFalse,
                }
            }
        };
        if write_stream(&stream, &bytes) { kResultOk } else { kResultFalse }
    }
}

impl IAudioProcessorTrait for Component {
    unsafe fn setBusArrangements(&self, inputs: *mut SpeakerArrangement, num_ins: i32, outputs: *mut SpeakerArrangement, num_outs: i32) -> tresult {
        let want_ins = self.audio_bus_count(BusDirections_::kInput as BusDirection);
        if num_ins != want_ins || num_outs != 1 {
            return kResultFalse;
        }
        for i in 0..num_ins as usize {
            if unsafe { *inputs.add(i) } != SpeakerArr::kStereo {
                return kResultFalse;
            }
        }
        if unsafe { *outputs } != SpeakerArr::kStereo {
            return kResultFalse;
        }
        kResultTrue
    }
    unsafe fn getBusArrangement(&self, dir: BusDirection, index: i32, arr: *mut SpeakerArrangement) -> tresult {
        if index < 0 || index >= self.audio_bus_count(dir) {
            return kInvalidArgument;
        }
        unsafe { *arr = SpeakerArr::kStereo };
        kResultOk
    }
    unsafe fn canProcessSampleSize(&self, symbolic_sample_size: i32) -> tresult {
        if symbolic_sample_size as SymbolicSampleSizes == SymbolicSampleSizes_::kSample32 { kResultOk } else { kNotImplemented }
    }
    unsafe fn getLatencySamples(&self) -> u32 {
        self.latency.load(Ordering::Relaxed)
    }
    unsafe fn setupProcessing(&self, setup: *mut ProcessSetup) -> tresult {
        let setup = unsafe { &*setup };
        if setup.symbolicSampleSize as SymbolicSampleSizes != SymbolicSampleSizes_::kSample32 {
            return kResultFalse;
        }
        let spec = PrepareSpec::new(setup.sampleRate, (setup.maxSamplesPerBlock.max(1) as usize).min(MAX_FRAMES), BusLayout::Stereo);
        if !spec.is_valid() {
            return kResultFalse;
        }
        let Ok(mut st) = self.audio.lock() else { return kResultFalse };
        st.sample_rate = spec.sample_rate;
        st.max_frames = spec.max_frames;
        st.prepared = false;
        kResultOk
    }
    unsafe fn setProcessing(&self, state: TBool) -> tresult {
        self.processing.store(state != 0, Ordering::Relaxed);
        if state == 0
            && let Ok(mut st) = self.audio.lock()
        {
            st.processor.reset();
            let mix = self.mix();
            st.wrapper.reset(mix);
        }
        kResultOk
    }
    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        let pd = unsafe { &*data };
        if pd.numSamples <= 0 {
            return kResultOk;
        }
        let total = pd.numSamples as usize;
        let outputs = if pd.numOutputs >= 1 && !pd.outputs.is_null() { Some(unsafe { &*pd.outputs }) } else { None };
        let main_in = if pd.numInputs >= 1 && !pd.inputs.is_null() { Some(unsafe { &*pd.inputs }) } else { None };
        let sc_in = if self.plugin.has_sidechain && pd.numInputs >= 2 && !pd.inputs.is_null() { Some(unsafe { &*pd.inputs.add(1) }) } else { None };
        let chan = |bus: &AudioBusBuffers, c: usize| -> Option<*mut f32> {
            if (c as i32) < bus.numChannels {
                let p = unsafe { *bus.__field0.channelBuffers32.add(c) };
                if p.is_null() { None } else { Some(p) }
            } else {
                None
            }
        };
        let (Some(out_bus), Some(o0)) = (outputs, outputs.and_then(|b| chan(b, 0))) else { return kResultOk };
        let o1 = chan(out_bus, 1);
        let in0 = main_in.and_then(|b| chan(b, 0));
        let in1 = main_in.and_then(|b| chan(b, 1)).or(in0);
        // Never block the audio thread: a colliding main-thread state call wins and this
        // block passes the input through (counted).
        let Ok(mut st) = self.audio.try_lock() else {
            self.skipped_blocks.fetch_add(1, Ordering::Relaxed);
            let ol = unsafe { std::slice::from_raw_parts_mut(o0, total) };
            match in0 {
                Some(i) if !std::ptr::eq(i, o0) => ol.copy_from_slice(unsafe { std::slice::from_raw_parts(i, total) }),
                Some(_) => {}
                None => ol.fill(0.0),
            }
            if let Some(o1) = o1 {
                let or = unsafe { std::slice::from_raw_parts_mut(o1, total) };
                match in1 {
                    Some(i) if !std::ptr::eq(i, o1) => or.copy_from_slice(unsafe { std::slice::from_raw_parts(i, total) }),
                    Some(_) => {}
                    None => or.fill(0.0),
                }
            }
            return kResultOk;
        };
        let st = &mut *st;
        if !st.prepared {
            // the host skipped setActive(true): prepare lazily (allocates once, off the hot path)
            let spec = PrepareSpec::new(st.sample_rate, st.max_frames, BusLayout::Stereo);
            if st.processor.prepare(&spec).is_err() {
                return kResultFalse;
            }
            let latency = st.processor.latency_frames();
            self.latency.store(latency, Ordering::Relaxed);
            let n = st.max_frames;
            for b in [&mut st.in_l, &mut st.in_r, &mut st.sc_l, &mut st.sc_r, &mut st.out_l, &mut st.out_r] {
                b.clear();
                b.resize(n, 0.0);
            }
            if !self.plugin.is_synth() {
                let (mix, sr) = (self.mix(), st.sample_rate);
                st.wrapper.prepare(latency, sr, mix);
                st.wrapper.bypass = self.bypass.load(Ordering::Relaxed);
            }
            st.prepared = true;
        }
        self.collect_events(pd, st, total as u32);
        if !self.plugin.is_synth() {
            st.wrapper.take_wrapper_events(&mut st.events);
            // keep the main-thread copies in sync for state saves
            self.bypass.store(st.wrapper.bypass, Ordering::Relaxed);
            self.mix_bits.store(st.wrapper.mix_target().to_bits(), Ordering::Relaxed);
        }
        let transport = self.transport(pd, st);
        let mut done = 0usize;
        let mut ev_cursor = 0usize;
        while done < total {
            let n = (total - done).min(st.max_frames);
            let end = done + n;
            let start_idx = ev_cursor;
            while ev_cursor < st.events.len() && (st.events[ev_cursor].frame as usize) < end {
                ev_cursor += 1;
            }
            let slice_end = ev_cursor;
            for e in &mut st.events[start_idx..slice_end] {
                e.frame -= done as u32;
            }
            let chunk_events = std::mem::take(&mut st.events);
            let chunk = &chunk_events[start_idx..slice_end];
            let AudioState { processor, wrapper, in_l, in_r, sc_l, sc_r, out_l, out_r, .. } = st;
            if self.plugin.is_synth() {
                {
                    let mut outs: [&mut [f32]; 2] = [&mut out_l[..n], &mut out_r[..n]];
                    let mut ctx = ProcessContext { audio: AudioBlock::output_only(n, &mut outs), events: chunk, transport: &transport };
                    processor.process(&mut ctx);
                }
            } else {
                match in0 {
                    Some(i) => in_l[..n].copy_from_slice(unsafe { std::slice::from_raw_parts(i.add(done), n) }),
                    None => in_l[..n].fill(0.0),
                }
                match in1 {
                    Some(i) => in_r[..n].copy_from_slice(unsafe { std::slice::from_raw_parts(i.add(done), n) }),
                    None => in_r[..n].fill(0.0),
                }
                let sidechain_fed = match sc_in {
                    Some(bus) => match (chan(bus, 0), chan(bus, 1)) {
                        (Some(a), b) => {
                            sc_l[..n].copy_from_slice(unsafe { std::slice::from_raw_parts(a.add(done), n) });
                            sc_r[..n].copy_from_slice(unsafe { std::slice::from_raw_parts(b.unwrap_or(a).add(done), n) });
                            true
                        }
                        _ => false,
                    },
                    None => false,
                };
                let sidechain = sidechain_fed.then_some([&sc_l[..n], &sc_r[..n]]);
                wrapper.process_chunk(processor.as_mut(), n, chunk, &transport, [&in_l[..n], &in_r[..n]], sidechain, [&mut out_l[..n], &mut out_r[..n]]);
            }
            // scratch → host buffers (mono hosts get a downmix); non-finite output is muted
            let bad = out_l[..n].iter().chain(out_r[..n].iter()).any(|v| !v.is_finite());
            let ol = unsafe { std::slice::from_raw_parts_mut(o0.add(done), n) };
            match o1 {
                Some(o1) => {
                    let or = unsafe { std::slice::from_raw_parts_mut(o1.add(done), n) };
                    if bad {
                        ol.fill(0.0);
                        or.fill(0.0);
                    } else {
                        ol.copy_from_slice(&out_l[..n]);
                        or.copy_from_slice(&out_r[..n]);
                    }
                }
                None => {
                    for i in 0..n {
                        ol[i] = if bad { 0.0 } else { 0.5 * (out_l[i] + out_r[i]) };
                    }
                }
            }
            st.events = chunk_events;
            done = end;
        }
        // extra output buses (none declared) stay untouched; silence flags not claimed
        st.steady_frame = st.steady_frame.wrapping_add(total as i64);
        kResultOk
    }
    unsafe fn getTailSamples(&self) -> u32 {
        if self.plugin.is_synth() {
            return 0;
        }
        let Ok(st) = self.audio.try_lock() else { return kInfiniteTail };
        match st.wrapper.tail_frames(st.processor.as_ref()) {
            Some(n) => n,
            None => kInfiniteTail,
        }
    }
}

impl IProcessContextRequirementsTrait for Component {
    unsafe fn getProcessContextRequirements(&self) -> u32 {
        use IProcessContextRequirements_::Flags_ as F;
        // `i32` on MSVC, `u32` elsewhere
        (F::kNeedTempo | F::kNeedTransportState | F::kNeedProjectTimeMusic | F::kNeedTimeSignature) as u32
    }
}

/// The controller's state, shared with its editor view (UI thread only).
pub struct ControllerState {
    plugin: &'static ExportedPlugin,
    /// Main-thread processor copy holding the controller's parameter values.
    mirror: RefCell<Box<dyn Processor>>,
    visible: Vec<&'static ParamDescriptor>,
    bypass: Cell<bool>,
    mix: Cell<f32>,
    /// The host's component handler (edits from the editor go through it).
    handler: RefCell<Option<vst3::ComPtr<IComponentHandler>>>,
}

/// Edit controller for one exported plugin: parameter info and values from the native
/// descriptors (plus the wrapper's), formatting through the descriptors, and the editor.
pub struct Controller {
    state: std::rc::Rc<ControllerState>,
}

impl std::ops::Deref for Controller {
    type Target = ControllerState;
    fn deref(&self) -> &ControllerState {
        &self.state
    }
}

impl Class for Controller {
    type Interfaces = (IEditController, INoteExpressionController);
}

/// The note expressions the synth understands: (VST3 type id, title, short title, units,
/// bipolar, default normalized value).
const NOTE_EXPRESSIONS: [(u32, &str, &str, &str, bool, f64); 6] = [
    (NoteExpressionTypeIDs_::kVolumeTypeID, "Volume", "Vol", "dB", false, 0.25),
    (NoteExpressionTypeIDs_::kPanTypeID, "Pan", "Pan", "", true, 0.5),
    (NoteExpressionTypeIDs_::kTuningTypeID, "Tuning", "Tune", "semitones", true, 0.5),
    (NoteExpressionTypeIDs_::kVibratoTypeID, "Vibrato", "Vib", "", false, 0.0),
    (NoteExpressionTypeIDs_::kExpressionTypeID, "Expression", "Expr", "", false, 1.0),
    (NoteExpressionTypeIDs_::kBrightnessTypeID, "Brightness", "Bright", "", true, 0.5),
];

/// A VST3 note expression value (normalized) as rawDAWg's expression in CLAP ranges: volume
/// `4·v` (0.25 = 0 dB), tuning `240·(v − 0.5)` semitones, the others unchanged.
pub fn note_expression_from_vst3(type_id: u32, normalized: f64) -> Option<(NoteExpression, f32)> {
    if !normalized.is_finite() {
        return None;
    }
    let v = normalized.clamp(0.0, 1.0);
    let (expression, value) = match type_id {
        NoteExpressionTypeIDs_::kVolumeTypeID => (NoteExpression::Volume, v * 4.0),
        NoteExpressionTypeIDs_::kPanTypeID => (NoteExpression::Pan, v),
        NoteExpressionTypeIDs_::kTuningTypeID => (NoteExpression::Tuning, (v - 0.5) * 240.0),
        NoteExpressionTypeIDs_::kVibratoTypeID => (NoteExpression::Vibrato, v),
        NoteExpressionTypeIDs_::kExpressionTypeID => (NoteExpression::Expression, v),
        NoteExpressionTypeIDs_::kBrightnessTypeID => (NoteExpression::Brightness, v),
        _ => return None,
    };
    // computed in f64 so values survive the host's normalization exactly
    Some((expression, value as f32))
}

/// Readout of a note expression value, as hosts show it.
fn note_expression_text(type_id: u32, normalized: f64) -> Option<String> {
    let (expression, v) = note_expression_from_vst3(type_id, normalized)?;
    Some(match expression {
        NoteExpression::Volume if v <= 0.0 => "-inf dB".to_owned(),
        NoteExpression::Volume => format!("{:.1} dB", 20.0 * v.log10()),
        NoteExpression::Pan if (v - 0.5).abs() < 0.005 => "C".to_owned(),
        NoteExpression::Pan if v < 0.5 => format!("L{:.0}", (0.5 - v) * 200.0),
        NoteExpression::Pan => format!("R{:.0}", (v - 0.5) * 200.0),
        NoteExpression::Tuning => format!("{v:+.2} st"),
        _ => format!("{:.0} %", v * 100.0),
    })
}

impl INoteExpressionControllerTrait for Controller {
    unsafe fn getNoteExpressionCount(&self, bus_index: i32, _channel: i16) -> i32 {
        if self.plugin.is_synth() && bus_index == 0 { NOTE_EXPRESSIONS.len() as i32 } else { 0 }
    }
    unsafe fn getNoteExpressionInfo(&self, bus_index: i32, _channel: i16, index: i32, info: *mut NoteExpressionTypeInfo) -> tresult {
        if !self.plugin.is_synth() || bus_index != 0 || info.is_null() {
            return kInvalidArgument;
        }
        let Some(&(type_id, title, short, units, bipolar, default)) = usize::try_from(index).ok().and_then(|i| NOTE_EXPRESSIONS.get(i)) else {
            return kInvalidArgument;
        };
        // SAFETY: the host passes a valid, writable NoteExpressionTypeInfo (checked non-null).
        let info = unsafe { &mut *info };
        info.typeId = type_id;
        copy_wstring(title, &mut info.title);
        copy_wstring(short, &mut info.shortTitle);
        copy_wstring(units, &mut info.units);
        info.unitId = -1;
        info.valueDesc = NoteExpressionValueDescription { defaultValue: default, minimum: 0.0, maximum: 1.0, stepCount: 0 };
        info.associatedParameterId = u32::MAX;
        let flags = NoteExpressionTypeInfo_::NoteExpressionTypeFlags_::kIsAbsolute | if bipolar { NoteExpressionTypeInfo_::NoteExpressionTypeFlags_::kIsBipolar } else { 0 };
        // the flag enum's integer type differs between the SDK's Windows and Linux headers
        info.flags = flags as i32;
        kResultOk
    }
    unsafe fn getNoteExpressionStringByValue(&self, _bus_index: i32, _channel: i16, id: u32, value: f64, string: *mut String128) -> tresult {
        let Some(text) = note_expression_text(id, value) else { return kInvalidArgument };
        if string.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: non-null, and the host passes a writable String128.
        copy_wstring(&text, unsafe { &mut *string });
        kResultOk
    }
    unsafe fn getNoteExpressionValueByString(&self, _bus_index: i32, _channel: i16, id: u32, string: *const TChar, value: *mut f64) -> tresult {
        if string.is_null() || value.is_null() || note_expression_from_vst3(id, 0.5).is_none() {
            return kInvalidArgument;
        }
        let Some(normalized) = note_expression_parse(id, &read_wstring(string, 128)) else { return kResultFalse };
        // SAFETY: checked non-null; the host passes a writable f64.
        unsafe { *value = normalized };
        kResultOk
    }
}

/// Parse a note expression readout ([`note_expression_text`]) back to its normalized value.
fn note_expression_parse(type_id: u32, text: &str) -> Option<f64> {
    let t = text.trim();
    let number = |t: &str| t.trim_end_matches(|c: char| c.is_alphabetic() || c == '%' || c == ' ').trim().parse::<f64>().ok().filter(|n| n.is_finite());
    let normalized = match type_id {
        NoteExpressionTypeIDs_::kVolumeTypeID if t.starts_with("-inf") => 0.0,
        NoteExpressionTypeIDs_::kVolumeTypeID => 10f64.powf(number(t)? / 20.0) / 4.0,
        NoteExpressionTypeIDs_::kTuningTypeID => number(t)? / 240.0 + 0.5,
        NoteExpressionTypeIDs_::kPanTypeID => match t.chars().next()? {
            'C' | 'c' => 0.5,
            'L' | 'l' => 0.5 - number(&t[1..])? / 200.0,
            'R' | 'r' => 0.5 + number(&t[1..])? / 200.0,
            _ => number(t)? / 200.0 + 0.5,
        },
        _ if note_expression_from_vst3(type_id, 0.5).is_some() => number(t)? / 100.0,
        _ => return None,
    };
    Some(normalized.clamp(0.0, 1.0))
}

impl Controller {
    pub fn new(plugin: &'static ExportedPlugin) -> Result<Self, String> {
        let mirror = plugin.make()?;
        let visible = visible_descriptors(plugin, mirror.descriptors());
        let state = ControllerState { plugin, mirror: RefCell::new(mirror), visible, bypass: Cell::new(false), mix: Cell::new(1.0), handler: RefCell::new(None) };
        Ok(Self { state: std::rc::Rc::new(state) })
    }
}

impl ControllerState {
    fn descriptor(&self, id: u32) -> Option<&'static ParamDescriptor> {
        self.visible.iter().copied().find(|d| d.id.0 == id)
    }
    fn value(&self, id: u32) -> Option<f64> {
        match id {
            WRAP_BYPASS if !self.plugin.is_synth() => Some(if self.bypass.get() { 1.0 } else { 0.0 }),
            WRAP_MIX if self.plugin.has_mix => Some(f64::from(self.mix.get())),
            _ => self.mirror.borrow().param(ParamId(id)).map(f64::from),
        }
    }
}

impl IPluginBaseTrait for Controller {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }
    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IEditControllerTrait for Controller {
    unsafe fn setComponentState(&self, state: *mut IBStream) -> tresult {
        let Some(stream) = (unsafe { ComRef::from_raw(state) }) else { return kInvalidArgument };
        let Some(bytes) = read_stream(&stream) else { return kResultFalse };
        let parsed = if self.plugin.is_synth() {
            rawdawg_plugin_api::wrap::ParsedState { processor: bytes, bypass: None, mix: None }
        } else {
            match parse_state(self.plugin.native_type_id, &bytes) {
                Ok(p) => p,
                Err(_) => return kResultFalse,
            }
        };
        if self.mirror.borrow_mut().load_state(&parsed.processor).is_err() {
            return kResultFalse;
        }
        if let Some(b) = parsed.bypass {
            self.bypass.set(b);
        }
        if let Some(m) = parsed.mix {
            self.mix.set(m.clamp(0.0, 1.0));
        }
        kResultOk
    }
    unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }
    unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }
    unsafe fn getParameterCount(&self) -> i32 {
        self.visible.len() as i32
    }
    unsafe fn getParameterInfo(&self, index: i32, info: *mut ParameterInfo) -> tresult {
        let Some(d) = usize::try_from(index).ok().and_then(|i| self.visible.get(i)) else { return kInvalidArgument };
        let info = unsafe { &mut *info };
        info.id = d.id.0;
        copy_wstring(d.name, &mut info.title);
        copy_wstring(d.name, &mut info.shortTitle);
        copy_wstring(unit_label(d.unit), &mut info.units);
        info.stepCount = match d.mapping {
            ParamMapping::Toggle => 1,
            ParamMapping::Discrete { steps } => steps.saturating_sub(1) as i32,
            // stepped integer ranges (lookahead 1–10 ms): VST3 steps are evenly spaced in the
            // normalized value, exactly like the linear mapping
            ParamMapping::Linear { min, max } if d.flags.stepped && min.fract() == 0.0 && max.fract() == 0.0 && max > min => (max - min) as i32,
            _ => 0,
        };
        info.defaultNormalizedValue = f64::from(d.default_normalized);
        info.unitId = kRootUnitId;
        let mut flags = 0;
        if d.flags.automatable {
            flags |= ParameterInfo_::ParameterFlags_::kCanAutomate;
        }
        if d.flags.is_bypass {
            flags |= ParameterInfo_::ParameterFlags_::kIsBypass;
        }
        if d.flags.read_only {
            flags |= ParameterInfo_::ParameterFlags_::kIsReadOnly;
        }
        if d.labels.is_some() {
            flags |= ParameterInfo_::ParameterFlags_::kIsList;
        }
        info.flags = flags;
        kResultOk
    }
    unsafe fn getParamStringByValue(&self, id: u32, value: f64, string: *mut String128) -> tresult {
        let Some(d) = self.descriptor(id) else { return kInvalidArgument };
        copy_wstring(&d.format(value as f32), unsafe { &mut *string });
        kResultOk
    }
    unsafe fn getParamValueByString(&self, id: u32, string: *mut TChar, value: *mut f64) -> tresult {
        let Some(d) = self.descriptor(id) else { return kInvalidArgument };
        let text = read_wstring(string, 128);
        match d.parse(&text) {
            Some(v) => {
                unsafe { *value = f64::from(v) };
                kResultOk
            }
            None => kResultFalse,
        }
    }
    unsafe fn normalizedParamToPlain(&self, id: u32, v: f64) -> f64 {
        self.descriptor(id).map(|d| d.to_physical(v as f32)).unwrap_or(v)
    }
    unsafe fn plainParamToNormalized(&self, id: u32, v: f64) -> f64 {
        self.descriptor(id).map(|d| f64::from(d.to_normalized(v))).unwrap_or(v)
    }
    unsafe fn getParamNormalized(&self, id: u32) -> f64 {
        self.value(id).unwrap_or(0.0)
    }
    unsafe fn setParamNormalized(&self, id: u32, value: f64) -> tresult {
        self.state.set_normalized(id, value)
    }
    unsafe fn setComponentHandler(&self, handler: *mut IComponentHandler) -> tresult {
        // SAFETY: the host passes a live handler (or null); `to_com_ptr` takes its own reference.
        *self.handler.borrow_mut() = unsafe { ComRef::from_raw(handler) }.map(|h| h.to_com_ptr());
        kResultOk
    }
    unsafe fn createView(&self, name: FIDString) -> *mut IPlugView {
        // SAFETY: a NUL-terminated string from the host.
        if name.is_null() || unsafe { std::ffi::CStr::from_ptr(name) } != c"editor" {
            return std::ptr::null_mut();
        }
        editor::PlugView::create(self.state.clone())
    }
}

impl ControllerState {
    fn set_normalized(&self, id: u32, value: f64) -> tresult {
        if !value.is_finite() {
            return kInvalidArgument;
        }
        let v = value.clamp(0.0, 1.0) as f32;
        match id {
            WRAP_BYPASS if !self.plugin.is_synth() => {
                self.bypass.set(v >= 0.5);
                kResultOk
            }
            WRAP_MIX if self.plugin.has_mix => {
                self.mix.set(v);
                kResultOk
            }
            _ => {
                if self.mirror.borrow_mut().set_param(ParamId(id), v) {
                    kResultOk
                } else {
                    kInvalidArgument
                }
            }
        }
    }
}

pub struct Factory;

impl Class for Factory {
    type Interfaces = (IPluginFactory, IPluginFactory2);
}

impl Factory {
    fn entry(index: i32) -> Option<(&'static ExportedPlugin, bool)> {
        let i = usize::try_from(index).ok()?;
        let plugin = catalogue().get(i / 2)?;
        Some((plugin, i % 2 == 0))
    }
}

impl IPluginFactoryTrait for Factory {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        let info = unsafe { &mut *info };
        copy_cstring(VENDOR, &mut info.vendor);
        copy_cstring(URL, &mut info.url);
        copy_cstring("", &mut info.email);
        info.flags = PFactoryInfo_::FactoryFlags_::kUnicode as int32;
        kResultOk
    }
    unsafe fn countClasses(&self) -> i32 {
        (catalogue().len() * 2) as i32
    }
    unsafe fn getClassInfo(&self, index: i32, info: *mut PClassInfo) -> tresult {
        let Some((plugin, is_processor)) = Self::entry(index) else { return kInvalidArgument };
        let info = unsafe { &mut *info };
        let (pcid, ccid) = class_ids(plugin);
        info.cid = if is_processor { pcid } else { ccid };
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
        copy_cstring(if is_processor { "Audio Module Class" } else { "Component Controller Class" }, &mut info.category);
        copy_cstring(&plugin.name, &mut info.name);
        kResultOk
    }
    unsafe fn createInstance(&self, cid: FIDString, iid: FIDString, obj: *mut *mut c_void) -> tresult {
        if cid.is_null() || iid.is_null() || obj.is_null() {
            return kInvalidArgument;
        }
        let cid = unsafe { *(cid as *const TUID) };
        let mut instance = None;
        for plugin in catalogue() {
            let (pcid, ccid) = class_ids(plugin);
            if cid == pcid {
                instance = Component::new(plugin).ok().and_then(|c| ComWrapper::new(c).to_com_ptr::<FUnknown>());
                break;
            }
            if cid == ccid {
                instance = Controller::new(plugin).ok().and_then(|c| ComWrapper::new(c).to_com_ptr::<FUnknown>());
                break;
            }
        }
        match instance {
            Some(instance) => {
                let ptr = instance.as_ptr();
                unsafe { ((*(*ptr).vtbl).queryInterface)(ptr, iid as *mut TUID, obj) }
            }
            None => kNoInterface,
        }
    }
}

impl IPluginFactory2Trait for Factory {
    unsafe fn getClassInfo2(&self, index: i32, info: *mut PClassInfo2) -> tresult {
        let Some((plugin, is_processor)) = Self::entry(index) else { return kInvalidArgument };
        let info = unsafe { &mut *info };
        let (pcid, ccid) = class_ids(plugin);
        info.cid = if is_processor { pcid } else { ccid };
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
        copy_cstring(if is_processor { "Audio Module Class" } else { "Component Controller Class" }, &mut info.category);
        copy_cstring(&plugin.name, &mut info.name);
        info.classFlags = 0;
        copy_cstring(if is_processor { plugin.vst3_subcategories() } else { "" }, &mut info.subCategories);
        copy_cstring(VENDOR, &mut info.vendor);
        copy_cstring(rawdawg_types::VERSION, &mut info.version);
        copy_cstring(SDK_VERSION, &mut info.sdkVersion);
        kResultOk
    }
}

/// Create the factory COM object (also used by in-process tests).
pub fn create_factory() -> *mut IPluginFactory {
    ComWrapper::new(Factory).to_com_ptr::<IPluginFactory>().expect("factory implements IPluginFactory").into_raw()
}

#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
pub extern "system" fn InitDll() -> bool {
    true
}
#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
pub extern "system" fn ExitDll() -> bool {
    true
}
#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
pub extern "system" fn ModuleEntry(_library_handle: *mut c_void) -> bool {
    true
}
#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
pub extern "system" fn ModuleExit() -> bool {
    true
}
#[unsafe(no_mangle)]
pub extern "system" fn GetPluginFactory() -> *mut IPluginFactory {
    create_factory()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controller(synth: bool) -> Controller {
        let plugin = exported_plugins().iter().find(|p| p.is_synth() == synth).expect("an exported plugin of that kind");
        Controller::new(plugin).expect("controller")
    }

    #[test]
    fn the_synth_lists_its_note_expressions_and_effects_list_none() {
        let synth = controller(true);
        // SAFETY (all calls below): plain Rust calls on our own controller with valid pointers.
        assert_eq!(unsafe { synth.getNoteExpressionCount(0, 3) }, NOTE_EXPRESSIONS.len() as i32);
        assert_eq!(unsafe { synth.getNoteExpressionCount(1, 0) }, 0, "only the event bus");
        assert_eq!(unsafe { controller(false).getNoteExpressionCount(0, 0) }, 0);
        let mut info: NoteExpressionTypeInfo = unsafe { std::mem::zeroed() };
        let mut ids = Vec::new();
        for i in 0..NOTE_EXPRESSIONS.len() as i32 {
            assert_eq!(unsafe { synth.getNoteExpressionInfo(0, 0, i, &mut info) }, kResultOk);
            assert_eq!(info.valueDesc.minimum, 0.0);
            assert_eq!(info.valueDesc.maximum, 1.0);
            let bipolar = info.flags as u32 & NoteExpressionTypeInfo_::NoteExpressionTypeFlags_::kIsBipolar as u32 != 0;
            assert_eq!(bipolar, matches!(info.typeId, NoteExpressionTypeIDs_::kPanTypeID | NoteExpressionTypeIDs_::kTuningTypeID | NoteExpressionTypeIDs_::kBrightnessTypeID));
            assert!(read_wstring(info.title.as_ptr(), 128).len() > 2);
            // the default is neutral: no change to the note
            let (expression, neutral) = note_expression_from_vst3(info.typeId, info.valueDesc.defaultValue).unwrap();
            let expected = match expression {
                NoteExpression::Volume | NoteExpression::Expression => 1.0,
                NoteExpression::Pan | NoteExpression::Brightness => 0.5,
                _ => 0.0,
            };
            assert_eq!(neutral, expected, "{expression:?}");
            ids.push(info.typeId);
        }
        assert_eq!(ids.len(), 6);
        assert_eq!(unsafe { synth.getNoteExpressionInfo(0, 0, 6, &mut info) }, kInvalidArgument);
        assert_eq!(unsafe { synth.getNoteExpressionInfo(0, 0, -1, &mut info) }, kInvalidArgument);
    }

    #[test]
    fn note_expression_values_convert_to_clap_ranges_and_readouts_parse_back() {
        assert_eq!(note_expression_from_vst3(NoteExpressionTypeIDs_::kVolumeTypeID, 0.25), Some((NoteExpression::Volume, 1.0)));
        assert_eq!(note_expression_from_vst3(NoteExpressionTypeIDs_::kTuningTypeID, 0.55), Some((NoteExpression::Tuning, 12.0)));
        assert_eq!(note_expression_from_vst3(NoteExpressionTypeIDs_::kTuningTypeID, 0.0), Some((NoteExpression::Tuning, -120.0)));
        assert_eq!(note_expression_from_vst3(NoteExpressionTypeIDs_::kTextTypeID, 0.5), None, "text and phonemes are not sounds");
        assert_eq!(note_expression_from_vst3(NoteExpressionTypeIDs_::kPanTypeID, f64::NAN), None);
        let synth = controller(true);
        for &(id, ..) in &NOTE_EXPRESSIONS {
            for v in [0.0, 0.1, 0.25, 0.5, 0.8, 1.0] {
                let mut text: String128 = [0; 128];
                assert_eq!(unsafe { synth.getNoteExpressionStringByValue(0, 0, id, v, &mut text) }, kResultOk);
                let shown = read_wstring(text.as_ptr(), 128);
                let mut back = -1.0;
                let r = unsafe { synth.getNoteExpressionValueByString(0, 0, id, text.as_ptr(), &mut back) };
                if id == NoteExpressionTypeIDs_::kVolumeTypeID && v == 0.0 {
                    assert_eq!(shown, "-inf dB");
                }
                assert_eq!(r, kResultOk, "{id} {v} {shown:?}");
                // readouts are rounded for display: tolerance of one display step
                assert!((back - v).abs() < 0.006, "{id}: {v} → {shown:?} → {back}");
            }
        }
    }
}

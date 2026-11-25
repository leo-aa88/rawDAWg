//! T-PLUGIN (VST3 export): load the built VST3 module, enumerate every exported
//! class pair, create components and controllers, process audio and events through
//! the COM ABI (sample-accurate parameter queues, sidechain bus, latency, tail,
//! note events) and round-trip component state into the controller.
#![allow(non_snake_case)]

use rawdawg_dsp::analysis::dominant_frequency;
use rawdawg_dsp::math::{peak, rms};
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComWrapper, Interface};

fn built_module_path() -> std::path::PathBuf {
    let status = std::process::Command::new(env!("CARGO")).args(["build", "-q", "-p", "rawdawg-plugin-vst3"]).status().expect("cargo build");
    assert!(status.success());
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap();
    let name = if cfg!(target_os = "windows") { "rawdawg_vst3.dll" } else { "librawdawg_vst3.so" };
    dir.join(name)
}

/// Minimal in-memory IBStream for state round trips.
struct MemStream {
    data: RefCell<Vec<u8>>,
    pos: Cell<usize>,
}
impl MemStream {
    fn new(data: Vec<u8>) -> ComWrapper<Self> {
        ComWrapper::new(Self { data: RefCell::new(data), pos: Cell::new(0) })
    }
}
impl Class for MemStream {
    type Interfaces = (IBStream,);
}
impl IBStreamTrait for MemStream {
    unsafe fn read(&self, buffer: *mut c_void, num_bytes: int32, num_read: *mut int32) -> tresult {
        let data = self.data.borrow();
        let pos = self.pos.get();
        let n = (num_bytes.max(0) as usize).min(data.len().saturating_sub(pos));
        unsafe {
            std::ptr::copy_nonoverlapping(data[pos..].as_ptr(), buffer as *mut u8, n);
            if !num_read.is_null() {
                *num_read = n as int32;
            }
        }
        self.pos.set(pos + n);
        kResultOk
    }
    unsafe fn write(&self, buffer: *mut c_void, num_bytes: int32, num_written: *mut int32) -> tresult {
        let n = num_bytes.max(0) as usize;
        let slice = unsafe { std::slice::from_raw_parts(buffer as *const u8, n) };
        self.data.borrow_mut().extend_from_slice(slice);
        unsafe {
            if !num_written.is_null() {
                *num_written = n as int32;
            }
        }
        kResultOk
    }
    unsafe fn seek(&self, pos: int64, mode: int32, result: *mut int64) -> tresult {
        let len = self.data.borrow().len() as i64;
        let new = match mode as IBStream_::IStreamSeekMode {
            IBStream_::IStreamSeekMode_::kIBSeekSet => pos,
            IBStream_::IStreamSeekMode_::kIBSeekCur => self.pos.get() as i64 + pos,
            _ => len + pos,
        }
        .clamp(0, len);
        self.pos.set(new as usize);
        unsafe {
            if !result.is_null() {
                *result = new;
            }
        }
        kResultOk
    }
    unsafe fn tell(&self, pos: *mut int64) -> tresult {
        unsafe { *pos = self.pos.get() as int64 };
        kResultOk
    }
}

/// Host-side parameter queue mock.
struct Queue {
    id: u32,
    points: Vec<(i32, f64)>,
}
impl Class for Queue {
    type Interfaces = (IParamValueQueue,);
}
impl IParamValueQueueTrait for Queue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.id
    }
    unsafe fn getPointCount(&self) -> int32 {
        self.points.len() as int32
    }
    unsafe fn getPoint(&self, index: int32, sample_offset: *mut int32, value: *mut ParamValue) -> tresult {
        let Some((o, v)) = self.points.get(index as usize) else { return kResultFalse };
        unsafe {
            *sample_offset = *o;
            *value = *v;
        }
        kResultTrue
    }
    unsafe fn addPoint(&self, _o: int32, _v: ParamValue, _i: *mut int32) -> tresult {
        kNotImplemented
    }
}

struct Changes {
    queues: Vec<ComPtr<IParamValueQueue>>,
}
impl Changes {
    fn new(items: Vec<(u32, Vec<(i32, f64)>)>) -> ComWrapper<Self> {
        ComWrapper::new(Self { queues: items.into_iter().map(|(id, points)| ComWrapper::new(Queue { id, points }).to_com_ptr::<IParamValueQueue>().unwrap()).collect() })
    }
}
impl Class for Changes {
    type Interfaces = (IParameterChanges,);
}
impl IParameterChangesTrait for Changes {
    unsafe fn getParameterCount(&self) -> int32 {
        self.queues.len() as int32
    }
    unsafe fn getParameterData(&self, index: int32) -> *mut IParamValueQueue {
        self.queues.get(index as usize).map(|q| q.as_ptr()).unwrap_or(std::ptr::null_mut())
    }
    unsafe fn addParameterData(&self, _id: *const ParamID, _index: *mut int32) -> *mut IParamValueQueue {
        std::ptr::null_mut()
    }
}

struct Events {
    events: Vec<Event>,
}
impl Class for Events {
    type Interfaces = (IEventList,);
}
impl IEventListTrait for Events {
    unsafe fn getEventCount(&self) -> int32 {
        self.events.len() as int32
    }
    unsafe fn getEvent(&self, index: int32, e: *mut Event) -> tresult {
        let Some(ev) = self.events.get(index as usize) else { return kResultFalse };
        unsafe { *e = *ev };
        kResultOk
    }
    unsafe fn addEvent(&self, _e: *mut Event) -> tresult {
        kNotImplemented
    }
}

fn note_event(offset: i32, pitch: i16, on: bool) -> Event {
    let mut e: Event = unsafe { std::mem::zeroed() };
    e.sampleOffset = offset;
    e.r#type = if on { Event_::EventTypes_::kNoteOnEvent } else { Event_::EventTypes_::kNoteOffEvent } as uint16;
    if on {
        e.__field0.noteOn = NoteOnEvent { channel: 0, pitch, tuning: 0.0, velocity: 0.9, length: 0, noteId: 7 };
    } else {
        e.__field0.noteOff = NoteOffEvent { channel: 0, pitch, velocity: 0.0, noteId: 7, tuning: 0.0 };
    }
    e
}

unsafe fn create<I: Interface>(factory: &ComPtr<IPluginFactory>, cid: &TUID) -> ComPtr<I> {
    let mut obj: *mut c_void = std::ptr::null_mut();
    let iid = I::IID;
    let r = unsafe { factory.createInstance(cid.as_ptr() as FIDString, iid.as_ptr() as FIDString, &mut obj) };
    assert_eq!(r, kResultOk, "createInstance failed");
    unsafe { ComPtr::from_raw(obj as *mut I) }.expect("non-null instance")
}

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

/// Run `blocks` through a processor: `input` on the main bus (both channels), optional
/// sidechain, parameter queues on the first block, note events on the first block.
#[allow(clippy::too_many_arguments)]
unsafe fn run(
    processor: &ComPtr<IAudioProcessor>,
    input: &[f32],
    sidechain: Option<&[f32]>,
    params: Vec<(u32, Vec<(i32, f64)>)>,
    notes: Vec<Event>,
    blocks: usize,
    has_input: bool,
    out_r: &mut Vec<f32>,
) -> Vec<f32> {
    let mut out = Vec::new();
    let changes = Changes::new(params);
    let changes_ptr: ComPtr<IParameterChanges> = changes.to_com_ptr().unwrap();
    let empty = Changes::new(vec![]);
    let empty_ptr: ComPtr<IParameterChanges> = empty.to_com_ptr().unwrap();
    let events = ComWrapper::new(Events { events: notes });
    let events_ptr: ComPtr<IEventList> = events.to_com_ptr().unwrap();
    let no_events = ComWrapper::new(Events { events: vec![] });
    let no_events_ptr: ComPtr<IEventList> = no_events.to_com_ptr().unwrap();
    for b in 0..blocks {
        let mut in_l: Vec<f32> = (0..BLOCK).map(|i| input.get(b * BLOCK + i).copied().unwrap_or(0.0)).collect();
        let mut in_r = in_l.clone();
        let mut sc_l: Vec<f32> = (0..BLOCK).map(|i| sidechain.and_then(|s| s.get(b * BLOCK + i).copied()).unwrap_or(0.0)).collect();
        let mut sc_r = sc_l.clone();
        let mut ol = vec![0.0f32; BLOCK];
        let mut or = vec![0.0f32; BLOCK];
        let mut in_ch = [in_l.as_mut_ptr(), in_r.as_mut_ptr()];
        let mut sc_ch = [sc_l.as_mut_ptr(), sc_r.as_mut_ptr()];
        let mut out_ch = [ol.as_mut_ptr(), or.as_mut_ptr()];
        let mut in_bus: [AudioBusBuffers; 2] = unsafe { std::mem::zeroed() };
        in_bus[0].numChannels = 2;
        in_bus[0].__field0.channelBuffers32 = in_ch.as_mut_ptr();
        in_bus[1].numChannels = 2;
        in_bus[1].__field0.channelBuffers32 = sc_ch.as_mut_ptr();
        let mut out_bus: AudioBusBuffers = unsafe { std::mem::zeroed() };
        out_bus.numChannels = 2;
        out_bus.__field0.channelBuffers32 = out_ch.as_mut_ptr();
        let mut ctx: ProcessContext = unsafe { std::mem::zeroed() };
        ctx.state = ProcessContext_::StatesAndFlags_::kPlaying | ProcessContext_::StatesAndFlags_::kTempoValid | ProcessContext_::StatesAndFlags_::kTimeSigValid;
        ctx.sampleRate = SR;
        ctx.tempo = 120.0;
        ctx.timeSigNumerator = 4;
        ctx.timeSigDenominator = 4;
        ctx.projectTimeSamples = (b * BLOCK) as i64;
        let mut data: ProcessData = unsafe { std::mem::zeroed() };
        data.processMode = ProcessModes_::kRealtime as i32;
        data.symbolicSampleSize = SymbolicSampleSizes_::kSample32 as i32;
        data.numSamples = BLOCK as i32;
        data.numInputs = if has_input { if sidechain.is_some() { 2 } else { 1 } } else { 0 };
        data.numOutputs = 1;
        data.inputs = in_bus.as_mut_ptr();
        data.outputs = &mut out_bus;
        data.inputParameterChanges = if b == 0 { changes_ptr.as_ptr() } else { empty_ptr.as_ptr() };
        data.inputEvents = if b == 0 { events_ptr.as_ptr() } else { no_events_ptr.as_ptr() };
        data.processContext = &mut ctx;
        assert_eq!(unsafe { processor.process(&mut data) }, kResultOk);
        out.extend_from_slice(&ol);
        out_r.extend_from_slice(&or);
    }
    out
}

unsafe fn activate(component: &ComPtr<IComponent>, processor: &ComPtr<IAudioProcessor>, inputs: i32) {
    let mut ins = [SpeakerArr::kStereo, SpeakerArr::kStereo];
    let mut outs = SpeakerArr::kStereo;
    assert_eq!(unsafe { processor.setBusArrangements(ins.as_mut_ptr(), inputs, &mut outs, 1) }, kResultTrue);
    let mut setup =
        ProcessSetup { processMode: ProcessModes_::kRealtime as i32, symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32, maxSamplesPerBlock: BLOCK as i32, sampleRate: SR };
    assert_eq!(unsafe { processor.setupProcessing(&mut setup) }, kResultOk);
    assert_eq!(unsafe { component.setActive(1) }, kResultOk);
    assert_eq!(unsafe { processor.setProcessing(1) }, kResultOk);
}

fn sine(freq: f32, frames: usize, amp: f32) -> Vec<f32> {
    (0..frames).map(|i| amp * (2.0 * std::f32::consts::PI * freq * i as f32 / SR as f32).sin()).collect()
}

fn exercise(factory: ComPtr<IPluginFactory>) {
    let all = rawdawg_vst3::exported_plugins();
    unsafe {
        let mut finfo: PFactoryInfo = std::mem::zeroed();
        assert_eq!(factory.getFactoryInfo(&mut finfo), kResultOk);
        assert_eq!(std::ffi::CStr::from_ptr(finfo.vendor.as_ptr()).to_str().unwrap(), "rawDAWg");
        assert_eq!(factory.countClasses(), (all.len() * 2) as i32);
        assert_eq!(all.len(), 17);
        let factory2: ComPtr<IPluginFactory2> = factory.cast().expect("IPluginFactory2");
        let mut cinfo: PClassInfo = std::mem::zeroed();
        assert_eq!(factory.getClassInfo(factory.countClasses(), &mut cinfo), kInvalidArgument);
        for (i, plugin) in all.iter().enumerate() {
            let (pcid, ccid) = rawdawg_vst3::class_ids(plugin);
            assert_eq!(factory.getClassInfo(2 * i as i32, &mut cinfo), kResultOk);
            assert_eq!(cinfo.cid, pcid, "{}", plugin.name);
            assert_eq!(std::ffi::CStr::from_ptr(cinfo.category.as_ptr()).to_str().unwrap(), "Audio Module Class");
            assert_eq!(std::ffi::CStr::from_ptr(cinfo.name.as_ptr()).to_str().unwrap(), plugin.name);
            assert_eq!(factory.getClassInfo(2 * i as i32 + 1, &mut cinfo), kResultOk);
            assert_eq!(cinfo.cid, ccid);
            assert_eq!(std::ffi::CStr::from_ptr(cinfo.category.as_ptr()).to_str().unwrap(), "Component Controller Class");
            let mut c2: PClassInfo2 = std::mem::zeroed();
            assert_eq!(factory2.getClassInfo2(2 * i as i32, &mut c2), kResultOk);
            assert_eq!(std::ffi::CStr::from_ptr(c2.subCategories.as_ptr()).to_str().unwrap(), plugin.vst3_subcategories());
            assert_eq!(std::ffi::CStr::from_ptr(c2.vendor.as_ptr()).to_str().unwrap(), "rawDAWg");
        }
    }

    // every effect: buses, activation, finite audio from a sine, state round trip into the controller
    let input = sine(220.0, BLOCK * 80, 0.4);
    for plugin in all.iter().filter(|p| !p.is_synth()) {
        let (pcid, ccid) = rawdawg_vst3::class_ids(plugin);
        unsafe {
            let component: ComPtr<IComponent> = create(&factory, &pcid);
            let processor: ComPtr<IAudioProcessor> = component.cast().expect("IAudioProcessor");
            assert_eq!(component.initialize(std::ptr::null_mut()), kResultOk);
            let mut want: TUID = [0; 16];
            assert_eq!(component.getControllerClassId(&mut want), kResultOk);
            assert_eq!(want, ccid);
            let inputs = component.getBusCount(MediaTypes_::kAudio as MediaType, BusDirections_::kInput as BusDirection);
            assert_eq!(inputs, if plugin.has_sidechain { 2 } else { 1 }, "{}", plugin.name);
            assert_eq!(component.getBusCount(MediaTypes_::kAudio as MediaType, BusDirections_::kOutput as BusDirection), 1);
            assert_eq!(component.getBusCount(MediaTypes_::kEvent as MediaType, BusDirections_::kInput as BusDirection), 0);
            if plugin.has_sidechain {
                let mut bus: BusInfo = std::mem::zeroed();
                assert_eq!(component.getBusInfo(MediaTypes_::kAudio as MediaType, BusDirections_::kInput as BusDirection, 1, &mut bus), kResultOk);
                assert_eq!(bus.busType as BusTypes, BusTypes_::kAux);
            }
            let mut mono = [SpeakerArr::kMono, SpeakerArr::kStereo];
            let mut outs = SpeakerArr::kStereo;
            assert_eq!(processor.setBusArrangements(mono.as_mut_ptr(), inputs, &mut outs, 1), kResultFalse, "mono main input refused");
            assert_eq!(processor.canProcessSampleSize(SymbolicSampleSizes_::kSample64 as i32), kNotImplemented);
            activate(&component, &processor, inputs);
            let mut r = Vec::new();
            let out = run(&processor, &input, plugin.has_sidechain.then_some(&[][..]), vec![], vec![], 80, true, &mut r);
            assert!(out.iter().chain(r.iter()).all(|v| v.is_finite()), "{}", plugin.name);
            assert!(rms(&out[BLOCK * 16..]) > 0.001, "{} produces audio", plugin.name);
            let latency = processor.getLatencySamples();
            if plugin.slug == "pitch" {
                assert_eq!(latency, 2048);
            }
            // bypass through a sample-accurate queue: output equals the delayed input
            let mut r = Vec::new();
            let out = run(&processor, &input, plugin.has_sidechain.then_some(&[][..]), vec![(rawdawg_vst3::WRAP_BYPASS_ID, vec![(0, 1.0)])], vec![], 80, true, &mut r);
            let l = latency as usize;
            let err: f32 = (0..BLOCK * 60).map(|i| (out[i + l] - input[i]).abs()).fold(0.0, f32::max);
            assert!(err < 1e-5, "{} bypass is latency-aligned dry (max err {err})", plugin.name);
            assert_eq!(processor.getTailSamples(), 0, "{} bypassed → no tail", plugin.name);
            // state: component → stream → controller; another plugin's envelope is refused
            let stream = MemStream::new(vec![]);
            let stream_ptr: ComPtr<IBStream> = stream.to_com_ptr().unwrap();
            assert_eq!(component.getState(stream_ptr.as_ptr()), kResultOk);
            let json: serde_json::Value = serde_json::from_slice(&stream.data.borrow()).expect("JSON envelope");
            assert_eq!(json["rawdawg_state"], 1);
            assert_eq!(json["type_id"], plugin.native_type_id);
            assert_eq!(json["bypass"], true, "{} state carries the wrapper bypass", plugin.name);
            let controller: ComPtr<IEditController> = create(&factory, &ccid);
            assert_eq!(controller.initialize(std::ptr::null_mut()), kResultOk);
            stream.pos.set(0);
            assert_eq!(controller.setComponentState(stream_ptr.as_ptr()), kResultOk);
            assert_eq!(controller.getParamNormalized(rawdawg_vst3::WRAP_BYPASS_ID), 1.0);
            let count = controller.getParameterCount();
            assert!(count >= 2, "{}", plugin.name);
            let mut saw_bypass = false;
            for i in 0..count {
                let mut pinfo: ParameterInfo = std::mem::zeroed();
                assert_eq!(controller.getParameterInfo(i, &mut pinfo), kResultOk);
                saw_bypass |= pinfo.flags & ParameterInfo_::ParameterFlags_::kIsBypass != 0;
                let v = controller.getParamNormalized(pinfo.id);
                assert!((0.0..=1.0).contains(&v));
                let mut text: String128 = [0; 128];
                assert_eq!(controller.getParamStringByValue(pinfo.id, v, &mut text), kResultOk);
                assert_ne!(text[0], 0, "{} parameter {} formats", plugin.name, pinfo.id);
            }
            assert!(saw_bypass, "{} flags its bypass parameter", plugin.name);
            let mut other = json.clone();
            other["type_id"] = serde_json::Value::String("native.fx.something-else".into());
            let bad = MemStream::new(serde_json::to_vec(&other).unwrap());
            let bad_ptr: ComPtr<IBStream> = bad.to_com_ptr().unwrap();
            assert_eq!(component.setState(bad_ptr.as_ptr()), kResultFalse, "{} refuses another plugin's state", plugin.name);
            let garbage = MemStream::new(vec![1, 2, 3]);
            let garbage_ptr: ComPtr<IBStream> = garbage.to_com_ptr().unwrap();
            assert_eq!(component.setState(garbage_ptr.as_ptr()), kResultFalse);
            assert_eq!(processor.setProcessing(0), kResultOk);
            assert_eq!(component.setActive(0), kResultOk);
            assert_eq!(component.terminate(), kResultOk);
            assert_eq!(controller.terminate(), kResultOk);
        }
    }

    // behaviour: stereo gain through a mid-block parameter point; compressor sidechain ducks; reverb tail
    unsafe {
        let stereo = all.iter().find(|p| p.slug == "stereo").unwrap();
        let gain_id = rawdawg_effects::make_effect("native.fx.stereo", None).unwrap().descriptors().iter().find(|d| d.key == "gain").unwrap().id.0;
        let component: ComPtr<IComponent> = create(&factory, &rawdawg_vst3::class_ids(stereo).0);
        let processor: ComPtr<IAudioProcessor> = component.cast().unwrap();
        component.initialize(std::ptr::null_mut());
        activate(&component, &processor, 1);
        let mut r = Vec::new();
        let out = run(&processor, &input, None, vec![], vec![], 20, true, &mut r);
        let err: f32 = (0..BLOCK * 20).map(|i| (out[i] - input[i]).abs()).fold(0.0, f32::max);
        assert!(err < 1e-5, "stereo tool at default is identity ({err})");
        let mut r = Vec::new();
        // a queue point mid-block: delivered with its offset (native effects smooth per block)
        let out = run(&processor, &input, None, vec![(gain_id, vec![(128, 0.0)])], vec![], 20, true, &mut r);
        assert!(rms(&out[..BLOCK]) > rms(&out[BLOCK * 4..BLOCK * 8]) * 2.0, "the gain ramps down from the block of the point");
        assert!(rms(&out[BLOCK * 4..]) < rms(&input[BLOCK * 4..]) * 0.5, "after the point: attenuated");
        component.setActive(0);

        let comp = all.iter().find(|p| p.slug == "compressor").unwrap();
        let descs = rawdawg_effects::make_effect("native.fx.compressor", None).unwrap().descriptors();
        let id_of = |key: &str| descs.iter().find(|d| d.key == key).unwrap();
        let component: ComPtr<IComponent> = create(&factory, &rawdawg_vst3::class_ids(comp).0);
        let processor: ComPtr<IAudioProcessor> = component.cast().unwrap();
        component.initialize(std::ptr::null_mut());
        activate(&component, &processor, 2);
        let main = sine(220.0, BLOCK * 40, 0.05);
        let mut sc = vec![0.0f32; BLOCK * 40];
        for (i, s) in sc.iter_mut().enumerate().skip(BLOCK * 20) {
            *s = 0.9 * (2.0 * std::f32::consts::PI * 100.0 * i as f32 / SR as f32).sin();
        }
        let params = vec![
            (id_of("sidechain").id.0, vec![(0, 1.0)]),
            (id_of("threshold").id.0, vec![(0, f64::from(id_of("threshold").parse("-40").unwrap()))]),
            (id_of("ratio").id.0, vec![(0, 1.0)]),
        ];
        let mut r = Vec::new();
        let out = run(&processor, &main, Some(&sc), params, vec![], 40, true, &mut r);
        let before = rms(&out[BLOCK * 10..BLOCK * 19]);
        let after = rms(&out[BLOCK * 25..BLOCK * 39]);
        assert!(after < before * 0.7, "sidechain ducks the main signal: before {before} after {after}");
        component.setActive(0);

        let reverb = all.iter().find(|p| p.slug == "reverb").unwrap();
        let component: ComPtr<IComponent> = create(&factory, &rawdawg_vst3::class_ids(reverb).0);
        let processor: ComPtr<IAudioProcessor> = component.cast().unwrap();
        component.initialize(std::ptr::null_mut());
        activate(&component, &processor, 1);
        let mut burst = sine(220.0, BLOCK * 4, 0.5);
        burst.resize(BLOCK * 40, 0.0);
        let mut r = Vec::new();
        let out = run(&processor, &burst, None, vec![], vec![], 40, true, &mut r);
        assert!(rms(&out[BLOCK * 8..BLOCK * 12]) > 1e-4, "reverb rings after the burst");
        assert_ne!(processor.getTailSamples(), 0, "reverb reports a tail");
        component.setActive(0);
    }

    // the synth: event bus, note events → 440 Hz, release, raw patch state
    unsafe {
        let synth = all.iter().find(|p| p.is_synth()).unwrap();
        let (pcid, ccid) = rawdawg_vst3::class_ids(synth);
        let component: ComPtr<IComponent> = create(&factory, &pcid);
        let processor: ComPtr<IAudioProcessor> = component.cast().unwrap();
        assert_eq!(component.initialize(std::ptr::null_mut()), kResultOk);
        assert_eq!(component.getBusCount(MediaTypes_::kAudio as MediaType, BusDirections_::kInput as BusDirection), 0);
        assert_eq!(component.getBusCount(MediaTypes_::kEvent as MediaType, BusDirections_::kInput as BusDirection), 1);
        let mut bus: BusInfo = std::mem::zeroed();
        assert_eq!(component.getBusInfo(MediaTypes_::kEvent as MediaType, BusDirections_::kInput as BusDirection, 0, &mut bus), kResultOk);
        assert_eq!(bus.channelCount, 16);
        activate(&component, &processor, 0);
        let mut r = Vec::new();
        let out = run(&processor, &[], None, vec![], vec![note_event(0, 69, true)], 60, false, &mut r);
        let sounding = &out[4096..BLOCK * 58];
        assert!(rms(sounding) > 0.02, "synth sounds from a note event");
        let f = dominant_frequency(sounding, SR);
        assert!((f - 440.0).abs() < 0.5, "{f}");
        let mut r = Vec::new();
        let out = run(&processor, &[], None, vec![], vec![note_event(0, 69, false)], 60, false, &mut r);
        assert_eq!(peak(&out[BLOCK * 50..]), 0.0, "note released after note-off");
        let stream = MemStream::new(vec![]);
        let stream_ptr: ComPtr<IBStream> = stream.to_com_ptr().unwrap();
        assert_eq!(component.getState(stream_ptr.as_ptr()), kResultOk);
        assert!(stream.data.borrow().starts_with(b"{") && stream.data.borrow().len() > 100, "synth state is the raw patch");
        let controller: ComPtr<IEditController> = create(&factory, &ccid);
        controller.initialize(std::ptr::null_mut());
        stream.pos.set(0);
        assert_eq!(controller.setComponentState(stream_ptr.as_ptr()), kResultOk);
        assert!(controller.getParameterCount() > 20);
        let cutoff = rawdawg_synthesizer::params::FILTER_CUTOFF;
        let mut text: String128 = [0; 128];
        assert_eq!(controller.getParamStringByValue(cutoff, controller.getParamNormalized(cutoff), &mut text), kResultOk);
        let s = String::from_utf16_lossy(&text[..text.iter().position(|&c| c == 0).unwrap_or(0)]);
        assert!(s.contains("Hz"), "{s}");
        let mut v = 0.0;
        let utf16: Vec<u16> = "1000 Hz\0".encode_utf16().collect();
        assert_eq!(controller.getParamValueByString(cutoff, utf16.as_ptr() as *mut TChar, &mut v), kResultOk);
        assert!(v > 0.0 && v < 1.0);
        processor.setProcessing(0);
        component.setActive(0);
        component.terminate();
        controller.terminate();
    }
}

#[test]
fn in_process_factory() {
    let raw = rawdawg_vst3::create_factory();
    let factory = unsafe { ComPtr::from_raw(raw) }.unwrap();
    exercise(factory);
}

#[test]
fn built_module_exports_get_plugin_factory() {
    let path = built_module_path();
    assert!(path.is_file(), "expected built VST3 module at {}", path.display());
    unsafe {
        let lib = libloading::Library::new(&path).expect("dlopen module");
        #[cfg(target_os = "linux")]
        {
            let entry: libloading::Symbol<unsafe extern "system" fn(*mut c_void) -> bool> = lib.get(b"ModuleEntry").expect("ModuleEntry");
            assert!(entry(std::ptr::null_mut()));
        }
        let get: libloading::Symbol<unsafe extern "system" fn() -> *mut IPluginFactory> = lib.get(b"GetPluginFactory").expect("GetPluginFactory");
        let factory = ComPtr::from_raw(get()).expect("factory");
        exercise(factory);
        #[cfg(target_os = "linux")]
        {
            let exit: libloading::Symbol<unsafe extern "system" fn() -> bool> = lib.get(b"ModuleExit").expect("ModuleExit");
            assert!(exit());
        }
    }
}

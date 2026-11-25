//! VST3 export via the `vst3` (coupler-rs) COM bindings. PLUGIN_API.md §2.
//!
//! M0 ABI spike: a real stereo gain effect (`rawDAWg Gain`) exercising the
//! factory, component, audio processor, edit controller and parameter queue
//! paths with f32 processing. The flagship synth and effect suite reuse this
//! glue in M7. No DSP lives in this crate except the trivial gain multiply.
#![allow(non_snake_case, non_upper_case_globals, clippy::missing_safety_doc)]

use std::cell::Cell;
use std::ffi::{CString, c_char, c_void};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComRef, ComWrapper, uid};

pub const VENDOR: &str = "rawDAWg";
pub const URL: &str = "https://github.com/leonardo-aa/rawDAWg";
pub const GAIN_NAME: &str = "rawDAWg Gain";

/// Stable class IDs (never change once published).
pub const GAIN_PROCESSOR_CID: TUID = uid(0x7A57_D4A6, 0x0001_4A00, 0xB0A5_3E11, 0x9F2C_0A01);
pub const GAIN_CONTROLLER_CID: TUID = uid(0x7A57_D4A6, 0x0002_4A00, 0xB0A5_3E11, 0x9F2C_0A02);
pub const PARAM_GAIN: u32 = 0;

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

/// Audio component + processor. Gain is stored normalized [0,1] → 0..2 linear.
pub struct GainProcessor {
    gain_norm: AtomicU64,
    sample_rate: AtomicU64,
    latency: AtomicU32,
}

impl Class for GainProcessor {
    type Interfaces = (IComponent, IAudioProcessor, IProcessContextRequirements);
}

impl GainProcessor {
    pub fn new() -> Self {
        Self { gain_norm: AtomicU64::new(0.5f64.to_bits()), sample_rate: AtomicU64::new(48_000f64.to_bits()), latency: AtomicU32::new(0) }
    }
    fn gain_linear(&self) -> f32 {
        (f64::from_bits(self.gain_norm.load(Ordering::Relaxed)) * 2.0) as f32
    }
}

impl Default for GainProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl IPluginBaseTrait for GainProcessor {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }
    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IComponentTrait for GainProcessor {
    unsafe fn getControllerClassId(&self, class_id: *mut TUID) -> tresult {
        unsafe { *class_id = GAIN_CONTROLLER_CID };
        kResultOk
    }
    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }
    unsafe fn getBusCount(&self, media_type: MediaType, _dir: BusDirection) -> i32 {
        if media_type as MediaTypes == MediaTypes_::kAudio { 1 } else { 0 }
    }
    unsafe fn getBusInfo(&self, media_type: MediaType, dir: BusDirection, index: i32, bus: *mut BusInfo) -> tresult {
        if media_type as MediaTypes != MediaTypes_::kAudio || index != 0 {
            return kInvalidArgument;
        }
        let bus = unsafe { &mut *bus };
        bus.mediaType = MediaTypes_::kAudio as MediaType;
        bus.direction = dir;
        bus.channelCount = 2;
        copy_wstring(if dir as BusDirections == BusDirections_::kInput { "Input" } else { "Output" }, &mut bus.name);
        bus.busType = BusTypes_::kMain as BusType;
        bus.flags = BusInfo_::BusFlags_::kDefaultActive as uint32;
        kResultOk
    }
    unsafe fn getRoutingInfo(&self, _i: *mut RoutingInfo, _o: *mut RoutingInfo) -> tresult {
        kNotImplemented
    }
    unsafe fn activateBus(&self, _m: MediaType, _d: BusDirection, _i: i32, _s: TBool) -> tresult {
        kResultOk
    }
    unsafe fn setActive(&self, _state: TBool) -> tresult {
        kResultOk
    }
    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        let Some(stream) = (unsafe { ComRef::from_raw(state) }) else { return kInvalidArgument };
        let mut buf = [0u8; 8];
        let mut read: int32 = 0;
        let r = unsafe { stream.read(buf.as_mut_ptr() as *mut c_void, 8, &mut read) };
        if r != kResultOk || read != 8 {
            return kResultFalse;
        }
        let v = f64::from_le_bytes(buf);
        if !v.is_finite() || !(0.0..=1.0).contains(&v) {
            return kResultFalse;
        }
        self.gain_norm.store(v.to_bits(), Ordering::Relaxed);
        kResultOk
    }
    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        let Some(stream) = (unsafe { ComRef::from_raw(state) }) else { return kInvalidArgument };
        let v = f64::from_bits(self.gain_norm.load(Ordering::Relaxed));
        let bytes = v.to_le_bytes();
        let mut written: int32 = 0;
        let r = unsafe { stream.write(bytes.as_ptr() as *mut c_void, 8, &mut written) };
        if r == kResultOk && written == 8 { kResultOk } else { kResultFalse }
    }
}

impl IAudioProcessorTrait for GainProcessor {
    unsafe fn setBusArrangements(&self, inputs: *mut SpeakerArrangement, num_ins: i32, outputs: *mut SpeakerArrangement, num_outs: i32) -> tresult {
        if num_ins != 1 || num_outs != 1 {
            return kResultFalse;
        }
        if unsafe { *inputs } != SpeakerArr::kStereo || unsafe { *outputs } != SpeakerArr::kStereo {
            return kResultFalse;
        }
        kResultTrue
    }
    unsafe fn getBusArrangement(&self, _dir: BusDirection, index: i32, arr: *mut SpeakerArrangement) -> tresult {
        if index != 0 {
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
        self.sample_rate.store(setup.sampleRate.to_bits(), Ordering::Relaxed);
        kResultOk
    }
    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        kResultOk
    }
    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        let pd = unsafe { &*data };
        // apply the last parameter point of the gain queue (sample-accurate
        // ramps arrive with the shared parameter smoother in M7)
        if let Some(changes) = unsafe { ComRef::from_raw(pd.inputParameterChanges) } {
            let n = unsafe { changes.getParameterCount() };
            for i in 0..n {
                if let Some(q) = unsafe { ComRef::from_raw(changes.getParameterData(i)) }
                    && unsafe { q.getParameterId() } == PARAM_GAIN
                {
                    let points = unsafe { q.getPointCount() };
                    if points > 0 {
                        let mut off = 0;
                        let mut v = 0.0;
                        if unsafe { q.getPoint(points - 1, &mut off, &mut v) } == kResultTrue && v.is_finite() {
                            self.gain_norm.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
                        }
                    }
                }
            }
        }
        if pd.numInputs < 1 || pd.numOutputs < 1 || pd.numSamples <= 0 {
            return kResultOk;
        }
        let frames = pd.numSamples as usize;
        let inputs = unsafe { std::slice::from_raw_parts(pd.inputs, pd.numInputs as usize) };
        let outputs = unsafe { std::slice::from_raw_parts(pd.outputs, pd.numOutputs as usize) };
        let nch = inputs[0].numChannels.min(outputs[0].numChannels).max(0) as usize;
        let gain = self.gain_linear();
        for c in 0..nch {
            let inp = unsafe { *inputs[0].__field0.channelBuffers32.add(c) };
            let out = unsafe { *outputs[0].__field0.channelBuffers32.add(c) };
            if inp.is_null() || out.is_null() {
                continue;
            }
            let i = unsafe { std::slice::from_raw_parts(inp, frames) };
            let o = unsafe { std::slice::from_raw_parts_mut(out, frames) };
            for (o, i) in o.iter_mut().zip(i) {
                let v = i * gain;
                *o = if v.is_finite() { v } else { 0.0 };
            }
        }
        kResultOk
    }
    unsafe fn getTailSamples(&self) -> u32 {
        0
    }
}

impl IProcessContextRequirementsTrait for GainProcessor {
    unsafe fn getProcessContextRequirements(&self) -> u32 {
        0
    }
}

pub struct GainController {
    gain_norm: Cell<f64>,
}

impl Class for GainController {
    type Interfaces = (IEditController,);
}

impl GainController {
    pub fn new() -> Self {
        Self { gain_norm: Cell::new(0.5) }
    }
}

impl Default for GainController {
    fn default() -> Self {
        Self::new()
    }
}

impl IPluginBaseTrait for GainController {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }
    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IEditControllerTrait for GainController {
    unsafe fn setComponentState(&self, state: *mut IBStream) -> tresult {
        let Some(stream) = (unsafe { ComRef::from_raw(state) }) else { return kInvalidArgument };
        let mut buf = [0u8; 8];
        let mut read: int32 = 0;
        if unsafe { stream.read(buf.as_mut_ptr() as *mut c_void, 8, &mut read) } == kResultOk && read == 8 {
            let v = f64::from_le_bytes(buf);
            if v.is_finite() {
                self.gain_norm.set(v.clamp(0.0, 1.0));
                return kResultOk;
            }
        }
        kResultFalse
    }
    unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }
    unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }
    unsafe fn getParameterCount(&self) -> i32 {
        1
    }
    unsafe fn getParameterInfo(&self, index: i32, info: *mut ParameterInfo) -> tresult {
        if index != 0 {
            return kInvalidArgument;
        }
        let info = unsafe { &mut *info };
        info.id = PARAM_GAIN;
        copy_wstring("Gain", &mut info.title);
        copy_wstring("Gain", &mut info.shortTitle);
        copy_wstring("", &mut info.units);
        info.stepCount = 0;
        info.defaultNormalizedValue = 0.5;
        info.unitId = 0;
        info.flags = ParameterInfo_::ParameterFlags_::kCanAutomate;
        kResultOk
    }
    unsafe fn getParamStringByValue(&self, id: u32, value: f64, string: *mut String128) -> tresult {
        if id != PARAM_GAIN {
            return kInvalidArgument;
        }
        let db = rawdawg_dsp::math::lin_to_db((value * 2.0) as f32);
        let text = if db <= -144.0 { "-inf dB".to_owned() } else { format!("{db:.1} dB") };
        copy_wstring(&text, unsafe { &mut *string });
        kResultOk
    }
    unsafe fn getParamValueByString(&self, id: u32, string: *mut TChar, value: *mut f64) -> tresult {
        if id != PARAM_GAIN {
            return kInvalidArgument;
        }
        let mut len = 0;
        while unsafe { *string.add(len) } != 0 && len < 128 {
            len += 1;
        }
        let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(string as *const u16, len) });
        let db: f64 = match s.trim().trim_end_matches("dB").trim().parse() {
            Ok(v) => v,
            Err(_) => return kResultFalse,
        };
        let lin = f64::from(rawdawg_dsp::math::db_to_lin(db as f32));
        unsafe { *value = (lin / 2.0).clamp(0.0, 1.0) };
        kResultOk
    }
    unsafe fn normalizedParamToPlain(&self, _id: u32, v: f64) -> f64 {
        v * 2.0
    }
    unsafe fn plainParamToNormalized(&self, _id: u32, v: f64) -> f64 {
        (v / 2.0).clamp(0.0, 1.0)
    }
    unsafe fn getParamNormalized(&self, id: u32) -> f64 {
        if id == PARAM_GAIN { self.gain_norm.get() } else { 0.0 }
    }
    unsafe fn setParamNormalized(&self, id: u32, value: f64) -> tresult {
        if id != PARAM_GAIN || !value.is_finite() {
            return kInvalidArgument;
        }
        self.gain_norm.set(value.clamp(0.0, 1.0));
        kResultOk
    }
    unsafe fn setComponentHandler(&self, _handler: *mut IComponentHandler) -> tresult {
        kResultOk
    }
    unsafe fn createView(&self, _name: FIDString) -> *mut IPlugView {
        // No custom editor yet: hosts fall back to their generic parameter view.
        std::ptr::null_mut()
    }
}

pub struct Factory;

impl Class for Factory {
    type Interfaces = (IPluginFactory,);
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
        2
    }
    unsafe fn getClassInfo(&self, index: i32, info: *mut PClassInfo) -> tresult {
        let info = unsafe { &mut *info };
        match index {
            0 => {
                info.cid = GAIN_PROCESSOR_CID;
                info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
                copy_cstring("Audio Module Class", &mut info.category);
                copy_cstring(GAIN_NAME, &mut info.name);
                kResultOk
            }
            1 => {
                info.cid = GAIN_CONTROLLER_CID;
                info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
                copy_cstring("Component Controller Class", &mut info.category);
                copy_cstring(GAIN_NAME, &mut info.name);
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }
    unsafe fn createInstance(&self, cid: FIDString, iid: FIDString, obj: *mut *mut c_void) -> tresult {
        if cid.is_null() || iid.is_null() || obj.is_null() {
            return kInvalidArgument;
        }
        let cid = unsafe { *(cid as *const TUID) };
        let instance = if cid == GAIN_PROCESSOR_CID {
            ComWrapper::new(GainProcessor::new()).to_com_ptr::<FUnknown>()
        } else if cid == GAIN_CONTROLLER_CID {
            ComWrapper::new(GainController::new()).to_com_ptr::<FUnknown>()
        } else {
            None
        };
        match instance {
            Some(instance) => {
                let ptr = instance.as_ptr();
                unsafe { ((*(*ptr).vtbl).queryInterface)(ptr, iid as *mut TUID, obj) }
            }
            None => kNoInterface,
        }
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

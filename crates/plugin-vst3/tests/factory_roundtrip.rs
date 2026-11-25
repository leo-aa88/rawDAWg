//! T-PLUGIN (M0 subset): load the built VST3 module, obtain the factory, create
//! the component and controller, process audio through the COM ABI and
//! round-trip component state.
#![allow(non_snake_case)]

use std::ffi::c_void;
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComWrapper, Interface};

fn built_module_path() -> std::path::PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap();
    let name = if cfg!(target_os = "windows") { "rawdawg_vst3.dll" } else { "librawdawg_vst3.so" };
    dir.join(name)
}

/// Minimal in-memory IBStream for state round trips.
struct MemStream {
    data: std::cell::RefCell<Vec<u8>>,
    pos: std::cell::Cell<usize>,
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

unsafe fn create<I: Interface>(factory: &ComPtr<IPluginFactory>, cid: &TUID) -> ComPtr<I> {
    let mut obj: *mut c_void = std::ptr::null_mut();
    let iid = I::IID;
    let r = unsafe { factory.createInstance(cid.as_ptr() as FIDString, iid.as_ptr() as FIDString, &mut obj) };
    assert_eq!(r, kResultOk, "createInstance failed");
    unsafe { ComPtr::from_raw(obj as *mut I) }.expect("non-null instance")
}

fn exercise(factory: ComPtr<IPluginFactory>) {
    unsafe {
        let mut finfo: PFactoryInfo = std::mem::zeroed();
        assert_eq!(factory.getFactoryInfo(&mut finfo), kResultOk);
        let vendor = std::ffi::CStr::from_ptr(finfo.vendor.as_ptr()).to_str().unwrap();
        assert_eq!(vendor, "rawDAWg");
        assert_eq!(factory.countClasses(), 2);
        let mut cinfo: PClassInfo = std::mem::zeroed();
        assert_eq!(factory.getClassInfo(0, &mut cinfo), kResultOk);
        assert_eq!(std::ffi::CStr::from_ptr(cinfo.category.as_ptr()).to_str().unwrap(), "Audio Module Class");
        assert_eq!(std::ffi::CStr::from_ptr(cinfo.name.as_ptr()).to_str().unwrap(), "rawDAWg Gain");
        assert_eq!(cinfo.cid, rawdawg_vst3::GAIN_PROCESSOR_CID);
        assert_eq!(factory.getClassInfo(1, &mut cinfo), kResultOk);
        assert_eq!(std::ffi::CStr::from_ptr(cinfo.category.as_ptr()).to_str().unwrap(), "Component Controller Class");
        assert_eq!(factory.getClassInfo(2, &mut cinfo), kInvalidArgument);

        let component: ComPtr<IComponent> = create(&factory, &rawdawg_vst3::GAIN_PROCESSOR_CID);
        let processor: ComPtr<IAudioProcessor> = component.cast().expect("component implements IAudioProcessor");
        assert_eq!(component.initialize(std::ptr::null_mut()), kResultOk);
        let mut ccid: TUID = [0; 16];
        assert_eq!(component.getControllerClassId(&mut ccid), kResultOk);
        assert_eq!(ccid, rawdawg_vst3::GAIN_CONTROLLER_CID);
        assert_eq!(component.getBusCount(MediaTypes_::kAudio as MediaType, BusDirections_::kOutput as BusDirection), 1);
        assert_eq!(component.getBusCount(MediaTypes_::kEvent as MediaType, BusDirections_::kInput as BusDirection), 0);
        let mut ins = SpeakerArr::kStereo;
        let mut outs = SpeakerArr::kStereo;
        assert_eq!(processor.setBusArrangements(&mut ins, 1, &mut outs, 1), kResultTrue);
        let mut mono = SpeakerArr::kMono;
        assert_eq!(processor.setBusArrangements(&mut mono, 1, &mut outs, 1), kResultFalse, "unsupported arrangement must be refused");
        assert_eq!(processor.canProcessSampleSize(SymbolicSampleSizes_::kSample32 as i32), kResultOk);
        assert_eq!(processor.canProcessSampleSize(SymbolicSampleSizes_::kSample64 as i32), kNotImplemented);
        let mut setup = ProcessSetup {
            processMode: ProcessModes_::kRealtime as i32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
            maxSamplesPerBlock: 256,
            sampleRate: 48_000.0,
        };
        assert_eq!(processor.setupProcessing(&mut setup), kResultOk);
        assert_eq!(component.setActive(1), kResultOk);
        assert_eq!(processor.setProcessing(1), kResultOk);

        // Process a block: default gain 0.5 normalized = unity.
        let frames = 256usize;
        let mut in_l: Vec<f32> = (0..frames).map(|i| (i as f32 * 0.1).sin() * 0.5).collect();
        let mut in_r = in_l.clone();
        let mut out_l = vec![0.0f32; frames];
        let mut out_r = vec![0.0f32; frames];
        let mut in_ch = [in_l.as_mut_ptr(), in_r.as_mut_ptr()];
        let mut out_ch = [out_l.as_mut_ptr(), out_r.as_mut_ptr()];
        let mut in_bus: AudioBusBuffers = std::mem::zeroed();
        in_bus.numChannels = 2;
        in_bus.__field0.channelBuffers32 = in_ch.as_mut_ptr();
        let mut out_bus: AudioBusBuffers = std::mem::zeroed();
        out_bus.numChannels = 2;
        out_bus.__field0.channelBuffers32 = out_ch.as_mut_ptr();
        let mut data: ProcessData = std::mem::zeroed();
        data.processMode = ProcessModes_::kRealtime as i32;
        data.symbolicSampleSize = SymbolicSampleSizes_::kSample32 as i32;
        data.numSamples = frames as i32;
        data.numInputs = 1;
        data.numOutputs = 1;
        data.inputs = &mut in_bus;
        data.outputs = &mut out_bus;
        assert_eq!(processor.process(&mut data), kResultOk);
        let expected: Vec<f32> = (0..frames).map(|i| (i as f32 * 0.1).sin() * 0.5).collect();
        for i in 0..frames {
            assert!((out_l[i] - expected[i]).abs() < 1e-6 && (out_r[i] - expected[i]).abs() < 1e-6);
        }

        // Component state round trip: write, modify, read back.
        let stream = ComWrapper::new(MemStream { data: Default::default(), pos: Default::default() });
        let stream_ptr: ComPtr<IBStream> = stream.to_com_ptr().unwrap();
        assert_eq!(component.getState(stream_ptr.as_ptr()), kResultOk);
        assert_eq!(stream.data.borrow().len(), 8);
        // Controller side reads component state and exposes the parameter.
        let controller: ComPtr<IEditController> = create(&factory, &rawdawg_vst3::GAIN_CONTROLLER_CID);
        assert_eq!(controller.initialize(std::ptr::null_mut()), kResultOk);
        stream.pos.set(0);
        assert_eq!(controller.setComponentState(stream_ptr.as_ptr()), kResultOk);
        assert_eq!(controller.getParameterCount(), 1);
        assert!((controller.getParamNormalized(0) - 0.5).abs() < 1e-12);
        let mut pinfo: ParameterInfo = std::mem::zeroed();
        assert_eq!(controller.getParameterInfo(0, &mut pinfo), kResultOk);
        assert_eq!(pinfo.id, rawdawg_vst3::PARAM_GAIN);
        assert_ne!(pinfo.flags & ParameterInfo_::ParameterFlags_::kCanAutomate, 0);
        // garbage state rejected
        let bad = ComWrapper::new(MemStream { data: std::cell::RefCell::new(vec![1, 2, 3]), pos: Default::default() });
        let bad_ptr: ComPtr<IBStream> = bad.to_com_ptr().unwrap();
        assert_eq!(component.setState(bad_ptr.as_ptr()), kResultFalse);
        assert_eq!(processor.setProcessing(0), kResultOk);
        assert_eq!(component.setActive(0), kResultOk);
        assert_eq!(component.terminate(), kResultOk);
        assert_eq!(controller.terminate(), kResultOk);
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

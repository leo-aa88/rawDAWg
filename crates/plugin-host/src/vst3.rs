//! In-process VST3 bridge (PLUGIN_API.md §3, PLUG-02), the VST3 twin of [`crate::clap`].
//!
//! [`Vst3Instance`] owns the main-thread half (component, controller, host
//! context, connection) and is the app's mirror; [`Vst3Processor`] owns the
//! `IAudioProcessor` pointer inside the realtime plan with preallocated bus
//! buffers, event list and parameter queues. Activation (`setupProcessing`,
//! `setActive`, `setProcessing`) happens on the main thread during compilation;
//! the processor migrates between plans and is handed back through a channel
//! when its plan is dropped off the RT thread.
//!
//! Only f32 processing and stereo buses are negotiated; a plugin that refuses
//! that arrangement stays a stand-in with an explicit reason.
#![allow(non_snake_case, clippy::missing_safety_doc)]

use crate::catalog::{Format, PluginDescription, ScannedLibrary};
use crate::hosted::{MAX_PARAM_TEXT_CHARS, ParamTextCache};
use crate::main_thread::MainThreadCell;
use rawdawg_dsp::processor::{PrepareError, PrepareSpec, ProcessContext, Processor, Tail, sanitize_display_text};
use rawdawg_types::events::{EventKind, TimedEvent};
use rawdawg_types::params::ParamCategory;
use rawdawg_types::{ParamDescriptor, ParamId, ParamMapping, ParamUnit};
use std::cell::{RefCell, UnsafeCell};
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_void};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComWrapper, Interface};

/// Maximum events forwarded per block (bounded, preallocated).
pub const MAX_EVENTS: usize = 4096;
/// Maximum parameter points queued per block.
const MAX_POINTS: usize = 4096;
/// Opaque state larger than this is refused (PROJECT_FORMAT.md: 64 MiB per instance).
pub const MAX_STATE_BYTES: usize = 64 * 1024 * 1024;
/// Parameters above this count are not exposed as descriptors.
pub const MAX_PARAMS: usize = 4096;
/// Project-side JSON envelope keys for a VST3 instance's state.
pub const COMPONENT_STATE_KEY: &str = "vst3_component_b64";
pub const CONTROLLER_STATE_KEY: &str = "vst3_controller_b64";

const AUDIO_MODULE_CLASS: &str = "Audio Module Class";

// ---------------------------------------------------------------------------------------------
// module loading

/// A loaded VST3 module (bundle or bare file); keeps the library mapped.
pub struct Vst3Module {
    pub path: PathBuf,
    factory: ComPtr<IPluginFactory>,
    exit: Option<unsafe extern "system" fn() -> bool>,
    _lib: libloading::Library,
}

// SAFETY: the factory pointer is a COM object the module keeps valid while loaded; VST3
// factories are documented as callable from any thread for class enumeration/creation.
unsafe impl Send for Vst3Module {}
unsafe impl Sync for Vst3Module {}

/// Resolve the shared object inside a `.vst3` bundle directory (or accept a bare module).
pub fn module_binary(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    if !path.is_dir() {
        return None;
    }
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let (arch, file) = if cfg!(target_os = "windows") { ("x86_64-win", format!("{stem}.vst3")) } else { ("x86_64-linux", format!("{stem}.so")) };
    let candidate = path.join("Contents").join(arch).join(&file);
    if candidate.is_file() {
        return Some(candidate);
    }
    // any architecture folder with a matching binary (bundles built elsewhere)
    let contents = path.join("Contents");
    for entry in std::fs::read_dir(&contents).ok()?.flatten() {
        let p = entry.path().join(&file);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

impl Vst3Module {
    /// # Safety
    /// Executes native code from `path`.
    pub unsafe fn load(path: &Path) -> Result<Self, String> {
        let binary = module_binary(path).ok_or_else(|| format!("{} is not a VST3 module or bundle", path.display()))?;
        // SAFETY: forwarded from the caller's contract.
        let lib = unsafe { libloading::Library::new(&binary) }.map_err(|e| format!("cannot load {}: {e}", binary.display()))?;
        let mut exit = None;
        #[cfg(target_os = "linux")]
        unsafe {
            if let Ok(entry) = lib.get::<unsafe extern "system" fn(*mut c_void) -> bool>(b"ModuleEntry")
                && !entry(std::ptr::null_mut())
            {
                return Err("ModuleEntry returned false".into());
            }
            if let Ok(f) = lib.get::<unsafe extern "system" fn() -> bool>(b"ModuleExit") {
                exit = Some(*f);
            }
        }
        #[cfg(target_os = "windows")]
        unsafe {
            if let Ok(init) = lib.get::<unsafe extern "system" fn() -> bool>(b"InitDll")
                && !init()
            {
                return Err("InitDll returned false".into());
            }
            if let Ok(f) = lib.get::<unsafe extern "system" fn() -> bool>(b"ExitDll") {
                exit = Some(*f);
            }
        }
        let get: unsafe extern "system" fn() -> *mut IPluginFactory = unsafe { *lib.get(b"GetPluginFactory").map_err(|_| "module exports no GetPluginFactory".to_owned())? };
        let factory = unsafe { ComPtr::from_raw(get()) }.ok_or_else(|| "GetPluginFactory returned null".to_owned())?;
        Ok(Self { path: path.to_path_buf(), factory, exit, _lib: lib })
    }

    pub fn factory(&self) -> &ComPtr<IPluginFactory> {
        &self.factory
    }
}

impl Drop for Vst3Module {
    fn drop(&mut self) {
        if let Some(exit) = self.exit {
            // SAFETY: the module's own exit hook, called once after our last use of its factory
            // (dropped right after this), before the library is unloaded.
            unsafe {
                exit();
            }
        }
    }
}

/// Per-process cache of loaded VST3 modules by canonical path.
#[derive(Default)]
pub struct Vst3ModuleCache {
    modules: HashMap<PathBuf, Arc<Vst3Module>>,
}

impl Vst3ModuleCache {
    /// # Safety
    /// Executes native code from `path` (catalog entries that passed the isolated scan only).
    pub unsafe fn load(&mut self, path: &Path) -> Result<Arc<Vst3Module>, String> {
        let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if let Some(m) = self.modules.get(&key) {
            return Ok(m.clone());
        }
        // SAFETY: forwarded.
        let m = Arc::new(unsafe { Vst3Module::load(path) }?);
        self.modules.insert(key, m.clone());
        Ok(m)
    }
}

// ---------------------------------------------------------------------------------------------
// host objects handed to plugins

fn cstr_field(bytes: &[std::ffi::c_char]) -> String {
    let len = bytes.iter().position(|&c| c == 0).unwrap_or(bytes.len());
    let raw: Vec<u8> = bytes[..len].iter().map(|&c| c as u8).collect();
    String::from_utf8_lossy(&raw).into_owned()
}

fn wstr_field(chars: &[TChar]) -> String {
    let len = chars.iter().position(|&c| c == 0).unwrap_or(chars.len());
    String::from_utf16_lossy(&chars[..len])
}

fn write_wstr(src: &str, dst: &mut [TChar]) {
    let mut n = 0;
    for (s, d) in src.encode_utf16().zip(dst.iter_mut()) {
        *d = s;
        n += 1;
    }
    if n < dst.len() {
        dst[n] = 0;
    } else if let Some(l) = dst.last_mut() {
        *l = 0;
    }
}

/// Hex of the 16 raw class-id bytes (the catalog's stable plugin id for VST3).
pub fn cid_hex(cid: &TUID) -> String {
    cid.iter().map(|b| format!("{:02X}", *b as u8)).collect()
}

fn cid_from_hex(s: &str) -> Option<TUID> {
    if s.len() != 32 {
        return None;
    }
    let mut out: TUID = [0; 16];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()? as std::ffi::c_char;
    }
    Some(out)
}

#[derive(Clone)]
enum Attr {
    Int(i64),
    Float(f64),
    Str(Vec<u16>),
    Bin(Vec<u8>),
}

/// IAttributeList owned by a [`HostMessage`].
pub struct HostAttributes {
    attrs: Mutex<HashMap<String, Attr>>,
}

impl Class for HostAttributes {
    type Interfaces = (IAttributeList,);
}

fn attr_key(id: IAttrID) -> String {
    if id.is_null() { String::new() } else { unsafe { CStr::from_ptr(id) }.to_string_lossy().into_owned() }
}

impl IAttributeListTrait for HostAttributes {
    unsafe fn setInt(&self, id: IAttrID, value: int64) -> tresult {
        self.attrs.lock().map(|mut a| a.insert(attr_key(id), Attr::Int(value))).map(|_| kResultOk).unwrap_or(kResultFalse)
    }
    unsafe fn getInt(&self, id: IAttrID, value: *mut int64) -> tresult {
        match self.attrs.lock().ok().and_then(|a| a.get(&attr_key(id)).cloned()) {
            Some(Attr::Int(v)) => {
                unsafe { *value = v };
                kResultOk
            }
            _ => kResultFalse,
        }
    }
    unsafe fn setFloat(&self, id: IAttrID, value: f64) -> tresult {
        self.attrs.lock().map(|mut a| a.insert(attr_key(id), Attr::Float(value))).map(|_| kResultOk).unwrap_or(kResultFalse)
    }
    unsafe fn getFloat(&self, id: IAttrID, value: *mut f64) -> tresult {
        match self.attrs.lock().ok().and_then(|a| a.get(&attr_key(id)).cloned()) {
            Some(Attr::Float(v)) => {
                unsafe { *value = v };
                kResultOk
            }
            _ => kResultFalse,
        }
    }
    unsafe fn setString(&self, id: IAttrID, string: *const TChar) -> tresult {
        let mut v = Vec::new();
        let mut i = 0;
        while !string.is_null() && i < 65_536 {
            let c = unsafe { *string.add(i) };
            if c == 0 {
                break;
            }
            v.push(c);
            i += 1;
        }
        self.attrs.lock().map(|mut a| a.insert(attr_key(id), Attr::Str(v))).map(|_| kResultOk).unwrap_or(kResultFalse)
    }
    unsafe fn getString(&self, id: IAttrID, string: *mut TChar, size: uint32) -> tresult {
        match self.attrs.lock().ok().and_then(|a| a.get(&attr_key(id)).cloned()) {
            Some(Attr::Str(v)) => {
                let cap = (size as usize / 2).max(1);
                let dst = unsafe { std::slice::from_raw_parts_mut(string, cap) };
                let n = v.len().min(cap - 1);
                dst[..n].copy_from_slice(&v[..n]);
                dst[n] = 0;
                kResultOk
            }
            _ => kResultFalse,
        }
    }
    unsafe fn setBinary(&self, id: IAttrID, data: *const c_void, size: uint32) -> tresult {
        let bytes = if data.is_null() { Vec::new() } else { unsafe { std::slice::from_raw_parts(data as *const u8, size as usize) }.to_vec() };
        self.attrs.lock().map(|mut a| a.insert(attr_key(id), Attr::Bin(bytes))).map(|_| kResultOk).unwrap_or(kResultFalse)
    }
    unsafe fn getBinary(&self, id: IAttrID, data: *mut *const c_void, size: *mut uint32) -> tresult {
        let Ok(a) = self.attrs.lock() else { return kResultFalse };
        match a.get(&attr_key(id)) {
            Some(Attr::Bin(v)) => {
                // the pointer stays valid while the attribute exists (the message outlives the call)
                unsafe {
                    *data = v.as_ptr() as *const c_void;
                    *size = v.len() as uint32;
                }
                kResultOk
            }
            _ => kResultFalse,
        }
    }
}

/// IMessage the host creates on request (component ↔ controller messages).
pub struct HostMessage {
    id: Mutex<Option<CString>>,
    attrs: ComWrapper<HostAttributes>,
}

impl Class for HostMessage {
    type Interfaces = (IMessage,);
}

impl IMessageTrait for HostMessage {
    unsafe fn getMessageID(&self) -> FIDString {
        self.id.lock().ok().and_then(|g| g.as_ref().map(|c| c.as_ptr())).unwrap_or(std::ptr::null())
    }
    unsafe fn setMessageID(&self, id: FIDString) {
        if let Ok(mut g) = self.id.lock() {
            *g = if id.is_null() { None } else { Some(unsafe { CStr::from_ptr(id) }.to_owned()) };
        }
    }
    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        // owned by the message, no reference added (SDK semantics)
        self.attrs.as_com_ref::<IAttributeList>().map(|r| r.as_ptr()).unwrap_or(std::ptr::null_mut())
    }
}

/// IHostApplication: names the host and creates messages.
pub struct HostApplication;

impl Class for HostApplication {
    type Interfaces = (IHostApplication,);
}

impl IHostApplicationTrait for HostApplication {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        write_wstr(crate::host::HOST_NAME, unsafe { &mut *name });
        kResultOk
    }
    unsafe fn createInstance(&self, cid: *mut TUID, iid: *mut TUID, obj: *mut *mut c_void) -> tresult {
        if cid.is_null() || iid.is_null() || obj.is_null() {
            return kInvalidArgument;
        }
        if unsafe { *cid }.map(|b| b as u8) == IMessage::IID {
            let msg = ComWrapper::new(HostMessage { id: Mutex::new(None), attrs: ComWrapper::new(HostAttributes { attrs: Mutex::new(HashMap::new()) }) });
            let Some(ptr) = msg.to_com_ptr::<FUnknown>() else { return kNoInterface };
            let raw = ptr.as_ptr();
            return unsafe { ((*(*raw).vtbl).queryInterface)(raw, iid, obj) };
        }
        kNoInterface
    }
}

/// IComponentHandler: records parameter edits coming from the plugin (its editor).
pub struct ComponentHandler {
    pub edits: Mutex<Vec<(u32, f64)>>,
    pub restart_flags: std::sync::atomic::AtomicI32,
}

impl Class for ComponentHandler {
    type Interfaces = (IComponentHandler,);
}

impl IComponentHandlerTrait for ComponentHandler {
    unsafe fn beginEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }
    unsafe fn performEdit(&self, id: ParamID, value: ParamValue) -> tresult {
        if let Ok(mut e) = self.edits.lock()
            && e.len() < MAX_POINTS
        {
            e.push((id, value));
        }
        kResultOk
    }
    unsafe fn endEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }
    unsafe fn restartComponent(&self, flags: int32) -> tresult {
        self.restart_flags.fetch_or(flags, std::sync::atomic::Ordering::Relaxed);
        kResultOk
    }
}

/// In-memory IBStream.
pub struct MemStream {
    data: RefCell<Vec<u8>>,
    pos: std::cell::Cell<usize>,
}

impl MemStream {
    pub fn new(data: Vec<u8>) -> ComWrapper<Self> {
        ComWrapper::new(Self { data: RefCell::new(data), pos: std::cell::Cell::new(0) })
    }
    pub fn take(&self) -> Vec<u8> {
        std::mem::take(&mut *self.data.borrow_mut())
    }
}

// SAFETY: a MemStream is created, handed to one plugin call and read back on the same
// thread; ComWrapper demands Sync for the COM object type only.
unsafe impl Sync for MemStream {}
unsafe impl Send for MemStream {}

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
        if self.data.borrow().len() + n > MAX_STATE_BYTES {
            return kOutOfMemory;
        }
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

// ---------------------------------------------------------------------------------------------
// realtime host objects (preallocated; touched only by the audio thread while processing)

/// One parameter's queue for the current block.
pub struct HostQueue {
    id: u32,
    points: UnsafeCell<Vec<(i32, f64)>>,
}
// SAFETY: the audio thread is the only user while `process` runs; the main thread only
// creates/destroys queues (never concurrently with a block).
unsafe impl Sync for HostQueue {}
unsafe impl Send for HostQueue {}
impl Class for HostQueue {
    type Interfaces = (IParamValueQueue,);
}
impl IParamValueQueueTrait for HostQueue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.id
    }
    unsafe fn getPointCount(&self) -> int32 {
        unsafe { (*self.points.get()).len() as int32 }
    }
    unsafe fn getPoint(&self, index: int32, sample_offset: *mut int32, value: *mut ParamValue) -> tresult {
        let points = unsafe { &*self.points.get() };
        let Some((o, v)) = usize::try_from(index).ok().and_then(|i| points.get(i)) else { return kResultFalse };
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

/// The block's parameter changes: queues with at least one point, by index.
pub struct HostChanges {
    queues: Vec<ComPtr<IParamValueQueue>>,
    wrappers: Vec<ComWrapper<HostQueue>>,
    active: UnsafeCell<Vec<usize>>,
}
// SAFETY: see HostQueue.
unsafe impl Sync for HostChanges {}
unsafe impl Send for HostChanges {}
impl Class for HostChanges {
    type Interfaces = (IParameterChanges,);
}
impl IParameterChangesTrait for HostChanges {
    unsafe fn getParameterCount(&self) -> int32 {
        unsafe { (*self.active.get()).len() as int32 }
    }
    unsafe fn getParameterData(&self, index: int32) -> *mut IParamValueQueue {
        let active = unsafe { &*self.active.get() };
        usize::try_from(index).ok().and_then(|i| active.get(i)).and_then(|&q| self.queues.get(q)).map(|q| q.as_ptr()).unwrap_or(std::ptr::null_mut())
    }
    unsafe fn addParameterData(&self, _id: *const ParamID, _index: *mut int32) -> *mut IParamValueQueue {
        std::ptr::null_mut()
    }
}

/// Output parameter changes from the plugin are accepted and dropped (bounded).
pub struct SinkChanges;
impl Class for SinkChanges {
    type Interfaces = (IParameterChanges,);
}
impl IParameterChangesTrait for SinkChanges {
    unsafe fn getParameterCount(&self) -> int32 {
        0
    }
    unsafe fn getParameterData(&self, _index: int32) -> *mut IParamValueQueue {
        std::ptr::null_mut()
    }
    unsafe fn addParameterData(&self, _id: *const ParamID, _index: *mut int32) -> *mut IParamValueQueue {
        std::ptr::null_mut()
    }
}

pub struct HostEvents {
    events: UnsafeCell<Vec<Event>>,
}
// SAFETY: see HostQueue.
unsafe impl Sync for HostEvents {}
unsafe impl Send for HostEvents {}
impl Class for HostEvents {
    type Interfaces = (IEventList,);
}
impl IEventListTrait for HostEvents {
    unsafe fn getEventCount(&self) -> int32 {
        unsafe { (*self.events.get()).len() as int32 }
    }
    unsafe fn getEvent(&self, index: int32, e: *mut Event) -> tresult {
        let events = unsafe { &*self.events.get() };
        let Some(ev) = usize::try_from(index).ok().and_then(|i| events.get(i)) else { return kResultFalse };
        unsafe { *e = *ev };
        kResultOk
    }
    unsafe fn addEvent(&self, e: *mut Event) -> tresult {
        // output events from the plugin: kept bounded, otherwise ignored
        let events = unsafe { &mut *self.events.get() };
        if e.is_null() || events.len() >= MAX_EVENTS {
            return kResultFalse;
        }
        events.push(unsafe { *e });
        kResultOk
    }
}

// ---------------------------------------------------------------------------------------------
// description (scanner child)

fn class_info(factory: &ComPtr<IPluginFactory>, index: i32) -> Option<(PClassInfo, Option<PClassInfo2>)> {
    let mut info: PClassInfo = unsafe { std::mem::zeroed() };
    if unsafe { factory.getClassInfo(index, &mut info) } != kResultOk {
        return None;
    }
    let info2 = factory.cast::<IPluginFactory2>().and_then(|f2| {
        let mut i2: PClassInfo2 = unsafe { std::mem::zeroed() };
        (unsafe { f2.getClassInfo2(index, &mut i2) } == kResultOk).then_some(i2)
    });
    Some((info, info2))
}

unsafe fn create_object<I: Interface>(factory: &ComPtr<IPluginFactory>, cid: &TUID) -> Option<ComPtr<I>> {
    let mut obj: *mut c_void = std::ptr::null_mut();
    let iid = I::IID;
    let r = unsafe { factory.createInstance(cid.as_ptr() as FIDString, iid.as_ptr() as FIDString, &mut obj) };
    if r != kResultOk || obj.is_null() {
        return None;
    }
    unsafe { ComPtr::from_raw(obj as *mut I) }
}

/// Describe every audio-module class of the VST3 module (bundle or file) at `path`.
///
/// # Safety
/// Loads and executes native code; call from the isolated scanner process.
pub unsafe fn describe_vst3_module(path: &Path) -> Result<ScannedLibrary, String> {
    let fingerprint_path = module_binary(path).unwrap_or_else(|| path.to_path_buf());
    let fingerprint = crate::catalog::Fingerprint::of(&fingerprint_path).ok_or_else(|| format!("cannot stat {}", path.display()))?;
    // SAFETY: forwarded.
    let module = unsafe { Vst3Module::load(path) }?;
    let factory = module.factory();
    let mut finfo: PFactoryInfo = unsafe { std::mem::zeroed() };
    let vendor_default = if unsafe { factory.getFactoryInfo(&mut finfo) } == kResultOk { cstr_field(&finfo.vendor) } else { String::new() };
    let host = ComWrapper::new(HostApplication);
    let host_ptr: ComPtr<FUnknown> = host.to_com_ptr().ok_or("host context")?;
    let count = unsafe { factory.countClasses() };
    let mut plugins = Vec::new();
    for i in 0..count.min(512) {
        let Some((info, info2)) = class_info(factory, i) else { continue };
        if cstr_field(&info.category) != AUDIO_MODULE_CLASS {
            continue;
        }
        let subcats = info2.as_ref().map(|i| cstr_field(&i.subCategories)).unwrap_or_default();
        let features: Vec<String> = subcats.split('|').filter(|s| !s.is_empty()).map(|s| s.to_owned()).collect();
        let mut d = PluginDescription {
            format: Format::Vst3,
            plugin_id: cid_hex(&info.cid),
            name: cstr_field(&info.name),
            vendor: info2.as_ref().map(|i| cstr_field(&i.vendor)).filter(|v| !v.is_empty()).unwrap_or_else(|| vendor_default.clone()),
            version: info2.as_ref().map(|i| cstr_field(&i.version)).unwrap_or_default(),
            description: subcats.clone(),
            is_instrument: features.iter().any(|f| f == "Instrument"),
            is_effect: features.iter().any(|f| f == "Fx"),
            features,
            audio_inputs: 0,
            audio_outputs: 0,
            main_input_channels: 0,
            main_output_channels: 0,
            note_inputs: 0,
            param_count: 0,
            has_gui: false,
            notes: Vec::new(),
        };
        match unsafe { create_object::<IComponent>(factory, &info.cid) } {
            Some(component) => {
                if unsafe { component.initialize(host_ptr.as_ptr()) } != kResultOk {
                    d.notes.push("component initialize failed during scan".into());
                } else {
                    let audio = MediaTypes_::kAudio as MediaType;
                    d.audio_inputs = unsafe { component.getBusCount(audio, BusDirections_::kInput as BusDirection) }.max(0) as u32;
                    d.audio_outputs = unsafe { component.getBusCount(audio, BusDirections_::kOutput as BusDirection) }.max(0) as u32;
                    d.note_inputs = unsafe { component.getBusCount(MediaTypes_::kEvent as MediaType, BusDirections_::kInput as BusDirection) }.max(0) as u32;
                    let mut bus: BusInfo = unsafe { std::mem::zeroed() };
                    if d.audio_inputs > 0 && unsafe { component.getBusInfo(audio, BusDirections_::kInput as BusDirection, 0, &mut bus) } == kResultOk {
                        d.main_input_channels = bus.channelCount.max(0) as u32;
                    }
                    if d.audio_outputs > 0 && unsafe { component.getBusInfo(audio, BusDirections_::kOutput as BusDirection, 0, &mut bus) } == kResultOk {
                        d.main_output_channels = bus.channelCount.max(0) as u32;
                    }
                    // controller: separate class or the component itself
                    let mut ccid: TUID = [0; 16];
                    let controller: Option<ComPtr<IEditController>> = if unsafe { component.getControllerClassId(&mut ccid) } == kResultOk {
                        let c = unsafe { create_object::<IEditController>(factory, &ccid) };
                        if let Some(c) = &c {
                            unsafe { c.initialize(host_ptr.as_ptr()) };
                        }
                        c
                    } else {
                        component.cast::<IEditController>()
                    };
                    if let Some(c) = &controller {
                        d.param_count = unsafe { c.getParameterCount() }.max(0) as u32;
                        let name = CString::new("editor").unwrap();
                        let view = unsafe { c.createView(name.as_ptr()) };
                        if let Some(view) = unsafe { ComPtr::<IPlugView>::from_raw(view) } {
                            d.has_gui = true;
                            drop(view);
                        }
                        if component.cast::<IEditController>().is_none() {
                            unsafe { c.terminate() };
                        }
                    }
                    unsafe { component.terminate() };
                }
                if !d.is_instrument && !d.is_effect {
                    d.is_instrument = d.note_inputs > 0 && d.audio_inputs == 0 && d.audio_outputs > 0;
                    d.is_effect = d.audio_inputs > 0 && d.audio_outputs > 0;
                }
            }
            None => d.notes.push("instantiation failed during scan".into()),
        }
        plugins.push(d);
    }
    if plugins.is_empty() {
        return Err("module exposes no audio module classes".to_owned());
    }
    Ok(ScannedLibrary { path: path.to_path_buf(), fingerprint, plugins, scanned_at_unix: crate::catalog::now_unix(), api_version: String::new() })
}

// ---------------------------------------------------------------------------------------------
// main-thread instance

/// Descriptors and, per descriptor, the plugin's unit label (`ParameterInfo::units`).
type DescriptorTable = (&'static [ParamDescriptor], &'static [&'static str]);

fn descriptor_cache() -> &'static Mutex<HashMap<String, DescriptorTable>> {
    static CACHE: OnceLock<Mutex<HashMap<String, DescriptorTable>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn descriptors_for(cid_hex: &str, controller: &ComPtr<IEditController>) -> DescriptorTable {
    let mut cache = descriptor_cache().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(d) = cache.get(cid_hex) {
        return *d;
    }
    let count = unsafe { controller.getParameterCount() }.max(0) as usize;
    let mut out = Vec::new();
    let mut units = Vec::new();
    for i in 0..count.min(MAX_PARAMS) {
        let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
        if unsafe { controller.getParameterInfo(i as i32, &mut info) } != kResultOk {
            continue;
        }
        let name = crate::clap::leak_str(&wstr_field(&info.title));
        let key = crate::clap::leak_str(&format!("p{}", info.id));
        let mapping = match info.stepCount {
            1 => ParamMapping::Toggle,
            n if n > 1 => ParamMapping::Discrete { steps: (n + 1) as u32 },
            _ => ParamMapping::Linear { min: 0.0, max: 1.0 },
        };
        let default = (info.defaultNormalizedValue as f32).clamp(0.0, 1.0);
        let mut d = ParamDescriptor::continuous(info.id, key, name, ParamUnit::None, mapping, if default.is_finite() { default } else { 0.0 }, ParamCategory::Effects);
        let f = info.flags;
        d.flags.automatable = f & ParameterInfo_::ParameterFlags_::kCanAutomate != 0;
        d.flags.modulatable = false;
        d.flags.stepped = info.stepCount > 0;
        d.flags.hidden = f & ParameterInfo_::ParameterFlags_::kIsHidden != 0;
        d.flags.read_only = f & ParameterInfo_::ParameterFlags_::kIsReadOnly != 0;
        d.flags.is_bypass = f & ParameterInfo_::ParameterFlags_::kIsBypass != 0;
        out.push(d);
        units.push(crate::clap::leak_str(&sanitize_display_text(&wstr_field(&info.units), 16).unwrap_or_default()));
    }
    let table: DescriptorTable = (Box::leak(out.into_boxed_slice()), Box::leak(units.into_boxed_slice()));
    cache.insert(cid_hex.to_owned(), table);
    table
}

/// A VST3 readout: the controller's string for the value, followed by the parameter's unit
/// label when the string is a bare number. VST3 keeps the two apart and plugins differ in
/// whether the string repeats the unit; a word such as "Off" or a value that already ends
/// with its unit is shown as it is.
pub(crate) fn vst3_readout(text: &str, units: &str) -> Option<String> {
    let text = text.trim_end();
    let bare_number = text.chars().last().is_some_and(|c| c.is_ascii_digit());
    if units.is_empty() || !bare_number || text.ends_with(units) {
        sanitize_display_text(text, MAX_PARAM_TEXT_CHARS)
    } else {
        sanitize_display_text(&format!("{text} {units}"), MAX_PARAM_TEXT_CHARS)
    }
}

/// Bus layout negotiated with the plugin.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Vst3Layout {
    /// Channel count per audio input bus (main first; aux buses after).
    pub inputs: Vec<u32>,
    pub outputs: Vec<u32>,
    pub event_input: bool,
}

struct Returned(ComPtr<IAudioProcessor>);

/// Main-thread half of a hosted VST3 plugin (and the app's mirror).
pub struct Vst3Instance {
    module: Arc<Vst3Module>,
    cell: MainThreadCell<Main>,
    plugin_id: String,
    type_id: &'static str,
    descriptors: &'static [ParamDescriptor],
    /// Unit label per descriptor.
    units: &'static [&'static str],
    layout: Vst3Layout,
    values: Vec<f32>,
    /// Readouts from the controller's `getParamStringByValue`.
    texts: ParamTextCache,
    active: Option<(f64, usize)>,
    latency: u32,
    /// Parameter sets made while no RT half exists (applied as queue points on activation).
    pending: Vec<(u32, f32)>,
    return_tx: Sender<Returned>,
    return_rx: Receiver<Returned>,
    pub info: PluginDescription,
    pub problems: Arc<std::sync::atomic::AtomicU32>,
}

/// Main-thread-only COM objects.
struct Main {
    component: ComPtr<IComponent>,
    processor: ComPtr<IAudioProcessor>,
    controller: ComPtr<IEditController>,
    separate_controller: bool,
    _host: ComWrapper<HostApplication>,
    handler: ComWrapper<ComponentHandler>,
}

impl Drop for Main {
    fn drop(&mut self) {
        unsafe {
            if let (Some(a), Some(b)) = (self.component.cast::<IConnectionPoint>(), self.controller.cast::<IConnectionPoint>()) {
                a.disconnect(b.as_ptr());
                b.disconnect(a.as_ptr());
            }
            if self.separate_controller {
                self.controller.terminate();
            }
            self.component.terminate();
        }
    }
}

impl Vst3Instance {
    /// Instantiate the class `info.plugin_id` (cid hex) from `module` on the main thread.
    pub fn new(module: Arc<Vst3Module>, info: &PluginDescription) -> Result<Self, String> {
        let cid = cid_from_hex(&info.plugin_id).ok_or_else(|| format!("{} is not a VST3 class id", info.plugin_id))?;
        let factory = module.factory();
        let host = ComWrapper::new(HostApplication);
        let host_ptr: ComPtr<FUnknown> = host.to_com_ptr().ok_or("host context")?;
        // SAFETY: the module passed the isolated scan; COM calls follow the VST3 lifecycle.
        let component = unsafe { create_object::<IComponent>(factory, &cid) }.ok_or_else(|| format!("cannot create component {}", info.name))?;
        if unsafe { component.initialize(host_ptr.as_ptr()) } != kResultOk {
            return Err("component initialize failed".into());
        }
        let processor = component.cast::<IAudioProcessor>().ok_or("component has no IAudioProcessor")?;
        let mut ccid: TUID = [0; 16];
        let (controller, separate_controller) = if unsafe { component.getControllerClassId(&mut ccid) } == kResultOk
            && let Some(c) = unsafe { create_object::<IEditController>(factory, &ccid) }
        {
            if unsafe { c.initialize(host_ptr.as_ptr()) } != kResultOk {
                unsafe { component.terminate() };
                return Err("controller initialize failed".into());
            }
            (c, true)
        } else if let Some(c) = component.cast::<IEditController>() {
            (c, false)
        } else {
            unsafe { component.terminate() };
            return Err("plugin exposes no edit controller".into());
        };
        let handler = ComWrapper::new(ComponentHandler { edits: Mutex::new(Vec::new()), restart_flags: std::sync::atomic::AtomicI32::new(0) });
        if let Some(h) = handler.to_com_ptr::<IComponentHandler>() {
            unsafe { controller.setComponentHandler(h.as_ptr()) };
        }
        if let (Some(a), Some(b)) = (component.cast::<IConnectionPoint>(), controller.cast::<IConnectionPoint>()) {
            unsafe {
                a.connect(b.as_ptr());
                b.connect(a.as_ptr());
            }
        }
        // initial controller sync from the component's default state
        let stream = MemStream::new(Vec::new());
        if let Some(s) = stream.to_com_ptr::<IBStream>()
            && unsafe { component.getState(s.as_ptr()) } == kResultOk
        {
            let bytes = stream.take();
            let s2 = MemStream::new(bytes);
            if let Some(p) = s2.to_com_ptr::<IBStream>() {
                unsafe { controller.setComponentState(p.as_ptr()) };
            }
        }
        let (descriptors, units) = descriptors_for(&info.plugin_id, &controller);
        let values: Vec<f32> = descriptors.iter().map(|d| (unsafe { controller.getParamNormalized(d.id.0) } as f32).clamp(0.0, 1.0)).collect();
        let mut layout = Vst3Layout::default();
        let audio = MediaTypes_::kAudio as MediaType;
        for (dir, out) in [(BusDirections_::kInput, &mut layout.inputs), (BusDirections_::kOutput, &mut layout.outputs)] {
            let n = unsafe { component.getBusCount(audio, dir as BusDirection) }.clamp(0, 16);
            for i in 0..n {
                let mut bus: BusInfo = unsafe { std::mem::zeroed() };
                let ch = if unsafe { component.getBusInfo(audio, dir as BusDirection, i, &mut bus) } == kResultOk { bus.channelCount.clamp(0, 64) as u32 } else { 2 };
                out.push(ch);
            }
        }
        layout.event_input = unsafe { component.getBusCount(MediaTypes_::kEvent as MediaType, BusDirections_::kInput as BusDirection) } > 0;
        let latency = unsafe { processor.getLatencySamples() };
        let (return_tx, return_rx) = channel();
        Ok(Self {
            module,
            cell: MainThreadCell::new(Main { component, processor, controller, separate_controller, _host: host, handler }),
            type_id: crate::clap::leak_str(&format!("vst3:{}", info.plugin_id)),
            plugin_id: info.plugin_id.clone(),
            descriptors,
            units,
            layout,
            values,
            texts: ParamTextCache::default(),
            active: None,
            latency,
            pending: Vec::new(),
            return_tx,
            return_rx,
            info: info.clone(),
            problems: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        })
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }
    pub fn module_path(&self) -> &Path {
        &self.module.path
    }
    pub fn layout(&self) -> &Vst3Layout {
        &self.layout
    }
    pub fn is_instrument(&self) -> bool {
        self.info.is_instrument
    }
    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }
    pub fn problem_count(&self) -> u32 {
        self.problems.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Refresh cached values from the controller, folding in edits the plugin reported.
    pub fn refresh_values(&mut self) {
        let Some(m) = self.cell.get() else { return };
        if let Ok(mut edits) = m.handler.edits.lock() {
            edits.clear();
        }
        for (d, v) in self.descriptors.iter().zip(self.values.iter_mut()) {
            let val = unsafe { m.controller.getParamNormalized(d.id.0) };
            if val.is_finite() {
                *v = (val as f32).clamp(0.0, 1.0);
            }
        }
    }

    pub fn collect_returns(&mut self) {
        while let Ok(Returned(p)) = self.return_rx.try_recv() {
            if let Some(m) = self.cell.get() {
                unsafe {
                    p.setProcessing(0);
                    m.component.setActive(0);
                }
            }
            self.active = None;
        }
    }

    /// Activate for `spec` and produce the RT half (or a carrying shell while one is out).
    pub fn activate_for_rt(&mut self, spec: &PrepareSpec) -> Result<Vst3Processor, String> {
        self.collect_returns();
        let wanted = (spec.sample_rate, spec.max_frames);
        if let Some(active) = self.active
            && active != wanted
        {
            return Err("plugin is still active at another sample rate or block size; try again after the engine switched".into());
        }
        let make_shell = |this: &Self, processor: Option<ComPtr<IAudioProcessor>>| Vst3Processor::new(this, spec, processor);
        if self.active.is_some() {
            return Ok(make_shell(self, None));
        }
        let m = self.cell.get().ok_or("plugin instance used off its main thread")?;
        unsafe {
            let mut ins: Vec<SpeakerArrangement> = self.layout.inputs.iter().map(|_| SpeakerArr::kStereo).collect();
            let mut outs: Vec<SpeakerArrangement> = self.layout.outputs.iter().map(|_| SpeakerArr::kStereo).collect();
            let r = m.processor.setBusArrangements(ins.as_mut_ptr(), ins.len() as i32, outs.as_mut_ptr(), outs.len() as i32);
            if r != kResultTrue && r != kResultOk {
                return Err("plugin refuses stereo buses".into());
            }
            if m.processor.canProcessSampleSize(SymbolicSampleSizes_::kSample32 as i32) != kResultOk {
                return Err("plugin cannot process 32-bit float audio".into());
            }
            let mut setup = ProcessSetup {
                processMode: ProcessModes_::kRealtime as i32,
                symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
                maxSamplesPerBlock: spec.max_frames as i32,
                sampleRate: spec.sample_rate,
            };
            if m.processor.setupProcessing(&mut setup) != kResultOk {
                return Err("setupProcessing failed".into());
            }
            for (i, _) in self.layout.inputs.iter().enumerate() {
                m.component.activateBus(MediaTypes_::kAudio as MediaType, BusDirections_::kInput as BusDirection, i as i32, 1);
            }
            for (i, _) in self.layout.outputs.iter().enumerate() {
                m.component.activateBus(MediaTypes_::kAudio as MediaType, BusDirections_::kOutput as BusDirection, i as i32, 1);
            }
            if self.layout.event_input {
                m.component.activateBus(MediaTypes_::kEvent as MediaType, BusDirections_::kInput as BusDirection, 0, 1);
            }
            if m.component.setActive(1) != kResultOk {
                return Err("setActive failed".into());
            }
            if m.processor.setProcessing(1) != kResultOk {
                m.component.setActive(0);
                return Err("setProcessing failed".into());
            }
            self.latency = m.processor.getLatencySamples();
        }
        let processor = m.processor.clone();
        self.active = Some(wanted);
        let mut p = make_shell(self, Some(processor));
        p.latency = self.latency;
        // sets made while inactive reach the audio half with the first block
        for (id, v) in self.pending.drain(..) {
            p.set_param(ParamId(id), v);
        }
        Ok(p)
    }

    /// Deliver parameter sets made while no realtime half exists: VST3 components only
    /// learn parameters through `process`, so an inactive instance is activated briefly and
    /// processed with zero samples (the spec's parameter-flush call), then deactivated.
    pub fn flush_pending(&mut self) {
        if self.pending.is_empty() || self.active.is_some() {
            return;
        }
        let Some(m) = self.cell.get() else { return };
        let wrappers: Vec<ComWrapper<HostQueue>> =
            self.pending.iter().map(|&(id, v)| ComWrapper::new(HostQueue { id, points: UnsafeCell::new(vec![(0, f64::from(v))]) })).collect();
        let queues: Vec<ComPtr<IParamValueQueue>> = wrappers.iter().filter_map(|w| w.to_com_ptr::<IParamValueQueue>()).collect();
        let active: Vec<usize> = (0..queues.len()).collect();
        let changes = ComWrapper::new(HostChanges { queues, wrappers, active: UnsafeCell::new(active) });
        let sink = ComWrapper::new(SinkChanges);
        let (Some(changes_ptr), Some(sink_ptr)) = (changes.to_com_ptr::<IParameterChanges>(), sink.to_com_ptr::<IParameterChanges>()) else { return };
        unsafe {
            let mut setup = ProcessSetup {
                processMode: ProcessModes_::kOffline as i32,
                symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
                maxSamplesPerBlock: 64,
                sampleRate: 48_000.0,
            };
            if m.processor.setupProcessing(&mut setup) != kResultOk || m.component.setActive(1) != kResultOk {
                return;
            }
            if m.processor.setProcessing(1) == kResultOk {
                let mut data: ProcessData = std::mem::zeroed();
                data.processMode = ProcessModes_::kOffline as i32;
                data.symbolicSampleSize = SymbolicSampleSizes_::kSample32 as i32;
                data.numSamples = 0;
                data.inputParameterChanges = changes_ptr.as_ptr();
                data.outputParameterChanges = sink_ptr.as_ptr();
                let _ = m.processor.process(&mut data);
                m.processor.setProcessing(0);
            }
            m.component.setActive(0);
        }
        self.pending.clear();
    }

    /// Component (and controller) state bytes (pending parameter sets are flushed first).
    pub fn plugin_state(&mut self) -> Option<(Vec<u8>, Vec<u8>)> {
        self.flush_pending();
        self.read_plugin_state()
    }

    fn read_plugin_state(&self) -> Option<(Vec<u8>, Vec<u8>)> {
        let m = self.cell.get()?;
        let s = MemStream::new(Vec::new());
        let p = s.to_com_ptr::<IBStream>()?;
        if unsafe { m.component.getState(p.as_ptr()) } != kResultOk {
            return None;
        }
        let component = s.take();
        let s2 = MemStream::new(Vec::new());
        let p2 = s2.to_com_ptr::<IBStream>()?;
        let controller = if unsafe { m.controller.getState(p2.as_ptr()) } == kResultOk { s2.take() } else { Vec::new() };
        Some((component, controller))
    }

    pub fn load_plugin_state(&mut self, component: &[u8], controller: Option<&[u8]>) -> Result<(), String> {
        if component.len() > MAX_STATE_BYTES || controller.is_some_and(|c| c.len() > MAX_STATE_BYTES) {
            return Err("plugin state exceeds the size limit".into());
        }
        let m = self.cell.get().ok_or("plugin instance used off its main thread")?;
        let s = MemStream::new(component.to_vec());
        let p = s.to_com_ptr::<IBStream>().ok_or("stream")?;
        if unsafe { m.component.setState(p.as_ptr()) } != kResultOk {
            return Err("plugin rejected the component state".into());
        }
        let s2 = MemStream::new(component.to_vec());
        if let Some(p2) = s2.to_com_ptr::<IBStream>() {
            unsafe { m.controller.setComponentState(p2.as_ptr()) };
        }
        if let Some(c) = controller
            && !c.is_empty()
        {
            let s3 = MemStream::new(c.to_vec());
            if let Some(p3) = s3.to_com_ptr::<IBStream>() {
                unsafe { m.controller.setState(p3.as_ptr()) };
            }
        }
        self.refresh_values();
        Ok(())
    }
}

impl Processor for Vst3Instance {
    fn type_id(&self) -> &'static str {
        self.type_id
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn descriptors(&self) -> &'static [ParamDescriptor] {
        self.descriptors
    }
    fn prepare(&mut self, _spec: &PrepareSpec) -> Result<(), PrepareError> {
        Ok(())
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        ctx.audio.copy_input_to_output();
    }
    fn reset(&mut self) {}
    fn latency_frames(&self) -> u32 {
        self.latency
    }
    fn tail(&self) -> Tail {
        Tail::Unknown
    }
    fn param(&self, id: ParamId) -> Option<f32> {
        self.descriptors.iter().position(|d| d.id == id).map(|i| self.values[i])
    }
    fn param_text(&self, id: ParamId, normalized: f32) -> Option<String> {
        let i = self.descriptors.iter().position(|d| d.id == id)?;
        let units = self.units.get(i).copied().unwrap_or("");
        self.texts.get_or(id.0, normalized, || {
            let m = self.cell.get()?;
            let mut text: String128 = [0; 128];
            // SAFETY: main thread (MainThreadCell), live controller, `text` is a valid String128.
            if unsafe { m.controller.getParamStringByValue(id.0, f64::from(normalized.clamp(0.0, 1.0)), &mut text) } != kResultOk {
                return None;
            }
            vst3_readout(&wstr_field(&text), units)
        })
    }
    fn set_param(&mut self, id: ParamId, normalized: f32) -> bool {
        let Some(i) = self.descriptors.iter().position(|d| d.id == id) else { return false };
        let v = normalized.clamp(0.0, 1.0);
        self.values[i] = v;
        if let Some(m) = self.cell.get() {
            unsafe { m.controller.setParamNormalized(id.0, f64::from(v)) };
        }
        if self.active.is_none() && self.pending.len() < MAX_POINTS {
            self.pending.retain(|(pid, _)| *pid != id.0);
            self.pending.push((id.0, v));
        }
        true
    }
    fn save_state(&self) -> Vec<u8> {
        let Some((component, controller)) = self.read_plugin_state() else { return Vec::new() };
        let mut env = serde_json::json!({ "format": "vst3", "plugin_id": self.plugin_id, COMPONENT_STATE_KEY: crate::b64::encode(&component), CONTROLLER_STATE_KEY: crate::b64::encode(&controller) });
        if !self.pending.is_empty() {
            // sets the component has not received yet (no flush happened): kept with the state
            env["pending"] = serde_json::Value::Object(self.pending.iter().map(|(id, v)| (id.to_string(), serde_json::json!(v))).collect());
        }
        serde_json::to_vec(&env).unwrap_or_default()
    }
    fn load_state(&mut self, bytes: &[u8]) -> Result<(), String> {
        let v: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| format!("external state envelope is not JSON: {e}"))?;
        let Some(obj) = v.as_object() else { return Err("external state envelope is not an object".into()) };
        if let Some(id) = obj.get("plugin_id").and_then(|p| p.as_str())
            && id != self.plugin_id
        {
            return Err(format!("state belongs to {id}, not {}", self.plugin_id));
        }
        let Some(field) = obj.get(COMPONENT_STATE_KEY) else { return Ok(()) };
        let Some(c) = field.as_str() else { return Err(crate::clap::NOT_INLINE.into()) };
        let component = crate::b64::decode(c)?;
        let controller = match obj.get(CONTROLLER_STATE_KEY) {
            None => None,
            Some(v) => Some(crate::b64::decode(v.as_str().ok_or(crate::clap::NOT_INLINE)?)?),
        };
        self.texts.clear();
        self.load_plugin_state(&component, controller.as_deref())?;
        if let Some(pending) = obj.get("pending").and_then(|p| p.as_object()) {
            for (id, v) in pending {
                if let (Ok(id), Some(v)) = (id.parse::<u32>(), v.as_f64()) {
                    self.set_param(ParamId(id), v as f32);
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// realtime half

/// Realtime half of a hosted VST3 plugin.
pub struct Vst3Processor {
    processor: Option<ComPtr<IAudioProcessor>>,
    return_tx: Sender<Returned>,
    type_id: &'static str,
    descriptors: &'static [ParamDescriptor],
    layout: Vst3Layout,
    in_bufs: Vec<Vec<f32>>,
    out_bufs: Vec<Vec<f32>>,
    in_ptrs: Vec<Vec<*mut f32>>,
    out_ptrs: Vec<Vec<*mut f32>>,
    in_buses: Vec<AudioBusBuffers>,
    out_buses: Vec<AudioBusBuffers>,
    events_in: ComWrapper<HostEvents>,
    events_out: ComWrapper<HostEvents>,
    changes: ComWrapper<HostChanges>,
    sink: ComWrapper<SinkChanges>,
    /// Descriptor index → queue index (same order).
    pending: Vec<(u32, f32)>,
    values: Vec<f32>,
    steady_frame: i64,
    latency: u32,
    sample_rate: f64,
    max_frames: usize,
    problems: Arc<std::sync::atomic::AtomicU32>,
}

// SAFETY: raw channel pointers point into our own scratch vectors (owned by this struct) and
// are rebuilt per block; the plan moves this struct to the audio thread as a whole.
unsafe impl Send for Vst3Processor {}

impl Vst3Processor {
    fn new(inst: &Vst3Instance, spec: &PrepareSpec, processor: Option<ComPtr<IAudioProcessor>>) -> Self {
        let n = spec.max_frames;
        let mk = |layout: &[u32]| -> (Vec<Vec<f32>>, Vec<Vec<*mut f32>>) {
            let mut bufs = Vec::new();
            let mut ptrs = Vec::new();
            for &ch in layout {
                let mut p = Vec::new();
                for _ in 0..ch {
                    bufs.push(vec![0.0f32; n]);
                    p.push(std::ptr::null_mut());
                }
                ptrs.push(p);
            }
            (bufs, ptrs)
        };
        let (in_bufs, in_ptrs) = mk(&inst.layout.inputs);
        let (out_bufs, out_ptrs) = mk(&inst.layout.outputs);
        let in_buses: Vec<AudioBusBuffers> = inst.layout.inputs.iter().map(|_| unsafe { std::mem::zeroed() }).collect();
        let out_buses: Vec<AudioBusBuffers> = inst.layout.outputs.iter().map(|_| unsafe { std::mem::zeroed() }).collect();
        let wrappers: Vec<ComWrapper<HostQueue>> =
            inst.descriptors.iter().map(|d| ComWrapper::new(HostQueue { id: d.id.0, points: UnsafeCell::new(Vec::with_capacity(64)) })).collect();
        let queues: Vec<ComPtr<IParamValueQueue>> = wrappers.iter().filter_map(|w| w.to_com_ptr::<IParamValueQueue>()).collect();
        let mut p = Self {
            processor,
            return_tx: inst.return_tx.clone(),
            type_id: inst.type_id,
            descriptors: inst.descriptors,
            layout: inst.layout.clone(),
            in_bufs,
            out_bufs,
            in_ptrs,
            out_ptrs,
            in_buses,
            out_buses,
            events_in: ComWrapper::new(HostEvents { events: UnsafeCell::new(Vec::with_capacity(MAX_EVENTS)) }),
            events_out: ComWrapper::new(HostEvents { events: UnsafeCell::new(Vec::with_capacity(MAX_EVENTS)) }),
            changes: ComWrapper::new(HostChanges { queues, wrappers, active: UnsafeCell::new(Vec::with_capacity(inst.descriptors.len().max(1))) }),
            sink: ComWrapper::new(SinkChanges),
            pending: Vec::with_capacity(MAX_POINTS),
            values: inst.values.clone(),
            steady_frame: 0,
            latency: inst.latency,
            sample_rate: spec.sample_rate,
            max_frames: n,
            problems: inst.problems.clone(),
        };
        p.rewire(n);
        p
    }

    /// Point the bus structures at the scratch buffers (after construction/migration).
    fn rewire(&mut self, _frames: usize) {
        let mut k = 0;
        for (b, ptrs) in self.in_ptrs.iter_mut().enumerate() {
            for p in ptrs.iter_mut() {
                *p = self.in_bufs[k].as_mut_ptr();
                k += 1;
            }
            self.in_buses[b].numChannels = ptrs.len() as i32;
            self.in_buses[b].silenceFlags = 0;
            self.in_buses[b].__field0.channelBuffers32 = ptrs.as_mut_ptr();
        }
        k = 0;
        for (b, ptrs) in self.out_ptrs.iter_mut().enumerate() {
            for p in ptrs.iter_mut() {
                *p = self.out_bufs[k].as_mut_ptr();
                k += 1;
            }
            self.out_buses[b].numChannels = ptrs.len() as i32;
            self.out_buses[b].silenceFlags = 0;
            self.out_buses[b].__field0.channelBuffers32 = ptrs.as_mut_ptr();
        }
    }

    fn push_events(&mut self, events: &[TimedEvent], frames: usize) {
        // SAFETY: only the audio thread touches these cells, and only inside `process`.
        let list = unsafe { &mut *self.events_in.events.get() };
        list.clear();
        let active = unsafe { &mut *self.changes.active.get() };
        active.clear();
        for q in &self.changes.wrappers {
            unsafe { (*q.points.get()).clear() };
        }
        let push_point = |this: &Self, active: &mut Vec<usize>, id: u32, offset: i32, value: f64| {
            if let Some(i) = this.descriptors.iter().position(|d| d.id.0 == id) {
                let points = unsafe { &mut *this.changes.wrappers[i].points.get() };
                if points.len() < 64 {
                    if points.is_empty() {
                        active.push(i);
                    }
                    points.push((offset, value));
                }
            }
        };
        for &(id, v) in &self.pending {
            push_point(self, active, id, 0, f64::from(v));
        }
        self.pending.clear();
        for e in events {
            let t = (e.frame as usize).min(frames.saturating_sub(1)) as i32;
            let ch = i16::from(e.channel & 0x0F);
            match e.kind {
                EventKind::NoteOn { note_id, pitch, velocity } if self.layout.event_input => {
                    if list.len() >= MAX_EVENTS {
                        continue;
                    }
                    let mut ev: Event = unsafe { std::mem::zeroed() };
                    ev.sampleOffset = t;
                    ev.r#type = Event_::EventTypes_::kNoteOnEvent as u16;
                    ev.__field0.noteOn = NoteOnEvent { channel: ch, pitch: i16::from(pitch), tuning: 0.0, velocity, length: 0, noteId: (note_id & 0x7FFF_FFFF) as i32 };
                    list.push(ev);
                }
                EventKind::NoteOff { note_id, pitch } if self.layout.event_input => {
                    if list.len() >= MAX_EVENTS {
                        continue;
                    }
                    let mut ev: Event = unsafe { std::mem::zeroed() };
                    ev.sampleOffset = t;
                    ev.r#type = Event_::EventTypes_::kNoteOffEvent as u16;
                    ev.__field0.noteOff = NoteOffEvent { channel: ch, pitch: i16::from(pitch), velocity: 0.0, noteId: (note_id & 0x7FFF_FFFF) as i32, tuning: 0.0 };
                    list.push(ev);
                }
                EventKind::NotePressure { note_id, value } if self.layout.event_input => {
                    if list.len() >= MAX_EVENTS {
                        continue;
                    }
                    let mut ev: Event = unsafe { std::mem::zeroed() };
                    ev.sampleOffset = t;
                    ev.r#type = Event_::EventTypes_::kPolyPressureEvent as u16;
                    ev.__field0.polyPressure = PolyPressureEvent { channel: ch, pitch: -1, pressure: value, noteId: (note_id & 0x7FFF_FFFF) as i32 };
                    list.push(ev);
                }
                EventKind::Param { id, value } => {
                    if let Some(i) = self.descriptors.iter().position(|d| d.id == id) {
                        self.values[i] = value.clamp(0.0, 1.0);
                    }
                    push_point(self, active, id.0, t, f64::from(value.clamp(0.0, 1.0)));
                }
                _ => {}
            }
        }
    }
}

impl Processor for Vst3Processor {
    fn type_id(&self) -> &'static str {
        self.type_id
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn carries_live_state(&self) -> bool {
        true
    }
    fn migrate_from(&mut self, old: &mut dyn Processor) {
        if let Some(prev) = old.as_any_mut().downcast_mut::<Vst3Processor>()
            && self.processor.is_none()
            && prev.sample_rate == self.sample_rate
            && prev.max_frames == self.max_frames
        {
            self.processor = prev.processor.take();
            self.steady_frame = prev.steady_frame;
            self.latency = prev.latency;
        }
    }
    fn descriptors(&self) -> &'static [ParamDescriptor] {
        self.descriptors
    }
    fn prepare(&mut self, spec: &PrepareSpec) -> Result<(), PrepareError> {
        if spec.sample_rate != self.sample_rate {
            return Err(PrepareError::UnsupportedSampleRate);
        }
        if spec.max_frames > self.max_frames {
            return Err(PrepareError::BlockTooLarge { max: self.max_frames });
        }
        Ok(())
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let frames = ctx.audio.frames.min(self.max_frames);
        if self.processor.is_none() {
            ctx.audio.copy_input_to_output();
            return;
        }
        self.push_events(ctx.events, frames);
        // inputs: main bus ← chain, bus 1 ← sidechain when fed, others silence
        let mut k = 0usize;
        for (bi, &chans) in self.layout.inputs.iter().enumerate() {
            let src: Option<(&[f32], &[f32])> = match bi {
                0 if ctx.audio.inputs.len() >= 2 => Some((&ctx.audio.inputs[0][..frames], &ctx.audio.inputs[1][..frames])),
                0 if ctx.audio.inputs.len() == 1 => Some((&ctx.audio.inputs[0][..frames], &ctx.audio.inputs[0][..frames])),
                1 if ctx.audio.sidechain.len() >= 2 => Some((&ctx.audio.sidechain[0][..frames], &ctx.audio.sidechain[1][..frames])),
                _ => None,
            };
            for c in 0..chans as usize {
                let buf = &mut self.in_bufs[k + c][..frames];
                match (src, chans, c) {
                    (Some((l, r)), 1, _) => {
                        for i in 0..frames {
                            buf[i] = 0.5 * (l[i] + r[i]);
                        }
                    }
                    (Some((l, _)), _, 0) => buf.copy_from_slice(l),
                    (Some((_, r)), _, 1) => buf.copy_from_slice(r),
                    _ => buf.fill(0.0),
                }
            }
            k += chans as usize;
        }
        for b in &mut self.out_bufs {
            b[..frames].fill(0.0);
        }
        let t = ctx.transport;
        let mut pc: vst3::Steinberg::Vst::ProcessContext = unsafe { std::mem::zeroed() };
        pc.state = vst3::Steinberg::Vst::ProcessContext_::StatesAndFlags_::kTempoValid
            | vst3::Steinberg::Vst::ProcessContext_::StatesAndFlags_::kTimeSigValid
            | vst3::Steinberg::Vst::ProcessContext_::StatesAndFlags_::kProjectTimeMusicValid
            | vst3::Steinberg::Vst::ProcessContext_::StatesAndFlags_::kContTimeValid;
        if t.playing {
            pc.state |= vst3::Steinberg::Vst::ProcessContext_::StatesAndFlags_::kPlaying;
        }
        pc.sampleRate = self.sample_rate;
        pc.projectTimeSamples = t.block_start_frame.max(0);
        pc.continousTimeSamples = self.steady_frame;
        pc.projectTimeMusic = t.position_quarters;
        pc.tempo = t.tempo_bpm;
        pc.timeSigNumerator = i32::from(t.meter_numerator.max(1));
        pc.timeSigDenominator = i32::from(t.meter_denominator.max(1));
        let Some(events_in) = self.events_in.to_com_ptr::<IEventList>() else { return };
        let Some(events_out) = self.events_out.to_com_ptr::<IEventList>() else { return };
        let Some(changes) = self.changes.to_com_ptr::<IParameterChanges>() else { return };
        let Some(sink) = self.sink.to_com_ptr::<IParameterChanges>() else { return };
        unsafe { (*self.events_out.events.get()).clear() };
        let mut data: ProcessData = unsafe { std::mem::zeroed() };
        data.processMode = ProcessModes_::kRealtime as i32;
        data.symbolicSampleSize = SymbolicSampleSizes_::kSample32 as i32;
        data.numSamples = frames as i32;
        data.numInputs = self.in_buses.len() as i32;
        data.numOutputs = self.out_buses.len() as i32;
        data.inputs = self.in_buses.as_mut_ptr();
        data.outputs = self.out_buses.as_mut_ptr();
        data.inputParameterChanges = changes.as_ptr();
        data.outputParameterChanges = sink.as_ptr();
        data.inputEvents = events_in.as_ptr();
        data.outputEvents = events_out.as_ptr();
        data.processContext = &mut pc;
        let r = unsafe { self.processor.as_ref().expect("checked above").process(&mut data) };
        self.steady_frame = self.steady_frame.wrapping_add(frames as i64);
        let mut bad = r != kResultOk;
        let main_out = self.layout.outputs.first().copied().unwrap_or(0) as usize;
        for (oi, out) in ctx.audio.outputs.iter_mut().enumerate() {
            let src = if main_out == 0 { None } else { Some(&self.out_bufs[oi.min(main_out - 1)][..frames]) };
            match src {
                Some(s) if !bad => out[..frames].copy_from_slice(s),
                _ => out[..frames].fill(0.0),
            }
        }
        if !bad && ctx.audio.outputs.iter().any(|o| o[..frames].iter().any(|v| !v.is_finite())) {
            bad = true;
            for o in ctx.audio.outputs.iter_mut() {
                o[..frames].fill(0.0);
            }
        }
        if bad {
            self.problems.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
    fn reset(&mut self) {
        self.pending.clear();
    }
    fn latency_frames(&self) -> u32 {
        self.latency
    }
    fn tail(&self) -> Tail {
        Tail::Unknown
    }
    fn param(&self, id: ParamId) -> Option<f32> {
        self.descriptors.iter().position(|d| d.id == id).map(|i| self.values[i])
    }
    fn set_param(&mut self, id: ParamId, normalized: f32) -> bool {
        let Some(i) = self.descriptors.iter().position(|d| d.id == id) else { return false };
        self.values[i] = normalized.clamp(0.0, 1.0);
        if self.pending.len() < MAX_POINTS {
            self.pending.push((id.0, self.values[i]));
        }
        true
    }
    fn save_state(&self) -> Vec<u8> {
        Vec::new()
    }
    fn load_state(&mut self, _bytes: &[u8]) -> Result<(), String> {
        Err("the realtime half of a plugin has no state; use the main-thread instance".into())
    }
}

impl Drop for Vst3Processor {
    fn drop(&mut self) {
        if let Some(p) = self.processor.take() {
            let _ = self.return_tx.send(Returned(p));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::vst3_readout;

    #[test]
    fn unit_labels_follow_bare_numbers_only() {
        assert_eq!(vst3_readout("-6.0", "dB").as_deref(), Some("-6.0 dB"));
        assert_eq!(vst3_readout("-6.0 dB", "dB").as_deref(), Some("-6.0 dB"));
        assert_eq!(vst3_readout("1.20 kHz", "Hz").as_deref(), Some("1.20 kHz"));
        assert_eq!(vst3_readout("Off", "Hz").as_deref(), Some("Off"));
        assert_eq!(vst3_readout("4.00:1", ":1").as_deref(), Some("4.00:1"));
        assert_eq!(vst3_readout("0.500", "").as_deref(), Some("0.500"));
        assert_eq!(vst3_readout("  ", "dB"), None);
    }
}

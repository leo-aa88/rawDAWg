//! In-process CLAP bridge (PLUGIN_API.md §3, PLUG-02).
//!
//! One plugin instance per project instance, split the way the ABI wants:
//! [`ClapInstance`] owns the main-thread half (`PluginInstance`: parameters,
//! state, latency, activation) and lives in the app's mirror map as a
//! `Box<dyn Processor>`; [`ClapProcessor`] owns the audio half
//! (`StartedPluginAudioProcessor`) inside the realtime plan. The audio half is
//! created by [`ClapInstance::activate_for_rt`] during plan compilation (main
//! thread), moves to the RT thread with the plan, migrates between plans without
//! re-activation, and returns through a channel when the retired plan is dropped
//! (off RT) so the main thread can deactivate it.
//!
//! Third-party plugins are untrusted native code: a crash or hang inside
//! `process` cannot be contained in-process (PLUGIN_API.md §3); non-finite output
//! and processing errors are caught, zeroed and counted.

use crate::catalog::PluginDescription;
use crate::hosted::{MAX_PARAM_TEXT_CHARS, ParamTextCache};
use crate::main_thread::MainThreadCell;
use clack_extensions::audio_ports::{AudioPortInfoBuffer, PluginAudioPorts};
use clack_extensions::latency::PluginLatency;
use clack_extensions::note_ports::{NoteDialects, NotePortInfoBuffer, PluginNotePorts};
use clack_extensions::params::{ParamInfoBuffer, ParamInfoFlags, PluginParams};
use clack_extensions::state::PluginState;
use clack_host::events::event_types::{MidiEvent, NoteExpressionEvent, NoteExpressionType, NoteOffEvent, NoteOnEvent, ParamValueEvent, TransportEvent, TransportFlags};
use clack_host::events::{EventFlags, EventHeader, Match};
use clack_host::prelude::*;
use clack_host::utils::{BeatTime, SecondsTime};
use rawdawg_dsp::processor::{PrepareError, PrepareSpec, ProcessContext, Processor, Tail, sanitize_display_text};
use rawdawg_types::events::{EventKind, NoteExpression, TimedEvent};
use rawdawg_types::params::ParamCategory;
use rawdawg_types::{ParamDescriptor, ParamId, ParamMapping, ParamUnit};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};

use crate::host::{MainThread, Shared};

/// Host handlers used for live instances.
pub struct RawdawgHost;

impl HostHandlers for RawdawgHost {
    type Shared<'a> = Shared;
    type MainThread<'a> = MainThread;
    type AudioProcessor<'a> = ();
}

/// Maximum events forwarded to a plugin per block (bounded, preallocated).
pub const MAX_EVENTS: usize = 4096;
/// Maximum parameter sets queued from the non-RT `set_param` path between blocks.
const MAX_PENDING_PARAMS: usize = 512;
/// Opaque state larger than this is refused (PROJECT_FORMAT.md: 64 MiB per instance).
pub const MAX_STATE_BYTES: usize = 64 * 1024 * 1024;
/// Parameters above this count are not exposed as descriptors (the plugin's GUI still works).
pub const MAX_PARAMS: usize = 4096;

/// A loaded module, shared by every instance created from it (keeps the library mapped).
pub struct LoadedModule {
    pub path: PathBuf,
    entry: PluginEntry,
}

/// Per-process cache of loaded modules by canonical path.
#[derive(Default)]
pub struct ModuleCache {
    modules: HashMap<PathBuf, Arc<LoadedModule>>,
}

impl ModuleCache {
    /// Load (or reuse) the module at `path`.
    ///
    /// # Safety
    /// Executes native code from `path`. Only call for modules the user chose to use
    /// (catalog entries that passed the isolated scan).
    pub unsafe fn load(&mut self, path: &Path) -> Result<Arc<LoadedModule>, String> {
        let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if let Some(m) = self.modules.get(&key) {
            return Ok(m.clone());
        }
        // SAFETY: forwarded from the caller's contract.
        let entry = unsafe { PluginEntry::load(path) }.map_err(|e| format!("cannot load {}: {e}", path.display()))?;
        let m = Arc::new(LoadedModule { path: key.clone(), entry });
        self.modules.insert(key, m.clone());
        Ok(m)
    }

    pub fn loaded(&self) -> impl Iterator<Item = &Arc<LoadedModule>> {
        self.modules.values()
    }
}

/// Static parameter descriptors for one plugin id, leaked once per process so they can be
/// handed out as `&'static` (bounded by the number of distinct plugins ever instantiated).
fn descriptor_cache() -> &'static Mutex<HashMap<String, &'static [ParamDescriptor]>> {
    static CACHE: OnceLock<Mutex<HashMap<String, &'static [ParamDescriptor]>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn leak_str(s: &str) -> &'static str {
    static CACHE: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut c = cache.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(v) = c.get(s) {
        return v;
    }
    let leaked: &'static str = Box::leak(s.to_owned().into_boxed_str());
    c.insert(s.to_owned(), leaked);
    leaked
}

/// Internal processor type id of a hosted CLAP plugin (`clap:<plugin id>`).
pub fn type_id_for(plugin_id: &str) -> &'static str {
    leak_str(&format!("clap:{plugin_id}"))
}

/// Build (or fetch) the descriptor table for a plugin from its live parameter info.
fn descriptors_for(plugin_id: &str, handle: &PluginMainThreadHandle, params: Option<&PluginParams>) -> &'static [ParamDescriptor] {
    let mut cache = descriptor_cache().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(d) = cache.get(plugin_id) {
        return d;
    }
    let mut out = Vec::new();
    if let Some(params) = params {
        let count = (params.count(handle) as usize).min(MAX_PARAMS);
        for i in 0..count as u32 {
            let mut buf = ParamInfoBuffer::new();
            let Some(info) = params.get_info(handle, i, &mut buf) else { continue };
            let name = leak_str(&String::from_utf8_lossy(info.name));
            let module = String::from_utf8_lossy(info.module);
            let key =
                leak_str(&if module.is_empty() { format!("p{}", info.id.get()) } else { format!("{}.{}", module.replace(['/', ' '], "_").to_ascii_lowercase(), info.id.get()) });
            let (min, max) =
                if info.min_value.is_finite() && info.max_value.is_finite() && info.max_value > info.min_value { (info.min_value, info.max_value) } else { (0.0, 1.0) };
            let default = (((info.default_value - min) / (max - min)) as f32).clamp(0.0, 1.0);
            let mapping = stepped_mapping(info.flags.contains(ParamInfoFlags::IS_STEPPED), min, max);
            let mut d = ParamDescriptor::continuous(info.id.get(), key, name, ParamUnit::None, mapping, if default.is_finite() { default } else { 0.0 }, ParamCategory::Effects);
            d.flags.automatable = info.flags.contains(ParamInfoFlags::IS_AUTOMATABLE);
            d.flags.modulatable = info.flags.contains(ParamInfoFlags::IS_MODULATABLE);
            d.flags.stepped = info.flags.contains(ParamInfoFlags::IS_STEPPED);
            d.flags.hidden = info.flags.contains(ParamInfoFlags::IS_HIDDEN);
            d.flags.read_only = info.flags.contains(ParamInfoFlags::IS_READONLY);
            d.flags.is_bypass = info.flags.contains(ParamInfoFlags::IS_BYPASS);
            out.push(d);
        }
    }
    let leaked: &'static [ParamDescriptor] = Box::leak(out.into_boxed_slice());
    cache.insert(plugin_id.to_owned(), leaked);
    leaked
}

/// Largest integer range shown as discrete steps; wider stepped ranges stay linear (rounded).
pub const MAX_DISCRETE_STEPS: u32 = 1024;

/// Mapping for a plugin parameter's plain range. CLAP stepped parameters take integer
/// values: 0..1 is a switch, 0..N (N ≤ [`MAX_DISCRETE_STEPS`] − 1) a choice of N + 1 steps,
/// anything else a linear range that is rounded when sent.
fn stepped_mapping(stepped: bool, min: f64, max: f64) -> ParamMapping {
    if stepped && min == 0.0 && max <= 1.0 {
        ParamMapping::Toggle
    } else if stepped && min == 0.0 && max.fract() == 0.0 && max < f64::from(MAX_DISCRETE_STEPS) {
        ParamMapping::Discrete { steps: max as u32 + 1 }
    } else {
        ParamMapping::Linear { min, max }
    }
}

/// Audio port layout as declared by the plugin (channel counts per port).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PortLayout {
    pub inputs: Vec<u32>,
    pub outputs: Vec<u32>,
    pub note_inputs: u32,
    pub accepts_clap_notes: bool,
    pub accepts_midi: bool,
}

/// What the RT half returns when its plan is dropped.
struct Returned(StartedPluginAudioProcessor<RawdawgHost>);

/// Main-thread half of a hosted CLAP plugin; also the app's parameter/state mirror.
pub struct ClapInstance {
    module: Arc<LoadedModule>,
    cell: MainThreadCell<RefCell<PluginInstance<RawdawgHost>>>,
    plugin_id: String,
    type_id: &'static str,
    descriptors: &'static [ParamDescriptor],
    layout: PortLayout,
    /// Cached normalized values (refreshed from the plugin on the main thread).
    values: Vec<f32>,
    /// Readouts from the plugin's `value_to_text`.
    texts: ParamTextCache,
    /// Activation currently out on the RT thread, if any.
    active: Option<(f64, usize)>,
    latency: u32,
    return_tx: Sender<Returned>,
    return_rx: Receiver<Returned>,
    pub info: PluginDescription,
    /// Processing problems reported by RT halves (non-finite output, process errors).
    pub problems: Arc<std::sync::atomic::AtomicU32>,
}

impl ClapInstance {
    /// Instantiate `plugin_id` from `module` (main thread). Does not activate.
    pub fn new(module: Arc<LoadedModule>, info: &PluginDescription) -> Result<Self, String> {
        let plugin_id = info.plugin_id.clone();
        let id_c = std::ffi::CString::new(plugin_id.as_str()).map_err(|_| "plugin id contains NUL".to_owned())?;
        let mut instance = PluginInstance::<RawdawgHost>::new(|_| Shared::default(), |_| MainThread, &module.entry, &id_c, &crate::host::host_info())
            .map_err(|e| format!("cannot instantiate {plugin_id}: {e}"))?;
        let (descriptors, layout, latency, values) = {
            let mut handle = instance.plugin_handle();
            let params = handle.get_extension::<PluginParams>();
            let descriptors = descriptors_for(&plugin_id, &handle, params.as_ref());
            let mut layout = PortLayout::default();
            if let Some(ports) = handle.get_extension::<PluginAudioPorts>() {
                for is_input in [true, false] {
                    let count = ports.count(&handle, is_input).min(16);
                    for i in 0..count {
                        let mut buf = AudioPortInfoBuffer::new();
                        let ch = ports.get(&handle, i, is_input, &mut buf).map(|p| p.channel_count).unwrap_or(0).min(64);
                        if is_input { layout.inputs.push(ch) } else { layout.outputs.push(ch) }
                    }
                }
            }
            if let Some(notes) = handle.get_extension::<PluginNotePorts>() {
                layout.note_inputs = notes.count(&handle, true);
                let mut buf = NotePortInfoBuffer::new();
                if let Some(p) = notes.get(&handle, 0, true, &mut buf) {
                    layout.accepts_clap_notes = p.supported_dialects.contains(NoteDialects::CLAP);
                    layout.accepts_midi = p.supported_dialects.contains(NoteDialects::MIDI);
                }
            }
            let latency = handle.get_extension::<PluginLatency>().map(|l| l.get(&handle)).unwrap_or(0);
            let values: Vec<f32> = descriptors
                .iter()
                .map(|d| params.as_ref().and_then(|p| p.get_value(&handle, ClapId::new(d.id.0))).map(|v| to_normalized(d, v)).unwrap_or(d.default_normalized))
                .collect();
            let _ = &mut handle;
            (descriptors, layout, latency, values)
        };
        let (return_tx, return_rx) = channel();
        Ok(Self {
            module,
            cell: MainThreadCell::new(RefCell::new(instance)),
            type_id: type_id_for(&plugin_id),
            plugin_id,
            descriptors,
            layout,
            values,
            texts: ParamTextCache::default(),
            active: None,
            latency,
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
    pub fn layout(&self) -> &PortLayout {
        &self.layout
    }
    pub fn is_instrument(&self) -> bool {
        self.info.is_instrument
    }
    /// Whether an audio half is currently out on the RT thread.
    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }
    pub fn problem_count(&self) -> u32 {
        self.problems.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Run `f` with the main-thread handle (owner thread only; `None` elsewhere).
    fn with_handle<R>(&self, f: impl FnOnce(&mut PluginMainThreadHandle) -> R) -> Option<R> {
        let inst = self.cell.get()?;
        let mut inst = inst.borrow_mut();
        let mut handle = inst.plugin_handle();
        Some(f(&mut handle))
    }

    /// Run `f` with the plugin instance itself (owner thread only).
    fn with_instance<R>(&self, f: impl FnOnce(&mut PluginInstance<RawdawgHost>) -> R) -> Option<R> {
        let inst = self.cell.get()?;
        Some(f(&mut inst.borrow_mut()))
    }

    /// Refresh the cached parameter values from the plugin (main thread; call at UI rate).
    pub fn refresh_values(&mut self) {
        let descriptors = self.descriptors;
        let fresh: Option<Vec<Option<f32>>> = self.with_handle(|handle| {
            let params = handle.get_extension::<PluginParams>();
            descriptors.iter().map(|d| params.as_ref().and_then(|p| p.get_value(handle, ClapId::new(d.id.0))).map(|val| to_normalized(d, val))).collect()
        });
        if let Some(fresh) = fresh {
            for (v, f) in self.values.iter_mut().zip(fresh) {
                if let Some(f) = f {
                    *v = f;
                }
            }
        }
    }

    /// Take back every audio half that a dropped plan returned, and deactivate it.
    pub fn collect_returns(&mut self) {
        while let Ok(Returned(started)) = self.return_rx.try_recv() {
            let stopped = started.stop_processing();
            self.with_instance(|inst| inst.deactivate(stopped));
            self.active = None;
        }
    }

    /// Produce the RT half for a plan compiled with `spec`. If an audio half is already out
    /// (a previous plan still running), a carrying shell is returned and takes the running
    /// processor over in `migrate_from`; otherwise the plugin is activated now.
    pub fn activate_for_rt(&mut self, spec: &PrepareSpec) -> Result<ClapProcessor, String> {
        self.collect_returns();
        let wanted = (spec.sample_rate, spec.max_frames);
        if let Some(active) = self.active
            && active != wanted
        {
            // The old plan still holds a processor activated for another configuration; it
            // comes back once that plan is dropped. Until then we cannot re-activate.
            return Err("plugin is still active at another sample rate or block size; try again after the engine switched".into());
        }
        let shell = |audio: Option<StartedPluginAudioProcessor<RawdawgHost>>, this: &Self| {
            let in_ch: usize = this.layout.inputs.iter().map(|&c| c as usize).sum();
            let out_ch: usize = this.layout.outputs.iter().map(|&c| c as usize).sum();
            ClapProcessor {
                audio,
                return_tx: this.return_tx.clone(),
                type_id: this.type_id,
                descriptors: this.descriptors,
                layout: this.layout.clone(),
                in_bufs: (0..in_ch).map(|_| vec![0.0f32; spec.max_frames]).collect(),
                out_bufs: (0..out_ch).map(|_| vec![0.0f32; spec.max_frames]).collect(),
                in_ports: AudioPorts::with_capacity(in_ch, this.layout.inputs.len()),
                out_ports: AudioPorts::with_capacity(out_ch, this.layout.outputs.len()),
                events_in: EventBuffer::with_capacity(MAX_EVENTS),
                events_out: EventBuffer::with_capacity(MAX_EVENTS),
                pending: Vec::with_capacity(MAX_PENDING_PARAMS),
                steady_time: 0,
                latency: this.latency,
                sample_rate: spec.sample_rate,
                max_frames: spec.max_frames,
                problems: this.problems.clone(),
                values: this.values.clone(),
            }
        };
        if self.active.is_some() {
            return Ok(shell(None, self));
        }
        let config = PluginAudioConfiguration { sample_rate: spec.sample_rate, min_frames_count: 1, max_frames_count: spec.max_frames as u32 };
        let (started, latency) = self
            .with_instance(|inst| {
                let stopped = inst.activate(|_, _| (), config).map_err(|e| format!("activation failed: {e}"))?;
                // latency is valid once activated
                let latency = {
                    let handle = inst.plugin_handle();
                    handle.get_extension::<PluginLatency>().map(|l| l.get(&handle)).unwrap_or(0)
                };
                match stopped.start_processing() {
                    Ok(s) => Ok((s, latency)),
                    Err(e) => {
                        inst.deactivate(e.into_stopped_processor());
                        Err("start_processing failed".to_owned())
                    }
                }
            })
            .ok_or_else(|| "plugin instance used off its main thread".to_owned())??;
        self.latency = latency;
        self.active = Some(wanted);
        let mut p = shell(None, self);
        p.latency = latency;
        p.audio = Some(started);
        Ok(p)
    }

    /// Opaque plugin state bytes (main thread).
    pub fn plugin_state(&mut self) -> Option<Vec<u8>> {
        self.with_handle(|handle| {
            let state = handle.get_extension::<PluginState>()?;
            let mut out = Vec::new();
            state.save(handle, &mut out).ok()?;
            Some(out)
        })
        .flatten()
    }

    /// Load opaque plugin state bytes (main thread).
    pub fn load_plugin_state(&mut self, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() > MAX_STATE_BYTES {
            return Err(format!("plugin state is {} bytes (limit {MAX_STATE_BYTES})", bytes.len()));
        }
        let r = self.with_handle(|handle| {
            let state = handle.get_extension::<PluginState>().ok_or("plugin has no state extension")?;
            state.load(handle, &mut std::io::Cursor::new(bytes)).map_err(|e| format!("plugin rejected the state: {e}"))
        });
        let r = r.unwrap_or_else(|| Err("plugin instance used off its main thread".to_owned()));
        if r.is_ok() {
            self.refresh_values();
        }
        r
    }

    /// Mark the instance as returned if the plan that held its audio half was dropped
    /// before collection (defensive; `collect_returns` handles the normal path).
    pub fn poll(&mut self) {
        self.collect_returns();
    }
}

fn to_normalized(d: &ParamDescriptor, value: f64) -> f32 {
    match d.mapping {
        ParamMapping::Linear { min, max } if max > min => (((value - min) / (max - min)) as f32).clamp(0.0, 1.0),
        ParamMapping::Toggle => {
            if value >= 0.5 {
                1.0
            } else {
                0.0
            }
        }
        ParamMapping::Discrete { .. } => d.to_normalized(value.round()),
        _ => (value as f32).clamp(0.0, 1.0),
    }
}

fn to_plugin_value(d: &ParamDescriptor, normalized: f32) -> f64 {
    let v = d.to_physical(normalized);
    if d.flags.stepped { v.round() } else { v }
}

/// Project-side JSON envelope for an external instance's state.
pub const STATE_KEY: &str = "clap_state_b64";

/// Error for a state blob that is still a `plugin-state/` file reference (unreadable file).
pub(crate) const NOT_INLINE: &str = "the saved plugin state is still a plugin-state file reference (the file could not be read)";

impl Processor for ClapInstance {
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
        // The mirror never processes; activation happens in `activate_for_rt`.
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
        let d = self.descriptors.iter().find(|d| d.id == id)?;
        let value = to_plugin_value(d, normalized.clamp(0.0, 1.0));
        self.texts.get_or(id.0, normalized, || {
            self.with_handle(|handle| {
                let params = handle.get_extension::<PluginParams>()?;
                let mut buf = [0u8; 256];
                let text = params.value_to_text(handle, ClapId::new(id.0), value, &mut buf).ok()?;
                sanitize_display_text(&String::from_utf8_lossy(text), MAX_PARAM_TEXT_CHARS)
            })
            .flatten()
        })
    }
    fn set_param(&mut self, id: ParamId, normalized: f32) -> bool {
        let Some(i) = self.descriptors.iter().position(|d| d.id == id) else { return false };
        let v = normalized.clamp(0.0, 1.0);
        self.values[i] = v;
        if self.active.is_none() {
            // inactive: the main-thread flush is the only way to reach the plugin
            let value = to_plugin_value(&self.descriptors[i], v);
            self.with_instance(|inst| {
                let params = inst.plugin_handle().get_extension::<PluginParams>();
                if let (Some(params), Some(mut handle)) = (params, inst.inactive_plugin_handle()) {
                    let mut buf = EventBuffer::with_capacity(1);
                    buf.push(&ParamValueEvent::new(0, ClapId::new(id.0), Pckn::match_all(), value));
                    let mut out = EventBuffer::new();
                    params.flush(&mut handle, &buf.as_input(), &mut out.as_output());
                }
            });
        }
        // active: the RT half receives the same change as an `EventKind::Param` event
        true
    }
    fn save_state(&self) -> Vec<u8> {
        let Some(bytes) = self
            .with_handle(|handle| {
                let state = handle.get_extension::<PluginState>()?;
                let mut bytes = Vec::new();
                state.save(handle, &mut bytes).ok()?;
                Some(bytes)
            })
            .flatten()
        else {
            return Vec::new();
        };
        let env = serde_json::json!({ "format": "clap", "plugin_id": self.plugin_id, STATE_KEY: crate::b64::encode(&bytes) });
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
        let Some(field) = obj.get(STATE_KEY) else { return Ok(()) };
        let Some(b64) = field.as_str() else { return Err(NOT_INLINE.into()) };
        let raw = crate::b64::decode(b64)?;
        self.texts.clear();
        self.load_plugin_state(&raw)
    }
}

/// Realtime half of a hosted CLAP plugin.
pub struct ClapProcessor {
    audio: Option<StartedPluginAudioProcessor<RawdawgHost>>,
    return_tx: Sender<Returned>,
    type_id: &'static str,
    descriptors: &'static [ParamDescriptor],
    layout: PortLayout,
    in_bufs: Vec<Vec<f32>>,
    out_bufs: Vec<Vec<f32>>,
    in_ports: AudioPorts,
    out_ports: AudioPorts,
    events_in: EventBuffer,
    events_out: EventBuffer,
    pending: Vec<(u32, f32)>,
    steady_time: u64,
    latency: u32,
    sample_rate: f64,
    max_frames: usize,
    problems: Arc<std::sync::atomic::AtomicU32>,
    values: Vec<f32>,
}

impl ClapProcessor {
    fn push_note_events(&mut self, events: &[TimedEvent]) {
        for e in events {
            if self.events_in.len() as usize >= MAX_EVENTS {
                break;
            }
            let t = e.frame;
            let ch = u16::from(e.channel & 0x0F);
            match e.kind {
                EventKind::NoteOn { note_id, pitch, velocity } => {
                    if self.layout.accepts_clap_notes || !self.layout.accepts_midi {
                        self.events_in.push(&NoteOnEvent::new(t, Pckn::new(0u16, ch, u16::from(pitch), note_id & 0x7FFF_FFFF), f64::from(velocity)));
                    } else {
                        self.events_in.push(&MidiEvent::new(t, 0, [0x90 | ch as u8, pitch & 0x7F, (velocity * 127.0).round().clamp(1.0, 127.0) as u8]));
                    }
                }
                EventKind::NoteOff { note_id, pitch } => {
                    if self.layout.accepts_clap_notes || !self.layout.accepts_midi {
                        self.events_in.push(&NoteOffEvent::new(t, Pckn::new(0u16, ch, u16::from(pitch), note_id & 0x7FFF_FFFF), 0.0));
                    } else {
                        self.events_in.push(&MidiEvent::new(t, 0, [0x80 | ch as u8, pitch & 0x7F, 0]));
                    }
                }
                EventKind::AllNotesOff { .. } | EventKind::Reset => {
                    if self.layout.accepts_midi {
                        self.events_in.push(&MidiEvent::new(t, 0, [0xB0 | ch as u8, 123, 0]));
                    } else if self.layout.accepts_clap_notes {
                        self.events_in.push(&NoteOffEvent::new(t, Pckn::new(0u16, ch, Match::<u16>::All, Match::<u32>::All), 0.0));
                    }
                }
                EventKind::Control { cc, value } if self.layout.accepts_midi => {
                    self.events_in.push(&MidiEvent::new(t, 0, [0xB0 | ch as u8, cc & 0x7F, (value * 127.0).round().clamp(0.0, 127.0) as u8]));
                }
                EventKind::PitchBend { value } if self.layout.accepts_midi => {
                    let v = ((value.clamp(-1.0, 1.0) + 1.0) * 8191.5).round() as u16;
                    self.events_in.push(&MidiEvent::new(t, 0, [0xE0 | ch as u8, (v & 0x7F) as u8, (v >> 7) as u8]));
                }
                EventKind::ChannelPressure { value } if self.layout.accepts_midi => {
                    self.events_in.push(&MidiEvent::new(t, 0, [0xD0 | ch as u8, (value * 127.0).round().clamp(0.0, 127.0) as u8, 0]));
                }
                EventKind::NotePressure { note_id, value } if self.layout.accepts_clap_notes => {
                    self.events_in.push(&NoteExpressionEvent::new(
                        t,
                        Pckn::new(0u16, ch, Match::<u16>::All, note_id & 0x7FFF_FFFF),
                        NoteExpressionType::Pressure,
                        f64::from(value.clamp(0.0, 1.0)),
                    ));
                }
                EventKind::NoteExpression { note_id, expression, value } if self.layout.accepts_clap_notes => {
                    let (kind, v) = match expression {
                        NoteExpression::Volume => (NoteExpressionType::Volume, f64::from(value.clamp(0.0, 1.0)) * 4.0),
                        NoteExpression::Pan => (NoteExpressionType::Pan, f64::from(value.clamp(0.0, 1.0))),
                        NoteExpression::Tuning => (NoteExpressionType::Tuning, f64::from(value.clamp(-120.0, 120.0))),
                        NoteExpression::Vibrato => (NoteExpressionType::Vibrato, f64::from(value.clamp(0.0, 1.0))),
                        NoteExpression::Expression => (NoteExpressionType::Expression, f64::from(value.clamp(0.0, 1.0))),
                        NoteExpression::Brightness => (NoteExpressionType::Brightness, f64::from(value.clamp(0.0, 1.0))),
                        NoteExpression::Pressure => (NoteExpressionType::Pressure, f64::from(value.clamp(0.0, 1.0))),
                    };
                    self.events_in.push(&NoteExpressionEvent::new(t, Pckn::new(0u16, ch, Match::<u16>::All, note_id & 0x7FFF_FFFF), kind, v));
                }
                EventKind::Param { id, value } => {
                    if let Some(i) = self.descriptors.iter().position(|d| d.id == id) {
                        self.values[i] = value.clamp(0.0, 1.0);
                        self.events_in.push(&ParamValueEvent::new(t, ClapId::new(id.0), Pckn::match_all(), to_plugin_value(&self.descriptors[i], value)));
                    }
                }
                _ => {}
            }
        }
    }

    fn transport_event(&self, ctx: &ProcessContext<'_>) -> TransportEvent {
        let t = ctx.transport;
        let mut flags = TransportFlags::HAS_TEMPO | TransportFlags::HAS_BEATS_TIMELINE | TransportFlags::HAS_SECONDS_TIMELINE | TransportFlags::HAS_TIME_SIGNATURE;
        if t.playing {
            flags |= TransportFlags::IS_PLAYING;
        }
        let beats_per_bar = f64::from(t.meter_numerator.max(1)) * 4.0 / f64::from(t.meter_denominator.max(1));
        let bar_number = (t.position_quarters / beats_per_bar).floor();
        TransportEvent {
            header: EventHeader::new_core(0, EventFlags::empty()),
            flags,
            song_pos_beats: BeatTime::from_float(t.position_quarters),
            song_pos_seconds: SecondsTime::from_float(t.block_start_frame.max(0) as f64 / self.sample_rate.max(1.0)),
            tempo: t.tempo_bpm,
            tempo_inc: 0.0,
            loop_start_beats: BeatTime::from_int(0),
            loop_end_beats: BeatTime::from_int(0),
            loop_start_seconds: SecondsTime::from_int(0),
            loop_end_seconds: SecondsTime::from_int(0),
            bar_start: BeatTime::from_float(bar_number * beats_per_bar),
            bar_number: bar_number.clamp(0.0, i32::MAX as f64) as i32,
            time_signature_numerator: u16::from(t.meter_numerator),
            time_signature_denominator: u16::from(t.meter_denominator),
        }
    }
}

impl Processor for ClapProcessor {
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
        if let Some(prev) = old.as_any_mut().downcast_mut::<ClapProcessor>()
            && self.audio.is_none()
            && prev.sample_rate == self.sample_rate
            && prev.max_frames == self.max_frames
        {
            self.audio = prev.audio.take();
            self.steady_time = prev.steady_time;
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
        if self.audio.is_none() {
            // carrying shell before migration, or audio half lost: pass through
            ctx.audio.copy_input_to_output();
            return;
        }
        self.events_in.clear();
        self.events_out.clear();
        // queued non-RT sets first (bypassed inserts, UI), then this block's events
        for &(id, v) in &self.pending {
            if let Some(i) = self.descriptors.iter().position(|d| d.id.0 == id) {
                self.events_in.push(&ParamValueEvent::new(0, ClapId::new(id), Pckn::match_all(), to_plugin_value(&self.descriptors[i], v)));
            }
        }
        self.pending.clear();
        self.push_note_events(ctx.events);
        // inputs: main port gets the chain audio (mono downmix when the port is mono),
        // the second port the sidechain when fed, further ports silence
        let mut ch_i = 0usize;
        for (pi, &chans) in self.layout.inputs.iter().enumerate() {
            let src: Option<(&[f32], &[f32])> = match pi {
                0 if ctx.audio.inputs.len() >= 2 => Some((&ctx.audio.inputs[0][..frames], &ctx.audio.inputs[1][..frames])),
                0 if ctx.audio.inputs.len() == 1 => Some((&ctx.audio.inputs[0][..frames], &ctx.audio.inputs[0][..frames])),
                1 if ctx.audio.sidechain.len() >= 2 => Some((&ctx.audio.sidechain[0][..frames], &ctx.audio.sidechain[1][..frames])),
                _ => None,
            };
            for c in 0..chans as usize {
                let buf = &mut self.in_bufs[ch_i + c][..frames];
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
            ch_i += chans as usize;
        }
        for b in &mut self.out_bufs {
            b[..frames].fill(0.0);
        }
        let transport = self.transport_event(ctx);
        let Self { audio, layout, in_bufs, out_bufs, in_ports, out_ports, events_in, events_out, steady_time, .. } = self;
        let status = {
            let input_audio = in_ports.with_input_buffers(PortChunks::new(in_bufs, &layout.inputs).map(|chunk| AudioPortBuffer {
                latency: 0,
                channels: AudioPortBufferType::f32_input_only(chunk.iter_mut().map(move |b| InputChannel::variable(&mut b[..frames]))),
            }));
            let mut output_audio = out_ports.with_output_buffers(
                PortChunks::new(out_bufs, &layout.outputs)
                    .map(|chunk| AudioPortBuffer { latency: 0, channels: AudioPortBufferType::f32_output_only(chunk.iter_mut().map(move |b| &mut b[..frames])) }),
            );
            let in_events = events_in.as_input();
            let mut out_events = events_out.as_output();
            audio.as_mut().expect("checked above").process(&input_audio, &mut output_audio, &in_events, &mut out_events, Some(*steady_time), Some(&transport))
        };
        self.steady_time = self.steady_time.wrapping_add(frames as u64);
        let mut bad = status.is_err();
        // main output port → chain outputs (mono out duplicated), non-finite → silence
        let main_out = self.layout.outputs.first().copied().unwrap_or(0) as usize;
        for (oi, out) in ctx.audio.outputs.iter_mut().enumerate() {
            let src = if main_out == 0 { None } else { Some(&self.out_bufs[oi.min(main_out - 1)][..frames]) };
            match src {
                Some(s) if !bad => out[..frames].copy_from_slice(s),
                _ => out[..frames].fill(0.0),
            }
        }
        if !bad && ctx.audio.outputs.iter().any(|out| out[..frames].iter().any(|v| !v.is_finite())) {
            bad = true;
            for out in ctx.audio.outputs.iter_mut() {
                out[..frames].fill(0.0);
            }
        }
        if bad {
            self.problems.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
    fn reset(&mut self) {
        if let Some(a) = self.audio.as_mut() {
            a.reset();
        }
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
        if self.pending.len() < MAX_PENDING_PARAMS {
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

impl Drop for ClapProcessor {
    fn drop(&mut self) {
        // Plans are dropped off the RT thread (`Engine::collect_retired`); hand the audio
        // half back so the main thread can stop and deactivate it.
        if let Some(a) = self.audio.take() {
            let _ = self.return_tx.send(Returned(a));
        }
    }
}

/// Splits the flat per-channel scratch list into one mutable sub-slice per port, without
/// allocating (the plan's buffers are fixed for its lifetime).
struct PortChunks<'a> {
    rest: &'a mut [Vec<f32>],
    sizes: std::slice::Iter<'a, u32>,
}

impl<'a> PortChunks<'a> {
    fn new(bufs: &'a mut [Vec<f32>], sizes: &'a [u32]) -> Self {
        Self { rest: bufs, sizes: sizes.iter() }
    }
}

impl<'a> Iterator for PortChunks<'a> {
    type Item = &'a mut [Vec<f32>];
    fn next(&mut self) -> Option<Self::Item> {
        let n = (*self.sizes.next()? as usize).min(self.rest.len());
        let rest = std::mem::take(&mut self.rest);
        let (head, tail) = rest.split_at_mut(n);
        self.rest = tail;
        Some(head)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.sizes.size_hint()
    }
}

impl ExactSizeIterator for PortChunks<'_> {}

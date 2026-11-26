//! CLAP export of rawDAWg native processors (PLUGIN_API.md §2, PLUG-01).
//!
//! The format layer is thin: CLAP note/param events are translated into
//! [`TimedEvent`]s and the shared [`Processor`] renders audio. No DSP lives here.
//! One generic plugin type ([`RawPlugin`]) wraps any native processor; the
//! factory ([`RawFactory`]) exposes the flagship synth (`rawdawg.synth`) and
//! every listed effect family (`rawdawg.fx.<family>`) from the format-neutral
//! catalogue in `rawdawg_plugin_api`. Supported extensions: audio ports (stereo
//! main in/out, stereo sidechain where the effect reads one), note ports
//! (instrument), params, state, latency and tail.
//!
//! Effects additionally expose the wrapper parameters `Bypass` and `Mix`
//! (`rawdawg_plugin_api::wrap`), shared with the VST3 export, so an exported
//! reverb behaves like its insert in rawDAWg and its state loads in both formats.

mod gui;

use clack_extensions::audio_ports::{AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts, PluginAudioPortsImpl};
use clack_extensions::latency::{PluginLatency, PluginLatencyImpl};
use clack_extensions::note_ports::{NoteDialect, NoteDialects, NotePortInfo, NotePortInfoWriter, PluginNotePorts, PluginNotePortsImpl};
use clack_extensions::params::{ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, PluginAudioProcessorParams, PluginMainThreadParams, PluginParams};
use clack_extensions::state::{PluginState, PluginStateImpl};
use clack_extensions::tail::{PluginTail, PluginTailImpl, TailLength};
use clack_plugin::entry::prelude::*;
use clack_plugin::prelude::*;
use rawdawg_dsp::processor::{PrepareSpec, ProcessContext, Processor, TransportView};
use rawdawg_dsp::{AudioBlock, BusLayout};
use rawdawg_plugin_api::ExportedPlugin;
use rawdawg_plugin_api::wrap::{EffectWrapper, parse_state, save_envelope, wrapper_descriptors};
use rawdawg_types::events::{EventKind, EventSource, TimedEvent, sort_events};
use rawdawg_types::{ParamDescriptor, ParamId, ParamMapping};
use std::ffi::{CStr, CString};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

pub use rawdawg_plugin_api::exported::{FX_CLAP_ID_PREFIX, SYNTH_CLAP_ID, URL, VENDOR, clap_id_for_native, native_type_id_for_clap};
pub use rawdawg_plugin_api::wrap::{WRAP_BYPASS, WRAP_MIX};

/// Maximum events translated per process call (bounded, preallocated).
const MAX_EVENTS: usize = 4096;
/// Maximum frames per process call we prepare for; larger host blocks are split.
const MAX_FRAMES: usize = 8192;
/// Hard cap for host-provided state blobs.
const MAX_STATE_BYTES: u64 = 16 * 1024 * 1024;

/// One plugin the entry exposes: the catalogue entry plus its CLAP descriptor.
pub struct Exported {
    pub plugin: ExportedPlugin,
    descriptor: PluginDescriptor,
    /// Owned feature strings the descriptor was built from (kept for the entry's lifetime).
    _features: Vec<CString>,
}

impl std::ops::Deref for Exported {
    type Target = ExportedPlugin;
    fn deref(&self) -> &ExportedPlugin {
        &self.plugin
    }
}

impl Exported {
    fn new(plugin: ExportedPlugin) -> Self {
        let features: Vec<CString> = plugin.clap_features().into_iter().filter_map(|f| CString::new(f).ok()).collect();
        let descriptor = PluginDescriptor::new(&plugin.clap_id, &plugin.name)
            .with_vendor(VENDOR)
            .with_url(URL)
            .with_version(rawdawg_types::VERSION)
            .with_description(plugin.description)
            .with_features(features.iter().map(|c| c.as_c_str()));
        Self { plugin, descriptor, _features: features }
    }
    pub fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }
    /// Display name as the host sees it (`rawDAWg Reverb`).
    pub fn display_name(&self) -> String {
        self.plugin.name.clone()
    }
}

/// Every plugin the entry exports (synth first, then the catalogue effects).
pub fn exported_plugins() -> Vec<Exported> {
    rawdawg_plugin_api::exported_plugins().into_iter().map(Exported::new).collect()
}

/// The generic plugin type: every exported plugin is an instance of it.
pub struct RawPlugin;

/// State the main thread, the audio thread and the editor share. Behind an `Arc` so the
/// editor (which outlives no plugin call but must be `'static`) can hold it.
pub struct Core {
    /// Main-thread copy of the processor used for descriptors, state and param queries.
    main: Mutex<Box<dyn Processor>>,
    bypass: AtomicBool,
    /// Wet/dry as f32 bits.
    mix_bits: AtomicU32,
    is_synth: bool,
    has_mix: bool,
    /// Parameter edits made in the editor, for the audio thread (or an inactive flush) to
    /// apply and report to the host.
    edits: gui::PendingEdits,
}

impl Core {
    fn mix(&self) -> f32 {
        f32::from_bits(self.mix_bits.load(Ordering::Relaxed))
    }
    fn set_wrapper_param(&self, id: u32, value: f32) -> bool {
        match id {
            WRAP_BYPASS if !self.is_synth => {
                self.bypass.store(value >= 0.5, Ordering::Relaxed);
                true
            }
            WRAP_MIX if self.has_mix => {
                self.mix_bits.store(value.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
                true
            }
            _ => false,
        }
    }
    /// Current normalized value of a visible parameter (wrapper or processor).
    fn value(&self, id: u32) -> Option<f32> {
        match id {
            WRAP_BYPASS if !self.is_synth => Some(if self.bypass.load(Ordering::Relaxed) { 1.0 } else { 0.0 }),
            WRAP_MIX if self.has_mix => Some(self.mix()),
            id => self.main.lock().ok()?.param(ParamId(id)),
        }
    }
}

/// Thread-safe shared state: the authoritative processor state for the main
/// thread (parameters, save/load). The audio processor owns a separate
/// prepared instance; parameter changes flow through CLAP events.
pub struct Shared<'a> {
    exported: &'a Exported,
    core: Arc<Core>,
    /// Latency of the active (or default-prepared) processor, for the latency extension.
    latency: AtomicU32,
}

impl std::ops::Deref for Shared<'_> {
    type Target = Core;
    fn deref(&self) -> &Core {
        &self.core
    }
}

impl<'a> PluginShared<'a> for Shared<'a> {}

impl<'a> Shared<'a> {
    fn new(exported: &'a Exported) -> Result<Self, PluginError> {
        let mut main = exported.make().map_err(|_| PluginError::Message("processor construction failed"))?;
        // A default preparation gives hosts a sensible latency before activation.
        let _ = main.prepare(&PrepareSpec::new(48_000.0, 512, BusLayout::Stereo));
        let latency = main.latency_frames();
        let wrapper = wrapper_descriptors(exported.is_synth(), exported.has_mix);
        let ids = main.descriptors().iter().filter(|d| !d.flags.hidden).chain(wrapper.iter()).map(|d| d.id.0);
        let edits = gui::PendingEdits::new(ids);
        let core = Core {
            main: Mutex::new(main),
            bypass: AtomicBool::new(false),
            mix_bits: AtomicU32::new(1.0f32.to_bits()),
            is_synth: exported.is_synth(),
            has_mix: exported.has_mix,
            edits,
        };
        Ok(Self { exported, core: Arc::new(core), latency: AtomicU32::new(latency) })
    }
    fn wrapper_descriptors(&self) -> &'static [ParamDescriptor] {
        wrapper_descriptors(self.exported.is_synth(), self.exported.has_mix)
    }
    fn native_descriptors(&self) -> &'static [ParamDescriptor] {
        self.main.lock().map(|m| m.descriptors()).unwrap_or(&[])
    }
    /// Only non-hidden descriptors are exposed to hosts; wrapper params come last.
    fn visible(&self) -> impl Iterator<Item = &'static ParamDescriptor> {
        self.native_descriptors().iter().filter(|d| !d.flags.hidden).chain(self.wrapper_descriptors().iter())
    }
    fn descriptor_by_id(&self, id: u32) -> Option<&'static ParamDescriptor> {
        self.native_descriptors().iter().chain(self.wrapper_descriptors().iter()).find(|d| d.id.0 == id)
    }
    /// Serialized state: raw processor state for the synth; the shared envelope for effects.
    fn save_state(&self) -> Result<Vec<u8>, PluginError> {
        let bytes = self.main.lock().map_err(|_| PluginError::Message("state lock poisoned"))?.save_state();
        if self.exported.is_synth() {
            return Ok(bytes);
        }
        save_envelope(self.exported.native_type_id, &bytes, self.bypass.load(Ordering::Relaxed), self.mix()).map_err(|_| PluginError::Message("state serialization failed"))
    }
    /// Accepts both the envelope and a bare processor state (app preset).
    fn load_state(&self, bytes: &[u8]) -> Result<(), PluginError> {
        let parsed = if self.exported.is_synth() {
            rawdawg_plugin_api::wrap::ParsedState { processor: bytes.to_vec(), bypass: None, mix: None }
        } else {
            parse_state(self.exported.native_type_id, bytes).map_err(|_| PluginError::Message("state belongs to a different rawDAWg plugin or is malformed"))?
        };
        let mut s = self.main.lock().map_err(|_| PluginError::Message("state lock poisoned"))?;
        s.load_state(&parsed.processor).map_err(|_| PluginError::Message("invalid state rejected; current state kept"))?;
        if let Some(b) = parsed.bypass {
            self.bypass.store(b, Ordering::Relaxed);
        }
        if let Some(m) = parsed.mix {
            self.mix_bits.store(m.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        }
        Ok(())
    }
}

pub struct MainThread<'a> {
    shared: &'a Shared<'a>,
    host: HostMainThreadHandle<'a>,
    /// The editor, when the host opened one (main thread only).
    gui: std::cell::RefCell<gui::GuiState>,
}

impl<'a> PluginMainThread<'a, Shared<'a>> for MainThread<'a> {}

impl Plugin for RawPlugin {
    type AudioProcessor<'a> = AudioProcessor<'a>;
    type Shared<'a> = Shared<'a>;
    type MainThread<'a> = MainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&Shared<'_>>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginNotePorts>()
            .register::<PluginParams>()
            .register::<PluginState>()
            .register::<PluginLatency>()
            .register::<PluginTail>()
            .register::<clack_extensions::gui::PluginGui>()
            .register::<clack_extensions::timer::PluginTimer>();
    }
}

/// Factory exposing every [`exported_plugins`] entry.
pub struct RawFactory {
    plugins: Vec<Exported>,
}

impl RawFactory {
    pub fn plugins(&self) -> &[Exported] {
        &self.plugins
    }
}

impl PluginFactoryImpl for RawFactory {
    fn plugin_count(&self) -> u32 {
        self.plugins.len() as u32
    }

    fn plugin_descriptor(&self, index: u32) -> Option<&PluginDescriptor> {
        self.plugins.get(index as usize).map(|p| &p.descriptor)
    }

    fn create_plugin<'a>(&'a self, host_info: HostInfo<'a>, plugin_id: &CStr) -> Option<PluginInstance<'a>> {
        let exported = self.plugins.iter().find(|p| p.clap_id.as_bytes() == plugin_id.to_bytes())?;
        Some(PluginInstance::new::<RawPlugin>(
            host_info,
            &exported.descriptor,
            move |_host| Shared::new(exported),
            |host, shared| Ok(MainThread { shared, host, gui: Default::default() }),
        ))
    }
}

/// The exported CLAP entry.
pub struct RawdawgEntry {
    factory: PluginFactoryWrapper<RawFactory>,
}

impl Entry for RawdawgEntry {
    fn new(_bundle_path: Option<&CStr>) -> Result<Self, EntryLoadError> {
        Ok(Self { factory: PluginFactoryWrapper::new(RawFactory { plugins: exported_plugins() }) })
    }

    fn declare_factories<'a>(&'a self, builder: &mut EntryFactories<'a>) {
        builder.register_factory(&self.factory);
    }
}

clack_export_entry!(RawdawgEntry);

pub struct AudioProcessor<'a> {
    processor: Box<dyn Processor>,
    events: Vec<TimedEvent>,
    /// Scratch copies of the host inputs (handles in-place and mono hosts uniformly).
    in_l: Vec<f32>,
    in_r: Vec<f32>,
    sc_l: Vec<f32>,
    sc_r: Vec<f32>,
    /// Scratch outputs for hosts offering fewer than two output channels.
    out_l: Vec<f32>,
    out_r: Vec<f32>,
    wrapper: EffectWrapper,
    is_synth: bool,
    has_sidechain: bool,
    sample_rate: f64,
    /// Integer-stepped parameters (id, low, high) for plain-value conversion on the audio thread.
    discrete: Vec<(u32, f64, f64)>,
    shared: &'a Shared<'a>,
}

/// CLAP plain values. CLAP's `IS_STEPPED` means integer values (hosts truncate a stepped
/// 0..1 range to its two ends), so a parameter with integer steps (a discrete choice, or a
/// stepped linear range such as lookahead 1–10 ms) is exposed in those integer units;
/// every other parameter is exposed as its normalized 0..1 value.
fn integer_range(d: &ParamDescriptor) -> Option<(f64, f64)> {
    match d.mapping {
        ParamMapping::Discrete { steps } if steps >= 2 => Some((0.0, f64::from(steps - 1))),
        ParamMapping::Linear { min, max } if d.flags.stepped && min.fract() == 0.0 && max.fract() == 0.0 && max > min => Some((min, max)),
        _ => None,
    }
}

fn plain_bounds(d: &ParamDescriptor) -> (f64, f64) {
    integer_range(d).unwrap_or((0.0, 1.0))
}

fn to_plain(d: &ParamDescriptor, normalized: f32) -> f64 {
    if integer_range(d).is_some() { d.to_physical(normalized).round() } else { f64::from(normalized.clamp(0.0, 1.0)) }
}

/// Integer plain value → normalized: the step's position in its range.
fn step_to_normalized(value: f64, lo: f64, hi: f64) -> f32 {
    ((value.round().clamp(lo, hi) - lo) / (hi - lo)) as f32
}

/// Plain value from a host → normalized; `None` for non-finite values.
fn from_plain(d: Option<&ParamDescriptor>, value: f64) -> Option<f32> {
    if !value.is_finite() {
        return None;
    }
    Some(match d.and_then(integer_range) {
        Some((lo, hi)) => step_to_normalized(value, lo, hi),
        None => (value as f32).clamp(0.0, 1.0),
    })
}

/// Integer ranges of a processor's stepped parameters (id, low, high), sorted by id, for
/// allocation-free lookup on the audio thread.
fn integer_table(descriptors: &[ParamDescriptor]) -> Vec<(u32, f64, f64)> {
    let mut t: Vec<(u32, f64, f64)> = descriptors.iter().filter_map(|d| integer_range(d).map(|(lo, hi)| (d.id.0, lo, hi))).collect();
    t.sort_unstable_by_key(|e| e.0);
    t
}

/// Normalized → plain on the audio thread (`table` from [`integer_table`]).
fn to_plain_rt(table: &[(u32, f64, f64)], id: u32, normalized: f32) -> f64 {
    match table.binary_search_by_key(&id, |e| e.0) {
        Ok(i) => (table[i].1 + f64::from(normalized.clamp(0.0, 1.0)) * (table[i].2 - table[i].1)).round(),
        Err(_) => f64::from(normalized.clamp(0.0, 1.0)),
    }
}

/// Report editor edits to the host: gesture begin, the latest value, gesture end.
fn report_edits(output: &mut OutputEvents, id: u32, flags: u8, plain: f64) {
    use clack_plugin::events::event_types::{ParamGestureBeginEvent, ParamGestureEndEvent, ParamValueEvent};
    let cid = ClapId::new(id);
    if flags & gui::BEGIN != 0 {
        let _ = output.try_push(ParamGestureBeginEvent::new(0, cid));
    }
    if flags & gui::VALUE != 0 {
        let _ = output.try_push(ParamValueEvent::new(0, cid, Pckn::match_all(), plain));
    }
    if flags & gui::END != 0 {
        let _ = output.try_push(ParamGestureEndEvent::new(0, cid));
    }
}

/// Plain → normalized on the audio thread (`table` from [`integer_table`]).
fn from_plain_rt(table: &[(u32, f64, f64)], id: u32, value: f64) -> Option<f32> {
    if !value.is_finite() {
        return None;
    }
    Some(match table.binary_search_by_key(&id, |e| e.0) {
        Ok(i) => step_to_normalized(value, table[i].1, table[i].2),
        Err(_) => (value as f32).clamp(0.0, 1.0),
    })
}

fn note_id_of(raw_note_id: i32, pitch: u8) -> u32 {
    if raw_note_id < 0 { u32::from(pitch) | 0x8000_0000 } else { raw_note_id as u32 }
}

fn translate_events(input: &InputEvents<'_>, out: &mut Vec<TimedEvent>, frames: u32, discrete: &[(u32, f64, f64)]) {
    use clack_plugin::events::event_types::{MidiEvent, NoteChokeEvent, NoteOffEvent, NoteOnEvent, ParamValueEvent};
    out.clear();
    for ev in input.iter() {
        if out.len() >= MAX_EVENTS {
            break;
        }
        let time = ev.header().time().min(frames.saturating_sub(1));
        if let Some(n) = ev.as_event::<NoteOnEvent>() {
            let raw = n.as_raw();
            let pitch = raw.key.clamp(0, 127) as u8;
            let velocity = (raw.velocity as f32).clamp(0.0, 1.0);
            let channel = raw.channel.clamp(0, 15) as u8;
            let note_id = note_id_of(raw.note_id, pitch);
            if velocity > 0.0 {
                out.push(TimedEvent::note_on(time, EventSource::Host, channel, note_id, pitch, velocity));
            } else {
                out.push(TimedEvent::note_off(time, EventSource::Host, channel, note_id, pitch));
            }
        } else if let Some(n) = ev.as_event::<NoteOffEvent>() {
            let raw = n.as_raw();
            let pitch = raw.key.clamp(0, 127) as u8;
            out.push(TimedEvent::note_off(time, EventSource::Host, raw.channel.clamp(0, 15) as u8, note_id_of(raw.note_id, pitch), pitch));
        } else if let Some(n) = ev.as_event::<NoteChokeEvent>() {
            let raw = n.as_raw();
            let pitch = raw.key.clamp(0, 127) as u8;
            out.push(TimedEvent::note_off(time, EventSource::Host, raw.channel.clamp(0, 15) as u8, note_id_of(raw.note_id, pitch), pitch));
        } else if let Some(p) = ev.as_event::<ParamValueEvent>() {
            if let Some(id) = p.param_id()
                && let Some(v) = from_plain_rt(discrete, id.get(), p.value())
            {
                out.push(TimedEvent::new(time, EventSource::Host, 0, EventKind::Param { id: ParamId(id.get()), value: v }));
            }
        } else if let Some(m) = ev.as_event::<MidiEvent>() {
            let data = m.data();
            let status = data[0];
            let channel = status & 0x0F;
            let note_id = u32::from(data[1] & 0x7F) | 0x8000_0000;
            if let Some(kind) = rawdawg_types::events::midi_message_to_kind(status, data[1], data[2], note_id) {
                out.push(TimedEvent::new(time, EventSource::Host, channel, kind));
            }
        }
    }
    sort_events(out);
}

fn transport_view(process: &Process, sample_rate: f64) -> TransportView {
    use clack_plugin::events::event_types::TransportFlags;
    let mut t = TransportView::stopped(sample_rate);
    t.block_start_frame = process.steady_time.map(|s| s.min(i64::MAX as u64) as i64).unwrap_or(0);
    if let Some(tr) = process.transport {
        t.playing = tr.flags.contains(TransportFlags::IS_PLAYING);
        if tr.flags.contains(TransportFlags::HAS_TEMPO) && tr.tempo.is_finite() && tr.tempo > 0.0 {
            t.tempo_bpm = tr.tempo.clamp(1.0, 999.0);
        }
        if tr.flags.contains(TransportFlags::HAS_BEATS_TIMELINE) {
            let q = tr.song_pos_beats.to_float();
            if q.is_finite() {
                t.position_quarters = q.max(0.0);
            }
        }
        if tr.flags.contains(TransportFlags::HAS_TIME_SIGNATURE) && tr.time_signature_numerator > 0 && tr.time_signature_denominator > 0 {
            t.meter_numerator = tr.time_signature_numerator.min(32) as u8;
            t.meter_denominator = tr.time_signature_denominator.min(32) as u8;
        }
    }
    t
}

impl<'a> AudioProcessor<'a> {
    /// Copy frames `start..start + frames` of the host's input port `index` into `(l, r)`;
    /// mono inputs feed both. Returns false when the port is absent or empty.
    fn copy_input(audio: &Audio, index: usize, start: usize, frames: usize, l: &mut [f32], r: &mut [f32]) -> bool {
        let Some(port) = audio.input_port(index) else { return false };
        let Ok(channels) = port.channels() else { return false };
        let Some(channels) = channels.into_f32() else { return false };
        let end = start + frames;
        match (channels.channel(0), channels.channel(1)) {
            (Some(a), Some(b)) if a.len() >= end && b.len() >= end => {
                l[..frames].copy_from_slice(&a[start..end]);
                r[..frames].copy_from_slice(&b[start..end]);
                true
            }
            (Some(a), None) if a.len() >= end => {
                l[..frames].copy_from_slice(&a[start..end]);
                r[..frames].copy_from_slice(&a[start..end]);
                true
            }
            _ => false,
        }
    }

    /// Render one prepared chunk of an effect from the scratch inputs (offset 0) into `out`.
    fn process_effect_chunk(&mut self, n: usize, events: &[TimedEvent], transport: &TransportView, sidechain_fed: bool, out: [&mut [f32]; 2]) {
        let Self { processor, wrapper, in_l, in_r, sc_l, sc_r, .. } = self;
        let sidechain = sidechain_fed.then_some([&sc_l[..n], &sc_r[..n]]);
        wrapper.process_chunk(processor.as_mut(), n, events, transport, [&in_l[..n], &in_r[..n]], sidechain, out);
    }

    fn status(&self) -> ProcessStatus {
        if self.is_synth {
            return if self.processor.active_voices() > 0 { ProcessStatus::Continue } else { ProcessStatus::ContinueIfNotQuiet };
        }
        match self.wrapper.tail_frames(self.processor.as_ref()) {
            Some(0) => ProcessStatus::ContinueIfNotQuiet,
            Some(_) => ProcessStatus::Tail,
            None => ProcessStatus::Continue,
        }
    }
}

impl<'a> PluginAudioProcessor<'a, Shared<'a>, MainThread<'a>> for AudioProcessor<'a> {
    fn activate(_host: HostAudioProcessorHandle<'a>, _main_thread: &MainThread<'a>, shared: &'a Shared<'a>, config: PluginAudioConfiguration) -> Result<Self, PluginError> {
        // Copy the authoritative main-thread state into a fresh processor instance.
        let bytes = shared.main.lock().map_err(|_| PluginError::Message("state lock poisoned"))?.save_state();
        let mut processor = shared.exported.make().map_err(|_| PluginError::Message("processor construction failed"))?;
        processor.load_state(&bytes).map_err(|_| PluginError::Message("invalid processor state"))?;
        let max_frames = (config.max_frames_count as usize).clamp(1, MAX_FRAMES);
        processor.prepare(&PrepareSpec::new(config.sample_rate, max_frames, BusLayout::Stereo)).map_err(|_| PluginError::Message("prepare failed"))?;
        let latency = processor.latency_frames();
        shared.latency.store(latency, Ordering::Relaxed);
        let is_synth = shared.exported.is_synth();
        let scratch = |len: usize| vec![0.0f32; len];
        let fx_len = if is_synth { 0 } else { max_frames };
        let mut wrapper = EffectWrapper::new(shared.exported.has_mix);
        if !is_synth {
            wrapper.prepare(latency, config.sample_rate, shared.mix());
            wrapper.bypass = shared.bypass.load(Ordering::Relaxed);
        }
        let discrete = integer_table(processor.descriptors());
        Ok(Self {
            processor,
            events: Vec::with_capacity(MAX_EVENTS),
            in_l: scratch(fx_len),
            in_r: scratch(fx_len),
            sc_l: scratch(fx_len),
            sc_r: scratch(fx_len),
            out_l: scratch(fx_len),
            out_r: scratch(fx_len),
            wrapper,
            is_synth,
            has_sidechain: shared.exported.has_sidechain,
            sample_rate: config.sample_rate,
            discrete,
            shared,
        })
    }

    fn process(&mut self, process: Process, mut audio: Audio, events: Events) -> Result<ProcessStatus, PluginError> {
        let frames = audio.frames_count();
        let total = frames as usize;
        translate_events(events.input, &mut self.events, frames, &self.discrete);
        // edits made in the editor: applied at the start of this block, reported to the host
        let (discrete, evs) = (&self.discrete, &mut self.events);
        self.shared.edits.drain(|id, flags, n| {
            if flags & gui::VALUE != 0 && evs.len() < evs.capacity() {
                evs.insert(0, TimedEvent::new(0, EventSource::Host, 0, EventKind::Param { id: ParamId(id), value: n }));
            }
            report_edits(events.output, id, flags, to_plain_rt(discrete, id, n));
        });
        if !self.is_synth {
            self.wrapper.take_wrapper_events(&mut self.events);
        }
        let transport = transport_view(&process, self.sample_rate);
        // Split oversized host blocks into prepared chunks; events keep their offsets.
        let mut done = 0usize;
        let mut ev_cursor = 0usize;
        while done < total {
            let n = (total - done).min(MAX_FRAMES);
            let end = done + n;
            let start_idx = ev_cursor;
            while ev_cursor < self.events.len() && (self.events[ev_cursor].frame as usize) < end {
                ev_cursor += 1;
            }
            let slice_end = ev_cursor;
            for e in &mut self.events[start_idx..slice_end] {
                e.frame -= done as u32;
            }
            // Borrow juggling: events are moved out of self for the chunk and restored after.
            let chunk_events = std::mem::take(&mut self.events);
            let chunk = &chunk_events[start_idx..slice_end];
            if self.is_synth {
                if let Some(mut port) = audio.output_port(0)
                    && let Some(mut channels) = port.channels()?.into_f32()
                {
                    let mut iter = channels.iter_mut();
                    if let (Some(l), Some(r)) = (iter.next(), iter.next()) {
                        let mut outs: [&mut [f32]; 2] = [&mut l[done..end], &mut r[done..end]];
                        let mut ctx = ProcessContext { audio: AudioBlock::output_only(n, &mut outs), events: chunk, transport: &transport };
                        self.processor.process(&mut ctx);
                    }
                }
            } else {
                if !Self::copy_input(&audio, 0, done, n, &mut self.in_l, &mut self.in_r) {
                    self.in_l[..n].fill(0.0);
                    self.in_r[..n].fill(0.0);
                }
                let sidechain_fed = self.has_sidechain && Self::copy_input(&audio, 1, done, n, &mut self.sc_l, &mut self.sc_r);
                let mut wrote = false;
                if let Some(mut port) = audio.output_port(0)
                    && let Some(mut channels) = port.channels()?.into_f32()
                {
                    let count = channels.channel_count();
                    if count >= 2 {
                        let (mut a, mut b) = channels.split_at_mut(1);
                        if let (Some(l), Some(r)) = (a.channel_mut(0), b.channel_mut(0)) {
                            self.process_effect_chunk(n, chunk, &transport, sidechain_fed, [&mut l[done..end], &mut r[done..end]]);
                            wrote = true;
                        }
                    } else if count == 1 {
                        let mut ol = std::mem::take(&mut self.out_l);
                        let mut or = std::mem::take(&mut self.out_r);
                        self.process_effect_chunk(n, chunk, &transport, sidechain_fed, [&mut ol[..n], &mut or[..n]]);
                        if let Some(m) = channels.channel_mut(0) {
                            for i in 0..n {
                                m[done + i] = 0.5 * (ol[i] + or[i]);
                            }
                        }
                        self.out_l = ol;
                        self.out_r = or;
                        wrote = true;
                    }
                }
                if !wrote {
                    // No usable output: still advance the effect so its state follows time.
                    let mut ol = std::mem::take(&mut self.out_l);
                    let mut or = std::mem::take(&mut self.out_r);
                    self.process_effect_chunk(n, chunk, &transport, sidechain_fed, [&mut ol[..n], &mut or[..n]]);
                    self.out_l = ol;
                    self.out_r = or;
                }
            }
            self.events = chunk_events;
            done = end;
        }
        Ok(self.status())
    }

    fn reset(&mut self) {
        self.processor.reset();
        self.wrapper.reset(self.shared.mix());
    }
}

impl PluginAudioPortsImpl for MainThread<'_> {
    fn count(&self, is_input: bool) -> u32 {
        let e = self.shared.exported;
        match (is_input, e.is_synth()) {
            (false, _) => 1,
            (true, true) => 0,
            (true, false) => 1 + u32::from(e.has_sidechain),
        }
    }
    fn get(&self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        let e = self.shared.exported;
        let main_pair = if e.is_synth() { None } else { Some(ClapId::new(0)) };
        match (is_input, index) {
            (false, 0) => writer.set(&AudioPortInfo {
                id: ClapId::new(0),
                name: b"Output",
                channel_count: 2,
                flags: AudioPortFlags::IS_MAIN,
                port_type: Some(AudioPortType::STEREO),
                in_place_pair: main_pair,
            }),
            (true, 0) if !e.is_synth() => writer.set(&AudioPortInfo {
                id: ClapId::new(0),
                name: b"Input",
                channel_count: 2,
                flags: AudioPortFlags::IS_MAIN,
                port_type: Some(AudioPortType::STEREO),
                in_place_pair: main_pair,
            }),
            (true, 1) if e.has_sidechain => writer.set(&AudioPortInfo {
                id: ClapId::new(1),
                name: b"Sidechain",
                channel_count: 2,
                flags: AudioPortFlags::empty(),
                port_type: Some(AudioPortType::STEREO),
                in_place_pair: None,
            }),
            _ => {}
        }
    }
}

impl PluginNotePortsImpl for MainThread<'_> {
    fn count(&self, is_input: bool) -> u32 {
        if is_input && self.shared.exported.is_synth() { 1 } else { 0 }
    }
    fn get(&self, index: u32, is_input: bool, writer: &mut NotePortInfoWriter) {
        if is_input && index == 0 && self.shared.exported.is_synth() {
            writer.set(&NotePortInfo {
                id: ClapId::new(0),
                name: b"Notes",
                supported_dialects: NoteDialects::CLAP | NoteDialects::MIDI,
                preferred_dialect: Some(NoteDialect::Clap),
            });
        }
    }
}

impl PluginLatencyImpl for MainThread<'_> {
    fn get(&self) -> u32 {
        self.shared.latency.load(Ordering::Relaxed)
    }
}

impl PluginTailImpl for AudioProcessor<'_> {
    fn get(&self) -> TailLength {
        if self.is_synth {
            return TailLength::Finite(0);
        }
        match self.wrapper.tail_frames(self.processor.as_ref()) {
            Some(n) => TailLength::Finite(n),
            None => TailLength::Infinite,
        }
    }
}

impl PluginMainThreadParams for MainThread<'_> {
    fn count(&self) -> u32 {
        self.shared.visible().count() as u32
    }

    fn get_info(&self, param_index: u32, info: &mut ParamInfoWriter) {
        let Some(d) = self.shared.visible().nth(param_index as usize) else { return };
        let mut flags = ParamInfoFlags::empty();
        if d.flags.automatable {
            flags |= ParamInfoFlags::IS_AUTOMATABLE;
        }
        if d.flags.modulatable {
            flags |= ParamInfoFlags::IS_MODULATABLE;
        }
        if d.flags.stepped || integer_range(d).is_some() {
            flags |= ParamInfoFlags::IS_STEPPED;
        }
        if d.flags.is_bypass {
            flags |= ParamInfoFlags::IS_BYPASS;
        }
        let module: &[u8] = if self.shared.exported.is_synth() {
            module_of(d.key)
        } else if d.id.0 >= WRAP_BYPASS {
            b"Wrapper"
        } else {
            b""
        };
        info.set(&ParamInfo {
            id: ClapId::new(d.id.0),
            flags,
            cookie: Default::default(),
            name: d.name.as_bytes(),
            module,
            min_value: plain_bounds(d).0,
            max_value: plain_bounds(d).1,
            default_value: to_plain(d, d.default_normalized),
        });
    }

    fn get_value(&self, param_id: ClapId) -> Option<f64> {
        match param_id.get() {
            WRAP_BYPASS if !self.shared.exported.is_synth() => Some(if self.shared.bypass.load(Ordering::Relaxed) { 1.0 } else { 0.0 }),
            WRAP_MIX if self.shared.exported.has_mix => Some(f64::from(self.shared.mix())),
            id => {
                let s = self.shared.main.lock().ok()?;
                let d = s.descriptors().iter().find(|d| d.id.0 == id)?;
                s.param(ParamId(id)).map(|n| to_plain(d, n))
            }
        }
    }

    fn value_to_text(&self, param_id: ClapId, value: f64, writer: &mut ParamDisplayWriter) -> std::fmt::Result {
        use std::fmt::Write;
        let d = self.shared.descriptor_by_id(param_id.get()).ok_or(std::fmt::Error)?;
        let n = from_plain(Some(d), value).ok_or(std::fmt::Error)?;
        write!(writer, "{}", d.format(n))
    }

    fn text_to_value(&self, param_id: ClapId, text: &CStr) -> Option<f64> {
        let d = self.shared.descriptor_by_id(param_id.get())?;
        d.parse(text.to_str().ok()?).map(|n| to_plain(d, n))
    }

    fn flush(&self, input: &InputEvents, output: &mut OutputEvents) {
        // Inactive flush: editor edits (already in the main-thread state) go to the host ...
        let discrete = self.shared.main.lock().map(|m| integer_table(m.descriptors())).unwrap_or_default();
        self.shared.edits.drain(|id, flags, n| report_edits(output, id, flags, to_plain_rt(&discrete, id, n)));
        // ... and parameter events from the host apply to the main-thread state.
        let Ok(mut s) = self.shared.main.lock() else { return };
        for ev in input.iter() {
            if let Some(p) = ev.as_event::<clack_plugin::events::event_types::ParamValueEvent>()
                && let Some(id) = p.param_id()
                && let Some(v) = from_plain(s.descriptors().iter().find(|d| d.id.0 == id.get()), p.value())
                && !self.shared.set_wrapper_param(id.get(), v)
            {
                s.set_param(ParamId(id.get()), v);
            }
        }
    }
}

fn module_of(key: &str) -> &'static [u8] {
    match key.split('.').next().unwrap_or("") {
        "amp" => b"Envelope",
        "filter" | "fenv" => b"Filter",
        "voice" | "tuning" => b"Voice",
        k if k.starts_with("layer") => b"Layers",
        _ => b"Main",
    }
}

impl PluginAudioProcessorParams for AudioProcessor<'_> {
    fn flush(&mut self, input: &InputEvents, output: &mut OutputEvents) {
        // editor edits while active but not processing: apply and report them
        let (discrete, processor, wrapper, is_synth) = (&self.discrete, &mut self.processor, &mut self.wrapper, self.is_synth);
        self.shared.edits.drain(|id, flags, n| {
            if flags & gui::VALUE != 0 && (is_synth || !wrapper.apply_param(id, n)) {
                processor.set_param(ParamId(id), n);
            }
            report_edits(output, id, flags, to_plain_rt(discrete, id, n));
        });
        for ev in input.iter() {
            if let Some(p) = ev.as_event::<clack_plugin::events::event_types::ParamValueEvent>()
                && let Some(id) = p.param_id()
                && let Some(v) = from_plain_rt(&self.discrete, id.get(), p.value())
                && (self.is_synth || !self.wrapper.apply_param(id.get(), v))
            {
                self.processor.set_param(ParamId(id.get()), v);
            }
        }
    }
}

impl PluginStateImpl for MainThread<'_> {
    fn save(&self, output: &mut clack_plugin::stream::OutputStream) -> Result<(), PluginError> {
        let bytes = self.shared.save_state()?;
        output.write_all(&bytes).map_err(|_| PluginError::Message("state write failed"))?;
        Ok(())
    }

    fn load(&self, input: &mut clack_plugin::stream::InputStream) -> Result<(), PluginError> {
        let mut bytes = Vec::new();
        input.take(MAX_STATE_BYTES).read_to_end(&mut bytes).map_err(|_| PluginError::Message("state read failed"))?;
        self.shared.load_state(&bytes)
    }
}

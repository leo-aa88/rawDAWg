//! CLAP export of rawDAWg native processors (PLUGIN_API.md §2, PLUG-01).
//!
//! The format layer is thin: CLAP note/param events are translated into
//! [`TimedEvent`]s and the shared [`Processor`] renders audio. No DSP lives here.
//! One generic plugin type ([`RawPlugin`]) wraps any native processor; the
//! factory ([`RawFactory`]) exposes the flagship synth (`rawdawg.synth`) and
//! every listed effect family (`rawdawg.fx.<family>`). Supported extensions:
//! audio ports (stereo main in/out, stereo sidechain where the effect reads
//! one), note ports (instrument), params, state, latency and tail.
//!
//! Effects additionally expose two wrapper parameters that the standalone app
//! applies outside the processor: `Bypass` (latency-preserving true bypass) and
//! `Mix` (wet/dry with the dry path delayed by the reported latency), so an
//! exported reverb behaves like its insert in rawDAWg.

use clack_extensions::audio_ports::{AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts, PluginAudioPortsImpl};
use clack_extensions::latency::{PluginLatency, PluginLatencyImpl};
use clack_extensions::note_ports::{NoteDialect, NoteDialects, NotePortInfo, NotePortInfoWriter, PluginNotePorts, PluginNotePortsImpl};
use clack_extensions::params::{ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, PluginAudioProcessorParams, PluginMainThreadParams, PluginParams};
use clack_extensions::state::{PluginState, PluginStateImpl};
use clack_extensions::tail::{PluginTail, PluginTailImpl, TailLength};
use clack_plugin::entry::prelude::*;
use clack_plugin::plugin::features;
use clack_plugin::prelude::*;
use rawdawg_dsp::delayline::DelayLine;
use rawdawg_dsp::processor::{PrepareSpec, ProcessContext, Processor, Tail, TransportView};
use rawdawg_dsp::smooth::Smoother;
use rawdawg_dsp::{AudioBlock, BusLayout};
use rawdawg_effects::EffectInfo;
use rawdawg_synthesizer::Synth;
use rawdawg_types::events::{EventKind, EventSource, TimedEvent, sort_events};
use rawdawg_types::params::ParamCategory;
use rawdawg_types::{ParamDescriptor, ParamId, ParamMapping, ParamUnit};
use std::ffi::CStr;
use std::io::{Read, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Stable external CLAP id of the flagship synth.
pub const SYNTH_CLAP_ID: &str = "rawdawg.synth";
/// Prefix of every exported effect id (`rawdawg.fx.reverb`, …).
pub const FX_CLAP_ID_PREFIX: &str = "rawdawg.fx.";
const NATIVE_FX_PREFIX: &str = "native.fx.";
pub const VENDOR: &str = "rawDAWg";
pub const URL: &str = "https://github.com/leonardo-aa/rawDAWg";

/// Maximum events translated per process call (bounded, preallocated).
const MAX_EVENTS: usize = 4096;
/// Maximum frames per process call we prepare for; larger host blocks are split.
const MAX_FRAMES: usize = 8192;
/// Wrapper parameter ids (outside the native descriptor id space, which is small).
pub const WRAP_BYPASS: u32 = 0xFFFF_0000;
pub const WRAP_MIX: u32 = 0xFFFF_0001;
/// Upper bound on the latency the dry path can compensate (10 s at 384 kHz would be absurd; 1 M frames is plenty).
const MAX_DRY_LATENCY: usize = 1 << 20;
/// Hard cap for host-provided state blobs.
const MAX_STATE_BYTES: u64 = 16 * 1024 * 1024;

static WRAP_DESCRIPTORS: [ParamDescriptor; 2] = [
    {
        let mut d = ParamDescriptor::continuous(WRAP_BYPASS, "wrap.bypass", "Bypass", ParamUnit::None, ParamMapping::Toggle, 0.0, ParamCategory::Mix).stepped();
        d.flags.is_bypass = true;
        d
    },
    ParamDescriptor::continuous(WRAP_MIX, "wrap.mix", "Mix", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 1.0 }, 1.0, ParamCategory::Mix),
];

/// What an exported plugin wraps.
#[derive(Clone, Copy, Debug)]
pub enum Kind {
    Synth,
    Effect(&'static EffectInfo),
}

/// One plugin the entry exposes.
pub struct Exported {
    pub clap_id: String,
    /// The native processor type id (`native.synth`, `native.fx.reverb`, …).
    pub native_type_id: &'static str,
    pub kind: Kind,
    /// Effects whose descriptors include a `sidechain` toggle get a second input port.
    pub has_sidechain: bool,
    pub has_mix: bool,
    descriptor: PluginDescriptor,
}

impl Exported {
    pub fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }
    pub fn is_synth(&self) -> bool {
        matches!(self.kind, Kind::Synth)
    }
    /// Display name as the host sees it (`rawDAWg Reverb`).
    pub fn display_name(&self) -> String {
        match self.kind {
            Kind::Synth => rawdawg_synthesizer::DISPLAY_NAME.to_owned(),
            Kind::Effect(info) => format!("rawDAWg {}", info.name),
        }
    }
    /// A fresh, unprepared native processor for this plugin.
    pub fn make(&self) -> Result<Box<dyn Processor>, String> {
        match self.kind {
            Kind::Synth => Ok(Box::new(Synth::default())),
            Kind::Effect(info) => rawdawg_effects::make_effect(info.type_id, None),
        }
    }
}

/// `native.fx.reverb` → `rawdawg.fx.reverb`; `native.synth` → `rawdawg.synth`.
pub fn clap_id_for_native(type_id: &str) -> Option<String> {
    if type_id == rawdawg_synthesizer::TYPE_ID { Some(SYNTH_CLAP_ID.to_owned()) } else { type_id.strip_prefix(NATIVE_FX_PREFIX).map(|slug| format!("{FX_CLAP_ID_PREFIX}{slug}")) }
}

/// Inverse of [`clap_id_for_native`].
pub fn native_type_id_for_clap(clap_id: &str) -> Option<String> {
    if clap_id == SYNTH_CLAP_ID { Some(rawdawg_synthesizer::TYPE_ID.to_owned()) } else { clap_id.strip_prefix(FX_CLAP_ID_PREFIX).map(|slug| format!("{NATIVE_FX_PREFIX}{slug}")) }
}

fn effect_features(info: &EffectInfo) -> Vec<&'static CStr> {
    let slug = info.type_id.strip_prefix(NATIVE_FX_PREFIX).unwrap_or(info.type_id);
    let specific: &[&'static CStr] = match slug {
        "eq" => &[features::EQUALIZER],
        "compressor" => &[features::COMPRESSOR],
        "limiter" => &[features::LIMITER, features::MASTERING],
        "gate" => &[features::GATE],
        "distortion" | "multiband" => &[features::DISTORTION],
        "delay" => &[features::DELAY],
        "chorus" => &[features::CHORUS],
        "flanger" => &[features::FLANGER],
        "phaser" => &[features::PHASER],
        "pitch" => &[features::PITCH_SHIFTER],
        "granular" => &[features::GRANULAR, features::GLITCH],
        "spectral" => &[features::PHASE_VOCODER, features::GLITCH],
        "reverb" | "convolution" => &[features::REVERB],
        "stereo" => &[features::UTILITY, features::MIXING],
        _ => &[],
    };
    let mut v = vec![features::AUDIO_EFFECT];
    v.extend_from_slice(specific);
    v.push(features::STEREO);
    v
}

/// Every plugin the entry exports: the synth first, then the listed effects in catalogue order.
pub fn exported_plugins() -> Vec<Exported> {
    let mut out = Vec::with_capacity(1 + rawdawg_effects::CATALOG.len());
    out.push(Exported {
        clap_id: SYNTH_CLAP_ID.to_owned(),
        native_type_id: rawdawg_synthesizer::TYPE_ID,
        kind: Kind::Synth,
        has_sidechain: false,
        has_mix: false,
        descriptor: PluginDescriptor::new(SYNTH_CLAP_ID, rawdawg_synthesizer::DISPLAY_NAME)
            .with_vendor(VENDOR)
            .with_url(URL)
            .with_version(rawdawg_types::VERSION)
            .with_description("rawDAWg flagship experimental synthesizer. Less thinking. More noise.")
            .with_features([features::INSTRUMENT, features::SYNTHESIZER, features::STEREO]),
    });
    for info in rawdawg_effects::CATALOG.iter().filter(|i| i.listed) {
        let Some(clap_id) = clap_id_for_native(info.type_id) else { continue };
        let has_sidechain = rawdawg_effects::make_effect(info.type_id, None).map(|p| p.descriptors().iter().any(|d| d.key == "sidechain")).unwrap_or(false);
        let name = format!("rawDAWg {}", info.name);
        out.push(Exported {
            descriptor: PluginDescriptor::new(&clap_id, &name)
                .with_vendor(VENDOR)
                .with_url(URL)
                .with_version(rawdawg_types::VERSION)
                .with_description(info.description)
                .with_features(effect_features(info)),
            clap_id,
            native_type_id: info.type_id,
            kind: Kind::Effect(info),
            has_sidechain,
            has_mix: info.has_mix,
        });
    }
    out
}

/// The generic plugin type: every exported plugin is an instance of it.
pub struct RawPlugin;

/// Thread-safe shared state: the authoritative processor state for the main
/// thread (parameters, save/load). The audio processor owns a separate
/// prepared instance; parameter changes flow through CLAP events.
pub struct Shared<'a> {
    exported: &'a Exported,
    /// Main-thread copy of the processor used for descriptors, state and param queries.
    main: Mutex<Box<dyn Processor>>,
    /// Latency of the active (or default-prepared) processor, for the latency extension.
    latency: AtomicU32,
    bypass: AtomicBool,
    /// Wet/dry as f32 bits.
    mix_bits: AtomicU32,
}

impl<'a> PluginShared<'a> for Shared<'a> {}

impl<'a> Shared<'a> {
    fn new(exported: &'a Exported) -> Result<Self, PluginError> {
        let mut main = exported.make().map_err(|_| PluginError::Message("processor construction failed"))?;
        // A default preparation gives hosts a sensible latency before activation.
        let _ = main.prepare(&PrepareSpec::new(48_000.0, 512, BusLayout::Stereo));
        let latency = main.latency_frames();
        Ok(Self { exported, main: Mutex::new(main), latency: AtomicU32::new(latency), bypass: AtomicBool::new(false), mix_bits: AtomicU32::new(1.0f32.to_bits()) })
    }
    fn mix(&self) -> f32 {
        f32::from_bits(self.mix_bits.load(Ordering::Relaxed))
    }
    fn set_wrapper_param(&self, id: u32, value: f32) -> bool {
        match id {
            WRAP_BYPASS => {
                self.bypass.store(value >= 0.5, Ordering::Relaxed);
                true
            }
            WRAP_MIX if self.exported.has_mix => {
                self.mix_bits.store(value.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
                true
            }
            _ => false,
        }
    }
    /// Wrapper descriptors this plugin exposes (none for instruments).
    fn wrapper_descriptors(&self) -> &'static [ParamDescriptor] {
        match (self.exported.is_synth(), self.exported.has_mix) {
            (true, _) => &[],
            (false, true) => &WRAP_DESCRIPTORS,
            (false, false) => &WRAP_DESCRIPTORS[..1],
        }
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
    /// Serialized state: raw processor state for the synth; a versioned envelope
    /// carrying the wrapper parameters for effects.
    fn save_state(&self) -> Result<Vec<u8>, PluginError> {
        let bytes = self.main.lock().map_err(|_| PluginError::Message("state lock poisoned"))?.save_state();
        if self.exported.is_synth() {
            return Ok(bytes);
        }
        let processor: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| PluginError::Message("processor state is not JSON"))?;
        let env = serde_json::json!({
            "rawdawg_clap": 1,
            "type_id": self.exported.native_type_id,
            "bypass": self.bypass.load(Ordering::Relaxed),
            "mix": self.mix(),
            "processor": processor,
        });
        serde_json::to_vec(&env).map_err(|_| PluginError::Message("state serialization failed"))
    }
    /// Accepts both the envelope and a bare processor state (app presets).
    fn load_state(&self, bytes: &[u8]) -> Result<(), PluginError> {
        let mut processor_bytes: &[u8] = bytes;
        let owned: Vec<u8>;
        let mut bypass = None;
        let mut mix = None;
        if !self.exported.is_synth()
            && let Ok(serde_json::Value::Object(map)) = serde_json::from_slice::<serde_json::Value>(bytes)
            && map.contains_key("rawdawg_clap")
        {
            if let Some(t) = map.get("type_id").and_then(|v| v.as_str())
                && t != self.exported.native_type_id
            {
                return Err(PluginError::Message("state belongs to a different rawDAWg plugin"));
            }
            bypass = map.get("bypass").and_then(|v| v.as_bool());
            mix = map.get("mix").and_then(|v| v.as_f64()).map(|m| m as f32).filter(|m| m.is_finite());
            let Some(p) = map.get("processor") else { return Err(PluginError::Message("state envelope lacks processor state")) };
            owned = serde_json::to_vec(p).map_err(|_| PluginError::Message("state serialization failed"))?;
            processor_bytes = &owned;
        }
        let mut s = self.main.lock().map_err(|_| PluginError::Message("state lock poisoned"))?;
        s.load_state(processor_bytes).map_err(|_| PluginError::Message("invalid state rejected; current state kept"))?;
        if let Some(b) = bypass {
            self.bypass.store(b, Ordering::Relaxed);
        }
        if let Some(m) = mix {
            self.mix_bits.store(m.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        }
        Ok(())
    }
}

pub struct MainThread<'a> {
    shared: &'a Shared<'a>,
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
            .register::<PluginTail>();
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
        Some(PluginInstance::new::<RawPlugin>(host_info, &exported.descriptor, move |_host| Shared::new(exported), |_host, shared| Ok(MainThread { shared })))
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
    dry: [DelayLine; 2],
    latency: usize,
    mix: Smoother,
    bypass: bool,
    is_synth: bool,
    has_mix: bool,
    has_sidechain: bool,
    sample_rate: f64,
    shared: &'a Shared<'a>,
}

fn note_id_of(raw_note_id: i32, pitch: u8) -> u32 {
    if raw_note_id < 0 { u32::from(pitch) | 0x8000_0000 } else { raw_note_id as u32 }
}

fn translate_events(input: &InputEvents<'_>, out: &mut Vec<TimedEvent>, frames: u32) {
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
            if let Some(id) = p.param_id() {
                let v = p.value() as f32;
                if v.is_finite() {
                    out.push(TimedEvent::new(time, EventSource::Host, 0, EventKind::Param { id: ParamId(id.get()), value: v.clamp(0.0, 1.0) }));
                }
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

    /// Apply wrapper parameter events (bypass/mix) and drop them from the native event list.
    fn take_wrapper_events(&mut self) {
        for e in &self.events {
            if let EventKind::Param { id, value } = e.kind {
                match id.0 {
                    WRAP_BYPASS => self.bypass = value >= 0.5,
                    WRAP_MIX if self.has_mix => self.mix.set_target(value),
                    _ => {}
                }
            }
        }
        self.events.retain(|e| !matches!(e.kind, EventKind::Param { id, .. } if id.0 == WRAP_BYPASS || id.0 == WRAP_MIX));
    }

    /// Render one prepared chunk of an effect: the scratch inputs (offset 0) → `out` with dry path, mix and bypass.
    fn process_effect_chunk(&mut self, n: usize, events: &[TimedEvent], transport: &TransportView, sidechain_fed: bool, out: [&mut [f32]; 2]) {
        let [out_l, out_r] = out;
        let start = 0usize;
        let end = start + n;
        if self.bypass {
            // parameters still arrive so the effect's state stays current
            for ev in events {
                if let EventKind::Param { id, value } = ev.kind {
                    self.processor.set_param(id, value);
                }
            }
            out_l.copy_from_slice(&self.in_l[start..end]);
            out_r.copy_from_slice(&self.in_r[start..end]);
            if self.latency > 0 {
                self.dry[0].delay_block(out_l, self.latency);
                self.dry[1].delay_block(out_r, self.latency);
            }
            return;
        }
        {
            let inputs: [&[f32]; 2] = [&self.in_l[start..end], &self.in_r[start..end]];
            let sc: [&[f32]; 2] = [&self.sc_l[start..end], &self.sc_r[start..end]];
            let mut outputs: [&mut [f32]; 2] = [out_l, out_r];
            let mut ctx =
                ProcessContext { audio: AudioBlock { frames: n, inputs: &inputs, outputs: &mut outputs, sidechain: if sidechain_fed { &sc } else { &[] } }, events, transport };
            self.processor.process(&mut ctx);
        }
        let lat = self.latency;
        if self.has_mix {
            for i in 0..n {
                let m = self.mix.next();
                self.dry[0].push(self.in_l[start + i]);
                self.dry[1].push(self.in_r[start + i]);
                let (dl, dr) = (self.dry[0].read(lat), self.dry[1].read(lat));
                out_l[i] = out_l[i] * m + dl * (1.0 - m);
                out_r[i] = out_r[i] * m + dr * (1.0 - m);
            }
        } else {
            for i in 0..n {
                self.dry[0].push(self.in_l[start + i]);
                self.dry[1].push(self.in_r[start + i]);
            }
        }
    }

    fn status(&self) -> ProcessStatus {
        if self.is_synth {
            return if self.processor.active_voices() > 0 { ProcessStatus::Continue } else { ProcessStatus::ContinueIfNotQuiet };
        }
        if self.bypass {
            return ProcessStatus::ContinueIfNotQuiet;
        }
        match self.processor.tail() {
            Tail::None => ProcessStatus::ContinueIfNotQuiet,
            Tail::Frames(_) => ProcessStatus::Tail,
            Tail::Unknown | Tail::Infinite => ProcessStatus::Continue,
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
        let latency = (processor.latency_frames() as usize).min(MAX_DRY_LATENCY);
        shared.latency.store(latency as u32, Ordering::Relaxed);
        let is_synth = shared.exported.is_synth();
        let scratch = |len: usize| vec![0.0f32; len];
        let fx_len = if is_synth { 0 } else { max_frames };
        let dry_len = if is_synth { 1 } else { (latency + 1).next_power_of_two() };
        let mut mix = Smoother::new(shared.mix(), 0.01, config.sample_rate as f32);
        mix.snap(shared.mix());
        Ok(Self {
            processor,
            events: Vec::with_capacity(MAX_EVENTS),
            in_l: scratch(fx_len),
            in_r: scratch(fx_len),
            sc_l: scratch(fx_len),
            sc_r: scratch(fx_len),
            out_l: scratch(fx_len),
            out_r: scratch(fx_len),
            dry: [DelayLine::new(dry_len), DelayLine::new(dry_len)],
            latency,
            mix,
            bypass: shared.bypass.load(Ordering::Relaxed),
            is_synth,
            has_mix: shared.exported.has_mix,
            has_sidechain: shared.exported.has_sidechain,
            sample_rate: config.sample_rate,
            shared,
        })
    }

    fn process(&mut self, process: Process, mut audio: Audio, events: Events) -> Result<ProcessStatus, PluginError> {
        let frames = audio.frames_count();
        let total = frames as usize;
        translate_events(events.input, &mut self.events, frames);
        if !self.is_synth {
            self.take_wrapper_events();
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
                            // the scratch inputs hold this chunk at offset 0
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
        self.dry[0].reset();
        self.dry[1].reset();
        self.mix.snap(self.shared.mix());
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
        if self.is_synth || self.bypass {
            return TailLength::Finite(0);
        }
        match self.processor.tail() {
            Tail::None => TailLength::Finite(0),
            Tail::Frames(n) => TailLength::Finite(n.saturating_add(self.latency as u32)),
            Tail::Unknown | Tail::Infinite => TailLength::Infinite,
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
        if d.flags.stepped {
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
            min_value: 0.0,
            max_value: 1.0,
            default_value: f64::from(d.default_normalized),
        });
    }

    fn get_value(&self, param_id: ClapId) -> Option<f64> {
        match param_id.get() {
            WRAP_BYPASS if !self.shared.exported.is_synth() => Some(if self.shared.bypass.load(Ordering::Relaxed) { 1.0 } else { 0.0 }),
            WRAP_MIX if self.shared.exported.has_mix => Some(f64::from(self.shared.mix())),
            id => {
                let s = self.shared.main.lock().ok()?;
                s.param(ParamId(id)).map(f64::from)
            }
        }
    }

    fn value_to_text(&self, param_id: ClapId, value: f64, writer: &mut ParamDisplayWriter) -> std::fmt::Result {
        use std::fmt::Write;
        let d = self.shared.descriptor_by_id(param_id.get()).ok_or(std::fmt::Error)?;
        write!(writer, "{}", d.format(value as f32))
    }

    fn text_to_value(&self, param_id: ClapId, text: &CStr) -> Option<f64> {
        let d = self.shared.descriptor_by_id(param_id.get())?;
        d.parse(text.to_str().ok()?).map(f64::from)
    }

    fn flush(&self, input: &InputEvents, _output: &mut OutputEvents) {
        // Inactive flush: apply parameter events to the main-thread state.
        let Ok(mut s) = self.shared.main.lock() else { return };
        for ev in input.iter() {
            if let Some(p) = ev.as_event::<clack_plugin::events::event_types::ParamValueEvent>()
                && let Some(id) = p.param_id()
            {
                let v = p.value() as f32;
                if !self.shared.set_wrapper_param(id.get(), v) {
                    s.set_param(ParamId(id.get()), v);
                }
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
    fn flush(&mut self, input: &InputEvents, _output: &mut OutputEvents) {
        for ev in input.iter() {
            if let Some(p) = ev.as_event::<clack_plugin::events::event_types::ParamValueEvent>()
                && let Some(id) = p.param_id()
            {
                let v = p.value() as f32;
                match id.get() {
                    WRAP_BYPASS if !self.is_synth => self.bypass = v >= 0.5,
                    WRAP_MIX if self.has_mix => self.mix.set_target(v.clamp(0.0, 1.0)),
                    id => {
                        self.processor.set_param(ParamId(id), v);
                    }
                }
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

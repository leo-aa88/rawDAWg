//! CLAP export of rawDAWg native processors (PLUGIN_API.md §2).
//!
//! The format layer is thin: CLAP note/param events are translated into
//! [`TimedEvent`]s and the shared [`Processor`] renders audio. No DSP lives here.
//! M0 scope: the flagship synth (`rawdawg.synth`) with note ports, a stereo
//! output port, parameters and state. GUI, latency/tail and effects follow in M7.

use clack_extensions::audio_ports::{AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts, PluginAudioPortsImpl};
use clack_extensions::note_ports::{NoteDialect, NoteDialects, NotePortInfo, NotePortInfoWriter, PluginNotePorts, PluginNotePortsImpl};
use clack_extensions::params::{ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, PluginAudioProcessorParams, PluginMainThreadParams, PluginParams};
use clack_extensions::state::{PluginState, PluginStateImpl};
use clack_plugin::plugin::features;
use clack_plugin::prelude::*;
use rawdawg_dsp::processor::{PrepareSpec, ProcessContext, Processor, TransportView};
use rawdawg_dsp::{AudioBlock, BusLayout};
use rawdawg_synthesizer::Synth;
use rawdawg_types::ParamId;
use rawdawg_types::events::{EventKind, EventSource, TimedEvent, sort_events};
use std::ffi::CStr;
use std::io::{Read, Write};
use std::sync::Mutex;

/// Stable external CLAP id of the flagship synth.
pub const SYNTH_CLAP_ID: &str = "rawdawg.synth";
pub const VENDOR: &str = "rawDAWg";
pub const URL: &str = "https://github.com/leonardo-aa/rawDAWg";

/// Maximum events translated per process call (bounded, preallocated).
const MAX_EVENTS: usize = 4096;
/// Maximum frames per process call we prepare for; larger host blocks are split.
const MAX_FRAMES: usize = 8192;

pub struct SynthPlugin;

/// Thread-safe shared state: the authoritative processor state for the main
/// thread (parameters, save/load). The audio processor owns a separate
/// prepared instance; parameter changes flow through CLAP events.
pub struct Shared {
    /// Main-thread copy of the processor used for descriptors, state and param queries.
    main: Mutex<Synth>,
}

impl<'a> PluginShared<'a> for Shared {}

pub struct MainThread<'a> {
    shared: &'a Shared,
}

impl<'a> PluginMainThread<'a, Shared> for MainThread<'a> {}

impl Plugin for SynthPlugin {
    type AudioProcessor<'a> = AudioProcessor<'a>;
    type Shared<'a> = Shared;
    type MainThread<'a> = MainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&Shared>) {
        builder.register::<PluginAudioPorts>().register::<PluginNotePorts>().register::<PluginParams>().register::<PluginState>();
    }
}

impl DefaultPluginFactory for SynthPlugin {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(SYNTH_CLAP_ID, rawdawg_synthesizer::DISPLAY_NAME)
            .with_vendor(VENDOR)
            .with_url(URL)
            .with_version(rawdawg_types::VERSION)
            .with_description("rawDAWg flagship experimental synthesizer. Less thinking. More noise.")
            .with_features([features::INSTRUMENT, features::SYNTHESIZER, features::STEREO])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<Shared, PluginError> {
        Ok(Shared { main: Mutex::new(Synth::default()) })
    }

    fn new_main_thread<'a>(_host: HostMainThreadHandle<'a>, shared: &'a Shared) -> Result<MainThread<'a>, PluginError> {
        Ok(MainThread { shared })
    }
}

pub struct AudioProcessor<'a> {
    synth: Synth,
    events: Vec<TimedEvent>,
    _shared: &'a Shared,
}

fn translate_events(input: &InputEvents<'_>, out: &mut Vec<TimedEvent>, frames: u32) {
    out.clear();
    for ev in input.iter() {
        if out.len() >= MAX_EVENTS {
            break;
        }
        let time = ev.header().time().min(frames.saturating_sub(1));
        if let Some(n) = ev.as_event::<clack_plugin::events::event_types::NoteOnEvent>() {
            let raw = n.as_raw();
            let pitch = raw.key.clamp(0, 127) as u8;
            let velocity = (raw.velocity as f32).clamp(0.0, 1.0);
            let channel = raw.channel.clamp(0, 15) as u8;
            let note_id = if raw.note_id < 0 { u32::from(pitch) | 0x8000_0000 } else { raw.note_id as u32 };
            if velocity > 0.0 {
                out.push(TimedEvent::note_on(time, EventSource::Host, channel, note_id, pitch, velocity));
            } else {
                out.push(TimedEvent::note_off(time, EventSource::Host, channel, note_id, pitch));
            }
        } else if let Some(n) = ev.as_event::<clack_plugin::events::event_types::NoteOffEvent>() {
            let raw = n.as_raw();
            let pitch = raw.key.clamp(0, 127) as u8;
            let channel = raw.channel.clamp(0, 15) as u8;
            let note_id = if raw.note_id < 0 { u32::from(pitch) | 0x8000_0000 } else { raw.note_id as u32 };
            out.push(TimedEvent::note_off(time, EventSource::Host, channel, note_id, pitch));
        } else if let Some(n) = ev.as_event::<clack_plugin::events::event_types::NoteChokeEvent>() {
            let raw = n.as_raw();
            let pitch = raw.key.clamp(0, 127) as u8;
            let channel = raw.channel.clamp(0, 15) as u8;
            let note_id = if raw.note_id < 0 { u32::from(pitch) | 0x8000_0000 } else { raw.note_id as u32 };
            out.push(TimedEvent::note_off(time, EventSource::Host, channel, note_id, pitch));
        } else if let Some(p) = ev.as_event::<clack_plugin::events::event_types::ParamValueEvent>() {
            if let Some(id) = p.param_id() {
                let v = p.value() as f32;
                if v.is_finite() {
                    out.push(TimedEvent::new(time, EventSource::Host, 0, EventKind::Param { id: ParamId(id.get()), value: v.clamp(0.0, 1.0) }));
                }
            }
        } else if let Some(m) = ev.as_event::<clack_plugin::events::event_types::MidiEvent>() {
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

impl<'a> PluginAudioProcessor<'a, Shared, MainThread<'a>> for AudioProcessor<'a> {
    fn activate(_host: HostAudioProcessorHandle<'a>, _main_thread: &MainThread<'a>, shared: &'a Shared, config: PluginAudioConfiguration) -> Result<Self, PluginError> {
        // Copy the authoritative main-thread state into a fresh processor instance.
        let bytes = shared.main.lock().map_err(|_| PluginError::Message("state lock poisoned"))?.save_state();
        let mut synth = Synth::default();
        synth.load_state(&bytes).map_err(|_| PluginError::Message("invalid synth state"))?;
        let max_frames = (config.max_frames_count as usize).clamp(1, MAX_FRAMES);
        synth.prepare(&PrepareSpec::new(config.sample_rate, max_frames, BusLayout::Stereo)).map_err(|_| PluginError::Message("prepare failed"))?;
        Ok(Self { synth, events: Vec::with_capacity(MAX_EVENTS), _shared: shared })
    }

    fn process(&mut self, _process: Process, mut audio: Audio, events: Events) -> Result<ProcessStatus, PluginError> {
        let frames = audio.frames_count();
        translate_events(events.input, &mut self.events, frames);
        let Some(mut port) = audio.output_port(0) else { return Ok(ProcessStatus::Continue) };
        let Some(mut channels) = port.channels()?.into_f32() else { return Ok(ProcessStatus::Continue) };
        let mut iter = channels.iter_mut();
        let (Some(l), Some(r)) = (iter.next(), iter.next()) else { return Ok(ProcessStatus::Continue) };
        let transport = TransportView::stopped(0.0);
        // Split oversized host blocks into prepared chunks; events keep their offsets.
        let mut done = 0usize;
        let total = frames as usize;
        let mut ev_cursor = 0usize;
        while done < total {
            let n = (total - done).min(MAX_FRAMES);
            let end = done + n;
            // events within [done, end)
            let start_idx = ev_cursor;
            while ev_cursor < self.events.len() && (self.events[ev_cursor].frame as usize) < end {
                ev_cursor += 1;
            }
            let slice_end = ev_cursor;
            for e in &mut self.events[start_idx..slice_end] {
                e.frame -= done as u32;
            }
            {
                let mut outs: [&mut [f32]; 2] = [&mut l[done..end], &mut r[done..end]];
                let mut ctx = ProcessContext { audio: AudioBlock::output_only(n, &mut outs), events: &self.events[start_idx..slice_end], transport: &transport };
                self.synth.process(&mut ctx);
            }
            done = end;
        }
        Ok(if self.synth.active_voices() > 0 { ProcessStatus::Continue } else { ProcessStatus::ContinueIfNotQuiet })
    }

    fn reset(&mut self) {
        self.synth.reset();
    }
}

impl PluginAudioPortsImpl for MainThread<'_> {
    fn count(&self, is_input: bool) -> u32 {
        if is_input { 0 } else { 1 }
    }
    fn get(&self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        if !is_input && index == 0 {
            writer.set(&AudioPortInfo {
                id: ClapId::new(0),
                name: b"Output",
                channel_count: 2,
                flags: AudioPortFlags::IS_MAIN,
                port_type: Some(AudioPortType::STEREO),
                in_place_pair: None,
            });
        }
    }
}

impl PluginNotePortsImpl for MainThread<'_> {
    fn count(&self, is_input: bool) -> u32 {
        if is_input { 1 } else { 0 }
    }
    fn get(&self, index: u32, is_input: bool, writer: &mut NotePortInfoWriter) {
        if is_input && index == 0 {
            writer.set(&NotePortInfo {
                id: ClapId::new(0),
                name: b"Notes",
                supported_dialects: NoteDialects::CLAP | NoteDialects::MIDI,
                preferred_dialect: Some(NoteDialect::Clap),
            });
        }
    }
}

fn descriptors() -> &'static [rawdawg_types::ParamDescriptor] {
    &rawdawg_synthesizer::params::DESCRIPTORS
}

/// Only non-hidden descriptors are exposed to hosts.
fn visible_descriptors() -> impl Iterator<Item = &'static rawdawg_types::ParamDescriptor> {
    descriptors().iter().filter(|d| !d.flags.hidden)
}

impl PluginMainThreadParams for MainThread<'_> {
    fn count(&self) -> u32 {
        visible_descriptors().count() as u32
    }

    fn get_info(&self, param_index: u32, info: &mut ParamInfoWriter) {
        let Some(d) = visible_descriptors().nth(param_index as usize) else { return };
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
        info.set(&ParamInfo {
            id: ClapId::new(d.id.0),
            flags,
            cookie: Default::default(),
            name: d.name.as_bytes(),
            module: module_of(d.key),
            min_value: 0.0,
            max_value: 1.0,
            default_value: f64::from(d.default_normalized),
        });
    }

    fn get_value(&self, param_id: ClapId) -> Option<f64> {
        let s = self.shared.main.lock().ok()?;
        s.param(ParamId(param_id.get())).map(f64::from)
    }

    fn value_to_text(&self, param_id: ClapId, value: f64, writer: &mut ParamDisplayWriter) -> std::fmt::Result {
        use std::fmt::Write;
        let d = descriptors().iter().find(|d| d.id.0 == param_id.get()).ok_or(std::fmt::Error)?;
        write!(writer, "{}", d.format(value as f32))
    }

    fn text_to_value(&self, param_id: ClapId, text: &CStr) -> Option<f64> {
        let d = descriptors().iter().find(|d| d.id.0 == param_id.get())?;
        d.parse(text.to_str().ok()?).map(f64::from)
    }

    fn flush(&self, input: &InputEvents, _output: &mut OutputEvents) {
        // Inactive flush: apply parameter events to the main-thread state.
        let Ok(mut s) = self.shared.main.lock() else { return };
        for ev in input.iter() {
            if let Some(p) = ev.as_event::<clack_plugin::events::event_types::ParamValueEvent>()
                && let Some(id) = p.param_id()
            {
                s.set_param(ParamId(id.get()), p.value() as f32);
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
                self.synth.set_param(ParamId(id.get()), p.value() as f32);
            }
        }
    }
}

impl PluginStateImpl for MainThread<'_> {
    fn save(&self, output: &mut clack_plugin::stream::OutputStream) -> Result<(), PluginError> {
        let bytes = self.shared.main.lock().map_err(|_| PluginError::Message("state lock poisoned"))?.save_state();
        output.write_all(&bytes).map_err(|_| PluginError::Message("state write failed"))?;
        Ok(())
    }

    fn load(&self, input: &mut clack_plugin::stream::InputStream) -> Result<(), PluginError> {
        let mut bytes = Vec::new();
        // bounded read: 16 MiB cap
        input.take(16 * 1024 * 1024).read_to_end(&mut bytes).map_err(|_| PluginError::Message("state read failed"))?;
        let mut s = self.shared.main.lock().map_err(|_| PluginError::Message("state lock poisoned"))?;
        s.load_state(&bytes).map_err(|_| PluginError::Message("invalid state rejected; current state kept"))?;
        Ok(())
    }
}

/// The exported CLAP entry.
pub type Entry = SinglePluginEntry<SynthPlugin>;

clack_export_entry!(Entry);

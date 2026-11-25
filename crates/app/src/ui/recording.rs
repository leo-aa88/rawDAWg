//! Recording and MIDI-learn glue for the app (IO-01/IO-03, CORE-08 subset):
//! starts/finishes takes from the engine's applied-event echo, commits them as
//! undoable commands, maps learned CCs to instrument parameters.

use super::{App, AudioState, EditorView, EngineCommand, TransportMode};
use rawdawg_audio_engine::Take;
use rawdawg_dsp::processor::Processor;
use rawdawg_project::{AutomationTarget, Clip, ClipContent, Command, Lane, MidiMapping, Pattern, Track};
use rawdawg_types::events::{EventKind, EventSource, TimedEvent};
use rawdawg_types::units::{PPQ, Tick};
use rawdawg_types::{ClipId, InstanceId, MappingId, ParamId, PatternId, TrackId};
use std::time::{Duration, Instant};

#[derive(Default)]
pub(crate) struct RecordState {
    pub armed: bool,
    pub take: Option<Take>,
    /// Song-mode takes record into a fresh pattern placed as a clip on this track.
    pub song_pattern: Option<(PatternId, String)>,
}

/// How long a MIDI-controlled parameter may rest before its state is committed as one undo entry.
const MIDI_COMMIT_QUIET: Duration = Duration::from_millis(300);

impl App {
    /// Count-in beats for the current meter and preference.
    pub(crate) fn count_in_beats(&self) -> u32 {
        let numerator = self.project.meter.first().map(|m| u32::from(m.numerator)).unwrap_or(4);
        self.prefs.count_in_bars * numerator
    }

    /// Play from the toolbar/space: count in first when recording is armed.
    pub(crate) fn play_pressed(&mut self) {
        if self.record.armed && self.prefs.count_in_bars > 0 {
            self.send(EngineCommand::PlayWithCountIn { beats: self.count_in_beats() });
        } else {
            self.send(EngineCommand::Play);
        }
    }

    pub(crate) fn set_record_armed(&mut self, armed: bool) {
        self.record.armed = armed;
        self.send(EngineCommand::SetRecording(armed));
    }

    /// Called every UI frame: start/feed/finish the take from the engine's echo queue.
    pub(crate) fn poll_record(&mut self) {
        let (playing, frame) = match &self.audio {
            AudioState::Running(s) => {
                (s.handle.status.playing.load(std::sync::atomic::Ordering::Relaxed), s.handle.status.position_frame.load(std::sync::atomic::Ordering::Relaxed))
            }
            _ => (false, 0),
        };
        if self.record.armed && playing && self.record.take.is_none() {
            self.start_take(frame);
        }
        // drain the echo queue (events the engine applied, with their frames)
        let mut events = Vec::new();
        if let AudioState::Running(s) = &mut self.audio {
            while let Some(ev) = s.handle.poll_recorded() {
                events.push(ev);
            }
        }
        if let Some(take) = self.record.take.as_mut() {
            for ev in events {
                take.push(ev);
            }
        }
        if self.record.take.is_some() && (!playing || !self.record.armed) {
            self.finish_take(frame);
        }
    }

    fn start_take(&mut self, frame: i64) {
        let Some(channel) = self.selected_channel else { return };
        let Some(sr) = (match &self.audio {
            AudioState::Running(s) => Some(s.sample_rate),
            _ => None,
        }) else {
            return;
        };
        let Ok(tempo) = self.project.tempo_map() else { return };
        match self.transport_mode {
            TransportMode::Pattern => {
                let Some(pid) = self.project.selected_pattern.or_else(|| self.project.patterns.first().map(|p| p.id)) else { return };
                let len = self.project.pattern(pid).map(|p| p.length_ticks.0).unwrap_or(PPQ * 4);
                let constant = rawdawg_transport::TempoMap::constant(tempo.tempo_at(Tick::ZERO));
                self.record.take = Some(Take::new(pid, channel, len, 0, true, constant, sr, self.prefs.record_offset_ms));
                self.record.song_pattern = None;
            }
            TransportMode::Song => {
                let bar = self.project.meter_map().ok().map(|m| m.bar_start_tick(1).0).filter(|t| *t > 0).unwrap_or(PPQ * 4);
                let now = tempo.frame_to_tick_floor(frame, sr).0;
                let origin = now.div_euclid(bar) * bar;
                let pid = PatternId::new();
                let name = format!("Take {}", self.project.patterns.len() + 1);
                self.record.take = Some(Take::new(pid, channel, i64::MAX / 4, origin, false, tempo, sr, self.prefs.record_offset_ms));
                self.record.song_pattern = Some((pid, name));
            }
        }
        self.status_line = "Recording…".into();
    }

    fn finish_take(&mut self, frame: i64) {
        let Some(mut take) = self.record.take.take() else { return };
        take.finish(frame);
        if take.is_empty() {
            self.status_line = "Recording stopped: nothing played".into();
            return;
        }
        let note_ids: Vec<_> = take.notes.iter().map(|n| n.id).collect();
        let count = take.notes.len();
        let mut batch = Vec::new();
        match self.record.song_pattern.take() {
            None => {
                if !take.notes.is_empty() {
                    batch.push(Command::AddNotes { pattern_id: take.pattern, channel_id: take.channel, notes: take.notes.clone() });
                }
                if !take.controllers.is_empty() {
                    batch.push(Command::AddControllerEvents { pattern_id: take.pattern, channel_id: take.channel, events: take.controllers.clone() });
                }
            }
            Some((pid, name)) => {
                // size the new pattern to whole bars covering the take
                let bar = self.project.meter_map().ok().map(|m| m.bar_start_tick(1).0).filter(|t| *t > 0).unwrap_or(PPQ * 4);
                let last = take.notes.iter().map(|n| n.tick.0 + n.duration_ticks.0).chain(take.controllers.iter().map(|c| c.tick.0)).max().unwrap_or(0);
                let length = ((last / bar) + 1).max(1) * bar;
                let steps = (length / (PPQ / 4)).clamp(1, 1024) as u16;
                let pattern = Pattern {
                    id: pid,
                    name,
                    color: None,
                    length_ticks: Tick(length),
                    steps,
                    step_ticks: Tick(PPQ / 4),
                    lanes: vec![Lane { channel_id: take.channel, notes: take.notes.clone(), controllers: take.controllers.clone() }],
                };
                batch.push(Command::AddPattern { pattern });
                // a clip at the take origin on the first track (created when the playlist is empty)
                let track_id = match self.project.tracks.first() {
                    Some(t) => t.id,
                    None => {
                        let id = TrackId::new();
                        batch.push(Command::AddTrack { track: Track { id, name: "Track 1".into(), color: None, mute: false, solo: false, height: None }, index: None });
                        id
                    }
                };
                batch.push(Command::AddClip {
                    clip: Clip {
                        id: ClipId::new(),
                        track_id,
                        start_tick: Tick(take.origin_tick),
                        length_ticks: Tick(length),
                        muted: false,
                        content: ClipContent::Pattern { pattern_id: pid, source_offset_ticks: Tick::ZERO, looped: false },
                    },
                });
                batch.push(Command::SelectPattern { id: Some(pid) });
            }
        }
        self.apply(Command::Batch { commands: batch });
        self.piano_roll.selection = note_ids.into_iter().collect();
        self.piano_roll.channel = Some(take.channel);
        let late = if take.late_events > 0 { format!(", {} late", take.late_events) } else { String::new() };
        self.status_line = format!("Recorded {count} note(s), {} controller event(s){late} — raw timing kept; use Quantize… (F7) to snap", take.controllers.len());
        if count > 0 && self.editor_view == EditorView::Rack {
            self.editor_view = EditorView::PianoRoll;
        }
    }

    /// MIDI learn arming (from a parameter's context menu).
    pub(crate) fn arm_midi_learn(&mut self, instance_id: InstanceId, param_id: u32) {
        self.midi_learn = Some(AutomationTarget { instance_id, param_id });
        self.status_line = "MIDI learn: move a controller…".into();
    }

    pub(crate) fn midi_mapping_for(&self, instance_id: InstanceId, param_id: u32) -> Option<&MidiMapping> {
        self.project.midi_mappings.iter().find(|m| m.target.instance_id == instance_id && m.target.param_id == param_id)
    }

    pub(crate) fn remove_midi_mapping(&mut self, instance_id: InstanceId, param_id: u32) {
        let mappings: Vec<MidiMapping> = self.project.midi_mappings.iter().filter(|m| !(m.target.instance_id == instance_id && m.target.param_id == param_id)).cloned().collect();
        self.apply(Command::SetMidiMappings { mappings });
    }

    /// Route a MIDI controller event: learning, or mapped parameter control.
    /// Returns true when the event was consumed by a mapping (not forwarded as a live CC).
    pub(crate) fn handle_midi_control(&mut self, ev: &TimedEvent) -> bool {
        let EventKind::Control { cc, value } = ev.kind else { return false };
        let EventSource::MidiDevice(dev) = ev.source else { return false };
        if let Some(target) = self.midi_learn.take() {
            let device = self.midi.devices().iter().find(|d| d.source_index == dev).map(|d| d.name.clone());
            let mut mappings = self.project.midi_mappings.clone();
            let replaced: Vec<String> = mappings.iter().filter(|m| m.cc == cc && (m.device.is_none() || m.device == device)).map(|m| format!("{}", m.target.param_id)).collect();
            mappings.retain(|m| !(m.cc == cc && (m.device.is_none() || m.device == device)));
            mappings.retain(|m| m.target != target);
            mappings.push(MidiMapping { id: MappingId::new(), device, channel: None, cc, target: target.clone(), min: 0.0, max: 1.0, invert: false });
            self.apply(Command::SetMidiMappings { mappings });
            self.status_line = if replaced.is_empty() {
                format!("Mapped CC {cc} → parameter {}", target.param_id)
            } else {
                format!("Mapped CC {cc} → parameter {} (replaced {} earlier mapping(s) of this CC)", target.param_id, replaced.len())
            };
            return true;
        }
        let device_name = self.midi.devices().iter().find(|d| d.source_index == dev).map(|d| d.name.clone());
        let hits: Vec<MidiMapping> = self
            .project
            .midi_mappings
            .iter()
            .filter(|m| m.cc == cc && (m.device.is_none() || m.device == device_name) && m.channel.is_none_or(|c| c == ev.channel))
            .cloned()
            .collect();
        if hits.is_empty() {
            return false;
        }
        for m in hits {
            let v = if m.invert { m.max - (m.max - m.min) * value } else { m.min + (m.max - m.min) * value };
            let v = v.clamp(0.0, 1.0);
            let id = ParamId(m.target.param_id);
            if let Some(s) = self.synth_mirrors.get_mut(&m.target.instance_id) {
                s.set_param(id, v);
            } else if let Some(o) = self.other_mirrors.get_mut(&m.target.instance_id) {
                o.set_param(id, v);
            }
            if let Some(ch) = self.project.channels.iter().find(|c| c.instrument.id == m.target.instance_id).map(|c| c.id) {
                self.send(EngineCommand::Live { channel: ch, event: TimedEvent::new(0, ev.source, ev.channel, EventKind::Param { id, value: v }) });
            }
            self.midi_dirty.insert(m.target.instance_id, Instant::now());
        }
        true
    }

    /// Commit MIDI-driven parameter changes once the controller has rested (one undo entry per burst).
    pub(crate) fn commit_quiet_midi_changes(&mut self) {
        let now = Instant::now();
        let due: Vec<InstanceId> = self.midi_dirty.iter().filter(|(_, t)| now.duration_since(**t) >= MIDI_COMMIT_QUIET).map(|(id, _)| *id).collect();
        for id in due {
            self.midi_dirty.remove(&id);
            let (type_id, bytes) = if let Some(s) = self.synth_mirrors.get(&id) {
                (rawdawg_synthesizer::TYPE_ID.to_owned(), s.save_state())
            } else if let Some(o) = self.other_mirrors.get(&id) {
                (Processor::type_id(o.as_ref()).to_owned(), o.save_state())
            } else {
                continue;
            };
            let state: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
            self.mark_live_state(id, &type_id, &state);
            if let Err(e) = self.history.apply(&mut self.project, Command::SetInstanceState { instance_id: id, state_version: 1, state }) {
                self.status_line = format!("MIDI edit rejected: {e}");
            }
        }
    }
}

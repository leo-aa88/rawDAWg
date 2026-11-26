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
    /// Playlist track armed for audio recording (its clips come from the input device).
    pub armed_track: Option<TrackId>,
    /// Running audio take.
    pub audio: Option<AudioTake>,
    /// Last take's report (lost frames, placement) for the status line.
    pub last_audio_report: Option<String>,
    /// Loopback latency measurement of a take (shown in its own window until closed).
    pub latency_check: Option<(ClipId, Result<crate::calibrate::LoopbackMeasurement, String>)>,
    /// Input level per channel (peak, falling back), sampled once per frame.
    pub input_meter: [f32; 2],
    /// The input reached full scale since the meter was last reset (click to reset).
    pub input_clipped: bool,
}

/// An audio take in progress: the disk writer, the target track and the aligned start.
pub(crate) struct AudioTake {
    pub writer: rawdawg_audio_io::capture_writer::CaptureWriter,
    pub track: TrackId,
    /// Project frame the first captured sample corresponds to (latency-aligned).
    pub start_frame: i64,
    pub sample_rate: u32,
}

/// Project frame the first captured sample belongs to: the transport position when the
/// take started, moved earlier by the output latency (what the performer heard was that
/// late), the input latency (the sound reached the converter before the callback delivered
/// it) and the user offset (positive = earlier). Never negative.
pub(crate) fn aligned_start_frame(position_frame: i64, output_latency_frames: i64, input_latency_frames: i64, offset_ms: f32, sample_rate: u32) -> i64 {
    let offset = (f64::from(offset_ms) / 1000.0 * f64::from(sample_rate)).round() as i64;
    (position_frame - output_latency_frames - input_latency_frames - offset).max(0)
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
                        batch.push(Command::AddTrack {
                            track: Track { id, name: "Track 1".into(), color: None, mute: false, solo: false, height: None, mixer_id: None },
                            index: None,
                        });
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

    /// Audio take lifecycle (every UI frame): start the disk writer when the transport
    /// records with an armed track and an input device, stop it when recording ends and
    /// place the take as an audio clip; drain the capture queue while idle.
    pub(crate) fn poll_audio_take(&mut self) {
        let (playing, frame, out_latency, in_latency_s, sample_rate, has_input) = match &self.audio {
            AudioState::Running(s) => (
                s.handle.status.playing.load(std::sync::atomic::Ordering::Relaxed),
                s.handle.status.position_frame.load(std::sync::atomic::Ordering::Relaxed),
                i64::from(s.handle.status.output_latency.load(std::sync::atomic::Ordering::Relaxed)),
                s.input.as_ref().map(|i| i.stream.status.input_latency_s.load()).unwrap_or(0.0),
                s.sample_rate.0,
                s.input.is_some(),
            ),
            _ => (false, 0, 0, 0.0, 48_000, false),
        };
        let recording = self.record.armed && playing && self.transport_mode == TransportMode::Song;
        if self.record.audio.is_none() {
            if let AudioState::Running(s) = &mut self.audio {
                s.drain_idle_capture();
            }
            if recording
                && has_input
                && let Some(track) = self.record.armed_track
            {
                self.start_audio_take(frame, out_latency, in_latency_s, sample_rate, track);
            }
        } else if !recording {
            self.finish_audio_take();
        }
    }

    fn start_audio_take(&mut self, frame: i64, out_latency: i64, in_latency_s: f64, sample_rate: u32, track: TrackId) {
        let Some(dir) = self.working_dir() else {
            self.status_line = "No writable folder for the recording".into();
            return;
        };
        let in_latency = (in_latency_s * f64::from(sample_rate)).round() as i64;
        let start_frame = aligned_start_frame(frame, out_latency, in_latency, self.prefs.record_offset_ms, sample_rate);
        let AudioState::Running(sys) = &mut self.audio else { return };
        let Some(input) = sys.input.as_mut() else { return };
        let Some(queue) = input.writer_queue.take() else { return };
        // drop whatever was captured before the take began
        let mut queue = queue;
        while queue.pop().is_some() {}
        match rawdawg_audio_io::capture_writer::CaptureWriter::start(&dir, queue, sample_rate, start_frame) {
            Ok(writer) => {
                self.record.audio = Some(AudioTake { writer, track, start_frame, sample_rate });
                self.status_line = "Recording audio…".into();
            }
            Err(e) => self.status_line = format!("Could not start the audio take: {e}"),
        }
    }

    fn finish_audio_take(&mut self) {
        let Some(take) = self.record.audio.take() else { return };
        let AudioTake { writer, track, start_frame, sample_rate } = take;
        let wav_path = writer.wav_path.clone();
        let journal_path = writer.journal_path.clone();
        match writer.stop() {
            Ok((journal, queue)) => {
                if let AudioState::Running(sys) = &mut self.audio
                    && let Some(input) = sys.input.as_mut()
                {
                    input.writer_queue = Some(queue);
                }
                if journal.frames_written == 0 {
                    let _ = std::fs::remove_file(&wav_path);
                    let _ = std::fs::remove_file(&journal_path);
                    self.status_line = "Audio take stopped: nothing captured".into();
                    return;
                }
                self.place_take(&wav_path, &journal_path, track, start_frame, journal.frames_written, journal.lost_frames, sample_rate);
            }
            Err(e) => self.status_line = format!("Audio take failed: {e} (the file stays in recovery/takes)"),
        }
    }

    /// Import a finished take as an asset and place it as a clip (one undo step).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn place_take(&mut self, wav_path: &std::path::Path, journal_path: &std::path::Path, track: TrackId, start_frame: i64, frames: u64, lost: u64, sample_rate: u32) {
        let Some(dir) = self.working_dir() else { return };
        let asset = match rawdawg_project::assets::import_asset(&dir, wav_path) {
            Ok(a) => a,
            Err(e) => {
                self.status_line = format!("Take kept in recovery/takes but could not be imported: {e}");
                return;
            }
        };
        let rate = rawdawg_types::SampleRate::new(sample_rate).unwrap_or(self.project.sample_rate_hz);
        let start_tick = self.project.tempo_map().map(|t| t.frame_to_tick_floor(start_frame, rate).0).unwrap_or(0).max(0);
        let out_frames = frames as f64 * self.project.sample_rate_hz.as_f64() / f64::from(sample_rate);
        let length_ticks = self
            .project
            .tempo_map()
            .map(|t| {
                (t.frame_to_tick_f64(t.tick_to_frame(Tick(start_tick), self.project.sample_rate_hz) + out_frames.round() as i64, self.project.sample_rate_hz) - start_tick as f64)
                    .round() as i64
            })
            .unwrap_or(PPQ)
            .max(1);
        let clip = Clip {
            id: ClipId::new(),
            track_id: track,
            start_tick: Tick(start_tick),
            length_ticks: Tick(length_ticks),
            muted: false,
            content: ClipContent::Audio {
                asset_id: asset.id.clone(),
                source_offset_frames: 0,
                source_length_frames: frames,
                gain_db: rawdawg_types::units::Db::UNITY,
                fade_in_frames: 0,
                fade_out_frames: 0,
                playback_rate: 1.0,
            },
        };
        let mut commands = Vec::new();
        if self.project.assets.iter().all(|a| a.id != asset.id) {
            commands.push(Command::AddAsset { asset });
        }
        commands.push(Command::AddClip { clip });
        self.apply(Command::Batch { commands });
        let _ = std::fs::remove_file(wav_path);
        let _ = std::fs::remove_file(journal_path);
        let report = if lost > 0 {
            format!(
                "Recorded {:.2} s at bar {} — {} frames were lost (queue overflow) and replaced by silence",
                frames as f64 / f64::from(sample_rate),
                start_tick / (PPQ * 4) + 1,
                lost
            )
        } else {
            format!("Recorded {:.2} s, placed at bar {}", frames as f64 / f64::from(sample_rate), start_tick / (PPQ * 4) + 1)
        };
        self.record.last_audio_report = Some(report.clone());
        self.status_line = report;
    }

    /// On open: unfinished takes left by a crash get their headers rebuilt and come back
    /// as clips on a "Recovered" track (nothing is deleted until the import succeeds).
    pub(crate) fn recover_takes(&mut self) {
        let Some(dir) = self.project_path.clone() else { return };
        let found = rawdawg_audio_io::capture_writer::scan_unfinished(&dir);
        if found.is_empty() {
            return;
        }
        let track = Track { id: TrackId::new(), name: "Recovered".into(), color: Some([0xE6, 0x6E, 0x6E]), mute: false, solo: false, height: None, mixer_id: None };
        let tid = track.id;
        self.apply(Command::AddTrack { track, index: None });
        let mut n = 0;
        for (journal_path, journal) in found {
            let wav = rawdawg_audio_io::capture_writer::takes_dir(&dir).join(&journal.wav);
            match rawdawg_audio_io::capture_writer::finalize_from_data(&wav) {
                Ok(frames) if frames > 0 => {
                    self.place_take(&wav, &journal_path, tid, journal.start_project_frame, frames, journal.lost_frames, journal.sample_rate);
                    n += 1;
                }
                Ok(_) => {}
                Err(e) => log::warn!("take {} not recoverable: {e}", journal.id),
            }
        }
        self.status_line = format!("Recovered {n} unfinished recording(s) onto the “Recovered” track");
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
            self.send(EngineCommand::Param { instance: m.target.instance_id, id, value: v });
            self.midi_dirty.insert(m.target.instance_id, Instant::now());
            // an armed Touch/Write curve records the controller like a mouse gesture
            super::automation::capture_edit(self, m.target.instance_id, m.target.param_id, v, false, false);
        }
        true
    }

    /// Commit MIDI-driven parameter changes once the controller has rested (one undo entry per burst).
    pub(crate) fn commit_quiet_midi_changes(&mut self) {
        let now = Instant::now();
        let due: Vec<InstanceId> = self.midi_dirty.iter().filter(|(_, t)| now.duration_since(**t) >= MIDI_COMMIT_QUIET).map(|(id, _)| *id).collect();
        for id in due {
            self.midi_dirty.remove(&id);
            // the controller rested: a touch capture ends here
            super::automation::release_touch(self, id);
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

impl App {
    /// Sample the input level once per frame (the meters in the toolbar, the armed track and
    /// Settings all read this; the stream's peak registers are reset here only).
    pub(crate) fn sample_input_meter(&mut self) {
        let read = match &self.audio {
            AudioState::Running(s) => s.input.as_ref().map(|i| {
                let st = &i.stream.status;
                let v = [st.peak_l.load(), st.peak_r.load()];
                st.peak_l.store(0.0);
                st.peak_r.store(0.0);
                v
            }),
            _ => None,
        };
        let m = &mut self.record;
        update_input_meter(&mut m.input_meter, &mut m.input_clipped, read);
    }

    /// Whether an input device is open (the input meters show).
    pub(crate) fn input_open(&self) -> bool {
        matches!(&self.audio, AudioState::Running(s) if s.input.is_some())
    }

    /// Measure where the metronome clicks of an audio take land (a loopback recording) and show
    /// the result with an offer to correct the recording offset.
    pub(crate) fn measure_take_latency(&mut self, clip_id: ClipId) {
        self.assets.sync(self.project_path.clone(), &self.project);
        let result = self.take_latency(clip_id);
        self.record.latency_check = Some((clip_id, result));
    }

    fn take_latency(&self, clip_id: ClipId) -> Result<crate::calibrate::LoopbackMeasurement, String> {
        use rawdawg_audio_engine::AssetResolver;
        let clip = self.project.clips.iter().find(|c| c.id == clip_id).ok_or("the take was removed")?;
        let ClipContent::Audio { asset_id, source_offset_frames, playback_rate, .. } = &clip.content else {
            return Err("not an audio clip".into());
        };
        let asset = self.assets.sample(asset_id).ok_or("the take is too long to measure here (or its file is missing): record 4 to 8 bars")?;
        let first = asset.channels.first().ok_or("the take has no audio")?;
        // one channel: the mix of all of them
        let n = first.len();
        let audio: Vec<f32> = (0..n).map(|i| asset.channels.iter().map(|c| c.get(i).copied().unwrap_or(0.0)).sum::<f32>() / asset.channels.len() as f32).collect();
        let tempo = self.project.tempo_map().map_err(|e| e.to_string())?;
        let meter = self.project.meter_map().map_err(|e| e.to_string())?;
        let rate = self.project.sample_rate_hz;
        let beat = meter.meter_at(clip.start_tick).beat_ticks().0.max(1);
        // the metronome clicks on multiples of the beat; where each lands in the take's file
        let start_frame = tempo.tick_to_frame(clip.start_tick, rate);
        let to_asset = f64::from(asset.sample_rate) / f64::from(rate.0) * playback_rate;
        let first_beat = (clip.start_tick.0 + beat - 1).div_euclid(beat) * beat;
        let beats: Vec<i64> = (0..)
            .map(|k| first_beat + k * beat)
            .take_while(|t| *t < clip.end_tick().0)
            .map(|t| ((tempo.tick_to_frame(Tick(t), rate) - start_frame) as f64 * to_asset).round() as i64 + *source_offset_frames as i64)
            .collect();
        crate::calibrate::measure_loopback(&audio, asset.sample_rate, &beats)
    }

    /// Apply a measurement: the clicks landed `late_ms` late, so recordings move that much
    /// earlier from now on.
    pub(crate) fn apply_latency_correction(&mut self, late_ms: f64) {
        let before = self.prefs.record_offset_ms;
        self.prefs.record_offset_ms = (f64::from(before) + late_ms).clamp(-100.0, 100.0) as f32;
        self.save_prefs();
        self.status_line = format!("Recording offset {before:.1} ms → {:.1} ms (new takes are placed with it)", self.prefs.record_offset_ms);
    }
}

/// One frame of the input meter: each channel holds its peak and falls back by a fixed factor
/// per frame; full scale lights the clip light; no input clears it; junk values read as silence.
pub(crate) fn update_input_meter(meter: &mut [f32; 2], clipped: &mut bool, read: Option<[f32; 2]>) {
    const FALL: f32 = 0.85;
    let Some(v) = read else {
        *meter = [0.0; 2];
        return;
    };
    for (m, x) in meter.iter_mut().zip(v) {
        let x = if x.is_finite() { x.abs() } else { 0.0 };
        *m = x.max(*m * FALL);
        if x >= 0.999 {
            *clipped = true;
        }
    }
}

/// A compact input level meter (left/right bars); click resets the clip light.
pub(crate) fn input_meter(app: &mut App, ui: &mut egui::Ui, width: f32) {
    use crate::theme;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 12.0), egui::Sense::click());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, theme::BACKGROUND);
    let clip_w = 6.0;
    let bars = egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x - clip_w - 2.0, rect.max.y));
    for (k, peak) in app.record.input_meter.iter().enumerate() {
        let db = rawdawg_dsp::math::lin_to_db(*peak).clamp(-60.0, 0.0);
        let frac = (db + 60.0) / 60.0;
        let y0 = bars.min.y + k as f32 * bars.height() / 2.0;
        let bar = egui::Rect::from_min_size(egui::pos2(bars.min.x, y0 + 0.5), egui::vec2(bars.width() * frac, bars.height() / 2.0 - 1.0));
        let color = if db > -1.0 {
            theme::ERROR
        } else if db > -6.0 {
            theme::WARNING
        } else {
            theme::ACCENT
        };
        painter.rect_filled(bar, 1.0, color);
    }
    let light = egui::Rect::from_min_max(egui::pos2(rect.max.x - clip_w, rect.min.y), rect.max);
    painter.rect_filled(light, 1.0, if app.record.input_clipped { theme::ERROR } else { theme::RAISED });
    let [l, r] = app.record.input_meter;
    let resp = resp.on_hover_text(format!(
        "Input level L {:.0} dB · R {:.0} dB{} (click to reset the clip light)",
        rawdawg_dsp::math::lin_to_db(l).max(-90.0),
        rawdawg_dsp::math::lin_to_db(r).max(-90.0),
        if app.record.input_clipped { " · clipped" } else { "" }
    ));
    if resp.clicked() {
        app.record.input_clipped = false;
    }
}

/// The measurement window: where the clicks landed, and Apply.
pub(crate) fn latency_window(app: &mut App, ctx: &egui::Context) {
    use crate::theme;
    let Some((_, result)) = app.record.latency_check.clone() else { return };
    let mut open = true;
    let mut apply = None;
    egui::Window::new("Recording latency").collapsible(false).resizable(false).open(&mut open).show(ctx, |ui| {
        match &result {
            Ok(m) => {
                let when = if m.late_ms.abs() < 0.05 {
                    "on the beat".to_owned()
                } else if m.late_ms > 0.0 {
                    format!("{:.1} ms late", m.late_ms)
                } else {
                    format!("{:.1} ms early", -m.late_ms)
                };
                ui.label(format!("The clicks land {when} ({} clicks, spread {:.1} ms).", m.clicks, m.spread_ms));
                if m.spread_ms > crate::calibrate::MAX_RELIABLE_SPREAD_MS {
                    ui.colored_label(theme::WARNING, "The clicks are irregular: this does not look like a loopback of the metronome. Check the cable, turn the metronome on, and record again.");
                }
                let new = (f64::from(app.prefs.record_offset_ms) + m.late_ms).clamp(-100.0, 100.0);
                ui.label(egui::RichText::new(format!("Recording offset now {:.1} ms; with the correction {new:.1} ms.", app.prefs.record_offset_ms)).color(theme::TEXT_SECONDARY));
                if ui.button("Apply to new recordings").clicked() {
                    apply = Some(m.late_ms);
                }
            }
            Err(e) => {
                ui.colored_label(theme::WARNING, format!("Could not measure: {e}"));
            }
        }
        ui.label(
            egui::RichText::new("How: turn the metronome on, connect an output to the input with a cable (or hold the microphone to a speaker), arm a track, record a few bars, then measure the take.")
                .small()
                .color(theme::TEXT_SECONDARY),
        );
    });
    if let Some(late) = apply {
        app.apply_latency_correction(late);
        open = false;
    }
    if !open {
        app.record.latency_check = None;
    }
}

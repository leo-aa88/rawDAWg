//! GUI shell (M1): dark workspace skeleton, transport, typing/MIDI/on-screen
//! keyboards, synth parameter panel, settings, status.

mod automation;
mod effect_panel;
mod evolve;
mod export;
mod graph_editor;
mod instrument_panel;
mod keyboard;
mod mixer;
mod piano_roll;
mod playlist;
mod presets;
mod recording;
mod sequencer;
mod settings;
mod synth_panel;
#[cfg(test)]
mod tests;
mod waveform;

use crate::audio::AudioSystem;
use crate::prefs::Preferences;
use crate::theme;
use rawdawg_audio_engine::commands::{EngineCommand, TransportMode};
use rawdawg_audio_engine::plan::compile;
use rawdawg_audio_io::stream::StreamConfigRequest;
use rawdawg_audio_io::{DeviceSnapshot, enumerate};
use rawdawg_dsp::processor::Processor;
use rawdawg_midi::MidiInputManager;
use rawdawg_project::{Command, History, Project};
use rawdawg_synthesizer::Synth;
use rawdawg_types::events::{EventSource, TimedEvent};
use rawdawg_types::limits::EngineLimits;
use rawdawg_types::{ChannelId, PRODUCT_NAME, TAGLINE, VERSION};
use std::collections::HashMap;
use std::path::PathBuf;

pub struct GuiOptions {
    pub open: Option<PathBuf>,
    pub safe_mode: bool,
    pub device: Option<String>,
    pub buffer: Option<u32>,
    pub no_audio: bool,
    /// Capture a screenshot to this path after the delay, then exit.
    pub screenshot: Option<(PathBuf, std::time::Duration)>,
    /// Initial UI state preset (`advanced`, `graph`).
    pub ui_preset: Option<String>,
}

pub fn run_gui(opts: GuiOptions) -> anyhow::Result<()> {
    let viewport = egui::ViewportBuilder::default().with_title(PRODUCT_NAME).with_app_id("rawdawg").with_inner_size([1440.0, 900.0]).with_min_inner_size([1024.0, 720.0]);
    let native = eframe::NativeOptions { viewport, centered: true, ..Default::default() };
    eframe::run_native(PRODUCT_NAME, native, Box::new(move |cc| Ok(Box::new(App::new(cc, opts))))).map_err(|e| anyhow::anyhow!("GUI failed to start: {e}"))?;
    Ok(())
}

pub(crate) enum AudioState {
    Disabled,
    Running(Box<AudioSystem>),
    Failed(String),
}

/// Pointer snapshot for custom editors (press/release are edge events of this frame).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PointerState {
    pub pos: Option<egui::Pos2>,
    pub primary_pressed: bool,
    pub secondary_pressed: bool,
    pub primary_down: bool,
    pub secondary_down: bool,
    pub primary_released: bool,
    pub secondary_released: bool,
}

impl PointerState {
    pub fn read(ui: &egui::Ui) -> Self {
        ui.input(|i| Self {
            pos: i.pointer.latest_pos(),
            primary_pressed: i.pointer.button_pressed(egui::PointerButton::Primary),
            secondary_pressed: i.pointer.button_pressed(egui::PointerButton::Secondary),
            primary_down: i.pointer.primary_down(),
            secondary_down: i.pointer.secondary_down(),
            primary_released: i.pointer.button_released(egui::PointerButton::Primary),
            secondary_released: i.pointer.button_released(egui::PointerButton::Secondary),
        })
    }
    pub fn any_down(&self) -> bool {
        self.primary_down || self.secondary_down
    }
    pub fn any_released(&self) -> bool {
        self.primary_released || self.secondary_released
    }
}

/// Which custom editor receives editing keys when the pointer is not over one.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum KeyTarget {
    #[default]
    None,
    Playlist,
    PianoRoll,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum EditorView {
    Rack,
    PianoRoll,
    Synth,
    Automation,
    Mixer,
    Effect,
}

pub(crate) struct App {
    pub project: Project,
    pub history: History,
    pub project_path: Option<PathBuf>,
    pub piano_roll: piano_roll::PianoRollState,
    pub playlist: playlist::PlaylistState,
    pub sequencer: sequencer::StepState,
    pub transport_mode: TransportMode,
    pub loop_enabled: bool,
    pub record: recording::RecordState,
    pub automation: automation::AutomationState,
    pub key_target: KeyTarget,
    pub mixer: mixer::MixerState,
    pub effect_panel: effect_panel::EffectPanelState,
    pub effect_editor: Option<rawdawg_types::InstanceId>,
    /// Initial editor panel height requested by a `--ui-preset`.
    pub editor_height_hint: Option<f32>,
    /// Processor slot of every instance in the live plan (meters, automation).
    pub slots: HashMap<rawdawg_types::InstanceId, usize>,
    pub midi_learn: Option<rawdawg_project::AutomationTarget>,
    pub midi_dirty: HashMap<rawdawg_types::InstanceId, std::time::Instant>,
    persist_prefs: bool,
    pub devices: DeviceSnapshot,
    pub audio: AudioState,
    pub limits: EngineLimits,
    pub plan_revision: u64,
    pub plan_error: Option<String>,
    pub prefs: Preferences,
    pub typing_enabled: bool,
    /// Held typing notes: key → (note id, pitch) so release uses the original pitch.
    held: HashMap<egui::Key, (u32, u8)>,
    pub next_note_id: u32,
    pub safe_mode: bool,
    pub status_line: String,
    pub midi: MidiInputManager,
    pub midi_activity: u64,
    pub selected_channel: Option<ChannelId>,
    pub show_settings: bool,
    pub(crate) export: export::ExportState,
    /// Browser search text (filters samples and effects).
    pub browser_search: String,
    /// Assets handed to the engine for preview: held here until the preview ended so the
    /// audio thread never drops the last reference.
    preview_held: Vec<std::sync::Arc<rawdawg_sampler::SampleAsset>>,
    pub editor_view: EditorView,
    /// Main-thread mirrors of native synth state per instance (param edits, commits).
    pub synth_mirrors: HashMap<rawdawg_types::InstanceId, Synth>,
    /// Fingerprint of the state each live processor is known to hold (carry-over decisions).
    pub live_fingerprints: HashMap<rawdawg_types::InstanceId, u64>,
    /// Live wavetable channels into running synth instances.
    pub table_senders: HashMap<rawdawg_types::InstanceId, rawdawg_synthesizer::TableSender>,
    /// Decoded project assets (samples, SoundFonts), synced to the open project.
    pub assets: rawdawg_audio_engine::ProjectAssetCache,
    /// Main-thread mirrors for non-synth native instruments.
    pub other_mirrors: HashMap<rawdawg_types::InstanceId, Box<dyn rawdawg_dsp::processor::Processor>>,
    pub instrument_panel: instrument_panel::InstrumentPanelState,
    pub import_path: String,
    pub synth_panel: synth_panel::SynthPanelState,
    pub keyboard: keyboard::KeyboardState,
    pub settings: settings::SettingsState,
    midi_rescan_counter: u32,
    /// Set after the one automatic retry with the device-default buffer.
    buffer_fallback_tried: bool,
    screenshot: Option<(PathBuf, std::time::Duration)>,
    started: std::time::Instant,
    screenshot_requested: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, opts: GuiOptions) -> Self {
        Self::build(&cc.egui_ctx, opts, Preferences::load(), true)
    }

    /// Build the app for a context; `persist_prefs` is false for headless UI tests.
    pub(crate) fn build(ctx: &egui::Context, opts: GuiOptions, prefs: Preferences, persist_prefs: bool) -> Self {
        theme::apply(ctx);
        let limits = EngineLimits::DEFAULT;
        let mut prefs = prefs;
        let (project, project_path, mut status_line) = match &opts.open {
            Some(p) => match rawdawg_project::load_project_dir(p) {
                Ok(proj) => {
                    prefs.remember_project(p.clone());
                    (proj, Some(p.clone()), format!("Opened {}", p.display()))
                }
                Err(e) => (default_project(), None, format!("Could not open {}: {e}", p.display())),
            },
            None => (default_project(), None, "Ready — hold Z X C V B N M to play".to_owned()),
        };
        let devices = enumerate();
        let req = StreamConfigRequest {
            device_id: opts.device.clone().or(prefs.audio_device_id.clone()),
            sample_rate_hz: prefs.sample_rate_hz,
            buffer_frames: opts.buffer.or(prefs.buffer_frames).or(Some(256)),
        };
        let audio = if opts.no_audio {
            AudioState::Disabled
        } else {
            let input_req = prefs.input_enabled.then(|| rawdawg_audio_io::capture::InputConfigRequest {
                device_id: prefs.input_device_id.clone(),
                sample_rate_hz: None,
                buffer_frames: req.buffer_frames,
                channels: prefs.input_channels,
            });
            match AudioSystem::start(&req, input_req.as_ref(), &limits) {
                Ok(sys) => {
                    if let Some(n) = &sys.note {
                        status_line = n.clone();
                    }
                    AudioState::Running(Box::new(sys))
                }
                Err(e) => {
                    status_line = format!("Audio unavailable: {e:#}");
                    AudioState::Failed(format!("{e:#}"))
                }
            }
        };
        let mut midi = MidiInputManager::new();
        for id in prefs.midi_enabled.clone() {
            if let Err(e) = midi.enable(&id) {
                log::info!("MIDI device {id} not enabled: {e}");
            }
        }
        let selected_channel = project.channels.first().map(|c| c.id);
        let mut app = Self {
            project,
            history: History::new(),
            project_path,
            devices,
            audio,
            limits,
            plan_revision: 0,
            plan_error: None,
            typing_enabled: true,
            held: HashMap::new(),
            next_note_id: 1,
            safe_mode: opts.safe_mode,
            status_line,
            midi,
            midi_activity: 0,
            selected_channel,
            show_settings: false,
            export: export::ExportState::default(),
            browser_search: String::new(),
            preview_held: Vec::new(),
            editor_view: EditorView::Synth,
            synth_mirrors: HashMap::new(),
            live_fingerprints: HashMap::new(),
            table_senders: HashMap::new(),
            assets: rawdawg_audio_engine::ProjectAssetCache::new(None),
            other_mirrors: HashMap::new(),
            instrument_panel: Default::default(),
            import_path: String::new(),
            synth_panel: Default::default(),
            keyboard: Default::default(),
            settings: settings::SettingsState::from_request(&req),
            midi_rescan_counter: 0,
            buffer_fallback_tried: false,
            screenshot: opts.screenshot.clone(),
            started: std::time::Instant::now(),
            screenshot_requested: false,
            prefs,
            piano_roll: Default::default(),
            playlist: Default::default(),
            sequencer: Default::default(),
            transport_mode: TransportMode::Pattern,
            loop_enabled: true,
            record: Default::default(),
            automation: Default::default(),
            key_target: KeyTarget::None,
            mixer: mixer::MixerState::new(),
            effect_panel: Default::default(),
            effect_editor: None,
            editor_height_hint: None,
            slots: HashMap::new(),
            midi_learn: None,
            midi_dirty: HashMap::new(),
            persist_prefs,
        };
        app.rebuild_mirrors();
        app.recompile();
        match opts.ui_preset.as_deref() {
            Some("advanced") => app.synth_panel.advanced = true,
            Some("rack") => app.editor_view = EditorView::Rack,
            Some("song") => {
                app.editor_view = EditorView::Rack;
                app.transport_mode = TransportMode::Song;
            }
            Some("piano-roll") => app.editor_view = EditorView::PianoRoll,
            Some("mixer") => {
                app.editor_view = EditorView::Mixer;
                app.editor_height_hint = Some(620.0);
                app.mixer.selected = app.project.mixer.channels.iter().find(|m| !m.inserts.is_empty()).map(|m| m.id);
            }
            Some("effect") => {
                app.editor_height_hint = Some(520.0);
                app.effect_editor = app.project.mixer.channels.iter().flat_map(|m| m.inserts.iter()).map(|i| i.id).next();
                app.editor_view = EditorView::Effect;
            }
            Some(p) if p.starts_with("effect:") => {
                // open a fresh insert of the named effect on the first mixer channel (demo/screenshot aid)
                let type_id = &p["effect:".len()..];
                if let (Some(fx), Some(mixer_id)) = (rawdawg_effects::CATALOG.iter().find(|f| f.type_id == type_id), app.project.mixer.channels.first().map(|m| m.id)) {
                    let mut inst = rawdawg_project::Instance::native(fx.type_id, serde_json::Value::Null);
                    inst.display_name = fx.name.to_owned();
                    let iid = inst.id;
                    app.apply(rawdawg_project::Command::AddInsert { mixer_id, instance: inst, index: None });
                    app.effect_editor = Some(iid);
                    app.editor_height_hint = Some(560.0);
                    app.editor_view = EditorView::Effect;
                } else {
                    app.status_line = format!("Unknown effect {type_id}");
                }
            }
            Some("settings") => {
                app.show_settings = true;
                app.prefs.input_enabled = true;
            }
            Some("export") => {
                app.export.open = true;
                app.export.stems = true;
                app.transport_mode = TransportMode::Song;
            }
            Some("automation") => {
                app.editor_view = EditorView::Automation;
                app.automation.curve = app.project.automation.first().map(|c| c.id);
                app.transport_mode = TransportMode::Song;
            }
            Some("graph") => {
                app.synth_panel.advanced = true;
                app.synth_panel.graph.scope = rawdawg_synthesizer::graph::Scope::Voice;
                app.synth_panel.graph.focus_frames = 4;
            }
            Some(other) => app.status_line = format!("Unknown --ui-preset {other}"),
            None => {}
        }
        app
    }

    /// Recreate main-thread synth mirrors from the project's instrument states.
    pub(crate) fn rebuild_mirrors(&mut self) {
        // assets must resolve even when no audio device is open (editing/offline mode)
        self.assets.sync(self.project_path.clone(), &self.project);
        self.synth_mirrors.clear();
        self.other_mirrors.clear();
        for c in &self.project.channels {
            if c.instrument.type_id.as_str() == rawdawg_types::ids::native_types::SYNTH {
                let mut s = Synth::default();
                if let Ok(bytes) = serde_json::to_vec(&c.instrument.state)
                    && !c.instrument.state.is_null()
                    && c.instrument.state.as_object().is_some_and(|o| !o.is_empty())
                    && let Err(e) = s.load_state(&bytes)
                {
                    log::warn!("instrument state for {} invalid: {e}; using default patch", c.name);
                }
                self.synth_mirrors.insert(c.instrument.id, s);
            }
        }
    }

    /// Compile the project and hand the plan to the engine (off the audio thread).
    pub(crate) fn recompile(&mut self) {
        let AudioState::Running(sys) = &mut self.audio else { return };
        let mut project = self.project.clone();
        project.sample_rate_hz = sys.sample_rate;
        self.plan_revision += 1;
        self.assets.sync(self.project_path.clone(), &self.project);
        match compile(&project, self.plan_revision, rawdawg_audio_io::stream::MAX_CHUNK_FRAMES, &self.limits, &self.live_fingerprints, &self.assets) {
            Ok(out) => {
                sys.handle.collect_retired();
                match sys.handle.publish_plan(out.plan) {
                    Ok(_) => {
                        self.plan_error = None;
                        self.live_fingerprints = out.fingerprints;
                        self.slots = out.slots;
                        for (id, sender) in out.table_senders {
                            self.table_senders.insert(id, sender);
                        }
                        self.table_senders.retain(|id, _| self.live_fingerprints.contains_key(id));
                    }
                    Err(_) => self.plan_error = Some("engine busy; change not applied yet".to_owned()),
                }
            }
            Err(e) => self.plan_error = Some(e.to_string()),
        }
    }

    /// Record that the live processor already holds `state` (applied through live
    /// events or table handoff), so the next recompile carries it over.
    pub(crate) fn mark_live_state(&mut self, instance_id: rawdawg_types::InstanceId, type_id: &str, state: &serde_json::Value) {
        self.live_fingerprints.insert(instance_id, rawdawg_audio_engine::fingerprint_state(type_id, state));
    }

    pub(crate) fn send(&mut self, cmd: EngineCommand) {
        if let AudioState::Running(sys) = &mut self.audio {
            let _ = sys.handle.send(cmd);
        }
    }

    /// Apply a persistent command as one undo entry and recompile.
    pub(crate) fn apply(&mut self, cmd: Command) {
        match self.history.apply(&mut self.project, cmd) {
            Ok(()) => {
                self.rebuild_mirrors_if_needed();
                self.recompile();
            }
            Err(e) => self.status_line = format!("Edit rejected: {e}"),
        }
    }

    fn rebuild_mirrors_if_needed(&mut self) {
        self.other_mirrors.retain(|id, _| self.project.instance(*id).is_some());
        let synth_ids: Vec<_> =
            self.project.channels.iter().filter(|c| c.instrument.type_id.as_str() == rawdawg_types::ids::native_types::SYNTH).map(|c| c.instrument.id).collect();
        if synth_ids.len() != self.synth_mirrors.len() || synth_ids.iter().any(|i| !self.synth_mirrors.contains_key(i)) {
            self.rebuild_mirrors();
        }
    }

    /// Folder that holds this project's managed assets; unsaved projects get a
    /// per-user working folder so imports never depend on naming the project first.
    pub(crate) fn working_dir(&mut self) -> Option<PathBuf> {
        if let Some(p) = &self.project_path {
            return Some(p.clone());
        }
        let dir = directories::ProjectDirs::from("", "", rawdawg_types::PACKAGE_NAME)?.data_dir().join("unsaved").join(format!("{}.rawdawg", self.project.project_id));
        if rawdawg_project::save_project_dir(&dir, &self.project).is_err() {
            return None;
        }
        self.project_path = Some(dir.clone());
        self.status_line = format!("Working folder {}", dir.display());
        Some(dir)
    }

    /// Import a WAV/FLAC/MP3 as a sampler channel or an .sf2 as a SoundFont channel.
    /// Sample rate used for display when audio is off.
    pub(crate) fn audio_sample_rate(&self) -> f32 {
        match &self.audio {
            AudioState::Running(s) => s.sample_rate.0 as f32,
            _ => 48_000.0,
        }
    }

    /// Import a Standard MIDI File: one synth channel (and mixer channel) per imported
    /// track, one pattern holding every track's notes/controllers, a playlist track with
    /// the pattern as a clip, and — when the project has no notes yet — its tempo and
    /// meter maps. One undo step. Warnings go to the status line.
    pub(crate) fn import_midi_file(&mut self, path: &std::path::Path) {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                self.status_line = format!("Could not read {}: {e}", path.display());
                return;
            }
        };
        let name = path.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "MIDI".into());
        let imp = match rawdawg_project::smf::import(&bytes, &name) {
            Ok(i) => i,
            Err(e) => {
                self.status_line = format!("MIDI import failed: {e}");
                return;
            }
        };
        let master = self.project.mixer.master_id;
        let patch = serde_json::to_value(rawdawg_synthesizer::Patch::default()).unwrap_or(serde_json::Value::Null);
        let mut commands = Vec::new();
        let mut lanes = Vec::new();
        for t in &imp.tracks {
            let mixer = rawdawg_project::MixerChannel {
                id: rawdawg_types::MixerChannelId::new(),
                name: t.name.clone(),
                color: None,
                gain_db: rawdawg_types::units::Db::UNITY,
                pan: rawdawg_types::units::Pan::CENTER,
                mute: false,
                solo: false,
                inserts: vec![],
                sends: vec![],
                output_id: Some(master),
            };
            let label = match t.program {
                Some(pg) => format!("{} (program {})", t.name, pg + 1),
                None => t.name.clone(),
            };
            let channel = rawdawg_project::Channel {
                id: rawdawg_types::ChannelId::new(),
                name: label,
                color: None,
                instrument: rawdawg_project::Instance::native(rawdawg_types::ids::native_types::SYNTH, patch.clone()),
                gain_db: rawdawg_types::units::Db::UNITY,
                pan: rawdawg_types::units::Pan::CENTER,
                mute: false,
                solo: false,
                mixer_id: mixer.id,
            };
            lanes.push(rawdawg_project::Lane { channel_id: channel.id, notes: t.notes.clone(), controllers: t.controllers.clone() });
            commands.push(Command::AddMixerChannel { channel: mixer, index: None });
            commands.push(Command::AddChannel { channel, index: None });
        }
        let pattern = rawdawg_project::Pattern {
            id: rawdawg_types::PatternId::new(),
            name: name.clone(),
            color: None,
            length_ticks: imp.length_ticks,
            steps: 16,
            step_ticks: rawdawg_types::units::Tick(rawdawg_types::units::PPQ / 4),
            lanes,
        };
        let pid = pattern.id;
        commands.push(Command::AddPattern { pattern });
        let track = rawdawg_project::Track { id: rawdawg_types::TrackId::new(), name: name.clone(), color: None, mute: false, solo: false, height: None, mixer_id: None };
        let tid = track.id;
        commands.push(Command::AddTrack { track, index: None });
        commands.push(Command::AddClip {
            clip: rawdawg_project::Clip {
                id: rawdawg_types::ClipId::new(),
                track_id: tid,
                start_tick: rawdawg_types::units::Tick::ZERO,
                length_ticks: imp.length_ticks,
                muted: false,
                content: rawdawg_project::ClipContent::Pattern { pattern_id: pid, source_offset_ticks: rawdawg_types::units::Tick::ZERO, looped: false },
            },
        });
        let project_empty = self.project.patterns.iter().all(|p| p.lanes.iter().all(|l| l.notes.is_empty()));
        let mut took_maps = false;
        if project_empty {
            commands.push(Command::SetTempoMap { tempo: imp.tempo.clone() });
            commands.push(Command::SetMeterMap { meter: imp.meter.clone() });
            took_maps = true;
        }
        self.apply(Command::Batch { commands });
        self.project.selected_pattern = Some(pid);
        let mut msg = format!("Imported {} MIDI track(s) from {} as pattern “{name}” on a new playlist track", imp.tracks.len(), path.display());
        if took_maps {
            msg.push_str("; tempo and meter maps taken from the file");
        } else if imp.tempo.len() > 1 || imp.meter.len() > 1 {
            msg.push_str("; the file's tempo/meter changes were not applied (project already has notes)");
        }
        if !imp.warnings.is_empty() {
            msg.push_str(&format!(" — {}", imp.warnings.join("; ")));
        }
        self.status_line = msg;
    }

    /// Import a file into the project's managed assets (no channel). Returns the asset id.
    pub(crate) fn import_asset_only(&mut self, path: &std::path::Path) -> Result<String, String> {
        let dir = self.working_dir().ok_or("No writable folder for assets")?;
        let asset = rawdawg_project::assets::import_asset(&dir, path).map_err(|e| format!("Import failed: {e}"))?;
        let id = asset.id.to_string();
        if self.project.assets.iter().all(|a| a.id != asset.id) {
            self.apply(Command::AddAsset { asset });
        }
        Ok(id)
    }

    pub(crate) fn import_file_as_channel(&mut self, path: &std::path::Path) {
        let Some(dir) = self.working_dir() else {
            self.status_line = "No writable folder for assets".into();
            return;
        };
        let asset = match rawdawg_project::assets::import_asset(&dir, path) {
            Ok(a) => a,
            Err(e) => {
                self.status_line = format!("Import failed: {e}");
                return;
            }
        };
        let name = path.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Sample".into());
        let (type_id, state) = match asset.kind {
            rawdawg_project::AssetKind::Sf2 => (
                rawdawg_types::ids::native_types::SF2,
                serde_json::to_value(rawdawg_sampler::Sf2Patch { asset_id: Some(asset.id.clone()), ..Default::default() }).unwrap_or(serde_json::Value::Null),
            ),
            rawdawg_project::AssetKind::Audio => (
                rawdawg_types::ids::native_types::SAMPLER,
                serde_json::to_value(rawdawg_sampler::SamplerPatch { asset_id: Some(asset.id.clone()), name: name.clone(), ..Default::default() })
                    .unwrap_or(serde_json::Value::Null),
            ),
            rawdawg_project::AssetKind::Midi => {
                self.import_midi_file(path);
                return;
            }
            _ => {
                self.status_line = format!("{} is not a sound, SoundFont or MIDI file", path.display());
                return;
            }
        };
        let master = self.project.mixer.master_id;
        let mixer = rawdawg_project::MixerChannel {
            id: rawdawg_types::MixerChannelId::new(),
            name: name.clone(),
            color: None,
            gain_db: rawdawg_types::units::Db::UNITY,
            pan: rawdawg_types::units::Pan::CENTER,
            mute: false,
            solo: false,
            inserts: vec![],
            sends: vec![],
            output_id: Some(master),
        };
        let channel = rawdawg_project::Channel {
            id: rawdawg_types::ChannelId::new(),
            name,
            color: None,
            instrument: rawdawg_project::Instance::native(type_id, state),
            gain_db: rawdawg_types::units::Db::UNITY,
            pan: rawdawg_types::units::Pan::CENTER,
            mute: false,
            solo: false,
            mixer_id: mixer.id,
        };
        let new_id = channel.id;
        self.apply(Command::Batch {
            commands: vec![Command::AddAsset { asset }, Command::AddMixerChannel { channel: mixer, index: None }, Command::AddChannel { channel, index: None }],
        });
        self.selected_channel = Some(new_id);
        self.editor_view = EditorView::Synth;
        let problems = rawdawg_audio_engine::AssetResolver::problems(&self.assets);
        self.status_line = if problems.is_empty() { format!("Imported {}", path.display()) } else { problems.join("; ") };
    }

    pub(crate) fn undo(&mut self) {
        if let Ok(label) = self.history.undo(&mut self.project) {
            self.status_line = format!("Undid: {label}");
            self.rebuild_mirrors();
            self.recompile();
        }
    }

    pub(crate) fn redo(&mut self) {
        if let Ok(label) = self.history.redo(&mut self.project) {
            self.status_line = format!("Redid: {label}");
            self.rebuild_mirrors();
            self.recompile();
        }
    }

    pub(crate) fn alloc_note_id(&mut self) -> u32 {
        let id = self.next_note_id;
        self.next_note_id = self.next_note_id.wrapping_add(1).max(1);
        id
    }

    pub(crate) fn live(&mut self, event: TimedEvent) {
        if let Some(channel) = self.selected_channel {
            self.send(EngineCommand::Live { channel, event });
        }
    }

    fn typing_pitch(key: egui::Key) -> Option<i32> {
        use egui::Key::*;
        Some(match key {
            Z => 0,
            S => 1,
            X => 2,
            D => 3,
            C => 4,
            V => 5,
            G => 6,
            B => 7,
            H => 8,
            N => 9,
            J => 10,
            M => 11,
            Comma => 12,
            L => 13,
            Period => 14,
            Semicolon => 15,
            Slash => 16,
            _ => return None,
        })
    }

    fn handle_typing(&mut self, ctx: &egui::Context) {
        let text_focused = ctx.memory(|m| m.focused().is_some());
        let modifiers = ctx.input(|i| i.modifiers);
        let release_all = !self.typing_enabled || text_focused || !ctx.input(|i| i.focused) || self.selected_channel.is_none();
        let mut to_send: Vec<TimedEvent> = Vec::new();
        if release_all {
            for (_, (id, pitch)) in self.held.drain() {
                to_send.push(TimedEvent::note_off(0, EventSource::TypingKeyboard, 0, id, pitch));
            }
        } else {
            let events: Vec<egui::Event> = ctx.input(|i| i.events.clone());
            for ev in events {
                if let egui::Event::Key { key, pressed, repeat, .. } = ev {
                    if repeat {
                        continue;
                    }
                    if pressed {
                        if modifiers.ctrl || modifiers.alt || modifiers.command {
                            continue;
                        }
                        let Some(semitone) = Self::typing_pitch(key) else { continue };
                        if self.held.contains_key(&key) {
                            continue;
                        }
                        let pitch = 60 + self.prefs.typing_octave * 12 + semitone;
                        if !(0..=127).contains(&pitch) {
                            continue;
                        }
                        let id = self.alloc_note_id();
                        self.held.insert(key, (id, pitch as u8));
                        to_send.push(TimedEvent::note_on(0, EventSource::TypingKeyboard, 0, id, pitch as u8, self.prefs.typing_velocity));
                    } else if let Some((id, pitch)) = self.held.remove(&key) {
                        to_send.push(TimedEvent::note_off(0, EventSource::TypingKeyboard, 0, id, pitch));
                    }
                }
            }
        }
        for ev in to_send {
            self.live(ev);
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let text_focused = ctx.memory(|m| m.focused().is_some());
        if text_focused {
            return;
        }
        let (undo, redo, space, f6, f7, f8, f9, esc) = ctx.input_mut(|i| {
            (
                i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::Z)),
                i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::Z))
                    || i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::Y)),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Space),
                i.consume_key(egui::Modifiers::NONE, egui::Key::F6),
                i.consume_key(egui::Modifiers::NONE, egui::Key::F7),
                i.consume_key(egui::Modifiers::NONE, egui::Key::F8),
                i.consume_key(egui::Modifiers::NONE, egui::Key::F9),
                // Escape cancels a piano-roll gesture first; otherwise it is the panic key
                if self.piano_roll.gesture_active() || self.playlist.gesture_active() || self.automation.gesture_active() {
                    false
                } else {
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)
                },
            )
        });
        if redo {
            self.redo();
        } else if undo {
            self.undo();
        }
        if space {
            if self.is_playing() {
                self.send(EngineCommand::Stop);
            } else {
                self.play_pressed();
            }
        }
        if f6 {
            self.editor_view = EditorView::Rack;
        }
        if f7 {
            self.editor_view = EditorView::PianoRoll;
        }
        if f8 {
            self.editor_view = EditorView::Synth;
        }
        if f9 {
            self.editor_view = EditorView::Mixer;
        }
        if esc {
            self.send(EngineCommand::Panic { source: None });
        }
    }

    fn poll_midi(&mut self) {
        self.midi_rescan_counter += 1;
        if self.midi_rescan_counter.is_multiple_of(60) {
            self.midi.rescan();
        }
        let events = self.midi.poll();
        if !events.is_empty() {
            self.midi_activity = self.midi_activity.wrapping_add(events.len() as u64);
        }
        for (ev, _ts) in events {
            if self.handle_midi_control(&ev) {
                continue;
            }
            self.live(ev);
        }
    }

    pub(crate) fn is_playing(&self) -> bool {
        match &self.audio {
            AudioState::Running(s) => s.handle.status.playing.load(std::sync::atomic::Ordering::Relaxed),
            _ => false,
        }
    }

    /// Engine playhead in project ticks (None without a running engine).
    pub(crate) fn playhead_tick(&self) -> Option<i64> {
        match &self.audio {
            AudioState::Running(s) => Some(s.handle.status.position_tick.load(std::sync::atomic::Ordering::Relaxed)),
            _ => None,
        }
    }

    /// Restart audio with the given request (settings Apply / retry). Persists the
    /// request as the user's preference.
    pub(crate) fn restart_audio(&mut self, req: &StreamConfigRequest) {
        self.buffer_fallback_tried = false;
        self.restart_audio_with(req, true);
    }

    fn restart_audio_with(&mut self, req: &StreamConfigRequest, persist: bool) {
        self.held.clear();
        self.audio = AudioState::Disabled; // drops the old stream first
        let input_req = self.prefs.input_enabled.then(|| rawdawg_audio_io::capture::InputConfigRequest {
            device_id: self.prefs.input_device_id.clone(),
            sample_rate_hz: None,
            buffer_frames: req.buffer_frames,
            channels: self.prefs.input_channels,
        });
        match AudioSystem::start(req, input_req.as_ref(), &self.limits) {
            Ok(sys) => {
                self.status_line = match &sys.note {
                    Some(n) => n.clone(),
                    None => format!("Audio: {} at {} Hz", sys.stream.config.device_name, sys.stream.config.sample_rate_hz),
                };
                self.audio = AudioState::Running(Box::new(sys));
                if persist {
                    self.prefs.audio_device_id = req.device_id.clone();
                    self.prefs.sample_rate_hz = req.sample_rate_hz;
                    self.prefs.buffer_frames = req.buffer_frames;
                    if let Err(e) = self.prefs.save() {
                        log::warn!("could not save preferences: {e}");
                    }
                }
                self.recompile();
            }
            Err(e) => {
                self.status_line = format!("Audio unavailable: {e:#}");
                self.audio = AudioState::Failed(format!("{e:#}"));
            }
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("raw").strong().size(18.0));
            ui.label(egui::RichText::new("DAW").strong().size(18.0).color(theme::ACCENT));
            ui.label(egui::RichText::new("g").strong().size(18.0));
            ui.separator();
            let playing = self.is_playing();
            if ui.add_enabled(!playing, egui::Button::new("Play")).on_hover_text("Space").clicked() {
                self.play_pressed();
            }
            if ui.add_enabled(playing, egui::Button::new("Stop")).on_hover_text("Space").clicked() {
                self.send(EngineCommand::Stop);
            }
            if ui.button("Panic").on_hover_text("All notes off (Esc)").clicked() {
                self.send(EngineCommand::Panic { source: None });
            }
            ui.separator();
            let bpm = self.project.tempo.first().map(|t| t.us_per_quarter.bpm().0).unwrap_or(120.0);
            ui.label(format!("{bpm:.2} BPM"));
            let (bar, beat) = match &self.audio {
                AudioState::Running(s) => {
                    let tick = s.handle.status.position_tick.load(std::sync::atomic::Ordering::Relaxed);
                    let pos = self.project.meter_map().map(|m| m.position(rawdawg_types::units::Tick(tick))).ok();
                    pos.map(|p| (p.bar + 1, p.beat + 1)).unwrap_or((1, 1))
                }
                _ => (1, 1),
            };
            ui.label(egui::RichText::new(format!("{bar}:{beat}")).monospace());
            let mut metro = match &self.audio {
                AudioState::Running(s) => s.handle.status.metronome.load(std::sync::atomic::Ordering::Relaxed),
                _ => false,
            };
            if ui.toggle_value(&mut metro, "Metronome").changed() {
                self.send(EngineCommand::SetMetronome(metro));
            }
            ui.separator();
            ui.checkbox(&mut self.typing_enabled, "Typing keys").on_hover_text("Z S X D C V G B H N J M , L . ; /  play notes on the selected channel");
            if ui.button("−").on_hover_text("Octave down").clicked() {
                self.prefs.typing_octave = (self.prefs.typing_octave - 1).max(-4);
            }
            ui.label(format!("C{}", 4 + self.prefs.typing_octave));
            if ui.button("+").on_hover_text("Octave up").clicked() {
                self.prefs.typing_octave = (self.prefs.typing_octave + 1).min(4);
            }
            ui.add(egui::Slider::new(&mut self.prefs.typing_velocity, 0.05..=1.0).text("Vel").fixed_decimals(2));
            ui.separator();
            let undo_label = self.history.undo_label();
            if ui.add_enabled(self.history.can_undo(), egui::Button::new("Undo")).on_hover_text(undo_label.unwrap_or("Nothing to undo")).clicked() {
                self.undo();
            }
            let redo_label = self.history.redo_label();
            if ui.add_enabled(self.history.can_redo(), egui::Button::new("Redo")).on_hover_text(redo_label.unwrap_or("Nothing to redo")).clicked() {
                self.redo();
            }
            ui.separator();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Settings").clicked() {
                    self.show_settings = !self.show_settings;
                }
                match &self.audio {
                    AudioState::Running(sys) => {
                        let st = &sys.stream.status;
                        let load = st.last_load(sys.stream.config.sample_rate_hz) * 100.0;
                        ui.label(egui::RichText::new(format!("CPU {load:.0}%")).color(if load > 80.0 { theme::WARNING } else { theme::TEXT_SECONDARY }));
                        let xruns = st.xruns.get();
                        if xruns > 0 {
                            ui.label(egui::RichText::new(format!("xruns {xruns}")).color(theme::WARNING));
                        }
                        let frames = st.last_callback_frames.load(std::sync::atomic::Ordering::Relaxed);
                        ui.label(
                            egui::RichText::new(format!("{} · {} Hz · {} frames", sys.stream.config.device_name, sys.stream.config.sample_rate_hz, frames))
                                .color(theme::TEXT_SECONDARY),
                        )
                        .on_hover_text("Actual device callback size");
                    }
                    AudioState::Disabled => {
                        ui.label(egui::RichText::new("Audio off").color(theme::WARNING));
                    }
                    AudioState::Failed(_) => {
                        ui.label(egui::RichText::new("Audio unavailable").color(theme::ERROR));
                    }
                }
                let midi_label = if self.midi.is_available() { format!("MIDI {}", self.midi.enabled_ids().len()) } else { "MIDI n/a".to_owned() };
                ui.label(egui::RichText::new(midi_label).color(theme::TEXT_SECONDARY)).on_hover_text(format!("{} messages received", self.midi_activity));
                if self.safe_mode {
                    ui.label(egui::RichText::new("Safe mode").color(theme::WARNING));
                }
            });
        });
        ui.horizontal(|ui| {
            let mut song = self.transport_mode == TransportMode::Song;
            let mode_label = if song { "Song" } else { "Pattern" };
            if ui.toggle_value(&mut song, mode_label).on_hover_text("Pattern mode loops the selected pattern; Song mode plays the playlist").changed() {
                self.transport_mode = if song { TransportMode::Song } else { TransportMode::Pattern };
                self.send(EngineCommand::SetMode(self.transport_mode));
            }
            let mut lp = self.loop_enabled;
            if ui.toggle_value(&mut lp, "Loop").on_hover_text("Repeat the pattern (or the loop range / song)").changed() {
                self.loop_enabled = lp;
                self.send(EngineCommand::SetLoopEnabled(lp));
            }
            let mut cmds = Vec::new();
            sequencer::pattern_selector(self, ui, &mut cmds);
            for c in cmds {
                self.apply(c);
            }
            ui.separator();
            let mut armed = self.record.armed;
            let rec_label = if self.record.take.is_some() { egui::RichText::new("● REC").color(theme::ERROR) } else { egui::RichText::new("Rec") };
            if ui
                .toggle_value(&mut armed, rec_label)
                .on_hover_text("Arm recording: notes/controllers you play while the transport runs go into the selected pattern (song mode: a new take pattern + clip)")
                .changed()
            {
                self.set_record_armed(armed);
            }
            egui::ComboBox::from_id_salt("count-in").selected_text(format!("Count-in {}", self.prefs.count_in_bars)).width(96.0).show_ui(ui, |ui| {
                for b in 0..=2u32 {
                    ui.selectable_value(&mut self.prefs.count_in_bars, b, format!("{b} bar{}", if b == 1 { "" } else { "s" }));
                }
            });
            ui.add(egui::DragValue::new(&mut self.prefs.record_offset_ms).range(-100.0..=100.0).speed(0.5).suffix(" ms"))
                .on_hover_text("Recording offset: positive moves recorded notes earlier (compensates input/buffer latency)");
            if let AudioState::Running(sys) = &self.audio {
                let left = sys.handle.status.count_in_frames.load(std::sync::atomic::Ordering::Relaxed);
                if left > 0 {
                    ui.label(egui::RichText::new(format!("count-in {:.1} s", left as f64 / f64::from(sys.sample_rate.0))).color(theme::WARNING));
                }
            }
            ui.separator();
            ui.selectable_value(&mut self.editor_view, EditorView::Rack, "Rack").on_hover_text("F6");
            ui.selectable_value(&mut self.editor_view, EditorView::PianoRoll, "Piano roll").on_hover_text("F7");
            ui.selectable_value(&mut self.editor_view, EditorView::Synth, "Synth").on_hover_text("F8");
            ui.selectable_value(&mut self.editor_view, EditorView::Automation, "Automation").on_hover_text("Parameter curves (right-click a parameter → Create automation clip)");
            ui.selectable_value(&mut self.editor_view, EditorView::Mixer, "Mixer").on_hover_text("F9");
            if self.effect_editor.is_some() {
                ui.selectable_value(&mut self.editor_view, EditorView::Effect, "Effect");
            }
        });
    }

    /// Audition a project sample through the master at −12 dB (browser preview).
    pub(crate) fn preview_asset(&mut self, id: &rawdawg_types::AssetId) {
        use rawdawg_audio_engine::AssetResolver;
        let Some(asset) = self.assets.sample(id) else {
            self.status_line = "That sample cannot be read".into();
            return;
        };
        self.preview_held.push(asset.clone());
        if self.preview_held.len() > 8 {
            self.preview_held.remove(0);
        }
        self.send(EngineCommand::PreviewSample { asset, gain: rawdawg_dsp::math::db_to_lin(-12.0) });
    }

    fn browser(&mut self, ui: &mut egui::Ui) {
        ui.heading("Browser");
        ui.add(egui::TextEdit::singleline(&mut self.browser_search).hint_text("search samples and effects…").desired_width(f32::INFINITY));
        ui.separator();
        egui::CollapsingHeader::new("Instruments").default_open(true).show(ui, |ui| {
            ui.label("rawDAWg Synth").on_hover_text("native.synth — the flagship instrument");
        });
        egui::CollapsingHeader::new("Projects").default_open(true).show(ui, |ui| {
            if self.prefs.recent_projects.is_empty() {
                ui.label(egui::RichText::new("No recent projects").color(theme::TEXT_SECONDARY));
            }
            let recent = self.prefs.recent_projects.clone();
            for p in recent {
                let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                if ui.button(&name).on_hover_text(p.display().to_string()).clicked() {
                    self.open_project(p);
                }
            }
        });
        let needle = self.browser_search.trim().to_lowercase();
        egui::CollapsingHeader::new("Effects").show(ui, |ui| {
            ui.label(egui::RichText::new("Click to add to the selected mixer channel (or the master)").color(theme::TEXT_SECONDARY).small());
            let target = self.mixer.selected.unwrap_or(self.project.mixer.master_id);
            let mut rows: Vec<(bool, &rawdawg_effects::EffectInfo)> =
                rawdawg_effects::CATALOG.iter().filter(|f| f.listed).map(|f| (self.prefs.favorites.iter().any(|x| x == f.type_id), f)).collect();
            rows.sort_by_key(|(fav, _)| !*fav);
            for (fav, fx) in rows {
                if !browser_matches(&needle, &[fx.name, fx.family]) {
                    continue;
                }
                ui.horizontal(|ui| {
                    if ui.small_button(if fav { "fav" } else { "+fav" }).on_hover_text(if fav { "Remove from favorites" } else { "Add to favorites (listed first)" }).clicked() {
                        toggle_favorite(&mut self.prefs, fx.type_id);
                    }
                    if ui.button(fx.name).on_hover_text(fx.description).clicked() {
                        let mut inst = rawdawg_project::Instance::native(fx.type_id, serde_json::Value::Null);
                        inst.display_name = fx.name.to_owned();
                        let iid = inst.id;
                        self.apply(Command::AddInsert { mixer_id: target, instance: inst, index: None });
                        self.effect_editor = Some(iid);
                        self.editor_view = EditorView::Effect;
                    }
                });
            }
        });
        egui::CollapsingHeader::new("Samples & SoundFonts").default_open(true).show(ui, |ui| {
            // project audio assets: click to arm for the playlist, "+ channel" for a sampler
            let audio: Vec<(rawdawg_types::AssetId, String)> =
                self.project.assets.iter().filter(|a| a.kind == rawdawg_project::AssetKind::Audio).map(|a| (a.id.clone(), a.source_name.clone())).collect();
            if !audio.is_empty() {
                ui.label(
                    egui::RichText::new("Project samples — click one, then click a playlist lane to place it; drop a file on a lane to do both at once")
                        .color(theme::TEXT_SECONDARY)
                        .small(),
                );
            }
            let previewing = match &self.audio {
                AudioState::Running(s) => s.handle.status.preview_active.load(std::sync::atomic::Ordering::Relaxed),
                _ => false,
            };
            if !previewing {
                self.preview_held.clear();
            }
            let mut rows: Vec<(bool, rawdawg_types::AssetId, String)> =
                audio.into_iter().map(|(id, name)| (self.prefs.favorites.iter().any(|x| x == &id.to_string()), id, name)).collect();
            rows.sort_by_key(|(fav, _, _)| !*fav);
            for (fav, id, name) in rows {
                if !browser_matches(&needle, &[&name]) {
                    continue;
                }
                ui.horizontal(|ui| {
                    if ui.small_button(if fav { "fav" } else { "+fav" }).on_hover_text(if fav { "Remove from favorites" } else { "Add to favorites (listed first)" }).clicked() {
                        toggle_favorite(&mut self.prefs, &id.to_string());
                    }
                    let can_play = matches!(self.audio, AudioState::Running(_));
                    if previewing {
                        if ui.small_button("stop").on_hover_text("Stop the preview").clicked() {
                            self.send(EngineCommand::StopPreview);
                        }
                    } else if ui
                        .add_enabled(can_play, egui::Button::new("play").small())
                        .on_hover_text("Preview at a safe level (−12 dB) through the master; needs audio on")
                        .clicked()
                    {
                        self.preview_asset(&id);
                    }
                    let armed = self.playlist.place_asset.as_ref() == Some(&id);
                    if ui.selectable_label(armed, &name).on_hover_text("Arm for placement in the playlist").clicked() {
                        self.playlist.place_asset = if armed { None } else { Some(id.clone()) };
                        if !armed {
                            self.editor_view = EditorView::Rack;
                        }
                    }
                });
            }
            ui.label(egui::RichText::new("Drop a .wav/.flac/.mp3 or .sf2 onto the window, or type a path:").color(theme::TEXT_SECONDARY));
            ui.add(egui::TextEdit::singleline(&mut self.import_path).hint_text("/path/to/sound.wav").desired_width(f32::INFINITY));
            if ui.button("Import as new channel").clicked() && !self.import_path.trim().is_empty() {
                let p = PathBuf::from(self.import_path.trim());
                self.import_file_as_channel(&p);
            }
        });
    }

    pub(crate) fn open_project(&mut self, path: PathBuf) {
        match rawdawg_project::load_project_dir(&path) {
            Ok(p) => {
                self.send(EngineCommand::Panic { source: None });
                self.project = p;
                self.history = History::new();
                self.project_path = Some(path.clone());
                self.selected_channel = self.project.channels.first().map(|c| c.id);
                self.prefs.remember_project(path.clone());
                let _ = self.prefs.save();
                self.status_line = format!("Opened {}", path.display());
                self.rebuild_mirrors();
                self.recompile();
                self.recover_takes();
            }
            Err(e) => self.status_line = format!("Could not open {}: {e}", path.display()),
        }
    }

    pub(crate) fn save_project(&mut self) {
        let Some(path) = self.project_path.clone() else {
            self.status_line = "Save As is not available yet (M6); use --open to work on a project folder".into();
            return;
        };
        let revision = self.history.revision();
        match rawdawg_project::save_project_dir(&path, &self.project) {
            Ok(()) => {
                self.history.mark_saved_at(revision);
                self.status_line = format!("Saved {}", path.display());
            }
            Err(e) => self.status_line = format!("Save failed: {e}"),
        }
    }

    fn rack(&mut self, ui: &mut egui::Ui) {
        ui.heading("Channel rack");
        ui.separator();
        let mut cmds: Vec<Command> = Vec::new();
        let mut select: Option<ChannelId> = None;
        let channels = self.project.channels.clone();
        for ch in &channels {
            ui.horizontal(|ui| {
                let selected = self.selected_channel == Some(ch.id);
                if ui.selectable_label(selected, "IN").on_hover_text("Route typing/MIDI/on-screen keys here").clicked() {
                    select = Some(ch.id);
                }
                let mut mute = ch.mute;
                if ui.toggle_value(&mut mute, "M").on_hover_text("Mute").changed() {
                    cmds.push(Command::SetChannelMute { id: ch.id, mute });
                }
                let mut solo = ch.solo;
                if ui.toggle_value(&mut solo, "S").on_hover_text("Solo").changed() {
                    cmds.push(Command::SetChannelSolo { id: ch.id, solo });
                }
                if ui.button(egui::RichText::new(&ch.name).strong()).on_hover_text("Open instrument editor (right-click: rename/delete)").clicked() {
                    select = Some(ch.id);
                    self.editor_view = EditorView::Synth;
                }
                let r = ui.small_button("…").on_hover_text("Rename / delete");
                r.context_menu(|ui| {
                    ui.label("Rename");
                    let mut name = ch.name.clone();
                    if ui.text_edit_singleline(&mut name).lost_focus() && name != ch.name && !name.trim().is_empty() {
                        cmds.push(Command::RenameChannel { id: ch.id, name: name.trim().to_owned() });
                        ui.close();
                    }
                    if ui.button("Delete channel (undoable)").clicked() {
                        cmds.push(Command::RemoveChannel { id: ch.id });
                        ui.close();
                    }
                });
                if r.clicked() {
                    // the context menu opens on secondary click; a primary click opens it too
                }
                let mut gain = ch.gain_db.0;
                let r = ui.add(egui::Slider::new(&mut gain, -60.0..=12.0).suffix(" dB").fixed_decimals(1).show_value(true));
                if r.drag_started() {
                    let _ = self.history.begin_gesture();
                }
                if r.changed() && self.history.gesture_active() {
                    let _ = self.history.preview(&mut self.project, Command::SetChannelGain { id: ch.id, gain_db: rawdawg_types::units::Db(gain) });
                    self.recompile_gain_only();
                } else if r.changed() {
                    cmds.push(Command::SetChannelGain { id: ch.id, gain_db: rawdawg_types::units::Db(gain) });
                }
                if r.drag_stopped() && self.history.gesture_active() {
                    let _ = self.history.commit_gesture();
                    self.recompile();
                }
                let mut pan = ch.pan.0;
                let r = ui.add(egui::Slider::new(&mut pan, -1.0..=1.0).text("Pan").fixed_decimals(2));
                if r.drag_started() {
                    let _ = self.history.begin_gesture();
                }
                if r.changed() && self.history.gesture_active() {
                    let _ = self.history.preview(&mut self.project, Command::SetChannelPan { id: ch.id, pan: rawdawg_types::units::Pan(pan) });
                    self.recompile_gain_only();
                } else if r.changed() {
                    cmds.push(Command::SetChannelPan { id: ch.id, pan: rawdawg_types::units::Pan(pan) });
                }
                if r.drag_stopped() && self.history.gesture_active() {
                    let _ = self.history.commit_gesture();
                    self.recompile();
                }
                let kind = match ch.instrument.type_id.as_str() {
                    rawdawg_types::ids::native_types::SYNTH => "synth",
                    rawdawg_types::ids::native_types::SAMPLER => "sample",
                    rawdawg_types::ids::native_types::SF2 => "soundfont",
                    other => other,
                };
                ui.label(egui::RichText::new(kind).color(theme::TEXT_SECONDARY));
                let mixer_name = self.project.mixer_channel(ch.mixer_id).map(|m| m.name.clone()).unwrap_or_else(|| "?".into());
                egui::ComboBox::from_id_salt(("ch-mixer", ch.id)).selected_text(format!("→ {mixer_name}")).width(90.0).show_ui(ui, |ui| {
                    for m in &self.project.mixer.channels {
                        if ui.selectable_label(m.id == ch.mixer_id, &m.name).clicked() && m.id != ch.mixer_id {
                            cmds.push(Command::SetChannelMixer { id: ch.id, mixer_id: m.id });
                        }
                    }
                });
                ui.separator();
                sequencer::step_row(self, ui, ch.id, &mut cmds);
            });
        }
        if let Some(id) = select {
            self.selected_channel = Some(id);
        }
        for cmd in cmds {
            self.apply(cmd);
        }
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new("Keys play the channel marked IN. Steps toggle notes of the selected pattern; the piano roll (F7) edits the same notes.")
                .color(theme::TEXT_SECONDARY),
        );
    }

    /// Gain/pan previews during a drag: recompile is cheap enough at M1 scale and keeps
    /// one code path; processors are re-created, so held notes restart (fixed in M2 by
    /// carrying processor state across plans).
    fn recompile_gain_only(&mut self) {
        self.recompile();
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(&self.status_line);
            ui.separator();
            let mut retry = false;
            match &self.audio {
                AudioState::Running(sys) => {
                    let st = &sys.handle.status;
                    let l = st.master_peak_l.load();
                    let r = st.master_peak_r.load();
                    st.master_peak_l.store(0.0);
                    st.master_peak_r.store(0.0);
                    meter(ui, l);
                    meter(ui, r);
                    let red = st.master_reduction_db.load();
                    if red > 0.1 {
                        ui.label(egui::RichText::new(format!("limiter −{red:.1} dB")).color(theme::WARNING));
                    }
                    ui.label(format!("voices {}", st.active_voices.load(std::sync::atomic::Ordering::Relaxed)));
                    let overflow = st.command_overflows.get() + st.dropped_events.get();
                    if overflow > 0 {
                        ui.label(egui::RichText::new(format!("dropped {overflow}")).color(theme::ERROR)).on_hover_text("Control queue overflow or events for unknown channels");
                    }
                    if let Some(e) = &self.plan_error {
                        ui.label(egui::RichText::new(e).color(theme::ERROR));
                    }
                }
                AudioState::Failed(e) => {
                    ui.label(egui::RichText::new(format!("Audio unavailable: {e}")).color(theme::ERROR));
                    if ui.button("Retry audio").clicked() {
                        retry = true;
                    }
                }
                AudioState::Disabled => {
                    ui.label(egui::RichText::new("Audio off — editing and offline render available").color(theme::WARNING));
                    if ui.button("Start audio").clicked() {
                        retry = true;
                    }
                }
            }
            if retry {
                let req = self.settings.request();
                self.restart_audio(&req);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(format!("{PRODUCT_NAME} {VERSION} — {TAGLINE}")).color(theme::TEXT_SECONDARY));
                if let Some(p) = &self.project_path {
                    ui.label(egui::RichText::new(p.display().to_string()).color(theme::TEXT_SECONDARY));
                }
                if self.history.is_dirty() {
                    ui.label(egui::RichText::new("unsaved").color(theme::WARNING));
                }
            });
        });
    }

    fn menu(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.menu_button("File", |ui| {
                if ui.add_enabled(self.project_path.is_some(), egui::Button::new("Save (Ctrl+S)")).clicked() {
                    self.save_project();
                    ui.close();
                }
                if ui.button("Export…").on_hover_text("Render to WAV/FLAC (and stems) or a MIDI file with the playback engine").clicked() {
                    self.export.open = true;
                    ui.close();
                }
                ui.label(egui::RichText::new("New / Open / Save As dialogs arrive in M6; use --open <folder>").color(theme::TEXT_SECONDARY));
            });
            ui.menu_button("Edit", |ui| {
                let undo_label = self.history.undo_label().unwrap_or("Undo");
                if ui.add_enabled(self.history.can_undo(), egui::Button::new(format!("Undo {undo_label} (Ctrl+Z)"))).clicked() {
                    self.undo();
                    ui.close();
                }
                let redo_label = self.history.redo_label().unwrap_or("Redo");
                if ui.add_enabled(self.history.can_redo(), egui::Button::new(format!("Redo {redo_label} (Ctrl+Shift+Z)"))).clicked() {
                    self.redo();
                    ui.close();
                }
            });
            ui.menu_button("View", |ui| {
                if ui.button("Channel rack (F6)").clicked() {
                    self.editor_view = EditorView::Rack;
                    ui.close();
                }
                if ui.button("Synth (F7)").clicked() {
                    self.editor_view = EditorView::Synth;
                    ui.close();
                }
                if ui.button("Settings").clicked() {
                    self.show_settings = true;
                    ui.close();
                }
            });
            ui.menu_button("Help", |ui| {
                ui.label(format!("{PRODUCT_NAME} {VERSION}"));
                ui.label(TAGLINE);
                ui.separator();
                ui.label("Typing keys: Z S X D C V G B H N J M , L . ; /");
                ui.label("Space play/stop · Esc panic · Ctrl+Z undo · Ctrl+Shift+Z redo · Ctrl+S save");
                ui.label("F6 rack · F7 synth");
            });
        });
    }
}

impl App {
    /// `--screenshot`: ask the viewport for a capture once, save it as PNG, then close.
    fn handle_screenshot(&mut self, ctx: &egui::Context) {
        let Some((path, delay)) = self.screenshot.clone() else { return };
        let elapsed = self.started.elapsed();
        if !self.screenshot_requested && elapsed >= delay {
            self.screenshot_requested = true;
            let ctx2 = ctx.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |img| {
                let [w, h] = img.size;
                let saved = image::RgbaImage::from_raw(w as u32, h as u32, img.as_raw().to_vec()).ok_or_else(|| "pixel buffer size mismatch".to_owned()).and_then(|rgba| {
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    rgba.save(&path).map_err(|e| e.to_string())
                });
                match saved {
                    Ok(()) => log::info!("screenshot saved to {} ({w}x{h})", path.display()),
                    Err(e) => log::error!("screenshot failed: {e}"),
                }
                ctx2.send_viewport_cmd(egui::ViewportCommand::Close);
            })));
        } else if self.screenshot_requested && elapsed >= delay + std::time::Duration::from_secs(5) {
            // capture never arrived (backend without screenshot support): exit anyway
            log::error!("screenshot was not delivered by the rendering backend");
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

pub(crate) fn meter(ui: &mut egui::Ui, peak: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(80.0, 10.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, theme::BACKGROUND);
    let db = rawdawg_dsp::math::lin_to_db(peak).clamp(-60.0, 0.0);
    let frac = (db + 60.0) / 60.0;
    let fill = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * frac, rect.height()));
    let color = if db > -1.0 {
        theme::ERROR
    } else if db > -6.0 {
        theme::WARNING
    } else {
        theme::ACCENT
    };
    painter.rect_filled(fill, 2.0, color);
}

fn default_project() -> Project {
    let synth = rawdawg_synthesizer::Patch::default();
    Project::new_default("Untitled", serde_json::to_value(&synth).unwrap_or(serde_json::Value::Null))
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        theme::BACKGROUND.to_normalized_gamma_f32()
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.step(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        if self.persist_prefs {
            let _ = self.prefs.save();
        }
    }
}

impl App {
    /// Per-frame logic (shortcuts, input polling, engine housekeeping). Also used by headless tests.
    pub(crate) fn step(&mut self, ctx: &egui::Context) {
        theme::apply(ctx);
        if ctx.input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::S))) {
            self.save_project();
        }
        self.handle_shortcuts(ctx);
        self.handle_typing(ctx);
        self.poll_midi();
        self.poll_record();
        self.poll_audio_take();
        self.commit_quiet_midi_changes();
        automation::poll_capture(self);
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        let drop_target = self.playlist.drop_target.take();
        for p in dropped {
            let ext = p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
            let is_audio = matches!(ext.as_str(), "wav" | "flac" | "mp3");
            if matches!(ext.as_str(), "mid" | "midi" | "smf") {
                self.import_midi_file(&p);
            } else if let (Some((track, tick)), true) = (drop_target, is_audio) {
                // dropped on a playlist lane: an audio clip, not a new channel
                match self.import_asset_only(&p) {
                    Ok(id) => {
                        if let Some(asset_id) = rawdawg_types::AssetId::from_hex(&id)
                            && playlist::place_audio_clip(self, &asset_id, track, tick).is_some()
                        {
                            self.status_line = format!("Placed {} on the playlist", p.display());
                        } else {
                            self.status_line = format!("Imported {} but could not place it", p.display());
                        }
                    }
                    Err(e) => self.status_line = e,
                }
            } else {
                self.import_file_as_channel(&p);
            }
        }
        for sender in self.table_senders.values_mut() {
            sender.collect_retired();
        }
        waveform::flush_pending(self);
        if let AudioState::Running(sys) = &mut self.audio {
            sys.handle.collect_retired();
            if sys.stream.is_failed() {
                let msg = sys.stream.status.error_message().unwrap_or_else(|| "stream lost".into());
                let fixed_buffer = sys.stream.config.requested_buffer_frames;
                self.audio = AudioState::Failed(msg.clone());
                self.held.clear();
                if let Some(b) = fixed_buffer
                    && !self.buffer_fallback_tried
                {
                    // One automatic recovery: the device refused the fixed buffer at run
                    // time; reopen with its default buffer. Preferences stay as the user set them.
                    self.buffer_fallback_tried = true;
                    // let the backend tear the failed stream down before reopening
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    let req = StreamConfigRequest { buffer_frames: None, ..self.settings.request() };
                    self.restart_audio_with(&req, false);
                    if matches!(self.audio, AudioState::Running(_)) {
                        self.status_line = format!("Device refused a {b}-frame buffer ({msg}); running with the device default buffer. Change it in Settings.");
                    }
                } else {
                    self.status_line = "Audio stream lost; notes released. Use Retry audio.".into();
                }
            }
        }
        self.handle_screenshot(ctx);
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }

    /// The whole window layout. Also used by headless tests.
    pub(crate) fn draw(&mut self, ui: &mut egui::Ui) {
        self.draw_parts(ui, u8::MAX);
    }

    /// Window layout with selectable parts (bitmask; tests isolate interactions).
    pub(crate) fn draw_parts(&mut self, ui: &mut egui::Ui, parts: u8) {
        if parts & 1 != 0 {
            egui::Panel::top("menu").show(ui, |ui| self.menu(ui));
        }
        if parts & 2 != 0 {
            egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        }
        if parts & 4 != 0 {
            egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        }
        if parts & 8 != 0 {
            egui::Panel::left("browser").resizable(true).default_size(220.0).show(ui, |ui| self.browser(ui));
        }
        if parts & 16 != 0 {
            egui::Panel::bottom("editor")
                .resizable(true)
                .default_size(self.editor_height_hint.unwrap_or(if self.synth_panel.graph.focus_frames > 0 { 560.0 } else { 380.0 }))
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.editor_contents(ui));
                });
        }
        if parts & 32 != 0 {
            egui::CentralPanel::default().show(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("playlist-scroll").auto_shrink([false, false]).show(ui, |ui| playlist::show(self, ui));
            });
        }
        if self.show_settings {
            settings::show(self, ui.ctx());
        }
        export::poll(self);
        if self.export.open {
            export::show(self, ui.ctx());
        }
    }

    fn editor_contents(&mut self, ui: &mut egui::Ui) {
        match self.editor_view {
            EditorView::Rack => self.rack(ui),
            EditorView::PianoRoll => piano_roll::show(self, ui),
            EditorView::Automation => automation::show(self, ui),
            EditorView::Mixer => mixer::show(self, ui),
            EditorView::Effect => match self.effect_editor {
                Some(id) => effect_panel::show(self, ui, id),
                None => {
                    ui.label("Pick an effect in the mixer (F9).");
                }
            },
            EditorView::Synth => {
                let is_synth =
                    self.selected_channel.and_then(|id| self.project.channel(id)).is_some_and(|c| c.instrument.type_id.as_str() == rawdawg_types::ids::native_types::SYNTH);
                match (is_synth, self.selected_channel.and_then(|id| self.project.channel(id)).map(|c| c.instrument.id)) {
                    (true, _) => synth_panel::show(self, ui),
                    (false, Some(inst)) => instrument_panel::show(self, ui, inst),
                    _ => {
                        ui.label("Select a channel in the rack.");
                    }
                }
            }
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.send(EngineCommand::Panic { source: None });
        if self.persist_prefs {
            let _ = self.prefs.save();
        }
    }
}

/// Case-insensitive search over the given fields (empty needle matches everything).
pub(crate) fn browser_matches(needle: &str, fields: &[&str]) -> bool {
    needle.is_empty() || fields.iter().any(|f| f.to_lowercase().contains(needle))
}

pub(crate) fn toggle_favorite(prefs: &mut crate::prefs::Preferences, key: &str) {
    if let Some(i) = prefs.favorites.iter().position(|x| x == key) {
        prefs.favorites.remove(i);
    } else {
        prefs.favorites.push(key.to_owned());
    }
    let _ = prefs.save();
}

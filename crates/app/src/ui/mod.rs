//! GUI shell (M1): dark workspace skeleton, transport, typing/MIDI/on-screen
//! keyboards, synth parameter panel, settings, status.

mod automation;
mod channels;
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
mod project_files;
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
    /// Plugin catalog file override (tests, portable setups).
    pub catalog: Option<PathBuf>,
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
    pub show_help: bool,
    pub(crate) export: export::ExportState,
    pub(crate) files: project_files::ProjectFilesState,
    /// Browser search text (filters samples and effects).
    pub browser_search: String,
    /// Assets handed to the engine for preview: held here until the preview ended so the
    /// audio thread never drops the last reference.
    preview_held: Vec<std::sync::Arc<rawdawg_sampler::SampleAsset>>,
    /// Decoding worker of the live plan's streamed audio clips (replaced on recompile).
    stream_worker: Option<rawdawg_audio_engine::stream::StreamWorker>,
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
    /// Hosted plugins whose own editor changed parameters, with the time of the last change;
    /// their state is committed to the project once the editing pauses.
    pub plugin_editor_edits: HashMap<rawdawg_types::InstanceId, std::time::Instant>,
    /// Rack channel being dragged by its grip to a new place.
    pub rack_drag: Option<ChannelId>,
    /// The name being typed in a rack channel's menu (reset each time the menu opens).
    rack_rename: Option<(ChannelId, String)>,
    /// Third-party plugin catalog, loaded modules and scan state.
    pub plugin_host: crate::plugins::PluginHost,
    plugin_refresh_counter: u32,
    pub instrument_panel: instrument_panel::InstrumentPanelState,
    pub import_path: String,
    pub synth_panel: synth_panel::SynthPanelState,
    pub keyboard: keyboard::KeyboardState,
    pub settings: settings::SettingsState,
    midi_rescan_counter: u32,
    /// Per MIDI device: its clock mapped onto the process clock, and its latency (timestamped
    /// input, `rawdawg_audio_engine::timing`).
    midi_clocks: HashMap<u16, (rawdawg_midi::MidiClock, rawdawg_audio_engine::timing::JitterBuffer)>,
    /// The playhead while no engine runs (set by seeking).
    offline_playhead: i64,
    /// Deterministic tests only: every live event sent to the engine.
    #[cfg(test)]
    pub live_log: Vec<TimedEvent>,
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
        // a preset file opens the default project and is imported once the app is built
        let preset_to_import = opts.open.clone().filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("rawdawgpreset")));
        let project_to_open = opts.open.clone().filter(|_| preset_to_import.is_none()).map(|p| crate::prefs::absolute(&p));
        let (project, project_path, mut status_line) = match &project_to_open {
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
        let opened_dir = project_path.clone();
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
            show_help: false,
            export: export::ExportState::default(),
            files: project_files::ProjectFilesState::default(),
            browser_search: String::new(),
            preview_held: Vec::new(),
            stream_worker: None,
            editor_view: EditorView::Synth,
            synth_mirrors: HashMap::new(),
            live_fingerprints: HashMap::new(),
            table_senders: HashMap::new(),
            assets: rawdawg_audio_engine::ProjectAssetCache::new(None),
            other_mirrors: HashMap::new(),
            plugin_editor_edits: HashMap::new(),
            rack_drag: None,
            rack_rename: None,
            plugin_host: crate::plugins::PluginHost::load(opts.safe_mode, persist_prefs, opts.catalog.clone()),
            plugin_refresh_counter: 0,
            instrument_panel: Default::default(),
            import_path: String::new(),
            synth_panel: Default::default(),
            keyboard: Default::default(),
            settings: settings::SettingsState::from_request(&req),
            midi_rescan_counter: 0,
            midi_clocks: HashMap::new(),
            offline_playhead: 0,
            #[cfg(test)]
            live_log: Vec::new(),
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
        if let Some(path) = &preset_to_import {
            app.import_preset_file(path);
        }
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
            Some("open-dialog") => {
                app.files.path_input = app.project_path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                app.files.dialog = Some(project_files::FileDialog::Open);
            }
            Some("settings") => {
                app.show_settings = true;
                app.prefs.input_enabled = true;
            }
            Some("plugins") => {
                // Settings › Plugins with the browser filtered to the catalog (screenshot aid)
                app.show_settings = true;
                app.browser_search = "rawDAWg".into();
                app.editor_view = EditorView::Mixer;
                app.mixer.add_open = true;
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
        if let Some(dir) = opened_dir {
            // the constructor already loaded the project; add the lock and the recovery check
            match rawdawg_project::lock::ProjectLock::acquire(&dir, false) {
                Ok(l) => app.files.lock = Some(l),
                Err(e) => app.files.read_only = Some(e.to_string()),
            }
            app.files.recovery = rawdawg_project::recovery_candidates(&dir);
            app.files.last_autosave = Some(std::time::Instant::now());
        }
        app
    }

    /// Recreate main-thread synth mirrors from the project's instrument states.
    pub(crate) fn rebuild_mirrors(&mut self) {
        // assets must resolve even when no audio device is open (editing/offline mode)
        self.assets.sync(self.project_path.clone(), &self.project);
        self.synth_mirrors.clear();
        // hosted plugin instances stay alive while their project instance exists
        let project = &self.project;
        self.other_mirrors.retain(|id, p| rawdawg_plugin_host::is_hosted(p.as_ref()) && project.instance(*id).is_some());
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
    /// The project as the engine plays it: the document plus listening-only adjustments that
    /// are never saved (an Evolve preview's level match, or an autosave being compared).
    pub(crate) fn engine_project(&self) -> rawdawg_project::Project {
        // comparing a recovery candidate: the engine plays the autosave (listening only)
        if let Some(c) = &self.files.compare
            && c.listen_autosave
            && !self.files.recovery.is_empty()
        {
            return c.autosave.clone();
        }
        let mut project = self.project.clone();
        let evolve = &self.synth_panel.evolve;
        if let (Some(trim), Some((instance, _))) = (evolve.preview_trim_db, evolve.preview_backup.as_ref())
            && let Some(ch) = project.channels.iter_mut().find(|c| c.instrument.id == *instance)
        {
            ch.gain_db.0 += f64::from(trim);
        }
        project
    }

    pub(crate) fn recompile(&mut self) {
        let mut project = self.engine_project();
        // the assets of the project the engine plays (an autosave being compared may have more)
        self.assets.sync(self.project_path.clone(), &project);
        let AudioState::Running(sys) = &mut self.audio else { return };
        project.sample_rate_hz = sys.sample_rate;
        self.plan_revision += 1;
        // retired plans are dropped here (off RT): hosted plugins hand their audio halves back
        sys.handle.collect_retired();
        for m in self.other_mirrors.values_mut() {
            if let Some(live) = rawdawg_plugin_host::hosted_mut(m.as_mut()) {
                live.collect_returns();
            }
        }
        let mut externals = crate::plugins::AppExternals {
            host: &mut self.plugin_host,
            mirrors: &mut self.other_mirrors,
            live_fingerprints: &self.live_fingerprints,
            unavailable: Vec::new(),
            live_ids: Vec::new(),
        };
        let compiled = rawdawg_audio_engine::compile_with(
            &project,
            self.plan_revision,
            rawdawg_audio_io::stream::MAX_CHUNK_FRAMES,
            &self.limits,
            &self.live_fingerprints,
            &self.assets,
            &mut externals,
        );
        let unavailable = std::mem::take(&mut externals.unavailable);
        let live_ids = std::mem::take(&mut externals.live_ids);
        match compiled {
            Ok(out) => {
                sys.handle.collect_retired();
                match sys.handle.publish_plan(out.plan) {
                    Ok(_) => {
                        self.plan_error = None;
                        // the old worker stops when replaced; the retired plan's streams go with it
                        self.stream_worker = out.stream_worker;
                        if !out.missing_plugins.is_empty() {
                            let why = if unavailable.is_empty() { out.missing_plugins.join("; ") } else { unavailable.join("; ") };
                            self.status_line = format!("{} plugin(s) not available (state preserved): {why}", out.missing_plugins.len());
                        }
                        self.plugin_host.mark_live(live_ids);
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
    /// Persist preferences unless this is a headless test instance.
    pub(crate) fn save_prefs(&self) {
        if self.persist_prefs
            && let Err(e) = self.prefs.save()
        {
            log::warn!("could not save preferences: {e}");
        }
    }

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
                if !matches!(self.audio, AudioState::Running(_)) {
                    // no plan is compiled without a device: report stand-ins from the model instead
                    let missing = missing_plugin_notes(&self.project, &self.plugin_host);
                    if !missing.is_empty() {
                        self.status_line = format!("{} plugin(s) not available (state preserved): {}", missing.len(), missing.join("; "));
                    }
                }
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
            // the document changed under any preview: its level match no longer applies
            self.synth_panel.evolve.preview_trim_db = None;
            self.rebuild_mirrors();
            self.recompile();
        }
    }

    pub(crate) fn redo(&mut self) {
        if let Ok(label) = self.history.redo(&mut self.project) {
            self.status_line = format!("Redid: {label}");
            // the document changed under any preview: its level match no longer applies
            self.synth_panel.evolve.preview_trim_db = None;
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
        #[cfg(test)]
        self.live_log.push(event);
        if let Some(channel) = self.selected_channel {
            self.send(EngineCommand::Live { channel, event });
        }
    }

    /// A timestamped MIDI event: placed on the audio stream at the time it was sent plus a
    /// steady latency, so delivery jitter (USB, the MIDI thread, the UI frame) does not move it.
    fn live_timed(&mut self, event: TimedEvent, time: rawdawg_midi::MidiTime) {
        let EventSource::MidiDevice(dev) = event.source else { return self.live(event) };
        if time.is_untimed() {
            return self.live(event);
        }
        let AudioState::Running(sys) = &self.audio else { return self.live(event) };
        let st = &sys.handle.status;
        let (frame, block_wall, block) = (
            st.block_start_frame.load(std::sync::atomic::Ordering::Acquire),
            st.block_wall_us.load(std::sync::atomic::Ordering::Relaxed),
            st.block_frames.load(std::sync::atomic::Ordering::Relaxed),
        );
        let sr = sys.sample_rate.0;
        let (clock, jitter) = self.midi_clocks.entry(dev).or_default();
        let wall = clock.wall_time(time);
        let target = rawdawg_audio_engine::timing::place_event(jitter, frame, block_wall, block, sr, wall, rawdawg_rt::clock::now_us());
        #[cfg(test)]
        self.live_log.push(event);
        if let Some(channel) = self.selected_channel {
            self.send(EngineCommand::LiveAt { channel, event, stream_frame: target });
        }
    }

    /// Add a fresh synth channel below the selected one and select it (one undo step).
    pub(crate) fn add_synth_channel(&mut self) {
        let (cmd, id) = channels::new_synth_channel(&self.project, self.selected_channel);
        self.apply(cmd);
        if self.project.channel(id).is_some() {
            self.selected_channel = Some(id);
            let name = self.project.channel(id).map(|c| c.name.clone()).unwrap_or_default();
            self.status_line = format!("Added {name}: play it on the keys, or open it in the Synth view");
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
        let (redo, undo, space, f6, f7, f8, f9, esc) = ctx.input_mut(|i| {
            (
                // redo first: egui matches Ctrl+Z "logically", so it also takes Ctrl+Shift+Z
                i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::Z))
                    || i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::Y)),
                i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::Z)),
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
        for (ev, time) in events {
            if self.handle_midi_control(&ev) {
                continue;
            }
            self.live_timed(ev, time);
        }
    }

    pub(crate) fn is_playing(&self) -> bool {
        match &self.audio {
            AudioState::Running(s) => s.handle.status.playing.load(std::sync::atomic::Ordering::Relaxed),
            _ => false,
        }
    }

    /// The playhead in project ticks: the engine's, or (no engine running) the position set by
    /// the last seek, so seeking and "at the playhead" edits work with audio off too.
    pub(crate) fn playhead_tick(&self) -> Option<i64> {
        match &self.audio {
            AudioState::Running(s) => Some(s.handle.status.position_tick.load(std::sync::atomic::Ordering::Relaxed)),
            _ => Some(self.offline_playhead),
        }
    }

    /// Move the playhead (the engine's when it runs; ours either way).
    pub(crate) fn seek(&mut self, tick: i64) {
        self.offline_playhead = tick.max(0);
        self.send(EngineCommand::SeekTick(tick.max(0)));
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
                    self.save_prefs();
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
        // the status on the right takes its place first; the controls fill what is left and wrap
        // onto a second line in a narrow window instead of running under it (UI_UX: 1024×720)
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.toolbar_status(ui);
                ui.separator();
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true), |ui| self.toolbar_controls(ui));
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
            if self.input_open() {
                ui.label(egui::RichText::new("In").small().color(theme::TEXT_SECONDARY));
                recording::input_meter(self, ui, 64.0);
            }
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

    /// The toolbar's controls, most important first (transport, tempo, undo), so a narrow
    /// window wraps the typing-keys group, not Undo/Redo.
    fn toolbar_controls(&mut self, ui: &mut egui::Ui) {
        rawdawg_ui::params::wordmark(ui, 18.0);
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
        self.tempo_control(ui);
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
        let undo_label = self.history.undo_label();
        if ui.add_enabled(self.history.can_undo(), egui::Button::new("Undo")).on_hover_text(undo_label.unwrap_or("Nothing to undo")).clicked() {
            self.undo();
        }
        let redo_label = self.history.redo_label();
        if ui.add_enabled(self.history.can_redo(), egui::Button::new("Redo")).on_hover_text(redo_label.unwrap_or("Nothing to redo")).clicked() {
            self.redo();
        }
        ui.separator();
        // the typing-keys group wraps as one piece (its width is known from the last frame)
        let group = ui.scope_id().with("typing-keys-group");
        if ui.available_size_before_wrap().x < ui.data(|d| d.get_temp::<f32>(group).unwrap_or(0.0)) {
            ui.end_row();
        }
        let r = ui.horizontal(|ui| {
            ui.checkbox(&mut self.typing_enabled, "Typing keys").on_hover_text("Z S X D C V G B H N J M , L . ; /  play notes on the selected channel");
            if ui.button("−").on_hover_text("Octave down").clicked() {
                self.prefs.typing_octave = (self.prefs.typing_octave - 1).max(-4);
            }
            ui.label(format!("C{}", 4 + self.prefs.typing_octave));
            if ui.button("+").on_hover_text("Octave up").clicked() {
                self.prefs.typing_octave = (self.prefs.typing_octave + 1).min(4);
            }
            ui.add(rawdawg_ui::params::slider(ui, &mut self.prefs.typing_velocity, 0.05..=1.0).text("Vel").fixed_decimals(2));
        });
        ui.data_mut(|d| d.insert_temp(group, r.response.rect.width()));
    }

    /// The toolbar's right end, drawn right to left: Settings, the audio device, MIDI.
    fn toolbar_status(&mut self, ui: &mut egui::Ui) {
        let full = ui.available_width();
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
                // a long device name is cut (it is on hover), so it never runs under the controls
                let text = format!("{} · {} Hz · {} frames", sys.stream.config.device_name, sys.stream.config.sample_rate_hz, frames);
                bounded_label(ui, egui::RichText::new(&text).color(theme::TEXT_SECONDARY), full * 0.22).on_hover_text(format!("{text}\nActual device callback size"));
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
    }

    fn browser(&mut self, ui: &mut egui::Ui) {
        ui.heading("Browser");
        ui.add(egui::TextEdit::singleline(&mut self.browser_search).hint_text("search samples and effects…").desired_width(f32::INFINITY));
        ui.separator();
        egui::CollapsingHeader::new("Instruments").default_open(true).show(ui, |ui| {
            if ui.add(egui::Button::new("rawDAWg Synth").frame(false)).on_hover_text("Click: add a new rawDAWg Synth channel to the rack (the flagship instrument)").clicked() {
                self.add_synth_channel();
            }
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
                        self.toggle_favorite(fx.type_id);
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
        egui::CollapsingHeader::new("Plugins").show(ui, |ui| {
            let count = self.plugin_host.catalog.plugins().count();
            if count == 0 {
                ui.label(egui::RichText::new("No third-party plugins in the catalog yet — scan in Settings › Plugins").color(theme::TEXT_SECONDARY).small());
            } else {
                ui.label(
                    egui::RichText::new("Effects go to the selected mixer channel; instruments replace the selected channel's instrument").color(theme::TEXT_SECONDARY).small(),
                );
            }
            if self.plugin_host.safe_mode {
                ui.label(egui::RichText::new("Safe mode: plugins are listed but not loaded").color(theme::WARNING).small());
            }
            let target = self.mixer.selected.unwrap_or(self.project.mixer.master_id);
            let rows: Vec<(rawdawg_plugin_host::PluginDescription, PathBuf)> = self.plugin_host.catalog.plugins().map(|(l, p)| (p.clone(), l.path.clone())).collect();
            for (desc, path) in rows {
                if !browser_matches(&needle, &[&desc.name, &desc.vendor, &desc.plugin_id]) {
                    continue;
                }
                let kind = if desc.is_instrument && !desc.is_effect { "instrument" } else { "effect" };
                let label = format!("{} ({} {kind})", desc.name, desc.format.label());
                let hover = format!("{} — {} {}\n{}\n{}", desc.plugin_id, desc.vendor, desc.version, desc.description, path.display());
                if ui.button(label).on_hover_text(hover).clicked() {
                    self.add_catalog_plugin(&desc, &path, target);
                }
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
                        self.toggle_favorite(&id.to_string());
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
                    let missing = self.project.assets.iter().find(|a| a.id == id).is_some_and(|a| self.asset_is_missing(a));
                    if missing && ui.small_button("relink…").on_hover_text("The file is missing from the project folder").clicked() {
                        self.files.relink = Some((id.clone(), String::new()));
                    }
                    let armed = self.playlist.place_asset.as_ref() == Some(&id);
                    if ui.selectable_label(armed, if missing { format!("{name} (missing)") } else { name.clone() }).on_hover_text("Arm for placement in the playlist").clicked() {
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
        let path = crate::prefs::absolute(&path);
        match rawdawg_project::load_project_dir(&path) {
            Ok(p) => {
                self.send(EngineCommand::Panic { source: None });
                self.project = p;
                self.history = History::new();
                self.project_path = Some(path.clone());
                self.selected_channel = self.project.channels.first().map(|c| c.id);
                self.prefs.remember_project(path.clone());
                self.save_prefs();
                self.status_line = format!("Opened {}", path.display());
                self.rebuild_mirrors();
                self.recompile();
                self.recover_takes();
            }
            Err(e) => self.status_line = format!("Could not open {}: {e}", path.display()),
        }
    }

    fn rack(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Channel rack");
            if ui.button("+ Synth").on_hover_text("Add a new rawDAWg Synth channel (the default sound) below the selected one").clicked() {
                self.add_synth_channel();
            }
        });
        if self.project.channels.is_empty() {
            ui.label(egui::RichText::new("No channels yet: + Synth adds an instrument, or drop a sample or a MIDI file onto the window.").color(theme::TEXT_SECONDARY));
        }
        ui.separator();
        let mut cmds: Vec<Command> = Vec::new();
        let mut select: Option<ChannelId> = None;
        let channels = self.project.channels.clone();
        let mut rows: Vec<egui::Rect> = Vec::with_capacity(channels.len());
        let mut dropped = false;
        let mut duplicate: Option<(ChannelId, bool)> = None;
        for (ci, ch) in channels.iter().enumerate() {
            let row = ui.horizontal(|ui| {
                // drag grip: move the channel up or down the rack (one undo step)
                let grip = mixer::drag_grip(ui, rack_grip_id(ch.id), self.rack_drag == Some(ch.id), "Drag to move this channel up or down the rack");
                if grip.drag_started() {
                    self.rack_drag = Some(ch.id);
                }
                if grip.drag_stopped() {
                    dropped = true;
                }
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
                if ui.button(egui::RichText::new(&ch.name).strong()).on_hover_text("Open the instrument editor").clicked() {
                    select = Some(ch.id);
                    self.editor_view = EditorView::Synth;
                }
                let r = ui.small_button("…").on_hover_text("Rename, duplicate, move or delete this channel");
                // left or right click opens it; it stays open while typing a new name (every entry closes it)
                let toggle = (r.clicked() || r.secondary_clicked()).then_some(egui::SetOpenCommand::Toggle);
                if toggle.is_some() {
                    self.rack_rename = None;
                }
                egui::Popup::menu(&r).open_memory(toggle).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
                    ui.label("Rename (Enter applies)");
                    if self.rack_rename.as_ref().is_none_or(|(id, _)| *id != ch.id) {
                        self.rack_rename = Some((ch.id, ch.name.clone()));
                    }
                    let Some((_, name)) = self.rack_rename.as_mut() else { return };
                    let field = ui.add(egui::TextEdit::singleline(name).id(rack_rename_id(ch.id)));
                    if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        let name = name.trim().to_owned();
                        if !name.is_empty() && name != ch.name {
                            cmds.push(Command::RenameChannel { id: ch.id, name });
                        }
                        self.rack_rename = None;
                        ui.close();
                    }
                    if ui.button("Duplicate").on_hover_text("A copy of this channel's sound and settings, below it").clicked() {
                        duplicate = Some((ch.id, false));
                        ui.close();
                    }
                    if ui.button("Duplicate with notes").on_hover_text("The copy also gets this channel's notes in every pattern").clicked() {
                        duplicate = Some((ch.id, true));
                        ui.close();
                    }
                    if ci > 0 && ui.button("Move up").clicked() {
                        cmds.push(Command::MoveChannel { id: ch.id, to_index: ci - 1 });
                        ui.close();
                    }
                    if ci + 1 < channels.len() && ui.button("Move down").clicked() {
                        cmds.push(Command::MoveChannel { id: ch.id, to_index: ci + 1 });
                        ui.close();
                    }
                    if ui.button("Delete channel (undoable)").on_hover_text("Its mixer strip goes too when nothing else plays into it and it holds no effects").clicked() {
                        let (cmd, strip) = rawdawg_project::routing::remove_channel(&self.project, ch.id);
                        self.status_line = match strip.and_then(|s| self.project.mixer_channel(s)) {
                            Some(s) => format!("Deleted {} and its mixer strip {} (Undo brings both back)", ch.name, s.name),
                            None => format!("Deleted {}", ch.name),
                        };
                        cmds.push(cmd);
                        ui.close();
                    }
                });
                let mut gain = ch.gain_db.0;
                let r = ui.add(rawdawg_ui::params::slider(ui, &mut gain, -60.0..=12.0).suffix(" dB").fixed_decimals(1).show_value(true));
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
                let r = ui.add(rawdawg_ui::params::slider(ui, &mut pan, -1.0..=1.0).text("Pan").fixed_decimals(2));
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
                    rawdawg_types::ids::native_types::MORPH => "morph",
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
            rows.push(row.response.rect);
        }
        // while dragging: a line shows where the channel will land; release moves it
        if let Some(dragged) = self.rack_drag
            && let Some(from) = channels.iter().position(|c| c.id == dragged)
        {
            let width = rows.first().map_or(200.0, |r| r.width());
            let target = mixer::drop_target(ui, &rows, from, width);
            if dropped {
                if let Some(to_index) = target {
                    cmds.push(Command::MoveChannel { id: dragged, to_index });
                }
                self.rack_drag = None;
            }
        } else if dropped {
            self.rack_drag = None;
        }
        if let Some((id, with_notes)) = duplicate {
            // a plugin window's latest edits belong to the sound being copied
            if let Some(inst) = channels.iter().find(|c| c.id == id).map(|c| c.instrument.id)
                && self.plugin_editor_edits.remove(&inst).is_some()
            {
                self.commit_plugin_state(inst, "plugin window edit");
            }
            if let Some((cmd, new_id)) = channels::duplicate_channel(&self.project, id, with_notes) {
                cmds.push(cmd);
                select = Some(new_id);
            }
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

    /// The song's starting tempo, editable in place: drag (one undo step per drag) or click and
    /// type. Later tempo changes are the markers above the playlist.
    fn tempo_control(&mut self, ui: &mut egui::Ui) {
        let mut bpm = self.project.tempo.first().map(|t| t.us_per_quarter.bpm().0).unwrap_or(120.0);
        let r = ui
            .add(egui::DragValue::new(&mut bpm).range(20.0..=400.0).speed(0.1).fixed_decimals(2).suffix(" BPM").update_while_editing(false))
            .on_hover_text("Song tempo: drag, or click and type (tempo changes later in the song: right-click the marker row above the playlist)");
        let set = |p: &rawdawg_project::Project, bpm: f64| -> Option<Command> {
            let v = rawdawg_types::units::UsPerQuarter::from_bpm(bpm).ok()?;
            let mut tempo = p.tempo.clone();
            match tempo.first_mut() {
                Some(t) if t.tick.0 == 0 => t.us_per_quarter = v,
                _ => tempo.insert(0, rawdawg_project::TempoPoint { tick: rawdawg_types::units::Tick(0), us_per_quarter: v }),
            }
            Some(Command::SetTempoMap { tempo })
        };
        if r.drag_started() {
            let _ = self.history.begin_gesture();
        }
        if r.changed()
            && let Some(cmd) = set(&self.project, bpm)
        {
            if self.history.gesture_active() {
                let _ = self.history.preview(&mut self.project, cmd);
                self.recompile();
            } else {
                self.apply(cmd);
            }
        }
        if r.drag_stopped() && self.history.gesture_active() {
            let _ = self.history.commit_gesture();
            self.recompile();
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        // right group first: the unsaved marker always shows, the project name and the version
        // get a bounded width and are cut (full text on hover); the left group fills the rest
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let full = ui.available_width();
                bounded_label(ui, egui::RichText::new(format!("{PRODUCT_NAME} {VERSION} — {TAGLINE}")).color(theme::TEXT_SECONDARY), full * 0.2);
                if let Some(p) = &self.project_path {
                    // the folder name says which project; the whole path is on hover
                    let name = p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned());
                    bounded_label(ui, egui::RichText::new(name).color(theme::TEXT_SECONDARY), full * 0.15).on_hover_text(p.display().to_string());
                }
                if self.history.is_dirty() {
                    ui.label(egui::RichText::new("unsaved").color(theme::WARNING));
                }
                ui.separator();
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| self.status_left(ui));
            });
        });
    }

    /// The status bar's message and the audio state, in the space the right group leaves.
    fn status_left(&mut self, ui: &mut egui::Ui) {
        // the message gets at most part of the row and is cut with an ellipsis (full text on
        // hover), so a long path never runs under the audio status and the version label
        let max = (ui.available_width() * 0.45).max(160.0);
        ui.allocate_ui_with_layout(egui::vec2(max, ui.available_height()), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.add(egui::Label::new(&self.status_line).truncate());
        });
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
                let late = st.late_events.get();
                if late > 0 {
                    ui.label(egui::RichText::new(format!("late MIDI {late}")).color(theme::WARNING))
                        .on_hover_text("MIDI notes that arrived after the moment they should have played (placed at once); a larger audio buffer gives them more room");
                }
                if let Some(e) = &self.plan_error {
                    bounded_label(ui, egui::RichText::new(e).color(theme::ERROR), ui.available_width());
                }
            }
            AudioState::Failed(e) => {
                // the error can be longer than the window: cut it (full text on hover) so
                // the Retry button always stays in reach
                let room = ui.available_width() - button_width(ui, "Retry audio");
                bounded_label(ui, egui::RichText::new(format!("Audio unavailable: {e}")).color(theme::ERROR), room);
                if ui.button("Retry audio").clicked() {
                    retry = true;
                }
            }
            AudioState::Disabled => {
                let room = ui.available_width() - button_width(ui, "Start audio");
                bounded_label(ui, egui::RichText::new("Audio off — editing and offline render available").color(theme::WARNING), room);
                if ui.button("Start audio").clicked() {
                    retry = true;
                }
            }
        }
        if retry {
            let req = self.settings.request();
            self.restart_audio(&req);
        }
    }

    fn menu(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.menu_button("File", |ui| {
                if ui.button("New…").clicked() {
                    self.files.dialog = Some(project_files::FileDialog::New);
                    ui.close();
                }
                if ui.button("Open…").clicked() {
                    self.files.path_input = self.project_path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                    self.files.dialog = Some(project_files::FileDialog::Open);
                    ui.close();
                }
                if ui.add_enabled(self.files.read_only.is_none(), egui::Button::new("Save (Ctrl+S)")).clicked() {
                    self.save_project_checked();
                    ui.close();
                }
                if ui.button("Save As…").clicked() {
                    self.files.path_input = self.project_path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                    self.files.dialog = Some(project_files::FileDialog::SaveAs);
                    ui.close();
                }
                if let Some(n) = &self.files.last_autosave_note {
                    ui.label(egui::RichText::new(n).color(theme::TEXT_SECONDARY).small());
                }
                if ui.button("Export…").on_hover_text("Render to WAV/FLAC (and stems) or a MIDI file with the playback engine").clicked() {
                    self.export.open = true;
                    ui.close();
                }
                ui.label(egui::RichText::new("Projects are folders ending in .rawdawg; autosaves every 2 min while unsaved").color(theme::TEXT_SECONDARY).small());
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
                if ui.button("User guide (offline)").clicked() {
                    self.show_help = true;
                    ui.close();
                }
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

/// The offline user guide (docs/USER_GUIDE.md), shown from Help.
const USER_GUIDE: &str = include_str!("../../../../docs/USER_GUIDE.md");

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

/// A label cut to at most `max_w` points (egui shows the whole text on hover when it is cut),
/// so it can never push what follows it out of the row.
fn bounded_label(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>, max_w: f32) -> egui::Response {
    let max_w = max_w.max(40.0);
    ui.allocate_ui(egui::vec2(max_w, ui.available_height()), |ui| ui.add(egui::Label::new(text).truncate())).inner
}

/// Width a button with `text` takes in `ui`, spacing included.
fn button_width(ui: &egui::Ui, text: &str) -> f32 {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let w = ui.fonts_mut(|f| f.layout_no_wrap(text.to_owned(), font, egui::Color32::WHITE).size().x);
    w + 2.0 * ui.spacing().button_padding.x + ui.spacing().item_spacing.x
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

    fn on_exit(&mut self) {
        // clean shutdown: the next start must not offer safe mode because of us
        self.plugin_host.clear_marker();
    }
}

impl App {
    /// Per-frame logic (shortcuts, input polling, engine housekeeping). Also used by headless tests.
    pub(crate) fn step(&mut self, ctx: &egui::Context) {
        theme::apply(ctx);
        if ctx.input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::S))) {
            self.save_project_checked();
        }
        self.poll_project_files(ctx);
        self.poll_plugins();
        self.poll_plugin_editors(ctx);
        self.handle_shortcuts(ctx);
        self.handle_typing(ctx);
        self.poll_midi();
        self.poll_record();
        self.poll_audio_take();
        self.sample_input_meter();
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
        project_files::show(self, ui.ctx());
        recording::latency_window(self, ui.ctx());
        if self.show_help {
            let mut open = self.show_help;
            egui::Window::new("User guide").open(&mut open).default_size([640.0, 520.0]).resizable(true).show(ui.ctx(), |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for line in USER_GUIDE.lines() {
                        if let Some(h) = line.strip_prefix("# ") {
                            ui.heading(h);
                        } else if let Some(h) = line.strip_prefix("## ") {
                            ui.add_space(8.0);
                            ui.label(egui::RichText::new(h).strong().size(16.0));
                        } else if let Some(h) = line.strip_prefix("### ") {
                            ui.label(egui::RichText::new(h).strong());
                        } else if line.trim().is_empty() {
                            ui.add_space(4.0);
                        } else {
                            ui.label(line.replace("**", "").replace('`', ""));
                        }
                    }
                });
            });
            self.show_help = open;
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

impl App {
    pub(crate) fn toggle_favorite(&mut self, key: &str) {
        if let Some(i) = self.prefs.favorites.iter().position(|x| x == key) {
            self.prefs.favorites.remove(i);
        } else {
            self.prefs.favorites.push(key.to_owned());
        }
        self.save_prefs();
    }
}

/// Third-party instances the project references (no host yet): shown as preserved stand-ins.
pub(crate) fn missing_plugin_notes(project: &Project, host: &crate::plugins::PluginHost) -> Vec<String> {
    let label = |i: &rawdawg_project::Instance, owner: &str, reason: &str| {
        format!(
            "{owner}: {} ({:?}) is not available — {reason}; its state is preserved",
            if i.display_name.is_empty() { i.type_id.as_str() } else { i.display_name.as_str() },
            i.format
        )
    };
    let mut out = Vec::new();
    for c in &project.channels {
        if let Some(reason) = host.unavailable_reason(&c.instrument) {
            out.push(label(&c.instrument, &c.name, &reason));
        }
    }
    for m in &project.mixer.channels {
        for i in &m.inserts {
            if let Some(reason) = host.unavailable_reason(i) {
                out.push(label(i, &m.name, &reason));
            }
        }
    }
    out
}

impl App {
    /// Scan progress, hosted-mirror value refresh and crash-marker housekeeping (UI rate).
    pub(crate) fn poll_plugins(&mut self) {
        if self.plugin_host.poll() {
            let r = self.plugin_host.last_report.clone().unwrap_or_default();
            self.status_line =
                format!("Plugin scan: {} found, {} scanned, {} quarantined, {} unchanged", self.plugin_host.catalog.plugins().count(), r.scanned, r.quarantined, r.unchanged);
            // instances that were stand-ins may be hostable now
            self.recompile();
        }
        self.plugin_refresh_counter = self.plugin_refresh_counter.wrapping_add(1);
        if self.plugin_refresh_counter.is_multiple_of(6) {
            for m in self.other_mirrors.values_mut() {
                if let Some(live) = rawdawg_plugin_host::hosted_mut(m.as_mut()) {
                    live.collect_returns();
                    live.refresh_values();
                }
            }
        }
    }

    /// Add a catalog plugin to the project: effects become inserts on `mixer_target`,
    /// instruments replace the selected channel's instrument.
    pub(crate) fn add_catalog_plugin(&mut self, desc: &rawdawg_plugin_host::PluginDescription, module_path: &std::path::Path, mixer_target: rawdawg_types::MixerChannelId) {
        let format = match desc.format {
            rawdawg_plugin_host::Format::Clap => rawdawg_project::PluginFormat::Clap,
            rawdawg_plugin_host::Format::Vst3 => rawdawg_project::PluginFormat::Vst3,
        };
        let inst = rawdawg_project::Instance::external(format, &desc.plugin_id, &desc.name, &desc.vendor, &desc.version, Some(module_path.display().to_string()));
        let iid = inst.id;
        if desc.is_instrument && !desc.is_effect {
            let Some(channel_id) = self.selected_channel else {
                self.status_line = "Select a channel to give it this instrument".to_owned();
                return;
            };
            self.apply(Command::SetInstrument { channel_id, instance: inst });
            self.editor_view = EditorView::Synth;
        } else {
            self.apply(Command::AddInsert { mixer_id: mixer_target, instance: inst, index: None });
            self.effect_editor = Some(iid);
            self.editor_view = EditorView::Effect;
        }
    }
}

/// Id of a rack row's drag grip (stable, for input tests).
pub(crate) fn rack_rename_id(id: ChannelId) -> egui::Id {
    egui::Id::unique(("rack-rename", id))
}

pub(crate) fn rack_grip_id(id: ChannelId) -> egui::Id {
    egui::Id::unique(("rack-grip", id))
}

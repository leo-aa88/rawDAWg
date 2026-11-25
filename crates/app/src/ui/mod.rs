//! GUI shell (M1): dark workspace skeleton, transport, typing/MIDI/on-screen
//! keyboards, synth parameter panel, settings, status.

mod keyboard;
mod settings;
mod synth_panel;

use crate::audio::AudioSystem;
use crate::prefs::Preferences;
use crate::theme;
use rawdawg_audio_engine::commands::EngineCommand;
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum EditorView {
    Rack,
    Synth,
}

pub(crate) struct App {
    pub project: Project,
    pub history: History,
    pub project_path: Option<PathBuf>,
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
    pub editor_view: EditorView,
    /// Main-thread mirrors of native synth state per instance (param edits, commits).
    pub synth_mirrors: HashMap<rawdawg_types::InstanceId, Synth>,
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
        theme::apply(&cc.egui_ctx);
        let limits = EngineLimits::DEFAULT;
        let mut prefs = Preferences::load();
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
            match AudioSystem::start(&req, &limits) {
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
            editor_view: EditorView::Synth,
            synth_mirrors: HashMap::new(),
            synth_panel: Default::default(),
            keyboard: Default::default(),
            settings: settings::SettingsState::from_request(&req),
            midi_rescan_counter: 0,
            buffer_fallback_tried: false,
            screenshot: opts.screenshot.clone(),
            started: std::time::Instant::now(),
            screenshot_requested: false,
            prefs,
        };
        app.rebuild_mirrors();
        app.recompile();
        app
    }

    /// Recreate main-thread synth mirrors from the project's instrument states.
    pub(crate) fn rebuild_mirrors(&mut self) {
        self.synth_mirrors.clear();
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
        match compile(&project, self.plan_revision, rawdawg_audio_io::stream::MAX_CHUNK_FRAMES, &self.limits) {
            Ok(plan) => {
                sys.handle.collect_retired();
                match sys.handle.publish_plan(plan) {
                    Ok(_) => self.plan_error = None,
                    Err(_) => self.plan_error = Some("engine busy; change not applied yet".to_owned()),
                }
            }
            Err(e) => self.plan_error = Some(e.to_string()),
        }
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
        let ids: Vec<_> = self.project.channels.iter().map(|c| c.instrument.id).collect();
        if ids.len() != self.synth_mirrors.len() || ids.iter().any(|i| !self.synth_mirrors.contains_key(i)) {
            self.rebuild_mirrors();
        }
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
        let (undo, redo, space, f6, f7, f9, esc) = ctx.input_mut(|i| {
            (
                i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::Z)),
                i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::Z))
                    || i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::Y)),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Space),
                i.consume_key(egui::Modifiers::NONE, egui::Key::F6),
                i.consume_key(egui::Modifiers::NONE, egui::Key::F7),
                i.consume_key(egui::Modifiers::NONE, egui::Key::F9),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        });
        if redo {
            self.redo();
        } else if undo {
            self.undo();
        }
        if space {
            let playing = self.is_playing();
            self.send(if playing { EngineCommand::Stop } else { EngineCommand::Play });
        }
        if f6 {
            self.editor_view = EditorView::Rack;
        }
        if f7 || f9 {
            self.editor_view = EditorView::Synth;
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
            self.live(ev);
        }
    }

    pub(crate) fn is_playing(&self) -> bool {
        match &self.audio {
            AudioState::Running(s) => s.handle.status.playing.load(std::sync::atomic::Ordering::Relaxed),
            _ => false,
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
        match AudioSystem::start(req, &self.limits) {
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
                self.send(EngineCommand::Play);
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
            ui.selectable_value(&mut self.editor_view, EditorView::Rack, "Rack").on_hover_text("F6");
            ui.selectable_value(&mut self.editor_view, EditorView::Synth, "Synth").on_hover_text("F7");
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
    }

    fn browser(&mut self, ui: &mut egui::Ui) {
        ui.heading("Browser");
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
        egui::CollapsingHeader::new("Effects").show(ui, |ui| {
            ui.label(egui::RichText::new("Native effects arrive in M4").color(theme::TEXT_SECONDARY));
        });
        egui::CollapsingHeader::new("Samples").show(ui, |ui| {
            ui.label(egui::RichText::new("Sampler and SF2 arrive in M2").color(theme::TEXT_SECONDARY));
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
                if ui.selectable_label(selected, "●").on_hover_text("Route typing/MIDI/on-screen keys here").clicked() {
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
                if ui.button(egui::RichText::new(&ch.name).strong()).on_hover_text("Open instrument editor").clicked() {
                    select = Some(ch.id);
                    self.editor_view = EditorView::Synth;
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
                ui.label(egui::RichText::new(ch.instrument.type_id.as_str()).color(theme::TEXT_SECONDARY));
            });
        }
        if let Some(id) = select {
            self.selected_channel = Some(id);
        }
        for cmd in cmds {
            self.apply(cmd);
        }
        ui.add_space(6.0);
        ui.label(egui::RichText::new("Step sequencer, piano roll and playlist arrive in M3. Keys play the selected (●) channel.").color(theme::TEXT_SECONDARY));
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
        theme::apply(ctx);
        if ctx.input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::S))) {
            self.save_project();
        }
        self.handle_shortcuts(ctx);
        self.handle_typing(ctx);
        self.poll_midi();
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

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("menu").show(ui, |ui| self.menu(ui));
        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::Panel::left("browser").resizable(true).default_size(220.0).show(ui, |ui| self.browser(ui));
        egui::Panel::bottom("editor").resizable(true).default_size(380.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| match self.editor_view {
                EditorView::Rack => self.rack(ui),
                EditorView::Synth => synth_panel::show(self, ui),
            });
        });
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("Playlist");
            ui.separator();
            ui.label(
                egui::RichText::new("Arrangement arrives in M3. Play loops the selected pattern; hold typing keys or a MIDI keyboard to hear the synth.")
                    .color(theme::TEXT_SECONDARY),
            );
        });
        if self.show_settings {
            settings::show(self, ui.ctx());
        }
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        let _ = self.prefs.save();
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.send(EngineCommand::Panic { source: None });
        let _ = self.prefs.save();
    }
}

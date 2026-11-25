//! M0 GUI shell: dark window with the workspace layout skeleton, live audio/
//! device status and a playable typing keyboard driving the default synth.

use crate::audio::AudioSystem;
use crate::theme;
use rawdawg_audio_engine::commands::EngineCommand;
use rawdawg_audio_engine::plan::compile;
use rawdawg_audio_io::stream::StreamConfigRequest;
use rawdawg_audio_io::{DeviceSnapshot, enumerate};
use rawdawg_project::{History, Project};
use rawdawg_types::events::{EventSource, TimedEvent};
use rawdawg_types::limits::EngineLimits;
use rawdawg_types::{PRODUCT_NAME, TAGLINE, VERSION};
use std::collections::HashMap;
use std::path::PathBuf;

pub struct GuiOptions {
    pub open: Option<PathBuf>,
    pub safe_mode: bool,
    pub device: Option<String>,
    pub buffer: Option<u32>,
    pub no_audio: bool,
}

pub fn run_gui(opts: GuiOptions) -> anyhow::Result<()> {
    let viewport = egui::ViewportBuilder::default().with_title(PRODUCT_NAME).with_app_id("rawdawg").with_inner_size([1440.0, 900.0]).with_min_inner_size([1024.0, 720.0]);
    let native = eframe::NativeOptions { viewport, centered: true, ..Default::default() };
    eframe::run_native(PRODUCT_NAME, native, Box::new(move |cc| Ok(Box::new(App::new(cc, opts))))).map_err(|e| anyhow::anyhow!("GUI failed to start: {e}"))?;
    Ok(())
}

enum AudioState {
    Disabled,
    Running(Box<AudioSystem>),
    Failed(String),
}

struct App {
    project: Project,
    history: History,
    project_path: Option<PathBuf>,
    devices: DeviceSnapshot,
    audio: AudioState,
    limits: EngineLimits,
    plan_revision: u64,
    plan_error: Option<String>,
    typing_enabled: bool,
    typing_octave: i32,
    typing_velocity: f32,
    /// Held typing notes: key → (note id, pitch) so release uses the original pitch.
    held: HashMap<egui::Key, (u32, u8)>,
    next_note_id: u32,
    safe_mode: bool,
    status_line: String,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, opts: GuiOptions) -> Self {
        theme::apply(&cc.egui_ctx);
        let limits = EngineLimits::DEFAULT;
        let (project, project_path, mut status_line) = match &opts.open {
            Some(p) => match rawdawg_project::load_project_dir(p) {
                Ok(proj) => (proj, Some(p.clone()), format!("Opened {}", p.display())),
                Err(e) => (default_project(), None, format!("Could not open {}: {e}", p.display())),
            },
            None => (default_project(), None, "Ready".to_owned()),
        };
        let devices = enumerate();
        let audio = if opts.no_audio {
            AudioState::Disabled
        } else {
            let req = StreamConfigRequest { device_id: opts.device.clone(), sample_rate_hz: None, buffer_frames: opts.buffer.or(Some(256)) };
            match AudioSystem::start(&req, &limits) {
                Ok(sys) => AudioState::Running(Box::new(sys)),
                Err(e) => {
                    status_line = format!("Audio unavailable: {e:#}");
                    AudioState::Failed(format!("{e:#}"))
                }
            }
        };
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
            typing_octave: 0,
            typing_velocity: 0.8,
            held: HashMap::new(),
            next_note_id: 1,
            safe_mode: opts.safe_mode,
            status_line,
        };
        app.recompile();
        app
    }

    /// Compile the project and hand the plan to the engine (off the audio thread).
    fn recompile(&mut self) {
        let AudioState::Running(sys) = &mut self.audio else { return };
        let mut project = self.project.clone();
        project.sample_rate_hz = sys.sample_rate;
        self.plan_revision += 1;
        match compile(&project, self.plan_revision, rawdawg_audio_io::stream::MAX_CHUNK_FRAMES, &self.limits) {
            Ok(plan) => {
                sys.handle.collect_retired();
                if let Err(_plan) = sys.handle.publish_plan(plan) {
                    self.plan_error = Some("engine busy; retrying".to_owned());
                } else {
                    self.plan_error = None;
                }
            }
            Err(e) => self.plan_error = Some(e.to_string()),
        }
    }

    fn send(&mut self, cmd: EngineCommand) {
        if let AudioState::Running(sys) = &mut self.audio {
            let _ = sys.handle.send(cmd);
        }
    }

    fn selected_channel(&self) -> Option<rawdawg_types::ChannelId> {
        self.project.channels.first().map(|c| c.id)
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
        let Some(channel) = self.selected_channel() else { return };
        let text_focused = ctx.memory(|m| m.focused().is_some());
        let modifiers = ctx.input(|i| i.modifiers);
        let release_all = !self.typing_enabled || text_focused || !ctx.input(|i| i.focused);
        let mut to_send: Vec<EngineCommand> = Vec::new();
        if release_all {
            for (_, (id, pitch)) in self.held.drain() {
                to_send.push(EngineCommand::Live { channel, event: TimedEvent::note_off(0, EventSource::TypingKeyboard, 0, id, pitch) });
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
                        let pitch = 60 + self.typing_octave * 12 + semitone;
                        if !(0..=127).contains(&pitch) {
                            continue;
                        }
                        let id = self.next_note_id;
                        self.next_note_id = self.next_note_id.wrapping_add(1).max(1);
                        self.held.insert(key, (id, pitch as u8));
                        to_send.push(EngineCommand::Live { channel, event: TimedEvent::note_on(0, EventSource::TypingKeyboard, 0, id, pitch as u8, self.typing_velocity) });
                    } else if let Some((id, pitch)) = self.held.remove(&key) {
                        to_send.push(EngineCommand::Live { channel, event: TimedEvent::note_off(0, EventSource::TypingKeyboard, 0, id, pitch) });
                    }
                }
            }
        }
        for cmd in to_send {
            self.send(cmd);
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("raw").strong().size(18.0));
            ui.label(egui::RichText::new("DAW").strong().size(18.0).color(theme::ACCENT));
            ui.label(egui::RichText::new("g").strong().size(18.0));
            ui.separator();
            let playing = match &self.audio {
                AudioState::Running(s) => s.handle.status.playing.load(std::sync::atomic::Ordering::Relaxed),
                _ => false,
            };
            if ui.add_enabled(!playing, egui::Button::new("▶ Play")).clicked() {
                self.send(EngineCommand::Play);
            }
            if ui.add_enabled(playing, egui::Button::new("■ Stop")).clicked() {
                self.send(EngineCommand::Stop);
            }
            if ui.button("Panic").on_hover_text("All notes off").clicked() {
                self.send(EngineCommand::Panic { source: None });
            }
            ui.separator();
            let bpm = self.project.tempo.first().map(|t| t.us_per_quarter.bpm().0).unwrap_or(120.0);
            ui.label(format!("{bpm:.2} BPM"));
            ui.separator();
            ui.checkbox(&mut self.typing_enabled, "Typing keyboard").on_hover_text("Z S X D C V G B H N J M , L . ; /  play notes");
            ui.label("Octave");
            if ui.button("−").clicked() {
                self.typing_octave = (self.typing_octave - 1).max(-4);
            }
            ui.label(format!("C{}", 4 + self.typing_octave));
            if ui.button("+").clicked() {
                self.typing_octave = (self.typing_octave + 1).min(4);
            }
            ui.add(egui::Slider::new(&mut self.typing_velocity, 0.05..=1.0).text("Velocity"));
            ui.separator();
            let undo_label = self.history.undo_label();
            if ui.add_enabled(self.history.can_undo(), egui::Button::new("Undo")).on_hover_text(undo_label.unwrap_or("Nothing to undo")).clicked()
                && self.history.undo(&mut self.project).is_ok()
            {
                self.recompile();
            }
            let redo_label = self.history.redo_label();
            if ui.add_enabled(self.history.can_redo(), egui::Button::new("Redo")).on_hover_text(redo_label.unwrap_or("Nothing to redo")).clicked()
                && self.history.redo(&mut self.project).is_ok()
            {
                self.recompile();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                match &self.audio {
                    AudioState::Running(sys) => {
                        let st = &sys.stream.status;
                        let load = st.last_load(sys.stream.config.sample_rate_hz) * 100.0;
                        ui.label(egui::RichText::new(format!("CPU {load:.0}%")).color(if load > 80.0 { theme::WARNING } else { theme::TEXT_SECONDARY }));
                        let xruns = st.xruns.get();
                        if xruns > 0 {
                            ui.label(egui::RichText::new(format!("xruns {xruns}")).color(theme::WARNING));
                        }
                        ui.label(egui::RichText::new(format!("{} · {} Hz", sys.stream.config.device_name, sys.stream.config.sample_rate_hz)).color(theme::TEXT_SECONDARY));
                    }
                    AudioState::Disabled => {
                        ui.label(egui::RichText::new("Audio off").color(theme::WARNING));
                    }
                    AudioState::Failed(_) => {
                        ui.label(egui::RichText::new("Audio unavailable").color(theme::ERROR));
                    }
                }
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
            ui.label("rawDAWg Synth");
        });
        egui::CollapsingHeader::new("Audio devices").default_open(true).show(ui, |ui| {
            if self.devices.devices.is_empty() {
                ui.label(egui::RichText::new("No audio devices found").color(theme::WARNING));
            }
            for d in &self.devices.devices {
                let marker = if d.is_default_output { " (default)" } else { "" };
                ui.label(format!("{} [{}]{}", d.name, d.backend.label(), marker)).on_hover_text(format!("id: {}\noutput rates: {:?}", d.id, d.supported_project_rates()));
            }
            if ui.button("Rescan").clicked() {
                self.devices = enumerate();
            }
        });
        egui::CollapsingHeader::new("Effects").show(ui, |ui| {
            ui.label(egui::RichText::new("Arrives in M4").color(theme::TEXT_SECONDARY));
        });
        egui::CollapsingHeader::new("Samples").show(ui, |ui| {
            ui.label(egui::RichText::new("Arrives in M2/M5").color(theme::TEXT_SECONDARY));
        });
    }

    fn rack(&mut self, ui: &mut egui::Ui) {
        ui.heading("Channel rack");
        ui.separator();
        let mut toggles: Vec<rawdawg_project::Command> = Vec::new();
        for ch in &self.project.channels {
            ui.horizontal(|ui| {
                let mut mute = ch.mute;
                if ui.toggle_value(&mut mute, "M").on_hover_text("Mute").changed() {
                    toggles.push(rawdawg_project::Command::SetChannelMute { id: ch.id, mute });
                }
                ui.label(egui::RichText::new(&ch.name).strong());
                ui.label(egui::RichText::new(ch.instrument.type_id.as_str()).color(theme::TEXT_SECONDARY));
                ui.label(format!("{:.1} dB", ch.gain_db.0));
            });
        }
        for cmd in toggles {
            if self.history.apply(&mut self.project, cmd).is_ok() {
                self.recompile();
            }
        }
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new("Step sequencer, piano roll, playlist and mixer arrive in M3/M4. The typing keyboard plays the selected channel now.").color(theme::TEXT_SECONDARY),
        );
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(&self.status_line);
            ui.separator();
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
                    if let Some(e) = &self.plan_error {
                        ui.label(egui::RichText::new(e).color(theme::ERROR));
                    }
                }
                AudioState::Failed(e) => {
                    ui.label(egui::RichText::new(format!("Audio unavailable: {e}")).color(theme::ERROR));
                    if ui.button("Retry audio").clicked() {
                        let req = StreamConfigRequest { device_id: None, sample_rate_hz: None, buffer_frames: Some(256) };
                        match AudioSystem::start(&req, &self.limits) {
                            Ok(sys) => {
                                self.audio = AudioState::Running(Box::new(sys));
                                self.status_line = "Audio started".into();
                                self.recompile();
                            }
                            Err(e) => self.audio = AudioState::Failed(format!("{e:#}")),
                        }
                    }
                }
                AudioState::Disabled => {
                    ui.label(egui::RichText::new("Audio disabled (--no-audio); editing and offline render available").color(theme::WARNING));
                }
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
}

fn meter(ui: &mut egui::Ui, peak: f32) {
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
        self.handle_typing(ctx);
        if let AudioState::Running(sys) = &mut self.audio {
            sys.handle.collect_retired();
            if sys.stream.is_failed() {
                let msg = sys.stream.status.error_message().unwrap_or_else(|| "stream lost".into());
                self.audio = AudioState::Failed(msg);
                self.held.clear();
            }
        }
        // meters/status refresh at ≤60 Hz; audio does not wait on the UI
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::Panel::left("browser").resizable(true).default_size(220.0).show(ui, |ui| self.browser(ui));
        egui::Panel::bottom("editor").resizable(true).default_size(300.0).show(ui, |ui| self.rack(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("Playlist");
            ui.separator();
            ui.label(egui::RichText::new("Arrangement arrives in M3. Hold keys on the typing keyboard to hear the default synth.").color(theme::TEXT_SECONDARY));
        });
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.send(EngineCommand::Panic { source: None });
    }
}

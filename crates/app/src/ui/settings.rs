//! Settings window: Audio (device/rate/buffer, requested vs actual), MIDI devices,
//! typing keyboard map. Dark, non-modal; nothing changes until Apply.

use super::{App, AudioState};
use crate::theme;
use rawdawg_audio_io::stream::StreamConfigRequest;
use rawdawg_types::SampleRate;

#[derive(Default)]
pub(crate) struct SettingsState {
    pub device_id: Option<String>,
    pub sample_rate: Option<u32>,
    pub buffer: Option<u32>,
}

impl SettingsState {
    pub fn from_request(req: &StreamConfigRequest) -> Self {
        Self { device_id: req.device_id.clone(), sample_rate: req.sample_rate_hz, buffer: req.buffer_frames }
    }
    pub fn request(&self) -> StreamConfigRequest {
        StreamConfigRequest { device_id: self.device_id.clone(), sample_rate_hz: self.sample_rate, buffer_frames: self.buffer }
    }
}

pub(crate) fn show(app: &mut App, ctx: &egui::Context) {
    let mut open = app.show_settings;
    egui::Window::new("Settings").open(&mut open).default_size([560.0, 480.0]).resizable(true).collapsible(false).show(ctx, |ui| {
        ui.heading("Audio");
        let mut apply = false;
        let mut rescan = false;
        ui.horizontal(|ui| {
            ui.label("Output device");
            let current_name = app.settings.device_id.as_ref().and_then(|id| app.devices.find(id)).map(|d| d.name.clone()).unwrap_or_else(|| "System default".to_owned());
            egui::ComboBox::from_id_salt("audio-device").selected_text(current_name).width(300.0).show_ui(ui, |ui| {
                if ui.selectable_label(app.settings.device_id.is_none(), "System default").clicked() {
                    app.settings.device_id = None;
                }
                for d in app.devices.devices.iter().filter(|d| d.supports_output()) {
                    let label = format!("{} [{}]", d.name, d.backend.label());
                    if ui.selectable_label(app.settings.device_id.as_deref() == Some(d.id.as_str()), label).clicked() {
                        app.settings.device_id = Some(d.id.clone());
                    }
                }
            });
            if ui.button("Rescan").clicked() {
                rescan = true;
            }
        });
        let supported_rates: Vec<u32> = match app.settings.device_id.as_ref().and_then(|id| app.devices.find(id)) {
            Some(d) => d.supported_project_rates(),
            None => app.devices.default_output().map(|d| d.supported_project_rates()).unwrap_or_else(|| SampleRate::SUPPORTED.to_vec()),
        };
        ui.horizontal(|ui| {
            ui.label("Sample rate");
            let text = app.settings.sample_rate.map(|r| SampleRate(r).to_string()).unwrap_or_else(|| "Device default".to_owned());
            egui::ComboBox::from_id_salt("audio-rate").selected_text(text).show_ui(ui, |ui| {
                if ui.selectable_label(app.settings.sample_rate.is_none(), "Device default").clicked() {
                    app.settings.sample_rate = None;
                }
                for r in SampleRate::SUPPORTED {
                    let ok = supported_rates.contains(&r);
                    let label = if ok { SampleRate(r).to_string() } else { format!("{} (not offered by device)", SampleRate(r)) };
                    if ui.add_enabled(ok, egui::Button::selectable(app.settings.sample_rate == Some(r), label)).clicked() {
                        app.settings.sample_rate = Some(r);
                    }
                }
            });
        });
        ui.horizontal(|ui| {
            ui.label("Buffer");
            let text = app.settings.buffer.map(|b| format!("{b} frames")).unwrap_or_else(|| "Device default".to_owned());
            egui::ComboBox::from_id_salt("audio-buffer").selected_text(text).show_ui(ui, |ui| {
                if ui.selectable_label(app.settings.buffer.is_none(), "Device default").clicked() {
                    app.settings.buffer = None;
                }
                for b in rawdawg_types::limits::EngineLimits::PREFERRED_BLOCK_SIZES {
                    let latency_ms = app.settings.sample_rate.map(|r| b as f64 / r as f64 * 1000.0);
                    let label = match latency_ms {
                        Some(ms) => format!("{b} frames ({ms:.1} ms)"),
                        None => format!("{b} frames"),
                    };
                    if ui.selectable_label(app.settings.buffer == Some(b), label).clicked() {
                        app.settings.buffer = Some(b);
                    }
                }
            });
        });
        ui.horizontal(|ui| {
            if ui.button("Apply").on_hover_text("Reopen the audio device with these settings").clicked() {
                apply = true;
            }
            match &app.audio {
                AudioState::Running(sys) => {
                    let c = &sys.stream.config;
                    ui.label(
                        egui::RichText::new(format!(
                            "Actual: {} · {} · {} Hz · {} ch · {} · buffer {}",
                            c.device_name,
                            c.backend.label(),
                            c.sample_rate_hz,
                            c.channels,
                            c.sample_format,
                            c.requested_buffer_frames.map(|b| b.to_string()).unwrap_or_else(|| "device default".into())
                        ))
                        .color(theme::TEXT_SECONDARY),
                    );
                }
                AudioState::Failed(e) => {
                    ui.label(egui::RichText::new(format!("Not running: {e}")).color(theme::ERROR));
                }
                AudioState::Disabled => {
                    ui.label(egui::RichText::new("Audio off").color(theme::WARNING));
                }
            }
        });
        if let AudioState::Running(sys) = &app.audio {
            let st = &sys.stream.status;
            ui.label(
                egui::RichText::new(format!(
                    "callbacks {} · xruns {} · last callback {:.2} ms · max {:.2} ms · reported output latency {:.1} ms",
                    st.callbacks.get(),
                    st.xruns.get(),
                    st.last_callback_ns.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e6,
                    st.max_callback_ns.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e6,
                    st.output_latency_s.load() * 1000.0
                ))
                .color(theme::TEXT_SECONDARY),
            );
        }
        ui.separator();
        ui.heading("MIDI input");
        if !app.midi.is_available() {
            ui.label(egui::RichText::new(format!("MIDI backend unavailable: {}", app.midi.last_error().unwrap_or("unknown"))).color(theme::WARNING));
        }
        let devices = app.midi.devices();
        if devices.is_empty() {
            ui.label(egui::RichText::new("No MIDI input devices found").color(theme::TEXT_SECONDARY));
        }
        let mut toggles: Vec<(String, bool)> = Vec::new();
        for d in &devices {
            ui.horizontal(|ui| {
                let mut enabled = d.enabled;
                if ui.checkbox(&mut enabled, &d.name).changed() {
                    toggles.push((d.id.clone(), enabled));
                }
                let state = if d.connected { "connected" } else { "disconnected" };
                let color = if d.connected { theme::ACCENT } else { theme::WARNING };
                ui.label(egui::RichText::new(state).color(color));
                if d.enabled {
                    ui.label(
                        egui::RichText::new(format!("{} msgs{}", d.messages, if d.dropped > 0 { format!(", {} dropped", d.dropped) } else { String::new() }))
                            .color(theme::TEXT_SECONDARY),
                    );
                }
            });
        }
        if ui.button("Rescan MIDI").clicked() {
            app.midi.rescan();
        }
        for (id, enabled) in toggles {
            if enabled {
                match app.midi.enable(&id) {
                    Ok(()) => {
                        if !app.prefs.midi_enabled.contains(&id) {
                            app.prefs.midi_enabled.push(id);
                        }
                    }
                    Err(e) => app.status_line = format!("MIDI: {e}"),
                }
            } else {
                for ev in app.midi.disable(&id) {
                    app.live(ev);
                }
                app.prefs.midi_enabled.retain(|x| x != &id);
            }
            let _ = app.prefs.save();
        }
        ui.separator();
        ui.heading("Typing keyboard");
        ui.label("Lower row plays notes; the octave and velocity controls are in the toolbar.");
        ui.monospace(" S D   G H J   L ;      black keys");
        ui.monospace("Z X C V B N M , . /     white keys C D E F G A B C D E");
        ui.label(egui::RichText::new("Keys are ignored while a text field is focused or Ctrl/Alt is held; focus loss releases held notes.").color(theme::TEXT_SECONDARY));
        if rescan {
            app.devices = rawdawg_audio_io::enumerate();
        }
        if apply {
            let req = app.settings.request();
            app.restart_audio(&req);
        }
    });
    app.show_settings = open;
}

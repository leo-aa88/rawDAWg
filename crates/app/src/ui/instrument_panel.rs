//! Generic editor for non-synth native instruments (sampler, SoundFont):
//! descriptor-driven parameters with live preview and one undo entry per
//! gesture, plus instrument-specific extras (sample name, SF2 program list).

use super::App;
use crate::theme;
use rawdawg_project::Command;
use rawdawg_types::events::{EventKind, EventSource, TimedEvent};
use rawdawg_types::{InstanceId, ParamId};

#[derive(Default)]
pub(crate) struct InstrumentPanelState {
    gesture: Option<(InstanceId, Vec<u8>)>,
}

pub(crate) fn show(app: &mut App, ui: &mut egui::Ui, instance_id: InstanceId) {
    let Some(channel) = app.project.channels.iter().find(|c| c.instrument.id == instance_id).cloned() else { return };
    if channel.instrument.format != rawdawg_project::PluginFormat::Native {
        if let Err(e) = crate::plugins::ensure_external_mirror(&mut app.plugin_host, &mut app.other_mirrors, &channel.instrument) {
            ui.label(egui::RichText::new(format!("{} is not available: {e}", channel.instrument.display_name)).color(theme::WARNING));
            ui.label(egui::RichText::new("The channel stays silent; its plugin state is preserved in the project.").color(theme::TEXT_SECONDARY));
            return;
        }
    } else if !app.other_mirrors.contains_key(&instance_id) {
        match rawdawg_audio_engine::plan::make_instrument(&channel.instrument, &channel.name, &app.assets) {
            Ok((p, _)) => {
                app.other_mirrors.insert(instance_id, p);
            }
            Err(e) => {
                ui.label(egui::RichText::new(format!("Cannot edit this instrument: {e}")).color(theme::ERROR));
                return;
            }
        }
    }
    let type_id = channel.instrument.type_id.as_str().to_owned();
    let title = match type_id.as_str() {
        rawdawg_types::ids::native_types::SAMPLER => rawdawg_sampler::SAMPLER_DISPLAY_NAME,
        rawdawg_types::ids::native_types::SF2 => rawdawg_sampler::SF2_DISPLAY_NAME,
        _ if channel.instrument.format != rawdawg_project::PluginFormat::Native && !channel.instrument.display_name.is_empty() => channel.instrument.display_name.as_str(),
        _ => "Instrument",
    };
    ui.horizontal(|ui| {
        ui.heading(format!("{title} — {}", channel.name));
    });
    // instrument-specific header
    let mut sf2_change: Option<(u16, u16)> = None;
    {
        let mirror = app.other_mirrors.get_mut(&instance_id).expect("inserted above");
        if let Some(s) = mirror.as_any_mut().downcast_mut::<rawdawg_sampler::Sampler>() {
            match s.asset() {
                Some(a) => ui.label(format!("Sample: {} · {} Hz · {} ch · {:.2} s", a.name, a.sample_rate, a.channels.len(), a.frames() as f64 / f64::from(a.sample_rate))),
                None => ui.label(egui::RichText::new("Sample missing — drop an audio file onto the window or import one in the browser").color(theme::WARNING)),
            };
        } else if let Some(s) = mirror.as_any_mut().downcast_mut::<rawdawg_sampler::Sf2Player>() {
            let presets = s.presets();
            let (bank, program) = (s.patch().bank, s.patch().preset);
            match s.font() {
                Some(f) => {
                    ui.horizontal(|ui| {
                        ui.label(format!("SoundFont: {} · {} presets", f.name, presets.len()));
                        let current = presets
                            .iter()
                            .find(|(b, p, _)| *b == bank && *p == program)
                            .map(|(b, p, n)| format!("{b}:{p:03} {n}"))
                            .unwrap_or_else(|| format!("{bank}:{program:03} (not in this font)"));
                        egui::ComboBox::from_id_salt("sf2-program").selected_text(current).width(280.0).show_ui(ui, |ui| {
                            for (b, p, n) in &presets {
                                if ui.selectable_label(*b == bank && *p == program, format!("{b}:{p:03} {n}")).clicked() {
                                    sf2_change = Some((*b, *p));
                                }
                            }
                        });
                    });
                    let warnings = f.warnings();
                    if !warnings.is_empty() {
                        ui.label(egui::RichText::new(format!("{} unsupported feature(s) in this SoundFont: {}", warnings.len(), warnings.join("; "))).color(theme::WARNING));
                    }
                    if s.unmapped_notes > 0 {
                        ui.label(egui::RichText::new(format!("{} note(s) had no zone in this program", s.unmapped_notes)).color(theme::TEXT_SECONDARY));
                    }
                }
                None => {
                    ui.label(egui::RichText::new("SoundFont missing — import an .sf2 file in the browser").color(theme::WARNING));
                }
            }
        }
    }
    let descriptors: Vec<rawdawg_types::ParamDescriptor> = app.other_mirrors[&instance_id].descriptors().iter().filter(|d| !d.flags.hidden).copied().collect();
    let mut edits: Vec<(ParamId, f32, bool, bool)> = Vec::new();
    if let Some((b, p)) = sf2_change {
        let bd = descriptors.iter().find(|d| d.id.0 == rawdawg_sampler::sf2_player::P_BANK).unwrap();
        let pd = descriptors.iter().find(|d| d.id.0 == rawdawg_sampler::sf2_player::P_PRESET).unwrap();
        edits.push((bd.id, bd.to_normalized(f64::from(b)), true, false));
        edits.push((pd.id, pd.to_normalized(f64::from(p)), false, true));
    }
    egui::Grid::new(("instrument-params", instance_id)).num_columns(3).spacing([12.0, 4.0]).show(ui, |ui| {
        for d in &descriptors {
            let Some(current) = app.other_mirrors[&instance_id].param(d.id) else { continue };
            let labels = super::effect_panel::step_labels(&*app.other_mirrors[&instance_id], d);
            ui.label(d.name);
            let mut n = current;
            let (resp, changed) = rawdawg_ui::params::param_control(ui, ("inst-combo", instance_id, d.id.0), d, n, labels.as_deref());
            n = changed;
            let cc = app.midi_mapping_for(instance_id, d.id.0).map(|m| format!(" CC{}", m.cc)).unwrap_or_default();
            let learning = app.midi_learn.as_ref().is_some_and(|t| t.instance_id == instance_id && t.param_id == d.id.0);
            let resp = resp
                .context_menu(|ui| {
                    if ui.button("MIDI learn (then move a controller)").clicked() {
                        app.arm_midi_learn(instance_id, d.id.0);
                        ui.close();
                    }
                    if app.midi_mapping_for(instance_id, d.id.0).is_some() && ui.button("Remove MIDI mapping").clicked() {
                        app.remove_midi_mapping(instance_id, d.id.0);
                        ui.close();
                    }
                })
                .map(|r| r.response)
                .unwrap_or(resp);
            let readout = rawdawg_dsp::processor::display_value(&*app.other_mirrors[&instance_id], d, n);
            ui.label(egui::RichText::new(format!("{readout}{cc}{}", if learning { " learn…" } else { "" })).monospace().color(if learning {
                theme::WARNING
            } else {
                theme::TEXT_SECONDARY
            }));
            ui.end_row();
            if resp.changed() && (n - current).abs() > 1e-7 {
                let started = resp.drag_started() || app.instrument_panel.gesture.is_none();
                edits.push((d.id, n, started, !resp.dragged()));
            } else if resp.drag_stopped() {
                edits.push((d.id, n, false, true));
            }
        }
    });
    for (id, n, started, stopped) in edits {
        super::automation::capture_edit(app, instance_id, id.0, n, started, stopped);
        if started && app.instrument_panel.gesture.is_none() {
            let before = app.other_mirrors[&instance_id].save_state();
            app.instrument_panel.gesture = Some((instance_id, before));
        }
        if let Some(m) = app.other_mirrors.get_mut(&instance_id) {
            m.set_param(id, n);
        }
        if let Some(ch) = app.selected_channel {
            app.send(rawdawg_audio_engine::EngineCommand::Live { channel: ch, event: TimedEvent::new(0, EventSource::Mouse, 0, EventKind::Param { id, value: n }) });
        }
        if stopped {
            finish_gesture(app, instance_id, &type_id);
        }
    }
    ui.separator();
    super::keyboard::show(app, ui);
}

fn finish_gesture(app: &mut App, instance_id: InstanceId, type_id: &str) {
    let Some((gid, before)) = app.instrument_panel.gesture.take() else { return };
    if gid != instance_id {
        return;
    }
    let after = app.other_mirrors[&instance_id].save_state();
    if after == before {
        return;
    }
    let state: serde_json::Value = serde_json::from_slice(&after).unwrap_or(serde_json::Value::Null);
    app.mark_live_state(instance_id, type_id, &state);
    if let Err(e) = app.history.apply(&mut app.project, Command::SetInstanceState { instance_id, state_version: 1, state }) {
        app.status_line = format!("Edit rejected: {e}");
    }
}

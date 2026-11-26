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
        rawdawg_types::ids::native_types::MORPH => rawdawg_synthesizer::morph_rack::DISPLAY_NAME,
        _ if channel.instrument.format != rawdawg_project::PluginFormat::Native && !channel.instrument.display_name.is_empty() => channel.instrument.display_name.as_str(),
        _ => "Instrument",
    };
    let heading = format!("{title} — {}", channel.name);
    ui.horizontal(|ui| {
        ui.heading(&heading);
        if channel.instrument.format != rawdawg_project::PluginFormat::Native {
            crate::plugins::plugin_window_button(app, ui, instance_id, &heading);
        }
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
    let mut as_synth: Option<rawdawg_synthesizer::Patch> = None;
    if let Some(rack) = app.other_mirrors.get_mut(&instance_id).and_then(|m| m.as_any_mut().downcast_mut::<rawdawg_synthesizer::morph_rack::MorphRack>()) {
        let state = rack.state().clone();
        let renderers = rack.renderers();
        morph_rack_pad(ui, instance_id, &state, &mut edits, app.instrument_panel.gesture.is_none());
        ui.label(
            egui::RichText::new(format!("{renderers} distinct sound(s) play in parallel on every note; X and Y take automation and MIDI learn (right-click them below)"))
                .small()
                .color(theme::TEXT_SECONDARY),
        );
        ui.horizontal(|ui| {
            for (i, name) in ["A", "B", "C", "D"].iter().enumerate() {
                if ui.button(format!("Corner {name} as a synth")).on_hover_text("Replace the rack with this corner's sound as an ordinary synth (undoable)").clicked() {
                    as_synth = Some(state.corners[i].clone());
                }
            }
        });
    }
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
                    if ui.button("Create automation clip").on_hover_text("A curve for this parameter on the Automation track (plays in Song mode)").clicked() {
                        super::automation::create_clip_for(app, instance_id, d.id.0);
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
    if let Some(patch) = as_synth {
        let value = serde_json::to_value(&patch).unwrap_or(serde_json::Value::Null);
        app.apply(Command::SetInstrument { channel_id: channel.id, instance: rawdawg_project::Instance::native(rawdawg_types::ids::native_types::SYNTH, value) });
        return;
    }
    ui.separator();
    super::keyboard::show(app, ui);
}

/// The rack's XY pad: a drag moves X and Y together, as one gesture (one undo step).
fn morph_rack_pad(ui: &mut egui::Ui, instance_id: InstanceId, state: &rawdawg_synthesizer::morph_rack::MorphRackState, edits: &mut Vec<(ParamId, f32, bool, bool)>, idle: bool) {
    use rawdawg_synthesizer::morph_rack::{PARAM_X, PARAM_Y};
    let (_, rect) = ui.allocate_space(egui::vec2(200.0, 200.0));
    let resp = ui.interact(rect, morph_rack_pad_id(instance_id), egui::Sense::click_and_drag());
    let resp = resp.on_hover_text("Drag to crossfade the four corners (A top-left, B top-right, C bottom-left, D bottom-right)");
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, theme::BACKGROUND);
    for (corner, (pos, anchor)) in ["A", "B", "C", "D"].iter().zip([
        (rect.left_top(), egui::Align2::LEFT_TOP),
        (rect.right_top(), egui::Align2::RIGHT_TOP),
        (rect.left_bottom(), egui::Align2::LEFT_BOTTOM),
        (rect.right_bottom(), egui::Align2::RIGHT_BOTTOM),
    ]) {
        painter.text(
            pos + egui::vec2(if anchor.x() == egui::Align::Min { 4.0 } else { -4.0 }, if anchor.y() == egui::Align::Min { 2.0 } else { -2.0 }),
            anchor,
            *corner,
            egui::FontId::proportional(12.0),
            theme::TEXT_SECONDARY,
        );
    }
    let dot = egui::pos2(rect.min.x + state.x * rect.width(), rect.min.y + state.y * rect.height());
    painter.circle_filled(dot, 6.0, theme::ACCENT);
    if (resp.dragged() || resp.clicked() || resp.drag_stopped())
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let x = ((pos.x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
        let y = ((pos.y - rect.min.y) / rect.height()).clamp(0.0, 1.0);
        let started = resp.drag_started() || resp.clicked() || idle;
        let stopped = resp.clicked() || resp.drag_stopped();
        edits.push((ParamId(PARAM_X), x, started, false));
        edits.push((ParamId(PARAM_Y), y, false, stopped));
    }
}

/// The widget id of a morph rack's pad (tests drive it with pointer events).
pub(crate) fn morph_rack_pad_id(instance_id: InstanceId) -> egui::Id {
    egui::Id::unique(("morph-rack-pad", instance_id))
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

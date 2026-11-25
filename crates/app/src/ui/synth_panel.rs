//! Synth editor (M1 subset): parameter sliders with live preview through
//! `Param` events and one undo entry per gesture (`SetInstanceState`), the
//! on-screen keyboard, and a live output meter. Waveform sculpting, randomize,
//! evolve and morph arrive in M2 on top of this panel.

use super::{App, keyboard};
use crate::theme;
use rawdawg_dsp::processor::Processor;
use rawdawg_project::Command;
use rawdawg_synthesizer::params;
use rawdawg_types::events::{EventKind, EventSource, TimedEvent};
use rawdawg_types::params::ParamMapping;
use rawdawg_types::{InstanceId, ParamId};

#[derive(Default)]
pub(crate) struct SynthPanelState {
    /// Instance whose parameter gesture is in progress, with the pre-gesture state.
    gesture: Option<(InstanceId, Vec<u8>)>,
    pub advanced: bool,
    pub waveform: super::waveform::WaveformState,
}

const SIMPLE: &[u32] = &[params::AMP_ATTACK, params::AMP_RELEASE, params::FILTER_CUTOFF, params::FILTER_RESONANCE, params::MASTER_GAIN, params::VOICE_MODE];

pub(crate) fn show(app: &mut App, ui: &mut egui::Ui) {
    let Some(channel_id) = app.selected_channel else {
        ui.label("Select a channel in the rack.");
        return;
    };
    let Some(channel) = app.project.channel(channel_id).cloned() else { return };
    let instance_id = channel.instrument.id;
    if !app.synth_mirrors.contains_key(&instance_id) {
        ui.label(format!("{} is not a native synth.", channel.name));
        return;
    }
    ui.horizontal(|ui| {
        ui.heading(format!("rawDAWg Synth — {}", channel.name));
        ui.label(egui::RichText::new(app.synth_mirrors[&instance_id].patch().name.clone()).color(theme::TEXT_SECONDARY));
        ui.checkbox(&mut app.synth_panel.advanced, "Advanced");
        if ui.button("Reset to Init").clicked() {
            let state = rawdawg_synthesizer::Patch::default().to_json();
            commit_state(app, instance_id, state);
        }
    });
    super::waveform::show(app, ui, instance_id);
    ui.label(egui::RichText::new("Randomize, Evolve and XY Morph arrive later in M2.").color(theme::TEXT_SECONDARY));
    ui.separator();
    let descriptors: Vec<rawdawg_types::ParamDescriptor> = rawdawg_synthesizer::params::DESCRIPTORS.iter().filter(|d| !d.flags.hidden).copied().collect();
    let advanced = app.synth_panel.advanced;
    let mut edits: Vec<(ParamId, f32, bool, bool)> = Vec::new(); // (id, normalized, started, stopped)
    egui::Grid::new("synth-params").num_columns(3).spacing([12.0, 6.0]).show(ui, |ui| {
        let mut shown = 0;
        for d in descriptors.iter() {
            if !advanced && !SIMPLE.contains(&d.id.0) {
                continue;
            }
            let Some(current) = app.synth_mirrors[&instance_id].param(d.id) else { continue };
            ui.label(d.name);
            let mut n = current;
            let resp = match d.mapping {
                ParamMapping::Toggle => {
                    let mut b = n >= 0.5;
                    let r = ui.checkbox(&mut b, "");
                    n = if b { 1.0 } else { 0.0 };
                    r
                }
                ParamMapping::Discrete { steps } => {
                    let mut idx = d.to_physical(n) as i64;
                    let labels: Option<Vec<&'static str>> = match d.id.0 {
                        params::FILTER_MODE => Some(rawdawg_dsp::filter::FilterMode::ALL.iter().map(|m| m.label()).collect()),
                        params::VOICE_MODE => Some(rawdawg_synthesizer::VoiceMode::ALL.iter().map(|m| m.label()).collect()),
                        x if params::decode_layer_param(x).is_some_and(|(_, off)| off == params::L_WAVE) => Some(vec!["Sine", "Triangle", "Saw", "Pulse", "Wavetable"]),
                        _ => None,
                    };
                    match labels {
                        Some(labels) => {
                            let mut changed = false;
                            let text = labels.get(idx as usize).copied().unwrap_or("?");
                            let r = egui::ComboBox::from_id_salt(d.id.0).selected_text(text).show_ui(ui, |ui| {
                                for (i, l) in labels.iter().enumerate() {
                                    if ui.selectable_label(idx == i as i64, *l).clicked() {
                                        idx = i as i64;
                                        changed = true;
                                    }
                                }
                            });
                            if changed {
                                n = d.to_normalized(idx as f64);
                                edits.push((d.id, n, true, true));
                            }
                            r.response
                        }
                        None => {
                            let r = ui.add(egui::Slider::new(&mut idx, 0..=(steps as i64 - 1)).show_value(true));
                            n = d.to_normalized(idx as f64);
                            r
                        }
                    }
                }
                _ => ui.add(egui::Slider::new(&mut n, 0.0..=1.0).show_value(false)),
            };
            let labeled_combo = matches!(d.id.0, params::FILTER_MODE | params::VOICE_MODE) || params::decode_layer_param(d.id.0).is_some_and(|(_, off)| off == params::L_WAVE);
            if labeled_combo {
                ui.label("");
            } else {
                ui.label(egui::RichText::new(d.format(n)).monospace().color(theme::TEXT_SECONDARY));
            }
            ui.end_row();
            if resp.changed() && (n - current).abs() > 1e-7 {
                let started = resp.drag_started() || !app.synth_panel.gesture.as_ref().is_some_and(|(i, _)| *i == instance_id);
                edits.push((d.id, n, started, !resp.dragged()));
            } else if resp.drag_stopped() {
                edits.push((d.id, n, false, true));
            }
            shown += 1;
        }
        if shown == 0 {
            ui.label("No parameters");
            ui.end_row();
        }
    });
    for (id, n, started, stopped) in edits {
        if started && app.synth_panel.gesture.is_none() {
            let before = app.synth_mirrors[&instance_id].save_state();
            app.synth_panel.gesture = Some((instance_id, before));
        }
        if let Some(m) = app.synth_mirrors.get_mut(&instance_id) {
            m.set_param(id, n);
        }
        // live preview on the audio thread (no plan rebuild, no retrigger)
        if let Some(ch) = app.selected_channel {
            app.send(rawdawg_audio_engine::EngineCommand::Live { channel: ch, event: TimedEvent::new(0, EventSource::Mouse, 0, EventKind::Param { id, value: n }) });
        }
        if stopped {
            finish_gesture(app, instance_id);
        }
    }
    ui.separator();
    ui.horizontal(|ui| {
        ui.label("Output");
        if let super::AudioState::Running(sys) = &app.audio {
            let st = &sys.handle.status;
            super::meter(ui, st.master_peak_l.load().max(st.master_peak_r.load()));
            ui.label(format!("{} voices", st.active_voices.load(std::sync::atomic::Ordering::Relaxed)));
        }
    });
    keyboard::show(app, ui);
}

/// Commit the current mirror state as one undo entry without rebuilding the plan
/// (the engine already applied the parameter events).
fn finish_gesture(app: &mut App, instance_id: InstanceId) {
    let Some((gid, before)) = app.synth_panel.gesture.take() else { return };
    if gid != instance_id {
        return;
    }
    let after = app.synth_mirrors[&instance_id].save_state();
    if after == before {
        return;
    }
    let state: serde_json::Value = serde_json::from_slice(&after).unwrap_or(serde_json::Value::Null);
    // the engine already holds these values (live Param events): mark before committing so
    // any recompile carries the instance over instead of restarting it
    app.mark_live_state(instance_id, rawdawg_synthesizer::TYPE_ID, &state);
    if let Err(e) = app.history.apply(&mut app.project, Command::SetInstanceState { instance_id, state_version: 1, state }) {
        app.status_line = format!("Edit rejected: {e}");
    }
}

/// Replace the whole patch (preset load / reset): undoable, rebuilds mirrors and plan.
fn commit_state(app: &mut App, instance_id: InstanceId, state: Vec<u8>) {
    let value: serde_json::Value = serde_json::from_slice(&state).unwrap_or(serde_json::Value::Null);
    app.apply(Command::SetInstanceState { instance_id, state_version: 1, state: value });
}

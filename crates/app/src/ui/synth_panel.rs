//! Synth editor: waveform canvas, simple controls, advanced sections (layers with
//! generator choice, filter/envelopes, LFOs, modulation matrix), on-screen
//! keyboard and a live output meter. Parameter drags preview live through
//! `Param` events and commit as one undo entry per gesture.

use super::{App, keyboard};
use crate::theme;
use rawdawg_dsp::processor::Processor;
use rawdawg_project::Command;
use rawdawg_synthesizer::params::{self, ParamBlock};
use rawdawg_synthesizer::patch::{Generator, Layer, MAX_LAYERS, MAX_MOD_ROUTES, ModRoute, ModSource, ModTarget};
use rawdawg_types::events::{EventKind, EventSource, TimedEvent};
use rawdawg_types::params::ParamMapping;
use rawdawg_types::{InstanceId, ParamDescriptor, ParamId};

pub(crate) struct SynthPanelState {
    /// Instance whose parameter gesture is in progress, with the pre-gesture state.
    gesture: Option<(InstanceId, Vec<u8>)>,
    pub advanced: bool,
    pub waveform: super::waveform::WaveformState,
    pub evolve: super::evolve::EvolveState,
    pub presets: super::presets::PresetBrowserState,
    pub graph: super::graph_editor::GraphEditorState,
}

impl Default for SynthPanelState {
    fn default() -> Self {
        Self { gesture: None, advanced: false, waveform: Default::default(), evolve: super::evolve::EvolveState::new(), presets: Default::default(), graph: Default::default() }
    }
}

impl SynthPanelState {
    pub(crate) fn waveform_builder(&mut self) -> &mut rawdawg_dsp::wavetable::TableBuilder {
        self.waveform.builder_mut()
    }
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
            commit_patch(app, instance_id, rawdawg_synthesizer::Patch::default());
        }
    });
    super::presets::show(app, ui, instance_id);
    super::waveform::show(app, ui, instance_id);
    super::evolve::randomize_bar(app, ui, instance_id);
    ui.separator();
    let descriptors: &'static [ParamDescriptor] = &rawdawg_synthesizer::params::DESCRIPTORS;
    let mut edits: Vec<(ParamId, f32, bool, bool)> = Vec::new();
    if !app.synth_panel.advanced {
        let simple: Vec<&ParamDescriptor> = descriptors.iter().filter(|d| SIMPLE.contains(&d.id.0)).collect();
        param_grid(app, ui, instance_id, "synth-simple", &simple, &mut edits);
    } else {
        let patch = app.synth_mirrors[&instance_id].patch().clone();
        // Layers: generator choice + core + generator-specific params
        for (li, layer) in patch.layers.iter().enumerate() {
            let kind = layer.generator.kind_index();
            egui::CollapsingHeader::new(format!("Layer {} — {}", li + 1, Generator::KINDS[kind])).default_open(li == 0).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Generator");
                    let mut new_kind = kind;
                    egui::ComboBox::from_id_salt(("gen-kind", li)).selected_text(Generator::KINDS[kind]).show_ui(ui, |ui| {
                        for (i, k) in Generator::KINDS.iter().enumerate() {
                            ui.selectable_value(&mut new_kind, i, *k);
                        }
                    });
                    if new_kind != kind {
                        let mut p = patch.clone();
                        p.layers[li].generator = Generator::default_of_kind(new_kind);
                        commit_patch(app, instance_id, p);
                    }
                    if patch.layers.len() > 1 && ui.button("Remove layer").clicked() {
                        let mut p = patch.clone();
                        p.layers.remove(li);
                        commit_patch(app, instance_id, p);
                    }
                    ui.label(egui::RichText::new(match kind {
                        2 => "Phase modulation, 4 operators",
                        3 => "Partials; harmonic mode uses a table, detuned mode a live bank",
                        4 => "Grains read from this layer's wave (edit it on the canvas)",
                        5 => "Plucked string (Karplus–Strong)",
                        _ => "",
                    })
                    .color(theme::TEXT_SECONDARY));
                });
                let core: Vec<&ParamDescriptor> = descriptors.iter().filter(|d| matches!(params::decode(d.id.0), Some(ParamBlock::Layer { layer, offset }) if layer == li && offset != params::L_KIND && generator_core_visible(kind, offset))).collect();
                param_grid(app, ui, instance_id, ("layer-core", li), &core, &mut edits);
                let gen_params: Vec<&ParamDescriptor> = descriptors.iter().filter(|d| matches!(params::decode(d.id.0), Some(ParamBlock::Gen { layer, offset }) if layer == li && gen_visible(kind, offset))).collect();
                if !gen_params.is_empty() {
                    param_grid(app, ui, instance_id, ("layer-gen", li), &gen_params, &mut edits);
                }
            });
        }
        if patch.layers.len() < MAX_LAYERS && ui.button("+ Add layer").clicked() {
            let mut p = patch.clone();
            p.layers.push(Layer::basic(rawdawg_dsp::oscillator::BasicWave::Saw));
            commit_patch(app, instance_id, p);
        }
        egui::CollapsingHeader::new("Filter, envelopes and voice").default_open(true).show(ui, |ui| {
            let globals: Vec<&ParamDescriptor> = descriptors.iter().filter(|d| matches!(params::decode(d.id.0), Some(ParamBlock::Global))).collect();
            param_grid(app, ui, instance_id, "synth-global", &globals, &mut edits);
        });
        egui::CollapsingHeader::new("LFOs").show(ui, |ui| {
            for n in 0..rawdawg_synthesizer::patch::MAX_LFOS {
                ui.label(egui::RichText::new(format!("LFO {}", n + 1)).strong());
                let lfo: Vec<&ParamDescriptor> = descriptors.iter().filter(|d| matches!(params::decode(d.id.0), Some(ParamBlock::Lfo { lfo, .. }) if lfo == n)).collect();
                param_grid(app, ui, instance_id, ("lfo", n), &lfo, &mut edits);
            }
        });
        egui::CollapsingHeader::new("Modulation matrix").show(ui, |ui| {
            mod_matrix(app, ui, instance_id, &patch);
        });
        let header = egui::CollapsingHeader::new("Modular graph").default_open(!patch.graph.is_empty()).show(ui, |ui| {
            super::graph_editor::show(app, ui, instance_id, &patch);
        });
        if app.synth_panel.graph.focus_frames > 0 {
            app.synth_panel.graph.focus_frames -= 1;
            header.header_response.scroll_to_me(Some(egui::Align::TOP));
        }
    }
    apply_edits(app, instance_id, edits);
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

fn generator_core_visible(kind: usize, offset: u32) -> bool {
    match offset {
        params::L_WAVE | params::L_PULSE_WIDTH => kind == 0,
        params::L_WT_POSITION => kind == 1,
        params::L_UNISON | params::L_UNISON_DETUNE | params::L_UNISON_SPREAD => kind <= 2,
        _ => true,
    }
}

fn gen_visible(kind: usize, offset: u32) -> bool {
    match kind {
        2 => offset <= params::G_FM_ALGORITHM,
        3 => matches!(offset, params::G_ADD_STRETCH | params::G_ADD_DETUNED),
        4 => (params::G_GRAIN_SIZE..=params::G_GRAIN_WINDOW).contains(&offset),
        5 => offset >= params::G_PLUCK_DAMPING,
        _ => false,
    }
}

fn labels_for(d: &ParamDescriptor) -> Option<Vec<&'static str>> {
    match params::decode(d.id.0)? {
        ParamBlock::Global => match d.id.0 {
            params::FILTER_MODE => Some(rawdawg_dsp::filter::FilterMode::ALL.iter().map(|m| m.label()).collect()),
            params::VOICE_MODE => Some(rawdawg_synthesizer::VoiceMode::ALL.iter().map(|m| m.label()).collect()),
            _ => None,
        },
        ParamBlock::Layer { offset: params::L_WAVE, .. } => Some(vec!["Sine", "Triangle", "Saw", "Pulse"]),
        ParamBlock::Gen { offset: params::G_FM_ALGORITHM, .. } => Some(rawdawg_synthesizer::patch::FmAlgorithm::ALL.iter().map(|a| a.label()).collect()),
        ParamBlock::Gen { offset: params::G_GRAIN_WINDOW, .. } => Some(vec!["Hann", "Triangle", "Rectangular"]),
        ParamBlock::Gen { offset: params::G_PLUCK_EXCITATION, .. } => Some(vec!["Noise", "Impulse", "Pick"]),
        ParamBlock::Lfo { offset: params::LFO_SHAPE, .. } => Some(rawdawg_synthesizer::patch::LfoShape::ALL.iter().map(|s| s.label()).collect()),
        _ => None,
    }
}

/// A grid of parameter widgets; collects edits as (id, normalized, gesture start, gesture end).
fn param_grid(
    app: &mut App,
    ui: &mut egui::Ui,
    instance_id: InstanceId,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    descs: &[&ParamDescriptor],
    edits: &mut Vec<(ParamId, f32, bool, bool)>,
) {
    egui::Grid::new(id_salt).num_columns(3).spacing([12.0, 4.0]).show(ui, |ui| {
        for d in descs {
            let Some(current) = app.synth_mirrors[&instance_id].param(d.id) else { continue };
            ui.label(d.name);
            let mut n = current;
            let labeled = labels_for(d);
            let resp = match d.mapping {
                ParamMapping::Toggle => {
                    let mut b = n >= 0.5;
                    let r = ui.checkbox(&mut b, "");
                    n = if b { 1.0 } else { 0.0 };
                    r
                }
                ParamMapping::Discrete { steps } => {
                    let mut idx = d.to_physical(n) as i64;
                    match &labeled {
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
            let resp = resp
                .context_menu(|ui| {
                    let locked = app.synth_panel.evolve.locks.params.contains(&d.id.0);
                    if ui.button(if locked { "Unlock for randomize" } else { "Lock for randomize" }).clicked() {
                        app.synth_panel.evolve.locks.set_param(d.id.0, !locked);
                        ui.close();
                    }
                    if ui.button("Reset to default").clicked() {
                        edits.push((d.id, d.default_normalized, true, true));
                        ui.close();
                    }
                    ui.separator();
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
                    if ui
                        .button("Automate in this pattern")
                        .on_hover_text("A curve inside the selected pattern: it plays whenever the pattern plays, in Pattern mode and in Song mode")
                        .clicked()
                    {
                        super::automation::create_pattern_curve_for(app, instance_id, d.id.0);
                        ui.close();
                    }
                })
                .map(|r| r.response)
                .unwrap_or(resp);
            let cc = app.midi_mapping_for(instance_id, d.id.0).map(|m| format!(" CC{}", m.cc)).unwrap_or_default();
            let learning = app.midi_learn.as_ref().is_some_and(|t| t.instance_id == instance_id && t.param_id == d.id.0);
            if labeled.is_some() {
                ui.label(egui::RichText::new(if learning { "learn…" } else { "" }).color(theme::WARNING));
            } else {
                let locked = app.synth_panel.evolve.locks.params.contains(&d.id.0);
                ui.label(
                    egui::RichText::new(format!("{}{}{}{}", d.format(n), if locked { " 🔒" } else { "" }, cc, if learning { " learn…" } else { "" }))
                        .monospace()
                        .color(if learning { theme::WARNING } else { theme::TEXT_SECONDARY }),
                );
            }
            ui.end_row();
            if resp.changed() && (n - current).abs() > 1e-7 {
                let started = resp.drag_started() || !app.synth_panel.gesture.as_ref().is_some_and(|(i, _)| *i == instance_id);
                edits.push((d.id, n, started, !resp.dragged()));
            } else if resp.drag_stopped() {
                edits.push((d.id, n, false, true));
            }
        }
    });
}

pub(crate) fn apply_edits(app: &mut App, instance_id: InstanceId, edits: Vec<(ParamId, f32, bool, bool)>) {
    for (id, n, started, stopped) in edits {
        super::automation::capture_edit(app, instance_id, id.0, n, started, stopped);
        if started && app.synth_panel.gesture.is_none() {
            let before = app.synth_mirrors[&instance_id].save_state();
            app.synth_panel.gesture = Some((instance_id, before));
        }
        if let Some(m) = app.synth_mirrors.get_mut(&instance_id) {
            m.set_param(id, n);
        }
        if let Some(ch) = app.selected_channel {
            app.send(rawdawg_audio_engine::EngineCommand::Live { channel: ch, event: TimedEvent::new(0, EventSource::Mouse, 0, EventKind::Param { id, value: n }) });
        }
        if stopped {
            finish_gesture(app, instance_id);
        }
    }
}

fn mod_matrix(app: &mut App, ui: &mut egui::Ui, instance_id: InstanceId, patch: &rawdawg_synthesizer::Patch) {
    let mut changed: Option<rawdawg_synthesizer::Patch> = None;
    egui::Grid::new("mod-matrix").num_columns(5).spacing([10.0, 4.0]).show(ui, |ui| {
        ui.label(egui::RichText::new("On").strong());
        ui.label(egui::RichText::new("Source").strong());
        ui.label(egui::RichText::new("Target").strong());
        ui.label(egui::RichText::new("Depth").strong());
        ui.label("");
        ui.end_row();
        for (i, r) in patch.mod_routes.iter().enumerate() {
            let mut route = *r;
            let mut dirty = false;
            if ui.checkbox(&mut route.enabled, "").changed() {
                dirty = true;
            }
            egui::ComboBox::from_id_salt(("mod-src", i)).selected_text(route.source.label()).show_ui(ui, |ui| {
                for s in ModSource::ALL {
                    if ui.selectable_value(&mut route.source, s, s.label()).changed() {
                        dirty = true;
                    }
                }
            });
            egui::ComboBox::from_id_salt(("mod-tgt", i)).selected_text(route.target.label()).show_ui(ui, |ui| {
                for t in ModTarget::ALL {
                    if ui.selectable_value(&mut route.target, t, t.label()).changed() {
                        dirty = true;
                    }
                }
            });
            let resp = ui.add(egui::Slider::new(&mut route.depth, -1.0..=1.0).fixed_decimals(2));
            if resp.changed() {
                // live preview of depth through the param path; commit on release
                let id = ParamId(params::mod_param(i as u32, params::MOD_DEPTH));
                let d = rawdawg_synthesizer::params::DESCRIPTORS.iter().find(|d| d.id == id).unwrap();
                let n = d.to_normalized(f64::from(route.depth));
                if let Some(m) = app.synth_mirrors.get_mut(&instance_id) {
                    m.set_param(id, n);
                }
                if let Some(ch) = app.selected_channel {
                    app.send(rawdawg_audio_engine::EngineCommand::Live { channel: ch, event: TimedEvent::new(0, EventSource::Mouse, 0, EventKind::Param { id, value: n }) });
                }
            }
            if resp.drag_stopped() {
                dirty = true;
            }
            if ui.small_button("✕").on_hover_text("Remove route").clicked() {
                let mut p = patch.clone();
                p.mod_routes.remove(i);
                changed = Some(p);
            } else if dirty {
                let mut p = patch.clone();
                p.mod_routes[i] = route;
                changed = Some(p);
            }
            ui.end_row();
        }
    });
    if patch.mod_routes.len() < MAX_MOD_ROUTES && ui.button("+ Add route").clicked() {
        let mut p = patch.clone();
        p.mod_routes.push(ModRoute { source: ModSource::Lfo1, target: ModTarget::Cutoff, depth: 0.25, enabled: true });
        changed = Some(p);
    }
    if let Some(p) = changed {
        commit_patch(app, instance_id, p);
    }
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
    app.mark_live_state(instance_id, rawdawg_synthesizer::TYPE_ID, &state);
    if let Err(e) = app.history.apply(&mut app.project, Command::SetInstanceState { instance_id, state_version: 1, state }) {
        app.status_line = format!("Edit rejected: {e}");
    }
}

/// Replace the whole patch (structure change, preset load, reset): undoable,
/// rebuilds the mirror and the plan (the live instance is replaced).
pub(crate) fn commit_patch(app: &mut App, instance_id: InstanceId, patch: rawdawg_synthesizer::Patch) {
    if let Err(e) = patch.validate() {
        app.status_line = format!("Patch rejected: {e}");
        return;
    }
    let value: serde_json::Value = serde_json::to_value(&patch).unwrap_or(serde_json::Value::Null);
    app.apply(Command::SetInstanceState { instance_id, state_version: 1, state: value });
}

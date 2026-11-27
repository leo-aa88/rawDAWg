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

/// The few controls under the wave editor in the simple view (the envelope has its own editor).
const SIMPLE: &[u32] = &[params::FILTER_CUTOFF, params::FILTER_RESONANCE, params::MASTER_GAIN, params::VOICE_MODE];

/// Most oscillators the Oscillators section stacks.
pub(crate) const MAX_OSCILLATORS: usize = 3;

/// Panel width from which the oscillators and the envelope sit side by side (the oscillator
/// table needs about 640 points).
const TWO_COLUMNS_FROM: f32 = 1340.0;

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
    let mut basics: Vec<(ParamId, f32, bool, bool)> = Vec::new();
    if ui.available_width() >= TWO_COLUMNS_FROM {
        ui.columns(2, |cols| {
            oscillators(app, &mut cols[0], instance_id, &mut basics);
            envelope_section(app, &mut cols[1], instance_id, &mut basics);
        });
    } else {
        oscillators(app, ui, instance_id, &mut basics);
        ui.separator();
        envelope_section(app, ui, instance_id, &mut basics);
    }
    apply_edits(app, instance_id, basics);
    ui.separator();
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

fn desc(pid: u32) -> Option<&'static ParamDescriptor> {
    params::DESCRIPTORS.iter().find(|d| d.id.0 == pid)
}

/// A slider over a parameter shown in its own units (`to_normalized` maps them), with the
/// parameter menu and the usual gesture: live while dragging, one undo step on release.
/// `label` names it for screen readers (and tests).
#[allow(clippy::too_many_arguments)]
fn unit_slider(
    app: &mut App,
    ui: &mut egui::Ui,
    instance_id: InstanceId,
    label: &str,
    pid: u32,
    value: f32,
    range: std::ops::RangeInclusive<f32>,
    to_normalized: impl Fn(f32) -> f32,
    edits: &mut Vec<(ParamId, f32, bool, bool)>,
) -> egui::Response {
    let Some(d) = desc(pid) else { return ui.label("") };
    let current = app.synth_mirrors[&instance_id].param(d.id).unwrap_or(d.default_normalized);
    let mut v = value;
    let resp = ui.add(rawdawg_ui::params::slider(ui, &mut v, range).show_value(false));
    let resp = rawdawg_ui::params::context_menu(&resp, |ui| param_menu(app, ui, instance_id, d, edits)).map(|r| r.response).unwrap_or(resp);
    resp.widget_info(|| egui::WidgetInfo::slider(true, f64::from(v), label));
    let n = to_normalized(v).clamp(0.0, 1.0);
    if resp.changed() && (n - current).abs() > 1e-7 {
        let started = resp.drag_started() || !app.synth_panel.gesture.as_ref().is_some_and(|(i, _)| *i == instance_id);
        edits.push((d.id, n, started, !resp.dragged()));
    } else if resp.drag_stopped() {
        edits.push((d.id, current, false, true));
    }
    resp
}

/// The quick starts of the Oscillators section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Template {
    Sine,
    Saw,
    Square,
    Triangle,
    SawSquare,
    SawOctaves,
    DetunedSaws,
}

impl Template {
    pub(crate) const ALL: [Template; 7] = [Template::Sine, Template::Saw, Template::Square, Template::Triangle, Template::SawSquare, Template::SawOctaves, Template::DetunedSaws];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Template::Sine => "Sine",
            Template::Saw => "Saw",
            Template::Square => "Square",
            Template::Triangle => "Triangle",
            Template::SawSquare => "Saw + Square",
            Template::SawOctaves => "Saw octaves",
            Template::DetunedSaws => "Detuned saws + sub",
        }
    }

    fn tip(self) -> &'static str {
        match self {
            Template::SawSquare => "A saw and a square together",
            Template::SawOctaves => "Three saws: as played, one octave up, two octaves up",
            Template::DetunedSaws => "Two saws 7 cents apart and a sine an octave below",
            _ => "One oscillator with this wave",
        }
    }

    /// A fresh sound (Init's envelope and filter) with these oscillators.
    pub(crate) fn patch(self) -> rawdawg_synthesizer::Patch {
        use rawdawg_dsp::oscillator::BasicWave::{Pulse, Saw, Sine, Triangle};
        let osc = |wave, octave: i32, cents: f32, level: f32| Layer { coarse_semitones: 12 * octave, fine_cents: cents, level_db: 20.0 * level.log10(), ..Layer::basic(wave) };
        let layers = match self {
            Template::Sine => vec![osc(Sine, 0, 0.0, 1.0)],
            Template::Saw => vec![osc(Saw, 0, 0.0, 1.0)],
            Template::Square => vec![osc(Pulse, 0, 0.0, 1.0)],
            Template::Triangle => vec![osc(Triangle, 0, 0.0, 1.0)],
            Template::SawSquare => vec![osc(Saw, 0, 0.0, 0.7), osc(Pulse, 0, 0.0, 0.7)],
            Template::SawOctaves => vec![osc(Saw, 0, 0.0, 0.6), osc(Saw, 1, 0.0, 0.6), osc(Saw, 2, 0.0, 0.6)],
            Template::DetunedSaws => vec![osc(Saw, 0, -7.0, 0.7), osc(Saw, 0, 7.0, 0.7), osc(Sine, -1, 0.0, 0.4)],
        };
        rawdawg_synthesizer::Patch { name: self.label().to_owned(), layers, ..rawdawg_synthesizer::Patch::default() }
    }
}

/// The waves an oscillator row offers, in menu order.
const WAVES: [rawdawg_dsp::oscillator::BasicWave; 4] =
    [rawdawg_dsp::oscillator::BasicWave::Sine, rawdawg_dsp::oscillator::BasicWave::Saw, rawdawg_dsp::oscillator::BasicWave::Pulse, rawdawg_dsp::oscillator::BasicWave::Triangle];

fn wave_name(w: rawdawg_dsp::oscillator::BasicWave) -> &'static str {
    use rawdawg_dsp::oscillator::BasicWave::*;
    match w {
        Sine => "Sine",
        Saw => "Saw",
        Pulse => "Square",
        Triangle => "Triangle",
    }
}

/// What an oscillator plays, in the words of the Waveform column.
fn generator_name(g: &Generator) -> &'static str {
    match g {
        Generator::Basic { wave, .. } => wave_name(*wave),
        Generator::Wavetable { .. } => "Custom wave",
        Generator::Fm(_) => "FM",
        Generator::Additive(_) => "Additive",
        Generator::Granular { .. } => "Granular",
        Generator::Pluck(_) => "Pluck",
    }
}

/// What a readout adds after its value: locked for randomize, its MIDI controller, MIDI learn.
fn param_badges(app: &App, instance_id: InstanceId, pid: u32) -> String {
    let locked = app.synth_panel.evolve.locks.params.contains(&pid);
    let cc = app.midi_mapping_for(instance_id, pid).map(|m| format!(" CC{}", m.cc)).unwrap_or_default();
    let learning = app.midi_learn.as_ref().is_some_and(|t| t.instance_id == instance_id && t.param_id == pid);
    format!("{}{cc}{}", if locked { " 🔒" } else { "" }, if learning { " learn…" } else { "" })
}

/// Level in percent of full scale for a level in dB (−60 dB and below read 0 %).
fn level_percent(db: f32) -> f32 {
    if db <= -60.0 { 0.0 } else { 100.0 * 10f32.powf(db / 20.0) }
}

fn percent_db(pct: f32) -> f32 {
    if pct <= 0.1 { -60.0 } else { (20.0 * (pct / 100.0).log10()).max(-60.0) }
}

/// Oscillators: quick starts, then one row per layer (on, waveform, octave, detune, level).
fn oscillators(app: &mut App, ui: &mut egui::Ui, instance_id: InstanceId, edits: &mut Vec<(ParamId, f32, bool, bool)>) {
    let patch = app.synth_mirrors[&instance_id].patch().clone();
    ui.label(egui::RichText::new("Oscillators").strong());
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new("Start from").color(theme::TEXT_SECONDARY));
        for t in Template::ALL {
            if ui.button(t.label()).on_hover_text(format!("{} (a fresh sound; Undo goes back)", t.tip())).clicked() {
                commit_patch(app, instance_id, t.patch());
                app.status_line = format!("Started from {}", t.label());
            }
        }
    });
    let param = |app: &App, layer: usize, offset: u32| -> Option<(u32, f32, f64)> {
        let pid = params::layer_param(layer as u32, offset);
        let d = desc(pid)?;
        let n = app.synth_mirrors[&instance_id].param(d.id)?;
        Some((pid, n, d.to_physical(n)))
    };
    let mut structural: Option<rawdawg_synthesizer::Patch> = None;
    egui::Grid::new(("oscillators", instance_id)).num_columns(6).spacing([10.0, 4.0]).show(ui, |ui| {
        for h in ["Oscillator", "Waveform", "Octave", "Detune", "Level", ""] {
            ui.label(egui::RichText::new(h).color(theme::TEXT_SECONDARY));
        }
        ui.end_row();
        for (i, layer) in patch.layers.iter().enumerate() {
            // on/off
            let enabled = param(app, i, params::L_ENABLED).map_or(layer.enabled, |p| p.2 >= 0.5);
            let mut on = enabled;
            if ui.checkbox(&mut on, format!("OSC {}", i + 1)).on_hover_text("Switch this oscillator on or off").changed()
                && let Some((pid, ..)) = param(app, i, params::L_ENABLED)
            {
                edits.push((ParamId(pid), if on { 1.0 } else { 0.0 }, true, true));
            }
            // waveform
            let live_wave = match &layer.generator {
                Generator::Basic { .. } => param(app, i, params::L_WAVE).map(|p| rawdawg_dsp::oscillator::BasicWave::ALL[(p.2 as usize).min(3)]),
                _ => None,
            };
            let shown = live_wave.map_or_else(|| generator_name(&layer.generator), wave_name);
            let mut pick = None;
            egui::ComboBox::from_id_salt(("osc-wave", instance_id, i)).selected_text(shown).width(110.0).show_ui(ui, |ui| {
                for w in WAVES {
                    if ui.selectable_label(live_wave == Some(w), wave_name(w)).clicked() {
                        pick = Some(w);
                    }
                }
            });
            if let Some(w) = pick {
                match (&layer.generator, param(app, i, params::L_WAVE)) {
                    // a basic oscillator switches its wave live (one undo step)
                    (Generator::Basic { .. }, Some((pid, ..))) => {
                        let d = desc(pid).expect("wave parameter");
                        edits.push((ParamId(pid), d.to_normalized(w.index() as f64), true, true));
                    }
                    // anything else becomes a basic oscillator with that wave
                    _ => {
                        let mut p = patch.clone();
                        p.layers[i].generator = Generator::Basic { wave: w, pulse_width: 0.5 };
                        structural = Some(p);
                    }
                }
            }
            // octave (whole octaves of the coarse tuning; the parameter counts from −48 semitones)
            let coarse = param(app, i, params::L_COARSE).map_or(layer.coarse_semitones, |p| p.2.round() as i32 - 48);
            ui.horizontal(|ui| {
                let mut step = 0;
                if ui.add_enabled(coarse - 12 >= -48, egui::Button::new("−").small()).on_hover_text("One octave down").clicked() {
                    step = -12;
                }
                let rest = coarse - 12 * (coarse / 12);
                let text = if rest == 0 { format!("{:+}", coarse / 12) } else { format!("{:+} {rest:+}st", coarse / 12) };
                ui.label(egui::RichText::new(text.replace("+0", "0")).monospace());
                if ui.add_enabled(coarse + 12 <= 48, egui::Button::new("+").small()).on_hover_text("One octave up").clicked() {
                    step = 12;
                }
                if step != 0
                    && let Some((pid, ..)) = param(app, i, params::L_COARSE)
                {
                    let d = desc(pid).expect("coarse parameter");
                    edits.push((ParamId(pid), d.to_normalized(f64::from(coarse + step + 48)), true, true));
                }
            });
            // detune in cents
            ui.horizontal(|ui| {
                let cents = param(app, i, params::L_FINE).map_or(layer.fine_cents, |p| p.2 as f32);
                ui.spacing_mut().slider_width = 90.0;
                let pid = params::layer_param(i as u32, params::L_FINE);
                let d = desc(pid);
                unit_slider(app, ui, instance_id, &format!("OSC {} Detune", i + 1), pid, cents, -100.0..=100.0, |c| d.map_or(0.5, |d| d.to_normalized(f64::from(c))), edits)
                    .on_hover_text("Fine detune in cents (right-click: reset, automation, MIDI learn)");
                ui.label(egui::RichText::new(format!("{:+.0} cents{}", cents, param_badges(app, instance_id, pid))).monospace());
            });
            // level in percent
            ui.horizontal(|ui| {
                let db = param(app, i, params::L_LEVEL).map_or(layer.level_db, |p| p.2 as f32);
                let pct = level_percent(db);
                // up to 100 %, or to the level it has, so a louder layer keeps it when touched;
                // the range stays as it was until the button is up (no jump mid-drag or on release)
                let range_id = egui::Id::unique(("osc-level-range", instance_id, i));
                let top = match ui.ctx().data(|d| d.get_temp::<f32>(range_id)) {
                    Some(t) if ui.input(|inp| inp.pointer.any_down() || inp.pointer.any_released()) => t,
                    _ => pct.max(100.0),
                };
                ui.ctx().data_mut(|d| d.insert_temp(range_id, top));
                ui.spacing_mut().slider_width = 90.0;
                let pid = params::layer_param(i as u32, params::L_LEVEL);
                let d = desc(pid);
                unit_slider(app, ui, instance_id, &format!("OSC {} Level", i + 1), pid, pct, 0.0..=top, |p| d.map_or(1.0, |d| d.to_normalized(f64::from(percent_db(p)))), edits);
                ui.label(egui::RichText::new(format!("{pct:.0}%{}", param_badges(app, instance_id, pid))).monospace());
            });
            if patch.layers.len() > 1 && ui.small_button("×").on_hover_text("Remove this oscillator").clicked() {
                let mut p = patch.clone();
                p.layers.remove(i);
                structural = Some(p);
            } else if patch.layers.len() <= 1 {
                ui.label("");
            }
            ui.end_row();
        }
    });
    if patch.layers.len() < MAX_OSCILLATORS && ui.button("+ Oscillator").on_hover_text("Another oscillator: a copy of the last one, an octave higher (up to three)").clicked() {
        let mut p = patch.clone();
        if let Some(last) = p.layers.last().cloned() {
            p.layers.push(Layer { coarse_semitones: (last.coarse_semitones + 12).min(48), ..last });
        }
        structural = Some(p);
    }
    if let Some(p) = structural {
        commit_patch(app, instance_id, p);
    }
}

/// The amplitude envelope's editor id (tests drag its points).
pub(crate) fn envelope_id(instance_id: InstanceId) -> egui::Id {
    egui::Id::unique(("amp-envelope", instance_id))
}

const ENVELOPE: [u32; 4] = [params::AMP_ATTACK, params::AMP_DECAY, params::AMP_SUSTAIN, params::AMP_RELEASE];

/// Envelope readouts: times in ms (s from one second), sustain in percent.
fn envelope_text(pid: u32, physical: f64) -> String {
    if pid == params::AMP_SUSTAIN {
        format!("{:.0}%", physical * 100.0)
    } else if physical < 1.0 {
        format!("{:.0} ms", physical.max(0.0) * 1000.0)
    } else {
        format!("{physical:.2} s")
    }
}

/// The amplitude envelope: drag the curve's points, or use the sliders under it; the values
/// follow either way while dragging, and each drag is one undo step.
fn envelope_section(app: &mut App, ui: &mut egui::Ui, instance_id: InstanceId, edits: &mut Vec<(ParamId, f32, bool, bool)>) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Amplitude envelope").strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(egui::RichText::new("ADSR").color(theme::TEXT_SECONDARY)));
    });
    let norm = |app: &App, pid: u32| desc(pid).and_then(|d| app.synth_mirrors[&instance_id].param(d.id)).unwrap_or(0.0);
    let phys = |app: &App, pid: u32| desc(pid).map_or(0.0, |d| d.to_physical(norm(app, pid)) as f32);
    let v = rawdawg_ui::envelope::Adsr {
        attack_s: phys(app, params::AMP_ATTACK),
        decay_s: phys(app, params::AMP_DECAY),
        sustain: phys(app, params::AMP_SUSTAIN),
        release_s: phys(app, params::AMP_RELEASE),
    };
    if let Some(drag) = rawdawg_ui::envelope::editor(ui, envelope_id(instance_id), &v, 150.0) {
        use rawdawg_ui::envelope::AdsrPoint;
        let w = drag.value;
        let moves: Vec<(u32, f32)> = match drag.point {
            AdsrPoint::Attack => vec![(params::AMP_ATTACK, w.attack_s)],
            AdsrPoint::Decay => vec![(params::AMP_DECAY, w.decay_s), (params::AMP_SUSTAIN, w.sustain)],
            AdsrPoint::Sustain => vec![(params::AMP_SUSTAIN, w.sustain)],
            AdsrPoint::Release => vec![(params::AMP_RELEASE, w.release_s)],
        };
        let new: Vec<(ParamId, f32)> = moves.iter().filter_map(|(pid, phys)| desc(*pid).map(|d| (d.id, d.to_normalized(f64::from(*phys)).clamp(0.0, 1.0)))).collect();
        // every value first, then the ends: the first end commits the whole drag
        edits.extend(new.iter().map(|(id, n)| (*id, *n, drag.started, false)));
        if drag.stopped {
            edits.extend(new.iter().map(|(id, n)| (*id, *n, false, true)));
        }
    }
    for (row, pair) in ENVELOPE.chunks(2).enumerate() {
        // each row of columns gets its own id, or the two rows' sliders would share theirs
        ui.push_id(("envelope-row", row), |ui| {
            ui.columns(2, |cols| {
                for (ui, &pid) in cols.iter_mut().zip(pair) {
                    let Some(d) = desc(pid) else { continue };
                    let n = norm(app, pid);
                    ui.horizontal(|ui| {
                        ui.label(d.name);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add_space(12.0);
                            ui.label(egui::RichText::new(format!("{}{}", envelope_text(pid, d.to_physical(n)), param_badges(app, instance_id, pid))).monospace())
                        });
                    });
                    ui.spacing_mut().slider_width = (ui.available_width() - 20.0).max(60.0);
                    unit_slider(app, ui, instance_id, d.name, pid, n, 0.0..=1.0, |x| x, edits);
                }
            })
        });
    }
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
                            let r = ui.add(rawdawg_ui::params::slider(ui, &mut idx, 0..=(steps as i64 - 1)).show_value(true));
                            n = d.to_normalized(idx as f64);
                            r
                        }
                    }
                }
                _ => ui.add(rawdawg_ui::params::slider(ui, &mut n, 0.0..=1.0).show_value(false)),
            };
            let resp = rawdawg_ui::params::context_menu(&resp, |ui| param_menu(app, ui, instance_id, d, edits)).map(|r| r.response).unwrap_or(resp);
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

/// The right-click menu of a synth parameter: randomize lock, reset, MIDI learn, automation.
fn param_menu(app: &mut App, ui: &mut egui::Ui, instance_id: InstanceId, d: &ParamDescriptor, edits: &mut Vec<(ParamId, f32, bool, bool)>) {
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
    if ui.button("Automate in this pattern").on_hover_text("A curve inside the selected pattern: it plays whenever the pattern plays, in Pattern mode and in Song mode").clicked() {
        super::automation::create_pattern_curve_for(app, instance_id, d.id.0);
        ui.close();
    }
}

pub(crate) fn apply_edits(app: &mut App, instance_id: InstanceId, edits: Vec<(ParamId, f32, bool, bool)>) {
    for (id, n, started, stopped) in edits {
        super::automation::capture_edit(app, instance_id, id.0, n, started, stopped);
        // a gesture still open on another synth (its release was never seen) is committed first,
        // or this one could not start and its edit would never be saved
        if started && app.synth_panel.gesture.as_ref().is_some_and(|(i, _)| *i != instance_id) {
            finish_gesture(app);
        }
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
            finish_gesture(app);
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
            let resp = ui.add(rawdawg_ui::params::slider(ui, &mut route.depth, -1.0..=1.0).fixed_decimals(2));
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
            if ui.small_button("×").on_hover_text("Remove route").clicked() {
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
/// Commit the open parameter gesture as one undo step, for the synth it belongs to.
fn finish_gesture(app: &mut App) {
    let Some((gid, before)) = app.synth_panel.gesture.take() else { return };
    let Some(after) = app.synth_mirrors.get(&gid).map(|m| m.save_state()) else { return };
    if after == before {
        return;
    }
    let state: serde_json::Value = serde_json::from_slice(&after).unwrap_or(serde_json::Value::Null);
    app.mark_live_state(gid, rawdawg_synthesizer::TYPE_ID, &state);
    if let Err(e) = app.history.apply(&mut app.project, Command::SetInstanceState { instance_id: gid, state_version: 1, state }) {
        app.status_line = format!("Edit rejected: {e}");
    }
}

/// A gesture whose release no control saw (its panel was hidden mid-drag) is committed once no
/// pointer button is down, so it neither lingers nor swallows later edits.
pub(crate) fn finish_abandoned(app: &mut App) {
    if app.synth_panel.gesture.is_some() {
        finish_gesture(app);
    }
}

/// Replace the whole patch (structure change, preset load, reset): undoable,
/// rebuilds the mirror and the plan (the live instance is replaced).
pub(crate) fn commit_patch(app: &mut App, instance_id: InstanceId, patch: rawdawg_synthesizer::Patch) {
    if let Err(e) = patch.validate() {
        app.status_line = format!("Patch rejected: {e}");
        return;
    }
    // the committed sound plays at its own level (an Evolve level match was for its card only)
    app.synth_panel.evolve.preview_trim_db = None;
    let value: serde_json::Value = serde_json::to_value(&patch).unwrap_or(serde_json::Value::Null);
    // the panel's controls show (and their edits start from) the mirror: it must hold the new
    // sound too, or the next touch writes the old values back (Randomize did that)
    if let Some(m) = app.synth_mirrors.get_mut(&instance_id) {
        let _ = m.set_patch(patch);
    }
    app.apply(Command::SetInstanceState { instance_id, state_version: 1, state: value });
}

//! Waveform canvas (SYN-02/SYN-06): one cycle of the selected layer's stored
//! waveform, editable with brushes while notes play; spectrum paint mode; live
//! output scope. Every stroke is one undo entry; audio updates via table handoff.

use super::App;
use crate::theme;
use rawdawg_dsp::processor::Processor;
use rawdawg_dsp::wavetable::{CYCLE_LEN, MipTable, TableBuilder, cycles};
use rawdawg_project::Command;
use rawdawg_synthesizer::patch::{Generator, MAX_FRAMES};
use rawdawg_synthesizer::sculpt::{self, Brush, BrushSettings, StrokePoint};
use rawdawg_types::{InstanceId, ParamId};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum CanvasMode {
    #[default]
    Waveform,
    Spectrum,
    Harmonics,
}

/// The harmonic editor (SYN-02): the wave as harmonics 1..=count with a level each.
pub(crate) struct HarmonicsState {
    pub count: usize,
    /// Level (0..1, relative) and phase of harmonics 1..=MAX_HARMONICS.
    pub(crate) levels: Vec<f32>,
    phases: Vec<f32>,
    /// What the levels describe: instance, layer, frame and a fingerprint of that cycle. When
    /// the cycle changes elsewhere (undo, the brushes, a preset) they are read from it again.
    source: Option<(InstanceId, usize, usize, u64)>,
    /// Pre-edit state of the instance being painted (one undo step per press).
    before: Option<(InstanceId, Vec<u8>)>,
    /// Last bar painted in this press (bars between two frames are filled in).
    last: Option<(usize, f32)>,
    seed: u64,
}

impl Default for HarmonicsState {
    fn default() -> Self {
        Self { count: 16, levels: Vec::new(), phases: Vec::new(), source: None, before: None, last: None, seed: super::evolve::launch_entropy() }
    }
}

pub(crate) struct WaveformState {
    pub mode: CanvasMode,
    pub settings: BrushSettings,
    pub layer: usize,
    /// Motion frame being edited (wavetable layers).
    pub frame: usize,
    pub spectrum_db: bool,
    pub harmonics: HarmonicsState,
    builder: TableBuilder,
    /// Active stroke: instance, pre-stroke state bytes, last pointer sample, dab counter.
    stroke: Option<Stroke>,
    /// A table built but not yet accepted by the queue (coalesced retry): instance, layer, frame.
    pending_table: Option<(InstanceId, usize, usize, Box<MipTable>)>,
    scope: Vec<f32>,
    seed: u64,
    pub status_tip: String,
}

struct Stroke {
    instance: InstanceId,
    before: Vec<u8>,
    last: StrokePoint,
    dabs: u64,
    cycle: Vec<f32>,
    dirty: bool,
}

impl WaveformState {
    pub(crate) fn builder_mut(&mut self) -> &mut TableBuilder {
        &mut self.builder
    }
}

impl Default for WaveformState {
    fn default() -> Self {
        Self {
            mode: CanvasMode::Waveform,
            settings: BrushSettings::default(),
            layer: 0,
            frame: 0,
            spectrum_db: false,
            harmonics: HarmonicsState::default(),
            builder: TableBuilder::new(),
            stroke: None,
            pending_table: None,
            scope: vec![0.0; 1024],
            seed: 0x5CA1_9E57,
            status_tip: Brush::Draw.tip().to_owned(),
        }
    }
}

/// Cycle of a basic wave, used when a basic layer is first sculpted.
fn basic_cycle(wave: rawdawg_dsp::oscillator::BasicWave) -> Vec<f32> {
    use rawdawg_dsp::oscillator::BasicWave::*;
    match wave {
        Sine => cycles::sine(),
        Triangle => cycles::triangle(),
        Saw => cycles::saw(),
        Pulse => cycles::square(),
    }
}

/// Harmonic-mode additive cycle for display.
pub(crate) fn additive_cycle(a: &rawdawg_synthesizer::patch::AdditiveSettings) -> Vec<f32> {
    let mut mag = vec![0.0f32];
    mag.extend_from_slice(&a.amplitudes);
    mag.resize(CYCLE_LEN / 2 + 1, 0.0);
    let mut ph = vec![0.0f32];
    ph.extend_from_slice(&a.phases);
    ph.resize(CYCLE_LEN / 2 + 1, 0.0);
    sculpt::synthesize(&sculpt::Harmonics { magnitude: mag, phase: ph })
}

/// Current editable cycle of the layer (or the basic wave it would become).
fn layer_cycle(app: &App, instance: InstanceId, layer: usize) -> Option<(Vec<f32>, bool)> {
    let patch = app.synth_mirrors.get(&instance)?.patch();
    let l = patch.layers.get(layer)?;
    let frame = app.synth_panel.waveform.frame;
    Some(match &l.generator {
        Generator::Wavetable { cycle, frames, .. } => (frames.get(frame.min(frames.len().saturating_sub(1))).cloned().unwrap_or_else(|| cycle.clone()), true),
        Generator::Basic { wave, .. } => (basic_cycle(*wave), false),
        Generator::Granular { source, .. } => (source.clone(), true),
        Generator::Additive(a) => (crate::ui::waveform::additive_cycle(a), false),
        Generator::Fm(_) | Generator::Pluck(_) => (cycles::sine(), false),
    })
}

/// Push a cycle into the live synth (table handoff) and into the mirror patch.
fn apply_cycle_live(app: &mut App, instance: InstanceId, layer: usize, cycle: &[f32]) {
    let table = Box::new(app.synth_panel.waveform.builder.build(cycle, true));
    let mut frame = app.synth_panel.waveform.frame;
    if let Some(m) = app.synth_mirrors.get_mut(&instance) {
        let mut patch = m.patch().clone();
        if let Some(l) = patch.layers.get_mut(layer) {
            match &mut l.generator {
                Generator::Wavetable { cycle: c, frames, .. } => {
                    frame = frame.min(frames.len().saturating_sub(1));
                    if let Some(f) = frames.get_mut(frame) {
                        *f = cycle.to_vec();
                    } else {
                        *c = cycle.to_vec();
                    }
                }
                Generator::Granular { source, .. } => *source = cycle.to_vec(),
                _ => l.generator = Generator::Wavetable { cycle: cycle.to_vec(), remove_dc: true, frames: vec![], position: 0.0 },
            }
            let _ = m.set_patch(patch);
        }
    }
    send_table(app, instance, layer, frame, table);
}

fn send_table(app: &mut App, instance: InstanceId, layer: usize, frame: usize, table: Box<MipTable>) {
    // keep only the newest pending table (coalescing)
    let table = match app.table_senders.get_mut(&instance) {
        Some(s) => s.send(layer, frame, table).err(),
        None => None,
    };
    app.synth_panel.waveform.pending_table = table.map(|t| (instance, layer, frame, t));
}

/// Retry a pending table send (called every frame).
pub(crate) fn flush_pending(app: &mut App) {
    if let Some((instance, layer, frame, table)) = app.synth_panel.waveform.pending_table.take() {
        send_table(app, instance, layer, frame, table);
    }
}

/// Motion-frame strip for wavetable layers: select, add (duplicate), remove, reorder
/// frames and scrub the playback position (SYN-06). Structure changes are undoable
/// patch commits (voices carry over); the position is a live parameter gesture.
fn frame_strip(app: &mut App, ui: &mut egui::Ui, instance: InstanceId, layer: usize) {
    let Some(patch) = app.synth_mirrors.get(&instance).map(|m| m.patch().clone()) else { return };
    let Some(Generator::Wavetable { cycle, frames, position, .. }) = patch.layers.get(layer).map(|l| &l.generator) else { return };
    let count = frames.len().max(1);
    let st = &mut app.synth_panel.waveform;
    st.frame = st.frame.min(count - 1);
    let frame = st.frame;
    let mut new_patch: Option<rawdawg_synthesizer::Patch> = None;
    let mut edits: Vec<(ParamId, f32, bool, bool)> = Vec::new();
    ui.horizontal(|ui| {
        ui.label("Frames").on_hover_text("Motion frames: the wavetable position travels between them (modulate it with an LFO or envelope)");
        for i in 0..count {
            if ui.selectable_label(frame == i, format!("{}", i + 1)).clicked() {
                app.synth_panel.waveform.frame = i;
            }
        }
        if count < MAX_FRAMES && ui.button("+").on_hover_text("Add a frame after this one (copy of it)").clicked() {
            let mut p = patch.clone();
            if let Some(Generator::Wavetable { cycle, frames, .. }) = p.layers.get_mut(layer).map(|l| &mut l.generator) {
                if frames.is_empty() {
                    frames.push(cycle.clone());
                }
                let copy = frames[frame.min(frames.len() - 1)].clone();
                frames.insert(frame + 1, copy);
            }
            app.synth_panel.waveform.frame = frame + 1;
            new_patch = Some(p);
        }
        if count > 1 && ui.button("−").on_hover_text("Remove this frame").clicked() {
            let mut p = patch.clone();
            if let Some(Generator::Wavetable { cycle, frames, .. }) = p.layers.get_mut(layer).map(|l| &mut l.generator) {
                frames.remove(frame);
                if frames.len() == 1 {
                    *cycle = frames.remove(0);
                }
            }
            app.synth_panel.waveform.frame = frame.saturating_sub(1);
            new_patch = Some(p);
        }
        if count > 1 && frame > 0 && ui.button("<").on_hover_text("Move this frame earlier").clicked() {
            let mut p = patch.clone();
            if let Some(Generator::Wavetable { frames, .. }) = p.layers.get_mut(layer).map(|l| &mut l.generator) {
                frames.swap(frame, frame - 1);
            }
            app.synth_panel.waveform.frame = frame - 1;
            new_patch = Some(p);
        }
        if count > 1 && frame + 1 < count && ui.button(">").on_hover_text("Move this frame later").clicked() {
            let mut p = patch.clone();
            if let Some(Generator::Wavetable { frames, .. }) = p.layers.get_mut(layer).map(|l| &mut l.generator) {
                frames.swap(frame, frame + 1);
            }
            app.synth_panel.waveform.frame = frame + 1;
            new_patch = Some(p);
        }
        if count > 1 {
            ui.separator();
            let id = ParamId(rawdawg_synthesizer::params::layer_param(layer as u32, rawdawg_synthesizer::params::L_WT_POSITION));
            let current = app.synth_mirrors[&instance].param(id).unwrap_or(*position);
            let mut n = current;
            let resp = ui
                .add(rawdawg_ui::params::slider(ui, &mut n, 0.0..=1.0).text("Position").show_value(false))
                .on_hover_text("Where playback sits between the frames (live; drag to scrub)");
            let playing = n * (count - 1) as f32;
            ui.label(egui::RichText::new(format!("{:.2} → frame {:.1}", n, playing + 1.0)).monospace().color(theme::TEXT_SECONDARY));
            if resp.changed() && (n - current).abs() > 1e-7 {
                edits.push((id, n, resp.drag_started() || !resp.dragged(), !resp.dragged()));
            } else if resp.drag_stopped() {
                edits.push((id, n, false, true));
            }
        } else {
            let _ = cycle;
            ui.label(egui::RichText::new("one frame — press + to add motion").color(theme::TEXT_SECONDARY));
        }
    });
    if !edits.is_empty() {
        super::synth_panel::apply_edits(app, instance, edits);
    }
    if let Some(p) = new_patch {
        super::synth_panel::commit_patch(app, instance, p);
    }
}

/// The generator kind of every layer of a saved synth state.
fn layer_kinds(state: &[u8]) -> Option<Vec<usize>> {
    serde_json::from_slice::<rawdawg_synthesizer::Patch>(state).ok().map(|p| p.layers.iter().map(|l| l.generator.kind_index()).collect())
}

/// Commit the current mirror patch as one undo entry. The running instance is kept when it
/// already plays the result (its tables were handed over live); a layer that changed kind (a
/// basic oscillator turned into a drawn wave) cannot have been: its voices threw the tables
/// away, so the instance is rebuilt from the committed state.
fn commit(app: &mut App, instance: InstanceId, before: &[u8]) {
    let Some(after) = app.synth_mirrors.get(&instance).map(|m| m.save_state()) else { return };
    if after == before {
        return;
    }
    let restructured = layer_kinds(before) != layer_kinds(&after);
    let state: serde_json::Value = serde_json::from_slice(&after).unwrap_or(serde_json::Value::Null);
    if !restructured {
        app.mark_live_state(instance, rawdawg_synthesizer::TYPE_ID, &state);
    }
    match app.history.apply(&mut app.project, Command::SetInstanceState { instance_id: instance, state_version: 1, state }) {
        Err(e) => app.status_line = format!("Edit rejected: {e}"),
        Ok(()) if restructured => app.recompile(),
        Ok(()) => {}
    }
}

/// A brush stroke or harmonic press whose release this editor did not see (it was hidden
/// mid-drag) is committed once no pointer button is down.
pub(crate) fn finish_abandoned(app: &mut App) {
    if let Some(stroke) = app.synth_panel.waveform.stroke.take()
        && stroke.dirty
    {
        commit(app, stroke.instance, &stroke.before);
    }
    if let Some((instance, before)) = app.synth_panel.waveform.harmonics.before.take() {
        app.synth_panel.waveform.harmonics.last = None;
        commit(app, instance, &before);
    }
}

pub(crate) fn show(app: &mut App, ui: &mut egui::Ui, instance: InstanceId) {
    let layer_count = app.synth_mirrors.get(&instance).map(|m| m.patch().layers.len()).unwrap_or(0);
    if layer_count == 0 {
        return;
    }
    let ws_layer = app.synth_panel.waveform.layer.min(layer_count - 1);
    app.synth_panel.waveform.layer = ws_layer;
    ui.horizontal(|ui| {
        let mode = &mut app.synth_panel.waveform.mode;
        ui.selectable_value(mode, CanvasMode::Waveform, "Waveform");
        ui.selectable_value(mode, CanvasMode::Spectrum, "Spectrum");
        ui.selectable_value(mode, CanvasMode::Harmonics, "Harmonics").on_hover_text("Build the wave from harmonics: a level for each, classic shapes, random");
        ui.separator();
        for i in 0..layer_count {
            ui.selectable_value(&mut app.synth_panel.waveform.layer, i, format!("Layer {}", i + 1));
        }
        ui.separator();
        if ui.button("Reset wave").on_hover_text("Back to the default soft saw (undoable)").clicked() {
            let before = app.synth_mirrors[&instance].save_state();
            apply_cycle_live(app, instance, ws_layer, &cycles::soft_saw());
            commit(app, instance, &before);
        }
        let basic = matches!(layer_cycle(app, instance, ws_layer), Some((_, false)));
        if basic {
            ui.label(egui::RichText::new("Editing turns this layer into a wavetable").color(theme::WARNING));
        }
    });
    frame_strip(app, ui, instance, ws_layer);
    match app.synth_panel.waveform.mode {
        CanvasMode::Waveform => waveform_canvas(app, ui, instance, ws_layer),
        CanvasMode::Spectrum => spectrum_canvas(app, ui, instance, ws_layer),
        CanvasMode::Harmonics => harmonics_editor(app, ui, instance, ws_layer),
    }
    scope(app, ui);
}

fn waveform_canvas(app: &mut App, ui: &mut egui::Ui, instance: InstanceId, layer: usize) {
    ui.horizontal(|ui| {
        for b in Brush::ALL {
            let selected = app.synth_panel.waveform.settings.brush == b;
            if ui.selectable_label(selected, b.label()).on_hover_text(b.tip()).clicked() {
                app.synth_panel.waveform.settings.brush = b;
                app.synth_panel.waveform.status_tip = b.tip().to_owned();
            }
        }
        ui.separator();
        ui.add(rawdawg_ui::params::slider(ui, &mut app.synth_panel.waveform.settings.radius, 0.01..=0.5).text("Size").logarithmic(true).show_value(false))
            .on_hover_text("Brush size (mouse wheel over the wave)");
        ui.add(rawdawg_ui::params::slider(ui, &mut app.synth_panel.waveform.settings.strength, 0.0..=1.0).text("Strength").show_value(false));
    });
    let Some((cycle, _)) = layer_cycle(app, instance, layer) else { return };
    let height = 180.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width().max(200.0), height), egui::Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, theme::BACKGROUND);
    let mid = rect.center().y;
    painter.line_segment([egui::pos2(rect.min.x, mid), egui::pos2(rect.max.x, mid)], egui::Stroke::new(1.0, theme::BORDER));
    // the cycle being edited (stroke in progress shows the working copy)
    let shown: &[f32] = match &app.synth_panel.waveform.stroke {
        Some(s) if s.instance == instance => &s.cycle,
        _ => &cycle,
    };
    let to_pos = |i: usize, v: f32| egui::pos2(rect.min.x + rect.width() * i as f32 / CYCLE_LEN as f32, mid - v.clamp(-1.5, 1.5) * rect.height() * 0.45);
    let step = (CYCLE_LEN / (rect.width() as usize).max(1)).max(1);
    // the other motion frames, faint, for context
    if let Some(Generator::Wavetable { frames, .. }) = app.synth_mirrors.get(&instance).and_then(|m| m.patch().layers.get(layer)).map(|l| &l.generator) {
        let cur = app.synth_panel.waveform.frame;
        for (fi, f) in frames.iter().enumerate() {
            if fi == cur || f.len() != CYCLE_LEN {
                continue;
            }
            let pts: Vec<egui::Pos2> = (0..CYCLE_LEN).step_by(step).chain(std::iter::once(CYCLE_LEN - 1)).map(|i| to_pos(i, f[i])).collect();
            painter.add(egui::Shape::line(pts, egui::Stroke::new(1.0, theme::TEXT_SECONDARY.linear_multiply(0.25))));
        }
    }
    let pts: Vec<egui::Pos2> = (0..CYCLE_LEN).step_by(step).chain(std::iter::once(CYCLE_LEN - 1)).map(|i| to_pos(i, shown[i])).collect();
    painter.add(egui::Shape::line(pts, egui::Stroke::new(1.5, theme::ACCENT)));
    // brush cursor
    if let Some(p) = response.hover_pos() {
        let r = app.synth_panel.waveform.settings.clamped().radius * rect.width();
        painter.rect_filled(egui::Rect::from_min_max(egui::pos2(p.x - r, rect.min.y), egui::pos2(p.x + r, rect.max.y)), 0.0, theme::ACCENT.linear_multiply(0.12));
        painter.line_segment([egui::pos2(p.x, rect.min.y), egui::pos2(p.x, rect.max.y)], egui::Stroke::new(1.0, theme::ACCENT.linear_multiply(0.5)));
    }
    // wheel changes radius when the canvas is hovered
    if response.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            let s = &mut app.synth_panel.waveform.settings;
            s.radius = (s.radius * (1.0 + scroll.signum() * 0.15)).clamp(0.01, 0.5);
        }
    }
    let point_at = |pos: egui::Pos2, last: Option<StrokePoint>| -> StrokePoint {
        let phase = ((pos.x - rect.min.x) / rect.width()).clamp(0.0, 0.9999);
        let value = ((mid - pos.y) / (rect.height() * 0.45)).clamp(-1.0, 1.0);
        match last {
            Some(l) => StrokePoint { phase, value, dphase: phase - l.phase, dvalue: value - l.value },
            None => StrokePoint { phase, value, dphase: 0.0, dvalue: 0.0 },
        }
    };
    let settings = app.synth_panel.waveform.settings;
    let seed = app.synth_panel.waveform.seed;
    if response.drag_started()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let before = app.synth_mirrors[&instance].save_state();
        let p = point_at(pos, None);
        let mut work = cycle.clone();
        let mut dabs = 0u64;
        sculpt::apply_dab(&mut work, &settings, p, seed, dabs);
        dabs += 1;
        app.synth_panel.waveform.stroke = Some(Stroke { instance, before, last: p, dabs, cycle: work.clone(), dirty: true });
        apply_cycle_live(app, instance, layer, &work);
    } else if response.dragged()
        && let Some(pos) = response.interact_pointer_pos()
        && let Some(stroke) = app.synth_panel.waveform.stroke.as_mut()
        && stroke.instance == instance
    {
        let p = point_at(pos, Some(stroke.last));
        if p.phase != stroke.last.phase || p.value != stroke.last.value {
            let mut work = std::mem::take(&mut stroke.cycle);
            sculpt::apply_stroke(&mut work, &settings, stroke.last, p, seed, &mut stroke.dabs);
            stroke.last = p;
            stroke.cycle = work.clone();
            stroke.dirty = true;
            apply_cycle_live(app, instance, layer, &work);
        }
    }
    if response.drag_stopped()
        && let Some(stroke) = app.synth_panel.waveform.stroke.take()
        && stroke.dirty
    {
        app.synth_panel.waveform.seed = app.synth_panel.waveform.seed.wrapping_add(1);
        commit(app, instance, &stroke.before);
    }
    ui.label(egui::RichText::new(app.synth_panel.waveform.status_tip.clone()).color(theme::TEXT_SECONDARY));
}

const SPECTRUM_BARS: usize = 64;

fn spectrum_canvas(app: &mut App, ui: &mut egui::Ui, instance: InstanceId, layer: usize) {
    ui.horizontal(|ui| {
        ui.label("Drag to paint harmonic levels; right-drag to erase.");
        ui.checkbox(&mut app.synth_panel.waveform.spectrum_db, "dB view");
        ui.add(rawdawg_ui::params::slider(ui, &mut app.synth_panel.waveform.settings.strength, 0.0..=1.0).text("Strength").show_value(false));
    });
    let Some((cycle, _)) = layer_cycle(app, instance, layer) else { return };
    let shown: Vec<f32> = match &app.synth_panel.waveform.stroke {
        Some(s) if s.instance == instance => s.cycle.clone(),
        _ => cycle.clone(),
    };
    let h = sculpt::analyze(&shown);
    let height = 180.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width().max(200.0), height), egui::Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, theme::BACKGROUND);
    let db_view = app.synth_panel.waveform.spectrum_db;
    let bar_w = rect.width() / SPECTRUM_BARS as f32;
    let mag_to_frac = |m: f32| -> f32 { if db_view { ((20.0 * m.max(1e-5).log10() + 60.0) / 60.0).clamp(0.0, 1.0) } else { m.clamp(0.0, 1.0) } };
    let frac_to_mag = |f: f32| -> f32 { if db_view { if f <= 0.0 { 0.0 } else { 10f32.powf((f * 60.0 - 60.0) / 20.0) } } else { f.clamp(0.0, 1.0) } };
    for k in 1..=SPECTRUM_BARS {
        let m = h.magnitude.get(k).copied().unwrap_or(0.0);
        let f = mag_to_frac(m);
        let x = rect.min.x + (k - 1) as f32 * bar_w;
        let bar = egui::Rect::from_min_max(egui::pos2(x + 1.0, rect.max.y - f * rect.height()), egui::pos2(x + bar_w - 1.0, rect.max.y));
        painter.rect_filled(bar, 1.0, if k == 1 { theme::ACCENT } else { theme::ACCENT.linear_multiply(0.7) });
        if k % 8 == 1 {
            painter.text(egui::pos2(x + 2.0, rect.min.y + 2.0), egui::Align2::LEFT_TOP, format!("{k}"), egui::FontId::proportional(9.0), theme::TEXT_SECONDARY);
        }
    }
    let pressed = response.dragged() || response.drag_started();
    let erase = ui.input(|i| i.pointer.secondary_down());
    if pressed && let Some(pos) = response.interact_pointer_pos() {
        let k = (((pos.x - rect.min.x) / bar_w).floor() as usize + 1).clamp(1, SPECTRUM_BARS);
        let frac = ((rect.max.y - pos.y) / rect.height()).clamp(0.0, 1.0);
        let target = if erase { 0.0 } else { frac_to_mag(frac) };
        let strength = app.synth_panel.waveform.settings.strength.max(0.05);
        if app.synth_panel.waveform.stroke.as_ref().is_none_or(|s| s.instance != instance) {
            let before = app.synth_mirrors[&instance].save_state();
            app.synth_panel.waveform.stroke =
                Some(Stroke { instance, before, last: StrokePoint { phase: 0.0, value: 0.0, dphase: 0.0, dvalue: 0.0 }, dabs: 0, cycle: cycle.clone(), dirty: false });
        }
        let current = h.magnitude.get(k).copied().unwrap_or(0.0);
        let new_mag = current + (target - current) * strength;
        if (new_mag - current).abs() > 1e-5 {
            let work = sculpt::set_harmonic(&shown, k, new_mag, true);
            if let Some(s) = app.synth_panel.waveform.stroke.as_mut() {
                s.cycle = work.clone();
                s.dirty = true;
            }
            apply_cycle_live(app, instance, layer, &work);
        }
    }
    if response.drag_stopped()
        && let Some(stroke) = app.synth_panel.waveform.stroke.take()
        && stroke.dirty
    {
        commit(app, instance, &stroke.before);
    }
}

/// A fingerprint of a cycle's samples.
fn fingerprint(cycle: &[f32]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for v in cycle {
        v.to_bits().hash(&mut h);
    }
    h.finish()
}

/// Bars of the harmonic editor and the area under them for their numbers.
pub(crate) const HARMONIC_BARS_H: f32 = 150.0;
const HARMONIC_NUMBERS_H: f32 = 18.0;

/// The harmonic editor's bar area id (tests paint on it).
pub(crate) fn harmonic_bars_id(instance: InstanceId) -> egui::Id {
    egui::Id::unique(("harmonic-bars", instance))
}

/// Rebuild the layer's wave from the first `count` harmonic levels and play it at once.
fn rebuild_harmonics(app: &mut App, instance: InstanceId, layer: usize) {
    let hs = &app.synth_panel.waveform.harmonics;
    let n = hs.count.min(hs.levels.len());
    let cycle = sculpt::build_harmonics(&hs.levels[..n], &hs.phases[..n]);
    apply_cycle_live(app, instance, layer, &cycle);
    let frame = app.synth_panel.waveform.frame;
    app.synth_panel.waveform.harmonics.source = Some((instance, layer, frame, fingerprint(&cycle)));
}

/// One complete change (count, shape, random): rebuilt, played and one undo step.
fn harmonic_change(app: &mut App, instance: InstanceId, layer: usize) {
    let before = app.synth_mirrors[&instance].save_state();
    rebuild_harmonics(app, instance, layer);
    commit(app, instance, &before);
}

fn harmonics_editor(app: &mut App, ui: &mut egui::Ui, instance: InstanceId, layer: usize) {
    use sculpt::{HarmonicShape, MAX_HARMONICS};
    let Some((cycle, _)) = layer_cycle(app, instance, layer) else { return };
    let frame = app.synth_panel.waveform.frame;
    let key = (instance, layer, frame, fingerprint(&cycle));
    {
        let hs = &mut app.synth_panel.waveform.harmonics;
        if hs.source != Some(key) && hs.before.is_none() {
            let (levels, phases) = sculpt::harmonic_levels(&cycle, MAX_HARMONICS);
            hs.levels = levels;
            hs.phases = phases;
            hs.source = Some(key);
        }
        hs.count = hs.count.clamp(1, MAX_HARMONICS);
    }
    // title and count
    let mut count = app.synth_panel.waveform.harmonics.count;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Harmonic editor").strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("+").on_hover_text("One more harmonic").clicked() {
                count += 1;
            }
            ui.add(egui::DragValue::new(&mut count).range(1..=MAX_HARMONICS).suffix(" harmonics"))
                .on_hover_text(format!("How many harmonics to show and edit (1 to {MAX_HARMONICS}; drag or type). The wave changes at your next edit, which keeps only these"));
            if ui.small_button("−").on_hover_text("One harmonic fewer").clicked() {
                count = count.saturating_sub(1);
            }
        });
    });
    // how many bars to show and edit: the wave itself changes at the next edit, so looking at
    // more or fewer bars never rewrites a sound (and a dragged count is not an undo step each)
    let count = count.clamp(1, MAX_HARMONICS);
    app.synth_panel.waveform.harmonics.count = count;
    let above = sculpt::energy_above(&cycle, count);
    if above > 0.001 {
        ui.label(
            egui::RichText::new(format!("This wave also has harmonics above {count} ({:.0}% of its energy): editing here keeps the first {count}.", above * 100.0))
                .color(theme::WARNING),
        );
    }
    // the wave: two cycles
    let (prect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width().max(200.0), 110.0), egui::Sense::hover());
    let painter = ui.painter_at(prect);
    painter.rect_filled(prect, 4.0, theme::BACKGROUND);
    let mid = prect.center().y;
    painter.line_segment([egui::pos2(prect.min.x, mid), egui::pos2(prect.max.x, mid)], egui::Stroke::new(1.0, theme::BORDER));
    let n = prect.width().max(2.0) as usize;
    let pts: Vec<egui::Pos2> = (0..=n)
        .map(|x| {
            let phase = (2.0 * x as f32 / n as f32).fract();
            let i = ((phase * CYCLE_LEN as f32) as usize).min(CYCLE_LEN - 1);
            egui::pos2(prect.min.x + x as f32, mid - cycle[i].clamp(-1.2, 1.2) * prect.height() * 0.4)
        })
        .collect();
    painter.add(egui::Shape::line(pts, egui::Stroke::new(2.0, theme::KEY_HELD)));
    // the bars
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width().max(200.0), HARMONIC_BARS_H + HARMONIC_NUMBERS_H), egui::Sense::hover());
    let bars = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), HARMONIC_BARS_H));
    let response = ui.interact(bars, harmonic_bars_id(instance), egui::Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let slot = bars.width() / count as f32;
    let levels = app.synth_panel.waveform.harmonics.levels.clone();
    for k in 0..count {
        let x = bars.min.x + k as f32 * slot;
        let w = (slot * 0.72).max(2.0);
        let track = egui::Rect::from_min_size(egui::pos2(x + (slot - w) * 0.5, bars.min.y), egui::vec2(w, bars.height()));
        painter.rect_filled(track, 3.0, theme::RAISED);
        let level = levels.get(k).copied().unwrap_or(0.0);
        if level > 0.0005 {
            let fill = egui::Rect::from_min_max(egui::pos2(track.min.x, track.max.y - level.clamp(0.0, 1.0) * track.height()), track.max);
            painter.rect_filled(fill, 3.0, theme::KEY_HELD);
        }
        if slot >= 14.0 || k == 0 || (k + 1) % 4 == 0 {
            painter.text(egui::pos2(track.center().x, bars.max.y + 3.0), egui::Align2::CENTER_TOP, format!("{}", k + 1), egui::FontId::proportional(11.0), theme::TEXT_SECONDARY);
        }
    }
    let bar_at = |pos: egui::Pos2| -> (usize, f32) {
        let k = (((pos.x - bars.min.x) / slot).floor().max(0.0) as usize).min(count - 1);
        (k, ((bars.max.y - pos.y) / bars.height()).clamp(0.0, 1.0))
    };
    // press, click or drag across: the bar under the pointer takes its height (right button: off)
    if response.is_pointer_button_down_on()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let erase = ui.input(|i| i.pointer.secondary_down());
        let (k, mut level) = bar_at(pos);
        if erase {
            level = 0.0;
        }
        let hs = &mut app.synth_panel.waveform.harmonics;
        if hs.before.as_ref().is_none_or(|(i, _)| *i != instance) {
            hs.before = Some((instance, app.synth_mirrors[&instance].save_state()));
            hs.last = None;
        }
        // fill the bars between this frame and the last one
        let (from, from_level) = hs.last.unwrap_or((k, level));
        let (lo, hi) = (from.min(k), from.max(k));
        let mut changed = false;
        for j in lo..=hi {
            let t = if hi == lo {
                1.0
            } else if from <= k {
                (j - from) as f32 / (k - from) as f32
            } else {
                (from - j) as f32 / (from - k) as f32
            };
            let v = from_level + (level - from_level) * t;
            if let Some(l) = hs.levels.get_mut(j)
                && (*l - v).abs() > 1e-4
            {
                // a harmonic raised from silence starts as a sine (its old phase was noise)
                if *l < 1e-4
                    && let Some(p) = hs.phases.get_mut(j)
                {
                    *p = -std::f32::consts::FRAC_PI_2;
                }
                *l = v;
                changed = true;
            }
        }
        hs.last = Some((k, level));
        if changed {
            rebuild_harmonics(app, instance, layer);
        }
    } else if let Some((i, before)) = app.synth_panel.waveform.harmonics.before.take() {
        // the press ended: one undo step, for the synth it was made on
        app.synth_panel.waveform.harmonics.last = None;
        commit(app, i, &before);
    }
    // readout of the bar under the pointer
    let readout = response.hover_pos().map(|p| bar_at(p).0).map_or_else(
        || "Click or drag over the bars to set each harmonic's level; right-drag turns them off.".to_owned(),
        |k| {
            let l = levels.get(k).copied().unwrap_or(0.0);
            let db = if l > 0.0 { format!("{:.1} dB", 20.0 * l.log10()) } else { "off".to_owned() };
            format!("Harmonic {}: {:.0}% ({db})", k + 1, l * 100.0)
        },
    );
    ui.label(egui::RichText::new(readout).color(theme::TEXT_SECONDARY));
    // shapes and random
    let mut shape = None;
    ui.columns(HarmonicShape::ALL.len(), |cols| {
        for (ui, s) in cols.iter_mut().zip(HarmonicShape::ALL) {
            if ui
                .add_sized(egui::vec2(ui.available_width(), 24.0), egui::Button::new(s.label()))
                .on_hover_text(format!("{} built from the first {count} harmonics", s.label()))
                .clicked()
            {
                shape = Some(s);
            }
        }
    });
    let random = ui
        .add_sized(egui::vec2(ui.available_width(), 26.0), egui::Button::new("Randomize harmonics"))
        .on_hover_text(format!("Random harmonics among the first {count}, each at a random level (the fundamental always stays); Undo goes back"))
        .clicked();
    let picked = if let Some(s) = shape {
        Some(s.harmonics(count))
    } else if random {
        let hs = &mut app.synth_panel.waveform.harmonics;
        hs.seed = hs.seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        Some(sculpt::random_harmonics(count, hs.seed))
    } else {
        None
    };
    if let Some((levels, phases)) = picked {
        let hs = &mut app.synth_panel.waveform.harmonics;
        hs.levels = vec![0.0; MAX_HARMONICS];
        hs.phases = vec![-std::f32::consts::FRAC_PI_2; MAX_HARMONICS];
        hs.levels[..levels.len()].copy_from_slice(&levels);
        hs.phases[..phases.len()].copy_from_slice(&phases);
        harmonic_change(app, instance, layer);
        app.status_line = match shape {
            Some(s) => format!("{} from {count} harmonics (Undo goes back)", s.label()),
            None => format!("Random harmonics among the first {count} (Undo goes back)"),
        };
    }
}

fn scope(app: &mut App, ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width().max(200.0), 48.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, theme::BACKGROUND);
    painter.text(rect.min + egui::vec2(4.0, 2.0), egui::Align2::LEFT_TOP, "Live output", egui::FontId::proportional(10.0), theme::TEXT_SECONDARY);
    if let super::AudioState::Running(sys) = &app.audio {
        let buf = &mut app.synth_panel.waveform.scope;
        sys.handle.status.scope.snapshot(buf);
        let mid = rect.center().y;
        let n = buf.len();
        let pts: Vec<egui::Pos2> =
            buf.iter().enumerate().map(|(i, v)| egui::pos2(rect.min.x + rect.width() * i as f32 / n as f32, mid - v.clamp(-1.0, 1.0) * rect.height() * 0.45)).collect();
        painter.add(egui::Shape::line(pts, egui::Stroke::new(1.0, theme::TEXT_SECONDARY)));
    }
}

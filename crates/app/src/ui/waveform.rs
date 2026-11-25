//! Waveform canvas (SYN-02/SYN-06): one cycle of the selected layer's stored
//! waveform, editable with brushes while notes play; spectrum paint mode; live
//! output scope. Every stroke is one undo entry; audio updates via table handoff.

use super::App;
use crate::theme;
use rawdawg_dsp::processor::Processor;
use rawdawg_dsp::wavetable::{CYCLE_LEN, MipTable, TableBuilder, cycles};
use rawdawg_project::Command;
use rawdawg_synthesizer::patch::Generator;
use rawdawg_synthesizer::sculpt::{self, Brush, BrushSettings, StrokePoint};
use rawdawg_types::InstanceId;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum CanvasMode {
    #[default]
    Waveform,
    Spectrum,
}

pub(crate) struct WaveformState {
    pub mode: CanvasMode,
    pub settings: BrushSettings,
    pub layer: usize,
    pub spectrum_db: bool,
    builder: TableBuilder,
    /// Active stroke: instance, pre-stroke state bytes, last pointer sample, dab counter.
    stroke: Option<Stroke>,
    /// A table built but not yet accepted by the queue (coalesced retry).
    pending_table: Option<(InstanceId, usize, Box<MipTable>)>,
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
            spectrum_db: false,
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
    Some(match &l.generator {
        Generator::Wavetable { cycle, frames, .. } => (frames.first().cloned().unwrap_or_else(|| cycle.clone()), true),
        Generator::Basic { wave, .. } => (basic_cycle(*wave), false),
        Generator::Granular { source, .. } => (source.clone(), true),
        Generator::Additive(a) => (crate::ui::waveform::additive_cycle(a), false),
        Generator::Fm(_) | Generator::Pluck(_) => (cycles::sine(), false),
    })
}

/// Push a cycle into the live synth (table handoff) and into the mirror patch.
fn apply_cycle_live(app: &mut App, instance: InstanceId, layer: usize, cycle: &[f32]) {
    let table = Box::new(app.synth_panel.waveform.builder.build(cycle, true));
    if let Some(m) = app.synth_mirrors.get_mut(&instance) {
        let mut patch = m.patch().clone();
        if let Some(l) = patch.layers.get_mut(layer) {
            match &mut l.generator {
                Generator::Wavetable { cycle: c, frames, .. } => {
                    if let Some(f) = frames.first_mut() {
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
    send_table(app, instance, layer, table);
}

fn send_table(app: &mut App, instance: InstanceId, layer: usize, table: Box<MipTable>) {
    // keep only the newest pending table (coalescing)
    let table = match app.table_senders.get_mut(&instance) {
        Some(s) => s.send(layer, 0, table).err(),
        None => None,
    };
    app.synth_panel.waveform.pending_table = table.map(|t| (instance, layer, t));
}

/// Retry a pending table send (called every frame).
pub(crate) fn flush_pending(app: &mut App) {
    if let Some((instance, layer, table)) = app.synth_panel.waveform.pending_table.take() {
        send_table(app, instance, layer, table);
    }
}

/// Commit the current mirror patch as one undo entry, keeping the live instance.
fn commit(app: &mut App, instance: InstanceId, before: &[u8]) {
    let after = app.synth_mirrors[&instance].save_state();
    if after == before {
        return;
    }
    let state: serde_json::Value = serde_json::from_slice(&after).unwrap_or(serde_json::Value::Null);
    app.mark_live_state(instance, rawdawg_synthesizer::TYPE_ID, &state);
    if let Err(e) = app.history.apply(&mut app.project, Command::SetInstanceState { instance_id: instance, state_version: 1, state }) {
        app.status_line = format!("Edit rejected: {e}");
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
    match app.synth_panel.waveform.mode {
        CanvasMode::Waveform => waveform_canvas(app, ui, instance, ws_layer),
        CanvasMode::Spectrum => spectrum_canvas(app, ui, instance, ws_layer),
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
        ui.add(egui::Slider::new(&mut app.synth_panel.waveform.settings.radius, 0.01..=0.5).text("Size").logarithmic(true).show_value(false))
            .on_hover_text("Brush size (mouse wheel over the wave)");
        ui.add(egui::Slider::new(&mut app.synth_panel.waveform.settings.strength, 0.0..=1.0).text("Strength").show_value(false));
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
        ui.add(egui::Slider::new(&mut app.synth_panel.waveform.settings.strength, 0.0..=1.0).text("Strength").show_value(false));
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

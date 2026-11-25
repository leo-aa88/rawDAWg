//! Parameter automation (CORE-08): curve editor (draw/move/delete points,
//! step/linear interpolation, target label and value range), automation clips
//! created from any parameter's context menu, touch/write capture of parameter
//! gestures while the song plays, and morph gestures baked into curves.

use super::{App, AudioState, EditorView, PointerState, TransportMode};
use crate::theme;
use egui::{Pos2, Rect, Sense, Stroke, pos2, vec2};
use rawdawg_audio_engine::schedule::Schedule;
use rawdawg_dsp::processor::Processor;
use rawdawg_project::{AutomationCurve, AutomationMode, AutomationPoint, AutomationTarget, Clip, ClipContent, Command, Interpolation, Track};
use rawdawg_types::units::{PPQ, Tick};
use rawdawg_types::{ClipId, CurveId, InstanceId, ParamDescriptor, ParamId, PointId, TrackId};

const RULER_H: f32 = 20.0;

pub(crate) struct AutomationState {
    pub curve: Option<CurveId>,
    scroll_tick: f32,
    px_per_tick: f32,
    selected: Option<PointId>,
    drag: Option<(PointId, Vec<AutomationPoint>)>,
    /// Gesture capture while the song plays (touch/write): curve, points captured so far.
    capture: Option<Capture>,
}

struct Capture {
    curve: CurveId,
    clip_start: i64,
    source_offset: i64,
    first_tick: i64,
    last_tick: i64,
    points: Vec<(i64, f32)>,
    /// Write mode keeps writing the last value until the gesture ends or playback stops.
    latch: bool,
    last_value: f32,
}

impl Default for AutomationState {
    fn default() -> Self {
        Self { curve: None, scroll_tick: 0.0, px_per_tick: 80.0 / PPQ as f32, selected: None, drag: None, capture: None }
    }
}

impl AutomationState {
    pub fn gesture_active(&self) -> bool {
        self.drag.is_some()
    }
}

/// Descriptor of a curve's target parameter (for labels/formatting), if the instance is known.
pub(crate) fn descriptor(app: &App, target: &AutomationTarget) -> Option<ParamDescriptor> {
    let descs: &[ParamDescriptor] =
        if let Some(s) = app.synth_mirrors.get(&target.instance_id) { s.descriptors() } else { app.other_mirrors.get(&target.instance_id)?.descriptors() };
    descs.iter().find(|d| d.id.0 == target.param_id).copied()
}

/// Readout of a value of a curve's target: the processor's own text when it has one (hosted
/// plugins), the descriptor's formatting otherwise, the bare value when the target is unknown.
pub(crate) fn value_text(app: &App, target: &AutomationTarget, desc: Option<&ParamDescriptor>, v: f32) -> String {
    match (desc, app.other_mirrors.get(&target.instance_id)) {
        (Some(d), Some(m)) => rawdawg_dsp::processor::display_value(&**m, d, v),
        (Some(d), None) => d.format(v),
        (None, _) => format!("{v:.3}"),
    }
}

pub(crate) fn target_label(app: &App, target: &AutomationTarget) -> String {
    let chan = app.project.channels.iter().find(|c| c.instrument.id == target.instance_id).map(|c| c.name.clone()).unwrap_or_else(|| "?".into());
    let param = descriptor(app, target).map(|d| d.name.to_owned()).unwrap_or_else(|| format!("param {}", target.param_id));
    format!("{chan} · {param}")
}

fn automation_track(app: &App) -> Option<TrackId> {
    app.project.tracks.iter().find(|t| t.name == "Automation").map(|t| t.id)
}

/// Create a curve for `target` with a 4-bar clip at the playhead bar (or bar 1) and open the editor.
pub(crate) fn create_clip_for(app: &mut App, instance_id: InstanceId, param_id: u32) {
    let target = AutomationTarget { instance_id, param_id };
    let current = current_value(app, &target);
    let len = bar_ticks(app) * 4;
    let points = vec![
        AutomationPoint { id: PointId::new(), tick: Tick::ZERO, value: current, interpolation: Interpolation::Linear },
        AutomationPoint { id: PointId::new(), tick: Tick(len), value: current, interpolation: Interpolation::Linear },
    ];
    place_curve(app, target, points, len);
}

/// LFO shapes for generated modulation curves.
pub(crate) const LFO_SHAPES: [&str; 4] = ["Sine", "Triangle", "Square", "Saw"];

/// Create a periodic ("LFO") automation clip over four bars: `cycles_per_bar` cycles of
/// `shape` (0 sine, 1 triangle, 2 square, 3 saw) swinging ±`depth` around the current value.
/// Modulation of effect parameters rides on the automation system (FX-03), so it is
/// editable, undoable and rendered identically offline.
pub(crate) fn create_lfo_for(app: &mut App, instance_id: InstanceId, param_id: u32, shape: usize, cycles_per_bar: f32, depth: f32) {
    let target = AutomationTarget { instance_id, param_id };
    let centre = current_value(app, &target);
    let bar = bar_ticks(app);
    let len = bar * 4;
    let cycles = (cycles_per_bar.max(0.0625) * 4.0).max(0.25);
    let per_cycle = 16usize;
    let n = ((cycles * per_cycle as f32).round() as usize).clamp(4, 512);
    let mut points = Vec::with_capacity(n + 1);
    for i in 0..=n {
        let t = i as f32 / n as f32; // 0..1 over the clip
        let ph = (t * cycles).fract();
        let (v, interp) = match shape {
            1 => (1.0 - 4.0 * (ph - 0.5).abs(), Interpolation::Linear),
            2 => (if ph < 0.5 { 1.0 } else { -1.0 }, Interpolation::Step),
            3 => (1.0 - 2.0 * ph, Interpolation::Linear),
            _ => ((std::f32::consts::TAU * ph).sin(), Interpolation::Linear),
        };
        let tick = ((len as f32) * t).round() as i64;
        points.push(AutomationPoint { id: PointId::new(), tick: Tick(tick), value: (centre + v * depth).clamp(0.0, 1.0), interpolation: interp });
    }
    place_curve(app, target, points, len);
}

fn current_value(app: &App, target: &AutomationTarget) -> f32 {
    app.synth_mirrors
        .get(&target.instance_id)
        .and_then(|s| s.param(ParamId(target.param_id)))
        .or_else(|| app.other_mirrors.get(&target.instance_id).and_then(|o| o.param(ParamId(target.param_id))))
        .unwrap_or(0.5)
}

fn bar_ticks(app: &App) -> i64 {
    app.project.meter_map().ok().map(|m| m.bar_start_tick(1).0).filter(|t| *t > 0).unwrap_or(PPQ * 4)
}

/// Add a curve with `points` as a clip starting at the current bar on the automation track.
fn place_curve(app: &mut App, target: AutomationTarget, points: Vec<AutomationPoint>, len: i64) {
    let bar = bar_ticks(app);
    let at = app.playhead_tick().map(|t| t.div_euclid(bar) * bar).unwrap_or(0).max(0);
    let curve = AutomationCurve { id: CurveId::new(), name: target_label(app, &target), target, points, mode: AutomationMode::Read, disabled: false };
    let cid = curve.id;
    let mut batch = vec![Command::AddAutomationCurve { curve }];
    let track_id = match automation_track(app) {
        Some(t) => t,
        None => {
            let id = TrackId::new();
            batch.push(Command::AddTrack {
                track: Track { id, name: "Automation".into(), color: Some([0xE6, 0xC9, 0x6E]), mute: false, solo: false, height: None, mixer_id: None },
                index: None,
            });
            id
        }
    };
    batch.push(Command::AddClip {
        clip: Clip {
            id: ClipId::new(),
            track_id,
            start_tick: Tick(at),
            length_ticks: Tick(len),
            muted: false,
            content: ClipContent::Automation { curve_id: cid, source_offset_ticks: Tick::ZERO, looped: false },
        },
    });
    app.apply(Command::Batch { commands: batch });
    app.automation.curve = Some(cid);
    app.editor_view = EditorView::Automation;
    if app.transport_mode != TransportMode::Song {
        app.status_line = "Automation clip created. Automation plays in Song mode.".into();
    }
}

/// Touch/write capture hook called from parameter edits (normalized value, gesture start/stop).
pub(crate) fn capture_edit(app: &mut App, instance_id: InstanceId, param_id: u32, value: f32, started: bool, stopped: bool) {
    let playing = app.is_playing() && app.transport_mode == TransportMode::Song;
    let Some(tick) = app.playhead_tick() else { return };
    let Some(curve) = app
        .project
        .automation
        .iter()
        .find(|c| c.target.instance_id == instance_id && c.target.param_id == param_id && matches!(c.mode, AutomationMode::Touch | AutomationMode::Write))
        .cloned()
    else {
        return;
    };
    if !playing {
        return;
    }
    // the clip of this curve under the playhead decides the curve-relative tick
    let Some(clip) = app
        .project
        .clips
        .iter()
        .find(|c| matches!(&c.content, ClipContent::Automation { curve_id, .. } if *curve_id == curve.id) && c.start_tick.0 <= tick && c.end_tick().0 > tick)
        .cloned()
    else {
        app.status_line = "No automation clip for this parameter under the playhead".into();
        return;
    };
    let ClipContent::Automation { source_offset_ticks, .. } = clip.content else { return };
    let rel = tick - clip.start_tick.0 + source_offset_ticks.0;
    let st = &mut app.automation;
    if started || st.capture.as_ref().is_none_or(|c| c.curve != curve.id) {
        st.capture = Some(Capture {
            curve: curve.id,
            clip_start: clip.start_tick.0,
            source_offset: source_offset_ticks.0,
            first_tick: rel,
            last_tick: rel,
            points: Vec::new(),
            latch: curve.mode == AutomationMode::Write,
            last_value: value,
        });
    }
    if let Some(c) = st.capture.as_mut() {
        c.points.push((rel, value));
        c.last_tick = rel;
        c.last_value = value;
        if stopped && !c.latch {
            finish_capture(app);
        }
    }
}

/// Called every frame: extend write-mode latch while playing, finish when playback stops.
pub(crate) fn poll_capture(app: &mut App) {
    let playing = app.is_playing() && app.transport_mode == TransportMode::Song;
    let tick = app.playhead_tick();
    let Some(c) = app.automation.capture.as_mut() else { return };
    if playing
        && c.latch
        && let Some(t) = tick
    {
        let rel = t - c.clip_start + c.source_offset;
        if rel > c.last_tick {
            c.points.push((rel, c.last_value));
            c.last_tick = rel;
        }
    }
    if !playing {
        finish_capture(app);
    }
}

fn finish_capture(app: &mut App) {
    let Some(c) = app.automation.capture.take() else { return };
    let Some(mut curve) = app.project.automation.iter().find(|x| x.id == c.curve).cloned() else { return };
    if c.points.is_empty() {
        return;
    }
    let (lo, hi) = (c.first_tick.min(c.last_tick), c.first_tick.max(c.last_tick));
    // replace what was under the gesture, keep points outside it; thin to ≥ 1/64 note
    curve.points.retain(|p| p.tick.0 < lo || p.tick.0 > hi);
    let mut last_t = i64::MIN;
    let mut added = 0;
    for (t, v) in &c.points {
        if *t - last_t >= PPQ / 16 || added == 0 {
            curve.points.push(AutomationPoint { id: PointId::new(), tick: Tick((*t).max(0)), value: v.clamp(0.0, 1.0), interpolation: Interpolation::Linear });
            last_t = *t;
            added += 1;
        }
    }
    if let Some((t, v)) = c.points.last()
        && *t != last_t
    {
        curve.points.push(AutomationPoint { id: PointId::new(), tick: Tick((*t).max(0)), value: v.clamp(0.0, 1.0), interpolation: Interpolation::Linear });
        added += 1;
    }
    curve.points.sort_by_key(|p| p.tick.0);
    app.apply(Command::UpdateAutomationCurve { curve });
    app.status_line = format!("Automation written: {added} point(s)");
}

/// Bake a recorded morph gesture (x, y, seconds) into curves for every parameter it changed.
pub(crate) fn bake_morph_gesture(app: &mut App, instance: InstanceId, corners: [&rawdawg_synthesizer::Patch; 4], gesture: &[(f32, f32, f64)]) -> usize {
    use rawdawg_synthesizer::params::DESCRIPTORS;
    if gesture.len() < 2 {
        return 0;
    }
    let t0 = gesture[0].2;
    let tempo = app.project.tempo_map().ok();
    let us_per_quarter = tempo.as_ref().map(|t| t.tempo_at(Tick::ZERO).0).unwrap_or(500_000) as f64;
    let tick_of = |secs: f64| ((secs - t0) * 1_000_000.0 / us_per_quarter * PPQ as f64).round() as i64;
    // sample every gesture point into a synth mirror and read all continuous params
    let mut samples: Vec<(i64, Vec<f32>)> = Vec::new();
    let mut mirror = rawdawg_synthesizer::Synth::new(corners[0].clone());
    for (x, y, t) in gesture {
        let Some(patch) = rawdawg_synthesizer::morph::morph(&corners, *x, *y) else { continue };
        if mirror.set_patch(patch).is_err() {
            continue;
        }
        let vals: Vec<f32> = DESCRIPTORS.iter().map(|d| mirror.param(d.id).unwrap_or(0.0)).collect();
        samples.push((tick_of(*t), vals));
    }
    if samples.len() < 2 {
        return 0;
    }
    // choose the parameters that actually move (bounded to 16, largest ranges first)
    let mut ranges: Vec<(usize, f32)> = (0..DESCRIPTORS.len())
        .filter(|i| {
            !DESCRIPTORS[*i].flags.read_only
                && !matches!(DESCRIPTORS[*i].mapping, rawdawg_types::params::ParamMapping::Toggle | rawdawg_types::params::ParamMapping::Discrete { .. })
        })
        .map(|i| {
            let (lo, hi) = samples.iter().fold((1.0f32, 0.0f32), |(lo, hi), (_, v)| (lo.min(v[i]), hi.max(v[i])));
            (i, hi - lo)
        })
        .filter(|(_, r)| *r > 1e-3)
        .collect();
    ranges.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranges.truncate(16);
    if ranges.is_empty() {
        return 0;
    }
    let bar = app.project.meter_map().ok().map(|m| m.bar_start_tick(1).0).filter(|t| *t > 0).unwrap_or(PPQ * 4);
    let at = app.playhead_tick().map(|t| t.div_euclid(bar) * bar).unwrap_or(0).max(0);
    let len_ticks = samples.last().map(|s| s.0).unwrap_or(0).max(1);
    let len = ((len_ticks + bar - 1) / bar).max(1) * bar;
    let mut batch = Vec::new();
    let track_id = match automation_track(app) {
        Some(t) => t,
        None => {
            let id = TrackId::new();
            batch.push(Command::AddTrack {
                track: Track { id, name: "Automation".into(), color: Some([0xE6, 0xC9, 0x6E]), mute: false, solo: false, height: None, mixer_id: None },
                index: None,
            });
            id
        }
    };
    for (i, _) in &ranges {
        let d = &DESCRIPTORS[*i];
        let target = AutomationTarget { instance_id: instance, param_id: d.id.0 };
        let mut points: Vec<AutomationPoint> = Vec::new();
        let mut last_t = i64::MIN;
        for (t, v) in &samples {
            if *t - last_t >= PPQ / 16 || points.is_empty() {
                points.push(AutomationPoint { id: PointId::new(), tick: Tick(*t), value: v[*i].clamp(0.0, 1.0), interpolation: Interpolation::Linear });
                last_t = *t;
            }
        }
        let curve = AutomationCurve { id: CurveId::new(), name: format!("morph · {}", d.name), target, points, mode: AutomationMode::Read, disabled: false };
        let cid = curve.id;
        batch.push(Command::AddAutomationCurve { curve });
        batch.push(Command::AddClip {
            clip: Clip {
                id: ClipId::new(),
                track_id,
                start_tick: Tick(at),
                length_ticks: Tick(len),
                muted: false,
                content: ClipContent::Automation { curve_id: cid, source_offset_ticks: Tick::ZERO, looped: false },
            },
        });
    }
    let n = ranges.len();
    app.apply(Command::Batch { commands: batch });
    n
}

/// The automation editor (bottom panel view).
pub(crate) fn show(app: &mut App, ui: &mut egui::Ui) {
    let mut cmds: Vec<Command> = Vec::new();
    ui.horizontal_wrapped(|ui| {
        ui.heading("Automation");
        let names: Vec<(CurveId, String)> = app
            .project
            .automation
            .iter()
            .map(|c| (c.id, format!("{}{} ({} pts)", target_label(app, &c.target), if c.disabled { " [orphan]" } else { "" }, c.points.len())))
            .collect();
        let current = app.automation.curve.and_then(|id| names.iter().find(|(i, _)| *i == id).map(|(_, n)| n.clone())).unwrap_or_else(|| "choose a curve".into());
        egui::ComboBox::from_id_salt("auto-curve").selected_text(current).width(260.0).show_ui(ui, |ui| {
            for (id, n) in &names {
                if ui.selectable_label(app.automation.curve == Some(*id), n).clicked() {
                    app.automation.curve = Some(*id);
                    app.automation.selected = None;
                }
            }
        });
        if names.is_empty() {
            ui.label(egui::RichText::new("No curves yet: right-click any parameter → Create automation clip").color(theme::TEXT_SECONDARY));
        }
    });
    let Some(cid) = app.automation.curve else { return };
    let Some(curve) = app.project.automation.iter().find(|c| c.id == cid).cloned() else {
        app.automation.curve = None;
        return;
    };
    let desc = descriptor(app, &curve.target);
    if curve.disabled {
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(theme::WARNING, "Orphan: its instrument or effect was removed. The points are kept; retarget it or delete it.");
            if let Some(ch) = app.selected_channel.and_then(|c| app.project.channels.iter().find(|x| x.id == c)).cloned()
                && ui.button(format!("Retarget to {} (same parameter id)", ch.name)).clicked()
            {
                let target = AutomationTarget { instance_id: ch.instrument.id, param_id: curve.target.param_id };
                let name = target_label(app, &target);
                cmds.push(Command::UpdateAutomationCurve { curve: AutomationCurve { target, disabled: false, name, ..curve.clone() } });
            }
            if ui.button("Delete curve").clicked() {
                cmds.push(Command::RemoveAutomationCurve { id: curve.id });
                app.automation.curve = None;
            }
        });
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(target_label(app, &curve.target)).strong());
        let mut mode = curve.mode;
        egui::ComboBox::from_id_salt("auto-mode").selected_text(mode_label(mode)).show_ui(ui, |ui| {
            for m in [AutomationMode::Off, AutomationMode::Read, AutomationMode::Touch, AutomationMode::Write] {
                ui.selectable_value(&mut mode, m, mode_label(m));
            }
        });
        if mode != curve.mode {
            cmds.push(Command::UpdateAutomationCurve { curve: AutomationCurve { mode, ..curve.clone() } });
        }
        ui.label(
            egui::RichText::new(match mode {
                AutomationMode::Off => "not played",
                AutomationMode::Read => "plays in Song mode",
                AutomationMode::Touch => "plays; moving the control while playing writes points until you let go",
                AutomationMode::Write => "plays; the first touch writes the control's value until playback stops",
            })
            .color(theme::TEXT_SECONDARY),
        );
        if let Some(sel) = app.automation.selected
            && let Some(pt) = curve.points.iter().find(|p| p.id == sel)
        {
            ui.separator();
            let mut interp = pt.interpolation;
            if ui.selectable_label(interp == Interpolation::Linear, "Linear").clicked() {
                interp = Interpolation::Linear;
            }
            if ui.selectable_label(interp == Interpolation::Step, "Step").clicked() {
                interp = Interpolation::Step;
            }
            if interp != pt.interpolation {
                let mut c = curve.clone();
                if let Some(p) = c.points.iter_mut().find(|p| p.id == sel) {
                    p.interpolation = interp;
                }
                cmds.push(Command::UpdateAutomationCurve { curve: c });
            }
            ui.label(egui::RichText::new(format!("point: {} = {}", pt.tick.0, value_text(app, &curve.target, desc.as_ref(), pt.value))).monospace().color(theme::TEXT_SECONDARY));
        }
        if ui.button("Delete curve").on_hover_text("Removes the curve and its clips (undoable)").clicked() {
            let mut batch: Vec<Command> = app
                .project
                .clips
                .iter()
                .filter(|c| matches!(&c.content, ClipContent::Automation { curve_id, .. } if *curve_id == cid))
                .map(|c| Command::RemoveClip { id: c.id })
                .collect();
            batch.push(Command::RemoveAutomationCurve { id: cid });
            cmds.push(Command::Batch { commands: batch });
            app.automation.curve = None;
        }
    });
    ui.label(
        egui::RichText::new("Click: add point · drag: move · right-click: delete · wheel/Ctrl+wheel: pan/zoom · values are the parameter's own range")
            .color(theme::TEXT_SECONDARY)
            .small(),
    );
    let avail = ui.available_size();
    let (full, resp) = ui.allocate_exact_size(vec2(avail.x.max(300.0), avail.y.max(180.0)), Sense::click_and_drag());
    let painter = ui.painter_at(full);
    painter.rect_filled(full, 3.0, theme::BACKGROUND);
    let area = Rect::from_min_max(pos2(full.min.x + 8.0, full.min.y + RULER_H), pos2(full.max.x - 8.0, full.max.y - 8.0));
    if ui.rect_contains_pointer(full) {
        let (scroll, zoom, ptr) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.pointer.hover_pos()));
        let st = &mut app.automation;
        if zoom != 1.0 {
            if let Some(p) = ptr {
                let tick_at = st.scroll_tick + (p.x - area.min.x) / st.px_per_tick;
                st.px_per_tick = (st.px_per_tick * zoom).clamp(4.0 / PPQ as f32, 1000.0 / PPQ as f32);
                st.scroll_tick = (tick_at - (p.x - area.min.x) / st.px_per_tick).max(0.0);
            }
        } else if scroll != egui::Vec2::ZERO {
            st.scroll_tick = (st.scroll_tick - (scroll.x + scroll.y) / st.px_per_tick).max(0.0);
        }
    }
    let (scroll_tick, ppt) = (app.automation.scroll_tick, app.automation.px_per_tick);
    let x_of = |tick: i64| area.min.x + (tick as f32 - scroll_tick) * ppt;
    let y_of = |v: f32| area.max.y - v.clamp(0.0, 1.0) * area.height();
    let tick_at = |x: f32| (scroll_tick + (x - area.min.x) / ppt).round() as i64;
    let value_at = |y: f32| ((area.max.y - y) / area.height()).clamp(0.0, 1.0);
    let bar = app.project.meter_map().ok().map(|m| m.bar_start_tick(1).0).filter(|t| *t > 0).unwrap_or(PPQ * 4);
    // grid + ruler (curve-relative bars)
    let last_tick = tick_at(area.max.x) + 1;
    let mut t = (scroll_tick as i64).div_euclid(PPQ) * PPQ;
    while t <= last_tick {
        let x = x_of(t);
        let is_bar = t % bar == 0;
        painter.line_segment([pos2(x, area.min.y), pos2(x, area.max.y)], Stroke::new(1.0, if is_bar { theme::BORDER } else { theme::BORDER.linear_multiply(0.4) }));
        if is_bar {
            painter.text(pos2(x + 3.0, full.min.y + 3.0), egui::Align2::LEFT_TOP, format!("{}", t / bar + 1), egui::FontId::proportional(10.0), theme::TEXT_SECONDARY);
        }
        t += PPQ;
    }
    for frac in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
        let y = y_of(frac);
        painter.line_segment([pos2(area.min.x, y), pos2(area.max.x, y)], Stroke::new(1.0, theme::BORDER.linear_multiply(0.4)));
        painter.text(
            pos2(area.min.x + 2.0, y - 1.0),
            egui::Align2::LEFT_BOTTOM,
            value_text(app, &curve.target, desc.as_ref(), frac),
            egui::FontId::proportional(9.0),
            theme::TEXT_SECONDARY,
        );
    }
    // the curve (as the engine samples it)
    let pts: Vec<AutomationPoint> = match &app.automation.drag {
        Some((_, working)) => working.clone(),
        None => curve.points.clone(),
    };
    let working_curve = AutomationCurve { points: pts.clone(), ..curve.clone() };
    let mut poly: Vec<Pos2> = Vec::new();
    let mut tk = scroll_tick as i64;
    while tk <= last_tick {
        if let Some(v) = Schedule::curve_value(&working_curve, tk) {
            poly.push(pos2(x_of(tk), y_of(v)));
        }
        tk += ((2.0 / ppt) as i64).max(1);
    }
    if poly.len() > 1 {
        painter.add(egui::Shape::line(poly, Stroke::new(1.5, theme::WARNING)));
    }
    for p in &pts {
        let c = pos2(x_of(p.tick.0), y_of(p.value));
        if c.x < area.min.x - 4.0 || c.x > area.max.x + 4.0 {
            continue;
        }
        let selected = app.automation.selected == Some(p.id);
        painter.circle(c, if selected { 5.0 } else { 3.5 }, if selected { theme::TEXT } else { theme::WARNING }, Stroke::new(1.0, theme::BACKGROUND));
        if p.interpolation == Interpolation::Step {
            painter.rect_stroke(Rect::from_center_size(c, vec2(9.0, 9.0)), 0.0, Stroke::new(1.0, theme::WARNING), egui::StrokeKind::Inside);
        }
    }
    // playhead (clip-relative when a clip of this curve is under it)
    if let Some(tick) = app.playhead_tick()
        && app.transport_mode == TransportMode::Song
        && let Some(clip) =
            app.project.clips.iter().find(|c| matches!(&c.content, ClipContent::Automation { curve_id, .. } if *curve_id == cid) && c.start_tick.0 <= tick && c.end_tick().0 > tick)
        && let ClipContent::Automation { source_offset_ticks, .. } = &clip.content
    {
        let x = x_of(tick - clip.start_tick.0 + source_offset_ticks.0);
        if x >= area.min.x && x <= area.max.x {
            painter.line_segment([pos2(x, full.min.y), pos2(x, area.max.y)], Stroke::new(1.5, theme::ACCENT));
        }
    }
    // interaction
    let ptr = PointerState::read(ui);
    let pointer = ptr.pos;
    let hit = |p: Pos2| pts.iter().find(|pt| (pos2(x_of(pt.tick.0), y_of(pt.value)) - p).length() < 7.0).cloned();
    if ptr.primary_pressed
        && resp.contains_pointer()
        && let Some(p) = pointer
        && area.contains(p)
    {
        match hit(p) {
            Some(pt) => {
                app.automation.selected = Some(pt.id);
                app.automation.drag = Some((pt.id, pts.clone()));
            }
            None => {
                let mut c = curve.clone();
                let new = AutomationPoint { id: PointId::new(), tick: Tick(tick_at(p.x).max(0)), value: value_at(p.y), interpolation: Interpolation::Linear };
                app.automation.selected = Some(new.id);
                c.points.push(new);
                c.points.sort_by_key(|q| q.tick.0);
                cmds.push(Command::UpdateAutomationCurve { curve: c });
            }
        }
    }
    if ptr.primary_down
        && !ptr.primary_pressed
        && let Some(p) = pointer
        && let Some((id, working)) = app.automation.drag.as_mut()
        && let Some(pt) = working.iter_mut().find(|q| q.id == *id)
    {
        pt.tick = Tick(tick_at(p.x).max(0));
        pt.value = value_at(p.y);
    }
    if (ptr.primary_released || !ptr.primary_down)
        && let Some((_, mut working)) = app.automation.drag.take()
    {
        working.sort_by_key(|q| q.tick.0);
        cmds.push(Command::UpdateAutomationCurve { curve: AutomationCurve { points: working, ..curve.clone() } });
    }
    if resp.secondary_clicked()
        && let Some(p) = pointer
        && let Some(pt) = hit(p)
        && curve.points.len() > 1
    {
        let mut c = curve.clone();
        c.points.retain(|q| q.id != pt.id);
        cmds.push(Command::UpdateAutomationCurve { curve: c });
    }
    if let Some(p) = resp.hover_pos()
        && area.contains(p)
    {
        let v = value_at(p.y);
        resp.on_hover_text_at_pointer(format!("{} · {}", tick_at(p.x), value_text(app, &curve.target, desc.as_ref(), v)));
    }
    for c in cmds {
        app.apply(c);
    }
    let _ = AudioState::Disabled;
}

fn mode_label(m: AutomationMode) -> &'static str {
    match m {
        AutomationMode::Off => "Off",
        AutomationMode::Read => "Read",
        AutomationMode::Touch => "Touch",
        AutomationMode::Write => "Write",
    }
}

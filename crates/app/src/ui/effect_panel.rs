//! Effect editor (FX-02): presets, bypass, wet/dry, reset, generic parameter
//! controls with gestures/undo, MIDI learn and automation hooks, meters, and
//! specialised editors (parametric EQ: draggable bands on a response plot).

use super::{App, AudioState, EngineCommand};
use crate::theme;
use egui::{Pos2, Rect, Sense, Stroke, pos2, vec2};
use rawdawg_project::Command;
use rawdawg_types::params::ParamMapping;
use rawdawg_types::{InstanceId, ParamDescriptor, ParamId};

#[derive(Default)]
pub(crate) struct EffectPanelState {
    gesture: Option<(InstanceId, Vec<u8>)>,
    pub advanced: bool,
    /// EQ band being dragged.
    eq_drag: Option<usize>,
    /// Delay tap being dragged.
    tap_drag: Option<usize>,
    /// Reverb decay-shape band being dragged.
    shape_drag: Option<usize>,
}

fn ensure_mirror(app: &mut App, instance_id: InstanceId) -> Result<(), String> {
    if app.other_mirrors.contains_key(&instance_id) {
        return Ok(());
    }
    let inst = app.project.instance(instance_id).cloned().ok_or("the effect no longer exists")?;
    let mut p = rawdawg_audio_engine::plan::make_effect(&inst, &inst.display_name).map_err(|e| e.to_string())?;
    let sr = match &app.audio {
        AudioState::Running(s) => f64::from(s.sample_rate.0),
        _ => 48_000.0,
    };
    let _ = p.prepare(&rawdawg_dsp::processor::PrepareSpec::new(sr, 256, rawdawg_dsp::BusLayout::Stereo));
    app.other_mirrors.insert(instance_id, p);
    Ok(())
}

pub(crate) fn show(app: &mut App, ui: &mut egui::Ui, instance_id: InstanceId) {
    let Some(inst) = app.project.instance(instance_id).cloned() else {
        ui.label("Pick an effect in the mixer.");
        return;
    };
    if let Err(e) = ensure_mirror(app, instance_id) {
        ui.colored_label(theme::ERROR, format!("Cannot edit this effect: {e}"));
        return;
    }
    let type_id = inst.type_id.as_str().to_owned();
    let info = rawdawg_effects::info(&type_id);
    let owner = app.project.mixer.channels.iter().find(|m| m.inserts.iter().any(|i| i.id == instance_id)).map(|m| m.name.clone()).unwrap_or_default();
    let mut cmds: Vec<Command> = Vec::new();
    ui.horizontal_wrapped(|ui| {
        ui.heading(format!("{} — {owner}", info.map(|i| i.name).unwrap_or(type_id.as_str())));
        let mut on = !inst.bypass;
        let on_label = if on { "On" } else { "Bypassed" };
        if ui.toggle_value(&mut on, on_label).changed() {
            cmds.push(Command::SetInstanceBypass { instance_id, bypass: !on });
        }
        if info.is_some_and(|i| i.has_mix) {
            let mut mix = inst.mix;
            if ui.add(egui::Slider::new(&mut mix, 0.0..=1.0).text("Wet")).changed() {
                cmds.push(Command::SetInstanceMix { instance_id, mix });
            }
        }
        let presets = rawdawg_effects::presets(&type_id);
        if !presets.is_empty() {
            egui::ComboBox::from_id_salt(("fx-preset", instance_id)).selected_text("Presets").show_ui(ui, |ui| {
                for p in presets {
                    if ui.selectable_label(false, p.name).on_hover_text(p.description).clicked()
                        && let Some(bytes) = rawdawg_effects::preset_state(&type_id, p)
                    {
                        let state: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
                        cmds.push(Command::SetInstanceState { instance_id, state_version: 1, state });
                        if let Some(m) = app.other_mirrors.get_mut(&instance_id) {
                            let _ = m.load_state(&bytes);
                        }
                    }
                }
            });
        }
        if ui.button("Reset").on_hover_text("All parameters to their defaults (undoable)").clicked()
            && let Some(m) = app.other_mirrors.get_mut(&instance_id)
        {
            for d in m.descriptors() {
                m.set_param(d.id, d.default_normalized);
            }
            let bytes = m.save_state();
            let state: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
            cmds.push(Command::SetInstanceState { instance_id, state_version: 1, state });
        }
        ui.checkbox(&mut app.effect_panel.advanced, "Advanced").on_hover_text("Show every parameter as a numeric control");
        if let Some(d) = info.map(|i| i.description) {
            ui.label(egui::RichText::new(d).color(theme::TEXT_SECONDARY));
        }
    });
    // meters
    if let AudioState::Running(sys) = &app.audio
        && let Some(slot) = app.slots.get(&instance_id)
        && let Some(a) = sys.handle.status.slot_meters.get(*slot)
    {
        let v = a.load();
        if v > -999.0 {
            match type_id.as_str() {
                rawdawg_effects::stereo::TYPE_ID => {
                    ui.label(egui::RichText::new(format!("correlation {v:+.2}")).monospace().color(theme::TEXT_SECONDARY));
                }
                rawdawg_effects::dynamics::compressor::TYPE_ID | rawdawg_effects::dynamics::limiter::TYPE_ID | rawdawg_effects::dynamics::gate::TYPE_ID => {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(format!("gain reduction {v:5.1} dB")).monospace().color(theme::TEXT_SECONDARY));
                        let (r, _) = ui.allocate_exact_size(vec2(160.0, 10.0), Sense::hover());
                        let painter = ui.painter();
                        painter.rect_filled(r, 2.0, theme::BACKGROUND);
                        let frac = (v / 24.0).clamp(0.0, 1.0);
                        painter.rect_filled(Rect::from_min_size(pos2(r.max.x - r.width() * frac, r.min.y), vec2(r.width() * frac, r.height())), 2.0, theme::WARNING);
                    });
                }
                _ => {
                    ui.label(egui::RichText::new(format!("meter {v:.2}")).monospace().color(theme::TEXT_SECONDARY));
                }
            }
        }
    }
    let mut edits: Vec<(ParamId, f32, bool, bool)> = Vec::new();
    if type_id == rawdawg_effects::eq::TYPE_ID {
        eq_editor(app, ui, instance_id, &mut edits);
    }
    if type_id == rawdawg_effects::delay::TYPE_ID {
        delay_editor(app, ui, instance_id, &mut edits);
    }
    if type_id == rawdawg_effects::reverb::TYPE_ID {
        reverb_editor(app, ui, instance_id, &mut edits);
    }
    let descriptors: Vec<ParamDescriptor> = app.other_mirrors[&instance_id].descriptors().iter().filter(|d| !d.flags.hidden).copied().collect();
    let simple_only = !app.effect_panel.advanced && type_id == rawdawg_effects::eq::TYPE_ID;
    if !simple_only {
        // the multitap tap parameters are edited on the canvas unless Advanced is on
        let shown: Vec<ParamDescriptor> =
            if type_id == rawdawg_effects::delay::TYPE_ID && !app.effect_panel.advanced { descriptors.iter().filter(|d| d.id.0 < 100).copied().collect() } else { descriptors };
        param_grid(app, ui, instance_id, &shown, &mut edits);
    }
    apply_edits(app, instance_id, &type_id, edits);
    for c in cmds {
        app.apply(c);
    }
}

fn param_grid(app: &mut App, ui: &mut egui::Ui, instance_id: InstanceId, descriptors: &[ParamDescriptor], edits: &mut Vec<(ParamId, f32, bool, bool)>) {
    egui::Grid::new(("fx-params", instance_id)).num_columns(3).spacing([12.0, 4.0]).show(ui, |ui| {
        for d in descriptors {
            let Some(current) = app.other_mirrors[&instance_id].param(d.id) else { continue };
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
                    let r = ui.add(egui::Slider::new(&mut idx, 0..=(steps as i64 - 1)).show_value(true));
                    n = d.to_normalized(idx as f64);
                    r
                }
                _ => ui.add(egui::Slider::new(&mut n, 0.0..=1.0).show_value(false)),
            };
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
                    if ui.button("Create automation clip").clicked() {
                        super::automation::create_clip_for(app, instance_id, d.id.0);
                        ui.close();
                    }
                    if ui.button("Reset to default").clicked() {
                        edits.push((d.id, d.default_normalized, true, true));
                        ui.close();
                    }
                })
                .map(|r| r.response)
                .unwrap_or(resp);
            ui.label(egui::RichText::new(format!("{}{}{}", d.format(n), cc, if learning { " learn…" } else { "" })).monospace().color(if learning {
                theme::WARNING
            } else {
                theme::TEXT_SECONDARY
            }));
            ui.end_row();
            if resp.changed() && (n - current).abs() > 1e-7 {
                let started = resp.drag_started() || app.effect_panel.gesture.is_none();
                edits.push((d.id, n, started, !resp.dragged()));
            } else if resp.drag_stopped() {
                edits.push((d.id, n, false, true));
            }
        }
    });
}

pub(crate) fn apply_edits(app: &mut App, instance_id: InstanceId, type_id: &str, edits: Vec<(ParamId, f32, bool, bool)>) {
    for (id, n, started, stopped) in edits {
        super::automation::capture_edit(app, instance_id, id.0, n, started, stopped);
        if started && app.effect_panel.gesture.is_none() {
            let before = app.other_mirrors[&instance_id].save_state();
            app.effect_panel.gesture = Some((instance_id, before));
        }
        if let Some(m) = app.other_mirrors.get_mut(&instance_id) {
            m.set_param(id, n);
        }
        app.send(EngineCommand::Param { instance: instance_id, id, value: n });
        if stopped {
            finish_gesture(app, instance_id, type_id);
        }
    }
}

fn finish_gesture(app: &mut App, instance_id: InstanceId, type_id: &str) {
    let Some((gid, before)) = app.effect_panel.gesture.take() else { return };
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

// ---------------------------------------------------------------------------
// Parametric EQ editor

fn freq_to_x(rect: Rect, f: f32) -> f32 {
    rect.min.x + (f / 20.0).log10() / 3.0 * rect.width()
}
fn x_to_freq(rect: Rect, x: f32) -> f32 {
    20.0 * 10f32.powf(((x - rect.min.x) / rect.width()).clamp(0.0, 1.0) * 3.0)
}
fn db_to_y(rect: Rect, db: f32) -> f32 {
    rect.center().y - db.clamp(-24.0, 24.0) / 24.0 * rect.height() * 0.5
}
fn y_to_db(rect: Rect, y: f32) -> f32 {
    ((rect.center().y - y) / (rect.height() * 0.5) * 24.0).clamp(-24.0, 24.0)
}

fn eq_editor(app: &mut App, ui: &mut egui::Ui, instance_id: InstanceId, edits: &mut Vec<(ParamId, f32, bool, bool)>) {
    use rawdawg_effects::eq::{B_ENABLED, B_FREQ, B_GAIN, B_KIND, B_Q, BANDS, KIND_LABELS, ParametricEq, band_param};
    let descs = app.other_mirrors[&instance_id].descriptors();
    let desc = |id: u32| descs.iter().find(|d| d.id.0 == id).copied();
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width().max(300.0), 220.0), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, theme::BACKGROUND);
    // grid
    for f in [50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10_000.0] {
        let x = freq_to_x(rect, f);
        painter.line_segment([pos2(x, rect.min.y), pos2(x, rect.max.y)], Stroke::new(1.0, theme::BORDER.linear_multiply(0.5)));
        painter.text(
            pos2(x + 2.0, rect.max.y - 2.0),
            egui::Align2::LEFT_BOTTOM,
            if f >= 1000.0 { format!("{}k", f / 1000.0) } else { format!("{f}") },
            egui::FontId::proportional(9.0),
            theme::TEXT_SECONDARY,
        );
    }
    for db in [-18.0, -12.0, -6.0, 0.0, 6.0, 12.0, 18.0] {
        let y = db_to_y(rect, db);
        painter.line_segment(
            [pos2(rect.min.x, y), pos2(rect.max.x, y)],
            Stroke::new(if db == 0.0 { 1.5 } else { 1.0 }, theme::BORDER.linear_multiply(if db == 0.0 { 1.2 } else { 0.5 })),
        );
        painter.text(pos2(rect.min.x + 2.0, y - 1.0), egui::Align2::LEFT_BOTTOM, format!("{db:+.0}"), egui::FontId::proportional(9.0), theme::TEXT_SECONDARY);
    }
    // response
    type Band = (usize, bool, f32, f32, usize, f32);
    let (curve, bands): (Vec<Pos2>, Vec<Band>) = {
        let m = app.other_mirrors.get_mut(&instance_id).expect("mirror");
        let eq = m.as_any_mut().downcast_mut::<ParametricEq>().expect("eq mirror");
        let n = 160;
        let curve = (0..=n)
            .map(|i| {
                let f = 20.0 * 10f32.powf(i as f32 / n as f32 * 3.0);
                pos2(freq_to_x(rect, f), db_to_y(rect, eq.response_db(f)))
            })
            .collect();
        let p = eq.params();
        let bands = (0..BANDS)
            .map(|b| {
                (
                    b,
                    p.as_bool(band_param(b, B_ENABLED)),
                    p.phys(band_param(b, B_FREQ)),
                    p.phys(band_param(b, B_GAIN)),
                    p.as_index(band_param(b, B_KIND)),
                    p.phys(band_param(b, B_Q)),
                )
            })
            .collect();
        (curve, bands)
    };
    painter.add(egui::Shape::line(curve, Stroke::new(2.0, theme::ACCENT)));
    let pointer = ui.input(|i| i.pointer.latest_pos());
    let ptr = super::PointerState::read(ui);
    let over = resp.contains_pointer();
    let handle_pos = |b: &Band| pos2(freq_to_x(rect, b.2), db_to_y(rect, if matches!(b.4, 3..=5) { 0.0 } else { b.3 }));
    let mut hovered_band: Option<usize> = None;
    // disabled handles first so enabled ones stay on top
    let mut draw_order: Vec<&Band> = bands.iter().collect();
    draw_order.sort_by_key(|b| b.1);
    for b in draw_order {
        let p = handle_pos(b);
        let col = if b.1 { theme::WARNING } else { theme::TEXT_SECONDARY.linear_multiply(0.4) };
        painter.circle(p, if b.1 { 6.0 } else { 4.0 }, col, Stroke::new(1.0, theme::BACKGROUND));
        painter.text(p + vec2(0.0, -10.0), egui::Align2::CENTER_BOTTOM, format!("{}", b.0 + 1), egui::FontId::proportional(9.0), theme::TEXT);
        if pointer.is_some_and(|q| (q - p).length() < 9.0) {
            hovered_band = Some(b.0);
        }
    }
    // interaction: drag handle = freq/gain, wheel = Q, click empty = enable nearest band there, right-click = disable
    if ptr.primary_pressed
        && over
        && let Some(p) = pointer
    {
        match hovered_band {
            Some(b) => {
                app.effect_panel.eq_drag = Some(b);
                if !bands[b].1
                    && let Some(d) = desc(band_param(b, B_ENABLED))
                {
                    edits.push((d.id, 1.0, true, false));
                }
            }
            None => {
                if let Some(b) = bands.iter().find(|b| !b.1).map(|b| b.0) {
                    let f = x_to_freq(rect, p.x);
                    let g = y_to_db(rect, p.y);
                    if let (Some(de), Some(df), Some(dg), Some(dk)) =
                        (desc(band_param(b, B_ENABLED)), desc(band_param(b, B_FREQ)), desc(band_param(b, B_GAIN)), desc(band_param(b, B_KIND)))
                    {
                        edits.push((de.id, 1.0, true, false));
                        edits.push((dk.id, dk.to_normalized(0.0), false, false));
                        edits.push((df.id, df.to_normalized(f64::from(f)), false, false));
                        edits.push((dg.id, dg.to_normalized(f64::from(g)), false, false));
                    }
                    app.effect_panel.eq_drag = Some(b);
                }
            }
        }
    }
    if ptr.primary_down
        && !ptr.primary_pressed
        && let Some(b) = app.effect_panel.eq_drag
        && let Some(p) = pointer
    {
        let f = x_to_freq(rect, p.x);
        let g = y_to_db(rect, p.y);
        if let (Some(df), Some(dg)) = (desc(band_param(b, B_FREQ)), desc(band_param(b, B_GAIN))) {
            edits.push((df.id, df.to_normalized(f64::from(f)), false, false));
            if !matches!(bands[b].4, 3..=5) {
                edits.push((dg.id, dg.to_normalized(f64::from(g)), false, false));
            }
        }
    }
    if (ptr.primary_released || !ptr.primary_down)
        && let Some(b) = app.effect_panel.eq_drag.take()
        && let Some(df) = desc(band_param(b, B_FREQ))
    {
        edits.push((df.id, df.to_normalized(f64::from(bands[b].2)), false, true));
    }
    if ptr.secondary_pressed
        && over
        && let Some(b) = hovered_band
        && let Some(d) = desc(band_param(b, B_ENABLED))
    {
        edits.push((d.id, 0.0, true, true));
    }
    if over && let Some(b) = hovered_band {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0
            && let Some(dq) = desc(band_param(b, B_Q))
        {
            let q = (bands[b].5 * (1.0 + scroll.signum() * 0.1)).clamp(0.1, 18.0);
            edits.push((dq.id, dq.to_normalized(f64::from(q)), true, true));
        }
        resp.clone().on_hover_text_at_pointer(format!(
            "Band {}: {} · {:.0} Hz · {:+.1} dB · Q {:.2}\ndrag: frequency/gain · wheel: Q · right-click: off",
            b + 1,
            KIND_LABELS[bands[b].4.min(5)],
            bands[b].2,
            bands[b].3,
            bands[b].5
        ));
    }
    // band type row
    ui.horizontal_wrapped(|ui| {
        for b in &bands {
            if !b.1 {
                continue;
            }
            let mut kind = b.4.min(5);
            egui::ComboBox::from_id_salt(("eq-kind", instance_id, b.0)).selected_text(format!("{}: {}", b.0 + 1, KIND_LABELS[kind])).width(120.0).show_ui(ui, |ui| {
                for (i, l) in KIND_LABELS.iter().enumerate() {
                    ui.selectable_value(&mut kind, i, *l);
                }
            });
            if kind != b.4.min(5)
                && let Some(dk) = desc(band_param(b.0, B_KIND))
            {
                edits.push((dk.id, dk.to_normalized(kind as f64), true, true));
            }
        }
        ui.label(egui::RichText::new("Click the plot to add a bell · drag a band · wheel changes Q · right-click removes").color(theme::TEXT_SECONDARY).small());
    });
}

// ---------------------------------------------------------------------------
// Delay: multitap editor (time ratio × gain, drag taps; pan with the wheel)

fn delay_editor(app: &mut App, ui: &mut egui::Ui, instance_id: InstanceId, edits: &mut Vec<(ParamId, f32, bool, bool)>) {
    use rawdawg_effects::delay::{Delay, MAX_TAPS, MODE_LABELS, T_GAIN, T_ON, T_PAN, T_TIME, tap_param};
    let descs = app.other_mirrors[&instance_id].descriptors();
    let desc = |id: u32| descs.iter().find(|d| d.id.0 == id).copied();
    let (mode, taps, base_ms) = {
        let m = app.other_mirrors.get_mut(&instance_id).expect("mirror");
        let d = m.as_any_mut().downcast_mut::<Delay>().expect("delay mirror");
        (d.mode(), d.taps(), d.base_time_ms(app.project.tempo.first().map(|t| t.us_per_quarter.bpm().0).unwrap_or(120.0)))
    };
    ui.label(egui::RichText::new(format!("{} · base time {base_ms:.0} ms", MODE_LABELS[mode.min(4)])).color(theme::TEXT_SECONDARY));
    if mode != 3 {
        return;
    }
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width().max(300.0), 160.0), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, theme::BACKGROUND);
    let x_of = |ratio: f32| rect.min.x + ratio.clamp(0.0, 4.0) / 4.0 * rect.width();
    let y_of = |gain_db: f32| rect.max.y - (gain_db.clamp(-48.0, 6.0) + 48.0) / 54.0 * rect.height();
    for k in 0..=8 {
        let x = x_of(k as f32 * 0.5);
        painter.line_segment([pos2(x, rect.min.y), pos2(x, rect.max.y)], Stroke::new(1.0, theme::BORDER.linear_multiply(if k % 2 == 0 { 1.0 } else { 0.4 })));
        painter.text(
            pos2(x + 2.0, rect.max.y - 2.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{:.0} ms", k as f32 * 0.5 * base_ms),
            egui::FontId::proportional(9.0),
            theme::TEXT_SECONDARY,
        );
    }
    let ptr = super::PointerState::read(ui);
    let pointer = ptr.pos;
    let mut hovered: Option<usize> = None;
    for (t, (on, time, gain, pan)) in taps.iter().enumerate() {
        let p = pos2(x_of(*time), y_of(*gain));
        let col = if *on { theme::WARNING } else { theme::TEXT_SECONDARY.linear_multiply(0.35) };
        painter.line_segment([pos2(p.x, rect.max.y), p], Stroke::new(if *on { 2.0 } else { 1.0 }, col));
        painter.circle(p, 5.0, col, Stroke::new(1.0, theme::BACKGROUND));
        painter.text(
            p + vec2(0.0, -9.0),
            egui::Align2::CENTER_BOTTOM,
            format!("{}{}", t + 1, if pan.abs() > 0.05 { if *pan < 0.0 { "L" } else { "R" } } else { "" }),
            egui::FontId::proportional(9.0),
            theme::TEXT,
        );
        if pointer.is_some_and(|q| (q - p).length() < 9.0) {
            hovered = Some(t);
        }
    }
    let over = resp.contains_pointer();
    if ptr.primary_pressed
        && over
        && let Some(p) = pointer
    {
        match hovered {
            Some(t) => {
                app.effect_panel.tap_drag = Some(t);
                if !taps[t].0
                    && let Some(d) = desc(tap_param(t, T_ON))
                {
                    edits.push((d.id, 1.0, true, false));
                }
            }
            None => {
                if let Some(t) = taps.iter().position(|x| !x.0) {
                    let ratio = ((p.x - rect.min.x) / rect.width() * 4.0).clamp(0.0, 4.0);
                    let gain = (rect.max.y - p.y) / rect.height() * 54.0 - 48.0;
                    if let (Some(don), Some(dt), Some(dg)) = (desc(tap_param(t, T_ON)), desc(tap_param(t, T_TIME)), desc(tap_param(t, T_GAIN))) {
                        edits.push((don.id, 1.0, true, false));
                        edits.push((dt.id, dt.to_normalized(f64::from(ratio)), false, false));
                        edits.push((dg.id, dg.to_normalized(f64::from(gain)), false, false));
                    }
                    app.effect_panel.tap_drag = Some(t);
                }
            }
        }
    }
    if ptr.primary_down
        && !ptr.primary_pressed
        && let Some(t) = app.effect_panel.tap_drag
        && let Some(p) = pointer
    {
        let ratio = ((p.x - rect.min.x) / rect.width() * 4.0).clamp(0.0, 4.0);
        let gain = (rect.max.y - p.y) / rect.height() * 54.0 - 48.0;
        if let (Some(dt), Some(dg)) = (desc(tap_param(t, T_TIME)), desc(tap_param(t, T_GAIN))) {
            edits.push((dt.id, dt.to_normalized(f64::from(ratio)), false, false));
            edits.push((dg.id, dg.to_normalized(f64::from(gain)), false, false));
        }
    }
    if (ptr.primary_released || !ptr.primary_down)
        && let Some(t) = app.effect_panel.tap_drag.take()
        && let Some(dt) = desc(tap_param(t, T_TIME))
    {
        edits.push((dt.id, dt.to_normalized(f64::from(taps[t].1)), false, true));
    }
    if ptr.secondary_pressed
        && over
        && let Some(t) = hovered
        && let Some(d) = desc(tap_param(t, T_ON))
    {
        edits.push((d.id, if taps[t].0 { 0.0 } else { 1.0 }, true, true));
    }
    if over && let Some(t) = hovered {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0
            && let Some(dp) = desc(tap_param(t, T_PAN))
        {
            let pan = (taps[t].3 + scroll.signum() * 0.1).clamp(-1.0, 1.0);
            edits.push((dp.id, dp.to_normalized(f64::from(pan)), true, true));
        }
        resp.clone().on_hover_text_at_pointer(format!(
            "Tap {}: {:.0} ms ({:.2}×) · {:+.1} dB · pan {:+.2}\ndrag: time/gain · wheel: pan · right-click: on/off",
            t + 1,
            taps[t].1 * base_ms,
            taps[t].1,
            taps[t].2,
            taps[t].3
        ));
    }
    let _ = MAX_TAPS;
    ui.label(egui::RichText::new("Click the canvas to add a tap · drag taps · wheel pans · right-click toggles").color(theme::TEXT_SECONDARY).small());
}

// ---------------------------------------------------------------------------
// Reverb: decay shape editor (four bands, RT60 multipliers, drawn as a curve)

fn reverb_editor(app: &mut App, ui: &mut egui::Ui, instance_id: InstanceId, edits: &mut Vec<(ParamId, f32, bool, bool)>) {
    use rawdawg_dsp::processor::Processor;
    use rawdawg_effects::reverb::{BAND_LABELS, MODE_LABELS, P_SHAPE0, Reverb};
    let descs = app.other_mirrors[&instance_id].descriptors();
    let desc = |id: u32| descs.iter().find(|d| d.id.0 == id).copied();
    let (mode, shape) = {
        let m = app.other_mirrors.get_mut(&instance_id).expect("mirror");
        let r = m.as_any_mut().downcast_mut::<Reverb>().expect("reverb mirror");
        let s: [f32; 4] =
            std::array::from_fn(|b| r.param(ParamId(P_SHAPE0 + b as u32)).map(|n| desc(P_SHAPE0 + b as u32).map(|d| d.to_physical(n) as f32).unwrap_or(1.0)).unwrap_or(1.0));
        (r.mode(), s)
    };
    ui.label(
        egui::RichText::new(format!(
            "{} · decay shape: how long each band rings relative to the decay time (stable by construction: no band can exceed unity gain)",
            MODE_LABELS[mode.min(5)]
        ))
        .color(theme::TEXT_SECONDARY),
    );
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width().max(300.0), 120.0), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, theme::BACKGROUND);
    let col_w = rect.width() / 4.0;
    let y_of = |m: f32| rect.max.y - ((m.clamp(0.1, 2.0) / 0.1).log10() / (20.0f32).log10()) * rect.height();
    let m_of = |y: f32| 0.1 * 20f32.powf(((rect.max.y - y) / rect.height()).clamp(0.0, 1.0));
    let y1 = y_of(1.0);
    painter.line_segment([pos2(rect.min.x, y1), pos2(rect.max.x, y1)], Stroke::new(1.0, theme::BORDER));
    painter.text(pos2(rect.min.x + 2.0, y1 - 1.0), egui::Align2::LEFT_BOTTOM, "1.0×", egui::FontId::proportional(9.0), theme::TEXT_SECONDARY);
    let pts: Vec<Pos2> = (0..4).map(|b| pos2(rect.min.x + (b as f32 + 0.5) * col_w, y_of(shape[b]))).collect();
    painter.add(egui::Shape::line(pts.clone(), Stroke::new(2.0, theme::WARNING)));
    let ptr = super::PointerState::read(ui);
    let hovered = ptr.pos.filter(|p| rect.contains(*p)).map(|p| (((p.x - rect.min.x) / col_w) as usize).min(3));
    for (b, p) in pts.iter().enumerate() {
        painter.circle(*p, 5.0, theme::WARNING, Stroke::new(1.0, theme::BACKGROUND));
        painter.text(pos2(p.x, rect.max.y - 2.0), egui::Align2::CENTER_BOTTOM, BAND_LABELS[b], egui::FontId::proportional(9.0), theme::TEXT_SECONDARY);
        painter.text(*p + vec2(0.0, -9.0), egui::Align2::CENTER_BOTTOM, format!("{:.2}×", shape[b]), egui::FontId::proportional(9.0), theme::TEXT);
    }
    if ptr.primary_pressed
        && resp.contains_pointer()
        && let Some(b) = hovered
    {
        app.effect_panel.shape_drag = Some(b);
    }
    if ptr.primary_down
        && let Some(b) = app.effect_panel.shape_drag
        && let Some(p) = ptr.pos
        && let Some(d) = desc(P_SHAPE0 + b as u32)
    {
        edits.push((d.id, d.to_normalized(f64::from(m_of(p.y))), ptr.primary_pressed, false));
    }
    if (ptr.primary_released || !ptr.primary_down)
        && let Some(b) = app.effect_panel.shape_drag.take()
        && let Some(d) = desc(P_SHAPE0 + b as u32)
    {
        edits.push((d.id, d.to_normalized(f64::from(shape[b])), false, true));
    }
    ui.label(egui::RichText::new("Drag a band up to let it ring longer, down to damp it faster").color(theme::TEXT_SECONDARY).small());
}

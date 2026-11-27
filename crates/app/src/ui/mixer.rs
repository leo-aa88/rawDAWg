//! Mixer (CORE-05): master pinned, scrollable strips with fader/pan/mute/solo,
//! stereo meters with peak hold and clip indicators, output routing, insert
//! chains (add/reorder/bypass/mix/delete), pre/post/sidechain sends and the
//! compensated latency of each channel.

use super::{App, AudioState, EditorView};
use crate::theme;
use egui::{Sense, pos2, vec2};
use rawdawg_audio_engine::commands::EngineCommand;
use rawdawg_dsp::processor::Processor;
use rawdawg_effects::rack::{self, RackLocks};
use rawdawg_project::{Command, Instance, MixerChannel, Send, SendTap};
use rawdawg_types::params::ParamCategory;
use rawdawg_types::units::{Db, Pan};
use rawdawg_types::{InstanceId, MixerChannelId, ParamId};
use std::collections::HashMap;

const STRIP_W: f32 = 118.0;
const FADER_H: f32 = 150.0;

#[derive(Default)]
pub(crate) struct MixerState {
    pub selected: Option<MixerChannelId>,
    /// Peak hold per mixer channel: (value, frames since set) for L and R, and clip flags.
    hold: HashMap<MixerChannelId, [(f32, u32); 2]>,
    clip: HashMap<MixerChannelId, bool>,
    rename: Option<(MixerChannelId, String)>,
    add_search: String,
    pub add_open: bool,
    /// Rack randomisation: chaos 0–100 and locks (mix/safety/routing locked by default).
    pub chaos: f32,
    pub locks: RackLocks,
    seed: u64,
    /// Four morph slots per channel: the chain's parameter snapshot (instance → params).
    pub morph_slots: HashMap<MixerChannelId, [Option<RackSnapshot>; 4]>,
    pub morph_xy: (f32, f32),
    /// Morph drag in progress: the chain's states before the gesture.
    morph_gesture: Option<Vec<(InstanceId, String, Vec<u8>)>>,
    pub rack_preset_path: String,
    /// Insert being dragged by its handle to a new place in the chain.
    insert_drag: Option<InstanceId>,
}

/// Id of an insert row's drag handle (stable, for input tests).
pub(crate) fn insert_handle_id(instance_id: InstanceId) -> egui::Id {
    egui::Id::unique(("insert-handle", instance_id))
}

/// A drag grip (three painted lines; no font glyph needed) for reordering a row. `active`
/// while its row is being dragged.
pub(crate) fn drag_grip(ui: &mut egui::Ui, id: egui::Id, active: bool, hint: &str) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(vec2(16.0, 18.0), Sense::hover());
    let handle = ui.interact(rect, id, Sense::drag());
    let color = if active || handle.hovered() { theme::TEXT } else { theme::TEXT_SECONDARY };
    for k in [-4.0f32, 0.0, 4.0] {
        let y = rect.center().y + k;
        ui.painter().hline(rect.center().x - 5.0..=rect.center().x + 5.0, y, egui::Stroke::new(1.5, color));
    }
    if handle.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    handle.on_hover_cursor(egui::CursorIcon::Grab).on_hover_text(hint)
}

/// While a row from `from` is dragged: the index it would move to (see [`insert_drop_index`]),
/// and an accent line `width` wide at that gap.
pub(crate) fn drop_target(ui: &egui::Ui, rows: &[egui::Rect], from: usize, width: f32) -> Option<usize> {
    let y = ui.ctx().pointer_interact_pos()?.y;
    let target = insert_drop_index(rows, from, y);
    if let (Some(first), true) = (rows.first(), target.is_some()) {
        let gap = rows.iter().take_while(|r| r.center().y < y).count();
        let line_y = rows.get(gap).map_or_else(|| rows.last().map_or(first.top(), |r| r.bottom()), |r| r.top());
        ui.painter().hline(first.left()..=first.left() + width, line_y, egui::Stroke::new(2.0, theme::ACCENT));
    }
    target
}

/// Where a dragged insert lands: the gap nearest `pointer_y` among `rows` (top to bottom),
/// as the `to_index` of `Command::MoveInsert` for the insert at `from`. `None` when it
/// would stay where it is.
pub(crate) fn insert_drop_index(rows: &[egui::Rect], from: usize, pointer_y: f32) -> Option<usize> {
    let gap = rows.iter().take_while(|r| r.center().y < pointer_y).count();
    let to = if gap > from { gap - 1 } else { gap };
    (to != from && from < rows.len()).then_some(to)
}

pub(crate) type RackSnapshot = Vec<(InstanceId, Vec<(ParamId, f32)>)>;

impl MixerState {
    pub(crate) fn new() -> Self {
        Self { chaos: 35.0, ..Default::default() }
    }
}

/// Parameter snapshot of every native insert of a channel.
pub(crate) fn rack_snapshot(app: &App, m: &MixerChannel) -> RackSnapshot {
    m.inserts
        .iter()
        .filter_map(|ins| {
            let p = app.other_mirrors.get(&ins.id)?;
            Some((ins.id, p.descriptors().iter().filter_map(|d| p.param(d.id).map(|v| (d.id, v))).collect()))
        })
        .collect()
}

/// Push parameter values into the mirrors and the engine; with `commit`, record one undo
/// step holding every changed insert's new state.
pub(crate) fn apply_rack_values(app: &mut App, values: &[(InstanceId, Vec<(ParamId, f32)>)], before: Option<&[(InstanceId, String, Vec<u8>)]>, commit: bool) {
    for (iid, params) in values {
        for (id, v) in params {
            if let Some(m) = app.other_mirrors.get_mut(iid) {
                m.set_param(*id, *v);
            }
            app.send(EngineCommand::Param { instance: *iid, id: *id, value: *v });
        }
    }
    if !commit {
        return;
    }
    let mut cmds = Vec::new();
    for (iid, _) in values {
        let Some(m) = app.other_mirrors.get(iid) else { continue };
        let after = m.save_state();
        let type_id = Processor::type_id(m.as_ref()).to_owned();
        if before.is_some_and(|b| b.iter().any(|(i, _, s)| i == iid && *s == after)) {
            continue;
        }
        let state: serde_json::Value = serde_json::from_slice(&after).unwrap_or(serde_json::Value::Null);
        app.mark_live_state(*iid, &type_id, &state);
        cmds.push(Command::SetInstanceState { instance_id: *iid, state_version: 1, state });
    }
    if !cmds.is_empty() {
        app.apply(Command::Batch { commands: cmds });
    }
}

fn chain_states(app: &App, m: &MixerChannel) -> Vec<(InstanceId, String, Vec<u8>)> {
    m.inserts.iter().filter_map(|ins| app.other_mirrors.get(&ins.id).map(|p| (ins.id, Processor::type_id(p.as_ref()).to_owned(), p.save_state()))).collect()
}

/// Read and reset the engine's per-mixer-channel peak meters (called once per frame).
fn poll_meters(app: &mut App) {
    let AudioState::Running(sys) = &app.audio else { return };
    let st = &sys.handle.status;
    for (i, m) in app.project.mixer.channels.iter().enumerate() {
        let (Some(a), Some(b)) = (st.mixer_peak_l.get(i), st.mixer_peak_r.get(i)) else { continue };
        let (l, r) = (a.load(), b.load());
        a.store(0.0);
        b.store(0.0);
        let hold = app.mixer.hold.entry(m.id).or_insert([(0.0, 0); 2]);
        for (slot, v) in hold.iter_mut().zip([l, r]) {
            if v >= slot.0 || slot.1 > 90 {
                *slot = (v, 0);
            } else {
                slot.1 += 1;
            }
        }
        if l >= 1.0 || r >= 1.0 {
            app.mixer.clip.insert(m.id, true);
        }
    }
}

fn strip_meter(ui: &mut egui::Ui, hold: [(f32, u32); 2], clip: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(22.0, FADER_H), Sense::click());
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, theme::BACKGROUND);
    for (c, (peak, _)) in hold.iter().enumerate() {
        let x0 = rect.min.x + 2.0 + c as f32 * 10.0;
        let db = rawdawg_dsp::math::lin_to_db(*peak).clamp(-60.0, 0.0);
        let h = (db + 60.0) / 60.0 * rect.height();
        let col = if db > -1.0 {
            theme::ERROR
        } else if db > -6.0 {
            theme::WARNING
        } else {
            theme::ACCENT
        };
        painter.rect_filled(egui::Rect::from_min_max(pos2(x0, rect.max.y - h), pos2(x0 + 8.0, rect.max.y)), 1.0, col);
    }
    let clip_rect = egui::Rect::from_min_size(rect.min, vec2(rect.width(), 5.0));
    painter.rect_filled(clip_rect, 1.0, if clip { theme::ERROR } else { theme::BORDER });
    resp.on_hover_text("Peak meters with hold; red bar = clipped (click to reset)").clicked()
}

pub(crate) fn show(app: &mut App, ui: &mut egui::Ui) {
    poll_meters(app);
    let mut cmds: Vec<Command> = Vec::new();
    ui.horizontal_wrapped(|ui| {
        ui.heading("Mixer");
        if ui.button("+ Bus").on_hover_text("New mixer channel routed to the master").clicked() {
            let taken = |n: &usize| app.project.mixer.channels.iter().any(|m| m.name == format!("Bus {n}"));
            let n = (app.project.mixer.channels.len()..).find(|n| !taken(n)).unwrap_or_default();
            let master = app.project.mixer.master_id;
            cmds.push(Command::AddMixerChannel {
                channel: MixerChannel {
                    id: MixerChannelId::new(),
                    name: format!("Bus {n}"),
                    color: None,
                    gain_db: Db::UNITY,
                    pan: Pan::CENTER,
                    mute: false,
                    solo: false,
                    inserts: vec![],
                    sends: vec![],
                    output_id: Some(master),
                },
                index: None,
            });
        }
        let mut protect = app.project.mixer.master_protection;
        if ui.checkbox(&mut protect, "Master protection").on_hover_text("Lookahead limiter on the master output (QUAL-02). Disable only on purpose.").changed() {
            cmds.push(Command::SetMasterProtection { enabled: protect });
            app.send(super::EngineCommand::SetMasterProtection(protect));
        }
        if let AudioState::Running(sys) = &app.audio {
            let lat = sys.handle.status.output_latency.load(std::sync::atomic::Ordering::Relaxed);
            let ms = lat as f64 * 1000.0 / f64::from(sys.sample_rate.0);
            ui.label(egui::RichText::new(format!("output latency {lat} frames ({ms:.1} ms)")).color(theme::TEXT_SECONDARY))
                .on_hover_text("Longest insert-chain latency compensated at the master");
        }
        ui.label(
            egui::RichText::new("Double-click a fader or pan to reset · right-click a name to rename or delete · select a strip for inserts and sends")
                .color(theme::TEXT_SECONDARY)
                .small(),
        );
    });
    let channels = app.project.mixer.channels.clone();
    let master = app.project.mixer.master_id;
    // strips: master pinned first, the rest scroll horizontally (row height fixed so the
    // inserts/sends section below keeps its place)
    let row_h = FADER_H + 165.0;
    ui.allocate_ui_with_layout(vec2(ui.available_width(), row_h), egui::Layout::left_to_right(egui::Align::TOP), |ui| {
        ui.set_height(row_h);
        if let Some(m) = channels.iter().find(|c| c.id == master) {
            strip(app, ui, m, &channels, &mut cmds);
        }
        ui.separator();
        egui::ScrollArea::horizontal().id_salt("mixer-strips").auto_shrink([false, false]).max_height(row_h).show(ui, |ui| {
            ui.horizontal_top(|ui| {
                for m in channels.iter().filter(|c| c.id != master) {
                    strip(app, ui, m, &channels, &mut cmds);
                }
            });
        });
    });
    ui.separator();
    if let Some(sel) = app.mixer.selected.filter(|id| app.project.mixer_channel(*id).is_some()) {
        inserts_and_sends(app, ui, sel, &channels, &mut cmds);
    } else {
        ui.label(egui::RichText::new("Select a strip to edit its inserts and sends.").color(theme::TEXT_SECONDARY));
    }
    for c in cmds {
        app.apply(c);
    }
}

fn strip(app: &mut App, ui: &mut egui::Ui, m: &MixerChannel, all: &[MixerChannel], cmds: &mut Vec<Command>) {
    let selected = app.mixer.selected == Some(m.id);
    let is_master = m.id == app.project.mixer.master_id;
    egui::Frame::group(ui.style()).fill(if selected { theme::RAISED } else { theme::PANEL }).show(ui, |ui| {
        ui.set_width(STRIP_W);
        ui.spacing_mut().slider_width = FADER_H;
        ui.vertical(|ui| {
            // name
            let editing = app.mixer.rename.as_ref().is_some_and(|(id, _)| *id == m.id);
            if editing {
                if let Some((_, name)) = app.mixer.rename.as_mut() {
                    let te = ui.add(egui::TextEdit::singleline(name).desired_width(STRIP_W - 10.0));
                    te.request_focus();
                    if te.lost_focus() {
                        let n = name.trim().to_owned();
                        if !n.is_empty() && n != m.name {
                            cmds.push(Command::RenameMixerChannel { id: m.id, name: n });
                        }
                        app.mixer.rename = None;
                    }
                }
            } else {
                let r = ui.add(egui::Button::selectable(selected, egui::RichText::new(&m.name).strong()).min_size(vec2(STRIP_W - 10.0, 0.0)));
                if r.clicked() {
                    app.mixer.selected = Some(m.id);
                }
                if r.double_clicked() {
                    app.mixer.rename = Some((m.id, m.name.clone()));
                }
                r.context_menu(|ui| {
                    if ui.button("Rename").clicked() {
                        app.mixer.rename = Some((m.id, m.name.clone()));
                        ui.close();
                    }
                    if !is_master && ui.button("Delete strip (undoable)").on_hover_text("What plays into it is rerouted to where it plays; sends to it are removed").clicked() {
                        if let Ok((cmd, rerouted)) = rawdawg_project::routing::remove_mixer_channel(&app.project, m.id) {
                            app.status_line = deleted_strip_message(&m.name, &rerouted);
                            cmds.push(cmd);
                        }
                        ui.close();
                    }
                });
            }
            ui.horizontal(|ui| {
                let mut mute = m.mute;
                if ui.toggle_value(&mut mute, "M").on_hover_text("Mute").changed() {
                    cmds.push(Command::SetMixerMute { id: m.id, mute });
                }
                let mut solo = m.solo;
                if ui.toggle_value(&mut solo, "S").on_hover_text("Solo (keeps the buses it feeds and its sidechain sources audible)").changed() {
                    cmds.push(Command::SetMixerSolo { id: m.id, solo });
                }
                let lat = m.inserts.len();
                if lat > 0 {
                    ui.label(egui::RichText::new(format!("{lat} fx")).small().color(theme::TEXT_SECONDARY));
                }
            });
            // pan
            let mut pan = m.pan.0;
            ui.spacing_mut().slider_width = STRIP_W - 30.0;
            let pr = ui.add(rawdawg_ui::params::slider(ui, &mut pan, -1.0..=1.0).show_value(false)).on_hover_text(format!("Pan {:+.2} (double-click resets)", m.pan.0));
            if pr.double_clicked() {
                cmds.push(Command::SetMixerPan { id: m.id, pan: Pan::CENTER });
            } else {
                gesture_slider(app, &pr, || Command::SetMixerPan { id: m.id, pan: Pan(pan) }, cmds);
            }
            // fader + meter
            ui.spacing_mut().slider_width = FADER_H;
            ui.horizontal(|ui| {
                let mut gain = m.gain_db.0;
                let fr = ui
                    .add(rawdawg_ui::params::slider(ui, &mut gain, -60.0..=12.0).vertical().show_value(false).custom_formatter(|v, _| format!("{v:.1} dB")))
                    .on_hover_text("Fader (double-click resets to 0 dB)");
                if fr.double_clicked() {
                    cmds.push(Command::SetMixerGain { id: m.id, gain_db: Db::UNITY });
                } else {
                    gesture_slider(app, &fr, || Command::SetMixerGain { id: m.id, gain_db: Db(gain) }, cmds);
                }
                let hold = app.mixer.hold.get(&m.id).copied().unwrap_or([(0.0, 0); 2]);
                let clip = app.mixer.clip.get(&m.id).copied().unwrap_or(false);
                if strip_meter(ui, hold, clip) {
                    app.mixer.clip.insert(m.id, false);
                }
            });
            ui.label(egui::RichText::new(format!("{:+.1} dB", m.gain_db.0)).monospace().color(theme::TEXT_SECONDARY));
            // output
            if !is_master {
                let current = m.output_id.and_then(|o| all.iter().find(|c| c.id == o)).map(|c| c.name.clone()).unwrap_or_else(|| "(none)".into());
                egui::ComboBox::from_id_salt(("mix-out", m.id)).selected_text(format!("to {current}")).width(STRIP_W - 10.0).show_ui(ui, |ui| {
                    for c in all.iter().filter(|c| c.id != m.id) {
                        if ui.selectable_label(m.output_id == Some(c.id), &c.name).clicked() {
                            cmds.push(Command::SetMixerOutput { id: m.id, output_id: Some(c.id) });
                        }
                    }
                });
            } else {
                ui.label(egui::RichText::new("master").small().color(theme::TEXT_SECONDARY));
            }
        });
    });
}

/// What deleting a strip did, in one line.
fn deleted_strip_message(name: &str, r: &rawdawg_project::routing::Rerouted) -> String {
    let mut msg = format!("Deleted mixer strip {name}");
    if !r.sources.is_empty() {
        let verb = if r.sources.len() == 1 { "plays" } else { "play" };
        msg += &format!("; {} now {verb} into {}", r.sources.join(", "), r.to);
    }
    if !r.sends_dropped.is_empty() {
        msg += &format!("; removed the send from {}", r.sends_dropped.join(", "));
    }
    msg + " (Undo brings it back)"
}

/// Fader/pan gesture: live preview through the history gesture, one undo entry on release.
fn gesture_slider(app: &mut App, r: &egui::Response, cmd: impl Fn() -> Command, cmds: &mut Vec<Command>) {
    if r.drag_started() {
        let _ = app.history.begin_gesture();
    }
    if r.changed() && app.history.gesture_active() {
        let _ = app.history.preview(&mut app.project, cmd());
        app.recompile();
    } else if r.changed() {
        cmds.push(cmd());
    }
    if r.drag_stopped() && app.history.gesture_active() {
        let _ = app.history.commit_gesture();
        app.recompile();
    }
}

fn inserts_and_sends(app: &mut App, ui: &mut egui::Ui, id: MixerChannelId, all: &[MixerChannel], cmds: &mut Vec<Command>) {
    let Some(m) = app.project.mixer_channel(id).cloned() else { return };
    // three columns: inserts, rack tools, sends
    let half = (ui.available_width() / 3.0 - 12.0).max(240.0);
    ui.horizontal_top(|ui| {
        // ---- inserts
        ui.vertical(|ui| {
            ui.set_width(half);
            ui.horizontal(|ui| {
                let hdr = ui.label(egui::RichText::new(format!("Inserts — {}", m.name)).strong());
                ui.interact(hdr.rect, egui::Id::unique("mixer-inserts-header"), Sense::hover());
                if ui.button("+ Add effect").clicked() {
                    app.mixer.add_open = !app.mixer.add_open;
                }
            });
            if app.mixer.add_open {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    let r = ui.add(egui::TextEdit::singleline(&mut app.mixer.add_search).hint_text("search effects…").desired_width(180.0));
                    r.request_focus();
                    let needle = app.mixer.add_search.to_lowercase();
                    ui.horizontal_wrapped(|ui| {
                        for fx in rawdawg_effects::CATALOG.iter().filter(|f| f.listed) {
                            if !needle.is_empty() && !fx.name.to_lowercase().contains(&needle) && !fx.family.to_lowercase().contains(&needle) {
                                continue;
                            }
                            if ui.button(fx.name).on_hover_text(fx.description).clicked() {
                                let mut inst = Instance::native(fx.type_id, serde_json::Value::Null);
                                inst.display_name = fx.name.to_owned();
                                let iid = inst.id;
                                cmds.push(Command::AddInsert { mixer_id: id, instance: inst, index: None });
                                app.mixer.add_open = false;
                                app.effect_editor = Some(iid);
                                app.editor_view = EditorView::Effect;
                            }
                        }
                        // hosted effects from the catalog
                        let hosted: Vec<(rawdawg_plugin_host::PluginDescription, std::path::PathBuf)> =
                            app.plugin_host.catalog.plugins().filter(|(_, p)| p.is_effect).map(|(l, p)| (p.clone(), l.path.clone())).collect();
                        for (desc, path) in hosted {
                            if !needle.is_empty() && !desc.name.to_lowercase().contains(&needle) && !desc.vendor.to_lowercase().contains(&needle) {
                                continue;
                            }
                            if ui
                                .button(format!("{} ({})", desc.name, desc.format.label()))
                                .on_hover_text(format!("{} — {}\n{}", desc.plugin_id, desc.vendor, path.display()))
                                .clicked()
                            {
                                app.mixer.add_open = false;
                                app.add_catalog_plugin(&desc, &path, id);
                            }
                        }
                    });
                    if ui.small_button("Close").clicked() {
                        app.mixer.add_open = false;
                    }
                });
            }
            if m.inserts.is_empty() {
                ui.label(egui::RichText::new("No inserts. Effects process this channel in order, top to bottom.").color(theme::TEXT_SECONDARY));
            }
            let mut rows: Vec<egui::Rect> = Vec::with_capacity(m.inserts.len());
            let mut dropped = false;
            for (i, ins) in m.inserts.iter().enumerate() {
                let row = ui.horizontal(|ui| {
                    // drag handle: drag the row up or down to reorder the chain
                    let handle = drag_grip(ui, insert_handle_id(ins.id), app.mixer.insert_drag == Some(ins.id), "Drag to move this effect earlier or later in the chain");
                    if handle.drag_started() {
                        app.mixer.insert_drag = Some(ins.id);
                    }
                    if handle.drag_stopped() {
                        dropped = true;
                    }
                    let name = if ins.display_name.is_empty() { ins.type_id.as_str().to_owned() } else { ins.display_name.clone() };
                    let open = app.effect_editor == Some(ins.id);
                    if ui.add(egui::Button::selectable(open, name)).on_hover_text("Open the effect editor").clicked() {
                        app.effect_editor = Some(ins.id);
                        app.editor_view = EditorView::Effect;
                    }
                    let mut on = !ins.bypass;
                    let on_label = if on { "On" } else { "Bypassed" };
                    if ui.toggle_value(&mut on, on_label).on_hover_text("True bypass; latency alignment is kept").changed() {
                        cmds.push(Command::SetInstanceBypass { instance_id: ins.id, bypass: !on });
                    }
                    if rawdawg_effects::info(ins.type_id.as_str()).is_some_and(|f| f.has_mix) {
                        let mut mix = ins.mix;
                        let r = ui.add(rawdawg_ui::params::slider(ui, &mut mix, 0.0..=1.0).show_value(false).text("mix"));
                        if r.changed() {
                            cmds.push(Command::SetInstanceMix { instance_id: ins.id, mix });
                        }
                    }
                    if i > 0 && ui.small_button("up").on_hover_text("Move earlier in the chain").clicked() {
                        cmds.push(Command::MoveInsert { mixer_id: id, instance_id: ins.id, to_index: i - 1 });
                    }
                    if i + 1 < m.inserts.len() && ui.small_button("down").on_hover_text("Move later in the chain").clicked() {
                        cmds.push(Command::MoveInsert { mixer_id: id, instance_id: ins.id, to_index: i + 1 });
                    }
                    if ui.small_button("x").on_hover_text("Remove (undoable)").clicked() {
                        cmds.push(Command::RemoveInsert { mixer_id: id, instance_id: ins.id });
                        if app.effect_editor == Some(ins.id) {
                            app.effect_editor = None;
                        }
                    }
                    if let Some(lat) = insert_latency(app, ins.id)
                        && lat > 0
                    {
                        ui.label(egui::RichText::new(format!("{lat} fr")).small().color(theme::TEXT_SECONDARY)).on_hover_text("Reported latency (compensated)");
                    }
                });
                rows.push(row.response.rect);
            }
            // while dragging: a line shows where the insert will land; release moves it (one undo step)
            if let Some(dragged) = app.mixer.insert_drag
                && let Some(from) = m.inserts.iter().position(|i| i.id == dragged)
            {
                let target = drop_target(ui, &rows, from, half);
                if dropped {
                    if let Some(to_index) = target {
                        cmds.push(Command::MoveInsert { mixer_id: id, instance_id: dragged, to_index });
                    }
                    app.mixer.insert_drag = None;
                }
            } else if dropped {
                app.mixer.insert_drag = None;
            }
        });
        ui.separator();
        // ---- rack tools: randomize / morph / parallel branch (FX-03)
        ui.vertical(|ui| {
            ui.set_width(half);
            rack_tools(app, ui, &m, cmds);
        });
        ui.separator();
        // ---- sends (takes what is left)
        ui.vertical(|ui| {
            ui.set_width((ui.available_width() - 8.0).max(220.0));
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Sends").strong());
                let targets: Vec<&MixerChannel> = all.iter().filter(|c| c.id != id).collect();
                egui::ComboBox::from_id_salt(("add-send", id)).selected_text("+ send to…").show_ui(ui, |ui| {
                    for t in targets {
                        if ui.selectable_label(false, &t.name).clicked() {
                            let mut sends = m.sends.clone();
                            sends.push(Send { target_id: t.id, level_db: Db(-6.0), tap: SendTap::PostFader, sidechain: false });
                            cmds.push(Command::SetMixerSends { id, sends });
                        }
                    }
                });
            });
            if m.sends.is_empty() {
                ui.label(egui::RichText::new("No sends. A send copies this channel into another bus (or its sidechain input).").color(theme::TEXT_SECONDARY));
            }
            for (si, s) in m.sends.iter().enumerate() {
                ui.horizontal(|ui| {
                    let tname = all.iter().find(|c| c.id == s.target_id).map(|c| c.name.clone()).unwrap_or_else(|| "?".into());
                    ui.label(format!("to {tname}"));
                    let mut level = s.level_db.0;
                    if ui.add(rawdawg_ui::params::slider(ui, &mut level, -60.0..=12.0).suffix(" dB").show_value(true)).changed() {
                        let mut sends = m.sends.clone();
                        sends[si].level_db = Db(level);
                        cmds.push(Command::SetMixerSends { id, sends });
                    }
                    let mut pre = s.tap == SendTap::PreFader;
                    let pre_label = if pre { "pre" } else { "post" };
                    if ui.toggle_value(&mut pre, pre_label).on_hover_text("Pre-fader sends ignore this channel's fader and mute").changed() {
                        let mut sends = m.sends.clone();
                        sends[si].tap = if pre { SendTap::PreFader } else { SendTap::PostFader };
                        cmds.push(Command::SetMixerSends { id, sends });
                    }
                    let mut sc = s.sidechain;
                    if ui.toggle_value(&mut sc, "sidechain").on_hover_text("Feed the target's sidechain input instead of its audio").changed() {
                        let mut sends = m.sends.clone();
                        sends[si].sidechain = sc;
                        cmds.push(Command::SetMixerSends { id, sends });
                    }
                    if ui.small_button("x").clicked() {
                        let mut sends = m.sends.clone();
                        sends.remove(si);
                        cmds.push(Command::SetMixerSends { id, sends });
                    }
                });
            }
        });
    });
    // routing cycles are rejected by the project model; surface the reason
    if let Some(e) = &app.plan_error {
        ui.colored_label(theme::ERROR, e);
    }
}

/// Main-thread mirrors for every native insert of a channel (the rack tools and the
/// morph snapshots read and write them; the effect editor creates them lazily otherwise).
pub(crate) fn ensure_rack_mirrors(app: &mut App, m: &MixerChannel) {
    for ins in &m.inserts {
        let _ = super::effect_panel::ensure_mirror(app, ins.id);
    }
}

fn rack_tools(app: &mut App, ui: &mut egui::Ui, m: &MixerChannel, cmds: &mut Vec<Command>) {
    let id = m.id;
    ensure_rack_mirrors(app, m);
    ui.label(egui::RichText::new("Rack").strong());
    ui.horizontal(|ui| {
        let can = !m.inserts.is_empty();
        if ui
            .add_enabled(can, egui::Button::new("Randomize rack"))
            .on_hover_text("Deterministic random changes to every insert's parameters, bounded by Chaos and the locks (one undo step)")
            .clicked()
        {
            let changed = randomize_rack(app, id);
            app.status_line = format!("Rack randomized ({changed} effects changed)");
        }
        ui.add(rawdawg_ui::params::slider(ui, &mut app.mixer.chaos, 0.0..=100.0).text("Chaos").show_value(true));
        ui.menu_button("Locks", |ui| {
            for (c, label) in [
                (ParamCategory::Mix, "Mix / output levels"),
                (ParamCategory::Topology, "Modes and topology"),
                (ParamCategory::Effects, "Effect parameters"),
                (ParamCategory::Modulation, "Modulation"),
                (ParamCategory::Pitch, "Pitch"),
                (ParamCategory::Filter, "Filter / EQ"),
            ] {
                let mut locked = app.mixer.locks.categories.contains(&c);
                if ui.checkbox(&mut locked, label).changed() {
                    app.mixer.locks.set_category(c, locked);
                }
            }
            if ui.button("Reset locks").clicked() {
                app.mixer.locks = RackLocks::default();
            }
        });
    });
    ui.horizontal(|ui| {
        ui.label("Morph");
        let filled: [bool; 4] = std::array::from_fn(|i| app.mixer.morph_slots.get(&id).is_some_and(|s| s[i].is_some()));
        for (i, name) in ["A", "B", "C", "D"].iter().enumerate() {
            let text = if filled[i] { format!("{name} (stored)") } else { format!("{name} store") };
            if ui.small_button(text).on_hover_text("Store the rack's current settings in this corner").clicked() {
                let snap = rack_snapshot(app, m);
                app.mixer.morph_slots.entry(id).or_default()[i] = Some(snap);
            }
        }
        let slots = app.mixer.morph_slots.entry(id).or_default();
        let ready = slots.iter().all(|s| s.as_ref().is_some_and(|s| s.len() == m.inserts.len() && s.iter().zip(&m.inserts).all(|((a, _), b)| *a == b.id)));
        if !ready {
            ui.label(egui::RichText::new("store A–D to morph").color(theme::TEXT_SECONDARY).small())
                .on_hover_text("Store the rack's settings in all four corners (same chain), then drag on the pad");
        }
    });
    // XY pad
    let (_, rect) = ui.allocate_space(vec2(120.0, 120.0));
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, theme::BACKGROUND);
    ui.interact(rect, egui::Id::unique(("morph-pad", id)), Sense::click_and_drag());
    let (x, y) = app.mixer.morph_xy;
    let p = pos2(rect.min.x + x * rect.width(), rect.max.y - y * rect.height());
    painter.circle_filled(p, 5.0, theme::ACCENT);
    for (i, (dx, dy)) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)].iter().enumerate() {
        painter.text(
            pos2(rect.min.x + 6.0 + dx * (rect.width() - 12.0), rect.max.y - 6.0 - dy * (rect.height() - 12.0)),
            egui::Align2::CENTER_CENTER,
            ["A", "B", "C", "D"][i],
            egui::FontId::proportional(10.0),
            theme::TEXT_SECONDARY,
        );
    }
    let ptr = super::PointerState::read(ui);
    let over = ptr.pos.is_some_and(|p| rect.contains(p));
    let ready = app
        .mixer
        .morph_slots
        .get(&id)
        .is_some_and(|slots| slots.iter().all(|s| s.as_ref().is_some_and(|s| s.len() == m.inserts.len() && s.iter().zip(&m.inserts).all(|((a, _), b)| *a == b.id))));
    if ready && ptr.primary_pressed && over {
        app.mixer.morph_gesture = Some(chain_states(app, m));
    }
    if app.mixer.morph_gesture.is_some()
        && ptr.primary_down
        && let Some(p) = ptr.pos
    {
        let x = ((p.x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
        let y = ((rect.max.y - p.y) / rect.height()).clamp(0.0, 1.0);
        app.mixer.morph_xy = (x, y);
        let slots = app.mixer.morph_slots.get(&id).cloned().unwrap_or_default();
        let mut values = Vec::new();
        for (k, ins) in m.inserts.iter().enumerate() {
            let corners: Option<[&[(ParamId, f32)]; 4]> =
                (|| Some([&slots[0].as_ref()?[k].1[..], &slots[1].as_ref()?[k].1[..], &slots[2].as_ref()?[k].1[..], &slots[3].as_ref()?[k].1[..]]))();
            let (Some(corners), Some(p)) = (corners, app.other_mirrors.get(&ins.id)) else { continue };
            values.push((ins.id, rack::morph(p.descriptors(), corners, x, y)));
        }
        apply_rack_values(app, &values, None, false);
    }
    if app.mixer.morph_gesture.is_some() && (ptr.primary_released || !ptr.primary_down) {
        let before = app.mixer.morph_gesture.take().unwrap_or_default();
        let values: Vec<(InstanceId, Vec<(ParamId, f32)>)> = m.inserts.iter().map(|ins| (ins.id, Vec::new())).collect();
        apply_rack_values(app, &values, Some(&before), true);
    }
    ui.horizontal(|ui| {
        ui.menu_button("Rack preset…", |ui| {
            ui.label(egui::RichText::new("Save this channel's insert chain as a .rawdawgpreset, or load one (adds the inserts)").small().color(theme::TEXT_SECONDARY));
            ui.add(egui::TextEdit::singleline(&mut app.mixer.rack_preset_path).hint_text("/path/to/rack.rawdawgpreset").desired_width(320.0));
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() && !app.mixer.rack_preset_path.trim().is_empty() {
                    let path = std::path::PathBuf::from(app.mixer.rack_preset_path.trim()).with_extension("rawdawgpreset");
                    let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "rack".into());
                    let manifest = rawdawg_project::preset::PresetManifest::new_rack(&name, &m.inserts);
                    app.status_line = match rawdawg_project::preset::write_preset(&path, &manifest, &[]) {
                        Ok(()) => format!("Saved rack preset {}", path.display()),
                        Err(e) => format!("Could not save the rack preset: {e}"),
                    };
                    ui.close();
                }
                if ui.button("Load").clicked() && !app.mixer.rack_preset_path.trim().is_empty() {
                    let path = std::path::PathBuf::from(app.mixer.rack_preset_path.trim());
                    match rawdawg_project::preset::read_preset(&path) {
                        Ok(f) if f.manifest.kind == "rack" => {
                            let inserts = f.manifest.rack_inserts();
                            let n = inserts.len();
                            cmds.push(Command::Batch { commands: inserts.into_iter().map(|instance| Command::AddInsert { mixer_id: id, instance, index: None }).collect() });
                            app.status_line = format!("Loaded rack preset “{}” ({n} effects added)", f.manifest.name);
                        }
                        Ok(f) => app.status_line = format!("That preset is a {} preset, not a rack", f.manifest.kind),
                        Err(e) => app.status_line = format!("Could not load the rack preset: {e}"),
                    }
                    ui.close();
                }
            });
        });
        if ui
            .button("+ Parallel branch")
            .on_hover_text("A new bus fed by a post-fader send at 0 dB and summed into this channel's output: build a parallel effect path on it (latency is aligned at the sum)")
            .clicked()
        {
            let bus_id = add_parallel_branch(m, cmds);
            app.mixer.selected = Some(bus_id);
        }
    });
}

/// Randomise every native insert of a mixer channel (one undo step). Returns how many changed.
pub(crate) fn randomize_rack(app: &mut App, id: MixerChannelId) -> usize {
    let Some(m) = app.project.mixer_channel(id).cloned() else { return 0 };
    ensure_rack_mirrors(app, &m);
    let before = chain_states(app, &m);
    let snap = rack_snapshot(app, &m);
    // a different sequence every launch (seeded on first use)
    if app.mixer.seed == 0 {
        app.mixer.seed = super::evolve::launch_entropy();
    }
    app.mixer.seed = app.mixer.seed.wrapping_add(1);
    let mut rng = rawdawg_dsp::rng::DeterministicRng::new(app.mixer.seed ^ 0x7ac4, id.0.as_u128() as u64);
    let mut values = Vec::new();
    for (iid, current) in &snap {
        let Some(p) = app.other_mirrors.get(iid) else { continue };
        let edits = rack::randomize(p.descriptors(), current, &mut rng, app.mixer.chaos, &app.mixer.locks);
        if !edits.is_empty() {
            values.push((*iid, edits));
        }
    }
    apply_rack_values(app, &values, Some(&before), true);
    values.len()
}

/// Queue the commands for a parallel branch bus of `m`: fed by a 0 dB post-fader send and
/// routed to the same output, so effects on it sum with the channel (latency aligned at the
/// sum). One undo step; the new bus id is returned.
pub(crate) fn add_parallel_branch(m: &MixerChannel, cmds: &mut Vec<Command>) -> MixerChannelId {
    let bus = MixerChannel {
        id: MixerChannelId::new(),
        name: format!("{} parallel", m.name),
        color: m.color,
        gain_db: Db(0.0),
        pan: Pan(0.0),
        mute: false,
        solo: false,
        inserts: vec![],
        sends: vec![],
        output_id: m.output_id,
    };
    let bus_id = bus.id;
    let mut sends = m.sends.clone();
    sends.push(Send { target_id: bus_id, level_db: Db(0.0), tap: SendTap::PostFader, sidechain: false });
    cmds.push(Command::Batch { commands: vec![Command::AddMixerChannel { channel: bus, index: None }, Command::SetMixerSends { id: m.id, sends }] });
    bus_id
}

fn insert_latency(app: &App, id: InstanceId) -> Option<u32> {
    app.other_mirrors.get(&id).map(|p| p.latency_frames())
}

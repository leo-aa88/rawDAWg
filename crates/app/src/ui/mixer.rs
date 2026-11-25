//! Mixer (CORE-05): master pinned, scrollable strips with fader/pan/mute/solo,
//! stereo meters with peak hold and clip indicators, output routing, insert
//! chains (add/reorder/bypass/mix/delete), pre/post/sidechain sends and the
//! compensated latency of each channel.

use super::{App, AudioState, EditorView};
use crate::theme;
use egui::{Sense, pos2, vec2};
use rawdawg_project::{Command, Instance, MixerChannel, Send, SendTap};
use rawdawg_types::units::{Db, Pan};
use rawdawg_types::{InstanceId, MixerChannelId};
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
            let n = app.project.mixer.channels.len();
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
            egui::RichText::new("Double-click a fader or pan to reset · double-click a name to rename · select a strip for inserts and sends").color(theme::TEXT_SECONDARY).small(),
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
            let pr = ui.add(egui::Slider::new(&mut pan, -1.0..=1.0).show_value(false)).on_hover_text(format!("Pan {:+.2} (double-click resets)", m.pan.0));
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
                    .add(egui::Slider::new(&mut gain, -60.0..=12.0).vertical().show_value(false).custom_formatter(|v, _| format!("{v:.1} dB")))
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
    let half = (ui.available_width() * 0.5 - 12.0).max(260.0);
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
                    });
                    if ui.small_button("Close").clicked() {
                        app.mixer.add_open = false;
                    }
                });
            }
            if m.inserts.is_empty() {
                ui.label(egui::RichText::new("No inserts. Effects process this channel in order, top to bottom.").color(theme::TEXT_SECONDARY));
            }
            for (i, ins) in m.inserts.iter().enumerate() {
                ui.horizontal(|ui| {
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
                        let r = ui.add(egui::Slider::new(&mut mix, 0.0..=1.0).show_value(false).text("mix"));
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
            }
        });
        ui.separator();
        // ---- sends
        ui.vertical(|ui| {
            ui.set_width(half);
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
                    if ui.add(egui::Slider::new(&mut level, -60.0..=12.0).suffix(" dB").show_value(true)).changed() {
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

fn insert_latency(app: &App, id: InstanceId) -> Option<u32> {
    app.other_mirrors.get(&id).map(|p| p.latency_frames())
}

//! Step sequencer rows in the channel rack and the shared pattern selector
//! (CORE-02). The step grid and the piano roll edit the same `Pattern` lanes:
//! a step is "on" when a note starts inside it; notes that do not sit on the
//! grid are shown as markers and are never moved by this view.

use super::App;
use crate::theme;
use egui::{Sense, Stroke, vec2};
use rawdawg_project::{Command, Note, Pattern};
use rawdawg_types::units::{PPQ, Tick};
use rawdawg_types::{ChannelId, NoteId, PatternId};

/// Default pitch for a step toggled on (C5, the usual drum/one-shot root).
pub const STEP_PITCH: u8 = 60;

#[derive(Default)]
pub(crate) struct StepState {
    /// Painting state while dragging across steps: (channel, value being painted).
    paint: Option<(ChannelId, bool)>,
    pub new_name: String,
    rename_for: Option<PatternId>,
}

pub(crate) const STEP_CHOICES: [(&str, i64); 4] = [("1/4", PPQ), ("1/8", PPQ / 2), ("1/16", PPQ / 4), ("1/32", PPQ / 8)];

/// Pattern chooser + new/rename + length controls; pushes commands into `cmds`.
pub(crate) fn pattern_selector(app: &mut App, ui: &mut egui::Ui, cmds: &mut Vec<Command>) {
    let selected = app.project.selected_pattern.or_else(|| app.project.patterns.first().map(|p| p.id));
    let name = selected.and_then(|id| app.project.pattern(id)).map(|p| p.name.clone()).unwrap_or_else(|| "(no pattern)".into());
    ui.label("Pattern");
    egui::ComboBox::from_id_salt("pattern-select").selected_text(name).show_ui(ui, |ui| {
        for p in &app.project.patterns {
            if ui.selectable_label(Some(p.id) == selected, &p.name).clicked() && Some(p.id) != selected {
                cmds.push(Command::SelectPattern { id: Some(p.id) });
            }
        }
    });
    if ui.button("+").on_hover_text("New empty pattern (1 bar, 16 steps)").clicked() {
        let n = app.project.patterns.len() + 1;
        let pattern = Pattern { id: PatternId::new(), name: format!("Pattern {n}"), color: None, length_ticks: Tick(PPQ * 4), steps: 16, step_ticks: Tick(PPQ / 4), lanes: vec![] };
        let id = pattern.id;
        cmds.push(Command::Batch { commands: vec![Command::AddPattern { pattern }, Command::SelectPattern { id: Some(id) }] });
    }
    if let Some(id) = selected {
        let r = ui.small_button("…").on_hover_text("Rename / length / delete");
        r.context_menu(|ui| {
            let Some(pat) = app.project.pattern(id).cloned() else { return };
            ui.label("Rename");
            if app.sequencer.new_name.is_empty() {
                app.sequencer.new_name = pat.name.clone();
            }
            let te = ui.text_edit_singleline(&mut app.sequencer.new_name);
            if te.lost_focus() && app.sequencer.new_name.trim() != pat.name && !app.sequencer.new_name.trim().is_empty() {
                cmds.push(Command::RenamePattern { id, name: app.sequencer.new_name.trim().to_owned() });
                ui.close();
            }
            ui.separator();
            let bar_ticks = app.project.meter_map().ok().map(|m| m.bar_start_tick(1).0).filter(|t| *t > 0).unwrap_or(PPQ * 4);
            let bars = (pat.length_ticks.0 as f64 / bar_ticks as f64).max(0.25);
            ui.label(format!("Length: {bars:.2} bars"));
            ui.horizontal(|ui| {
                for b in [1i64, 2, 4, 8] {
                    if ui.selectable_label((bars - b as f64).abs() < 1e-6, format!("{b}")).clicked() {
                        let length = Tick(bar_ticks * b);
                        let steps = (length.0 / pat.step_ticks.0.max(1)).clamp(1, 1024) as u16;
                        cmds.push(Command::SetPatternLength { id, length_ticks: length, steps, step_ticks: pat.step_ticks });
                    }
                }
                ui.label("bars");
            });
            ui.label("Step size");
            ui.horizontal(|ui| {
                for (label, ticks) in STEP_CHOICES {
                    if ui.selectable_label(pat.step_ticks.0 == ticks, label).clicked() && pat.step_ticks.0 != ticks {
                        let steps = (pat.length_ticks.0 / ticks).clamp(1, 1024) as u16;
                        cmds.push(Command::SetPatternLength { id, length_ticks: pat.length_ticks, steps, step_ticks: Tick(ticks) });
                    }
                }
            });
            ui.separator();
            if app.project.patterns.len() > 1 && ui.button("Delete pattern (undoable)").clicked() {
                cmds.push(Command::RemovePattern { id });
                ui.close();
            }
        });
        if app.sequencer.rename_for != Some(id) {
            app.sequencer.rename_for = Some(id);
            app.sequencer.new_name.clear();
        }
    }
}

/// One channel's step row. Returns true when the pointer toggled something.
pub(crate) fn step_row(app: &mut App, ui: &mut egui::Ui, channel_id: ChannelId, cmds: &mut Vec<Command>) {
    let Some(pid) = app.project.selected_pattern.or_else(|| app.project.patterns.first().map(|p| p.id)) else { return };
    let Some(pat) = app.project.pattern(pid).cloned() else { return };
    let notes: Vec<Note> = pat.lanes.iter().find(|l| l.channel_id == channel_id).map(|l| l.notes.clone()).unwrap_or_default();
    let step = pat.step_ticks.0.max(1);
    let steps = pat.steps.min(((pat.length_ticks.0 + step - 1) / step).max(1) as u16) as usize;
    let size = vec2(14.0, 18.0);
    let mut off_grid = 0usize;
    for n in &notes {
        if n.tick.0 % step != 0 {
            off_grid += 1;
        }
    }
    let pointer_down = ui.input(|i| i.pointer.primary_down());
    if !pointer_down {
        app.sequencer.paint = None;
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing = vec2(2.0, 0.0);
        for i in 0..steps {
            if i > 0 && i % 4 == 0 {
                ui.add_space(5.0);
            }
            let start = i as i64 * step;
            let end = start + step;
            let in_step: Vec<&Note> = notes.iter().filter(|n| n.tick.0 >= start && n.tick.0 < end).collect();
            let on = in_step.iter().any(|n| n.tick.0 == start);
            let marker = in_step.iter().any(|n| n.tick.0 != start);
            let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
            let resp = ui.interact(rect, egui::Id::unique(("step", channel_id, i)), Sense::click_and_drag());
            let group_dark = (i / 4) % 2 == 1;
            let fill = if on {
                let v = in_step.iter().map(|n| n.velocity).fold(0.0f32, f32::max);
                theme::ACCENT.linear_multiply(0.45 + 0.55 * v)
            } else if group_dark {
                theme::RAISED.linear_multiply(0.8)
            } else {
                theme::RAISED
            };
            let painter = ui.painter();
            painter.rect(rect, 2.0, fill, Stroke::new(1.0, if resp.hovered() { theme::TEXT_SECONDARY } else { theme::BORDER }), egui::StrokeKind::Inside);
            if marker {
                painter.circle_filled(rect.center_bottom() - vec2(0.0, 3.0), 2.0, theme::WARNING);
            }
            // playing step
            if let Some(tick) = app.playhead_tick()
                && app.transport_mode == rawdawg_audio_engine::TransportMode::Pattern
                && app.is_playing()
            {
                let local = tick.rem_euclid(pat.length_ticks.0.max(1));
                if local >= start && local < end {
                    painter.rect_stroke(rect, 2.0, Stroke::new(1.5, theme::WARNING), egui::StrokeKind::Inside);
                }
            }
            let toggle_to = |app: &mut App, want: bool, cmds: &mut Vec<Command>| {
                if want && !on {
                    let pitch = in_step.first().map(|n| n.pitch).unwrap_or(STEP_PITCH);
                    cmds.push(Command::AddNotes {
                        pattern_id: pid,
                        channel_id,
                        notes: vec![Note {
                            id: NoteId::new(),
                            tick: Tick(start),
                            duration_ticks: Tick(step.min(pat.length_ticks.0 - start).max(1)),
                            pitch,
                            velocity: 0.8,
                            channel: 0,
                        }],
                    });
                    app.piano_roll.selection.clear();
                } else if !want && on {
                    let ids: Vec<NoteId> = in_step.iter().filter(|n| n.tick.0 == start).map(|n| n.id).collect();
                    cmds.push(Command::RemoveNotes { pattern_id: pid, channel_id, note_ids: ids });
                }
            };
            let pressed_here = resp.contains_pointer() && ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary));
            if pressed_here {
                let want = !on;
                app.sequencer.paint = Some((channel_id, want));
                toggle_to(app, want, cmds);
            } else if resp.contains_pointer()
                && pointer_down
                && let Some((ch, want)) = app.sequencer.paint
                && ch == channel_id
                && want != on
            {
                toggle_to(app, want, cmds);
            }
            // secondary: velocity / pitch of this step's notes
            let step_notes: Vec<Note> = in_step.iter().map(|n| **n).collect();
            if !step_notes.is_empty() {
                resp.context_menu(|ui| {
                    ui.label(format!("Step {}", i + 1));
                    let mut vel = step_notes[0].velocity;
                    if ui.add(egui::Slider::new(&mut vel, 0.05..=1.0).text("Velocity")).changed() {
                        cmds.push(Command::UpdateNotes { pattern_id: pid, channel_id, notes: step_notes.iter().map(|n| Note { velocity: vel, ..*n }).collect() });
                    }
                    let mut pitch = step_notes[0].pitch as i32;
                    if ui.add(egui::Slider::new(&mut pitch, 0..=127).text("Pitch").custom_formatter(|v, _| super::piano_roll::pitch_name(v as u8))).changed() {
                        cmds.push(Command::UpdateNotes { pattern_id: pid, channel_id, notes: step_notes.iter().map(|n| Note { pitch: pitch as u8, ..*n }).collect() });
                    }
                    if ui.button("Open in piano roll").clicked() {
                        app.selected_channel = Some(channel_id);
                        app.piano_roll.channel = Some(channel_id);
                        app.editor_view = super::EditorView::PianoRoll;
                        ui.close();
                    }
                });
            }
            resp.on_hover_text(if marker { "A note in this step is off the grid (see the piano roll)" } else { "Click: toggle · drag: paint · right-click: velocity/pitch" });
        }
        if off_grid > 0 {
            ui.add_space(6.0);
            ui.label(egui::RichText::new(format!("{off_grid} off-grid")).color(theme::WARNING).small())
                .on_hover_text("Notes not on the step grid are kept as they are; edit them in the piano roll");
        }
    });
}

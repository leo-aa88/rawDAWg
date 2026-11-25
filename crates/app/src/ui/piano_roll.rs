//! Piano roll (CORE-03): keyboard ruler, bar/beat grid, draw/move/resize/delete,
//! box selection, clipboard, duplicate, nudge, snap with triplets, velocity lane,
//! scale highlight/snap, chord insertion and quantize with strength + preview.
//! Same note model as the step grid; every persistent edit is a command.

use super::{App, EngineCommand};
use crate::theme;
use egui::{Color32, Pos2, Rect, Sense, Stroke, pos2, vec2};
use rawdawg_project::{Command, Note};
use rawdawg_types::events::{EventSource, TimedEvent};
use rawdawg_types::units::{PPQ, Tick};
use rawdawg_types::{ChannelId, NoteId, PatternId};
use std::collections::HashSet;

const KEYS_W: f32 = 52.0;
const RULER_H: f32 = 20.0;
const VEL_H: f32 = 72.0;
const MIN_ROW_H: f32 = 6.0;
const MAX_ROW_H: f32 = 28.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Snap {
    Off,
    Bar,
    Beat,
    Half,
    Quarter,
    Eighth,
    Sixteenth,
    ThirtySecond,
    EighthT,
    SixteenthT,
}

impl Snap {
    pub const ALL: [Snap; 10] = [Snap::Off, Snap::Bar, Snap::Beat, Snap::Half, Snap::Quarter, Snap::Eighth, Snap::Sixteenth, Snap::ThirtySecond, Snap::EighthT, Snap::SixteenthT];
    pub fn label(self) -> &'static str {
        match self {
            Snap::Off => "Off",
            Snap::Bar => "Bar",
            Snap::Beat => "Beat",
            Snap::Half => "1/2",
            Snap::Quarter => "1/4",
            Snap::Eighth => "1/8",
            Snap::Sixteenth => "1/16",
            Snap::ThirtySecond => "1/32",
            Snap::EighthT => "1/8T",
            Snap::SixteenthT => "1/16T",
        }
    }
    /// Grid size in ticks (`bar_ticks`/`beat_ticks` come from the meter at the pattern start).
    pub fn ticks(self, bar_ticks: i64, beat_ticks: i64) -> Option<i64> {
        Some(match self {
            Snap::Off => return None,
            Snap::Bar => bar_ticks,
            Snap::Beat => beat_ticks,
            Snap::Half => PPQ * 2,
            Snap::Quarter => PPQ,
            Snap::Eighth => PPQ / 2,
            Snap::Sixteenth => PPQ / 4,
            Snap::ThirtySecond => PPQ / 8,
            Snap::EighthT => PPQ / 3,
            Snap::SixteenthT => PPQ / 6,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Tool {
    Draw,
    Select,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Chord {
    None,
    Major,
    Minor,
    Seventh,
    MinorSeventh,
    MajorSeventh,
    Sus2,
    Sus4,
    Dim,
    Aug,
    Power,
    Octave,
}

impl Chord {
    pub const ALL: [Chord; 12] = [
        Chord::None,
        Chord::Major,
        Chord::Minor,
        Chord::Seventh,
        Chord::MinorSeventh,
        Chord::MajorSeventh,
        Chord::Sus2,
        Chord::Sus4,
        Chord::Dim,
        Chord::Aug,
        Chord::Power,
        Chord::Octave,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Chord::None => "Single note",
            Chord::Major => "Major",
            Chord::Minor => "Minor",
            Chord::Seventh => "7",
            Chord::MinorSeventh => "m7",
            Chord::MajorSeventh => "maj7",
            Chord::Sus2 => "sus2",
            Chord::Sus4 => "sus4",
            Chord::Dim => "dim",
            Chord::Aug => "aug",
            Chord::Power => "Power (5)",
            Chord::Octave => "Octave",
        }
    }
    pub fn intervals(self) -> &'static [i32] {
        match self {
            Chord::None => &[0],
            Chord::Major => &[0, 4, 7],
            Chord::Minor => &[0, 3, 7],
            Chord::Seventh => &[0, 4, 7, 10],
            Chord::MinorSeventh => &[0, 3, 7, 10],
            Chord::MajorSeventh => &[0, 4, 7, 11],
            Chord::Sus2 => &[0, 2, 7],
            Chord::Sus4 => &[0, 5, 7],
            Chord::Dim => &[0, 3, 6],
            Chord::Aug => &[0, 4, 8],
            Chord::Power => &[0, 7],
            Chord::Octave => &[0, 12],
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ScaleMode {
    Major,
    NaturalMinor,
    HarmonicMinor,
    Dorian,
    Mixolydian,
    PentatonicMajor,
    PentatonicMinor,
    Blues,
}

impl ScaleMode {
    pub const ALL: [ScaleMode; 8] = [
        ScaleMode::Major,
        ScaleMode::NaturalMinor,
        ScaleMode::HarmonicMinor,
        ScaleMode::Dorian,
        ScaleMode::Mixolydian,
        ScaleMode::PentatonicMajor,
        ScaleMode::PentatonicMinor,
        ScaleMode::Blues,
    ];
    pub fn label(self) -> &'static str {
        match self {
            ScaleMode::Major => "Major",
            ScaleMode::NaturalMinor => "Minor",
            ScaleMode::HarmonicMinor => "Harmonic minor",
            ScaleMode::Dorian => "Dorian",
            ScaleMode::Mixolydian => "Mixolydian",
            ScaleMode::PentatonicMajor => "Pentatonic major",
            ScaleMode::PentatonicMinor => "Pentatonic minor",
            ScaleMode::Blues => "Blues",
        }
    }
    pub fn degrees(self) -> &'static [u8] {
        match self {
            ScaleMode::Major => &[0, 2, 4, 5, 7, 9, 11],
            ScaleMode::NaturalMinor => &[0, 2, 3, 5, 7, 8, 10],
            ScaleMode::HarmonicMinor => &[0, 2, 3, 5, 7, 8, 11],
            ScaleMode::Dorian => &[0, 2, 3, 5, 7, 9, 10],
            ScaleMode::Mixolydian => &[0, 2, 4, 5, 7, 9, 10],
            ScaleMode::PentatonicMajor => &[0, 2, 4, 7, 9],
            ScaleMode::PentatonicMinor => &[0, 3, 5, 7, 10],
            ScaleMode::Blues => &[0, 3, 5, 6, 7, 10],
        }
    }
}

pub const NOTE_NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

pub fn pitch_name(p: u8) -> String {
    format!("{}{}", NOTE_NAMES[(p % 12) as usize], (p / 12) as i32 - 1)
}

fn in_scale(pitch: u8, root: u8, mode: ScaleMode) -> bool {
    mode.degrees().contains(&((pitch + 12 - root % 12) % 12))
}

fn snap_to_scale(pitch: i32, root: u8, mode: ScaleMode) -> i32 {
    for d in 0..7 {
        for cand in [pitch - d, pitch + d] {
            if (0..=127).contains(&cand) && in_scale(cand as u8, root, mode) {
                return cand;
            }
        }
    }
    pitch.clamp(0, 127)
}

enum Gesture {
    /// Move selected notes; `orig` are the notes before the gesture, `anchor` the pointer start.
    Move {
        orig: Vec<Note>,
        anchor_tick: i64,
        anchor_pitch: i32,
        moved: bool,
    },
    /// Resize selected notes from their right edge.
    Resize {
        orig: Vec<Note>,
        anchor_tick: i64,
    },
    Box {
        start: Pos2,
        additive: bool,
    },
    Erase,
    /// Velocity lane draw/ramp: (tick, velocity) of the press.
    Velocity {
        orig: Vec<Note>,
        start_tick: i64,
        start_vel: f32,
    },
}

pub(crate) struct QuantizeDialog {
    pub strength: f32,
    pub preview: bool,
    /// Notes before preview (restored on cancel/apply through the gesture).
    previewing: bool,
}

pub(crate) struct PianoRollState {
    pub snap: Snap,
    pub tool: Tool,
    pub chord: Chord,
    pub scale_highlight: bool,
    pub scale_snap: bool,
    pub scale_root: u8,
    pub scale_mode: ScaleMode,
    /// Pitch (fractional) at the top edge of the grid.
    view_top: f32,
    /// First visible tick.
    scroll_tick: f32,
    px_per_tick: f32,
    row_h: f32,
    pub selection: HashSet<NoteId>,
    gesture: Option<Gesture>,
    /// Pattern/channel the gesture belongs to.
    gesture_target: Option<(PatternId, ChannelId)>,
    last_len: i64,
    /// Note sounding from the mouse (live note id, pitch).
    audition: Vec<(u32, u8)>,
    next_live_id: u32,
    pub clipboard: Vec<Note>,
    /// Notes captured when a quantize preview starts (restored through the gesture).
    clipboard_preview: Vec<Note>,
    pub quantize: Option<QuantizeDialog>,
    /// Hovered-note tooltip text.
    hover_text: String,
    /// Channel shown when the selected channel is not a pattern lane target.
    pub channel: Option<ChannelId>,
}

impl Default for PianoRollState {
    fn default() -> Self {
        Self {
            snap: Snap::Sixteenth,
            tool: Tool::Draw,
            chord: Chord::None,
            scale_highlight: false,
            scale_snap: false,
            scale_root: 0,
            scale_mode: ScaleMode::Major,
            view_top: 76.0,
            scroll_tick: 0.0,
            px_per_tick: 80.0 / PPQ as f32,
            row_h: 14.0,
            selection: HashSet::new(),
            gesture: None,
            gesture_target: None,
            last_len: PPQ / 4,
            audition: Vec::new(),
            next_live_id: 0x4D00_0000,
            clipboard: Vec::new(),
            clipboard_preview: Vec::new(),
            quantize: None,
            hover_text: String::new(),
            channel: None,
        }
    }
}

impl PianoRollState {
    pub fn gesture_active(&self) -> bool {
        self.gesture.is_some()
    }
}

/// Snap a tick down to the grid (`None` = no snap).
fn snap_tick(tick: i64, grid: Option<i64>) -> i64 {
    match grid {
        Some(g) if g > 0 => tick.div_euclid(g) * g,
        _ => tick,
    }
}
fn snap_round(tick: i64, grid: Option<i64>) -> i64 {
    match grid {
        Some(g) if g > 0 => ((tick as f64 / g as f64).round() as i64) * g,
        _ => tick,
    }
}

fn velocity_color(v: f32) -> Color32 {
    let t = v.clamp(0.0, 1.0);
    let base = theme::ACCENT;
    Color32::from_rgb((base.r() as f32 * (0.45 + 0.55 * t)) as u8, (base.g() as f32 * (0.45 + 0.55 * t)) as u8, (base.b() as f32 * (0.45 + 0.55 * t)) as u8)
}

/// Pattern + channel the roll edits, with the lane's notes (empty when no lane yet).
fn target(app: &App) -> Option<(PatternId, ChannelId, Vec<Note>, Tick)> {
    let pid = app.project.selected_pattern.or_else(|| app.project.patterns.first().map(|p| p.id))?;
    let cid = app.piano_roll.channel.filter(|c| app.project.channel(*c).is_some()).or(app.selected_channel).or_else(|| app.project.channels.first().map(|c| c.id))?;
    let pat = app.project.pattern(pid)?;
    let notes = pat.lanes.iter().find(|l| l.channel_id == cid).map(|l| l.notes.clone()).unwrap_or_default();
    Some((pid, cid, notes, pat.length_ticks))
}

fn audition_on(app: &mut App, pitch: u8, velocity: f32) {
    let id = app.piano_roll.next_live_id;
    app.piano_roll.next_live_id = app.piano_roll.next_live_id.wrapping_add(1);
    if let Some(ch) = app.piano_roll.channel.or(app.selected_channel) {
        app.send(EngineCommand::Live { channel: ch, event: TimedEvent::note_on(0, EventSource::Mouse, 0, id, pitch, velocity.clamp(0.05, 1.0)) });
        app.piano_roll.audition.push((id, pitch));
    }
}

pub(crate) fn audition_off(app: &mut App) {
    let notes = std::mem::take(&mut app.piano_roll.audition);
    if let Some(ch) = app.piano_roll.channel.or(app.selected_channel) {
        for (id, pitch) in notes {
            app.send(EngineCommand::Live { channel: ch, event: TimedEvent::note_off(0, EventSource::Mouse, 0, id, pitch) });
        }
    }
}

/// Cancel the active gesture (Escape): restores the pre-gesture notes.
pub(crate) fn cancel_gesture(app: &mut App) {
    if app.piano_roll.gesture.take().is_some() {
        if app.history.gesture_active() {
            let _ = app.history.cancel_gesture(&mut app.project);
            app.recompile();
        }
        audition_off(app);
    }
    if let Some(q) = app.piano_roll.quantize.as_mut()
        && q.previewing
    {
        q.previewing = false;
        q.preview = false;
        if app.history.gesture_active() {
            let _ = app.history.cancel_gesture(&mut app.project);
            app.recompile();
        }
    }
}

fn preview_update(app: &mut App, pid: PatternId, cid: ChannelId, notes: Vec<Note>) {
    if !app.history.gesture_active() {
        let _ = app.history.begin_gesture();
    }
    if let Err(e) = app.history.preview(&mut app.project, Command::UpdateNotes { pattern_id: pid, channel_id: cid, notes }) {
        app.status_line = format!("Edit rejected: {e}");
    }
}

fn commit_gesture(app: &mut App) {
    if app.history.gesture_active() {
        let _ = app.history.commit_gesture();
        app.recompile();
    }
}

fn quantize_notes(notes: &[Note], grid: i64, strength: f32) -> Vec<Note> {
    notes
        .iter()
        .map(|n| {
            let target = snap_round(n.tick.0, Some(grid));
            let moved = n.tick.0 + ((target - n.tick.0) as f32 * strength).round() as i64;
            Note { tick: Tick(moved.max(0)), ..*n }
        })
        .collect()
}

pub(crate) fn show(app: &mut App, ui: &mut egui::Ui) {
    let Some((pid, cid, notes, _)) = target(app) else {
        ui.label("Add a channel and a pattern first.");
        return;
    };
    let meter = app.project.meter_map().ok();
    let beat_ticks = meter.as_ref().map(|m| m.position(Tick::ZERO)).map(|_| PPQ).unwrap_or(PPQ);
    let bar_ticks = meter.as_ref().map(|m| m.bar_start_tick(1).0).filter(|t| *t > 0).unwrap_or(PPQ * 4);
    let grid = app.piano_roll.snap.ticks(bar_ticks, beat_ticks);

    // header
    let mut cmds: Vec<Command> = Vec::new();
    ui.horizontal_wrapped(|ui| {
        ui.heading("Piano roll");
        super::sequencer::pattern_selector(app, ui, &mut cmds);
        ui.separator();
        let chan_name = app.project.channel(cid).map(|c| c.name.clone()).unwrap_or_default();
        egui::ComboBox::from_id_salt("pr-channel").selected_text(chan_name).show_ui(ui, |ui| {
            for c in app.project.channels.clone() {
                if ui.selectable_label(c.id == cid, &c.name).clicked() {
                    app.piano_roll.channel = Some(c.id);
                    app.selected_channel = Some(c.id);
                    app.piano_roll.selection.clear();
                }
            }
        });
        ui.separator();
        ui.label("Snap");
        egui::ComboBox::from_id_salt("pr-snap").selected_text(app.piano_roll.snap.label()).show_ui(ui, |ui| {
            for s in Snap::ALL {
                ui.selectable_value(&mut app.piano_roll.snap, s, s.label());
            }
        });
        ui.selectable_value(&mut app.piano_roll.tool, Tool::Draw, "Draw")
            .on_hover_text("Click: add note · drag: move · right edge: resize · right-click: delete · right-drag: erase");
        ui.selectable_value(&mut app.piano_roll.tool, Tool::Select, "Select").on_hover_text("Drag a box to select (Shift adds). Ctrl+drag does this in Draw mode too");
        ui.separator();
        ui.label("Chord");
        egui::ComboBox::from_id_salt("pr-chord").selected_text(app.piano_roll.chord.label()).show_ui(ui, |ui| {
            for c in Chord::ALL {
                ui.selectable_value(&mut app.piano_roll.chord, c, c.label());
            }
        });
        ui.separator();
        ui.checkbox(&mut app.piano_roll.scale_highlight, "Scale").on_hover_text("Highlight the rows of a scale");
        ui.checkbox(&mut app.piano_roll.scale_snap, "Scale snap").on_hover_text("New and moved notes land on scale degrees");
        if app.piano_roll.scale_highlight || app.piano_roll.scale_snap {
            egui::ComboBox::from_id_salt("pr-root").selected_text(NOTE_NAMES[app.piano_roll.scale_root as usize % 12]).width(48.0).show_ui(ui, |ui| {
                for (i, n) in NOTE_NAMES.iter().enumerate() {
                    ui.selectable_value(&mut app.piano_roll.scale_root, i as u8, *n);
                }
            });
            egui::ComboBox::from_id_salt("pr-mode").selected_text(app.piano_roll.scale_mode.label()).show_ui(ui, |ui| {
                for m in ScaleMode::ALL {
                    ui.selectable_value(&mut app.piano_roll.scale_mode, m, m.label());
                }
            });
        }
        ui.separator();
        if ui.button("Quantize…").on_hover_text("Move the selected notes (or all) toward the snap grid with a strength").clicked() && app.piano_roll.quantize.is_none() {
            app.piano_roll.quantize = Some(QuantizeDialog { strength: 1.0, preview: false, previewing: false });
        }
        let sel = app.piano_roll.selection.len();
        ui.label(egui::RichText::new(if sel == 0 { format!("{} notes", notes.len()) } else { format!("{sel} of {} selected", notes.len()) }).color(theme::TEXT_SECONDARY));
    });
    for c in cmds {
        app.apply(c);
    }
    // the pattern may have changed through the selector
    let Some((pid2, cid2, notes, length)) = target(app).filter(|t| t.0 == pid && t.1 == cid).or_else(|| target(app)) else { return };
    let (pid, cid) = (pid2, cid2);
    if app.piano_roll.gesture_target.is_some_and(|t| t != (pid, cid)) {
        cancel_gesture(app);
    }

    // layout
    let avail = ui.available_size();
    let total_h = avail.y.max(220.0);
    let (full, resp) = ui.allocate_exact_size(vec2(avail.x.max(300.0), total_h), Sense::click_and_drag());
    let painter = ui.painter_at(full);
    painter.rect_filled(full, 3.0, theme::BACKGROUND);
    let grid_rect = Rect::from_min_max(pos2(full.min.x + KEYS_W, full.min.y + RULER_H), pos2(full.max.x, full.max.y - VEL_H));
    let keys_rect = Rect::from_min_max(pos2(full.min.x, grid_rect.min.y), pos2(grid_rect.min.x, grid_rect.max.y));
    let ruler_rect = Rect::from_min_max(pos2(grid_rect.min.x, full.min.y), pos2(full.max.x, grid_rect.min.y));
    let vel_rect = Rect::from_min_max(pos2(grid_rect.min.x, grid_rect.max.y), pos2(full.max.x, full.max.y));

    // wheel: vertical scroll, Shift horizontal, Ctrl zoom (around pointer)
    let hovered = ui.rect_contains_pointer(full);
    if hovered {
        let (scroll, zoom, mods, ptr) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.modifiers, i.pointer.hover_pos()));
        let st = &mut app.piano_roll;
        if zoom != 1.0 {
            if let Some(p) = ptr {
                let tick_at = st.scroll_tick + (p.x - grid_rect.min.x) / st.px_per_tick;
                st.px_per_tick = (st.px_per_tick * zoom).clamp(8.0 / PPQ as f32, 2000.0 / PPQ as f32);
                st.scroll_tick = (tick_at - (p.x - grid_rect.min.x) / st.px_per_tick).max(0.0);
                if mods.shift {
                    let pitch_at = st.view_top - (p.y - grid_rect.min.y) / st.row_h;
                    st.row_h = (st.row_h * zoom).clamp(MIN_ROW_H, MAX_ROW_H);
                    st.view_top = pitch_at + (p.y - grid_rect.min.y) / st.row_h;
                }
            }
        } else if scroll != egui::Vec2::ZERO && !mods.command {
            if mods.shift || scroll.x != 0.0 {
                let dx = if mods.shift { scroll.y + scroll.x } else { scroll.x };
                st.scroll_tick = (st.scroll_tick - dx / st.px_per_tick).max(0.0);
            } else {
                st.view_top += scroll.y / st.row_h;
            }
        }
        let rows = grid_rect.height() / st.row_h;
        st.view_top = st.view_top.clamp(rows.min(128.0), 128.0);
    }
    let (view_top, scroll_tick, ppt, row_h) = {
        let st = &app.piano_roll;
        (st.view_top, st.scroll_tick, st.px_per_tick, st.row_h)
    };
    let x_of = |tick: i64| grid_rect.min.x + (tick as f32 - scroll_tick) * ppt;
    let y_of = |pitch_edge: f32| grid_rect.min.y + (view_top - pitch_edge) * row_h;
    let tick_at = |x: f32| (scroll_tick + (x - grid_rect.min.x) / ppt).floor() as i64;
    let pitch_at = |y: f32| (view_top - (y - grid_rect.min.y) / row_h).floor() as i32;
    let note_rect = |n: &Note| Rect::from_min_max(pos2(x_of(n.tick.0), y_of(n.pitch as f32 + 1.0)), pos2(x_of(n.end_tick().0).max(x_of(n.tick.0) + 3.0), y_of(n.pitch as f32)));

    // rows + scale highlight
    let bottom_pitch = (view_top - grid_rect.height() / row_h).floor().max(0.0) as i32;
    let top_pitch = view_top.ceil().min(127.0) as i32;
    for p in bottom_pitch..=top_pitch {
        let r = Rect::from_min_max(pos2(grid_rect.min.x, y_of(p as f32 + 1.0)), pos2(grid_rect.max.x, y_of(p as f32))).intersect(grid_rect);
        let black = matches!(p % 12, 1 | 3 | 6 | 8 | 10);
        let mut fill = if black { theme::BACKGROUND } else { theme::PANEL.linear_multiply(0.8) };
        if app.piano_roll.scale_highlight && in_scale(p as u8, app.piano_roll.scale_root, app.piano_roll.scale_mode) {
            fill = theme::ACCENT.linear_multiply(if p % 12 == app.piano_roll.scale_root as i32 % 12 { 0.22 } else { 0.1 });
        }
        painter.rect_filled(r, 0.0, fill);
        if p % 12 == 0 {
            painter.line_segment([pos2(grid_rect.min.x, r.max.y), pos2(grid_rect.max.x, r.max.y)], Stroke::new(1.0, theme::BORDER));
        }
        // keyboard: white keys with black keys drawn shorter on top
        let kr = Rect::from_min_max(pos2(keys_rect.min.x, r.min.y), pos2(keys_rect.max.x - 2.0, r.max.y));
        painter.rect_filled(kr, 0.0, Color32::from_gray(0xD8));
        if black {
            painter.rect_filled(Rect::from_min_max(kr.min, pos2(kr.min.x + kr.width() * 0.6, kr.max.y)), 0.0, Color32::from_gray(0x18));
        } else if matches!(p % 12, 0 | 5) {
            painter.line_segment([pos2(kr.min.x, kr.max.y), pos2(kr.max.x, kr.max.y)], Stroke::new(1.0, Color32::from_gray(0x80)));
        }
        if p % 12 == 0 && row_h >= 9.0 {
            painter.text(kr.right_center() - vec2(4.0, 0.0), egui::Align2::RIGHT_CENTER, pitch_name(p as u8), egui::FontId::proportional(10.0), Color32::from_gray(0x30));
        }
    }
    // grid lines
    let first_tick = scroll_tick as i64;
    let last_tick = tick_at(grid_rect.max.x) + 1;
    let fine = grid.unwrap_or(PPQ / 4).max(1);
    let fine_px = fine as f32 * ppt;
    let mut t = snap_tick(first_tick, Some(fine));
    while t <= last_tick {
        let x = x_of(t);
        let (w, col) = if t % bar_ticks == 0 {
            (1.5, theme::BORDER.linear_multiply(1.6))
        } else if t % beat_ticks == 0 {
            (1.0, theme::BORDER)
        } else if fine_px >= 6.0 {
            (1.0, theme::BORDER.linear_multiply(0.45))
        } else {
            t += fine;
            continue;
        };
        painter.line_segment([pos2(x, grid_rect.min.y), pos2(x, grid_rect.max.y)], Stroke::new(w, col));
        t += fine;
    }
    // pattern end
    let end_x = x_of(length.0);
    if end_x < grid_rect.max.x {
        painter.rect_filled(Rect::from_min_max(pos2(end_x.max(grid_rect.min.x), grid_rect.min.y), grid_rect.max), 0.0, Color32::from_black_alpha(110));
        painter.line_segment([pos2(end_x, grid_rect.min.y), pos2(end_x, grid_rect.max.y)], Stroke::new(1.5, theme::WARNING));
    }
    // ruler
    painter.rect_filled(ruler_rect, 0.0, theme::PANEL);
    let mut bar = first_tick / bar_ticks;
    while bar * bar_ticks <= last_tick {
        let x = x_of(bar * bar_ticks);
        painter.text(pos2(x + 3.0, ruler_rect.center().y), egui::Align2::LEFT_CENTER, format!("{}", bar + 1), egui::FontId::proportional(11.0), theme::TEXT);
        let beats = (bar_ticks / beat_ticks).max(1);
        for b in 1..beats {
            let bx = x_of(bar * bar_ticks + b * beat_ticks);
            if bx > x + 24.0 {
                painter.text(
                    pos2(bx + 2.0, ruler_rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    format!("{}.{}", bar + 1, b + 1),
                    egui::FontId::proportional(9.0),
                    theme::TEXT_SECONDARY,
                );
            }
        }
        bar += 1;
    }
    // notes
    let selection = app.piano_roll.selection.clone();
    for n in &notes {
        let r = note_rect(n);
        if !r.intersects(grid_rect) {
            continue;
        }
        let r = r.intersect(grid_rect);
        painter.rect_filled(r.shrink(0.5), 2.0, velocity_color(n.velocity));
        if selection.contains(&n.id) {
            painter.rect_stroke(r, 2.0, Stroke::new(1.5, theme::TEXT), egui::StrokeKind::Inside);
        } else {
            painter.rect_stroke(r, 2.0, Stroke::new(1.0, theme::BACKGROUND), egui::StrokeKind::Inside);
        }
        if row_h >= 11.0 && r.width() > 28.0 {
            painter.text(r.left_center() + vec2(3.0, 0.0), egui::Align2::LEFT_CENTER, pitch_name(n.pitch), egui::FontId::proportional(9.0), theme::BACKGROUND);
        }
    }
    // velocity lane
    painter.rect_filled(vel_rect, 0.0, theme::PANEL.linear_multiply(0.7));
    painter.line_segment([vel_rect.left_top(), vel_rect.right_top()], Stroke::new(1.0, theme::BORDER));
    painter.text(vel_rect.left_top() + vec2(4.0, 2.0), egui::Align2::LEFT_TOP, "velocity", egui::FontId::proportional(9.0), theme::TEXT_SECONDARY);
    for n in &notes {
        let x = x_of(n.tick.0);
        if x < vel_rect.min.x || x > vel_rect.max.x {
            continue;
        }
        let h = (vel_rect.height() - 12.0) * n.velocity;
        let col = if selection.contains(&n.id) { theme::TEXT } else { velocity_color(n.velocity) };
        painter.line_segment([pos2(x, vel_rect.max.y), pos2(x, vel_rect.max.y - h)], Stroke::new(2.0, col));
        painter.circle_filled(pos2(x, vel_rect.max.y - h), 2.5, col);
    }
    // playhead
    if let Some(tick) = app.playhead_tick() {
        let local = if length.0 > 0 && app.transport_mode == rawdawg_audio_engine::TransportMode::Pattern { tick.rem_euclid(length.0) } else { tick };
        let x = x_of(local);
        if x >= grid_rect.min.x && x <= grid_rect.max.x {
            painter.line_segment([pos2(x, ruler_rect.min.y), pos2(x, vel_rect.max.y)], Stroke::new(1.5, theme::WARNING));
        }
    }

    // --- interaction ---
    let mods = ui.input(|i| i.modifiers);
    let pointer = resp.interact_pointer_pos().or_else(|| resp.hover_pos());
    let effective_grid = if mods.alt { None } else { grid };
    let hit = |p: Pos2| notes.iter().rev().find(|n| note_rect(n).contains(p)).cloned();
    app.piano_roll.hover_text.clear();
    if let Some(p) = resp.hover_pos()
        && grid_rect.contains(p)
        && let Some(n) = hit(p)
    {
        app.piano_roll.hover_text = format!("{} · {} · len {} · vel {:.2}", pitch_name(n.pitch), fmt_pos(n.tick.0, bar_ticks, beat_ticks), fmt_len(n.duration_ticks.0), n.velocity);
    }
    let mut cmds: Vec<Command> = Vec::new();

    // keyboard ruler audition
    if resp.drag_started()
        && let Some(p) = pointer
        && keys_rect.contains(p)
    {
        let pitch = pitch_at(p.y).clamp(0, 127) as u8;
        audition_on(app, pitch, 0.8);
    }

    // primary press in the grid
    if resp.drag_started()
        && resp.dragged_by(egui::PointerButton::Primary)
        && let Some(p) = pointer
        && grid_rect.contains(p)
    {
        resp.request_focus();
        let st = &mut app.piano_roll;
        let box_mode = st.tool == Tool::Select || mods.command;
        match hit(p) {
            Some(n) if !box_mode => {
                if mods.shift {
                    if !st.selection.remove(&n.id) {
                        st.selection.insert(n.id);
                    }
                } else if !st.selection.contains(&n.id) {
                    st.selection.clear();
                    st.selection.insert(n.id);
                }
                let sel: Vec<Note> = notes.iter().filter(|x| st.selection.contains(&x.id)).cloned().collect();
                let on_edge = p.x > note_rect(&n).max.x - 7.0 && note_rect(&n).width() > 10.0;
                st.gesture_target = Some((pid, cid));
                st.gesture = Some(if on_edge {
                    Gesture::Resize { orig: sel, anchor_tick: tick_at(p.x) }
                } else {
                    Gesture::Move { orig: sel, anchor_tick: tick_at(p.x), anchor_pitch: pitch_at(p.y), moved: false }
                });
                let (pitch, vel) = (n.pitch, n.velocity);
                audition_on(app, pitch, vel);
            }
            Some(_) | None if box_mode => {
                app.piano_roll.gesture = Some(Gesture::Box { start: p, additive: mods.shift });
                app.piano_roll.gesture_target = Some((pid, cid));
            }
            _ => {
                // insert note(s)
                let tick = snap_tick(tick_at(p.x), effective_grid).max(0);
                let mut root = pitch_at(p.y).clamp(0, 127);
                if st.scale_snap {
                    root = snap_to_scale(root, st.scale_root, st.scale_mode);
                }
                let len = st.last_len.max(1).min((length.0 - tick).max(1));
                if tick < length.0 {
                    let mut new_notes = Vec::new();
                    for iv in st.chord.intervals() {
                        let pitch = root + iv;
                        if (0..=127).contains(&pitch) {
                            new_notes.push(Note { id: NoteId::new(), tick: Tick(tick), duration_ticks: Tick(len), pitch: pitch as u8, velocity: 0.8, channel: 0 });
                        }
                    }
                    st.selection = new_notes.iter().map(|n| n.id).collect();
                    let pitches: Vec<u8> = new_notes.iter().map(|n| n.pitch).collect();
                    cmds.push(Command::AddNotes { pattern_id: pid, channel_id: cid, notes: new_notes.clone() });
                    // dragging right after insertion resizes the new notes
                    app.piano_roll.gesture_target = Some((pid, cid));
                    app.piano_roll.gesture = Some(Gesture::Resize { orig: new_notes, anchor_tick: tick });
                    for pitch in pitches {
                        audition_on(app, pitch, 0.8);
                    }
                }
            }
        }
    }
    // secondary press: delete / erase
    if resp.drag_started()
        && resp.dragged_by(egui::PointerButton::Secondary)
        && let Some(p) = pointer
        && grid_rect.contains(p)
    {
        app.piano_roll.gesture = Some(Gesture::Erase);
        app.piano_roll.gesture_target = Some((pid, cid));
        if let Some(n) = hit(p) {
            app.piano_roll.selection.remove(&n.id);
            cmds.push(Command::RemoveNotes { pattern_id: pid, channel_id: cid, note_ids: vec![n.id] });
        }
    }
    // velocity lane press
    if resp.drag_started()
        && resp.dragged_by(egui::PointerButton::Primary)
        && let Some(p) = pointer
        && vel_rect.contains(p)
    {
        let v = ((vel_rect.max.y - p.y) / (vel_rect.height() - 12.0)).clamp(0.01, 1.0);
        let orig: Vec<Note> =
            if app.piano_roll.selection.is_empty() { notes.clone() } else { notes.iter().filter(|n| app.piano_roll.selection.contains(&n.id)).cloned().collect() };
        app.piano_roll.gesture = Some(Gesture::Velocity { orig, start_tick: tick_at(p.x), start_vel: v });
        app.piano_roll.gesture_target = Some((pid, cid));
    }

    // drag updates
    if resp.dragged()
        && let Some(p) = pointer
    {
        let st = &mut app.piano_roll;
        match st.gesture.as_mut() {
            Some(Gesture::Move { orig, anchor_tick, anchor_pitch, moved }) => {
                let dt_raw = tick_at(p.x) - *anchor_tick;
                let dp = pitch_at(p.y) - *anchor_pitch;
                if dt_raw != 0 || dp != 0 {
                    *moved = true;
                }
                if *moved {
                    let first = orig.iter().map(|n| n.tick.0).min().unwrap_or(0);
                    let dt = snap_tick(first + dt_raw, effective_grid) - first;
                    let (ssnap, root, mode) = (st.scale_snap, st.scale_root, st.scale_mode);
                    let updated: Vec<Note> = orig
                        .iter()
                        .map(|n| {
                            let mut pitch = (n.pitch as i32 + dp).clamp(0, 127);
                            if ssnap && dp != 0 {
                                pitch = snap_to_scale(pitch, root, mode);
                            }
                            let tick = (n.tick.0 + dt).clamp(0, (length.0 - n.duration_ticks.0).max(0));
                            Note { tick: Tick(tick), pitch: pitch as u8, ..*n }
                        })
                        .collect();
                    let changed_pitch = updated.first().map(|n| n.pitch) != st.audition.first().map(|a| a.1);
                    if changed_pitch && let Some(n) = updated.first() {
                        let (pitch, vel) = (n.pitch, n.velocity);
                        audition_off(app);
                        audition_on(app, pitch, vel);
                    }
                    preview_update(app, pid, cid, updated);
                }
            }
            Some(Gesture::Resize { orig, anchor_tick }) => {
                let dt = tick_at(p.x) - *anchor_tick;
                let updated: Vec<Note> = orig
                    .iter()
                    .map(|n| {
                        let end = snap_round(n.end_tick().0 + dt, effective_grid).max(n.tick.0 + effective_grid.unwrap_or(PPQ / 32).max(1));
                        let end = end.min(length.0.max(n.tick.0 + 1));
                        Note { duration_ticks: Tick(end - n.tick.0), ..*n }
                    })
                    .collect();
                if let Some(n) = updated.first() {
                    app.piano_roll.last_len = n.duration_ticks.0;
                }
                preview_update(app, pid, cid, updated);
            }
            Some(Gesture::Box { start, .. }) => {
                let r = Rect::from_two_pos(*start, p);
                painter.rect(r, 0.0, theme::ACCENT.linear_multiply(0.15), Stroke::new(1.0, theme::ACCENT), egui::StrokeKind::Inside);
            }
            Some(Gesture::Erase) => {
                if grid_rect.contains(p)
                    && let Some(n) = hit(p)
                {
                    st.selection.remove(&n.id);
                    cmds.push(Command::RemoveNotes { pattern_id: pid, channel_id: cid, note_ids: vec![n.id] });
                }
            }
            Some(Gesture::Velocity { orig, start_tick, start_vel }) => {
                let v = ((vel_rect.max.y - p.y) / (vel_rect.height() - 12.0)).clamp(0.01, 1.0);
                let t1 = tick_at(p.x);
                let (t0, v0) = (*start_tick, *start_vel);
                let updated: Vec<Note> = orig
                    .iter()
                    .map(|n| {
                        let (lo, hi) = (t0.min(t1), t0.max(t1));
                        if lo == hi {
                            // single column: set the notes under the pointer (or all selected)
                            if orig.len() == 1 || (n.tick.0 - t0).abs() < (PPQ / 8).max((6.0 / ppt) as i64) { Note { velocity: v, ..*n } } else { *n }
                        } else if n.tick.0 >= lo && n.tick.0 <= hi {
                            let f = (n.tick.0 - t0) as f32 / (t1 - t0) as f32;
                            Note { velocity: (v0 + (v - v0) * f).clamp(0.01, 1.0), ..*n }
                        } else {
                            *n
                        }
                    })
                    .collect();
                preview_update(app, pid, cid, updated);
            }
            None => {}
        }
    }
    // release
    if resp.drag_stopped() || (app.piano_roll.gesture.is_some() && !resp.dragged() && !resp.drag_started()) {
        match app.piano_roll.gesture.take() {
            Some(Gesture::Box { start, additive }) => {
                if let Some(p) = pointer {
                    let r = Rect::from_two_pos(start, p);
                    let st = &mut app.piano_roll;
                    if !additive {
                        st.selection.clear();
                    }
                    for n in &notes {
                        if note_rect(n).intersects(r) {
                            st.selection.insert(n.id);
                        }
                    }
                }
            }
            Some(Gesture::Move { moved: false, .. }) => {
                // plain click on a note: selection only
                if app.history.gesture_active() {
                    let _ = app.history.cancel_gesture(&mut app.project);
                }
            }
            Some(_) => commit_gesture(app),
            None => {}
        }
        audition_off(app);
    }
    if !ui.input(|i| i.pointer.any_down()) && !app.piano_roll.audition.is_empty() {
        audition_off(app);
    }

    // keys (when the roll is hovered or focused)
    let focused = resp.has_focus();
    if (hovered || focused) && !ui.memory(|m| m.focused().is_some_and(|f| f != resp.id)) {
        let (del, dup, copy, cut, paste, all, left, right, up, down, esc) = ui.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::Delete) || i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace),
                i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::D)),
                i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::C)),
                i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::X)),
                i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::V)),
                i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::A)),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) || i.consume_key(egui::Modifiers::SHIFT, egui::Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) || i.consume_key(egui::Modifiers::SHIFT, egui::Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        });
        let shift = mods.shift;
        let selected: Vec<Note> = notes.iter().filter(|n| app.piano_roll.selection.contains(&n.id)).cloned().collect();
        if esc {
            cancel_gesture(app);
            app.piano_roll.selection.clear();
        }
        if all {
            app.piano_roll.selection = notes.iter().map(|n| n.id).collect();
        }
        if del && !selected.is_empty() {
            cmds.push(Command::RemoveNotes { pattern_id: pid, channel_id: cid, note_ids: selected.iter().map(|n| n.id).collect() });
            app.piano_roll.selection.clear();
        }
        if (copy || cut) && !selected.is_empty() {
            let first = selected.iter().map(|n| n.tick.0).min().unwrap_or(0);
            app.piano_roll.clipboard = selected.iter().map(|n| Note { tick: Tick(n.tick.0 - first), ..*n }).collect();
            if cut {
                cmds.push(Command::RemoveNotes { pattern_id: pid, channel_id: cid, note_ids: selected.iter().map(|n| n.id).collect() });
                app.piano_roll.selection.clear();
            }
            app.status_line = format!("{} note(s) copied", app.piano_roll.clipboard.len());
        }
        if paste && !app.piano_roll.clipboard.is_empty() {
            // relative to the playhead (snapped), else the view start
            let at = app.playhead_tick().map(|t| if length.0 > 0 { t.rem_euclid(length.0) } else { t }).unwrap_or(scroll_tick as i64);
            let at = snap_tick(at, grid).max(0);
            let pasted: Vec<Note> = app
                .piano_roll
                .clipboard
                .iter()
                .filter(|n| at + n.tick.0 < length.0)
                .map(|n| Note { id: NoteId::new(), tick: Tick(at + n.tick.0), duration_ticks: Tick(n.duration_ticks.0.min(length.0 - at - n.tick.0)), ..*n })
                .collect();
            app.piano_roll.selection = pasted.iter().map(|n| n.id).collect();
            if !pasted.is_empty() {
                cmds.push(Command::AddNotes { pattern_id: pid, channel_id: cid, notes: pasted });
            }
        }
        if dup && !selected.is_empty() {
            // duplicate after the selection, by its length rounded up to the grid
            let first = selected.iter().map(|n| n.tick.0).min().unwrap_or(0);
            let last = selected.iter().map(|n| n.end_tick().0).max().unwrap_or(0);
            let span = match grid {
                Some(g) => ((last - first + g - 1) / g).max(1) * g,
                None => last - first,
            };
            let dupes: Vec<Note> = selected
                .iter()
                .filter(|n| n.tick.0 + span < length.0)
                .map(|n| Note { id: NoteId::new(), tick: Tick(n.tick.0 + span), duration_ticks: Tick(n.duration_ticks.0.min(length.0 - n.tick.0 - span)), ..*n })
                .collect();
            if dupes.is_empty() {
                app.status_line = "Nothing fits after the selection; lengthen the pattern".into();
            } else {
                app.piano_roll.selection = dupes.iter().map(|n| n.id).collect();
                cmds.push(Command::AddNotes { pattern_id: pid, channel_id: cid, notes: dupes });
            }
        }
        if (left || right || up || down) && !selected.is_empty() {
            let dt = if left {
                -grid.unwrap_or(PPQ / 4)
            } else if right {
                grid.unwrap_or(PPQ / 4)
            } else {
                0
            };
            let dp = if up {
                if shift { 12 } else { 1 }
            } else if down {
                if shift { -12 } else { -1 }
            } else {
                0
            };
            let moved: Vec<Note> = selected
                .iter()
                .map(|n| Note { tick: Tick((n.tick.0 + dt).clamp(0, (length.0 - n.duration_ticks.0).max(0))), pitch: (n.pitch as i32 + dp).clamp(0, 127) as u8, ..*n })
                .collect();
            cmds.push(Command::UpdateNotes { pattern_id: pid, channel_id: cid, notes: moved });
        }
    }
    for c in cmds {
        app.apply(c);
    }
    if !app.piano_roll.hover_text.is_empty() {
        resp.clone().on_hover_text_at_pointer(app.piano_roll.hover_text.clone());
    }
    quantize_dialog(app, ui, pid, cid, grid.unwrap_or(PPQ / 4));
}

fn fmt_pos(tick: i64, bar_ticks: i64, beat_ticks: i64) -> String {
    let bar = tick.div_euclid(bar_ticks);
    let rem = tick.rem_euclid(bar_ticks);
    let beat = rem / beat_ticks;
    format!("{}.{}.{:03}", bar + 1, beat + 1, rem % beat_ticks)
}

fn fmt_len(ticks: i64) -> String {
    if ticks % (PPQ / 4) == 0 { format!("{}/16", ticks / (PPQ / 4)) } else { format!("{ticks}t") }
}

fn quantize_dialog(app: &mut App, ui: &mut egui::Ui, pid: PatternId, cid: ChannelId, grid: i64) {
    let Some(mut q) = app.piano_roll.quantize.take() else { return };
    let mut open = true;
    let mut apply = false;
    let mut cancel = false;
    egui::Window::new("Quantize").collapsible(false).resizable(false).open(&mut open).show(ui.ctx(), |ui| {
        ui.label(format!("Grid: {} · {}", app.piano_roll.snap.label(), if app.piano_roll.selection.is_empty() { "all notes in the lane" } else { "selected notes" }));
        let changed = ui.add(egui::Slider::new(&mut q.strength, 0.0..=1.0).text("Strength").custom_formatter(|v, _| format!("{:.0} %", v * 100.0))).changed();
        let toggled = ui.checkbox(&mut q.preview, "Preview (hear it before applying)").changed();
        ui.horizontal(|ui| {
            if ui.button("Apply").clicked() {
                apply = true;
            }
            if ui.button("Cancel").clicked() {
                cancel = true;
            }
        });
        if (changed && q.preview) || (toggled && q.preview) {
            q.previewing = true;
        }
        if toggled && !q.preview {
            q.previewing = false;
        }
    });
    let target_notes = |app: &App| -> Vec<Note> {
        let notes = app.project.pattern(pid).and_then(|p| p.lanes.iter().find(|l| l.channel_id == cid)).map(|l| l.notes.clone()).unwrap_or_default();
        if app.piano_roll.selection.is_empty() { notes } else { notes.into_iter().filter(|n| app.piano_roll.selection.contains(&n.id)).collect() }
    };
    if q.previewing {
        // preview through the history gesture so the raw notes come back on cancel
        if !app.history.gesture_active() {
            let _ = app.history.begin_gesture();
            let base = target_notes(app);
            app.piano_roll.clipboard_preview = base;
        }
        let base = app.piano_roll.clipboard_preview.clone();
        let quantized = quantize_notes(&base, grid, q.strength);
        let _ = app.history.preview(&mut app.project, Command::UpdateNotes { pattern_id: pid, channel_id: cid, notes: quantized });
        app.recompile();
    } else if app.history.gesture_active() && !app.piano_roll.gesture_active() {
        let _ = app.history.cancel_gesture(&mut app.project);
        app.recompile();
    }
    if apply {
        if app.history.gesture_active() {
            let _ = app.history.cancel_gesture(&mut app.project);
        }
        let base = target_notes(app);
        let quantized = quantize_notes(&base, grid, q.strength);
        app.apply(Command::UpdateNotes { pattern_id: pid, channel_id: cid, notes: quantized });
        app.status_line = format!("Quantized {} note(s) at {:.0} %", base.len(), q.strength * 100.0);
    } else if cancel || !open {
        if app.history.gesture_active() && !app.piano_roll.gesture_active() {
            let _ = app.history.cancel_gesture(&mut app.project);
            app.recompile();
        }
    } else {
        app.piano_roll.quantize = Some(q);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapping_rounds_as_documented() {
        assert_eq!(snap_tick(250, Some(240)), 240);
        assert_eq!(snap_tick(-10, Some(240)), -240);
        assert_eq!(snap_round(250, Some(240)), 240);
        assert_eq!(snap_round(370, Some(240)), 480);
        assert_eq!(snap_round(370, None), 370);
        assert_eq!(Snap::Sixteenth.ticks(3840, 960), Some(240));
        assert_eq!(Snap::EighthT.ticks(3840, 960), Some(320));
        assert_eq!(Snap::Bar.ticks(2880, 960), Some(2880));
        assert_eq!(Snap::Off.ticks(3840, 960), None);
    }

    #[test]
    fn quantize_strength_moves_proportionally_and_keeps_order() {
        let n = |tick: i64| Note { id: NoteId::new(), tick: Tick(tick), duration_ticks: Tick(100), pitch: 60, velocity: 0.8, channel: 0 };
        let notes = vec![n(250), n(470), n(1000)];
        let full = quantize_notes(&notes, 240, 1.0);
        assert_eq!(full.iter().map(|x| x.tick.0).collect::<Vec<_>>(), vec![240, 480, 960]);
        let half = quantize_notes(&notes, 240, 0.5);
        assert_eq!(half.iter().map(|x| x.tick.0).collect::<Vec<_>>(), vec![245, 475, 980]);
        let none = quantize_notes(&notes, 240, 0.0);
        assert_eq!(none.iter().map(|x| x.tick.0).collect::<Vec<_>>(), vec![250, 470, 1000]);
        assert_eq!(full[0].id, notes[0].id, "ids survive quantization (undo exact)");
    }

    #[test]
    fn scale_helpers_and_chords() {
        assert!(in_scale(64, 0, ScaleMode::Major)); // E in C major
        assert!(!in_scale(63, 0, ScaleMode::Major)); // D#
        assert!(in_scale(63, 0, ScaleMode::NaturalMinor));
        assert_eq!(snap_to_scale(63, 0, ScaleMode::Major), 62, "nearest degree below wins ties");
        assert_eq!(snap_to_scale(61, 0, ScaleMode::PentatonicMajor), 60);
        assert_eq!(Chord::Major.intervals(), &[0, 4, 7]);
        assert_eq!(Chord::MinorSeventh.intervals().len(), 4);
        assert_eq!(pitch_name(60), "C4");
        assert_eq!(pitch_name(69), "A4");
        assert_eq!(pitch_name(61), "C#4");
    }
}

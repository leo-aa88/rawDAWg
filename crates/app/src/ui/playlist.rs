//! Playlist / arrangement (CORE-04, CORE-01 maps): tracks with reusable pattern
//! clips, paint/move/copy/split/trim/resize/loop, snap shared with the piano
//! roll, time selection → loop range, tempo and time-signature markers, song
//! playhead and click-to-seek. Audio clips are shown but play back in M5.

use super::piano_roll::Snap;
use super::{App, EngineCommand, PointerState, TransportMode};
use crate::theme;
use egui::{Color32, Pos2, Rect, Sense, Stroke, pos2, vec2};
use rawdawg_audio_engine::AssetResolver;
use rawdawg_project::{Clip, ClipContent, Command, LoopRange, MeterPoint, Pattern, TempoPoint, Track};
use rawdawg_types::units::{PPQ, Tick, UsPerQuarter};
use rawdawg_types::{AssetId, ClipId, PatternId, TrackId};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const HEADER_W: f32 = 150.0;
const RULER_H: f32 = 22.0;
const MARKER_H: f32 = 18.0;
const TRACK_H: f32 = 56.0;

enum Gesture {
    Move { orig: Vec<Clip>, anchor_tick: i64, anchor_row: i32, moved: bool, copy: bool },
    ResizeEnd { orig: Vec<Clip>, anchor_tick: i64 },
    TrimStart { orig: Vec<Clip>, anchor_tick: i64 },
    Paint { track: TrackId, pattern: PatternId, last_tick: i64 },
    Erase,
    Box { start: Pos2 },
    LoopRange { start_tick: i64 },
}

/// Screen position of (tick, track row) inside the lanes for the current view (tests).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn lane_point(st: &PlaylistState, full: Rect, tick: i64, row: usize) -> Pos2 {
    pos2(full.min.x + HEADER_W + (tick as f32 - st.scroll_tick) * st.px_per_tick, full.min.y + RULER_H + MARKER_H + (row as f32 + 0.5) * TRACK_H)
}

pub(crate) struct PlaylistState {
    scroll_tick: f32,
    px_per_tick: f32,
    pub snap: Snap,
    pub selection: HashSet<ClipId>,
    gesture: Option<Gesture>,
    /// Marker editor: (kind, index) with its working values.
    marker_edit: Option<MarkerEdit>,
    rename: Option<(TrackId, String)>,
    pub hover: String,
    /// Audio asset armed in the browser: the next click in a lane places it as a clip.
    pub place_asset: Option<AssetId>,
    /// Where a file dragged over the lanes would land (track, tick), refreshed every frame.
    pub drop_target: Option<(TrackId, i64)>,
    /// Waveform thumbnails per asset: 512 (min, max) buckets over the whole asset.
    thumbs: HashMap<AssetId, Arc<Vec<(f32, f32)>>>,
}

#[derive(Clone)]
struct MarkerEdit {
    tempo: bool,
    index: usize,
    bpm: f64,
    numerator: u8,
    denominator: u8,
    tick: i64,
}

impl Default for PlaylistState {
    fn default() -> Self {
        Self {
            scroll_tick: 0.0,
            px_per_tick: 40.0 / PPQ as f32,
            snap: Snap::Beat,
            selection: HashSet::new(),
            gesture: None,
            marker_edit: None,
            rename: None,
            hover: String::new(),
            place_asset: None,
            drop_target: None,
            thumbs: HashMap::new(),
        }
    }
}

impl PlaylistState {
    pub fn gesture_active(&self) -> bool {
        self.gesture.is_some()
    }
}

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

fn track_color(t: &Track, i: usize) -> Color32 {
    match t.color {
        Some([r, g, b]) => Color32::from_rgb(r, g, b),
        None => {
            let palette =
                [theme::ACCENT, Color32::from_rgb(0x7F, 0xA8, 0xE6), Color32::from_rgb(0xE6, 0xC9, 0x6E), Color32::from_rgb(0xD9, 0x8A, 0xE0), Color32::from_rgb(0xE6, 0x9A, 0x7D)];
            palette[i % palette.len()]
        }
    }
}

pub(crate) fn cancel_gesture(app: &mut App) {
    if app.playlist.gesture.take().is_some() && app.history.gesture_active() {
        let _ = app.history.cancel_gesture(&mut app.project);
        app.recompile();
    }
}

/// Source frames per output frame of an audio clip (its asset's rate against the project's).
fn audio_rate(app: &App, asset_id: &AssetId, playback_rate: f64) -> f64 {
    let asset_rate = app.project.assets.iter().find(|a| &a.id == asset_id).and_then(|a| a.original_rate_hz).unwrap_or(app.project.sample_rate_hz.0);
    (playback_rate * f64::from(asset_rate) / app.project.sample_rate_hz.as_f64()).max(1e-6)
}

/// Frames of a project asset (from its metadata, else from the decoded sample).
fn asset_frames(app: &App, asset_id: &AssetId) -> Option<u64> {
    app.project.assets.iter().find(|a| &a.id == asset_id).and_then(|a| a.frames).filter(|f| *f > 0).or_else(|| app.assets.sample(asset_id).map(|s| s.frames() as u64))
}

/// Ticks covered by `out_frames` project frames starting at `start_tick` under the tempo map.
fn frames_to_ticks(app: &App, start_tick: i64, out_frames: f64) -> i64 {
    let rate = app.project.sample_rate_hz;
    let Ok(tempo) = app.project.tempo_map() else { return (out_frames / rate.as_f64() * 2.0 * PPQ as f64) as i64 };
    let start = tempo.tick_to_frame(Tick(start_tick), rate);
    let end = tempo.frame_to_tick_f64(start + out_frames.round() as i64, rate);
    ((end - start_tick as f64).round() as i64).max(1)
}

/// Project frames covered by `ticks` ticks starting at `start_tick`.
fn ticks_to_frames(app: &App, start_tick: i64, ticks: i64) -> f64 {
    let rate = app.project.sample_rate_hz;
    let Ok(tempo) = app.project.tempo_map() else { return ticks as f64 / (2.0 * PPQ as f64) * rate.as_f64() };
    (tempo.tick_to_frame(Tick(start_tick + ticks), rate) - tempo.tick_to_frame(Tick(start_tick), rate)) as f64
}

/// Build an audio clip of a whole asset starting at `tick` on `track`; `None` when the
/// asset cannot be measured.
pub(crate) fn audio_clip_for(app: &App, asset_id: &AssetId, track: TrackId, tick: i64) -> Option<Clip> {
    let frames = asset_frames(app, asset_id)?;
    let rate = audio_rate(app, asset_id, 1.0);
    let out_frames = frames as f64 / rate;
    Some(Clip {
        id: ClipId::new(),
        track_id: track,
        start_tick: Tick(tick),
        length_ticks: Tick(frames_to_ticks(app, tick, out_frames)),
        muted: false,
        content: ClipContent::Audio {
            asset_id: asset_id.clone(),
            source_offset_frames: 0,
            source_length_frames: frames,
            gain_db: rawdawg_types::units::Db::UNITY,
            fade_in_frames: 0,
            fade_out_frames: 0,
            playback_rate: 1.0,
        },
    })
}

/// Place an asset as an audio clip (one undo step) and select it.
pub(crate) fn place_audio_clip(app: &mut App, asset_id: &AssetId, track: TrackId, tick: i64) -> Option<ClipId> {
    let clip = audio_clip_for(app, asset_id, track, tick)?;
    let id = clip.id;
    app.apply(Command::AddClip { clip });
    app.playlist.selection.clear();
    app.playlist.selection.insert(id);
    Some(id)
}

/// Re-derive an audio clip's source bounds from its (edited) tick bounds: the end edge sets
/// the source length, the start edge shifts the source offset. Pattern/automation clips are
/// returned unchanged.
fn audio_bounds_from_ticks(app: &App, orig: &Clip, edited: Clip) -> Clip {
    let ClipContent::Audio { asset_id, source_offset_frames, source_length_frames, gain_db, fade_in_frames, fade_out_frames, playback_rate } = &orig.content else { return edited };
    let rate = audio_rate(app, asset_id, *playback_rate);
    let total = asset_frames(app, asset_id).unwrap_or(*source_offset_frames + *source_length_frames);
    let shift_src = (ticks_to_frames(app, orig.start_tick.0, edited.start_tick.0 - orig.start_tick.0) * rate).round();
    let offset = ((*source_offset_frames as f64 + shift_src).max(0.0) as u64).min(total.saturating_sub(1));
    let len_src = (ticks_to_frames(app, edited.start_tick.0, edited.length_ticks.0) * rate).round().max(1.0) as u64;
    let length = len_src.min(total - offset).max(1);
    // the displayed length follows the source bounds exactly
    let length_ticks = frames_to_ticks(app, edited.start_tick.0, length as f64 / rate);
    Clip {
        length_ticks: Tick(length_ticks),
        content: ClipContent::Audio {
            asset_id: asset_id.clone(),
            source_offset_frames: offset,
            source_length_frames: length,
            gain_db: *gain_db,
            fade_in_frames: (*fade_in_frames).min(length),
            fade_out_frames: (*fade_out_frames).min(length),
            playback_rate: *playback_rate,
        },
        ..edited
    }
}

/// Waveform thumbnail of an asset (cached; decodes on first use).
fn thumbnail(app: &mut App, asset_id: &AssetId) -> Option<Arc<Vec<(f32, f32)>>> {
    if let Some(t) = app.playlist.thumbs.get(asset_id) {
        return Some(t.clone());
    }
    let sample = app.assets.sample(asset_id)?;
    let n = sample.frames();
    if n == 0 {
        return None;
    }
    const BUCKETS: usize = 512;
    let mut out = vec![(0.0f32, 0.0f32); BUCKETS];
    for (b, slot) in out.iter_mut().enumerate() {
        let (from, to) = (b * n / BUCKETS, ((b + 1) * n / BUCKETS).max(b * n / BUCKETS + 1).min(n));
        let (mut lo, mut hi) = (0.0f32, 0.0f32);
        for ch in &sample.channels {
            for v in &ch[from..to] {
                lo = lo.min(*v);
                hi = hi.max(*v);
            }
        }
        *slot = (lo, hi);
    }
    let t = Arc::new(out);
    app.playlist.thumbs.insert(asset_id.clone(), t.clone());
    Some(t)
}

fn preview_clips(app: &mut App, clips: Vec<Clip>) {
    if !app.history.gesture_active() {
        let _ = app.history.begin_gesture();
    }
    for c in clips {
        if let Err(e) = app.history.preview(&mut app.project, Command::UpdateClip { clip: c }) {
            app.status_line = format!("Edit rejected: {e}");
        }
    }
}

fn commit(app: &mut App) {
    if app.history.gesture_active() {
        let _ = app.history.commit_gesture();
        app.recompile();
    }
}

/// Clone a pattern under new ids (Make unique).
fn clone_pattern(p: &Pattern, n: usize) -> Pattern {
    p.duplicate(format!("{} copy {n}", p.name))
}

pub(crate) fn show(app: &mut App, ui: &mut egui::Ui) {
    let mut cmds: Vec<Command> = Vec::new();
    ui.horizontal_wrapped(|ui| {
        ui.heading("Playlist");
        if ui.button("+ Track").clicked() {
            let n = app.project.tracks.len() + 1;
            cmds.push(Command::AddTrack {
                track: Track { id: TrackId::new(), name: format!("Track {n}"), color: None, mute: false, solo: false, height: None, mixer_id: None },
                index: None,
            });
        }
        ui.separator();
        ui.label("Snap");
        egui::ComboBox::from_id_salt("pl-snap").selected_text(app.playlist.snap.label()).show_ui(ui, |ui| {
            for s in Snap::ALL {
                ui.selectable_value(&mut app.playlist.snap, s, s.label());
            }
        });
        ui.separator();
        let sel = app.playlist.selection.len();
        if ui.add_enabled(sel > 0, egui::Button::new("Split at playhead")).on_hover_text("S").clicked() {
            split_selected(app, &mut cmds);
        }
        if ui.add_enabled(sel > 0, egui::Button::new("Make unique")).on_hover_text("Clone the pattern of the selected clip(s) so edits no longer affect other clips").clicked() {
            make_unique(app, &mut cmds);
        }
        if ui.add_enabled(sel > 0, egui::Button::new("Delete")).clicked() {
            for id in app.playlist.selection.drain() {
                cmds.push(Command::RemoveClip { id });
            }
        }
        ui.separator();
        let lp = app.project.r#loop;
        match lp {
            Some(l) => {
                ui.label(
                    egui::RichText::new(format!(
                        "Loop {}–{}{}",
                        fmt_bar(l.start_tick.0, bar_ticks(app)),
                        fmt_bar(l.end_tick.0, bar_ticks(app)),
                        if l.enabled { "" } else { " (off)" }
                    ))
                    .color(theme::TEXT_SECONDARY),
                );
                if ui.small_button("clear").clicked() {
                    cmds.push(Command::SetLoop { range: None });
                }
            }
            None => {
                ui.label(egui::RichText::new("Drag on the ruler to set a loop / export range").color(theme::TEXT_SECONDARY));
            }
        }
        ui.label(
            egui::RichText::new(
                "Click in a track: paint the selected pattern · drag clips to move (Ctrl copies) · edges trim/resize · right-click: delete · double-click track name: rename",
            )
            .color(theme::TEXT_SECONDARY)
            .small(),
        );
    });
    let grid = app.playlist.snap.ticks(bar_ticks(app), PPQ);
    let avail = ui.available_size();
    let tracks = app.project.tracks.clone();
    let total_h = (RULER_H + MARKER_H + TRACK_H * tracks.len().max(1) as f32 + 8.0).max(avail.y.min(600.0)).max(160.0);
    let (full, _) = ui.allocate_exact_size(vec2(avail.x.max(300.0), total_h), Sense::hover());
    let resp = ui.interact(full, egui::Id::unique("playlist-surface"), Sense::click_and_drag());
    let painter = ui.painter_at(full);
    painter.rect_filled(full, 3.0, theme::BACKGROUND);
    let lanes_rect = Rect::from_min_max(pos2(full.min.x + HEADER_W, full.min.y + RULER_H + MARKER_H), full.max);
    let ruler_rect = Rect::from_min_max(pos2(lanes_rect.min.x, full.min.y), pos2(full.max.x, full.min.y + RULER_H));
    let marker_rect = Rect::from_min_max(pos2(lanes_rect.min.x, ruler_rect.max.y), pos2(full.max.x, lanes_rect.min.y));

    // scroll / zoom
    if ui.rect_contains_pointer(full) {
        let (scroll, zoom, mods, ptr) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.modifiers, i.pointer.hover_pos()));
        let st = &mut app.playlist;
        if zoom != 1.0 {
            if let Some(p) = ptr {
                let tick_at = st.scroll_tick + (p.x - lanes_rect.min.x) / st.px_per_tick;
                st.px_per_tick = (st.px_per_tick * zoom).clamp(2.0 / PPQ as f32, 400.0 / PPQ as f32);
                st.scroll_tick = (tick_at - (p.x - lanes_rect.min.x) / st.px_per_tick).max(0.0);
            }
        } else if scroll != egui::Vec2::ZERO && !mods.command {
            let dx = if mods.shift { scroll.y } else { scroll.x + if scroll.x == 0.0 { scroll.y } else { 0.0 } };
            st.scroll_tick = (st.scroll_tick - dx / st.px_per_tick).max(0.0);
        }
    }
    let (scroll_tick, ppt) = (app.playlist.scroll_tick, app.playlist.px_per_tick);
    let x_of = |tick: i64| lanes_rect.min.x + (tick as f32 - scroll_tick) * ppt;
    let tick_at = |x: f32| (scroll_tick + (x - lanes_rect.min.x) / ppt).floor() as i64;
    let row_of = |y: f32| ((y - lanes_rect.min.y) / TRACK_H).floor() as i32;
    let row_rect =
        |i: usize| Rect::from_min_max(pos2(lanes_rect.min.x, lanes_rect.min.y + i as f32 * TRACK_H), pos2(lanes_rect.max.x, lanes_rect.min.y + (i + 1) as f32 * TRACK_H));
    let clip_rect = |c: &Clip| {
        let row = tracks.iter().position(|t| t.id == c.track_id).unwrap_or(0);
        let r = row_rect(row);
        Rect::from_min_max(pos2(x_of(c.start_tick.0), r.min.y + 3.0), pos2(x_of(c.end_tick().0).max(x_of(c.start_tick.0) + 4.0), r.max.y - 3.0))
    };
    let bar_t = bar_ticks(app);
    let last_tick = tick_at(full.max.x) + 1;

    // ruler with bars
    painter.rect_filled(ruler_rect, 0.0, theme::PANEL);
    let mut bar = (scroll_tick as i64) / bar_t;
    let label_every = ((40.0 / (bar_t as f32 * ppt)).ceil() as i64).max(1);
    while bar * bar_t <= last_tick {
        let x = x_of(bar * bar_t);
        painter.line_segment([pos2(x, ruler_rect.min.y + 12.0), pos2(x, ruler_rect.max.y)], Stroke::new(1.0, theme::BORDER));
        if bar % label_every == 0 {
            painter.text(pos2(x + 3.0, ruler_rect.min.y + 2.0), egui::Align2::LEFT_TOP, format!("{}", bar + 1), egui::FontId::proportional(11.0), theme::TEXT);
        }
        bar += 1;
    }
    // loop range
    if let Some(l) = app.project.r#loop {
        let r = Rect::from_min_max(pos2(x_of(l.start_tick.0), ruler_rect.min.y), pos2(x_of(l.end_tick.0), ruler_rect.max.y)).intersect(ruler_rect);
        painter.rect_filled(r, 0.0, if l.enabled { theme::ACCENT.linear_multiply(0.35) } else { theme::TEXT_SECONDARY.linear_multiply(0.2) });
    }
    // marker row: tempo (top half) and meter markers
    painter.rect_filled(marker_rect, 0.0, theme::PANEL.linear_multiply(0.8));
    let mut marker_hit: Option<(bool, usize)> = None;
    for (i, t) in app.project.tempo.iter().enumerate() {
        let x = x_of(t.tick.0);
        if x < marker_rect.min.x - 60.0 || x > marker_rect.max.x {
            continue;
        }
        let label = format!("{:.1} BPM", t.us_per_quarter.bpm().0);
        let w = label.len() as f32 * 6.0 + 8.0;
        let r = Rect::from_min_size(pos2(x, marker_rect.min.y + 1.0), vec2(w, MARKER_H - 2.0));
        let mr = ui.interact(r, ui.scope_id().with(("tempo-marker", i)), Sense::click());
        painter.rect(r, 2.0, theme::WARNING.linear_multiply(if mr.hovered() { 0.5 } else { 0.3 }), Stroke::new(1.0, theme::WARNING), egui::StrokeKind::Inside);
        painter.text(r.left_center() + vec2(4.0, 0.0), egui::Align2::LEFT_CENTER, label, egui::FontId::proportional(10.0), theme::TEXT);
        if mr.clicked() {
            marker_hit = Some((true, i));
        }
        mr.on_hover_text("Tempo change: click to edit");
    }
    for (i, m) in app.project.meter.iter().enumerate() {
        let x = x_of(m.tick.0);
        if x < marker_rect.min.x - 60.0 || x > marker_rect.max.x {
            continue;
        }
        let label = format!("{}/{}", m.numerator, m.denominator);
        let r = Rect::from_min_size(pos2(x + 2.0, marker_rect.min.y + 1.0), vec2(30.0, MARKER_H - 2.0));
        // put meter markers after any tempo label at the same tick
        let shift = app.project.tempo.iter().any(|t| t.tick == m.tick) as i32 as f32 * 70.0;
        let r = r.translate(vec2(shift, 0.0));
        let mr = ui.interact(r, ui.scope_id().with(("meter-marker", i)), Sense::click());
        painter.rect(
            r,
            2.0,
            Color32::from_rgb(0x7F, 0xA8, 0xE6).linear_multiply(if mr.hovered() { 0.5 } else { 0.3 }),
            Stroke::new(1.0, Color32::from_rgb(0x7F, 0xA8, 0xE6)),
            egui::StrokeKind::Inside,
        );
        painter.text(r.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(10.0), theme::TEXT);
        if mr.clicked() {
            marker_hit = Some((false, i));
        }
        mr.on_hover_text("Time signature: click to edit");
    }
    if let Some((tempo, index)) = marker_hit {
        let (bpm, numerator, denominator, tick) = if tempo {
            let t = &app.project.tempo[index];
            (t.us_per_quarter.bpm().0, 4, 4, t.tick.0)
        } else {
            let m = &app.project.meter[index];
            (120.0, m.numerator, m.denominator, m.tick.0)
        };
        app.playlist.marker_edit = Some(MarkerEdit { tempo, index, bpm, numerator, denominator, tick });
    }
    // marker row right-click: add markers at the pointer
    let marker_resp = ui.interact(marker_rect, ui.scope_id().with("marker-row"), Sense::click());
    marker_resp.context_menu(|ui| {
        let at = ui.ctx().pointer_latest_pos().map(|p| snap_round(tick_at(p.x), Some(bar_t)).max(0)).unwrap_or(0);
        ui.label(format!("At bar {}", at / bar_t + 1));
        if ui.button("Add tempo change here").clicked() {
            let mut tempo = app.project.tempo.clone();
            let current = app.project.tempo_map().map(|m| m.tempo_at(Tick(at))).unwrap_or(UsPerQuarter(500_000));
            tempo.retain(|t| t.tick.0 != at);
            tempo.push(TempoPoint { tick: Tick(at), us_per_quarter: current });
            tempo.sort_by_key(|t| t.tick.0);
            cmds.push(Command::SetTempoMap { tempo });
            ui.close();
        }
        if ui.button("Add time signature here").clicked() {
            let mut meter = app.project.meter.clone();
            let cur = app.project.meter.iter().rev().find(|m| m.tick.0 <= at).map(|m| (m.numerator, m.denominator)).unwrap_or((4, 4));
            meter.retain(|m| m.tick.0 != at);
            meter.push(MeterPoint { tick: Tick(at), numerator: cur.0, denominator: cur.1 });
            meter.sort_by_key(|m| m.tick.0);
            cmds.push(Command::SetMeterMap { meter });
            ui.close();
        }
    });
    marker_resp.on_hover_text("Tempo / time signature markers (right-click to add)");

    // track headers + lanes
    let mut rename_commit: Option<(TrackId, String)> = None;
    for (i, t) in tracks.iter().enumerate() {
        let r = row_rect(i);
        if r.min.y > full.max.y {
            break;
        }
        let header = Rect::from_min_max(pos2(full.min.x, r.min.y), pos2(lanes_rect.min.x - 2.0, r.max.y));
        painter.rect_filled(header, 0.0, theme::PANEL);
        painter.rect_filled(r, 0.0, if i % 2 == 0 { theme::BACKGROUND } else { theme::PANEL.linear_multiply(0.5) });
        painter.line_segment([r.left_bottom(), r.right_bottom()], Stroke::new(1.0, theme::BORDER));
        painter.rect_filled(Rect::from_min_size(header.min, vec2(4.0, header.height())), 0.0, track_color(t, i));
        // name (double-click renames)
        let name_rect = Rect::from_min_max(pos2(header.min.x + 8.0, header.min.y + 4.0), pos2(header.max.x - 4.0, header.min.y + 22.0));
        let editing = app.playlist.rename.as_ref().is_some_and(|(id, _)| *id == t.id);
        if editing {
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(name_rect));
            if let Some((_, name)) = app.playlist.rename.as_mut() {
                let te = child.add(egui::TextEdit::singleline(name).desired_width(name_rect.width()));
                te.request_focus();
                if te.lost_focus() {
                    let n = name.trim().to_owned();
                    if !n.is_empty() && n != t.name {
                        rename_commit = Some((t.id, n));
                    }
                    app.playlist.rename = None;
                }
            }
        } else {
            painter.text(name_rect.left_center(), egui::Align2::LEFT_CENTER, &t.name, egui::FontId::proportional(12.0), theme::TEXT);
            let nr = ui.interact(name_rect, ui.scope_id().with(("track-name", t.id)), Sense::click());
            if nr.double_clicked() {
                app.playlist.rename = Some((t.id, t.name.clone()));
            }
            nr.on_hover_text("Double-click to rename");
        }
        // mute / solo / delete
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_max(pos2(header.min.x + 8.0, header.min.y + 26.0), pos2(header.max.x - 4.0, header.max.y - 2.0)))
                .layout(egui::Layout::left_to_right(egui::Align::Min)),
        );
        child.spacing_mut().item_spacing = vec2(3.0, 0.0);
        let mut mute = t.mute;
        if child.toggle_value(&mut mute, "M").on_hover_text("Mute track").changed() {
            cmds.push(Command::SetTrackMute { id: t.id, mute });
        }
        let mut solo = t.solo;
        if child.toggle_value(&mut solo, "S").on_hover_text("Solo track").changed() {
            cmds.push(Command::SetTrackSolo { id: t.id, solo });
        }
        if child.small_button("×").on_hover_text("Delete track and its clips (undoable)").clicked() {
            let clips: Vec<Command> = app.project.clips.iter().filter(|c| c.track_id == t.id).map(|c| Command::RemoveClip { id: c.id }).collect();
            let mut all = clips;
            all.push(Command::RemoveTrack { id: t.id });
            cmds.push(Command::Batch { commands: all });
        }
        {
            let mixers = app.project.mixer.channels.clone();
            let master = app.project.mixer.master_id;
            let current = t.mixer_id.unwrap_or(master);
            let cur_name = mixers.iter().find(|m| m.id == current).map(|m| m.name.clone()).unwrap_or_else(|| "Master".into());
            let short: String = cur_name.chars().take(6).collect();
            egui::ComboBox::from_id_salt(("track-mixer", t.id)).selected_text(short).width(52.0).show_ui(&mut child, |ui| {
                for m in &mixers {
                    if ui.selectable_label(m.id == current, &m.name).on_hover_text("Audio clips on this track play into this mixer channel").clicked() {
                        cmds.push(Command::SetTrackMixer { id: t.id, mixer_id: if m.id == master { None } else { Some(m.id) } });
                    }
                }
            });
        }
        // grid lines
        let mut b = (scroll_tick as i64) / bar_t;
        while b * bar_t <= last_tick {
            let x = x_of(b * bar_t);
            painter.line_segment([pos2(x, r.min.y), pos2(x, r.max.y)], Stroke::new(1.0, theme::BORDER.linear_multiply(0.6)));
            if bar_t as f32 * ppt > 60.0 {
                for q in 1..(bar_t / PPQ).max(1) {
                    let qx = x_of(b * bar_t + q * PPQ);
                    painter.line_segment([pos2(qx, r.min.y), pos2(qx, r.max.y)], Stroke::new(1.0, theme::BORDER.linear_multiply(0.25)));
                }
            }
            b += 1;
        }
    }
    if let Some((id, name)) = rename_commit {
        cmds.push(Command::RenameTrack { id, name });
    }
    if tracks.is_empty() {
        painter.text(
            lanes_rect.min + vec2(12.0, 12.0),
            egui::Align2::LEFT_TOP,
            "No tracks yet — press + Track, then click in the lane to place the selected pattern.",
            egui::FontId::proportional(13.0),
            theme::TEXT_SECONDARY,
        );
    }
    // clips
    let clips = app.project.clips.clone();
    let selection = app.playlist.selection.clone();
    for c in &clips {
        let Some(row) = tracks.iter().position(|t| t.id == c.track_id) else { continue };
        let cr = clip_rect(c);
        if !cr.intersects(lanes_rect) {
            continue;
        }
        let col = track_color(&tracks[row], row);
        let cr_vis = cr.intersect(lanes_rect);
        let muted = c.muted;
        painter.rect_filled(cr_vis, 3.0, if muted { col.linear_multiply(0.25) } else { col.linear_multiply(0.55) });
        painter.rect_stroke(
            cr_vis,
            3.0,
            Stroke::new(if selection.contains(&c.id) { 2.0 } else { 1.0 }, if selection.contains(&c.id) { theme::TEXT } else { col }),
            egui::StrokeKind::Inside,
        );
        let th = if let ClipContent::Audio { asset_id, .. } = &c.content { thumbnail(app, asset_id) } else { None };
        let (name, mini): (String, Option<&Pattern>) = match &c.content {
            ClipContent::Pattern { pattern_id, looped, .. } => {
                let p = app.project.pattern(*pattern_id);
                (format!("{}{}", p.map(|p| p.name.clone()).unwrap_or_else(|| "?".into()), if *looped { " (loop)" } else { "" }), p)
            }
            ClipContent::Audio { asset_id, .. } => {
                (app.project.assets.iter().find(|a| &a.id == asset_id).map(|a| a.source_name.clone()).unwrap_or_else(|| "audio (missing asset)".into()), None)
            }
            ClipContent::Automation { curve_id, .. } => {
                (app.project.automation.iter().find(|a| a.id == *curve_id).map(|a| format!("~ {}", a.name)).unwrap_or_else(|| "automation".into()), None)
            }
        };
        painter.text(cr_vis.left_top() + vec2(4.0, 2.0), egui::Align2::LEFT_TOP, name, egui::FontId::proportional(10.0), theme::TEXT);
        // waveform thumbnail (source frames mapped onto the clip's ticks)
        if let ClipContent::Audio { asset_id, source_offset_frames, source_length_frames, playback_rate, gain_db, .. } = &c.content
            && let Some(th) = th
        {
            let total = asset_frames(app, asset_id).unwrap_or(*source_offset_frames + *source_length_frames).max(1) as f32;
            let area = Rect::from_min_max(pos2(cr_vis.min.x, cr.min.y + 14.0), pos2(cr_vis.max.x, cr.max.y - 2.0));
            let mid = area.center().y;
            let half = area.height() * 0.5;
            let rate = audio_rate(app, asset_id, *playback_rate) as f32;
            let out_frames = *source_length_frames as f32 / rate;
            let g = gain_db.to_linear() as f32;
            let mut x = area.min.x;
            while x < area.max.x {
                let t0 = (tick_at(x) - c.start_tick.0) as f32 / c.length_ticks.0.max(1) as f32;
                let t1 = (tick_at(x + 2.0) - c.start_tick.0) as f32 / c.length_ticks.0.max(1) as f32;
                let s0 = (*source_offset_frames as f32 + t0.clamp(0.0, 1.0) * out_frames * rate) / total;
                let s1 = (*source_offset_frames as f32 + t1.clamp(0.0, 1.0) * out_frames * rate) / total;
                let (b0, b1) = ((s0 * th.len() as f32) as usize, ((s1 * th.len() as f32) as usize).max((s0 * th.len() as f32) as usize + 1));
                let (mut lo, mut hi) = (0.0f32, 0.0f32);
                for b in b0.min(th.len() - 1)..b1.min(th.len()) {
                    lo = lo.min(th[b].0);
                    hi = hi.max(th[b].1);
                }
                let (lo, hi) = ((lo * g).clamp(-1.0, 1.0), (hi * g).clamp(-1.0, 1.0));
                painter.line_segment([pos2(x, mid - hi * half), pos2(x, mid - lo * half)], Stroke::new(2.0, theme::TEXT.linear_multiply(0.75)));
                x += 2.0;
            }
        }
        // automation curve preview
        if let ClipContent::Automation { curve_id, source_offset_ticks, .. } = &c.content
            && let Some(curve) = app.project.automation.iter().find(|a| a.id == *curve_id)
        {
            let area = Rect::from_min_max(pos2(cr_vis.min.x, cr.min.y + 14.0), pos2(cr_vis.max.x, cr.max.y - 2.0));
            let mut poly = Vec::new();
            let mut x = area.min.x;
            while x <= area.max.x {
                let t = tick_at(x) - c.start_tick.0 + source_offset_ticks.0;
                if let Some(v) = rawdawg_audio_engine::schedule::Schedule::curve_value(curve, t) {
                    poly.push(pos2(x, area.max.y - v * area.height()));
                }
                x += 3.0;
            }
            if poly.len() > 1 {
                painter.add(egui::Shape::line(poly, Stroke::new(1.5, theme::TEXT.linear_multiply(0.9))));
            }
        }
        // note miniatures
        if let (Some(p), ClipContent::Pattern { source_offset_ticks, looped, .. }) = (mini, &c.content) {
            let area = Rect::from_min_max(pos2(cr.min.x, cr.min.y + 14.0), pos2(cr.max.x, cr.max.y - 2.0));
            let notes: Vec<(i64, i64, u8)> = p.lanes.iter().flat_map(|l| l.notes.iter().map(|n| (n.tick.0, n.duration_ticks.0, n.pitch))).collect();
            if !notes.is_empty() {
                let (lo, hi) = notes.iter().fold((127u8, 0u8), |(lo, hi), n| (lo.min(n.2), hi.max(n.2)));
                let span = (hi - lo).max(4) as f32;
                let plen = p.length_ticks.0.max(1);
                let reps = if *looped { (c.length_ticks.0 + source_offset_ticks.0 + plen - 1) / plen } else { 1 };
                for rep in 0..reps.min(64) {
                    for (tick, dur, pitch) in &notes {
                        let t0 = tick + rep * plen - source_offset_ticks.0;
                        if t0 < 0 || t0 >= c.length_ticks.0 {
                            continue;
                        }
                        let t1 = (t0 + dur).min(c.length_ticks.0);
                        let y = area.max.y - (*pitch as f32 - lo as f32) / span * area.height();
                        let nr = Rect::from_min_max(pos2(x_of(c.start_tick.0 + t0), y - 1.5), pos2(x_of(c.start_tick.0 + t1).max(x_of(c.start_tick.0 + t0) + 1.0), y + 1.5))
                            .intersect(cr_vis);
                        if nr.is_positive() {
                            painter.rect_filled(nr, 0.0, theme::TEXT.linear_multiply(0.8));
                        }
                    }
                }
                if *looped && reps > 1 {
                    for rep in 1..reps.min(64) {
                        let x = x_of(c.start_tick.0 + rep * plen - source_offset_ticks.0);
                        if x > cr_vis.min.x && x < cr_vis.max.x {
                            painter.line_segment([pos2(x, area.min.y), pos2(x, area.max.y)], Stroke::new(1.0, theme::BACKGROUND));
                        }
                    }
                }
            }
        }
    }
    // playhead
    if let Some(tick) = app.playhead_tick()
        && app.transport_mode == TransportMode::Song
    {
        let x = x_of(tick);
        if x >= lanes_rect.min.x && x <= full.max.x {
            painter.line_segment([pos2(x, full.min.y), pos2(x, full.max.y)], Stroke::new(1.5, theme::WARNING));
        }
    }

    // --- interaction ---
    let mods = ui.input(|i| i.modifiers);
    let ptr = PointerState::read(ui);
    let over = resp.contains_pointer();
    let pointer = ptr.pos;
    let egrid = if mods.alt { None } else { grid };
    let hit = |p: Pos2| clips.iter().rev().find(|c| clip_rect(c).contains(p)).cloned();
    // a file dragged over a lane lands as an audio clip there (see App::step)
    app.playlist.drop_target = None;
    let dragging_files = ui.input(|i| !i.raw.hovered_files.is_empty());
    if dragging_files
        && let Some(p) = ui.input(|i| i.pointer.hover_pos())
        && lanes_rect.contains(p)
        && let Some(track) = tracks.get(row_of(p.y).max(0) as usize)
    {
        app.playlist.drop_target = Some((track.id, snap_tick(tick_at(p.x), egrid).max(0)));
        let r = row_rect(row_of(p.y).max(0) as usize);
        painter.rect_stroke(r, 0.0, Stroke::new(2.0, theme::ACCENT), egui::StrokeKind::Inside);
        let x = x_of(snap_tick(tick_at(p.x), egrid).max(0));
        painter.line_segment([pos2(x, r.min.y), pos2(x, r.max.y)], Stroke::new(2.0, theme::ACCENT));
    }
    if let Some(asset) = app.playlist.place_asset.clone() {
        let name = app.project.assets.iter().find(|a| a.id == asset).map(|a| a.source_name.clone()).unwrap_or_default();
        app.playlist.hover = format!("Click in a track to place “{name}” as an audio clip (Esc cancels)");
    }
    let selected_pattern = app.project.selected_pattern.or_else(|| app.project.patterns.first().map(|p| p.id));
    app.playlist.hover = String::new();
    if let Some(p) = resp.hover_pos()
        && lanes_rect.contains(p)
        && let Some(c) = hit(p)
    {
        app.playlist.hover = format!("{} → {} ({} bars)", fmt_bar(c.start_tick.0, bar_t), fmt_bar(c.end_tick().0, bar_t), c.length_ticks.0 as f32 / bar_t as f32);
    }

    // ruler: click seeks, drag sets the loop range
    if ptr.primary_pressed
        && over
        && let Some(p) = pointer
        && ruler_rect.contains(p)
    {
        let t = snap_tick(tick_at(p.x), egrid).max(0);
        app.playlist.gesture = Some(Gesture::LoopRange { start_tick: t });
    }
    // double-click an automation clip: open its curve
    if resp.double_clicked()
        && let Some(p) = pointer
        && let Some(c) = hit(p)
        && let ClipContent::Automation { curve_id, .. } = &c.content
    {
        app.automation.curve = Some(*curve_id);
        app.editor_view = super::EditorView::Automation;
    }
    // lanes: primary press
    if ptr.primary_pressed
        && over
        && let Some(p) = pointer
        && lanes_rect.contains(p)
    {
        app.key_target = super::KeyTarget::Playlist;
        let st = &mut app.playlist;
        let armed = st.place_asset.clone();
        match hit(p) {
            Some(c) => {
                if mods.shift {
                    if !st.selection.remove(&c.id) {
                        st.selection.insert(c.id);
                    }
                } else if !st.selection.contains(&c.id) {
                    st.selection.clear();
                    st.selection.insert(c.id);
                }
                let sel: Vec<Clip> = clips.iter().filter(|x| st.selection.contains(&x.id)).cloned().collect();
                let cr = clip_rect(&c);
                let t = tick_at(p.x);
                st.gesture = Some(if p.x > cr.max.x - 8.0 && cr.width() > 16.0 {
                    Gesture::ResizeEnd { orig: sel, anchor_tick: t }
                } else if p.x < cr.min.x + 8.0 && cr.width() > 16.0 {
                    Gesture::TrimStart { orig: sel, anchor_tick: t }
                } else {
                    Gesture::Move { orig: sel, anchor_tick: t, anchor_row: row_of(p.y), moved: false, copy: mods.command }
                });
            }
            None => {
                let row = row_of(p.y);
                if mods.command || tracks.is_empty() {
                    st.gesture = Some(Gesture::Box { start: p });
                } else if let (Some(track), Some(asset)) = (tracks.get(row.max(0) as usize), armed) {
                    // place the armed audio asset as a clip
                    let t = snap_tick(tick_at(p.x), egrid).max(0);
                    let track_id = track.id;
                    if place_audio_clip(app, &asset, track_id, t).is_none() {
                        app.status_line = "That sample cannot be read; re-import it".into();
                    }
                } else if let (Some(track), Some(pid)) = (tracks.get(row.max(0) as usize), selected_pattern) {
                    // paint the selected pattern as a clip
                    let plen = app.project.pattern(pid).map(|p| p.length_ticks.0).unwrap_or(PPQ * 4).max(1);
                    let t = snap_tick(tick_at(p.x), egrid).max(0);
                    let clip = Clip {
                        id: ClipId::new(),
                        track_id: track.id,
                        start_tick: Tick(t),
                        length_ticks: Tick(plen),
                        muted: false,
                        content: ClipContent::Pattern { pattern_id: pid, source_offset_ticks: Tick::ZERO, looped: false },
                    };
                    st.selection.clear();
                    st.selection.insert(clip.id);
                    st.gesture = Some(Gesture::Paint { track: track.id, pattern: pid, last_tick: t });
                    cmds.push(Command::AddClip { clip });
                }
            }
        }
    }
    // secondary press on empty lane: erase gesture (right-click on a clip opens its menu)
    if ptr.secondary_pressed
        && over
        && let Some(p) = pointer
        && lanes_rect.contains(p)
        && hit(p).is_none()
    {
        app.playlist.gesture = Some(Gesture::Erase);
    }
    // drag updates (while a button is held)
    if ptr.any_down()
        && !ptr.primary_pressed
        && !ptr.secondary_pressed
        && app.playlist.gesture.is_some()
        && let Some(p) = pointer
    {
        match app.playlist.gesture.as_mut() {
            Some(Gesture::Move { orig, anchor_tick, anchor_row, moved, copy }) => {
                let dt_raw = tick_at(p.x) - *anchor_tick;
                let drow = row_of(p.y) - *anchor_row;
                if dt_raw != 0 || drow != 0 {
                    *moved = true;
                }
                if *moved {
                    let first = orig.iter().map(|c| c.start_tick.0).min().unwrap_or(0);
                    let dt = snap_tick(first + dt_raw, egrid) - first;
                    let copy = *copy;
                    let updated: Vec<Clip> = orig
                        .iter()
                        .map(|c| {
                            let row = tracks.iter().position(|t| t.id == c.track_id).unwrap_or(0) as i32 + drow;
                            let track_id = tracks.get(row.clamp(0, tracks.len() as i32 - 1) as usize).map(|t| t.id).unwrap_or(c.track_id);
                            Clip { start_tick: Tick((c.start_tick.0 + dt).max(0)), track_id, ..c.clone() }
                        })
                        .collect();
                    if copy {
                        // copies are created on release; show the originals moving as a preview
                        preview_clips(app, updated);
                    } else {
                        preview_clips(app, updated);
                    }
                }
            }
            Some(Gesture::ResizeEnd { orig, anchor_tick }) => {
                let (orig, anchor_tick) = (orig.clone(), *anchor_tick);
                let dt = tick_at(p.x) - anchor_tick;
                let updated: Vec<Clip> = orig
                    .iter()
                    .map(|c| {
                        let end = snap_round(c.end_tick().0 + dt, egrid).max(c.start_tick.0 + egrid.unwrap_or(PPQ / 4).max(1));
                        audio_bounds_from_ticks(app, c, Clip { length_ticks: Tick(end - c.start_tick.0), ..c.clone() })
                    })
                    .collect();
                preview_clips(app, updated);
            }
            Some(Gesture::TrimStart { orig, anchor_tick }) => {
                let (orig, anchor_tick) = (orig.clone(), *anchor_tick);
                let dt = tick_at(p.x) - anchor_tick;
                let updated: Vec<Clip> = orig
                    .iter()
                    .map(|c| {
                        let start = snap_round(c.start_tick.0 + dt, egrid).clamp(0, c.end_tick().0 - egrid.unwrap_or(PPQ / 4).max(1));
                        let shift = start - c.start_tick.0;
                        let content = match &c.content {
                            ClipContent::Pattern { pattern_id, source_offset_ticks, looped } => {
                                ClipContent::Pattern { pattern_id: *pattern_id, source_offset_ticks: Tick((source_offset_ticks.0 + shift).max(0)), looped: *looped }
                            }
                            other => other.clone(),
                        };
                        audio_bounds_from_ticks(app, c, Clip { start_tick: Tick(start), length_ticks: Tick(c.end_tick().0 - start), content, ..c.clone() })
                    })
                    .collect();
                preview_clips(app, updated);
            }
            Some(Gesture::Paint { track, pattern, last_tick }) => {
                let plen = app.project.pattern(*pattern).map(|p| p.length_ticks.0).unwrap_or(PPQ * 4).max(1);
                let t = snap_tick(tick_at(p.x), egrid).max(0);
                // brush: fill every pattern-length slot between the last painted tick and the pointer
                let (track, pattern, lt) = (*track, *pattern, *last_tick);
                let mut cursor = lt + plen;
                let mut new_last = lt;
                while cursor <= t {
                    if !app.project.clips.iter().any(|c| c.track_id == track && c.start_tick.0 < cursor + plen && c.end_tick().0 > cursor) {
                        let clip = Clip {
                            id: ClipId::new(),
                            track_id: track,
                            start_tick: Tick(cursor),
                            length_ticks: Tick(plen),
                            muted: false,
                            content: ClipContent::Pattern { pattern_id: pattern, source_offset_ticks: Tick::ZERO, looped: false },
                        };
                        app.playlist.selection.insert(clip.id);
                        cmds.push(Command::AddClip { clip });
                    }
                    new_last = cursor;
                    cursor += plen;
                }
                if let Some(Gesture::Paint { last_tick, .. }) = app.playlist.gesture.as_mut() {
                    *last_tick = new_last;
                }
            }
            Some(Gesture::Erase) => {
                if lanes_rect.contains(p)
                    && let Some(c) = hit(p)
                {
                    app.playlist.selection.remove(&c.id);
                    cmds.push(Command::RemoveClip { id: c.id });
                }
            }
            Some(Gesture::Box { start }) => {
                let r = Rect::from_two_pos(*start, p);
                painter.rect(r, 0.0, theme::ACCENT.linear_multiply(0.15), Stroke::new(1.0, theme::ACCENT), egui::StrokeKind::Inside);
            }
            Some(Gesture::LoopRange { start_tick }) => {
                let t = snap_round(tick_at(p.x), egrid).max(0);
                let (a, b) = (*start_tick, t);
                let r = Rect::from_min_max(pos2(x_of(a.min(b)), ruler_rect.min.y), pos2(x_of(a.max(b)), ruler_rect.max.y));
                painter.rect_filled(r, 0.0, theme::ACCENT.linear_multiply(0.5));
            }
            None => {}
        }
    }
    // release
    if app.playlist.gesture.is_some() && (ptr.any_released() || !ptr.any_down()) {
        match app.playlist.gesture.take() {
            Some(Gesture::Box { start }) => {
                if let Some(p) = pointer {
                    let r = Rect::from_two_pos(start, p);
                    if !mods.shift {
                        app.playlist.selection.clear();
                    }
                    for c in &clips {
                        if clip_rect(c).intersects(r) {
                            app.playlist.selection.insert(c.id);
                        }
                    }
                }
            }
            Some(Gesture::LoopRange { start_tick }) => {
                if let Some(p) = pointer {
                    let t = snap_round(tick_at(p.x), egrid).max(0);
                    if t == start_tick {
                        // plain click: seek
                        app.send(EngineCommand::SeekTick(start_tick));
                    } else {
                        cmds.push(Command::SetLoop { range: Some(LoopRange { start_tick: Tick(start_tick.min(t)), end_tick: Tick(start_tick.max(t)), enabled: true }) });
                    }
                }
            }
            Some(Gesture::Move { moved: false, .. }) => {
                if app.history.gesture_active() {
                    let _ = app.history.cancel_gesture(&mut app.project);
                }
            }
            Some(Gesture::Move { orig, copy: true, .. }) => {
                // Ctrl-drag copy: restore the originals and add the moved copies
                let moved: Vec<Clip> = app.project.clips.iter().filter(|c| orig.iter().any(|o| o.id == c.id)).cloned().collect();
                if app.history.gesture_active() {
                    let _ = app.history.cancel_gesture(&mut app.project);
                }
                let copies: Vec<Clip> = moved.into_iter().map(|c| Clip { id: ClipId::new(), ..c }).collect();
                app.playlist.selection = copies.iter().map(|c| c.id).collect();
                cmds.push(Command::Batch { commands: copies.into_iter().map(|clip| Command::AddClip { clip }).collect() });
            }
            Some(Gesture::Paint { .. }) | Some(Gesture::Erase) => {}
            Some(_) => commit(app),
            None => {}
        }
    }
    // clip context menu (right-click without drag): mute / loop / make unique / delete
    if let Some(p) = resp.hover_pos()
        && lanes_rect.contains(p)
        && let Some(c) = hit(p)
    {
        resp.clone().context_menu(|ui| {
            ui.label(egui::RichText::new("Clip").strong());
            let mut muted = c.muted;
            if ui.checkbox(&mut muted, "Muted").changed() {
                cmds.push(Command::UpdateClip { clip: Clip { muted, ..c.clone() } });
                ui.close();
            }
            if let ClipContent::Pattern { pattern_id, source_offset_ticks, looped } = &c.content {
                let mut lp = *looped;
                if ui.checkbox(&mut lp, "Loop pattern inside the clip").changed() {
                    cmds.push(Command::UpdateClip {
                        clip: Clip { content: ClipContent::Pattern { pattern_id: *pattern_id, source_offset_ticks: *source_offset_ticks, looped: lp }, ..c.clone() },
                    });
                    ui.close();
                }
                if ui.button("Make unique (clone pattern)").clicked() {
                    app.playlist.selection.clear();
                    app.playlist.selection.insert(c.id);
                    make_unique(app, &mut cmds);
                    ui.close();
                }
                if ui.button("Edit pattern in piano roll").clicked() {
                    cmds.push(Command::SelectPattern { id: Some(*pattern_id) });
                    app.editor_view = super::EditorView::PianoRoll;
                    ui.close();
                }
            }
            if let ClipContent::Audio { asset_id, source_offset_frames, source_length_frames, gain_db, fade_in_frames, fade_out_frames, playback_rate } = &c.content {
                let rate = audio_rate(app, asset_id, *playback_rate);
                let (mut gain, mut fi, mut fo) = (
                    gain_db.0,
                    *fade_in_frames as f64 / rate / app.project.sample_rate_hz.as_f64() * 1000.0,
                    *fade_out_frames as f64 / rate / app.project.sample_rate_hz.as_f64() * 1000.0,
                );
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label("Gain");
                    changed |= ui.add(egui::DragValue::new(&mut gain).range(-48.0..=24.0).speed(0.5).suffix(" dB")).changed();
                });
                ui.horizontal(|ui| {
                    ui.label("Fade in");
                    changed |= ui.add(egui::DragValue::new(&mut fi).range(0.0..=5000.0).speed(5.0).suffix(" ms")).changed();
                    ui.label("out");
                    changed |= ui.add(egui::DragValue::new(&mut fo).range(0.0..=5000.0).speed(5.0).suffix(" ms")).changed();
                });
                if changed {
                    let to_src = |ms: f64| ((ms / 1000.0 * app.project.sample_rate_hz.as_f64() * rate).round() as u64).min(*source_length_frames);
                    cmds.push(Command::UpdateClip {
                        clip: Clip {
                            content: ClipContent::Audio {
                                asset_id: asset_id.clone(),
                                source_offset_frames: *source_offset_frames,
                                source_length_frames: *source_length_frames,
                                gain_db: rawdawg_types::units::Db(gain),
                                fade_in_frames: to_src(fi),
                                fade_out_frames: to_src(fo),
                                playback_rate: *playback_rate,
                            },
                            ..c.clone()
                        },
                    });
                }
            }
            if ui.button("Split at playhead").clicked() {
                app.playlist.selection.clear();
                app.playlist.selection.insert(c.id);
                split_selected(app, &mut cmds);
                ui.close();
            }
            if ui.button("Delete").clicked() {
                cmds.push(Command::RemoveClip { id: c.id });
                ui.close();
            }
        });
    }
    // keys
    let text_focused = ui.memory(|m| m.focused().is_some());
    if (ui.rect_contains_pointer(full) || app.key_target == super::KeyTarget::Playlist) && !text_focused {
        let (del, split, dup, all, esc) = ui.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::Delete),
                i.consume_key(egui::Modifiers::NONE, egui::Key::S),
                i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::D)),
                i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::A)),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        });
        if esc {
            cancel_gesture(app);
            app.playlist.selection.clear();
            app.playlist.place_asset = None;
        }
        if all {
            app.playlist.selection = clips.iter().map(|c| c.id).collect();
        }
        if del {
            for id in app.playlist.selection.drain() {
                cmds.push(Command::RemoveClip { id });
            }
        }
        if split {
            split_selected(app, &mut cmds);
        }
        if dup {
            let sel: Vec<Clip> = clips.iter().filter(|c| app.playlist.selection.contains(&c.id)).cloned().collect();
            if !sel.is_empty() {
                let first = sel.iter().map(|c| c.start_tick.0).min().unwrap_or(0);
                let last = sel.iter().map(|c| c.end_tick().0).max().unwrap_or(0);
                let span = match grid {
                    Some(g) => ((last - first + g - 1) / g).max(1) * g,
                    None => last - first,
                };
                let copies: Vec<Clip> = sel.iter().map(|c| Clip { id: ClipId::new(), start_tick: Tick(c.start_tick.0 + span), ..c.clone() }).collect();
                app.playlist.selection = copies.iter().map(|c| c.id).collect();
                cmds.push(Command::Batch { commands: copies.into_iter().map(|clip| Command::AddClip { clip }).collect() });
            }
        }
    }
    if !app.playlist.hover.is_empty() {
        resp.on_hover_text_at_pointer(app.playlist.hover.clone());
    }
    for c in cmds {
        app.apply(c);
    }
    marker_dialog(app, ui);
}

fn bar_ticks(app: &App) -> i64 {
    app.project.meter_map().ok().map(|m| m.bar_start_tick(1).0).filter(|t| *t > 0).unwrap_or(PPQ * 4)
}

fn fmt_bar(tick: i64, bar_ticks: i64) -> String {
    let bar = tick.div_euclid(bar_ticks) + 1;
    let rem = tick.rem_euclid(bar_ticks);
    if rem == 0 { format!("{bar}") } else { format!("{bar}.{}", rem / PPQ + 1) }
}

/// Split selected clips at the playhead (song position); pattern clips keep their source offset.
fn split_selected(app: &mut App, cmds: &mut Vec<Command>) {
    let Some(at) = app.playhead_tick() else {
        app.status_line = "Start audio to use the playhead for splitting".into();
        return;
    };
    let sel: Vec<Clip> = app.project.clips.iter().filter(|c| app.playlist.selection.contains(&c.id)).cloned().collect();
    let mut batch = Vec::new();
    for c in sel {
        if at <= c.start_tick.0 || at >= c.end_tick().0 {
            continue;
        }
        let left = Clip { length_ticks: Tick(at - c.start_tick.0), ..c.clone() };
        let shift = at - c.start_tick.0;
        let content = match &c.content {
            ClipContent::Pattern { pattern_id, source_offset_ticks, looped } => {
                ClipContent::Pattern { pattern_id: *pattern_id, source_offset_ticks: Tick(source_offset_ticks.0 + shift), looped: *looped }
            }
            other => other.clone(),
        };
        let right = Clip { id: ClipId::new(), start_tick: Tick(at), length_ticks: Tick(c.end_tick().0 - at), content, ..c.clone() };
        app.playlist.selection.insert(right.id);
        batch.push(Command::UpdateClip { clip: left });
        batch.push(Command::AddClip { clip: right });
    }
    if batch.is_empty() {
        app.status_line = "The playhead is not inside a selected clip".into();
    } else {
        cmds.push(Command::Batch { commands: batch });
    }
}

/// Clone the pattern of every selected clip and point the clip at the clone.
fn make_unique(app: &mut App, cmds: &mut Vec<Command>) {
    let sel: Vec<Clip> = app.project.clips.iter().filter(|c| app.playlist.selection.contains(&c.id)).cloned().collect();
    let mut batch = Vec::new();
    let mut n = app.project.patterns.len();
    for c in sel {
        if let ClipContent::Pattern { pattern_id, source_offset_ticks, looped } = &c.content
            && let Some(p) = app.project.pattern(*pattern_id)
        {
            n += 1;
            let clone = clone_pattern(p, n);
            let new_id = clone.id;
            batch.push(Command::AddPattern { pattern: clone });
            batch.push(Command::UpdateClip {
                clip: Clip { content: ClipContent::Pattern { pattern_id: new_id, source_offset_ticks: *source_offset_ticks, looped: *looped }, ..c.clone() },
            });
        }
    }
    if !batch.is_empty() {
        cmds.push(Command::Batch { commands: batch });
    }
}

fn marker_dialog(app: &mut App, ui: &mut egui::Ui) {
    let Some(mut m) = app.playlist.marker_edit.take() else { return };
    let mut open = true;
    let mut apply = false;
    let mut remove = false;
    let title = if m.tempo { "Tempo change" } else { "Time signature" };
    egui::Window::new(title).collapsible(false).resizable(false).open(&mut open).show(ui.ctx(), |ui| {
        ui.label(format!("At bar {}", fmt_bar(m.tick, bar_ticks(app))));
        if m.tempo {
            ui.add(egui::DragValue::new(&mut m.bpm).range(20.0..=400.0).speed(0.1).suffix(" BPM"));
        } else {
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut m.numerator).range(1..=32));
                ui.label("/");
                let mut d = m.denominator as usize;
                egui::ComboBox::from_id_salt("meter-den").selected_text(format!("{}", m.denominator)).show_ui(ui, |ui| {
                    for v in [1usize, 2, 4, 8, 16, 32] {
                        ui.selectable_value(&mut d, v, format!("{v}"));
                    }
                });
                m.denominator = d as u8;
            });
        }
        ui.horizontal(|ui| {
            if ui.button("Apply").clicked() {
                apply = true;
            }
            if m.index > 0 && ui.button("Remove").clicked() {
                remove = true;
            }
        });
    });
    if apply || remove {
        if m.tempo {
            let mut tempo = app.project.tempo.clone();
            if remove {
                tempo.remove(m.index);
            } else if let Some(t) = tempo.get_mut(m.index) {
                match UsPerQuarter::from_bpm(m.bpm) {
                    Ok(v) => t.us_per_quarter = v,
                    Err(e) => app.status_line = format!("Tempo rejected: {e}"),
                }
            }
            app.apply(Command::SetTempoMap { tempo });
        } else {
            let mut meter = app.project.meter.clone();
            if remove {
                meter.remove(m.index);
            } else if let Some(x) = meter.get_mut(m.index) {
                x.numerator = m.numerator;
                x.denominator = m.denominator;
            }
            app.apply(Command::SetMeterMap { meter });
        }
    } else if open {
        app.playlist.marker_edit = Some(m);
    }
}

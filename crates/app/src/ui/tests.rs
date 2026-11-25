//! Headless UI input tests: a real egui context drives the actual window layout
//! with synthesized pointer/keyboard events; assertions read the project model.

use super::*;
use rawdawg_types::units::Tick;

struct Harness {
    ctx: egui::Context,
    app: App,
    pointer: egui::Pos2,
    parts: u8,
}

const W: f32 = 1400.0;
const H: f32 = 900.0;

impl Harness {
    fn new(project_dir: Option<&str>) -> Self {
        let ctx = egui::Context::default();
        let opts = GuiOptions { open: project_dir.map(PathBuf::from), safe_mode: false, device: None, buffer: None, no_audio: true, screenshot: None, ui_preset: None };
        let app = App::build(&ctx, opts, Preferences::default(), false);
        let mut h = Self { ctx, app, pointer: egui::pos2(0.0, 0.0), parts: u8::MAX };
        // two warm-up frames so panels and scroll areas settle
        h.frame(vec![]);
        h.frame(vec![]);
        h
    }

    fn frame(&mut self, events: Vec<egui::Event>) {
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(W, H))), events, ..Default::default() };
        let app = &mut self.app;
        let parts = self.parts;
        let mut out = self.ctx.run_ui(input, |ui| {
            app.step(ui.ctx());
            app.draw_parts(ui, parts);
        });
        // no renderer here: acknowledge the font texture deltas explicitly
        out.textures_delta.clear();
    }

    fn rect(&self, id: egui::Id) -> egui::Rect {
        self.ctx.read_response(id).unwrap_or_else(|| panic!("no widget with id {id:?} was drawn")).rect
    }

    fn move_to(&mut self, pos: egui::Pos2) {
        self.pointer = pos;
        self.frame(vec![egui::Event::PointerMoved(pos)]);
    }

    fn click_at(&mut self, pos: egui::Pos2, button: egui::PointerButton) {
        self.move_to(pos);
        self.frame(vec![egui::Event::PointerButton { pos, button, pressed: true, modifiers: egui::Modifiers::NONE }]);
        self.frame(vec![egui::Event::PointerButton { pos, button, pressed: false, modifiers: egui::Modifiers::NONE }]);
        self.frame(vec![]);
    }

    fn drag(&mut self, from: egui::Pos2, to: egui::Pos2, button: egui::PointerButton) {
        self.move_to(from);
        self.frame(vec![egui::Event::PointerButton { pos: from, button, pressed: true, modifiers: egui::Modifiers::NONE }]);
        let steps = 6;
        for i in 1..=steps {
            let p = from + (to - from) * (i as f32 / steps as f32);
            self.frame(vec![egui::Event::PointerMoved(p)]);
        }
        self.frame(vec![egui::Event::PointerButton { pos: to, button, pressed: false, modifiers: egui::Modifiers::NONE }]);
        self.frame(vec![]);
    }

    fn key(&mut self, key: egui::Key, modifiers: egui::Modifiers) {
        self.frame(vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }]);
        self.frame(vec![egui::Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers }]);
        self.frame(vec![]);
    }

    fn lane_notes(&self) -> Vec<rawdawg_project::Note> {
        let pid = self.app.project.selected_pattern.unwrap();
        let cid = self.app.selected_channel.unwrap();
        self.app.project.pattern(pid).unwrap().lanes.iter().find(|l| l.channel_id == cid).map(|l| l.notes.clone()).unwrap_or_default()
    }
}

#[test]
fn rack_steps_toggle_notes_and_undo_restores() {
    let mut h = Harness::new(None);
    h.app.editor_view = EditorView::Rack;
    h.frame(vec![]);
    let cid = h.app.selected_channel.unwrap();
    assert!(h.lane_notes().is_empty());
    let step3 = h.rect(egui::Id::unique(("step", cid, 2usize)));
    h.click_at(step3.center(), egui::PointerButton::Primary);
    let notes = h.lane_notes();
    assert_eq!(notes.len(), 1, "clicking a step adds one note");
    assert_eq!(notes[0].tick, Tick(480), "step 3 of a 1/16 grid");
    assert_eq!(notes[0].duration_ticks, Tick(240));
    // click again: removed
    h.click_at(step3.center(), egui::PointerButton::Primary);
    assert!(h.lane_notes().is_empty());
    // undo brings it back, redo removes it
    h.key(egui::Key::Z, egui::Modifiers::CTRL);
    assert_eq!(h.lane_notes().len(), 1);
    h.key(egui::Key::Z, egui::Modifiers::CTRL | egui::Modifiers::SHIFT);
    assert!(h.lane_notes().is_empty());
    // paint across steps 5..8 by dragging
    let s5 = h.rect(egui::Id::unique(("step", cid, 4usize)));
    let s8 = h.rect(egui::Id::unique(("step", cid, 7usize)));
    h.drag(s5.center(), s8.center(), egui::PointerButton::Primary);
    let ticks: Vec<i64> = h.lane_notes().iter().map(|n| n.tick.0).collect();
    assert_eq!(ticks.len(), 4, "drag paints every step it crosses: {ticks:?}");
    assert!(ticks.contains(&960) && ticks.contains(&1680));
}

#[test]
fn piano_roll_draws_moves_and_deletes_notes_with_the_mouse() {
    let mut h = Harness::new(None);
    h.app.editor_view = EditorView::PianoRoll;
    h.frame(vec![]);
    let surface = h.rect(egui::Id::unique("piano-roll-surface"));
    // click at beat 2, pitch E4 (64)
    let p = super::piano_roll::grid_point(&h.app.piano_roll, surface, 960 + 100, 64.5);
    h.click_at(p, egui::PointerButton::Primary);
    let notes = h.lane_notes();
    assert_eq!(notes.len(), 1, "click in empty grid draws a note");
    assert_eq!(notes[0].pitch, 64);
    assert_eq!(notes[0].tick, Tick(960), "snapped to the 1/16 grid");
    assert_eq!(notes[0].duration_ticks, Tick(240), "default length one grid step");
    // drag the note body one beat right and two semitones up
    let surface = h.rect(egui::Id::unique("piano-roll-surface"));
    let from = super::piano_roll::grid_point(&h.app.piano_roll, surface, 960 + 60, 64.5);
    let to = super::piano_roll::grid_point(&h.app.piano_roll, surface, 1920 + 60, 66.5);
    h.drag(from, to, egui::PointerButton::Primary);
    let notes = h.lane_notes();
    assert_eq!(notes.len(), 1, "{:?}", notes.iter().map(|n| (n.tick.0, n.pitch, n.duration_ticks.0)).collect::<Vec<_>>());
    assert_eq!((notes[0].tick, notes[0].pitch), (Tick(1920), 66), "moved by a beat and two semitones (one undo entry)");
    assert!(h.app.history.can_undo());
    // right-click deletes
    let surface = h.rect(egui::Id::unique("piano-roll-surface"));
    let on_note = super::piano_roll::grid_point(&h.app.piano_roll, surface, 1920 + 60, 66.5);
    h.click_at(on_note, egui::PointerButton::Secondary);
    assert!(h.lane_notes().is_empty(), "right-click removes the note");
    h.key(egui::Key::Z, egui::Modifiers::CTRL);
    assert_eq!(h.lane_notes().len(), 1, "undo restores the deleted note");
    // chord insertion: select Major, click at C4
    h.app.piano_roll.chord = super::piano_roll::Chord::Major;
    let surface = h.rect(egui::Id::unique("piano-roll-surface"));
    let p = super::piano_roll::grid_point(&h.app.piano_roll, surface, 2880 + 30, 60.5);
    h.click_at(p, egui::PointerButton::Primary);
    let mut pitches: Vec<u8> = h.lane_notes().iter().filter(|n| n.tick == Tick(2880)).map(|n| n.pitch).collect();
    pitches.sort_unstable();
    assert_eq!(pitches, vec![60, 64, 67], "major chord at the pointer");
}

#[test]
fn playlist_paints_clips_and_shortcuts_switch_views() {
    let mut h = Harness::new(Some("../../fixtures/demo-song.rawdawg"));
    assert_eq!(h.app.project.name, "rawDAWg Song Demo");
    let before = h.app.project.clips.len();
    let surface = h.rect(egui::Id::unique("playlist-surface"));
    // paint the selected pattern at bar 6 on the first track
    let p = super::playlist::lane_point(&h.app.playlist, surface, 3840 * 5 + 200, 0);
    h.click_at(p, egui::PointerButton::Primary);
    assert_eq!(h.app.project.clips.len(), before + 1, "click in a lane places the selected pattern");
    let clip = h.app.project.clips.last().unwrap().clone();
    assert_eq!(clip.start_tick, Tick(3840 * 5), "snapped to the beat grid");
    assert_eq!(clip.length_ticks, Tick(3840), "one pattern length");
    // select it and press Delete
    let surface = h.rect(egui::Id::unique("playlist-surface"));
    let on_clip = super::playlist::lane_point(&h.app.playlist, surface, 3840 * 5 + 1000, 0);
    h.click_at(on_clip, egui::PointerButton::Primary);
    assert!(h.app.playlist.selection.contains(&clip.id), "click selects the clip");
    h.move_to(on_clip);
    h.key(egui::Key::Delete, egui::Modifiers::NONE);
    assert_eq!(h.app.project.clips.len(), before, "Delete removes the selected clip");
    // view shortcuts
    h.key(egui::Key::F7, egui::Modifiers::NONE);
    assert_eq!(h.app.editor_view, EditorView::PianoRoll);
    h.key(egui::Key::F6, egui::Modifiers::NONE);
    assert_eq!(h.app.editor_view, EditorView::Rack);
    h.key(egui::Key::F8, egui::Modifiers::NONE);
    assert_eq!(h.app.editor_view, EditorView::Synth);
}

#[test]
fn window_is_dark_only_and_draws_every_view_without_panicking() {
    let mut h = Harness::new(Some("../../fixtures/demo-graph.rawdawg"));
    h.app.synth_panel.advanced = true;
    for view in [EditorView::Rack, EditorView::PianoRoll, EditorView::Automation, EditorView::Synth] {
        h.app.editor_view = view;
        h.frame(vec![]);
        h.frame(vec![]);
    }
    let visuals = h.ctx.global_style().visuals.clone();
    assert!(visuals.dark_mode, "the owned UI is dark only");
    assert_eq!(visuals.panel_fill, theme::PANEL);
    assert!(h.app.plan_error.is_none(), "{:?}", h.app.plan_error);
}

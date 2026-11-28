//! Headless UI input tests: a real egui context drives the actual window layout
//! with synthesized pointer/keyboard events; assertions read the project model.

use super::*;
use rawdawg_types::units::Tick;

struct Harness {
    ctx: egui::Context,
    app: App,
    pointer: egui::Pos2,
    parts: u8,
    /// Window size in points (W × H unless a test resizes it).
    size: egui::Vec2,
}

const W: f32 = 1400.0;
const H: f32 = 900.0;

impl Harness {
    fn new(project_dir: Option<&str>) -> Self {
        Self::with_preset(project_dir, None)
    }

    /// The committed sampler demo, with its generated (git-ignored) assets created if missing.
    fn sampler_demo() -> Self {
        rawdawg_fixtures::ensure_signals(&rawdawg_fixtures::fixtures_dir()).expect("generate fixtures");
        Self::new(Some("../../fixtures/demo-sampler.rawdawg"))
    }

    fn with_preset(project_dir: Option<&str>, ui_preset: Option<&str>) -> Self {
        // each test app keeps its never-saved projects in a folder of its own (never the user's)
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let recovery = std::env::temp_dir().join(format!("rawdawg-test-unsaved-{}-{n}", std::process::id()));
        Self::with_recovery_dir(project_dir, ui_preset, recovery)
    }

    /// A test app whose never-saved projects live in `recovery` (a crash test starts two apps on
    /// the same folder).
    fn with_recovery_dir(project_dir: Option<&str>, ui_preset: Option<&str>, recovery: PathBuf) -> Self {
        let ctx = egui::Context::default();
        let opts = GuiOptions {
            open: project_dir.map(PathBuf::from),
            safe_mode: false,
            device: None,
            buffer: None,
            no_audio: true,
            screenshot: None,
            ui_preset: ui_preset.map(str::to_owned),
            catalog: None,
            recovery_dir: Some(recovery),
        };
        let app = App::build(&ctx, opts, Preferences::default(), false);
        let mut h = Self { ctx, app, pointer: egui::pos2(0.0, 0.0), parts: u8::MAX, size: egui::vec2(W, H) };
        // fixed randomize seeds, so a test that randomizes can be reproduced (the app draws
        // them from launch entropy; `randomize_starts_from_a_different_seed_every_launch` checks that)
        h.app.synth_panel.evolve.seed_counter = 0x5EED_1234;
        h.app.mixer.seed = 0x5EED_5678;
        // two warm-up frames so panels and scroll areas settle
        h.frame(vec![]);
        h.frame(vec![]);
        h
    }

    fn frame(&mut self, events: Vec<egui::Event>) {
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, self.size)), events, ..Default::default() };
        let app = &mut self.app;
        let parts = self.parts;
        let mut out = self.ctx.run_ui(input, |ui| {
            app.step(ui.ctx());
            app.draw_parts(ui, parts);
        });
        // no renderer here: acknowledge the font texture deltas explicitly
        out.textures_delta.clear();
    }

    /// The shapes one frame paints.
    fn shapes(&mut self) -> Vec<egui::epaint::ClippedShape> {
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, self.size)), ..Default::default() };
        let app = &mut self.app;
        let parts = self.parts;
        let mut out = self.ctx.run_ui(input, |ui| {
            app.step(ui.ctx());
            app.draw_parts(ui, parts);
        });
        out.textures_delta.clear();
        out.shapes
    }

    /// The topmost drawn widget with exactly this text.
    fn node(&mut self, text: &str) -> Option<egui::Rect> {
        self.drawn_nodes().into_iter().filter(|(t, _)| t == text).map(|(_, r)| r).min_by(|a, b| a.top().total_cmp(&b.top()))
    }

    /// Scroll the editor panel (under the playlist) until `find` sees the widget inside its
    /// visible part (laid-out widgets scrolled away are clipped and take no input).
    fn scroll_into_view(&mut self, what: &str, find: impl Fn(&mut Harness) -> Option<egui::Rect>) -> egui::Rect {
        for _ in 0..80 {
            let clip = self.app.editor_clip;
            let at = clip.center();
            match find(self) {
                Some(r) if r.top() >= clip.top() + 2.0 && r.bottom() <= clip.bottom() - 2.0 => return r,
                Some(r) if r.top() < clip.top() + 2.0 => self.scroll(at, 90.0),
                _ => self.scroll(at, -90.0),
            }
        }
        panic!("{what} never came into view")
    }

    /// Every label and value drawn in one frame, read from egui's accessibility tree.
    fn drawn_texts(&mut self) -> Vec<String> {
        self.drawn_nodes().into_iter().map(|(t, _)| t).collect()
    }

    /// Text and on-screen bounds of every labelled widget drawn in one frame.
    fn drawn_nodes(&mut self) -> Vec<(String, egui::Rect)> {
        self.accessibility_nodes()
            .iter()
            .flat_map(|n| {
                let r = Self::node_rect(n);
                n.label().into_iter().chain(n.value()).map(move |t| (t.to_owned(), r))
            })
            .collect()
    }

    fn node_rect(node: &egui::accesskit::Node) -> egui::Rect {
        node.bounds().map(|b| egui::Rect::from_min_max(egui::pos2(b.x0 as f32, b.y0 as f32), egui::pos2(b.x1 as f32, b.y1 as f32))).unwrap_or(egui::Rect::NOTHING)
    }

    fn accessibility_nodes(&mut self) -> Vec<egui::accesskit::Node> {
        self.ctx.enable_accesskit();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, self.size)), ..Default::default() };
        let app = &mut self.app;
        let parts = self.parts;
        let mut out = self.ctx.run_ui(input, |ui| {
            app.step(ui.ctx());
            app.draw_parts(ui, parts);
        });
        out.textures_delta.clear();
        out.platform_output.accesskit_update.expect("accessibility tree").nodes.into_iter().map(|(_, node)| node).collect()
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

    /// Two-finger trackpad scroll at `at` (negative `dy` reveals content further down).
    fn scroll(&mut self, at: egui::Pos2, dy: f32) {
        let wheel = |phase| egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, dy),
            phase,
            source: egui::MouseWheelSource::Trackpad,
            modifiers: egui::Modifiers::NONE,
        };
        self.move_to(at);
        self.frame(vec![wheel(egui::TouchPhase::Start), wheel(egui::TouchPhase::Move)]);
        self.frame(vec![wheel(egui::TouchPhase::End)]);
        self.frame(vec![]);
    }

    /// Run frames until the widget `id` stops moving (scroll animations finished).
    fn settle(&mut self, id: egui::Id) {
        let mut last = self.rect(id);
        for _ in 0..600 {
            self.frame(vec![]);
            let now = self.rect(id);
            if now == last {
                return;
            }
            last = now;
        }
        panic!("{id:?} kept moving");
    }

    /// One frame with the window out of focus (another application took it).
    fn frame_unfocused(&mut self) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, self.size)),
            focused: false,
            events: vec![egui::Event::WindowFocused(false)],
            ..Default::default()
        };
        let app = &mut self.app;
        let parts = self.parts;
        let mut out = self.ctx.run_ui(input, |ui| {
            app.step(ui.ctx());
            app.draw_parts(ui, parts);
        });
        out.textures_delta.clear();
    }

    /// A key event as the OS sends it (`repeat`: auto-repeat while held).
    fn key_event(&mut self, key: egui::Key, pressed: bool, repeat: bool, modifiers: egui::Modifiers) {
        // the platform reports a change of held modifiers before the key, as egui expects
        self.frame(vec![egui::Event::ModifiersChanged(modifiers), egui::Event::Key { key, physical_key: None, pressed, repeat, modifiers }]);
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

#[test]
fn multiband_canvas_paints_one_band_per_gesture_and_undo_restores_it() {
    use rawdawg_effects::distortion::multiband::{Multiband, TYPE_ID};
    let mut h = Harness::with_preset(Some("../../fixtures/demo-song.rawdawg"), Some(&format!("effect:{TYPE_ID}")));
    let iid = h.app.effect_editor.expect("the preset opened a fresh insert");
    assert_eq!(h.app.editor_view, EditorView::Effect);
    let bands = |h: &mut Harness| h.app.other_mirrors.get_mut(&iid).unwrap().as_any_mut().downcast_mut::<Multiband>().unwrap().bands();
    assert_eq!(bands(&mut h)[1].0, 0.0, "fresh insert: no drive");
    let canvas = h.rect(egui::Id::unique("multiband-canvas"));
    let rows = (canvas.height() - 14.0) / 3.0;
    let col = canvas.width() / 4.0;
    // drag upward in the Drive row of the second band: drive rises with the pointer
    let x = canvas.min.x + col * 1.5;
    h.drag(egui::pos2(x, canvas.min.y + rows - 6.0), egui::pos2(x, canvas.min.y + 8.0), egui::PointerButton::Primary);
    let painted = bands(&mut h);
    assert!(painted[1].0 > 30.0, "band 1 drive painted high: {}", painted[1].0);
    assert_eq!(painted[0].0, 0.0, "neighbouring band untouched");
    assert_eq!(painted[1].1, 100.0, "mix row untouched");
    // the whole stroke is one undo entry; undo restores the unpainted band
    h.key(egui::Key::Z, egui::Modifiers::CTRL);
    assert_eq!(bands(&mut h)[1].0, 0.0, "undo restores the band in one step");
    assert!(h.app.plan_error.is_none(), "{:?}", h.app.plan_error);
}

#[test]
fn rack_randomize_morph_pad_and_parallel_branch_are_single_undo_steps() {
    use rawdawg_effects::reverb;
    let mut h = Harness::with_preset(Some("../../fixtures/demo-song.rawdawg"), Some(&format!("effect:{}", reverb::TYPE_ID)));
    let iid = h.app.effect_editor.expect("the preset opened a fresh insert");
    let mixer_id = h.app.project.mixer.channels.first().unwrap().id;
    h.app.editor_view = EditorView::Mixer;
    h.app.mixer.selected = Some(mixer_id);
    h.frame(vec![]);
    h.frame(vec![]);
    // mirrors are rebuilt lazily after undo: always go through the effect panel's ensure
    let param = |h: &mut Harness, id: u32| {
        super::effect_panel::ensure_mirror(&mut h.app, iid).unwrap();
        h.app.other_mirrors[&iid].param(rawdawg_types::ParamId(id)).unwrap()
    };
    let before_decay = param(&mut h, reverb::P_DECAY);
    let before_mix = param(&mut h, reverb::P_MIX);
    let undo_depth = |h: &Harness| h.app.history.undo_len();
    let depth0 = undo_depth(&h);
    // randomize at full chaos: something moves, mix stays (locked by default), one undo step restores
    h.app.mixer.chaos = 100.0;
    let changed = super::mixer::randomize_rack(&mut h.app, mixer_id);
    assert_eq!(changed, 1, "the one insert changed");
    let snap: Vec<f32> = reverb::DESCRIPTORS.iter().map(|d| param(&mut h, d.id.0)).collect();
    let base: Vec<f32> = reverb::DESCRIPTORS.iter().map(|d| d.default_normalized).collect();
    assert_ne!(snap, base, "randomize changed parameters");
    assert_eq!(param(&mut h, reverb::P_MIX), before_mix, "mix is locked by default");
    assert_eq!(undo_depth(&h), depth0 + 1, "one undo entry");
    h.key(egui::Key::Z, egui::Modifiers::CTRL);
    h.frame(vec![]);
    assert_eq!(param(&mut h, reverb::P_DECAY), before_decay, "undo restores the rack");
    // morph: store A..D with different decays, drag on the pad, release → one undo entry
    let set_decay = |h: &mut Harness, v: f32| {
        super::effect_panel::ensure_mirror(&mut h.app, iid).unwrap();
        super::mixer::apply_rack_values(&mut h.app, &[(iid, vec![(rawdawg_types::ParamId(reverb::P_DECAY), v)])], None, false);
    };
    for (i, v) in [0.1f32, 0.9, 0.1, 0.9].iter().enumerate() {
        set_decay(&mut h, *v);
        let m = h.app.project.mixer_channel(mixer_id).cloned().unwrap();
        let snap = super::mixer::rack_snapshot(&h.app, &m);
        h.app.mixer.morph_slots.entry(mixer_id).or_default()[i] = Some(snap);
    }
    set_decay(&mut h, 0.5);
    h.frame(vec![]);
    let depth1 = undo_depth(&h);
    let pad = h.rect(egui::Id::unique(("morph-pad", mixer_id)));
    // drag from the left edge (A/C: decay 0.1) to the right edge (B/D: decay 0.9)
    h.drag(egui::pos2(pad.min.x + 2.0, pad.center().y), egui::pos2(pad.max.x - 2.0, pad.center().y), egui::PointerButton::Primary);
    let decay = param(&mut h, reverb::P_DECAY);
    assert!(decay > 0.85, "morph moved the decay toward the right corners: {decay}");
    assert_eq!(undo_depth(&h), depth1 + 1, "the whole morph drag is one undo entry");
    h.key(egui::Key::Z, egui::Modifiers::CTRL);
    h.frame(vec![]);
    assert!((param(&mut h, reverb::P_DECAY) - 0.5).abs() < 1e-6, "undo restores the pre-drag rack");
    // parallel branch: a new bus fed by a 0 dB post-fader send, routed like the channel
    let m = h.app.project.mixer_channel(mixer_id).cloned().unwrap();
    let mut cmds = Vec::new();
    let bus = super::mixer::add_parallel_branch(&m, &mut cmds);
    for c in cmds {
        h.app.apply(c);
    }
    let src = h.app.project.mixer_channel(mixer_id).unwrap();
    assert!(src.sends.iter().any(|s| s.target_id == bus && s.level_db.0 == 0.0 && s.tap == rawdawg_project::SendTap::PostFader));
    let b = h.app.project.mixer_channel(bus).unwrap();
    assert_eq!(b.output_id, src.output_id);
    assert!(h.app.plan_error.is_none(), "{:?}", h.app.plan_error);
    h.key(egui::Key::Z, egui::Modifiers::CTRL);
    assert!(h.app.project.mixer_channel(bus).is_none(), "one undo removes the branch and its send");
    assert!(!h.app.project.mixer_channel(mixer_id).unwrap().sends.iter().any(|s| s.target_id == bus));
}

#[test]
fn midi_learn_maps_a_cc_to_an_effect_parameter_and_lfo_clips_modulate_it() {
    use rawdawg_effects::reverb;
    use rawdawg_types::events::{EventKind, EventSource, TimedEvent};
    let mut h = Harness::with_preset(Some("../../fixtures/demo-song.rawdawg"), Some(&format!("effect:{}", reverb::TYPE_ID)));
    let iid = h.app.effect_editor.expect("the preset opened a fresh insert");
    super::effect_panel::ensure_mirror(&mut h.app, iid).unwrap();
    // learn: arm, then the next controller event binds CC 74 to the decay
    h.app.arm_midi_learn(iid, reverb::P_DECAY);
    let cc = |value: f32| TimedEvent::new(0, EventSource::MidiDevice(0), 0, EventKind::Control { cc: 74, value });
    assert!(h.app.handle_midi_control(&cc(0.3)), "the learning event is consumed");
    let mapping = h.app.midi_mapping_for(iid, reverb::P_DECAY).cloned().expect("mapping stored in the project");
    assert_eq!(mapping.cc, 74);
    assert!(h.app.handle_midi_control(&cc(0.9)), "mapped events are consumed");
    super::effect_panel::ensure_mirror(&mut h.app, iid).unwrap();
    let decay = h.app.other_mirrors[&iid].param(rawdawg_types::ParamId(reverb::P_DECAY)).unwrap();
    assert!((decay - 0.9).abs() < 1e-6, "the controller moves the parameter: {decay}");
    assert!(!h.app.handle_midi_control(&TimedEvent::new(0, EventSource::MidiDevice(0), 0, EventKind::Control { cc: 1, value: 0.5 })), "unmapped CCs pass through");
    // LFO clip: a sine, one cycle per bar over four bars, ±0.25 around the current value (0.5)
    assert!(h.app.handle_midi_control(&cc(0.5)));
    let curves = h.app.project.automation.len();
    let clips = h.app.project.clips.len();
    super::automation::create_lfo_for(&mut h.app, iid, reverb::P_DECAY, 0, 1.0, 0.25);
    assert_eq!(h.app.project.automation.len(), curves + 1);
    assert_eq!(h.app.project.clips.len(), clips + 1);
    let curve = h.app.project.automation.last().unwrap();
    assert_eq!(curve.target.instance_id, iid);
    assert_eq!(curve.target.param_id, reverb::P_DECAY);
    assert_eq!(curve.points.len(), 65, "16 points per cycle × 4 cycles + end point");
    let (lo, hi) = curve.points.iter().fold((1.0f32, 0.0f32), |(a, b), p| (a.min(p.value), b.max(p.value)));
    assert!((hi - lo - 0.5).abs() < 0.02, "swing ±0.25: {lo}..{hi}");
    assert!(curve.points.windows(2).all(|w| w[1].tick >= w[0].tick), "ticks are monotonic");
    assert_eq!(h.app.editor_view, EditorView::Automation);
    // square shape uses step interpolation; undo removes clip and curve together
    super::automation::create_lfo_for(&mut h.app, iid, reverb::P_DECAY, 2, 4.0, 0.1);
    assert!(h.app.project.automation.last().unwrap().points.iter().all(|p| p.interpolation == rawdawg_project::Interpolation::Step));
    h.key(egui::Key::Z, egui::Modifiers::CTRL);
    h.key(egui::Key::Z, egui::Modifiers::CTRL);
    assert_eq!(h.app.project.automation.len(), curves);
    assert_eq!(h.app.project.clips.len(), clips);
}

#[test]
fn armed_sample_places_an_audio_clip_and_edge_drags_trim_its_source_frames() {
    use rawdawg_project::{ClipContent, Command, Track};
    let mut h = Harness::sampler_demo();
    let asset = h.app.project.assets.iter().find(|a| a.kind == rawdawg_project::AssetKind::Audio).cloned().expect("the sampler demo has a WAV asset");
    // import records no frame count; the playlist measures the decoded sample instead
    let frames = {
        use rawdawg_audio_engine::AssetResolver;
        h.app.assets.sample(&asset.id).expect("decodes").frames()
    };
    assert_eq!(frames, 96_000, "2 s at 48 kHz");
    let tid = rawdawg_types::TrackId::new();
    h.app.apply(Command::AddTrack { track: Track { id: tid, name: "Audio".into(), color: None, mute: false, solo: false, height: None, mixer_id: None }, index: None });
    h.app.playlist.place_asset = Some(asset.id.clone());
    h.app.editor_view = EditorView::Rack;
    h.frame(vec![]);
    let before = h.app.project.clips.len();
    let surface = h.rect(egui::Id::unique("playlist-surface"));
    let p = super::playlist::lane_point(&h.app.playlist, surface, 3840 + 100, 0);
    h.click_at(p, egui::PointerButton::Primary);
    assert_eq!(h.app.project.clips.len(), before + 1, "click places the armed sample");
    let clip = h.app.project.clips.last().unwrap().clone();
    assert_eq!(clip.track_id, tid);
    assert_eq!(clip.start_tick, Tick(3840), "snapped to the beat grid at bar 2");
    assert_eq!(clip.length_ticks, Tick(3840), "2 s at 120 BPM = one bar");
    let ClipContent::Audio { source_length_frames, source_offset_frames, playback_rate, .. } = &clip.content else { panic!("audio clip") };
    assert_eq!((*source_length_frames, *source_offset_frames, *playback_rate), (96_000, 0, 1.0));
    // drag the right edge back half a bar: the source is trimmed to one second
    let surface = h.rect(egui::Id::unique("playlist-surface"));
    let edge = super::playlist::lane_point(&h.app.playlist, surface, clip.end_tick().0, 0) - egui::vec2(3.0, 0.0);
    let target = super::playlist::lane_point(&h.app.playlist, surface, clip.end_tick().0 - 1920, 0);
    h.drag(edge, target, egui::PointerButton::Primary);
    let trimmed = h.app.project.clips.iter().find(|c| c.id == clip.id).unwrap().clone();
    assert_eq!(trimmed.length_ticks, Tick(1920), "half a bar long now");
    let ClipContent::Audio { source_length_frames, .. } = &trimmed.content else { panic!("audio clip") };
    assert!((*source_length_frames as i64 - 48_000).abs() <= 2, "source trimmed to 1 s: {source_length_frames}");
    // trim the start by a beat: the source offset moves with it
    let surface = h.rect(egui::Id::unique("playlist-surface"));
    let edge = super::playlist::lane_point(&h.app.playlist, surface, trimmed.start_tick.0, 0) + egui::vec2(3.0, 0.0);
    let target = super::playlist::lane_point(&h.app.playlist, surface, trimmed.start_tick.0 + 960, 0);
    h.drag(edge, target, egui::PointerButton::Primary);
    let t2 = h.app.project.clips.iter().find(|c| c.id == clip.id).unwrap().clone();
    assert_eq!(t2.start_tick, Tick(3840 + 960));
    let ClipContent::Audio { source_length_frames, source_offset_frames, .. } = &t2.content else { panic!("audio clip") };
    assert!((*source_offset_frames as i64 - 24_000).abs() <= 2, "offset follows the trimmed start: {source_offset_frames}");
    assert!((*source_length_frames as i64 - 24_000).abs() <= 2, "length shrinks accordingly: {source_length_frames}");
    // routing: the track can be sent to a bus and back, one undo each
    let bus = h.app.project.mixer.channels.iter().find(|m| m.id != h.app.project.mixer.master_id).map(|m| m.id).unwrap();
    h.app.apply(Command::SetTrackMixer { id: tid, mixer_id: Some(bus) });
    assert_eq!(h.app.project.tracks.iter().find(|t| t.id == tid).unwrap().mixer_id, Some(bus));
    h.key(egui::Key::Z, egui::Modifiers::CTRL);
    assert_eq!(h.app.project.tracks.iter().find(|t| t.id == tid).unwrap().mixer_id, None);
    assert!(h.app.plan_error.is_none(), "{:?}", h.app.plan_error);
}

#[test]
fn export_dialog_renders_on_a_worker_with_progress_and_midi_import_round_trips() {
    use rawdawg_audio_engine::offline::{ExportFormat, WavFormat};
    let root = std::env::temp_dir().join(format!("rawdawg-ui-export-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let mut h = Harness::new(Some("../../fixtures/demo-song.rawdawg"));
    h.app.export.open = true;
    h.app.export.song = true;
    h.app.export.format = ExportFormat::Wav(WavFormat::Pcm16);
    h.app.export.stems = true;
    h.app.export.tail_seconds = 0.1;
    h.app.export.path = root.join("mix.wav").display().to_string();
    h.frame(vec![]);
    super::export::start(&mut h.app);
    assert!(h.app.export.running(), "export runs on a worker");
    let started = std::time::Instant::now();
    while h.app.export.running() && started.elapsed().as_secs() < 60 {
        std::thread::sleep(std::time::Duration::from_millis(20));
        h.frame(vec![]);
    }
    let result = h.app.export.last_result.clone().expect("a result message");
    assert!(result.starts_with("Exported"), "{result}");
    let mix = rawdawg_sampler::decode::decode_file(&root.join("mix.wav")).unwrap();
    assert_eq!(mix.channels.len(), 2);
    assert!(mix.frames() > 369_000, "whole song plus tail: {}", mix.frames());
    let stems: Vec<_> = std::fs::read_dir(root.join("mix-stems")).unwrap().flatten().collect();
    assert_eq!(stems.len(), h.app.project.mixer.channels.len(), "one stem per mixer channel");
    for s in &stems {
        assert_eq!(rawdawg_sampler::decode::decode_file(&s.path()).unwrap().frames(), mix.frames(), "stems share the render length");
    }
    // FLAC through the same path
    h.app.export.format = ExportFormat::Flac24;
    h.app.export.stems = false;
    h.app.export.path = root.join("mix.flac").display().to_string();
    super::export::start(&mut h.app);
    let started = std::time::Instant::now();
    while h.app.export.running() && started.elapsed().as_secs() < 60 {
        std::thread::sleep(std::time::Duration::from_millis(20));
        h.frame(vec![]);
    }
    assert_eq!(rawdawg_sampler::decode::decode_file(&root.join("mix.flac")).unwrap().frames(), mix.frames());
    // MIDI: export the arrangement, import it back as new channels/pattern/track in one undo step
    let bytes = rawdawg_project::smf::export_song(&h.app.project).unwrap();
    let mid = root.join("song.mid");
    std::fs::write(&mid, bytes).unwrap();
    let (channels, patterns, tracks, clips) = (h.app.project.channels.len(), h.app.project.patterns.len(), h.app.project.tracks.len(), h.app.project.clips.len());
    h.app.import_midi_file(&mid);
    assert!(h.app.status_line.starts_with("Imported"), "{}", h.app.status_line);
    assert_eq!(h.app.project.channels.len(), channels + 1, "one channel per MIDI track with notes");
    assert_eq!(h.app.project.patterns.len(), patterns + 1);
    assert_eq!(h.app.project.tracks.len(), tracks + 1);
    assert_eq!(h.app.project.clips.len(), clips + 1);
    let pat = h.app.project.patterns.last().unwrap();
    let imported_notes: usize = pat.lanes.iter().map(|l| l.notes.len()).sum();
    let arranged = rawdawg_project::smf::song_notes(&h.app.project).iter().filter(|(c, _)| h.app.project.channels[..channels].iter().any(|ch| ch.id == *c)).count();
    assert_eq!(imported_notes, arranged, "every arranged note came back");
    assert!(h.app.plan_error.is_none(), "{:?}", h.app.plan_error);
    h.key(egui::Key::Z, egui::Modifiers::CTRL);
    assert_eq!(
        (h.app.project.channels.len(), h.app.project.patterns.len(), h.app.project.tracks.len(), h.app.project.clips.len()),
        (channels, patterns, tracks, clips),
        "one undo removes the import"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn audio_takes_are_aligned_placed_as_clips_and_recovered_after_a_crash() {
    use rawdawg_audio_io::capture::{CaptureProducer, InputChannels};
    use rawdawg_audio_io::capture_writer::CaptureWriter;
    use rawdawg_project::{ClipContent, Track};
    // alignment: transport at 96 000, 512 frames output latency, 240 input, +10 ms offset → 94 768
    assert_eq!(super::recording::aligned_start_frame(96_000, 512, 240, 10.0, 48_000), 96_000 - 512 - 240 - 480);
    assert_eq!(super::recording::aligned_start_frame(100, 512, 0, 0.0, 48_000), 0, "never negative");
    let root = std::env::temp_dir().join(format!("rawdawg-ui-take-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("T.rawdawg");
    let mut h = Harness::new(None);
    rawdawg_project::save_project_dir(&dir, &h.app.project).unwrap();
    h.app.project_path = Some(dir.clone());
    let tid = rawdawg_types::TrackId::new();
    h.app.apply(rawdawg_project::Command::AddTrack {
        track: Track { id: tid, name: "Vocal".into(), color: None, mute: false, solo: false, height: None, mixer_id: None },
        index: None,
    });
    // a finished take: 1.5 s of a mock stereo input, started at project frame 96 000 (bar 2)
    let (mut producer, queues) = CaptureProducer::new(2, InputChannels::Stereo(0, 1));
    let writer = CaptureWriter::start(&dir, queues.writer, 48_000, 96_000).unwrap();
    let data: Vec<f32> = (0..72_000)
        .flat_map(|i| {
            let v = ((i as f32) * 0.01).sin() * 0.5;
            [v, -v]
        })
        .collect();
    producer.push_interleaved(&data);
    producer.flush_partial();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let (wav, journal) = (writer.wav_path.clone(), writer.journal_path.clone());
    let (j, _queue) = writer.stop().unwrap();
    assert_eq!(j.frames_written, 72_000);
    let clips = h.app.project.clips.len();
    h.app.place_take(&wav, &journal, tid, j.start_project_frame, j.frames_written, j.lost_frames, 48_000);
    assert_eq!(h.app.project.clips.len(), clips + 1);
    let clip = h.app.project.clips.last().unwrap().clone();
    assert_eq!(clip.track_id, tid);
    assert_eq!(clip.start_tick, Tick(3840), "96 000 frames at 120 BPM = bar 2");
    assert_eq!(clip.length_ticks, Tick(2880), "1.5 s = three beats");
    let ClipContent::Audio { source_length_frames, asset_id, .. } = &clip.content else { panic!("audio clip") };
    assert_eq!(*source_length_frames, 72_000);
    assert!(h.app.project.assets.iter().any(|a| &a.id == asset_id), "take imported as a project asset");
    assert!(!wav.exists() && !journal.exists(), "imported take files are removed from recovery");
    assert!(h.app.status_line.starts_with("Recorded 1.50 s"), "{}", h.app.status_line);
    // one undo removes clip and asset together
    h.key(egui::Key::Z, egui::Modifiers::CTRL);
    assert_eq!(h.app.project.clips.len(), clips);
    // a crashed take: journal unfinished, header never finalized → recovered on open
    let (mut producer, queues) = CaptureProducer::new(1, InputChannels::Mono(0));
    let writer = CaptureWriter::start(&dir, queues.writer, 48_000, 48_000).unwrap();
    producer.push_interleaved(&vec![0.3f32; 24_000]);
    producer.flush_partial();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::mem::forget(writer);
    rawdawg_project::save_project_dir(&dir, &h.app.project).unwrap();
    h.app.open_project(dir.clone());
    assert!(h.app.status_line.starts_with("Recovered 1 unfinished"), "{}", h.app.status_line);
    let rec = h.app.project.tracks.iter().find(|t| t.name == "Recovered").expect("recovered track");
    let c = h.app.project.clips.iter().find(|c| c.track_id == rec.id).expect("recovered clip");
    assert_eq!(c.start_tick, Tick(1920), "48 000 frames = beat 3 of bar 1");
    let ClipContent::Audio { source_length_frames, .. } = &c.content else { panic!("audio clip") };
    assert!(*source_length_frames >= 768 && *source_length_frames <= 24_000, "{source_length_frames}");
    assert!(h.app.plan_error.is_none(), "{:?}", h.app.plan_error);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn browser_search_filters_and_favorites_sort_first_and_effects_add_to_the_selected_channel() {
    assert!(super::browser_matches("", &["anything"]));
    assert!(super::browser_matches("rev", &["Reverb", "Reverb family"]));
    assert!(!super::browser_matches("delay", &["Reverb"]));
    let mut h = Harness::sampler_demo();
    let asset = h.app.project.assets.iter().find(|a| a.kind == rawdawg_project::AssetKind::Audio).map(|a| a.id.to_string()).unwrap();
    h.app.toggle_favorite(&asset);
    assert!(h.app.prefs.favorites.contains(&asset));
    h.app.toggle_favorite(&asset);
    assert!(!h.app.prefs.favorites.contains(&asset));
    // the browser draws with a search term without panicking and the effect list adds inserts
    h.app.browser_search = "verb".into();
    h.frame(vec![]);
    let master = h.app.project.mixer.master_id;
    let before = h.app.project.mixer_channel(master).unwrap().inserts.len();
    let mut inst = rawdawg_project::Instance::native(rawdawg_effects::reverb::TYPE_ID, serde_json::Value::Null);
    inst.display_name = "Reverb".into();
    h.app.apply(rawdawg_project::Command::AddInsert { mixer_id: master, instance: inst, index: None });
    assert_eq!(h.app.project.mixer_channel(master).unwrap().inserts.len(), before + 1);
    // preview without audio: a clear status, nothing held
    let id = h.app.project.assets.iter().find(|a| a.kind == rawdawg_project::AssetKind::Audio).map(|a| a.id.clone()).unwrap();
    h.app.preview_asset(&id);
    assert!(h.app.plan_error.is_none());
}

#[test]
fn projects_save_as_autosave_recover_lock_and_relink() {
    use rawdawg_project::{Command, Note};
    use rawdawg_types::NoteId;
    let root = std::env::temp_dir().join(format!("rawdawg-ui-files-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    // new project → Save As creates the folder, locks it and clears the dirty flag
    let mut h = Harness::new(None);
    h.app.new_project();
    assert!(h.app.project_path.is_none());
    let dir = root.join("Song.rawdawg");
    h.app.save_project_as(dir.clone());
    assert_eq!(h.app.project_path.as_deref(), Some(dir.as_path()));
    assert!(dir.join("project.json").is_file() && dir.join("recovery/lock.json").is_file());
    assert!(!h.app.history.is_dirty());
    assert!(h.app.files.error.is_none(), "{:?}", h.app.files.error);
    // an edit makes it dirty; an autosave records it without touching project.json
    let pid = h.app.project.patterns[0].id;
    let cid = h.app.project.channels[0].id;
    h.app.apply(Command::AddNotes {
        pattern_id: pid,
        channel_id: cid,
        notes: vec![Note { id: NoteId::new(), tick: Tick(0), duration_ticks: Tick(480), pitch: 64, velocity: 0.7, channel: 0 }],
    });
    assert!(h.app.history.is_dirty());
    h.app.autosave_now(&dir);
    assert!(h.app.files.last_autosave_note.as_ref().is_some_and(|n| n.is_ok()), "{:?}", h.app.files.last_autosave_note);
    assert_eq!(rawdawg_project::load_project_dir(&dir).unwrap().patterns[0].lanes[0].notes.len(), 0, "saved manifest unchanged by the autosave");
    // "crash": a second app opens the folder → the lock from the first instance is ours (same pid → not live), recovery offered
    drop(h);
    let mut h2 = Harness::new(None);
    h2.app.open_project_checked(dir.clone());
    assert!(h2.app.files.read_only.is_none(), "{:?}", h2.app.files.read_only);
    assert_eq!(h2.app.files.recovery.len(), 1, "one newer autosave");
    let cand = h2.app.files.recovery[0].clone();
    h2.app.recover_from(&cand);
    assert_eq!(h2.app.project.patterns[0].lanes[0].notes.len(), 1, "recovered the unsaved note");
    assert!(h2.app.history.is_dirty(), "recovered state must be saved explicitly");
    h2.app.save_project_checked();
    assert!(!h2.app.history.is_dirty());
    assert!(rawdawg_project::recovery_candidates(&dir).is_empty(), "autosaves cleared after a save");
    assert_eq!(rawdawg_project::load_project_dir(&dir).unwrap().revision, h2.app.history.revision());
    // a foreign live lock makes the project read-only; taking over enables saving
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    let foreign = rawdawg_project::lock::LockInfo { pid: 1, host: "elsewhere".into(), acquired_unix_ms: now };
    drop(h2);
    std::fs::write(dir.join("recovery/lock.json"), serde_json::to_vec(&foreign).unwrap()).unwrap();
    let mut h3 = Harness::new(None);
    h3.app.open_project_checked(dir.clone());
    assert!(h3.app.files.read_only.is_some());
    h3.app.apply(Command::RenameProject { name: "renamed".into() });
    h3.app.save_project_checked();
    assert!(h3.app.status_line.starts_with("Read-only"), "{}", h3.app.status_line);
    assert_eq!(rawdawg_project::load_project_dir(&dir).unwrap().name, "Song", "not written while read-only (named after its folder at Save As)");
    h3.app.take_over_lock();
    assert!(h3.app.files.read_only.is_none());
    h3.app.save_project_checked();
    assert_eq!(rawdawg_project::load_project_dir(&dir).unwrap().name, "renamed");
    // relink: import a sample, delete its file → missing; relinking the same file restores it
    let wav = root.join("tone.wav");
    let spec = hound::WavSpec { channels: 1, sample_rate: 48_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(&wav, spec).unwrap();
    for i in 0..4800 {
        w.write_sample(((i % 100) as i16) * 100).unwrap();
    }
    w.finalize().unwrap();
    let id = h3.app.import_asset_only(&wav).unwrap();
    let asset = h3.app.project.assets.iter().find(|a| a.id.to_string() == id).cloned().unwrap();
    assert!(!h3.app.asset_is_missing(&asset));
    std::fs::remove_file(dir.join(&asset.path)).unwrap();
    assert!(h3.app.asset_is_missing(&asset));
    h3.app.relink_asset(&asset.id, &wav);
    assert!(!h3.app.asset_is_missing(&asset), "same content restored the file");
    assert!(h3.app.status_line.contains("same content"), "{}", h3.app.status_line);
    // relinking with different content replaces the asset (undoable)
    let other = root.join("other.wav");
    let mut w = hound::WavWriter::create(&other, spec).unwrap();
    for i in 0..4800 {
        w.write_sample(((i % 50) as i16) * 100).unwrap();
    }
    w.finalize().unwrap();
    h3.app.relink_asset(&asset.id, &other);
    assert!(h3.app.project.assets.iter().all(|a| a.id != asset.id), "old id replaced");
    assert!(h3.app.status_line.contains("Replaced"), "{}", h3.app.status_line);
    h3.key(egui::Key::Z, egui::Modifiers::CTRL);
    assert!(h3.app.project.assets.iter().any(|a| a.id == asset.id), "undo restores the original asset entry");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn missing_plugins_are_reported_without_blocking_and_the_help_window_draws() {
    use rawdawg_project::{Command, Instance, PluginFormat};
    let mut h = Harness::new(None);
    let mut ext = Instance::native("com.example.fx", serde_json::json!({ "opaque": true }));
    ext.format = PluginFormat::Clap;
    ext.display_name = "Example FX".into();
    let master = h.app.project.mixer.master_id;
    h.app.apply(Command::AddInsert { mixer_id: master, instance: ext, index: None });
    assert!(h.app.plan_error.is_none(), "{:?}", h.app.plan_error);
    assert!(h.app.status_line.contains("not available") && h.app.status_line.contains("Example FX"), "{}", h.app.status_line);
    h.app.editor_view = EditorView::Mixer;
    h.app.mixer.selected = Some(master);
    h.frame(vec![]);
    h.app.show_help = true;
    h.frame(vec![]);
    h.frame(vec![]);
    assert!(h.app.show_help);
    // undo removes the stand-in insert again
    h.key(egui::Key::Z, egui::Modifiers::CTRL);
    assert!(h.app.project.mixer_channel(master).unwrap().inserts.is_empty());
}

/// Build (cached) and describe rawDAWg's own CLAP module so tests have a real catalog entry.
fn hosted_catalog() -> rawdawg_plugin_host::ScannedLibrary {
    let status = std::process::Command::new(env!("CARGO")).args(["build", "-q", "-p", "rawdawg-plugin-clap"]).status().expect("cargo build");
    assert!(status.success());
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap();
    let name = if cfg!(target_os = "windows") { "rawdawg_clap.dll" } else { "librawdawg_clap.so" };
    // SAFETY: our own freshly built module.
    unsafe { rawdawg_plugin_host::describe::describe_clap_library(&dir.join(name)) }.expect("describe")
}

#[test]
fn hosted_plugins_are_added_from_the_catalog_edited_through_mirrors_and_shown_in_settings() {
    use rawdawg_plugin_host::catalog::{Fingerprint, QuarantineReason, QuarantineRecord, now_unix};
    let mut h = Harness::new(None);
    assert!(h.app.plugin_host.catalog_path.is_none(), "headless tests never touch the user's catalog");
    let lib = hosted_catalog();
    let module_path = lib.path.clone();
    h.app.plugin_host.catalog.insert_library(lib);
    assert_eq!(h.app.plugin_host.catalog.plugins().count(), 17);

    // browser lists catalog plugins and the search filters them
    h.app.browser_search = "rawdawg.fx.rev".into();
    h.frame(vec![]);
    // an effect from the catalog becomes an external insert on the master
    let master = h.app.project.mixer.master_id;
    let (reverb, _) = h.app.plugin_host.catalog.find(rawdawg_plugin_host::Format::Clap, "rawdawg.fx.reverb", None).map(|(l, p)| (p.clone(), l.path.clone())).unwrap();
    h.app.add_catalog_plugin(&reverb, &module_path, master);
    let inserts = h.app.project.mixer_channel(master).unwrap().inserts.clone();
    assert_eq!(inserts.len(), 1);
    let ext = &inserts[0];
    assert_eq!(ext.format, rawdawg_project::PluginFormat::Clap);
    assert_eq!(ext.type_id.as_str(), "rawdawg.fx.reverb");
    assert_eq!(ext.display_name, "rawDAWg Reverb");
    assert_eq!(ext.path_hint.as_deref(), Some(module_path.display().to_string().as_str()));
    assert!(!h.app.status_line.contains("not available"), "{}", h.app.status_line);
    assert_eq!(h.app.editor_view, EditorView::Effect);
    assert_eq!(h.app.effect_editor, Some(ext.id));
    // the effect panel creates the live main-thread instance as the mirror and draws its parameters
    h.frame(vec![]);
    let mirror = h.app.other_mirrors.get_mut(&ext.id).expect("hosted mirror");
    assert_eq!(rawdawg_dsp::processor::Processor::type_id(mirror.as_ref()), "clap:rawdawg.fx.reverb");
    let descs = mirror.descriptors();
    assert!(descs.len() > 5, "{}", descs.len());
    let bypass = descs.iter().find(|d| d.flags.is_bypass).expect("wrapper bypass exposed");
    let first = descs.iter().find(|d| !d.flags.is_bypass && !d.flags.stepped).expect("a continuous parameter");
    // committing a state from the mirror stores the base64 envelope in the project
    assert!(mirror.set_param(first.id, 0.33));
    let state: serde_json::Value = serde_json::from_slice(&mirror.save_state()).unwrap();
    assert_eq!(state["format"], "clap");
    assert!(state[rawdawg_plugin_host::clap::STATE_KEY].as_str().is_some_and(|s| !s.is_empty()));
    assert_eq!(mirror.param(bypass.id), Some(0.0));
    h.app.apply(rawdawg_project::Command::SetInstanceState { instance_id: ext.id, state_version: 1, state: state.clone() });
    assert_eq!(h.app.project.instance(ext.id).unwrap().state, state);
    // the mirror survives model edits (it is the live plugin instance)
    h.app.rebuild_mirrors();
    assert!(h.app.other_mirrors.contains_key(&ext.id));

    // an instrument from the catalog replaces the selected channel's instrument, with undo
    let channel = h.app.project.channels[0].id;
    let old_instrument = h.app.project.channels[0].instrument.clone();
    h.app.selected_channel = Some(channel);
    let synth = h.app.plugin_host.catalog.find(rawdawg_plugin_host::Format::Clap, "rawdawg.synth", None).map(|(_, p)| p.clone()).unwrap();
    h.app.add_catalog_plugin(&synth, &module_path, master);
    let now = h.app.project.channels[0].instrument.clone();
    assert_eq!(now.format, rawdawg_project::PluginFormat::Clap);
    assert_eq!(now.type_id.as_str(), "rawdawg.synth");
    h.frame(vec![]); // instrument panel draws the hosted instrument
    assert!(h.app.other_mirrors.get(&now.id).is_some_and(|m| rawdawg_dsp::processor::Processor::type_id(m.as_ref()) == "clap:rawdawg.synth"));
    h.key(egui::Key::Z, egui::Modifiers::CTRL);
    assert_eq!(h.app.project.channels[0].instrument, old_instrument, "undo restores the native instrument");

    // settings: quarantine entries are listed and retry clears the record; the window draws
    h.app.plugin_host.catalog.insert_quarantine(QuarantineRecord {
        path: PathBuf::from("/tmp/broken.clap"),
        fingerprint: Some(Fingerprint { size: 1, mtime_unix: 1 }),
        reason: QuarantineReason::Timeout { secs: 15 },
        at_unix: now_unix(),
    });
    h.app.show_settings = true;
    h.frame(vec![]);
    h.frame(vec![]);
    assert!(h.app.plugin_host.catalog.clear_quarantine(std::path::Path::new("/tmp/broken.clap")));
    assert!(h.app.plugin_host.catalog.quarantine.is_empty());

    // a catalog plugin that is not hostable (unknown id) stays a stand-in with a reason
    let mut unknown = rawdawg_project::Instance::external(rawdawg_project::PluginFormat::Clap, "com.example.missing", "Missing FX", "Nobody", "1.0", None);
    unknown.state = serde_json::json!({ "format": "clap", "plugin_id": "com.example.missing", "clap_state_b64": "AAAA" });
    let saved_state = unknown.state.clone();
    let uid = unknown.id;
    h.app.apply(rawdawg_project::Command::AddInsert { mixer_id: master, instance: unknown, index: None });
    assert!(h.app.status_line.contains("not available") && h.app.status_line.contains("catalog"), "{}", h.app.status_line);
    assert_eq!(h.app.project.instance(uid).unwrap().state, saved_state, "opaque state untouched");
    h.app.effect_editor = Some(uid);
    h.frame(vec![]);
    assert!(!h.app.other_mirrors.contains_key(&uid), "no mirror for an unavailable plugin");

    // the same flow for a VST3 bundle: described, listed, inserted and mirrored through the VST3 bridge
    let bundle = {
        let exe = std::env::current_exe().unwrap();
        let target = exe.parent().unwrap().parent().unwrap();
        let status = std::process::Command::new(env!("CARGO")).args(["build", "-q", "-p", "rawdawg-plugin-vst3"]).status().expect("cargo build");
        assert!(status.success());
        let base = std::env::temp_dir().join(format!("rawdawg-ui-vst3-{}", std::process::id()));
        let arch = if cfg!(target_os = "windows") { "x86_64-win" } else { "x86_64-linux" };
        let inner = base.join("rawDAWg.vst3").join("Contents").join(arch);
        std::fs::create_dir_all(&inner).unwrap();
        let name = if cfg!(target_os = "windows") { "rawdawg_vst3.dll" } else { "librawdawg_vst3.so" };
        std::fs::copy(target.join(name), inner.join(if cfg!(target_os = "windows") { "rawDAWg.vst3" } else { "rawDAWg.so" })).unwrap();
        base.join("rawDAWg.vst3")
    };
    // SAFETY: our own freshly built module.
    let vst3_lib = unsafe { rawdawg_plugin_host::vst3::describe_vst3_module(&bundle) }.expect("describe vst3");
    h.app.plugin_host.catalog.insert_library(vst3_lib);
    assert_eq!(h.app.plugin_host.catalog.plugins().count(), 34);
    let (vreverb, vpath) = h
        .app
        .plugin_host
        .catalog
        .plugins()
        .find(|(_, p)| p.format == rawdawg_plugin_host::Format::Vst3 && p.name == "rawDAWg Reverb")
        .map(|(l, p)| (p.clone(), l.path.clone()))
        .unwrap();
    h.app.add_catalog_plugin(&vreverb, &vpath, master);
    let vinst = h.app.project.mixer_channel(master).unwrap().inserts.iter().find(|i| i.format == rawdawg_project::PluginFormat::Vst3).cloned().expect("VST3 insert");
    assert_eq!(vinst.type_id.as_str(), vreverb.plugin_id);
    h.frame(vec![]);
    let vm = h.app.other_mirrors.get_mut(&vinst.id).expect("hosted VST3 mirror");
    assert!(rawdawg_dsp::processor::Processor::type_id(vm.as_ref()).starts_with("vst3:"));
    assert!(vm.descriptors().iter().any(|d| d.flags.is_bypass));
    let vstate: serde_json::Value = serde_json::from_slice(&vm.save_state()).unwrap();
    assert_eq!(vstate["format"], "vst3");
    let _ = std::fs::remove_dir_all(bundle.parent().unwrap());

    // safe mode: catalog plugins are never loaded
    h.app.plugin_host.safe_mode = true;
    h.app.other_mirrors.clear();
    h.app.effect_editor = Some(ext.id);
    h.frame(vec![]);
    assert!(!h.app.other_mirrors.contains_key(&ext.id));
    assert!(h.app.plugin_host.unavailable_reason(h.app.project.instance(ext.id).unwrap()).is_some_and(|r| r.contains("safe mode")));
}

#[test]
fn open_accepts_preset_files_and_the_project_marker() {
    let dir = std::env::temp_dir().join(format!("rawdawg-open-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // synth preset: loads onto the synth channel of a fresh project
    let mut patch = rawdawg_synthesizer::presets::factory_presets()[3].patch.clone();
    patch.name = "Opened Sound".into();
    let synth_file = dir.join("Opened Sound.rawdawgpreset");
    rawdawg_project::preset::write_preset(&synth_file, &rawdawg_project::preset::PresetManifest::new_synth(&patch.name, &patch, &[]), &[]).unwrap();
    let h = Harness::new(Some(synth_file.to_str().unwrap()));
    assert!(h.app.status_line.contains("Opened Sound"), "{}", h.app.status_line);
    assert!(h.app.project_path.is_none(), "a preset opens into a new, unsaved project");
    assert_eq!(h.app.project.channels[0].instrument.state["name"], "Opened Sound");
    // effect preset: becomes an insert on the master, undoable
    let reverb_state = rawdawg_effects::preset_state(rawdawg_effects::reverb::TYPE_ID, &rawdawg_effects::presets(rawdawg_effects::reverb::TYPE_ID)[0]).unwrap();
    let state: serde_json::Value = serde_json::from_slice(&reverb_state).unwrap();
    let fx_file = dir.join("Hall.rawdawgpreset");
    rawdawg_project::preset::write_preset(&fx_file, &rawdawg_project::preset::PresetManifest::new_effect("Hall", rawdawg_effects::reverb::TYPE_ID, state.clone()), &[]).unwrap();
    let mut h = Harness::new(Some(fx_file.to_str().unwrap()));
    let master = h.app.project.mixer.master_id;
    let inserts = h.app.project.mixer_channel(master).unwrap().inserts.clone();
    assert_eq!(inserts.len(), 1, "{}", h.app.status_line);
    assert_eq!(inserts[0].type_id.as_str(), rawdawg_effects::reverb::TYPE_ID);
    assert_eq!(inserts[0].state, state);
    assert_eq!(h.app.editor_view, EditorView::Effect);
    h.key(egui::Key::Z, egui::Modifiers::CTRL);
    assert!(h.app.project.mixer_channel(master).unwrap().inserts.is_empty());
    // a broken preset reports and leaves the default project untouched
    let bad = dir.join("Broken.rawdawgpreset");
    std::fs::write(&bad, b"not a zip").unwrap();
    let h = Harness::new(Some(bad.to_str().unwrap()));
    assert!(h.app.status_line.contains("Could not open preset"), "{}", h.app.status_line);
    // the project marker opens the folder it sits in
    let proj = dir.join("Song.rawdawg");
    let p = rawdawg_project::Project::new_default("Marker Song", serde_json::json!({}));
    rawdawg_project::persist::save_project_dir(&proj, &p).unwrap();
    let marker = proj.join(rawdawg_project::persist::PROJECT_MARKER);
    let h = Harness::new(Some(marker.to_str().unwrap()));
    assert_eq!(h.app.project.project_id, p.project_id, "{}", h.app.status_line);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hosted_effect_panel_shows_the_plugins_value_text_and_named_choices() {
    let mut h = Harness::new(None);
    let lib = hosted_catalog();
    let module_path = lib.path.clone();
    h.app.plugin_host.catalog.insert_library(lib);
    let master = h.app.project.mixer.master_id;
    let delay = h.app.plugin_host.catalog.find(rawdawg_plugin_host::Format::Clap, "rawdawg.fx.delay", None).map(|(_, p)| p.clone()).unwrap();
    h.app.add_catalog_plugin(&delay, &module_path, master);
    let ext = h.app.project.mixer_channel(master).unwrap().inserts[0].clone();
    assert_eq!(h.app.effect_editor, Some(ext.id));
    h.frame(vec![]);
    let native = rawdawg_effects::make_effect("native.fx.delay", None).unwrap();
    let mirror = h.app.other_mirrors.get(&ext.id).expect("hosted mirror");
    let hosted: Vec<rawdawg_types::ParamDescriptor> = mirror.descriptors().to_vec();
    let find = |key: &str| {
        let nd = *native.descriptors().iter().find(|d| d.key == key).unwrap();
        (nd, *hosted.iter().find(|d| d.id == nd.id).unwrap())
    };
    // hosted descriptors carry no labels; the step names come from the plugin itself
    let (sync_native, sync) = find("sync");
    assert!(sync.labels.is_none());
    let names = super::effect_panel::step_labels(&**mirror, &sync).expect("the plugin names every step");
    let expected: Vec<String> = sync_native.labels.unwrap().iter().map(|l| (*l).to_owned()).collect();
    assert_eq!(names, expected);
    // readouts in the drawn panel are the plugin's text, not a bare 0..1 number
    let (time_native, time) = find("time");
    let time_value = mirror.param(time.id).unwrap();
    let time_text = rawdawg_dsp::processor::display_value(&**mirror, &time, time_value);
    assert_eq!(time_text, time_native.format(time_value));
    assert_ne!(time_text, time.format(time_value), "the descriptor alone would show {}", time.format(time_value));
    let texts = h.drawn_texts();
    assert!(texts.iter().any(|t| t.contains(&time_text)), "{time_text:?} not drawn in {texts:?}");
    let sync_now = names[sync.to_physical(h.app.other_mirrors[&ext.id].param(sync.id).unwrap()) as usize].clone();
    assert!(texts.iter().any(|t| t == &sync_now), "the choice list shows {sync_now:?}");
    // choosing another step reaches the plugin and the list follows
    let third = sync.to_normalized(3.0);
    assert!(h.app.other_mirrors.get_mut(&ext.id).unwrap().set_param(sync.id, third));
    let texts = h.drawn_texts();
    assert!(texts.iter().any(|t| t == &names[3]), "{:?} not drawn after the change", names[3]);
    // automation readouts use the same text
    let target = rawdawg_project::AutomationTarget { instance_id: ext.id, param_id: time.id.0 };
    assert_eq!(super::automation::value_text(&h.app, &target, Some(&time), 0.5), time_native.format(0.5));
}

#[test]
fn large_hosted_plugin_state_is_saved_beside_the_manifest_and_a_missing_file_keeps_a_stand_in() {
    use rawdawg_project::plugin_state::{FILE_REF_KEY, SPILL_ABOVE_BYTES};
    let root = std::env::temp_dir().join(format!("rawdawg-ui-pstate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("Big.rawdawg");
    rawdawg_project::save_project_dir(&dir, &rawdawg_project::Project::new_default("Big", serde_json::json!({}))).unwrap();
    let dir_s = dir.to_str().unwrap().to_owned();
    let open = |with_catalog: bool| {
        let mut h = Harness::new(Some(&dir_s));
        if with_catalog {
            h.app.plugin_host.catalog.insert_library(hosted_catalog());
            h.app.rebuild_mirrors();
            h.app.recompile();
            h.frame(vec![]);
        }
        h
    };

    // the hosted rawDAWg Synth (CLAP) replaces channel 1's instrument; one parameter is edited
    let mut h = open(true);
    let lib_path = hosted_catalog().path;
    let synth = h.app.plugin_host.catalog.find(rawdawg_plugin_host::Format::Clap, "rawdawg.synth", None).map(|(_, p)| p.clone()).unwrap();
    h.app.selected_channel = Some(h.app.project.channels[0].id);
    let master = h.app.project.mixer.master_id;
    h.app.add_catalog_plugin(&synth, &lib_path, master);
    h.frame(vec![]);
    let id = h.app.project.channels[0].instrument.id;
    let mirror = h.app.other_mirrors.get_mut(&id).expect("hosted synth");
    let cutoff = *mirror.descriptors().iter().find(|d| d.name.contains("Cutoff")).expect("a cutoff parameter");
    assert!(mirror.set_param(cutoff.id, 0.37));
    let state: serde_json::Value = serde_json::from_slice(&mirror.save_state()).unwrap();
    h.app.apply(rawdawg_project::Command::SetInstanceState { instance_id: id, state_version: 1, state: state.clone() });
    let raw = rawdawg_plugin_host::b64::decode(state[rawdawg_plugin_host::clap::STATE_KEY].as_str().unwrap()).unwrap();
    assert!(raw.len() > SPILL_ABOVE_BYTES, "the synth's own state is {} bytes", raw.len());
    h.app.save_project_checked();
    assert!(h.app.status_line.starts_with("Saved"), "{}", h.app.status_line);
    drop(h);

    // on disk: the manifest names the blob, the blob holds the plugin's bytes
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("project.json")).unwrap()).unwrap();
    let field = &manifest["channels"][0]["instrument"]["state"][rawdawg_plugin_host::clap::STATE_KEY];
    let rel = field[FILE_REF_KEY].as_str().expect("a plugin-state reference").to_owned();
    assert_eq!(field["bytes"], raw.len());
    assert_eq!(std::fs::read(dir.join(&rel)).unwrap(), raw);
    assert!(std::fs::metadata(dir.join("project.json")).unwrap().len() < raw.len() as u64, "the manifest no longer carries the blob");

    // reopening restores the edited value in the live plugin
    let h = open(true);
    let live = h.app.other_mirrors.get(&id).expect("hosted again after reopening");
    assert!(rawdawg_plugin_host::is_hosted(live.as_ref()));
    assert!((live.param(cutoff.id).unwrap() - 0.37).abs() < 1e-4);
    assert_eq!(h.app.project.channels[0].instrument.state, state, "the loaded model carries the state inline");
    drop(h);

    // without the file the instrument stays a stand-in and saving keeps the reference
    let blob = dir.join(&rel);
    let kept = root.join("blob.bin");
    std::fs::rename(&blob, &kept).unwrap();
    let mut h = open(true);
    assert!(h.app.other_mirrors.get(&id).is_none_or(|m| !rawdawg_plugin_host::is_hosted(m.as_ref())), "not hosted with a default state");
    let notes = missing_plugin_notes(&h.app.project, &h.app.plugin_host).join("; ");
    assert!(notes.contains("missing or damaged") && notes.contains(&rel), "{notes}");
    h.app.save_project_checked();
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("project.json")).unwrap()).unwrap();
    assert_eq!(manifest["channels"][0]["instrument"]["state"][rawdawg_plugin_host::clap::STATE_KEY][FILE_REF_KEY], rel.as_str(), "reference saved unchanged");
    drop(h);

    // once the file is back, the plugin and its state are back
    std::fs::rename(&kept, &blob).unwrap();
    let h = open(true);
    let live = h.app.other_mirrors.get(&id).expect("hosted once the file is back");
    assert!((live.param(cutoff.id).unwrap() - 0.37).abs() < 1e-4);

    // a saved state the plugin cannot restore also keeps a stand-in (never a default-state plugin)
    let mut damaged = h.app.project.channels[0].instrument.clone();
    damaged.id = rawdawg_types::InstanceId::new();
    damaged.state[rawdawg_plugin_host::clap::STATE_KEY] = serde_json::json!("@@@@");
    let mut mirrors = std::collections::HashMap::new();
    let mut h = h;
    let err = crate::plugins::ensure_external_mirror(&mut h.app.plugin_host, &mut mirrors, &damaged).expect_err("refused");
    assert!(err.contains("could not be restored") && err.contains("keeps the state"), "{err}");
    assert!(mirrors.is_empty());
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn insert_drop_slot_is_the_gap_nearest_the_pointer() {
    use super::mixer::insert_drop_index;
    let rows: Vec<egui::Rect> = (0..3).map(|i| egui::Rect::from_min_size(egui::pos2(0.0, 20.0 * i as f32), egui::vec2(100.0, 20.0))).collect();
    assert_eq!(insert_drop_index(&rows, 0, 70.0), Some(2), "below the last row: to the end");
    assert_eq!(insert_drop_index(&rows, 0, 5.0), None, "its own place");
    assert_eq!(insert_drop_index(&rows, 0, 25.0), None, "the gap right after itself is its own place");
    assert_eq!(insert_drop_index(&rows, 0, 35.0), Some(1));
    assert_eq!(insert_drop_index(&rows, 2, -10.0), Some(0), "above the first row: to the start");
    assert_eq!(insert_drop_index(&rows, 1, 45.0), None);
    assert_eq!(insert_drop_index(&rows, 1, 55.0), Some(2));
    assert_eq!(insert_drop_index(&rows, 5, 55.0), None, "unknown source row");
}

#[test]
fn inserts_are_reordered_by_dragging_their_handle_in_one_undo_step() {
    let mut h = Harness::new(None);
    let master = h.app.project.mixer.master_id;
    for fx in rawdawg_effects::CATALOG.iter().filter(|f| f.listed).take(3) {
        let mut inst = rawdawg_project::Instance::native(fx.type_id, serde_json::Value::Null);
        inst.display_name = fx.name.to_owned();
        h.app.apply(Command::AddInsert { mixer_id: master, instance: inst, index: None });
    }
    let order = |h: &Harness| h.app.project.mixer_channel(master).unwrap().inserts.iter().map(|i| i.id).collect::<Vec<_>>();
    h.app.editor_view = EditorView::Mixer;
    h.app.mixer.selected = Some(master);
    h.frame(vec![]);
    h.frame(vec![]);
    // the insert list sits below the strips: scroll the editor down to it, as a user would
    h.scroll(egui::pos2(W / 2.0, H - 120.0), -600.0);
    let o = order(&h);
    h.settle(super::mixer::insert_handle_id(o[0]));
    assert_eq!(o.len(), 3);
    let depth = h.app.history.undo_len();
    // first insert dragged below the last one
    let first = h.rect(super::mixer::insert_handle_id(o[0]));
    let last = h.rect(super::mixer::insert_handle_id(o[2]));
    h.drag(first.center(), egui::pos2(last.center().x, last.bottom() + 6.0), egui::PointerButton::Primary);
    assert_eq!(order(&h), vec![o[1], o[2], o[0]]);
    assert_eq!(h.app.history.undo_len(), depth + 1, "one undo step");
    h.key(egui::Key::Z, egui::Modifiers::CTRL);
    assert_eq!(order(&h), o, "undo restores the chain");
    // last insert dragged above the first one
    let first = h.rect(super::mixer::insert_handle_id(o[0]));
    let last = h.rect(super::mixer::insert_handle_id(o[2]));
    h.drag(last.center(), egui::pos2(first.center().x, first.top() - 4.0), egui::PointerButton::Primary);
    assert_eq!(order(&h), vec![o[2], o[0], o[1]]);
    // a drag that ends in its own place changes nothing
    let depth = h.app.history.undo_len();
    let middle = h.rect(super::mixer::insert_handle_id(o[0]));
    h.drag(middle.center(), middle.center() + egui::vec2(30.0, 3.0), egui::PointerButton::Primary);
    assert_eq!(order(&h), vec![o[2], o[0], o[1]]);
    assert_eq!(h.app.history.undo_len(), depth);
    assert!(h.app.plan_error.is_none(), "{:?}", h.app.plan_error);
}

#[test]
fn a_long_status_message_or_project_path_never_overlaps_the_rest_of_the_status_bar() {
    let mut h = Harness::new(None);
    h.app.status_line = format!("Opened /{}", "a-rather-long-folder-name/".repeat(30));
    h.app.project_path = Some(PathBuf::from(format!("/{}/Song.rawdawg", "deep/".repeat(60))));
    let nodes = h.drawn_nodes();
    // only widgets in the status bar (the bottom row); the toolbar has its own "Audio off"
    let find = |pred: &dyn Fn(&str) -> bool, what: &str| {
        nodes.iter().find(|(t, r)| r.center().y > H - 40.0 && pred(t)).map(|(_, r)| *r).unwrap_or_else(|| panic!("{what} not drawn in the status bar"))
    };
    let status = find(&|t| t.starts_with("Opened /a-rather-long"), "status message");
    let audio = find(&|t| t.starts_with("Audio off —"), "audio status");
    let project = find(&|t| t == "Song.rawdawg", "project folder name");
    let brand = find(&|t| t.contains("Less thinking. More noise."), "version and tagline");
    assert!(status.right() <= audio.left(), "message {status:?} runs into the audio status {audio:?}");
    assert!(audio.right() <= project.left(), "audio status {audio:?} / project {project:?}");
    assert!(project.right() <= brand.left(), "project {project:?} / brand {brand:?}");
    for (what, r) in [("message", status), ("audio status", audio), ("project", project), ("brand", brand)] {
        assert!(r.left() >= 0.0 && r.right() <= W + 0.5, "{what} {r:?} leaves the window");
    }
    assert!(status.width() > 100.0, "the message keeps a useful width");
}

/// A hosted rawDAWg reverb (CLAP) as an insert on the master, its panel shown.
fn hosted_reverb_on_master(h: &mut Harness) -> rawdawg_types::InstanceId {
    let lib = hosted_catalog();
    let module_path = lib.path.clone();
    h.app.plugin_host.catalog.insert_library(lib);
    let master = h.app.project.mixer.master_id;
    let reverb = h.app.plugin_host.catalog.find(rawdawg_plugin_host::Format::Clap, "rawdawg.fx.reverb", None).map(|(_, p)| p.clone()).unwrap();
    h.app.add_catalog_plugin(&reverb, &module_path, master);
    h.frame(vec![]);
    h.app.project.mixer_channel(master).unwrap().inserts[0].id
}

#[test]
fn edits_in_a_plugin_window_are_committed_once_editing_pauses_as_one_undo_step() {
    let mut h = Harness::new(None);
    let id = hosted_reverb_on_master(&mut h);
    let stored = h.app.project.instance(id).unwrap().state.clone();
    let depth = h.app.history.undo_len();
    // the plugin's own state changes (as its editor would change it)
    let first = *h.app.other_mirrors[&id].descriptors().iter().find(|d| !d.flags.is_bypass).unwrap();
    assert!(h.app.other_mirrors.get_mut(&id).unwrap().set_param(first.id, 0.123));
    // a recent edit is not committed yet ...
    h.app.plugin_editor_edits.insert(id, std::time::Instant::now());
    h.frame(vec![]);
    assert_eq!(h.app.project.instance(id).unwrap().state, stored, "still editing");
    // ... once editing paused, the state is stored as one undo step
    h.app.plugin_editor_edits.insert(id, std::time::Instant::now() - crate::plugins::EDITOR_COMMIT_AFTER);
    h.frame(vec![]);
    assert_ne!(h.app.project.instance(id).unwrap().state, stored);
    assert_eq!(h.app.history.undo_len(), depth + 1);
    assert!(h.app.status_line.contains("plugin window edit"), "{}", h.app.status_line);
    assert!(h.app.plugin_editor_edits.is_empty());
    h.key(egui::Key::Z, egui::Modifiers::CTRL);
    assert_eq!(h.app.project.instance(id).unwrap().state, stored, "undo restores the stored state");
}

#[test]
#[ignore = "needs a display server; run by `cargo xtask verify` (GUI-02)"]
fn the_effect_panel_opens_and_closes_a_hosted_plugins_own_window() {
    let mut h = Harness::new(None);
    let id = hosted_reverb_on_master(&mut h);
    let click_text = |h: &mut Harness, text: &str| {
        let r = h.drawn_nodes().into_iter().find(|(t, _)| t == text).map(|(_, r)| r).unwrap_or_else(|| panic!("{text:?} not drawn"));
        h.click_at(r.center(), egui::PointerButton::Primary);
    };
    let is_open = |h: &mut Harness| h.app.other_mirrors.get_mut(&id).and_then(|m| rawdawg_plugin_host::hosted_mut(m.as_mut())).is_some_and(|x| x.editor_is_open());
    click_text(&mut h, "Open plugin window");
    assert!(is_open(&mut h), "{}", h.app.status_line);
    for _ in 0..20 {
        h.frame(vec![]);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(is_open(&mut h));
    click_text(&mut h, "Close plugin window");
    assert!(!is_open(&mut h));
    click_text(&mut h, "Open plugin window");
    assert!(is_open(&mut h), "reopens");
}

/// Open the "…" menu of rack row `index` (right-click) and click the entry `item`.
fn rack_menu(h: &mut Harness, index: usize, item: &str) {
    open_rack_menu(h, index, egui::PointerButton::Secondary);
    let entry = h.drawn_nodes().into_iter().find(|(t, _)| t == item).map(|(_, r)| r).unwrap_or_else(|| panic!("menu entry {item:?} not drawn"));
    h.click_at(entry.center(), egui::PointerButton::Primary);
}

/// Click the "…" button of rack row `index` with `button`.
fn open_rack_menu(h: &mut Harness, index: usize, button: egui::PointerButton) {
    h.frame(vec![]);
    let grip = h.rect(super::rack_grip_id(h.app.project.channels[index].id));
    let r = h
        .drawn_nodes()
        .into_iter()
        .find(|(t, r)| t == "…" && r.center().y > grip.top() && r.center().y < grip.bottom() && r.left() > grip.right())
        .map(|(_, r)| r)
        .unwrap_or_else(|| panic!("rack row {index} has no … button"));
    h.click_at(r.center(), button);
    // a popup's first frame is an invisible sizing pass; let it take its place
    h.frame(vec![]);
}

#[test]
fn rack_channels_are_duplicated_from_their_menu_and_reordered_by_dragging() {
    let mut h = Harness::new(None);
    h.app.editor_view = EditorView::Rack;
    h.frame(vec![]);
    let first = h.app.project.channels[0].clone();
    // put two notes on the first channel through its step row
    for step in [0usize, 4] {
        let r = h.rect(egui::Id::unique(("step", first.id, step)));
        h.click_at(r.center(), egui::PointerButton::Primary);
    }
    assert_eq!(h.lane_notes().len(), 2);
    let names = |h: &Harness| h.app.project.channels.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
    let lane_len = |h: &Harness, id| h.app.project.patterns[0].lanes.iter().find(|l| l.channel_id == id).map_or(0, |l| l.notes.len());

    // Duplicate: the sound and settings below the original, selected, no notes
    let depth = h.app.history.undo_len();
    rack_menu(&mut h, 0, "Duplicate");
    assert_eq!(names(&h), vec![first.name.clone(), format!("{} 2", first.name)]);
    let copy = h.app.project.channels[1].clone();
    assert_eq!(copy.instrument.state, first.instrument.state);
    assert_ne!(copy.instrument.id, first.instrument.id);
    assert_eq!(h.app.selected_channel, Some(copy.id), "the copy is selected");
    assert_eq!(lane_len(&h, copy.id), 0);
    assert_eq!(h.app.history.undo_len(), depth + 1);

    // Duplicate with notes (from the original): a third channel right below it, with notes
    rack_menu(&mut h, 0, "Duplicate with notes");
    assert_eq!(names(&h), vec![first.name.clone(), format!("{} 3", first.name), format!("{} 2", first.name)]);
    let with_notes = h.app.project.channels[1].id;
    assert_eq!(lane_len(&h, with_notes), 2, "the notes came along");
    h.key(egui::Key::Z, egui::Modifiers::CTRL);
    assert_eq!(h.app.project.channels.len(), 2, "one undo removes the copy and its notes");
    assert_eq!(lane_len(&h, first.id), 2, "the original keeps its notes");

    // drag the first row's grip below the last row: it moves to the end (one undo step)
    let order = |h: &Harness| h.app.project.channels.iter().map(|c| c.id).collect::<Vec<_>>();
    h.frame(vec![]);
    let o = order(&h);
    let top = h.rect(super::rack_grip_id(o[0]));
    let bottom = h.rect(super::rack_grip_id(o[1]));
    let depth = h.app.history.undo_len();
    h.drag(top.center(), egui::pos2(bottom.center().x, bottom.bottom() + 8.0), egui::PointerButton::Primary);
    assert_eq!(order(&h), vec![o[1], o[0]]);
    assert_eq!(h.app.history.undo_len(), depth + 1);
    // and back up with "Move up" from its menu
    rack_menu(&mut h, 1, "Move up");
    assert_eq!(order(&h), o);
    h.key(egui::Key::Z, egui::Modifiers::CTRL);
    assert_eq!(order(&h), vec![o[1], o[0]], "undo reverts the move");

    // a left click opens the same menu, and it stays open while typing a new name
    open_rack_menu(&mut h, 0, egui::PointerButton::Primary);
    let field = h.rect(super::rack_rename_id(h.app.project.channels[0].id));
    h.click_at(field.center(), egui::PointerButton::Primary);
    h.key(egui::Key::A, egui::Modifiers::COMMAND);
    h.frame(vec![egui::Event::Text("Bassline".into())]);
    h.key(egui::Key::Enter, egui::Modifiers::NONE);
    assert_eq!(h.app.project.channels[0].name, "Bassline", "renamed from the menu");
    assert!(h.app.plan_error.is_none(), "{:?}", h.app.plan_error);
}

#[test]
fn evolve_previews_are_level_matched_for_listening_only() {
    use rawdawg_audio_engine::offline::{OfflineOptions, render_project};
    use rawdawg_dsp::math::rms;
    let mut h = Harness::new(None);
    let ch = h.app.project.channels[0].clone();
    let instance = ch.instrument.id;
    // a note to listen to
    let pid = h.app.project.selected_pattern.unwrap();
    let note = rawdawg_project::Note {
        id: rawdawg_types::NoteId::new(),
        tick: rawdawg_types::units::Tick(0),
        duration_ticks: rawdawg_types::units::Tick(960),
        pitch: 60,
        velocity: 0.8,
        channel: 0,
    };
    h.app.apply(rawdawg_project::Command::AddNotes { pattern_id: pid, channel_id: ch.id, notes: vec![note] });
    let loudness = |p: &rawdawg_project::Project| {
        let opts = OfflineOptions { tail_seconds: 0.0, ..OfflineOptions::default() };
        let (l, _, _) = render_project(p, &opts).expect("render");
        20.0 * rms(&l).log10()
    };
    let original = loudness(&h.app.engine_project());
    // a variation 9 dB louder than the sound it came from
    let mut loud = h.app.synth_mirrors[&instance].patch().clone();
    loud.master_db += 9.0;
    super::evolve::preview(&mut h.app, instance, &loud);
    let trim = h.app.synth_panel.evolve.preview_trim_db.expect("level matched");
    assert!((trim + 9.0).abs() < 0.3, "the preview is turned down by about 9 dB: {trim}");
    // listening only: the document keeps the channel's own gain
    assert_eq!(h.app.project.channels[0].gain_db, ch.gain_db);
    let heard = loudness(&h.app.engine_project());
    assert!((heard - original).abs() < 0.5, "the preview plays as loud as the original: {heard:.2} vs {original:.2} dBFS");
    let unmatched = loudness(&h.app.project);
    assert!((unmatched - original - 9.0).abs() < 0.5, "without the match it is 9 dB louder: {unmatched:.2}");
    // bounded: a 40 dB drop is lifted by at most 12 dB
    let mut quiet = h.app.synth_mirrors[&instance].patch().clone();
    quiet.master_db -= 40.0;
    super::evolve::preview(&mut h.app, instance, &quiet);
    assert_eq!(h.app.synth_panel.evolve.preview_trim_db, Some(super::evolve::MAX_PREVIEW_TRIM_DB), "quiet variations are lifted by 12 dB at most");
    // switched off: the raw level
    h.app.synth_panel.evolve.match_levels = false;
    super::evolve::preview(&mut h.app, instance, &loud);
    assert_eq!(h.app.synth_panel.evolve.preview_trim_db, None);
    h.app.synth_panel.evolve.match_levels = true;
    super::evolve::preview(&mut h.app, instance, &loud);
    assert!(h.app.synth_panel.evolve.preview_trim_db.is_some());
    // Cancel ends it, and so does Use sound (the kept sound plays at its own level)
    super::evolve::cancel_preview(&mut h.app);
    assert_eq!(h.app.synth_panel.evolve.preview_trim_db, None);
    assert!((loudness(&h.app.engine_project()) - original).abs() < 0.01, "cancel restores the original");
    super::evolve::preview(&mut h.app, instance, &loud);
    super::evolve::use_sound(&mut h.app, instance, &loud);
    assert_eq!(h.app.synth_panel.evolve.preview_trim_db, None);
    assert!((loudness(&h.app.engine_project()) - original - 9.0).abs() < 0.5, "the kept sound is the real, louder one");
    assert_eq!(super::evolve::match_gain_db(0.0, 0.5), 0.0, "silence is never boosted");
    assert_eq!(super::evolve::match_gain_db(0.1, 0.1), 0.0);
}

#[test]
fn incompatible_corners_become_a_morph_rack_whose_pad_and_xy_automation_work() {
    use rawdawg_audio_engine::offline::{OfflineOptions, render_project};
    use rawdawg_synthesizer::morph_rack::{MorphRackState, PARAM_X};
    use rawdawg_synthesizer::patch::{Generator, Layer, Patch, PluckSettings};
    let mut h = Harness::new(None);
    let ch = h.app.project.channels[0].clone();
    let instance = ch.instrument.id;
    let pid = h.app.project.selected_pattern.unwrap();
    let note = rawdawg_project::Note {
        id: rawdawg_types::NoteId::new(),
        tick: rawdawg_types::units::Tick(0),
        duration_ticks: rawdawg_types::units::Tick(960),
        pitch: 57,
        velocity: 0.8,
        channel: 0,
    };
    h.app.apply(rawdawg_project::Command::AddNotes { pattern_id: pid, channel_id: ch.id, notes: vec![note] });
    let a = h.app.synth_mirrors[&instance].patch().clone();
    let b = Patch {
        layers: vec![
            Layer::basic(rawdawg_dsp::oscillator::BasicWave::Saw),
            Layer { generator: Generator::Pluck(PluckSettings::default()), ..Layer::basic(rawdawg_dsp::oscillator::BasicWave::Saw) },
        ],
        ..a.clone()
    };
    assert!(!rawdawg_synthesizer::morph::compatible(&[&a, &b, &a, &b]));
    // too heavy: refused with a message, nothing changes
    let mut heavy = b.clone();
    heavy.max_voices = 64;
    for l in &mut heavy.layers {
        l.unison = 8;
    }
    let mut heavier = heavy.clone();
    heavier.layers[0].coarse_semitones = 7;
    let depth = h.app.history.undo_len();
    super::evolve::make_morph_rack(&mut h.app, instance, [&heavy, &heavier, &a, &b]);
    assert!(h.app.status_line.contains("oscillator voices"), "{}", h.app.status_line);
    assert_eq!(h.app.history.undo_len(), depth);
    // the morph rack replaces the synth in one undo step
    h.app.synth_panel.evolve.morph_xy = (0.5, 0.5);
    super::evolve::make_morph_rack(&mut h.app, instance, [&a, &b, &a, &b]);
    let rack = h.app.project.channels[0].instrument.clone();
    assert_eq!(rack.type_id.as_str(), rawdawg_types::ids::native_types::MORPH);
    assert_eq!(h.app.history.undo_len(), depth + 1);
    // its panel: drag the pad to the top-right corner (B): one gesture, one undo step
    h.app.selected_channel = Some(ch.id);
    h.app.editor_view = EditorView::Synth;
    h.frame(vec![]);
    h.frame(vec![]);
    let pad = h.rect(super::instrument_panel::morph_rack_pad_id(rack.id));
    // past the edge: the position clamps to the corner itself
    h.drag(pad.center(), pad.right_top() + egui::vec2(30.0, -30.0), egui::PointerButton::Primary);
    let state: MorphRackState = serde_json::from_value(h.app.project.channels[0].instrument.state.clone()).unwrap();
    assert_eq!((state.x, state.y), (1.0, 0.0), "pad moved to B");
    assert_eq!(h.app.history.undo_len(), depth + 2, "the drag is one undo step");
    // what the engine plays at B is corner B itself
    let render = |p: &rawdawg_project::Project| render_project(p, &OfflineOptions { tail_seconds: 0.0, ..OfflineOptions::default() }).expect("render").0;
    let mut as_b = h.app.project.clone();
    as_b.channels[0].instrument = rawdawg_project::Instance::native(rawdawg_types::ids::native_types::SYNTH, serde_json::to_value(&b).unwrap());
    let (got, want) = (render(&h.app.project), render(&as_b));
    let diff = got.iter().zip(&want).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
    // the pad glides over 20 ms from the centre, so compare after the start
    let tail_diff = got[4800..].iter().zip(&want[4800..]).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
    assert!(rawdawg_dsp::math::rms(&want) > 0.01 && tail_diff < 1e-5, "rack at B = corner B ({tail_diff}, whole {diff})");
    // X takes automation like any parameter
    super::automation::create_clip_for(&mut h.app, rack.id, PARAM_X);
    assert!(h.app.project.automation.iter().any(|c| c.target.instance_id == rack.id && c.target.param_id == PARAM_X), "an X curve exists");
    // undo twice: pad back, then the synth again
    for _ in 0..3 {
        h.key(egui::Key::Z, egui::Modifiers::CTRL);
    }
    assert_eq!(h.app.project.channels[0].instrument.type_id.as_str(), rawdawg_types::ids::native_types::SYNTH);
    assert!(h.app.plan_error.is_none(), "{:?}", h.app.plan_error);
}

#[test]
fn pattern_automation_is_created_captured_in_both_modes_and_learns_from_midi_controllers() {
    use rawdawg_project::{AutomationMode, Clip, ClipContent, Command, MidiMapping, Track};
    use rawdawg_types::events::{EventKind, EventSource, TimedEvent};
    use rawdawg_types::{ClipId, MappingId, TrackId};
    let mut h = Harness::new(None);
    let instance = h.app.project.channels[0].instrument.id;
    let param = rawdawg_synthesizer::params::MASTER_GAIN;
    let pid = h.app.project.selected_pattern.unwrap();
    let plen = h.app.project.pattern(pid).unwrap().length_ticks.0;
    super::automation::create_pattern_curve_for(&mut h.app, instance, param);
    let curve = h.app.project.automation.last().unwrap().clone();
    assert_eq!(curve.pattern_id, Some(pid), "the curve belongs to the selected pattern");
    assert_eq!(curve.points.iter().map(|p| p.tick.0).collect::<Vec<_>>(), vec![0, plen], "it spans the pattern");
    assert!(h.app.project.clips.is_empty(), "no automation clip is needed");
    assert_eq!(h.app.editor_view, EditorView::Automation);
    let curve_now = |h: &Harness| h.app.project.automation.iter().find(|c| c.id == curve.id).unwrap().clone();
    h.app.apply(Command::UpdateAutomationCurve { curve: rawdawg_project::AutomationCurve { mode: AutomationMode::Touch, ..curve.clone() } });

    // pattern mode, playing: a touch gesture writes into the pattern's own timeline
    h.app.transport_mode = TransportMode::Pattern;
    h.app.automation.test_transport = Some((true, plen / 4));
    super::automation::capture_edit(&mut h.app, instance, param, 0.9, true, false);
    h.app.automation.test_transport = Some((true, plen / 2));
    super::automation::capture_edit(&mut h.app, instance, param, 0.4, false, true);
    let c = curve_now(&h);
    let written: Vec<(i64, f32)> = c.points.iter().map(|p| (p.tick.0, p.value)).filter(|(t, _)| *t > 0 && *t < plen).collect();
    assert_eq!(written, vec![(plen / 4, 0.9), (plen / 2, 0.4)], "written at pattern-relative ticks");

    // song mode: the pattern's clip at bar 3 decides where the pattern is
    h.app.transport_mode = TransportMode::Song;
    let tid = TrackId::new();
    h.app.apply(Command::AddTrack { track: Track { id: tid, name: "T".into(), color: None, mute: false, solo: false, height: None, mixer_id: None }, index: None });
    let clip = Clip {
        id: ClipId::new(),
        track_id: tid,
        start_tick: rawdawg_types::units::Tick(plen * 2),
        length_ticks: rawdawg_types::units::Tick(plen),
        muted: false,
        content: ClipContent::Pattern { pattern_id: pid, source_offset_ticks: rawdawg_types::units::Tick::ZERO, looped: false },
    };
    h.app.apply(Command::AddClip { clip: clip.clone() });
    h.app.automation.test_transport = Some((true, plen * 2 + plen * 3 / 4));
    super::automation::capture_edit(&mut h.app, instance, param, 0.1, true, true);
    assert!(curve_now(&h).points.iter().any(|p| p.tick.0 == plen * 3 / 4 && p.value == 0.1), "song tick → pattern tick inside the clip");
    // outside any clip of the pattern: nothing is written, and the reason is shown
    let before = curve_now(&h).points.len();
    h.app.automation.test_transport = Some((true, plen * 5));
    super::automation::capture_edit(&mut h.app, instance, param, 0.7, true, true);
    assert_eq!(curve_now(&h).points.len(), before);
    assert!(h.app.status_line.contains("not playing"), "{}", h.app.status_line);

    // a MIDI-learned controller writes into the armed curve, and resting ends the touch
    h.app.transport_mode = TransportMode::Pattern;
    let mapping = MidiMapping {
        id: MappingId::new(),
        device: None,
        channel: None,
        cc: 20,
        target: rawdawg_project::AutomationTarget { instance_id: instance, param_id: param },
        min: 0.0,
        max: 1.0,
        invert: false,
    };
    h.app.apply(Command::SetMidiMappings { mappings: vec![mapping] });
    let cc = |v: f32| TimedEvent::new(0, EventSource::MidiDevice(0), 0, EventKind::Control { cc: 20, value: v });
    h.app.automation.test_transport = Some((true, plen / 8));
    assert!(h.app.handle_midi_control(&cc(0.25)));
    h.app.automation.test_transport = Some((true, plen / 8 + rawdawg_types::units::PPQ));
    assert!(h.app.handle_midi_control(&cc(0.75)));
    // the controller rests (the quiet period has passed)
    h.app.midi_dirty.insert(instance, std::time::Instant::now() - std::time::Duration::from_secs(10));
    h.app.commit_quiet_midi_changes();
    let pts: Vec<(i64, f32)> = curve_now(&h).points.iter().map(|p| (p.tick.0, p.value)).collect();
    assert!(pts.contains(&(plen / 8, 0.25)) && pts.contains(&(plen / 8 + rawdawg_types::units::PPQ, 0.75)), "CC moves recorded: {pts:?}");

    // Make unique copies the pattern's automation with new ids
    h.app.playlist.selection.insert(clip.id);
    let mut cmds = Vec::new();
    super::playlist::make_unique(&mut h.app, &mut cmds);
    for c in cmds {
        h.app.apply(c);
    }
    let new_pid = match &h.app.project.clips.iter().find(|c| c.id == clip.id).unwrap().content {
        ClipContent::Pattern { pattern_id, .. } => *pattern_id,
        _ => unreachable!(),
    };
    assert_ne!(new_pid, pid);
    let copy = h.app.project.automation.iter().find(|c| c.pattern_id == Some(new_pid)).expect("the clone has the curve");
    assert_ne!(copy.id, curve.id);
    assert_eq!(copy.points.iter().map(|p| (p.tick, p.value)).collect::<Vec<_>>(), curve_now(&h).points.iter().map(|p| (p.tick, p.value)).collect::<Vec<_>>());
    assert!(h.app.plan_error.is_none(), "{:?}", h.app.plan_error);
}

#[test]
fn a_loopback_take_of_the_metronome_measures_the_recording_latency() {
    use rawdawg_audio_io::capture::{CaptureProducer, InputChannels};
    use rawdawg_audio_io::capture_writer::CaptureWriter;
    use rawdawg_project::{ClipContent, Track};
    let root = std::env::temp_dir().join(format!("rawdawg-ui-loopback-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("L.rawdawg");
    let mut h = Harness::new(None);
    rawdawg_project::save_project_dir(&dir, &h.app.project).unwrap();
    h.app.project_path = Some(dir.clone());
    let tid = rawdawg_types::TrackId::new();
    h.app.apply(rawdawg_project::Command::AddTrack {
        track: Track { id: tid, name: "Loopback".into(), color: None, mute: false, solo: false, height: None, mixer_id: None },
        index: None,
    });
    // a take placed at bar 2 whose metronome clicks arrive 12 ms after their beats
    let late = 576usize; // 12 ms at 48 kHz
    let frames = 8 * 24_000;
    let mut mono = vec![0.0f32; frames];
    for beat in 0..8 {
        for k in 0..1200 {
            let i = beat * 24_000 + late + k;
            if i < frames {
                mono[i] = 0.4 * (-(k as f32) / 240.0).exp() * (std::f32::consts::TAU * 1500.0 * k as f32 / 48_000.0 + 0.4).sin();
            }
        }
    }
    let (mut producer, queues) = CaptureProducer::new(2, InputChannels::Stereo(0, 1));
    let writer = CaptureWriter::start(&dir, queues.writer, 48_000, 96_000).unwrap();
    // fed in blocks, as an input stream would (the queue is bounded)
    for chunk in mono.chunks(4800) {
        let data: Vec<f32> = chunk.iter().flat_map(|v| [*v, *v]).collect();
        producer.push_interleaved(&data);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    producer.flush_partial();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let (wav, journal) = (writer.wav_path.clone(), writer.journal_path.clone());
    let (j, _queue) = writer.stop().unwrap();
    assert_eq!((j.frames_written, j.lost_frames), (frames as u64, 0), "the whole take was captured");
    h.app.place_take(&wav, &journal, tid, j.start_project_frame, j.frames_written, j.lost_frames, 48_000);
    let clip = h.app.project.clips.last().unwrap().clone();
    // measure: 12 ms late, eight clicks, tight
    h.app.prefs.record_offset_ms = 3.0;
    h.app.measure_take_latency(clip.id);
    let m = h.app.record.latency_check.as_ref().unwrap().1.clone().expect("measured");
    assert!((m.late_ms - 12.0).abs() < 0.1, "{m:?}");
    assert_eq!(m.clicks, 8);
    assert!(m.spread_ms < 0.1);
    // the window says so, and Apply adds it to the recording offset (a new window's first
    // frame is an invisible sizing pass)
    h.frame(vec![]);
    h.frame(vec![]);
    let texts = h.drawn_texts();
    assert!(texts.iter().any(|t| t.contains("12.0 ms late")), "{:?}", texts.iter().filter(|t| t.contains("ms")).collect::<Vec<_>>());
    let apply = h.drawn_nodes().into_iter().find(|(t, _)| t == "Apply to new recordings").map(|(_, r)| r).expect("Apply button");
    h.click_at(apply.center(), egui::PointerButton::Primary);
    assert!((h.app.prefs.record_offset_ms - 15.0).abs() < 0.1, "3 ms + 12 ms: {}", h.app.prefs.record_offset_ms);
    assert!(h.app.record.latency_check.is_none(), "the window closes");
    // a clip that is not a loopback of the metronome says why
    let other = h.app.project.clips.iter().find(|c| c.id != clip.id && matches!(c.content, ClipContent::Pattern { .. })).map(|c| c.id);
    if let Some(id) = other {
        h.app.measure_take_latency(id);
        assert!(h.app.record.latency_check.as_ref().unwrap().1.is_err());
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_input_meter_holds_peaks_falls_back_and_lights_on_full_scale() {
    use super::recording::update_input_meter;
    let (mut m, mut clipped) = ([0.0f32; 2], false);
    update_input_meter(&mut m, &mut clipped, Some([0.5, 0.1]));
    assert_eq!(m, [0.5, 0.1]);
    update_input_meter(&mut m, &mut clipped, Some([0.0, 0.2]));
    assert!((m[0] - 0.425).abs() < 1e-6 && m[1] == 0.2, "falls back, rises at once: {m:?}");
    assert!(!clipped);
    update_input_meter(&mut m, &mut clipped, Some([1.0, f32::NAN]));
    assert!(clipped, "full scale lights the clip light");
    assert!((m[1] - 0.17).abs() < 1e-6, "NaN reads as silence");
    update_input_meter(&mut m, &mut clipped, None);
    assert_eq!(m, [0.0; 2], "no input, no level");
}

#[test]
fn the_recovery_offer_compares_the_autosave_and_plays_either_version_before_choosing() {
    use rawdawg_audio_engine::offline::{OfflineOptions, render_project};
    use rawdawg_project::{Command, Note};
    use rawdawg_types::NoteId;
    let root = std::env::temp_dir().join(format!("rawdawg-ui-compare-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let dir = root.join("C.rawdawg");
    let mut h = Harness::new(None);
    h.app.new_project();
    h.app.save_project_as(dir.clone());
    // unsaved work: a note and a renamed channel, autosaved, then a "crash"
    let (pid, cid) = (h.app.project.patterns[0].id, h.app.project.channels[0].id);
    h.app.apply(Command::AddNotes {
        pattern_id: pid,
        channel_id: cid,
        notes: vec![Note { id: NoteId::new(), tick: Tick(0), duration_ticks: Tick(960), pitch: 60, velocity: 0.8, channel: 0 }],
    });
    h.app.apply(Command::RenameChannel { id: cid, name: "Lead".into() });
    // a sample imported after the save (only the autosave knows it)
    let wav = root.join("late.wav");
    {
        let spec = hound::WavSpec { channels: 1, sample_rate: 48_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(&wav, spec).unwrap();
        for i in 0..4800 {
            w.write_sample(((i as f32 * 0.05).sin() * 8000.0) as i16).unwrap();
        }
        w.finalize().unwrap();
    }
    let late_asset = rawdawg_types::AssetId::from_hex(&h.app.import_asset_only(&wav).unwrap()).unwrap();
    h.app.autosave_now(&dir);
    drop(h);
    let mut h = Harness::new(None);
    h.app.open_project_checked(dir.clone());
    assert_eq!(h.app.files.recovery.len(), 1);
    let click = |h: &mut Harness, label: &str| {
        h.frame(vec![]);
        h.frame(vec![]);
        let r = h.drawn_nodes().into_iter().find(|(t, _)| t == label).map(|(_, r)| r).unwrap_or_else(|| panic!("{label:?} not drawn"));
        h.click_at(r.center(), egui::PointerButton::Primary);
    };
    click(&mut h, "Compare");
    let changes = h.app.files.compare.as_ref().expect("comparing").changes.clone();
    assert!(changes.iter().any(|l| l.contains("0 → 1 notes")), "{changes:?}");
    assert!(changes.iter().any(|l| l.contains("Lead")), "{changes:?}");
    h.frame(vec![]);
    assert!(h.drawn_texts().iter().any(|t| t.contains("0 → 1 notes")), "the summary is shown");
    // listen to the autosave: the engine plays it (with the sample imported after the save),
    // the document stays the saved version
    click(&mut h, "Autosave");
    {
        use rawdawg_audio_engine::AssetResolver;
        assert!(h.app.assets.sample(&late_asset).is_some(), "the autosave's own sample resolves while listening to it");
    }
    let notes = |p: &rawdawg_project::Project| p.patterns[0].lanes.iter().map(|l| l.notes.len()).sum::<usize>();
    assert_eq!(notes(&h.app.engine_project()), 1, "the engine plays the autosave");
    assert_eq!(notes(&h.app.project), 0, "nothing is changed until you choose");
    let loud = |p: &rawdawg_project::Project| {
        rawdawg_dsp::math::rms(&render_project(p, &OfflineOptions { tail_seconds: 0.0, end_tick: Some(Tick(1920)), ..OfflineOptions::default() }).unwrap().0)
    };
    assert!(loud(&h.app.engine_project()) > 0.01, "the autosave's note sounds");
    click(&mut h, "Saved");
    assert_eq!(notes(&h.app.engine_project()), 0, "back to the saved version");
    // recover: the autosave becomes the (unsaved) document and the compare view ends
    click(&mut h, "Autosave");
    click(&mut h, "Recover");
    assert!(h.app.files.compare.is_none() && h.app.files.recovery.is_empty());
    assert_eq!(notes(&h.app.project), 1);
    assert_eq!(h.app.project.channels[0].name, "Lead");
    assert!(h.app.history.is_dirty());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_typing_keyboard_ignores_key_repeat_releases_on_focus_loss_and_keeps_note_identity() {
    use rawdawg_types::events::EventKind;
    let mut h = Harness::new(None);
    h.app.typing_enabled = true;
    h.app.prefs.typing_octave = 0;
    let none = egui::Modifiers::NONE;
    let log = |h: &mut Harness| std::mem::take(&mut h.app.live_log).into_iter().map(|e| e.kind).collect::<Vec<_>>();
    // press Z: one note-on at C4; the OS repeats the key while held: no more notes
    h.key_event(egui::Key::Z, true, false, none);
    for _ in 0..5 {
        h.key_event(egui::Key::Z, true, true, none);
    }
    let on = log(&mut h);
    assert_eq!(on.len(), 1, "{on:?}");
    let EventKind::NoteOn { note_id, pitch: 60, .. } = on[0] else { panic!("{on:?}") };
    // change the octave while holding, then release: the note-off has the original id and pitch
    h.app.prefs.typing_octave = 1;
    h.key_event(egui::Key::Z, false, false, none);
    assert_eq!(log(&mut h), vec![EventKind::NoteOff { note_id, pitch: 60 }], "released with the pitch it started with");
    // the next press plays in the new octave, under a new id
    h.key_event(egui::Key::Z, true, false, none);
    let on2 = log(&mut h);
    let EventKind::NoteOn { note_id: id2, pitch: 72, .. } = on2[0] else { panic!("{on2:?}") };
    assert_ne!(id2, note_id);
    // focus goes to another window while two keys are held: both are released at once
    h.key_event(egui::Key::C, true, false, none);
    log(&mut h);
    h.frame_unfocused();
    let mut released: Vec<(u32, u8)> = log(&mut h)
        .into_iter()
        .map(|k| match k {
            EventKind::NoteOff { note_id, pitch } => (note_id, pitch),
            other => panic!("only releases on focus loss: {other:?}"),
        })
        .collect();
    released.sort_unstable();
    assert_eq!(released.len(), 2, "{released:?}");
    assert!(released.iter().any(|(id, p)| *id == id2 && *p == 72));
    // a key-up arriving after the focus came back releases nothing twice
    h.frame(vec![egui::Event::WindowFocused(true)]);
    h.key_event(egui::Key::Z, false, false, none);
    assert!(log(&mut h).is_empty(), "no stuck or doubled notes");
    // with Ctrl held, keys are shortcuts, not notes
    h.key_event(egui::Key::X, true, false, egui::Modifiers::CTRL);
    h.key_event(egui::Key::X, false, false, egui::Modifiers::CTRL);
    assert!(log(&mut h).is_empty(), "Ctrl+key plays nothing");
}

#[test]
fn timestamped_midi_lands_at_a_constant_latency_whatever_the_delivery_jitter() {
    use rawdawg_audio_engine::timing::{JitterBuffer, place_event};
    use rawdawg_midi::{MidiClock, MidiTime};
    // a device with a far-off clock sends every 2.5 ms; messages arrive 0.3–9 ms late and the
    // UI polls every 16.7 ms; the audio stream runs 1024-frame blocks (an ALSA/Pulse default)
    let (sr, block) = (48_000u32, 1024u32);
    let block_us = f64::from(block) / f64::from(sr) * 1e6;
    let device_ahead = 7_000_000_000u64;
    let (mut clock, mut jb) = (MidiClock::default(), JitterBuffer::default());
    let mut rng = 99u64;
    let mut msgs: Vec<(u64, u64)> = (0..2_000u64)
        .map(|k| {
            let sent = 1_000_000 + k * 2_500;
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (sent, sent + 300 + (rng >> 33) % 8_700)
        })
        .collect();
    msgs.sort_by_key(|m| m.1);
    let (mut out, mut i, mut poll) = (Vec::new(), 0usize, 1_000_000u64);
    while i < msgs.len() {
        poll += 16_667;
        let b = ((poll - 1_000_000) as f64 / block_us).floor() as u64;
        let (bstart, bwall) = (b * u64::from(block), 1_000_000 + (b as f64 * block_us) as u64);
        let next_block = ((poll - 1_000_000) as f64 / block_us).ceil() as u64 * u64::from(block);
        while i < msgs.len() && msgs[i].1 <= poll {
            let (sent, arrival) = msgs[i];
            let wall = clock.wall_time(MidiTime { device_us: sent + device_ahead, arrival_us: arrival });
            let target = place_event(&mut jb, bstart, bwall, block, sr, wall, poll);
            let true_frame = ((sent - 1_000_000) as f64 * f64::from(sr) / 1e6).round() as i64;
            out.push((target as i64 - true_frame, target >= next_block));
            i += 1;
        }
    }
    // after a second every note is on time (none late); against the true send times the latency
    // holds within 3.5 ms (the buffer's slow fall-back plus the device clock estimate), where
    // polling alone would scatter notes over 16 ms and more
    let steady = &out[400..];
    assert!(steady.iter().all(|(_, on_time)| *on_time), "{} late", steady.iter().filter(|o| !o.1).count());
    let (lo, hi) = steady.iter().fold((i64::MAX, i64::MIN), |(lo, hi), (l, _)| (lo.min(*l), hi.max(*l)));
    assert!(hi - lo <= 168, "latency varies by {} frames", hi - lo);
    assert!(hi < 2 * 1024 + 48 * 40, "latency {hi} frames stays below two blocks + 40 ms");
}

#[test]
fn an_evolve_level_match_never_applies_to_another_sound() {
    let mut h = Harness::new(None);
    let ch = h.app.project.channels[0].clone();
    let instance = ch.instrument.id;
    let base = h.app.synth_mirrors[&instance].patch().clone();
    let mut loud = base.clone();
    loud.master_db += 9.0;
    let gain = |h: &Harness| h.app.engine_project().channels[0].gain_db.0;
    // previewing the card: matched
    super::evolve::preview(&mut h.app, instance, &loud);
    assert!(gain(&h) < ch.gain_db.0 - 8.0);
    // a knob tweak on the card keeps the match (no jump while auditioning)
    h.app.synth_mirrors.get_mut(&instance).unwrap().set_param(rawdawg_types::ParamId(rawdawg_synthesizer::params::AMP_ATTACK), 0.3);
    assert!(gain(&h) < ch.gain_db.0 - 8.0);
    // a preset from the preset browser is a different sound: its own level
    let mut preset = base.clone();
    preset.master_db -= 3.0;
    super::presets::preview(&mut h.app, instance, &preset);
    assert_eq!(gain(&h), ch.gain_db.0, "the preset plays at its own level");
    // committing another sound or undoing ends it too
    super::evolve::cancel_preview(&mut h.app);
    super::evolve::preview(&mut h.app, instance, &loud);
    assert!(h.app.synth_panel.evolve.preview_trim_db.is_some());
    super::synth_panel::commit_patch(&mut h.app, instance, preset.clone());
    assert_eq!(gain(&h), ch.gain_db.0, "a committed sound plays at its own level");
    super::evolve::preview(&mut h.app, instance, &loud);
    h.key(egui::Key::Z, egui::Modifiers::CTRL);
    assert_eq!(gain(&h), h.app.project.channels[0].gain_db.0, "undo ends the match");
}

#[test]
fn tempo_is_edited_in_the_toolbar_and_through_its_markers() {
    let mut h = Harness::new(None);
    let bpm = |h: &Harness| h.app.project.tempo[0].us_per_quarter.bpm().0;
    let node = |h: &mut Harness, text: &str| h.drawn_nodes().into_iter().find(|(t, _)| t == text).map(|(_, r)| r).unwrap_or_else(|| panic!("{text:?} not drawn"));
    // the toolbar tempo: click, type 140, Enter (stored as whole µs per quarter: ±0.001 BPM)
    let field = node(&mut h, "120.00 BPM");
    let depth = h.app.history.undo_len();
    h.click_at(field.center(), egui::PointerButton::Primary);
    h.key(egui::Key::A, egui::Modifiers::COMMAND);
    h.frame(vec![egui::Event::Text("140".into())]);
    h.key(egui::Key::Enter, egui::Modifiers::NONE);
    assert!((bpm(&h) - 140.0).abs() < 1e-3, "typed tempo: {}", bpm(&h));
    assert_eq!(h.app.history.undo_len(), depth + 1);
    // dragging it is one undo step too
    let field = node(&mut h, "140.00 BPM");
    h.drag(field.center(), field.center() + egui::vec2(60.0, 0.0), egui::PointerButton::Primary);
    assert!(bpm(&h) > 140.5, "dragged faster: {}", bpm(&h));
    assert_eq!(h.app.history.undo_len(), depth + 2, "one undo step per drag");
    h.key(egui::Key::Z, egui::Modifiers::CTRL);
    assert!((bpm(&h) - 140.0).abs() < 1e-3);
    // the tempo marker above the playlist opens its editor (the marker row no longer takes the click)
    h.frame(vec![]);
    let marker = h.rect(super::playlist::marker_id(true, 0));
    h.click_at(marker.center(), egui::PointerButton::Primary);
    h.frame(vec![]);
    h.frame(vec![]);
    assert!(h.drawn_texts().iter().any(|t| t == "Tempo change"), "the Tempo change window opens");
    // the window's field (one decimal; the toolbar's has two)
    let field = node(&mut h, "140.0 BPM");
    h.click_at(field.center(), egui::PointerButton::Primary);
    h.key(egui::Key::A, egui::Modifiers::COMMAND);
    h.frame(vec![egui::Event::Text("96".into())]);
    h.key(egui::Key::Enter, egui::Modifiers::NONE);
    let apply = node(&mut h, "Apply");
    h.click_at(apply.center(), egui::PointerButton::Primary);
    assert!((bpm(&h) - 96.0).abs() < 1e-3, "set through the marker: {}", bpm(&h));
    // the time-signature marker opens its editor as well
    h.frame(vec![]);
    let meter = h.rect(super::playlist::marker_id(false, 0));
    h.click_at(meter.center(), egui::PointerButton::Primary);
    h.frame(vec![]);
    h.frame(vec![]);
    assert!(h.drawn_texts().iter().any(|t| t == "Time signature"), "the Time signature window opens");
}

#[test]
fn right_clicking_a_slider_opens_its_menu_and_keeps_the_value() {
    let mut h = Harness::new(None);
    h.app.editor_view = EditorView::Synth;
    h.frame(vec![]);
    let instance = h.app.project.channels[0].instrument.id;
    let cutoff = rawdawg_types::ParamId(rawdawg_synthesizer::params::FILTER_CUTOFF);
    let value = |h: &Harness| h.app.synth_mirrors[&instance].param(cutoff).unwrap();
    // bring the basic controls into view
    let label = |h: &mut Harness| h.drawn_nodes().into_iter().filter(|(t, _)| t == "Cutoff").map(|(_, r)| r).min_by(|a, b| a.top().total_cmp(&b.top()));
    let into_view = |h: &mut Harness| {
        for _ in 0..40 {
            match label(h) {
                Some(r) if r.bottom() < H - 80.0 && r.top() > 120.0 => return r,
                _ => h.scroll(egui::pos2(W * 0.5, H * 0.6), -120.0),
            }
        }
        panic!("Cutoff never came into view")
    };
    let l = into_view(&mut h);
    // the slider sits between its label and its readout in the grid row
    let readout = |h: &mut Harness, l: egui::Rect| {
        h.drawn_nodes()
            .into_iter()
            .filter(|(t, r)| t != "Cutoff" && (r.center().y - l.center().y).abs() < 4.0 && r.left() > l.right())
            .map(|(_, r)| r)
            .min_by(|a, b| a.left().total_cmp(&b.left()))
            .expect("the readout")
    };
    let rd = readout(&mut h, l);
    let on_slider = egui::pos2((l.right() + rd.left()) * 0.5, l.center().y);
    // (sanity: a left click there does move it, so the right-click below hits the slider)
    let original = value(&h);
    h.click_at(on_slider, egui::PointerButton::Primary);
    assert_ne!(value(&h), original, "the point is on the Cutoff slider");
    h.key(egui::Key::Z, egui::Modifiers::CTRL);
    assert_eq!(value(&h), original);
    let before = value(&h);
    let depth = h.app.history.undo_len();
    h.click_at(on_slider, egui::PointerButton::Secondary);
    h.frame(vec![]);
    assert_eq!(value(&h), before, "a right-click does not move the slider");
    assert_eq!(h.app.history.undo_len(), depth, "and adds no undo step");
    let texts = h.drawn_texts();
    for entry in ["Create automation clip", "MIDI learn (then move a controller)", "Reset to default"] {
        assert!(texts.iter().any(|t| t == entry), "the menu offers {entry:?}");
    }
    // the menu works: an automation clip for Cutoff
    let create = h.drawn_nodes().into_iter().find(|(t, _)| t == "Create automation clip").map(|(_, r)| r).unwrap();
    h.click_at(create.center(), egui::PointerButton::Primary);
    assert!(h.app.project.automation.iter().any(|c| c.target.instance_id == instance && c.target.param_id == cutoff.0), "Cutoff curve created");
    // a left drag still moves it
    h.app.editor_view = EditorView::Synth;
    h.frame(vec![]);
    let l = into_view(&mut h);
    let rd = readout(&mut h, l);
    let start = egui::pos2((l.right() + rd.left()) * 0.5, l.center().y);
    let v0 = value(&h);
    h.drag(start, start + egui::vec2(-30.0, 0.0), egui::PointerButton::Primary);
    assert!(value(&h) < v0, "the primary button still drags it");
}

#[test]
fn effect_sliders_and_dropdowns_open_their_menu_on_right_click_without_changing() {
    let mut h = Harness::new(None);
    let master = h.app.project.mixer.master_id;
    let mut inst = rawdawg_project::Instance::native("native.fx.delay", serde_json::Value::Null);
    inst.display_name = "Delay".into();
    let id = inst.id;
    h.app.apply(rawdawg_project::Command::AddInsert { mixer_id: master, instance: inst, index: None });
    h.app.editor_view = EditorView::Effect;
    h.app.effect_editor = Some(id);
    h.frame(vec![]);
    h.frame(vec![]);
    let state = |h: &Harness| h.app.project.mixer_channel(master).unwrap().inserts[0].state.clone();
    let node = |h: &mut Harness, text: &str| h.drawn_nodes().into_iter().filter(|(t, _)| t == text).map(|(_, r)| r).min_by(|a, b| a.top().total_cmp(&b.top()));
    let into_view = |h: &mut Harness, text: &str| {
        for _ in 0..40 {
            match node(h, text) {
                Some(r) if r.bottom() < H - 80.0 && r.top() > 120.0 => return r,
                _ => h.scroll(egui::pos2(W * 0.5, H * 0.6), -120.0),
            }
        }
        panic!("{text} never came into view")
    };
    // Feedback: the slider between its label and its readout
    let l = into_view(&mut h, "Feedback");
    let rd = h
        .drawn_nodes()
        .into_iter()
        .filter(|(t, r)| t != "Feedback" && (r.center().y - l.center().y).abs() < 4.0 && r.left() > l.right() + 30.0)
        .map(|(_, r)| r)
        .min_by(|a, b| a.left().total_cmp(&b.left()))
        .expect("readout");
    let on_slider = egui::pos2((l.right() + rd.left()) * 0.5, l.center().y);
    let before = state(&h);
    let depth = h.app.history.undo_len();
    h.click_at(on_slider, egui::PointerButton::Secondary);
    h.frame(vec![]);
    assert_eq!(state(&h), before, "Feedback keeps its value");
    assert_eq!(h.app.history.undo_len(), depth);
    let texts = h.drawn_texts();
    assert!(texts.iter().any(|t| t == "Create automation clip") && texts.iter().any(|t| t == "Modulate with an LFO clip"), "the menu is open");
    // close it (Escape), then the Mode dropdown: its menu, not its list
    h.key(egui::Key::Escape, egui::Modifiers::NONE);
    let mode = into_view(&mut h, "Mode");
    let combo = h
        .drawn_nodes()
        .into_iter()
        .filter(|(t, r)| t != "Mode" && (r.center().y - mode.center().y).abs() < 6.0 && r.left() > mode.right())
        .map(|(_, r)| r)
        .min_by(|a, b| a.left().total_cmp(&b.left()))
        .expect("the Mode dropdown");
    h.click_at(combo.center(), egui::PointerButton::Secondary);
    h.frame(vec![]);
    let texts = h.drawn_texts();
    assert!(texts.iter().any(|t| t == "Create automation clip"), "the dropdown's menu opens");
    let choices = ["Stereo", "Ping-pong", "Tape", "Multitap", "Reverse"];
    let listed = choices.iter().filter(|c| texts.iter().any(|t| t == **c)).count();
    assert!(listed <= 1, "its list of choices does not open as well ({listed} choices drawn)");
    assert_eq!(state(&h), before);
}

#[test]
fn a_synth_channel_can_be_added_from_the_rack_and_the_browser_even_after_deleting_the_last() {
    let mut h = Harness::new(None);
    h.app.editor_view = EditorView::Rack;
    h.frame(vec![]);
    let node = |h: &mut Harness, text: &str| h.drawn_nodes().into_iter().find(|(t, _)| t == text).map(|(_, r)| r).unwrap_or_else(|| panic!("{text:?} not drawn"));
    // delete the only synth: the rack is empty and says how to add one
    rack_menu(&mut h, 0, "Delete channel (undoable)");
    assert!(h.app.project.channels.is_empty());
    h.frame(vec![]);
    assert!(h.drawn_texts().iter().any(|t| t.starts_with("No channels yet")), "the empty rack says what to do");
    // + Synth brings one back, selected and playable
    let plus = node(&mut h, "+ Synth");
    h.click_at(plus.center(), egui::PointerButton::Primary);
    assert_eq!(h.app.project.channels.len(), 1);
    let ch = h.app.project.channels[0].clone();
    assert_eq!(ch.instrument.type_id.as_str(), rawdawg_types::ids::native_types::SYNTH);
    assert_eq!(h.app.selected_channel, Some(ch.id), "the new channel is selected");
    h.app.typing_enabled = true;
    h.key_event(egui::Key::Z, true, false, egui::Modifiers::NONE);
    h.key_event(egui::Key::Z, false, false, egui::Modifiers::NONE);
    assert!(!h.app.live_log.is_empty(), "the keys play it");
    // its Synth view opens (a mirror exists for the new instrument)
    h.app.editor_view = EditorView::Synth;
    h.frame(vec![]);
    h.frame(vec![]);
    assert!(h.drawn_texts().iter().any(|t| t == &format!("rawDAWg Synth — {}", ch.name)), "the synth panel shows the new channel");
    // the browser entry adds another one, below the selected channel
    let entry = node(&mut h, "rawDAWg Synth");
    h.click_at(entry.center(), egui::PointerButton::Primary);
    assert_eq!(h.app.project.channels.len(), 2);
    assert_eq!(h.app.project.channels[1].name, "Synth 2");
    assert_eq!(h.app.selected_channel, Some(h.app.project.channels[1].id));
    // each add is one undo step
    h.key(egui::Key::Z, egui::Modifiers::CTRL);
    assert_eq!(h.app.project.channels.len(), 1);
    assert!(h.app.plan_error.is_none(), "{:?}", h.app.plan_error);
}

#[test]
fn after_randomize_the_controls_show_the_new_sound_and_a_tweak_keeps_the_rest() {
    let mut h = Harness::new(None);
    h.app.editor_view = EditorView::Synth;
    h.frame(vec![]);
    let instance = h.app.project.channels[0].instrument.id;
    let saved = |h: &Harness| -> rawdawg_synthesizer::Patch { serde_json::from_value(h.app.project.channels[0].instrument.state.clone()).unwrap() };
    let before = saved(&h);
    // Randomize sound, as a user clicks it
    let randomize = h.scroll_into_view("Randomize sound", |h| h.node("Randomize sound"));
    h.click_at(randomize.center(), egui::PointerButton::Primary);
    let randomized = saved(&h);
    assert_ne!(randomized, before, "Randomize changed the sound");
    // the panel shows the new sound: its mirror is the saved patch
    assert_eq!(h.app.synth_mirrors[&instance].patch(), &randomized, "the controls show the randomized values");
    // touch only Attack (its slider under its name in the envelope section)
    let attack_slider = |h: &mut Harness| {
        let l = h.node("Attack")?;
        h.drawn_nodes().into_iter().filter(|(t, r)| t == "Attack" && r.top() > l.bottom() - 1.0).map(|(_, r)| r).next()
    };
    h.scroll_into_view("the Attack slider", attack_slider);
    let l = h.node("Attack").unwrap();
    let slider = h
        .drawn_nodes()
        .into_iter()
        .filter(|(t, r)| t == "Attack" && r.top() > l.bottom() - 1.0 && r.top() < l.bottom() + 30.0)
        .map(|(_, r)| r)
        .min_by(|a, b| a.top().total_cmp(&b.top()))
        .expect("the Attack slider under its name");
    let p = slider.center();
    h.drag(p, p + egui::vec2(15.0, 0.0), egui::PointerButton::Primary);
    let after = saved(&h);
    assert_ne!(after.amp_env.attack_s, randomized.amp_env.attack_s, "Attack changed");
    let mut expected = randomized.clone();
    expected.amp_env.attack_s = after.amp_env.attack_s;
    assert_eq!(after, expected, "nothing but Attack changed; resonance stays {}", randomized.filter.resonance);
}

#[test]
fn drawing_past_the_end_grows_the_pattern_and_its_menu_opens_on_a_left_click() {
    let mut h = Harness::new(None);
    h.app.editor_view = EditorView::PianoRoll;
    h.frame(vec![]);
    let pid = h.app.project.selected_pattern.unwrap();
    let len = |h: &Harness| h.app.project.pattern(pid).unwrap().length_ticks.0;
    assert_eq!(len(&h), 3840, "a new pattern is one bar");
    let surface = h.rect(egui::Id::unique("piano-roll-surface"));
    // a click in bar 3: a note there, and the pattern grows to three bars (one undo step)
    let depth = h.app.history.undo_len();
    let p = super::piano_roll::grid_point(&h.app.piano_roll, surface, 2 * 3840 + 60, 60.5);
    h.click_at(p, egui::PointerButton::Primary);
    assert_eq!(len(&h), 3 * 3840, "grew to 3 bars");
    assert!(h.lane_notes().iter().any(|n| n.tick.0 >= 2 * 3840 && n.pitch == 60), "the note is in bar 3");
    assert!(h.app.status_line.contains("3 bars"), "{}", h.app.status_line);
    assert_eq!(h.app.history.undo_len(), depth + 1);
    h.key(egui::Key::Z, egui::Modifiers::CTRL);
    assert_eq!(len(&h), 3840, "undo takes note and length back together");
    assert!(h.lane_notes().is_empty());
    // stretching a note across the end grows it too, instead of clipping the note
    let surface = h.rect(egui::Id::unique("piano-roll-surface"));
    let start = super::piano_roll::grid_point(&h.app.piano_roll, surface, 3840 - 480 + 30, 62.5);
    let end = super::piano_roll::grid_point(&h.app.piano_roll, surface, 3840 + 960, 62.5);
    h.drag(start, end, egui::PointerButton::Primary);
    let n = h.lane_notes();
    assert_eq!(n.len(), 1);
    assert!(n[0].end_tick().0 > 3840, "the note crosses the old end: {:?}", n[0]);
    assert_eq!(len(&h), 2 * 3840, "and the pattern grew to hold it");
    // the pattern's … menu opens on a left click and sets the length
    let dots = h.drawn_nodes().into_iter().filter(|(t, _)| t == "…").map(|(_, r)| r).min_by(|a, b| a.top().total_cmp(&b.top())).expect("the pattern … button");
    h.click_at(dots.center(), egui::PointerButton::Primary);
    h.frame(vec![]);
    assert!(h.drawn_texts().iter().any(|t| t == "Length: 2.00 bars"), "the menu opens on a left click");
    let four = h.drawn_nodes().into_iter().find(|(t, _)| t == "4").map(|(_, r)| r).expect("the 4 bars choice");
    h.click_at(four.center(), egui::PointerButton::Primary);
    assert_eq!(len(&h), 4 * 3840, "set to 4 bars");
}

#[test]
fn midi_export_follows_the_scope_and_refuses_an_empty_file() {
    let root = std::env::temp_dir().join(format!("rawdawg-ui-midi-scope-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let mut h = Harness::new(None);
    // three notes in the pattern, nothing on the playlist
    let (pid, cid) = (h.app.project.patterns[0].id, h.app.project.channels[0].id);
    let notes = [60u8, 64, 67]
        .iter()
        .enumerate()
        .map(|(i, p)| rawdawg_project::Note { id: rawdawg_types::NoteId::new(), tick: Tick(i as i64 * 480), duration_ticks: Tick(240), pitch: *p, velocity: 0.8, channel: 0 })
        .collect();
    h.app.apply(rawdawg_project::Command::AddNotes { pattern_id: pid, channel_id: cid, notes });
    assert!(h.app.project.clips.is_empty());
    h.app.export.open = true;
    // a typed .mid name is kept as typed; the audio and MIDI files are derived from it
    h.app.export.path = root.join("rec.mid").display().to_string();
    h.frame(vec![]);
    h.frame(vec![]);
    assert!(h.app.export.path.ends_with("rec.mid"), "the field is not rewritten while typing: {}", h.app.export.path);
    assert_eq!(super::export::midi_path(&h.app).unwrap(), root.join("rec.mid"));
    assert_eq!(super::export::audio_path(&h.app).unwrap(), root.join("rec.wav"));
    let click = |h: &mut Harness, text: &str| {
        let r = h.drawn_nodes().into_iter().find(|(t, _)| t == text).map(|(_, r)| r).unwrap_or_else(|| panic!("{text:?} not drawn"));
        h.click_at(r.center(), egui::PointerButton::Primary);
    };
    // Song scope: the playlist is empty, so nothing is written and the dialog says why
    click(&mut h, "Song");
    click(&mut h, "Export MIDI (type 1)");
    assert!(h.app.export.last_result.as_deref().is_some_and(|r| r.starts_with("Nothing to export")), "{:?}", h.app.export.last_result);
    assert!(!root.join("rec.mid").exists(), "no empty file");
    // Pattern scope: the selected pattern's notes
    click(&mut h, "Pattern");
    click(&mut h, "Export MIDI (type 1)");
    let bytes = std::fs::read(root.join("rec.mid")).expect("written");
    let back = rawdawg_project::smf::import(&bytes, "rec").unwrap();
    let pitches: Vec<u8> = back.tracks.iter().flat_map(|t| t.notes.iter().map(|n| n.pitch)).collect();
    assert_eq!(pitches.len(), 3, "the three notes: {pitches:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn export_asks_before_replacing_and_its_path_belongs_to_the_project() {
    let root = std::env::temp_dir().join(format!("rawdawg-ui-export-overwrite-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let mut h = Harness::new(None);
    h.app.new_project();
    h.app.save_project_as(root.join("A.rawdawg"));
    let (pid, cid) = (h.app.project.patterns[0].id, h.app.project.channels[0].id);
    h.app.apply(rawdawg_project::Command::AddNotes {
        pattern_id: pid,
        channel_id: cid,
        notes: vec![rawdawg_project::Note { id: rawdawg_types::NoteId::new(), tick: Tick(0), duration_ticks: Tick(240), pitch: 60, velocity: 0.8, channel: 0 }],
    });
    h.app.export.open = true;
    h.app.export.song = false;
    h.frame(vec![]);
    let shared = root.join("mix");
    h.app.export.path = shared.display().to_string();
    h.frame(vec![]);
    let click = |h: &mut Harness, text: &str| {
        let r = h.drawn_nodes().into_iter().find(|(t, _)| t == text).map(|(_, r)| r).unwrap_or_else(|| panic!("{text:?} not drawn"));
        h.click_at(r.center(), egui::PointerButton::Primary);
    };
    click(&mut h, "Export MIDI (type 1)");
    let first = std::fs::read(root.join("mix.mid")).expect("written");
    // another project: the dialog proposes that project's own file, not project A's
    h.app.save_project_checked();
    h.app.new_project();
    h.app.save_project_as(root.join("B.rawdawg"));
    h.frame(vec![]);
    assert!(h.app.export.path.contains("B.rawdawg"), "the path follows the open project: {}", h.app.export.path);
    // point it at A's file on purpose: replacing it needs a confirmation
    let (pid, cid) = (h.app.project.patterns[0].id, h.app.project.channels[0].id);
    h.app.apply(rawdawg_project::Command::AddNotes {
        pattern_id: pid,
        channel_id: cid,
        notes: vec![rawdawg_project::Note { id: rawdawg_types::NoteId::new(), tick: Tick(480), duration_ticks: Tick(960), pitch: 72, velocity: 0.5, channel: 0 }],
    });
    h.app.export.path = shared.display().to_string();
    h.frame(vec![]);
    click(&mut h, "Export MIDI (type 1)");
    h.frame(vec![]);
    assert!(h.drawn_texts().iter().any(|t| t.contains("already exists. Replace?")), "it asks");
    assert_eq!(std::fs::read(root.join("mix.mid")).unwrap(), first, "nothing replaced yet");
    click(&mut h, "Keep the existing file");
    assert_eq!(std::fs::read(root.join("mix.mid")).unwrap(), first, "kept");
    click(&mut h, "Export MIDI (type 1)");
    click(&mut h, "Replace");
    assert_ne!(std::fs::read(root.join("mix.mid")).unwrap(), first, "replaced after confirming");
    // audio export asks the same way
    std::fs::write(root.join("mix.wav"), b"old").unwrap();
    // the button (the window title says "Export" too)
    let button = h.drawn_nodes().into_iter().filter(|(t, _)| t == "Export").map(|(_, r)| r).max_by(|a, b| a.top().total_cmp(&b.top())).unwrap();
    h.click_at(button.center(), egui::PointerButton::Primary);
    assert!(!h.app.export.running(), "waits for the answer");
    h.frame(vec![]);
    assert!(h.drawn_texts().iter().any(|t| t.contains("mix.wav already exists")));
    click(&mut h, "Keep the existing file");
    assert_eq!(std::fs::read(root.join("mix.wav")).unwrap(), b"old");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn ctrl_shift_z_redoes_and_ctrl_z_undoes() {
    let mut h = Harness::new(None);
    h.app.editor_view = EditorView::Rack;
    h.frame(vec![]);
    let cid = h.app.selected_channel.unwrap();
    // two separate edits: a sequence where undo and redo give different results
    for step in [0usize, 4] {
        let r = h.rect(egui::Id::unique(("step", cid, step)));
        h.click_at(r.center(), egui::PointerButton::Primary);
    }
    assert_eq!(h.lane_notes().len(), 2);
    h.key(egui::Key::Z, egui::Modifiers::CTRL);
    assert_eq!(h.lane_notes().len(), 1, "Ctrl+Z undoes the second note");
    h.key(egui::Key::Z, egui::Modifiers::CTRL | egui::Modifiers::SHIFT);
    assert_eq!(h.lane_notes().len(), 2, "Ctrl+Shift+Z redoes it (undoing again would leave none)");
    assert!(h.app.status_line.starts_with("Redid"), "{}", h.app.status_line);
    h.key(egui::Key::Z, egui::Modifiers::CTRL);
    h.key(egui::Key::Y, egui::Modifiers::CTRL);
    assert_eq!(h.lane_notes().len(), 2, "Ctrl+Y redoes too");
}

#[test]
fn projects_saved_under_a_relative_name_are_remembered_by_their_absolute_path() {
    // the tests run in crates/app: a relative name lands in the workspace's target folder
    let rel_dir = PathBuf::from(format!("../../target/rawdawg-relative-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&rel_dir);
    std::fs::create_dir_all(&rel_dir).unwrap();
    let mut h = Harness::new(None);
    h.app.new_project();
    h.app.save_project_as(rel_dir.join("MySong"));
    let remembered = h.app.prefs.recent_projects[0].clone();
    assert!(remembered.is_absolute(), "{}", remembered.display());
    assert!(!remembered.components().any(|c| matches!(c, std::path::Component::ParentDir | std::path::Component::CurDir)), "one spelling: {}", remembered.display());
    assert!(remembered.join("project.json").is_file(), "it points at the saved project");
    assert!(h.app.project_path.as_ref().is_some_and(|p| p.is_absolute()));
    // the same project opened again through a relative path is one entry, not two
    h.app.open_project(rel_dir.join("MySong.rawdawg"));
    assert_eq!(h.app.prefs.recent_projects.iter().filter(|p| **p == remembered).count(), 1);
    assert_eq!(h.app.prefs.recent_projects[0], remembered);
    // old relative entries: kept (made absolute) when they still resolve, dropped otherwise
    let mut prefs = crate::prefs::Preferences { recent_projects: vec![rel_dir.join("MySong.rawdawg"), PathBuf::from("Nowhere.rawdawg"), remembered.clone()], ..Default::default() };
    prefs.normalize_recent();
    assert_eq!(prefs.recent_projects, vec![remembered.clone()]);
    let _ = std::fs::remove_dir_all(&rel_dir);
}

#[test]
fn a_tempo_change_lands_in_the_bar_that_was_right_clicked() {
    let mut h = Harness::new(None);
    h.frame(vec![]);
    let surface = h.rect(egui::Id::unique("playlist-surface"));
    let marker = h.rect(super::playlist::marker_id(true, 0));
    // just right of the bar 4 line (tick 3 × 3840), on the marker row
    let x = super::playlist::lane_point(&h.app.playlist, surface, 3 * 3840 + 120, 0).x;
    let at = egui::pos2(x, marker.center().y);
    h.click_at(at, egui::PointerButton::Secondary);
    h.frame(vec![]);
    assert!(h.drawn_texts().iter().any(|t| t == "At bar 4"), "the menu names bar 4");
    // the entry sits to the right; moving there must not change the bar
    let entry = h.drawn_nodes().into_iter().find(|(t, _)| t == "Add tempo change here").map(|(_, r)| r).unwrap();
    h.move_to(egui::pos2(entry.center().x + 80.0, entry.center().y));
    h.frame(vec![]);
    assert!(h.drawn_texts().iter().any(|t| t == "At bar 4"), "the heading stays at bar 4 while the pointer moves");
    h.click_at(entry.center(), egui::PointerButton::Primary);
    let ticks: Vec<i64> = h.app.project.tempo.iter().map(|t| t.tick.0).collect();
    assert_eq!(ticks, vec![0, 3 * 3840], "the new marker is at bar 4");
}

#[test]
fn randomize_starts_from_a_different_seed_every_launch() {
    // two launches: the first Randomize seeds differ (they were equal on every launch)
    let firsts: Vec<u64> = (0..2).map(|_| super::evolve::next_seed(&mut super::evolve::EvolveState::new())).collect();
    assert_ne!(firsts[0], firsts[1], "a new launch is a new sequence");
    // within one launch the sequence still moves on
    let mut st = super::evolve::EvolveState::new();
    let (a, b) = (super::evolve::next_seed(&mut st), super::evolve::next_seed(&mut st));
    assert_ne!(a, b);
    // the rack randomizer too: the same project, reopened in two launches, randomizes differently
    let mut project = Harness::new(None).app.project.clone();
    let master = project.mixer.master_id;
    let inst = rawdawg_project::Instance::native("native.fx.delay", serde_json::Value::Null);
    project.mixer.channels.iter_mut().find(|m| m.id == master).unwrap().inserts.push(inst);
    let rack_after = || {
        let mut h = Harness::new(None);
        // as the app starts: no seed yet, drawn from launch entropy on first use
        h.app.mixer.seed = 0;
        h.app.project = project.clone();
        h.app.rebuild_mirrors();
        super::mixer::randomize_rack(&mut h.app, master);
        h.app.project.mixer_channel(master).unwrap().inserts[0].state.clone()
    };
    assert_ne!(rack_after(), rack_after(), "Randomize rack differs between launches");
}

#[test]
fn drawing_one_note_is_one_undo_step() {
    let mut h = Harness::new(None);
    h.app.editor_view = EditorView::PianoRoll;
    h.frame(vec![]);
    let surface = h.rect(egui::Id::unique("piano-roll-surface"));
    let depth = h.app.history.undo_len();
    // press and drag right four sixteenths
    let from = super::piano_roll::grid_point(&h.app.piano_roll, surface, 30, 60.5);
    let to = super::piano_roll::grid_point(&h.app.piano_roll, surface, 4 * 240 + 30, 60.5);
    h.drag(from, to, egui::PointerButton::Primary);
    let n = h.lane_notes();
    assert_eq!(n.len(), 1);
    assert!(n[0].duration_ticks.0 >= 3 * 240, "the drag set its length: {:?}", n[0]);
    assert_eq!(h.app.history.undo_len(), depth + 1, "one undo step for the note");
    h.key(egui::Key::Z, egui::Modifiers::CTRL);
    assert!(h.lane_notes().is_empty(), "one Ctrl+Z removes it (it used to shrink it first)");
    // a plain click draws one note, one step too
    let p = super::piano_roll::grid_point(&h.app.piano_roll, surface, 960 + 30, 62.5);
    h.click_at(p, egui::PointerButton::Primary);
    assert_eq!(h.lane_notes().len(), 1);
    assert_eq!(h.app.history.undo_len(), depth + 1);
    h.key(egui::Key::Z, egui::Modifiers::CTRL);
    assert!(h.lane_notes().is_empty());
}

#[test]
fn save_and_close_on_a_new_project_closes_after_the_save_as() {
    let root = std::env::temp_dir().join(format!("rawdawg-ui-save-close-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let mut h = Harness::new(None);
    h.app.new_project();
    let (pid, cid) = (h.app.project.patterns[0].id, h.app.project.channels[0].id);
    h.app.apply(rawdawg_project::Command::AddNotes {
        pattern_id: pid,
        channel_id: cid,
        notes: vec![rawdawg_project::Note { id: rawdawg_types::NoteId::new(), tick: Tick(0), duration_ticks: Tick(240), pitch: 60, velocity: 0.8, channel: 0 }],
    });
    let click = |h: &mut Harness, text: &str| {
        h.frame(vec![]);
        h.frame(vec![]);
        let r = h.drawn_nodes().into_iter().find(|(t, _)| t == text).map(|(_, r)| r).unwrap_or_else(|| panic!("{text:?} not drawn"));
        h.click_at(r.center(), egui::PointerButton::Primary);
    };
    // the close request found unsaved changes: Save and close → Save As (cancelled first)
    h.app.files.close_prompt = true;
    click(&mut h, "Save and close");
    assert!(!h.app.files.closing, "not yet: there is no folder to save to");
    click(&mut h, "Cancel");
    assert!(!h.app.files.closing && !h.app.files.close_after_save, "cancelled: rawDAWg stays open");
    // again, and this time save
    h.app.files.close_prompt = true;
    click(&mut h, "Save and close");
    h.app.files.path_input = root.join("closetest").display().to_string();
    click(&mut h, "Save as");
    assert!(!h.app.history.is_dirty(), "saved");
    assert!(root.join("closetest.rawdawg/project.json").is_file());
    assert!(h.app.files.closing, "and rawDAWg closes, as asked");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn with_audio_off_the_ruler_seeks_and_clips_split_at_the_playhead_or_where_right_clicked() {
    let mut h = Harness::new(Some("../../fixtures/demo-song.rawdawg"));
    assert!(!matches!(h.app.audio, super::AudioState::Running(_)), "no engine in this test (like --no-audio)");
    h.frame(vec![]);
    let surface = h.rect(egui::Id::unique("playlist-surface"));
    // the first clip of the first track
    let track = h.app.project.tracks[0].id;
    let clip = h.app.project.clips.iter().filter(|c| c.track_id == track).min_by_key(|c| c.start_tick.0).unwrap().clone();
    let inside = clip.start_tick.0 + clip.length_ticks.0 / 2;
    // click the ruler above it: the playhead moves there
    let x = super::playlist::lane_point(&h.app.playlist, surface, inside, 0).x;
    h.click_at(egui::pos2(x, surface.min.y + 6.0), egui::PointerButton::Primary);
    let at = h.app.playhead_tick().unwrap();
    assert!((at - inside).abs() <= 240, "seeked without audio: {at} vs {inside}");
    // select the clip and split it at the playhead
    h.app.playlist.selection.clear();
    h.app.playlist.selection.insert(clip.id);
    let clips = h.app.project.clips.len();
    let split = h.drawn_nodes().into_iter().find(|(t, _)| t == "Split at playhead").map(|(_, r)| r).expect("the toolbar button");
    h.click_at(split.center(), egui::PointerButton::Primary);
    assert_eq!(h.app.project.clips.len(), clips + 1, "split: {}", h.app.status_line);
    let left = h.app.project.clips.iter().find(|c| c.id == clip.id).unwrap();
    assert_eq!(left.end_tick().0, at, "at the playhead");
    // right-click the right half: "Split here" splits where the right-click was
    let right = h.app.project.clips.iter().find(|c| c.track_id == track && c.start_tick.0 == at).unwrap().clone();
    let here = right.start_tick.0 + right.length_ticks.0 / 2;
    let p = super::playlist::lane_point(&h.app.playlist, surface, here, 0);
    h.click_at(p, egui::PointerButton::Secondary);
    h.frame(vec![]);
    let entry = h.drawn_nodes().into_iter().find(|(t, _)| t.starts_with("Split here")).map(|(_, r)| r).expect("Split here is offered");
    h.click_at(entry.center(), egui::PointerButton::Primary);
    assert_eq!(h.app.project.clips.len(), clips + 2, "split again: {}", h.app.status_line);
    let ends: Vec<i64> = h.app.project.clips.iter().filter(|c| c.track_id == track).map(|c| c.end_tick().0).collect();
    assert!(ends.iter().any(|e| (e - here).abs() <= 240), "split where right-clicked: {ends:?} vs {here}");
}

fn open_mixer_rename_dialog(h: &mut Harness, channel_name: &str, double_click: bool) -> egui::Rect {
    h.app.editor_view = EditorView::Mixer;
    h.frame(vec![]);
    let strip = h.drawn_nodes().into_iter().filter(|(t, _)| t == channel_name).map(|(_, r)| r).max_by(|a, b| a.top().total_cmp(&b.top())).unwrap();
    if double_click {
        h.click_at(strip.center(), egui::PointerButton::Primary);
        h.click_at(strip.center(), egui::PointerButton::Primary);
    } else {
        h.click_at(strip.center(), egui::PointerButton::Secondary);
        let rename = h.node("Rename").expect("strip context menu");
        h.click_at(rename.center(), egui::PointerButton::Primary);
    }
    // Let the newly opened window finish its initial sizing frame.
    h.frame(vec![]);
    strip
}

#[test]
fn mixer_rename_dialog_commits_on_enter_and_button_and_undoes_cleanly() {
    let mut h = Harness::new(None);
    h.app.typing_enabled = true;
    h.app.live_log.clear();

    // 1. Context menu → type → Enter commits in one undo step without triggering typing notes.
    let strip = open_mixer_rename_dialog(&mut h, "Insert 1", false);
    let title = h.node("Rename channel").expect("dialog is open");
    assert!((title.center().x - W * 0.5).abs() < 120.0 && (title.center().y - H * 0.5).abs() < 120.0, "dialog appears centered in the window, not top-left: {title:?}");
    let depth = h.app.history.undo_len();
    h.key(egui::Key::A, egui::Modifiers::COMMAND);
    h.frame(vec![egui::Event::Key { key: egui::Key::Z, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }, egui::Event::Text("Drums".into())]);
    assert!(h.app.live_log.is_empty(), "keys typed in the dialog do not trigger typing notes");
    h.key(egui::Key::Enter, egui::Modifiers::NONE);
    assert_eq!(h.app.project.mixer.channels[1].name, "Drums");
    assert_eq!(h.app.history.undo_len(), depth + 1);
    assert!(h.node("Rename channel").is_none(), "dialog closed after Enter");
    // The strip stays a normal control and undo restores the old name.
    h.click_at(strip.center(), egui::PointerButton::Secondary);
    assert!(h.node("Rename").is_some());
    h.key(egui::Key::Escape, egui::Modifiers::NONE);
    h.app.undo();
    assert_eq!(h.app.project.mixer.channels[1].name, "Insert 1");

    // 2. Double-click → type → Rename button commits in one undo step.
    let mut h = Harness::new(None);
    open_mixer_rename_dialog(&mut h, "Insert 1", true);
    assert!(h.node("Rename channel").is_some(), "double-click opened the rename dialog");
    let depth = h.app.history.undo_len();
    h.key(egui::Key::A, egui::Modifiers::COMMAND);
    h.frame(vec![egui::Event::Text("Vocals".into())]);
    let btn = h.node("Rename").expect("Rename confirm button");
    h.click_at(btn.center(), egui::PointerButton::Primary);
    assert_eq!(h.app.project.mixer.channels[1].name, "Vocals");
    assert_eq!(h.app.history.undo_len(), depth + 1);
    assert!(h.node("Rename channel").is_none(), "dialog closed after clicking Rename");
}

#[test]
fn mixer_rename_dialog_cancels_on_escape_and_cancel_and_rejects_empty_names() {
    let mut h = Harness::new(None);
    h.app.held_notes.track(&TimedEvent::note_on(0, rawdawg_types::events::EventSource::Mouse, 0, 1, 60, 0.8));

    // 1. Escape cancels the dialog without committing or triggering panic.
    open_mixer_rename_dialog(&mut h, "Insert 1", false);
    let depth = h.app.history.undo_len();
    h.key(egui::Key::A, egui::Modifiers::COMMAND);
    h.frame(vec![egui::Event::Text("Draft".into())]);
    h.key(egui::Key::Escape, egui::Modifiers::NONE);
    assert!(h.node("Rename channel").is_none(), "Escape closed the dialog");
    assert_eq!(h.app.project.mixer.channels[1].name, "Insert 1");
    assert_eq!(h.app.history.undo_len(), depth);
    assert!(h.app.held_notes.contains(60), "Escape closed the dialog instead of triggering panic");

    // 2. Cancel button discards edits.
    open_mixer_rename_dialog(&mut h, "Insert 1", false);
    h.key(egui::Key::A, egui::Modifiers::COMMAND);
    h.frame(vec![egui::Event::Text("Draft".into())]);
    let cancel = h.node("Cancel").expect("Cancel button");
    h.click_at(cancel.center(), egui::PointerButton::Primary);
    assert!(h.node("Rename channel").is_none());
    assert_eq!(h.app.project.mixer.channels[1].name, "Insert 1");

    // 3. Whitespace-only name cannot be confirmed by Enter.
    open_mixer_rename_dialog(&mut h, "Insert 1", false);
    h.key(egui::Key::A, egui::Modifiers::COMMAND);
    h.frame(vec![egui::Event::Text("   ".into())]);
    h.key(egui::Key::Enter, egui::Modifiers::NONE);
    assert!(h.node("Rename channel").is_some(), "empty name keeps the dialog open");
    assert_eq!(h.app.project.mixer.channels[1].name, "Insert 1");
    h.key(egui::Key::Escape, egui::Modifiers::NONE);
    assert!(h.node("Rename channel").is_none());
}

#[test]
fn mixer_rename_dialog_survives_fader_drag_and_editor_switch_with_separate_undo() {
    let mut h = Harness::new(None);
    let strip = open_mixer_rename_dialog(&mut h, "Insert 1", false);
    h.key(egui::Key::A, egui::Modifiers::COMMAND);
    h.frame(vec![egui::Event::Text("Vocals".into())]);

    // Dragging the channel's fader keeps the dialog open and records its own undo step.
    let fader = h
        .accessibility_nodes()
        .iter()
        .filter(|n| n.role() == egui::Role::Slider && n.min_numeric_value() == Some(-60.0) && n.max_numeric_value() == Some(12.0))
        .map(Harness::node_rect)
        .find(|r| strip.x_range().contains(r.center().x))
        .expect("the channel's fader");
    let gain = h.app.project.mixer.channels[1].gain_db;
    let depth = h.app.history.undo_len();
    h.drag(fader.center(), egui::pos2(fader.center().x, fader.bottom() - 10.0), egui::PointerButton::Primary);
    assert_ne!(h.app.project.mixer.channels[1].gain_db, gain);
    assert_eq!(h.app.project.mixer.channels[1].name, "Insert 1", "not committed yet");
    assert!(h.node("Rename channel").is_some(), "dialog survives dragging the fader");

    // Switching to another editor keeps the dialog open; confirming commits as a second undo step.
    let rack = h.node("Rack").unwrap().center();
    h.click_at(rack, egui::PointerButton::Primary);
    assert!(h.app.editor_view == EditorView::Rack);
    assert!(h.node("Rename channel").is_some(), "dialog survives leaving the mixer");
    let btn = h.node("Rename").expect("Rename confirm button");
    h.click_at(btn.center(), egui::PointerButton::Primary);
    assert_eq!(h.app.project.mixer.channels[1].name, "Vocals");
    assert_eq!(h.app.history.undo_len(), depth + 2);
    h.app.undo();
    assert_eq!(h.app.project.mixer.channels[1].name, "Insert 1");
    assert_ne!(h.app.project.mixer.channels[1].gain_db, gain);
    h.app.undo();
    assert_eq!(h.app.project.mixer.channels[1].gain_db, gain);
}

#[test]
fn mixer_rename_dialog_handles_ime_composition_and_closes_when_channel_is_deleted() {
    let mut h = Harness::new(None);
    open_mixer_rename_dialog(&mut h, "Insert 1", false);
    h.key(egui::Key::A, egui::Modifiers::COMMAND);
    h.frame(vec![egui::Event::Ime(egui::ImeEvent::Preedit { text: "ka".into(), active_range_chars: None })]);
    assert!(h.ctx.memory(|m| m.focused().is_some()), "IME preedit keeps focus in the dialog");
    h.frame(vec![egui::Event::Ime(egui::ImeEvent::Preedit { text: String::new(), active_range_chars: None }), egui::Event::Ime(egui::ImeEvent::Commit("Song".into()))]);
    assert!(h.ctx.memory(|m| m.focused().is_some()));
    h.key(egui::Key::Enter, egui::Modifiers::NONE);
    assert_eq!(h.app.project.mixer.channels[1].name, "Song");

    // If the target channel disappears while the dialog is open, the dialog closes cleanly.
    let plus = h.drawn_nodes().into_iter().find(|(t, _)| t == "+ Bus").unwrap().1;
    h.click_at(plus.center(), egui::PointerButton::Primary);
    open_mixer_rename_dialog(&mut h, "Bus 2", false);
    assert!(h.node("Rename channel").is_some());
    h.app.undo();
    h.frame(vec![]);
    assert!(h.node("Rename channel").is_none(), "dialog closed after its channel was removed");
}

#[test]
fn mixer_rename_dialog_closes_on_project_switch_and_survives_failed_open() {
    let root = std::env::temp_dir().join(format!("rawdawg-ui-mixer-rename-project-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let source = root.join("Source.rawdawg");
    let copy = root.join("Copy.rawdawg");

    let mut h = Harness::new(None);
    h.app.new_project();
    h.app.save_project_as(source.clone());
    h.app.save_project_as(copy.clone());
    assert!(h.app.open_project_checked(source.clone()));

    open_mixer_rename_dialog(&mut h, "Insert 1", false);
    h.key(egui::Key::A, egui::Modifiers::COMMAND);
    h.frame(vec![egui::Event::Text("Source draft".into())]);
    assert!(h.node("Rename channel").is_some());

    // A failed open preserves the current project and its rename draft.
    assert!(!h.app.open_project_checked(root.join("Missing.rawdawg")));
    h.frame(vec![]);
    assert!(h.node("Rename channel").is_some(), "failed open keeps the dialog open");
    assert!(h.drawn_texts().iter().any(|t| t == "Source draft"), "draft preserved after failed open");

    // Opening Copy (which shares the same channel IDs from Save As) clears Source's rename dialog.
    assert!(h.app.open_project_checked(copy.clone()));
    h.frame(vec![]);
    assert!(h.node("Rename channel").is_none(), "switching projects closes the old rename dialog");
    assert_eq!(h.app.project.mixer.channels[1].name, "Insert 1");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn mixer_rename_dialog_drops_provisional_ime_preedit_and_keeps_draft_on_rejected_edit() {
    let mut h = Harness::new(None);

    // 1. Clicking Rename while IME preedit is active commits confirmed text only, never preedit.
    open_mixer_rename_dialog(&mut h, "Insert 1", false);
    h.key(egui::Key::A, egui::Modifiers::COMMAND);
    h.frame(vec![egui::Event::Text("Vocals".into())]);
    h.frame(vec![egui::Event::Ime(egui::ImeEvent::Preedit { text: "ka".into(), active_range_chars: None })]);
    let btn = h.node("Rename").expect("Rename confirm button");
    h.click_at(btn.center(), egui::PointerButton::Primary);
    assert_eq!(h.app.project.mixer.channels[1].name, "Vocals");
    assert!(h.node("Rename channel").is_none());

    // 2. If a command is rejected (e.g. an active history gesture), the dialog keeps its draft open.
    open_mixer_rename_dialog(&mut h, "Vocals", false);
    h.key(egui::Key::A, egui::Modifiers::COMMAND);
    h.frame(vec![egui::Event::Text("Lead".into())]);
    h.app.history.begin_gesture().unwrap();
    h.key(egui::Key::Enter, egui::Modifiers::NONE);
    assert_eq!(h.app.project.mixer.channels[1].name, "Vocals");
    assert!(h.app.status_line.contains("Edit rejected"), "{}", h.app.status_line);
    assert!(h.node("Rename channel").is_some(), "rejected rename keeps the dialog open");
    assert!(h.drawn_texts().iter().any(|t| t == "Lead"), "draft preserved after rejection");
    // Once the gesture ends, clicking Rename commits the preserved draft.
    h.app.history.commit_gesture().unwrap();
    let btn = h.node("Rename").expect("Rename confirm button");
    h.click_at(btn.center(), egui::PointerButton::Primary);
    assert_eq!(h.app.project.mixer.channels[1].name, "Lead");
    assert!(h.node("Rename channel").is_none());
}

#[test]
fn mixer_rename_dialog_escape_closes_focused_save_as_before_background_rename() {
    let mut h = Harness::new(None);
    open_mixer_rename_dialog(&mut h, "Insert 1", false);
    h.key(egui::Key::A, egui::Modifiers::COMMAND);
    h.frame(vec![egui::Event::Text("Draft".into())]);

    // Ctrl+S on a never-saved project opens and focuses Save As on top of Rename channel.
    h.key_event(egui::Key::S, true, false, egui::Modifiers { ctrl: true, command: true, ..Default::default() });
    h.key_event(egui::Key::S, false, false, egui::Modifiers::NONE);
    h.frame(vec![]);
    assert_eq!(h.app.files.dialog, Some(super::project_files::FileDialog::SaveAs));
    h.frame(vec![egui::Event::Text("MySong".into())]);

    // Escape closes the focused Save As dialog and preserves the background Rename channel draft.
    h.key(egui::Key::Escape, egui::Modifiers::NONE);
    assert!(h.app.files.dialog.is_none(), "Escape dismissed Save As");
    assert!(h.node("Rename channel").is_some(), "background Rename channel dialog stayed open");
    assert!(h.drawn_texts().iter().any(|t| t == "Draft"), "Rename channel draft survived Escape");
    let btn = h.node("Rename").expect("Rename confirm button");
    h.click_at(btn.center(), egui::PointerButton::Primary);
    assert_eq!(h.app.project.mixer.channels[1].name, "Draft");
}

#[test]
fn mixer_rename_dialog_enter_and_ctrl_s_order_controls_what_is_saved() {
    let root = std::env::temp_dir().join(format!("rawdawg-ui-mixer-rename-save-order-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let project_dir = root.join("RenameDialogSave.rawdawg");

    let mut h = Harness::new(None);
    h.app.new_project();
    h.app.save_project_as(project_dir.clone());

    // 1. Text + Enter followed by Ctrl+S in one frame commits first, then saves "Vocals".
    open_mixer_rename_dialog(&mut h, "Insert 1", false);
    h.key(egui::Key::A, egui::Modifiers::COMMAND);
    h.frame(vec![egui::Event::Text("Vocal".into())]);
    h.frame(vec![
        egui::Event::Text("s".into()),
        egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE },
        egui::Event::Key { key: egui::Key::S, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::CTRL },
    ]);
    assert_eq!(h.app.project.mixer.channels[1].name, "Vocals");
    assert!(!h.app.history.is_dirty());
    assert_eq!(rawdawg_project::load_project_dir(&project_dir).unwrap().mixer.channels[1].name, "Vocals");

    // 2. Shift+Enter followed by Ctrl+S commits and saves "Drums".
    open_mixer_rename_dialog(&mut h, "Vocals", false);
    h.key(egui::Key::A, egui::Modifiers::COMMAND);
    h.frame(vec![egui::Event::Text("Drums".into())]);
    h.frame(vec![
        egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::SHIFT },
        egui::Event::Key { key: egui::Key::S, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::CTRL },
    ]);
    assert_eq!(h.app.project.mixer.channels[1].name, "Drums");
    assert!(!h.app.history.is_dirty());
    assert_eq!(rawdawg_project::load_project_dir(&project_dir).unwrap().mixer.channels[1].name, "Drums");

    // 3. Ctrl+S followed by Enter saves the old name first and leaves the committed rename dirty.
    open_mixer_rename_dialog(&mut h, "Drums", false);
    h.key(egui::Key::A, egui::Modifiers::COMMAND);
    h.frame(vec![egui::Event::Text("Lead".into())]);
    h.frame(vec![
        egui::Event::Key { key: egui::Key::S, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::CTRL },
        egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE },
    ]);
    assert_eq!(h.app.project.mixer.channels[1].name, "Lead");
    assert!(h.app.history.is_dirty());
    assert_eq!(rawdawg_project::load_project_dir(&project_dir).unwrap().mixer.channels[1].name, "Drums");

    // 4. Ctrl+S on both sides of Enter saves the old revision first and the new name second.
    open_mixer_rename_dialog(&mut h, "Lead", false);
    h.key(egui::Key::A, egui::Modifiers::COMMAND);
    h.frame(vec![egui::Event::Text("Bass".into())]);
    h.frame(vec![
        egui::Event::Key { key: egui::Key::S, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::CTRL },
        egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE },
        egui::Event::Key { key: egui::Key::S, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::CTRL },
    ]);
    assert_eq!(h.app.project.mixer.channels[1].name, "Bass");
    assert!(!h.app.history.is_dirty());
    assert_eq!(rawdawg_project::load_project_dir(&project_dir).unwrap().mixer.channels[1].name, "Bass");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn mixer_strips_are_deleted_from_their_menu_and_a_deleted_channel_takes_its_own_strip() {
    let mut h = Harness::new(None);
    h.app.editor_view = EditorView::Mixer;
    h.frame(vec![]);
    let node = |h: &mut Harness, text: &str| {
        h.drawn_nodes().into_iter().filter(|(t, _)| t == text).map(|(_, r)| r).max_by(|a, b| a.top().total_cmp(&b.top())).unwrap_or_else(|| panic!("{text:?} not drawn"))
    };
    let names = |h: &Harness| h.app.project.mixer.channels.iter().map(|m| m.name.clone()).collect::<Vec<_>>();
    // + Bus, then the synth plays into it
    let plus = node(&mut h, "+ Bus");
    h.click_at(plus.center(), egui::PointerButton::Primary);
    assert_eq!(names(&h), ["Master", "Insert 1", "Bus 2"]);
    let bus = h.app.project.mixer.channels[2].id;
    let cid = h.app.project.channels[0].id;
    h.app.apply(Command::SetChannelMixer { id: cid, mixer_id: bus });
    let depth = h.app.history.undo_len();
    // right-click its name: Delete strip
    h.frame(vec![]);
    let name = node(&mut h, "Bus 2");
    h.click_at(name.center(), egui::PointerButton::Secondary);
    h.frame(vec![]);
    let del = node(&mut h, "Delete strip (undoable)");
    h.click_at(del.center(), egui::PointerButton::Primary);
    assert_eq!(names(&h), ["Master", "Insert 1"], "the bus is gone");
    assert_eq!(h.app.project.channel(cid).unwrap().mixer_id, h.app.project.mixer.master_id, "its source plays where it played");
    assert!(h.app.status_line.contains("Synth now plays into Master"), "{}", h.app.status_line);
    assert_eq!(h.app.history.undo_len(), depth + 1, "one undo step");
    h.app.undo();
    assert_eq!(names(&h), ["Master", "Insert 1", "Bus 2"]);
    assert_eq!(h.app.project.channel(cid).unwrap().mixer_id, bus);
    // the master has no delete entry
    h.frame(vec![]);
    let master = node(&mut h, "Master");
    h.click_at(master.center(), egui::PointerButton::Secondary);
    h.frame(vec![]);
    assert!(h.drawn_texts().iter().any(|t| t == "Rename"));
    assert!(!h.drawn_texts().iter().any(|t| t == "Delete strip (undoable)"), "never the master");
    h.key(egui::Key::Escape, egui::Modifiers::NONE);
    // a channel deleted in the rack takes the strip that only existed for it
    h.app.apply(Command::SetChannelMixer { id: cid, mixer_id: h.app.project.mixer.channels[1].id });
    h.app.editor_view = EditorView::Rack;
    rack_menu(&mut h, 0, "Delete channel (undoable)");
    assert!(h.app.project.channels.is_empty());
    assert_eq!(names(&h), ["Master", "Bus 2"], "Insert 1 went with its channel, the bus stays");
    assert!(h.app.status_line.contains("and its mixer strip Insert 1"), "{}", h.app.status_line);
    h.app.undo();
    assert_eq!(names(&h), ["Master", "Insert 1", "Bus 2"], "one undo brings both back");
    assert_eq!(h.app.project.channels.len(), 1);
    // a new synth's strip never repeats a name that is taken
    h.app.add_synth_channel();
    let last = h.app.project.mixer.channels.last().unwrap().name.clone();
    assert_eq!(last, "Insert 3");
    h.app.add_synth_channel();
    assert_eq!(h.app.project.mixer.channels.last().unwrap().name, "Insert 4");
    // Master, Insert 1, Insert 3, Insert 4: three strips, but "Insert 4" is taken
    let (cmd, _) = rawdawg_project::routing::remove_mixer_channel(&h.app.project, bus).unwrap();
    h.app.apply(cmd);
    h.app.add_synth_channel();
    assert_eq!(h.app.project.mixer.channels.last().unwrap().name, "Insert 5");
}

/// Every non-ASCII character in the code outside comments, by crate source file: what the UI
/// can draw (labels, parameter names and units, messages).
fn source_symbols() -> std::collections::BTreeMap<char, std::collections::BTreeSet<String>> {
    fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut files = Vec::new();
    for c in std::fs::read_dir(crates).unwrap().flatten() {
        walk(&c.path().join("src"), &mut files);
    }
    let mut found: std::collections::BTreeMap<char, std::collections::BTreeSet<String>> = Default::default();
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        for line in text.lines() {
            // drop a line comment (one not inside a string literal)
            let (mut in_str, mut esc, mut end) = (false, false, line.len());
            for (i, ch) in line.char_indices() {
                match ch {
                    _ if esc => esc = false,
                    '\\' => esc = true,
                    '"' => in_str = !in_str,
                    '/' if !in_str && line[i..].starts_with("//") => {
                        end = i;
                        break;
                    }
                    _ => {}
                }
            }
            for ch in line[..end].chars().filter(|c| !c.is_ascii()) {
                found.entry(ch).or_default().insert(f.strip_prefix(crates).unwrap().display().to_string());
            }
        }
    }
    found
}

#[test]
fn every_symbol_the_code_can_draw_has_a_glyph_in_the_fonts() {
    let mut h = Harness::new(None);
    h.frame(vec![]);
    let mut symbols = source_symbols();
    assert!(symbols.contains_key(&'·'), "the scan sees the UI's strings");
    // the user guide is shown in Help too
    for ch in super::USER_GUIDE.chars().filter(|c| !c.is_ascii() && !c.is_whitespace()) {
        symbols.entry(ch).or_default().insert("docs/USER_GUIDE.md".into());
    }
    let missing: Vec<String> = h.ctx.fonts_mut(|f| {
        symbols
            .iter()
            .filter(|(c, _)| !(f.has_glyph(&egui::FontId::proportional(14.0), **c) && f.has_glyph(&egui::FontId::monospace(14.0), **c)))
            .map(|(c, files)| format!("{c:?} (U+{:04X}) in {files:?}", *c as u32))
            .collect()
    });
    assert!(missing.is_empty(), "no font draws these (they show as empty boxes):\n{}", missing.join("\n"));
}

#[test]
fn at_the_minimum_window_size_the_toolbar_and_status_bar_keep_every_control_visible() {
    // UI_UX: responsive down to 1024×720 with no clipped critical controls (issues #12, #21)
    let long_error = "opening audio stream: the audio stream was lost: ALSA function 'snd_pcm_avail_delay' failed with error 'I/O error (5)'; \
                      fallback to device default buffer also failed: opening audio stream: the audio stream was lost: ALSA function 'snd_pcm_avail_delay' failed with error 'I/O error (5)'";
    for (size, audio, toolbar_audio, status_audio, button) in [
        (egui::vec2(1024.0, 720.0), AudioState::Failed(long_error.into()), "Audio unavailable", "Audio unavailable: opening", "Retry audio"),
        (egui::vec2(1024.0, 720.0), AudioState::Disabled, "Audio off", "Audio off —", "Start audio"),
        // the window's default size
        (egui::vec2(1440.0, 900.0), AudioState::Failed(long_error.into()), "Audio unavailable", "Audio unavailable: opening", "Retry audio"),
    ] {
        let mut h = Harness::new(None);
        h.size = size;
        h.app.audio = audio;
        h.app.project_path = Some(PathBuf::from("/home/me/Music/My first song.rawdawg"));
        h.app.apply(Command::RenameProject { name: "edited".into() });
        h.frame(vec![]);
        h.frame(vec![]);
        let nodes = h.drawn_nodes();
        let (w, ht) = (h.size.x, h.size.y);
        let find = |pred: &dyn Fn(&str) -> bool, in_status: bool, what: &str| {
            nodes
                .iter()
                .find(|(t, r)| pred(t) && if in_status { r.center().y > ht - 40.0 } else { r.center().y < 100.0 })
                .map(|(_, r)| *r)
                .unwrap_or_else(|| panic!("{what} not drawn ({toolbar_audio})"))
        };
        let mut toolbar: Vec<(&str, egui::Rect)> =
            ["Undo", "Redo", "Settings", toolbar_audio, "Metronome", "Play", "Typing keys", "C4", "Vel"].into_iter().map(|t| (t, find(&|x: &str| x == t, false, t))).collect();
        // "MIDI n/a" without a MIDI backend, "MIDI 0" with one (the Windows runner has WinMM)
        let midi = |x: &str| x.strip_prefix("MIDI ").is_some_and(|n| n == "n/a" || n.parse::<usize>().is_ok());
        toolbar.push(("MIDI status", find(&midi, false, "MIDI status")));
        let status: Vec<(&str, egui::Rect)> = vec![
            ("message", find(&|t: &str| t.starts_with("Ready"), true, "message")),
            ("audio state", find(&|t: &str| t.starts_with(status_audio), true, "audio state")),
            (button, find(&|t: &str| t == button, true, button)),
            ("unsaved", find(&|t: &str| t == "unsaved", true, "unsaved")),
            ("project", find(&|t: &str| t == "My first song.rawdawg", true, "project")),
            ("brand", find(&|t: &str| t.contains("Less thinking."), true, "brand")),
        ];
        for group in [&toolbar, &status] {
            for (i, (a, ra)) in group.iter().enumerate() {
                assert!(ra.left() >= 0.0 && ra.right() <= w + 0.5, "{a} {ra:?} leaves the {w} px window ({toolbar_audio})");
                for (b, rb) in &group[i + 1..] {
                    assert!(ra.intersect(*rb).area() <= 1.0, "{a} {ra:?} overlaps {b} {rb:?} ({toolbar_audio})");
                }
            }
        }
        // Undo/Redo stay on the first row with the transport; the typing-keys group moves as one
        let t = |name: &str| toolbar.iter().find(|(n, _)| *n == name).unwrap().1;
        assert!((t("Undo").center().y - t("Play").center().y).abs() < 2.0, "Undo is on the transport row");
        for part in ["C4", "Vel"] {
            assert!((t(part).center().y - t("Typing keys").center().y).abs() < 6.0, "{part} stays with Typing keys");
        }
        // the status bar keeps its reading order
        let x = |name: &str| status.iter().find(|(n, _)| *n == name).unwrap().1;
        assert!(x("message").right() <= x("audio state").left() && x("audio state").right() <= x(button).left());
        assert!(x(button).right() <= x("unsaved").left() && x("unsaved").right() <= x("project").left() && x("project").right() <= x("brand").left());
    }
}

/// Fill colour of every rectangle painted in `shapes`, with its rect.
fn filled_rects(shapes: &[egui::epaint::ClippedShape]) -> Vec<(egui::Color32, egui::Rect)> {
    fn walk(s: &egui::Shape, out: &mut Vec<(egui::Color32, egui::Rect)>) {
        match s {
            egui::Shape::Rect(r) => out.push((r.fill, r.rect)),
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for c in shapes {
        walk(&c.shape, &mut out);
    }
    out
}

#[test]
fn held_keys_are_named_in_the_toolbar_and_light_up_in_the_piano_roll() {
    let mut h = Harness::new(None);
    h.app.editor_view = EditorView::PianoRoll;
    h.app.typing_enabled = true;
    h.frame(vec![]);
    let toolbar_text = |h: &mut Harness| h.drawn_nodes().into_iter().filter(|(_, r)| r.center().y < 100.0).map(|(t, _)| t).collect::<Vec<_>>();
    let lit = |h: &mut Harness| {
        let mut ys: Vec<f32> = filled_rects(&h.shapes()).into_iter().filter(|(c, _)| *c == theme::KEY_HELD).map(|(_, r)| r.center().y).collect();
        ys.sort_by(|a, b| b.total_cmp(a));
        ys
    };
    assert!(lit(&mut h).is_empty());
    // C, Eb and G on the typing keys (Z, D, B): middle C is C4
    for k in [egui::Key::Z, egui::Key::D, egui::Key::B] {
        h.key_event(k, true, false, egui::Modifiers::NONE);
    }
    h.frame(vec![]);
    assert_eq!(h.app.held_notes.pitches(), vec![60, 63, 67]);
    assert!(toolbar_text(&mut h).iter().any(|t| t == "C4 Eb4 G4 · Cm"), "{:?}", toolbar_text(&mut h));
    // the piano roll lights those three keys: rows 3 and 4 semitones apart, lowest at the bottom
    let ys = lit(&mut h);
    assert_eq!(ys.len(), 3, "{ys:?}");
    let row = h.app.piano_roll.row_h;
    assert!(((ys[0] - ys[1]) - 3.0 * row).abs() < 1.0 && ((ys[1] - ys[2]) - 4.0 * row).abs() < 1.0, "{ys:?} with rows of {row}");
    // letting go of Eb leaves C and G, then nothing
    h.key_event(egui::Key::D, false, false, egui::Modifiers::NONE);
    h.frame(vec![]);
    assert!(toolbar_text(&mut h).iter().any(|t| t == "C4 G4 · C power"));
    assert_eq!(lit(&mut h).len(), 2);
    h.key_event(egui::Key::Z, false, false, egui::Modifiers::NONE);
    h.key_event(egui::Key::B, false, false, egui::Modifiers::NONE);
    h.frame(vec![]);
    assert!(h.app.held_notes.pitches().is_empty());
    assert!(lit(&mut h).is_empty());
    // a MIDI keyboard's keys show too, and a panic (Esc) clears them
    h.app.live(TimedEvent::note_on(0, EventSource::MidiDevice(0), 0, 40, 64, 0.7));
    h.app.live(TimedEvent::note_on(0, EventSource::MidiDevice(0), 0, 41, 67, 0.7));
    h.app.live(TimedEvent::note_on(0, EventSource::MidiDevice(0), 0, 42, 72, 0.7));
    h.frame(vec![]);
    assert!(toolbar_text(&mut h).iter().any(|t| t == "E4 G4 C5 · C/E"));
    h.key(egui::Key::Escape, egui::Modifiers::NONE);
    assert!(h.app.held_notes.pitches().is_empty(), "Panic lets go of the display too");
    // with typing keys on, S plays C#4 over the playlist instead of splitting clips
    h.app.key_target = super::KeyTarget::Playlist;
    let status = h.app.status_line.clone();
    h.key_event(egui::Key::S, true, false, egui::Modifiers::NONE);
    h.frame(vec![]);
    assert_eq!(h.app.held_notes.pitches(), vec![61]);
    assert!(toolbar_text(&mut h).iter().any(|t| t == "C#4"));
    assert_eq!(h.app.status_line, status, "no split message");
    h.key_event(egui::Key::S, false, false, egui::Modifiers::NONE);
    h.frame(vec![]);
}

#[test]
fn the_amplitude_envelope_is_edited_by_dragging_its_points_and_the_values_follow_live() {
    use rawdawg_synthesizer::params;
    use rawdawg_ui::envelope::{Adsr, AdsrPoint, curve_rect, point_pos};
    let mut h = Harness::new(None);
    h.app.editor_view = EditorView::Synth;
    h.frame(vec![]);
    let instance = h.app.project.channels[0].instrument.id;
    let id = super::synth_panel::envelope_id(instance);
    let norm = |h: &Harness, pid: u32| h.app.synth_mirrors[&instance].param(rawdawg_types::ParamId(pid)).unwrap();
    let phys = |h: &Harness, pid: u32| {
        let d = params::DESCRIPTORS.iter().find(|d| d.id.0 == pid).unwrap();
        d.to_physical(norm(h, pid)) as f32
    };
    let adsr = |h: &Harness| Adsr {
        attack_s: phys(h, params::AMP_ATTACK),
        decay_s: phys(h, params::AMP_DECAY),
        sustain: phys(h, params::AMP_SUSTAIN),
        release_s: phys(h, params::AMP_RELEASE),
    };
    let rect = h.scroll_into_view("the envelope", |h| {
        h.frame(vec![]);
        h.ctx.read_response(id).map(|r| r.rect)
    });
    for name in ["Attack", "Decay", "Sustain", "Release"] {
        assert!(h.node(name).is_some(), "{name} slider");
    }
    let saved = |h: &Harness| -> rawdawg_synthesizer::Patch { serde_json::from_value(h.app.project.channels[0].instrument.state.clone()).unwrap() };
    let before = saved(&h);
    let depth = h.app.history.undo_len();
    // drag the release point to the right, step by step: the value follows while dragging
    let release_text = |h: &mut Harness| {
        let l = h.node("Release").unwrap();
        h.drawn_nodes().into_iter().filter(|(t, r)| t != "Release" && (r.center().y - l.center().y).abs() < 4.0 && r.left() > l.right()).map(|(t, _)| t).next().unwrap()
    };
    let text0 = release_text(&mut h);
    let from = point_pos(curve_rect(rect), &adsr(&h), AdsrPoint::Release);
    let to = from - egui::vec2(50.0, 0.0);
    h.move_to(from);
    h.frame(vec![egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }]);
    for i in 1..=4 {
        h.frame(vec![egui::Event::PointerMoved(from + (to - from) * (i as f32 / 8.0))]);
    }
    let text_mid = release_text(&mut h);
    assert_ne!(text_mid, text0, "the Release readout follows the drag");
    assert_eq!(h.app.history.undo_len(), depth, "nothing committed mid-drag");
    for i in 5..=8 {
        h.frame(vec![egui::Event::PointerMoved(from + (to - from) * (i as f32 / 8.0))]);
    }
    h.frame(vec![egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }]);
    h.frame(vec![]);
    let after = saved(&h);
    assert!(after.amp_env.release_s < before.amp_env.release_s, "dragged left: shorter release ({} → {})", before.amp_env.release_s, after.amp_env.release_s);
    assert_eq!(h.app.history.undo_len(), depth + 1, "one undo step");
    let mut only_release = before.clone();
    only_release.amp_env.release_s = after.amp_env.release_s;
    assert_eq!(after, only_release, "nothing else moved");
    let now = point_pos(curve_rect(rect), &adsr(&h), AdsrPoint::Release);
    assert!((now.x - to.x).abs() < 1.5, "the release end is where it was dropped: {now:?} vs {to:?}");
    // the attack peak follows the pointer too
    let from = point_pos(curve_rect(rect), &adsr(&h), AdsrPoint::Attack);
    h.drag(from, from + egui::vec2(60.0, 0.0), egui::PointerButton::Primary);
    let now = point_pos(curve_rect(rect), &adsr(&h), AdsrPoint::Attack);
    assert!((now.x - from.x - 60.0).abs() < 1.5 && saved(&h).amp_env.attack_s > after.amp_env.attack_s, "{now:?} vs {from:?}");
    h.app.undo();
    assert_eq!(saved(&h), after, "the attack drag is one undo step");
    // the decay point moves decay (left/right) and sustain (up/down) in one step
    let from = point_pos(curve_rect(rect), &adsr(&h), AdsrPoint::Decay);
    h.drag(from, from + egui::vec2(40.0, 30.0), egui::PointerButton::Primary);
    let moved = saved(&h);
    assert!(point_pos(curve_rect(rect), &adsr(&h), AdsrPoint::Decay).distance(from + egui::vec2(40.0, 30.0)) < 1.5);
    assert!(moved.amp_env.decay_s > after.amp_env.decay_s && moved.amp_env.sustain < after.amp_env.sustain, "{:?}", moved.amp_env);
    assert_eq!(h.app.history.undo_len(), depth + 2);
    h.app.undo();
    assert_eq!(saved(&h), after, "one undo restores both");
    // the sustain point only moves the level
    let from = point_pos(curve_rect(rect), &adsr(&h), AdsrPoint::Sustain);
    h.drag(from, from - egui::vec2(0.0, 20.0), egui::PointerButton::Primary);
    let up = saved(&h);
    assert!(up.amp_env.sustain > after.amp_env.sustain && up.amp_env.decay_s == after.amp_env.decay_s);
    // a press away from the points changes nothing
    let depth = h.app.history.undo_len();
    let empty = egui::pos2(rect.center().x, rect.max.y - 40.0);
    h.drag(empty, empty + egui::vec2(30.0, 0.0), egui::PointerButton::Primary);
    assert_eq!(h.app.history.undo_len(), depth);
}

#[test]
fn oscillators_start_from_templates_and_stack_up_to_three_with_octave_detune_and_level() {
    use rawdawg_dsp::oscillator::BasicWave;
    use rawdawg_synthesizer::patch::Generator;
    let mut h = Harness::new(None);
    h.app.editor_view = EditorView::Synth;
    h.frame(vec![]);
    let saved = |h: &Harness| -> rawdawg_synthesizer::Patch { serde_json::from_value(h.app.project.channels[0].instrument.state.clone()).unwrap() };
    let start_from = h.scroll_into_view("Start from", |h| h.node("Start from"));
    // a template button: on the "Start from" row
    // a template button: the first with that name from the "Start from" row on (it may wrap)
    let template = |h: &mut Harness, name: &str| {
        let row = h.node("Start from").unwrap();
        let r = h
            .drawn_nodes()
            .into_iter()
            .filter(|(t, r)| t == name && r.top() > row.top() - 4.0)
            .map(|(_, r)| r)
            .min_by(|a, b| a.top().total_cmp(&b.top()).then(a.left().total_cmp(&b.left())))
            .unwrap_or_else(|| panic!("template {name}"));
        h.click_at(r.center(), egui::PointerButton::Primary);
    };
    let _ = start_from;
    let wave = |p: &rawdawg_synthesizer::Patch, i: usize| match &p.layers[i].generator {
        Generator::Basic { wave, .. } => Some(*wave),
        _ => None,
    };
    let depth = h.app.history.undo_len();
    for (name, w) in [("Sine", BasicWave::Sine), ("Saw", BasicWave::Saw), ("Square", BasicWave::Pulse), ("Triangle", BasicWave::Triangle)] {
        template(&mut h, name);
        let p = saved(&h);
        assert_eq!(p.layers.len(), 1);
        assert_eq!(wave(&p, 0), Some(w), "{name}");
        assert_eq!(p.name, name);
    }
    assert_eq!(h.app.history.undo_len(), depth + 4, "one undo step each");
    template(&mut h, "Saw octaves");
    let p = saved(&h);
    assert_eq!(
        (0..3).map(|i| (wave(&p, i), p.layers[i].coarse_semitones)).collect::<Vec<_>>(),
        vec![(Some(BasicWave::Saw), 0), (Some(BasicWave::Saw), 12), (Some(BasicWave::Saw), 24)]
    );
    template(&mut h, "Detuned saws + sub");
    let p = saved(&h);
    assert_eq!(p.layers.len(), 3);
    assert_eq!((wave(&p, 0), p.layers[0].fine_cents, p.layers[0].coarse_semitones), (Some(BasicWave::Saw), -7.0, 0));
    assert_eq!((wave(&p, 1), p.layers[1].fine_cents), (Some(BasicWave::Saw), 7.0));
    assert_eq!((wave(&p, 2), p.layers[2].coarse_semitones), (Some(BasicWave::Sine), -12));
    h.frame(vec![]);
    let texts = h.drawn_texts();
    for t in ["-7 cents", "+7 cents", "70%", "40%", "OSC 3"] {
        assert!(texts.iter().any(|x| x == t), "{t} shown");
    }
    assert!(h.node("+ Oscillator").is_none(), "three is the most");
    // from one saw: + Oscillator stacks a copy an octave up; its octave + goes one more
    template(&mut h, "Saw");
    let add = h.node("+ Oscillator").unwrap();
    h.click_at(add.center(), egui::PointerButton::Primary);
    let p = saved(&h);
    assert_eq!(p.layers.len(), 2);
    assert_eq!((wave(&p, 1), p.layers[1].coarse_semitones), (Some(BasicWave::Saw), 12));
    let osc2 = h.node("OSC 2").unwrap();
    let plus = h
        .drawn_nodes()
        .into_iter()
        .filter(|(t, r)| t == "+" && (r.center().y - osc2.center().y).abs() < 6.0 && r.left() > osc2.right())
        .map(|(_, r)| r)
        .min_by(|a, b| a.left().total_cmp(&b.left()))
        .expect("octave + on OSC 2");
    let depth = h.app.history.undo_len();
    h.click_at(plus.center(), egui::PointerButton::Primary);
    assert_eq!(saved(&h).layers[1].coarse_semitones, 24, "two octaves up");
    assert_eq!(h.app.history.undo_len(), depth + 1);
    // OSC 1's waveform menu: Square
    let osc1 = h.node("OSC 1").unwrap();
    let combo = h
        .drawn_nodes()
        .into_iter()
        .filter(|(t, r)| t == "Saw" && (r.center().y - osc1.center().y).abs() < 6.0 && r.left() > osc1.right())
        .map(|(_, r)| r)
        .next()
        .expect("OSC 1 waveform");
    h.click_at(combo.center(), egui::PointerButton::Primary);
    h.frame(vec![]);
    let square =
        h.drawn_nodes().into_iter().filter(|(t, r)| t == "Square" && r.top() > combo.top()).map(|(_, r)| r).min_by(|a, b| a.top().total_cmp(&b.top())).expect("menu entry");
    h.click_at(square.center(), egui::PointerButton::Primary);
    assert_eq!(wave(&saved(&h), 0), Some(BasicWave::Pulse));
    // × removes an oscillator
    let x = h.drawn_nodes().into_iter().filter(|(t, _)| t == "×").map(|(_, r)| r).max_by(|a, b| a.top().total_cmp(&b.top())).expect("remove");
    h.click_at(x.center(), egui::PointerButton::Primary);
    assert_eq!(saved(&h).layers.len(), 1);
}

#[test]
fn the_harmonic_editor_builds_the_wave_from_levels_shapes_and_random_harmonics() {
    use rawdawg_synthesizer::sculpt;
    let mut h = Harness::new(None);
    h.app.editor_view = EditorView::Synth;
    h.app.synth_panel.waveform.mode = super::waveform::CanvasMode::Harmonics;
    h.frame(vec![]);
    let instance = h.app.project.channels[0].instrument.id;
    let cycle = |h: &Harness| -> Vec<f32> {
        let p: rawdawg_synthesizer::Patch = serde_json::from_value(h.app.project.channels[0].instrument.state.clone()).unwrap();
        match &p.layers[0].generator {
            rawdawg_synthesizer::patch::Generator::Wavetable { cycle, .. } => cycle.clone(),
            g => panic!("not a drawn wave: {g:?}"),
        }
    };
    // the editor's own buttons sit under its bars (the Oscillators section has a "Square" too)
    let bars_rect = |h: &mut Harness| {
        h.frame(vec![]);
        h.ctx.read_response(super::waveform::harmonic_bars_id(instance)).map(|r| r.rect)
    };
    let button = |h: &mut Harness, name: &str| {
        let r = h.scroll_into_view(name, |h| {
            let bars = bars_rect(h)?;
            h.drawn_nodes().into_iter().filter(|(t, r)| t == name && r.top() > bars.bottom()).map(|(_, r)| r).min_by(|a, b| a.top().total_cmp(&b.top()))
        });
        h.click_at(r.center(), egui::PointerButton::Primary);
    };
    let depth = h.app.history.undo_len();
    button(&mut h, "Square");
    let (levels, _) = sculpt::harmonic_levels(&cycle(&h), 20);
    for (k, l) in levels.iter().enumerate().take(16) {
        let want = if k % 2 == 0 { 1.0 / (k + 1) as f32 } else { 0.0 };
        assert!((l - want).abs() < 0.01, "harmonic {}: {l} vs {want}", k + 1);
    }
    assert!(sculpt::energy_above(&cycle(&h), 16) < 1e-6, "nothing above the 16 harmonics");
    assert_eq!(h.app.history.undo_len(), depth + 1);
    // paint harmonic 2 to about half height
    let bars = h.scroll_into_view("the bars", bars_rect);
    let slot = bars.width() / 16.0;
    let at = egui::pos2(bars.min.x + slot * 1.5, bars.max.y - bars.height() * 0.5);
    h.click_at(at, egui::PointerButton::Primary);
    let (levels, _) = sculpt::harmonic_levels(&cycle(&h), 16);
    assert!((levels[1] - 0.5).abs() < 0.03, "harmonic 2 at half: {}", levels[1]);
    assert!((levels[0] - 1.0).abs() < 1e-3 && (levels[2] - 1.0 / 3.0).abs() < 0.01, "the others stay: {levels:?}");
    assert_eq!(h.app.history.undo_len(), depth + 2, "a click is one undo step");
    // a drag across bars 5..8 at full height sets each of them
    let y = bars.min.y + 2.0;
    h.drag(egui::pos2(bars.min.x + slot * 4.5, y), egui::pos2(bars.min.x + slot * 7.5, y), egui::PointerButton::Primary);
    let (levels, _) = sculpt::harmonic_levels(&cycle(&h), 16);
    assert!(levels[4..8].iter().all(|l| *l > 0.95), "{levels:?}");
    assert_eq!(h.app.history.undo_len(), depth + 3, "a drag is one undo step");
    // fewer bars: looking at 8 changes nothing yet (no rewrite, no undo step) ...
    h.app.synth_panel.waveform.harmonics.count = 9;
    let minus = {
        let label = h.scroll_into_view("the count", |h| h.node("9 harmonics"));
        h.drawn_nodes().into_iter().filter(|(t, r)| t == "−" && (r.center().y - label.center().y).abs() < 6.0).map(|(_, r)| r).next().expect("−")
    };
    let (wave, depth) = (cycle(&h), h.app.history.undo_len());
    h.click_at(minus.center(), egui::PointerButton::Primary);
    assert_eq!(h.app.synth_panel.waveform.harmonics.count, 8);
    assert_eq!(cycle(&h), wave, "changing the count does not rewrite the wave");
    assert_eq!(h.app.history.undo_len(), depth);
    assert!(sculpt::energy_above(&cycle(&h), 8) > 1e-4, "the 9th is still in the wave");
    h.frame(vec![]);
    assert!(h.drawn_texts().iter().any(|t| t.starts_with("This wave also has harmonics above 8")), "and the editor says so");
    // ... the next edit keeps the first 8
    // random harmonics: within the count, the fundamental kept, one undo step, new each time
    let before = cycle(&h);
    button(&mut h, "Randomize harmonics");
    let first = cycle(&h);
    assert_ne!(first, before);
    assert!(sculpt::energy_above(&first, 8) < 1e-6);
    let (levels, _) = sculpt::harmonic_levels(&first, 8);
    assert!(levels[0] >= 0.5, "the fundamental keeps the pitch: {levels:?}");
    button(&mut h, "Randomize harmonics");
    assert_ne!(cycle(&h), first, "another set");
    h.app.undo();
    assert_eq!(cycle(&h), first, "Undo goes back one randomization");
    // the editor follows an undo: its bars describe the wave again
    h.frame(vec![]);
    let shown = h.app.synth_panel.waveform.harmonics.levels.clone();
    let (levels, _) = sculpt::harmonic_levels(&first, sculpt::MAX_HARMONICS);
    assert!(shown.iter().zip(&levels).all(|(a, b)| (a - b).abs() < 1e-4));
}

#[test]
fn the_clip_menu_belongs_to_the_clip_right_clicked_and_the_ruler_opens_none() {
    let mut h = Harness::new(Some("../../fixtures/demo-song.rawdawg"));
    h.frame(vec![]);
    let surface = h.rect(egui::Id::unique("playlist-surface"));
    let track = h.app.project.tracks[0].id;
    let clip = h.app.project.clips.iter().filter(|c| c.track_id == track).min_by_key(|c| c.start_tick.0).unwrap().clone();
    let clips = h.app.project.clips.len();
    // right-click the clip: its menu
    let p = super::playlist::lane_point(&h.app.playlist, surface, clip.start_tick.0 + clip.length_ticks.0 / 2, 0);
    h.click_at(p, egui::PointerButton::Secondary);
    h.frame(vec![]);
    assert!(h.node("Delete").is_some(), "the clip menu is open");
    // close it (click elsewhere), then right-click the ruler: no clip menu, nothing deleted
    let heading = h.node("Playlist").unwrap();
    h.click_at(heading.center(), egui::PointerButton::Primary);
    h.frame(vec![]);
    let ruler = egui::pos2(p.x + 40.0, surface.min.y + 6.0);
    h.click_at(ruler, egui::PointerButton::Secondary);
    h.frame(vec![]);
    let deletes: Vec<egui::Rect> = h.drawn_nodes().into_iter().filter(|(t, r)| t == "Delete" && r.top() > surface.min.y).map(|(_, r)| r).collect();
    assert!(deletes.is_empty(), "no clip menu on the ruler: {deletes:?}");
    assert_eq!(h.app.project.clips.len(), clips);
}

#[test]
fn with_audio_off_the_toolbar_shows_the_playhead_and_paste_goes_into_view() {
    let mut h = Harness::new(None);
    h.app.seek(2 * 3840 + 960);
    h.frame(vec![]);
    let toolbar: Vec<String> = h.drawn_nodes().into_iter().filter(|(_, r)| r.center().y < 100.0).map(|(t, _)| t).collect();
    assert!(toolbar.iter().any(|t| t == "3:2"), "bar 3, beat 2 with audio off: {toolbar:?}");
    // a copied note pastes into the visible part of the roll, not at the playhead off-screen
    h.app.editor_view = EditorView::PianoRoll;
    h.app.key_target = super::KeyTarget::PianoRoll;
    let (pid, cid) = (h.app.project.patterns[0].id, h.app.project.channels[0].id);
    h.app.apply(Command::SetPatternLength { id: pid, length_ticks: Tick(8 * 3840), steps: 16 * 8, step_ticks: Tick(240) });
    let note = rawdawg_project::Note { id: rawdawg_types::NoteId::new(), tick: Tick(0), duration_ticks: Tick(240), pitch: 60, velocity: 0.8, channel: 0 };
    h.app.apply(Command::AddNotes { pattern_id: pid, channel_id: cid, notes: vec![note] });
    h.app.seek(0);
    h.app.piano_roll.clipboard = vec![rawdawg_project::Note { tick: Tick(0), ..note }];
    h.app.piano_roll.scroll_tick = 4.0 * 3840.0;
    h.frame(vec![]);
    // Ctrl on Linux and Windows: egui's ctrl and command flags both set
    let ctrl = egui::Modifiers { ctrl: true, command: true, ..Default::default() };
    h.key_event(egui::Key::V, true, false, ctrl);
    h.key_event(egui::Key::V, false, false, ctrl);
    h.frame(vec![]);
    let ticks: Vec<i64> = h.app.project.pattern(pid).unwrap().lanes.iter().find(|l| l.channel_id == cid).unwrap().notes.iter().map(|n| n.tick.0).collect();
    assert!(ticks.iter().any(|t| *t >= 4 * 3840), "pasted at the view start: {ticks:?}");
}

#[test]
fn save_and_close_is_forgotten_when_its_save_as_is_left_for_another_dialog() {
    let root = std::env::temp_dir().join(format!("rawdawg-ui-save-close-left-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let mut h = Harness::new(None);
    h.app.new_project();
    h.app.apply(Command::RenameProject { name: "changed".into() });
    let click = |h: &mut Harness, text: &str| {
        h.frame(vec![]);
        h.frame(vec![]);
        let r = h.drawn_nodes().into_iter().find(|(t, _)| t == text).map(|(_, r)| r).unwrap_or_else(|| panic!("{text:?} not drawn"));
        h.click_at(r.center(), egui::PointerButton::Primary);
    };
    h.app.files.close_prompt = true;
    click(&mut h, "Save and close");
    assert!(h.app.files.close_after_save, "Save As opened for the close");
    // the user goes to File › New instead
    h.app.files.dialog = Some(super::project_files::FileDialog::New);
    h.frame(vec![]);
    assert!(!h.app.files.close_after_save, "leaving that Save As forgets the close");
    h.app.files.dialog = None;
    // a later ordinary Save As saves and stays open
    h.app.save_project_checked();
    h.app.files.path_input = root.join("later").display().to_string();
    click(&mut h, "Save as");
    assert!(root.join("later.rawdawg/project.json").is_file());
    assert!(!h.app.files.closing, "an ordinary Save As does not quit");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn export_replaces_only_what_it_named_and_its_path_follows_the_project() {
    let root = std::env::temp_dir().join(format!("rawdawg-ui-export-replace-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let mut h = Harness::new(None);
    let (pid, cid) = (h.app.project.patterns[0].id, h.app.project.channels[0].id);
    h.app.apply(Command::AddNotes {
        pattern_id: pid,
        channel_id: cid,
        notes: vec![rawdawg_project::Note { id: rawdawg_types::NoteId::new(), tick: Tick(0), duration_ticks: Tick(240), pitch: 60, velocity: 0.8, channel: 0 }],
    });
    h.app.export.song = false;
    std::fs::write(root.join("a.mid"), b"old a").unwrap();
    std::fs::write(root.join("precious.mid"), b"precious").unwrap();
    h.app.export.open = true;
    h.app.export.path = root.join("a").display().to_string();
    h.frame(vec![]);
    super::export::request_midi(&mut h.app);
    h.frame(vec![]);
    assert!(h.drawn_texts().iter().any(|t| t.contains("a.mid already exists")));
    // the target changes before Replace (the form is locked; anything else changing it): asked again
    h.app.export.path = root.join("precious").display().to_string();
    let replace = h.node("Replace").unwrap();
    h.click_at(replace.center(), egui::PointerButton::Primary);
    assert_eq!(std::fs::read(root.join("precious.mid")).unwrap(), b"precious", "never overwritten without being named");
    assert!(h.drawn_texts().iter().any(|t| t.contains("precious.mid already exists")), "the prompt names the new target");
    let keep = h.node("Keep the existing file").unwrap();
    h.click_at(keep.center(), egui::PointerButton::Primary);
    // relative paths are in the project folder, a folder gets the project's name, names are made safe
    h.app.project_path = Some(root.clone());
    h.app.project.name = "Mix: final?".into();
    h.app.export.path = "rel/mix".into();
    assert_eq!(super::export::midi_path(&h.app).unwrap(), root.join("rel/mix.mid"));
    h.app.export.path = format!("{}/", root.display());
    assert_eq!(super::export::audio_path(&h.app).unwrap(), root.join("Mix_ final_.wav"));
    h.app.export.path = root.join("Song v1.2").display().to_string();
    assert_eq!(super::export::midi_path(&h.app).unwrap(), root.join("Song v1.2.mid"), "a dot in the name stays");
    // a never-saved project's typed path does not follow File › New
    h.app.project_path = None;
    h.app.export.path = "/somewhere/mix".into();
    h.frame(vec![]);
    h.app.new_project();
    h.frame(vec![]);
    assert_eq!(h.app.export.path, "", "a new project starts without the old path");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn oscillator_rows_drive_detune_level_switch_octave_and_waveform() {
    use rawdawg_dsp::oscillator::BasicWave;
    use rawdawg_synthesizer::patch::Generator;
    let mut h = Harness::new(None);
    h.app.editor_view = EditorView::Synth;
    h.frame(vec![]);
    let instance = h.app.project.channels[0].instrument.id;
    let saved = |h: &Harness| -> rawdawg_synthesizer::Patch { serde_json::from_value(h.app.project.channels[0].instrument.state.clone()).unwrap() };
    // Init's custom wave becomes a square from the waveform menu
    let osc1 = h.scroll_into_view("OSC 1", |h| h.node("OSC 1"));
    let combo = h.drawn_nodes().into_iter().filter(|(t, r)| t == "Custom wave" && (r.center().y - osc1.center().y).abs() < 6.0).map(|(_, r)| r).next().expect("waveform menu");
    h.click_at(combo.center(), egui::PointerButton::Primary);
    h.frame(vec![]);
    let square = h.drawn_nodes().into_iter().filter(|(t, r)| t == "Square" && r.top() > combo.top()).map(|(_, r)| r).min_by(|a, b| a.top().total_cmp(&b.top())).unwrap();
    h.click_at(square.center(), egui::PointerButton::Primary);
    assert!(matches!(saved(&h).layers[0].generator, Generator::Basic { wave: BasicWave::Pulse, .. }), "a basic square now");
    // detune and level sliders: one undo step per drag, the readouts follow
    let slider = |h: &mut Harness, name: &str| h.scroll_into_view(name, |h| h.node(name));
    let depth = h.app.history.undo_len();
    let detune = slider(&mut h, "OSC 1 Detune");
    h.drag(detune.center(), detune.center() + egui::vec2(20.0, 0.0), egui::PointerButton::Primary);
    let cents = saved(&h).layers[0].fine_cents;
    assert!(cents > 5.0, "detuned up: {cents}");
    assert_eq!(h.app.history.undo_len(), depth + 1);
    assert!(h.drawn_texts().iter().any(|t| *t == format!("{cents:+.0} cents")));
    let level = slider(&mut h, "OSC 1 Level");
    h.drag(level.center(), level.left_center() + egui::vec2(2.0, 0.0), egui::PointerButton::Primary);
    assert!(saved(&h).layers[0].level_db < -20.0, "turned down: {}", saved(&h).layers[0].level_db);
    // octave − / + move whole octaves; on/off switches the oscillator
    let osc1 = slider(&mut h, "OSC 1");
    let minus = h.drawn_nodes().into_iter().filter(|(t, r)| t == "−" && (r.center().y - osc1.center().y).abs() < 6.0 && r.left() > osc1.right()).map(|(_, r)| r).next().unwrap();
    h.click_at(minus.center(), egui::PointerButton::Primary);
    assert_eq!(saved(&h).layers[0].coarse_semitones, -12);
    h.click_at(osc1.center(), egui::PointerButton::Primary);
    assert!(!saved(&h).layers[0].enabled, "switched off");
    // a layer louder than 100 % keeps its level when its slider is touched where it stands
    let mut p = saved(&h);
    p.layers[0].level_db = 3.0;
    p.layers[0].enabled = true;
    super::synth_panel::commit_patch(&mut h.app, instance, p);
    h.frame(vec![]);
    assert!(h.drawn_texts().iter().any(|t| t.starts_with("141%")), "+3 dB reads 141 %");
    let level = slider(&mut h, "OSC 1 Level");
    let depth = h.app.history.undo_len();
    // the handle is at the right end (141 % is the top of the scale now); nudge it a little
    let handle = level.right_center() - egui::vec2(6.0, 0.0);
    h.drag(handle, handle - egui::vec2(9.0, 0.0), egui::PointerButton::Primary);
    let db = saved(&h).layers[0].level_db;
    assert!(db > 1.0 && db < 3.0, "a small step down from +3 dB, not a drop to 100 % or less: {db}");
    assert_eq!(h.app.history.undo_len(), depth + 1);
}

#[test]
fn at_1920_the_oscillators_and_envelope_sit_side_by_side_and_both_work() {
    let mut h = Harness::new(None);
    h.size = egui::vec2(1920.0, 1036.0);
    h.app.editor_view = EditorView::Synth;
    h.frame(vec![]);
    h.frame(vec![]);
    let instance = h.app.project.channels[0].instrument.id;
    let env_id = super::synth_panel::envelope_id(instance);
    let start = h.scroll_into_view("Start from", |h| h.node("Start from"));
    let saw_square = h.drawn_nodes().into_iter().filter(|(t, r)| t == "Saw + Square" && r.top() > start.top() - 4.0).map(|(_, r)| r).next().unwrap();
    h.click_at(saw_square.center(), egui::PointerButton::Primary);
    let env = h.scroll_into_view("the envelope", |h| {
        h.frame(vec![]);
        h.ctx.read_response(env_id).map(|r| r.rect)
    });
    let osc2 = h.node("OSC 2").unwrap();
    assert!((env.top() - osc2.top()).abs() < 200.0 && env.left() > osc2.right(), "side by side: {env:?} / {osc2:?}");
    let x = h.drawn_nodes().into_iter().filter(|(t, r)| t == "×" && (r.center().y - osc2.center().y).abs() < 6.0).map(|(_, r)| r).next().unwrap();
    assert!(x.right() < env.left(), "the oscillator table stays left of the envelope: {x:?} / {env:?}");
    h.click_at(x.center(), egui::PointerButton::Primary);
    let layers = |h: &Harness| -> usize { serde_json::from_value::<rawdawg_synthesizer::Patch>(h.app.project.channels[0].instrument.state.clone()).unwrap().layers.len() };
    assert_eq!(layers(&h), 1, "× removed the second oscillator");
}

#[test]
fn harmonic_edits_on_a_plain_oscillator_rebuild_the_synth_and_the_bars_follow_the_layer() {
    use rawdawg_synthesizer::sculpt;
    let mut h = Harness::new(None);
    h.app.editor_view = EditorView::Synth;
    h.frame(vec![]);
    let instance = h.app.project.channels[0].instrument.id;
    let saved = |h: &Harness| -> rawdawg_synthesizer::Patch { serde_json::from_value(h.app.project.channels[0].instrument.state.clone()).unwrap() };
    super::synth_panel::commit_patch(&mut h.app, instance, super::synth_panel::Template::SawOctaves.patch());
    h.app.synth_panel.waveform.mode = super::waveform::CanvasMode::Harmonics;
    let bars_rect = |h: &mut Harness| {
        h.frame(vec![]);
        h.ctx.read_response(super::waveform::harmonic_bars_id(instance)).map(|r| r.rect)
    };
    let shape = |h: &mut Harness, name: &str| {
        let r = h.scroll_into_view(name, |h| {
            let bars = bars_rect(h)?;
            h.drawn_nodes().into_iter().filter(|(t, r)| t == name && r.top() > bars.bottom()).map(|(_, r)| r).next()
        });
        h.click_at(r.center(), egui::PointerButton::Primary);
    };
    shape(&mut h, "Square");
    let p = saved(&h);
    let rawdawg_synthesizer::patch::Generator::Wavetable { cycle, .. } = &p.layers[0].generator else { panic!("a drawn wave now") };
    // the running synth cannot play it (its basic voices drop tables): it is rebuilt, not reused
    let fp = rawdawg_audio_engine::fingerprint_state(rawdawg_synthesizer::TYPE_ID, &h.app.project.channels[0].instrument.state);
    assert_ne!(h.app.live_fingerprints.get(&instance), Some(&fp), "the old instance is not claimed to play the new wave");
    // a bar raised from silence is a sine-phase harmonic
    let bars = h.scroll_into_view("the bars", bars_rect);
    let slot = bars.width() / h.app.synth_panel.waveform.harmonics.count as f32;
    h.click_at(egui::pos2(bars.min.x + slot * 1.5, bars.max.y - bars.height() * 0.5), egui::PointerButton::Primary);
    let wave = |h: &Harness| match &saved(h).layers[0].generator {
        rawdawg_synthesizer::patch::Generator::Wavetable { cycle, .. } => cycle.clone(),
        _ => unreachable!(),
    };
    let (levels, phases) = sculpt::harmonic_levels(&wave(&h), 4);
    assert!((levels[1] - 0.5).abs() < 0.03 && (phases[1] + std::f32::consts::FRAC_PI_2).abs() < 1e-3, "{levels:?} {phases:?}");
    let _ = cycle;
    // right-drag turns a bar off
    h.drag(egui::pos2(bars.min.x + slot * 2.5, bars.min.y + 5.0), egui::pos2(bars.min.x + slot * 2.5, bars.min.y + 8.0), egui::PointerButton::Secondary);
    let (levels, _) = sculpt::harmonic_levels(&wave(&h), 4);
    assert!(levels[2] < 1e-3, "harmonic 3 off: {levels:?}");
    // the bars describe the layer shown: Layer 2 is a plain saw an octave up
    h.app.synth_panel.waveform.layer = 1;
    h.frame(vec![]);
    let shown = h.app.synth_panel.waveform.harmonics.levels.clone();
    let (want, _) = sculpt::harmonic_levels(&rawdawg_dsp::wavetable::cycles::saw(), sculpt::MAX_HARMONICS);
    assert!(shown.iter().zip(&want).all(|(a, b)| (a - b).abs() < 1e-3), "layer 2's harmonics: {:?}", &shown[..4]);
}

#[test]
fn a_drag_abandoned_by_switching_views_is_committed_and_does_not_eat_the_next_edit() {
    use rawdawg_ui::envelope::{Adsr, AdsrPoint, curve_rect, point_pos};
    let mut h = Harness::new(None);
    h.app.editor_view = EditorView::Synth;
    h.frame(vec![]);
    let a = h.app.project.channels[0].instrument.id;
    let saved = |h: &Harness, i: usize| -> rawdawg_synthesizer::Patch { serde_json::from_value(h.app.project.channels[i].instrument.state.clone()).unwrap() };
    let rect = h.scroll_into_view("the envelope", |h| {
        h.frame(vec![]);
        h.ctx.read_response(super::synth_panel::envelope_id(a)).map(|r| r.rect)
    });
    let env = saved(&h, 0).amp_env;
    let from = point_pos(curve_rect(rect), &Adsr { attack_s: env.attack_s, decay_s: env.decay_s, sustain: env.sustain, release_s: env.release_s }, AdsrPoint::Release);
    let depth = h.app.history.undo_len();
    h.move_to(from);
    h.frame(vec![egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }]);
    h.frame(vec![egui::Event::PointerMoved(from + egui::vec2(40.0, 0.0))]);
    // F6 mid-drag: the envelope is gone when the button comes up
    h.app.editor_view = EditorView::Rack;
    h.frame(vec![egui::Event::PointerButton { pos: from + egui::vec2(40.0, 0.0), button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }]);
    h.frame(vec![]);
    assert_eq!(h.app.history.undo_len(), depth + 1, "the drag was committed");
    assert!(saved(&h, 0).amp_env.release_s > env.release_s, "with its value");
    assert_eq!(h.app.synth_mirrors[&a].patch(), &saved(&h, 0), "the project holds what the synth plays");
    // another synth's edit is saved normally
    h.app.add_synth_channel();
    h.app.editor_view = EditorView::Synth;
    h.frame(vec![]);
    let attack_slider = |h: &mut Harness| {
        let l = h.node("Attack")?;
        h.drawn_nodes().into_iter().filter(|(t, r)| t == "Attack" && r.top() > l.bottom() - 1.0).map(|(_, r)| r).next()
    };
    let s = h.scroll_into_view("B's Attack slider", attack_slider);
    let before = saved(&h, 1).amp_env.attack_s;
    h.drag(s.center(), s.center() + egui::vec2(30.0, 0.0), egui::PointerButton::Primary);
    assert_ne!(saved(&h, 1).amp_env.attack_s, before, "B's edit reached the project");
}

#[test]
fn the_on_screen_piano_lights_and_names_what_the_mouse_plays() {
    let mut h = Harness::new(None);
    h.app.editor_view = EditorView::Synth;
    h.frame(vec![]);
    let hint = h.scroll_into_view("the on-screen piano", |h| h.node("Click or drag to play"));
    // the keys are just above their hint row
    let key = egui::pos2(hint.left() + 30.0, hint.top() - 20.0);
    h.move_to(key);
    h.frame(vec![egui::Event::PointerButton { pos: key, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }]);
    h.frame(vec![]);
    let held = h.app.held_notes.pitches();
    assert_eq!(held.len(), 1, "the mouse holds one key");
    let name = super::held::describe(&held);
    let toolbar: Vec<String> = h.drawn_nodes().into_iter().filter(|(_, r)| r.center().y < 100.0).map(|(t, _)| t).collect();
    assert!(toolbar.contains(&name), "{name} in {toolbar:?}");
    assert!(filled_rects(&h.shapes()).iter().any(|(c, r)| *c == theme::KEY_HELD && r.contains(key)), "the key is lit");
    h.frame(vec![egui::Event::PointerButton { pos: key, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }]);
    h.frame(vec![]);
    assert!(h.app.held_notes.pitches().is_empty());
}

#[test]
fn the_running_status_puts_voices_first_and_only_shows_counters_that_matter() {
    let plain = super::running_status(3, 0.0, 0, 0, None);
    assert_eq!(plain.iter().map(|p| p.0.as_str()).collect::<Vec<_>>(), ["voices 3"]);
    let busy = super::running_status(12, 2.5, 4, 2, Some("plan error"));
    assert_eq!(busy.iter().map(|p| p.0.as_str()).collect::<Vec<_>>(), ["voices 12", "limiter −2.5 dB", "dropped 4", "late MIDI 2", "plan error"]);
    assert_eq!(busy[2].1, theme::ERROR);
}

/// A saved project folder `root/<name>.rawdawg` with `notes` notes, at `revision`.
fn saved_song(root: &std::path::Path, name: &str, notes: usize, revision: u64) -> PathBuf {
    let dir = root.join(format!("{name}.rawdawg"));
    let mut p = rawdawg_project::Project::new_default(name, serde_json::to_value(rawdawg_synthesizer::Patch::default()).unwrap());
    p.revision = revision;
    p.patterns[0].lanes[0].notes = (0..notes)
        .map(|i| rawdawg_project::Note { id: rawdawg_types::NoteId::new(), tick: Tick(i as i64 * 240), duration_ticks: Tick(120), pitch: 60, velocity: 0.8, channel: 0 })
        .collect();
    rawdawg_project::save_project_dir(&dir, &p).unwrap();
    dir
}

fn add_note(h: &mut Harness, tick: i64) {
    let (pid, cid) = (h.app.project.patterns[0].id, h.app.project.channels[0].id);
    h.app.apply(Command::AddNotes {
        pattern_id: pid,
        channel_id: cid,
        notes: vec![rawdawg_project::Note { id: rawdawg_types::NoteId::new(), tick: Tick(tick), duration_ticks: Tick(120), pitch: 64, velocity: 0.8, channel: 0 }],
    });
}

fn note_count(h: &Harness) -> usize {
    h.app.project.patterns[0].lanes[0].notes.len()
}

fn lock_file(dir: &std::path::Path) -> PathBuf {
    dir.join("recovery/lock.json")
}

#[test]
fn opening_another_project_asks_about_unsaved_changes_and_moves_the_lock() {
    let root = std::env::temp_dir().join(format!("rawdawg-ui-open-prompt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let a = saved_song(&root, "A", 1, 3);
    let b = saved_song(&root, "B", 2, 3);
    let mut h = Harness::new(None);
    h.app.prefs.recent_projects = vec![a.clone(), b.clone()];
    h.app.request_open(a.clone());
    assert_eq!(h.app.project_path.as_deref(), Some(a.as_path()));
    assert!(lock_file(&a).is_file(), "A is locked");
    add_note(&mut h, 960);
    // Browser › Projects › B: asked first, nothing lost yet
    let click = |h: &mut Harness, text: &str| {
        h.frame(vec![]);
        h.frame(vec![]);
        let r = h.node(text).unwrap_or_else(|| panic!("{text:?} not drawn"));
        h.click_at(r.center(), egui::PointerButton::Primary);
    };
    click(&mut h, "B.rawdawg");
    h.frame(vec![]);
    assert!(h.drawn_texts().iter().any(|t| t == "Save the current project before opening B.rawdawg?"));
    assert_eq!(h.app.project_path.as_deref(), Some(a.as_path()));
    click(&mut h, "Cancel");
    assert_eq!((h.app.project_path.as_deref(), note_count(&h)), (Some(a.as_path()), 2), "cancelled: A with its edit");
    assert!(h.app.history.is_dirty() && h.app.history.can_undo());
    // again, discarding: B opens, locked; A's lock is released
    click(&mut h, "B.rawdawg");
    click(&mut h, "Discard and open");
    assert_eq!((h.app.project_path.as_deref(), note_count(&h)), (Some(b.as_path()), 2));
    assert!(lock_file(&b).is_file(), "the project opened from the list is locked");
    assert!(!lock_file(&a).exists(), "the one left is not");
    assert_eq!(rawdawg_project::load_project_dir(&a).unwrap().patterns[0].lanes[0].notes.len(), 1, "A as saved");
    // File › Open › Recent with an edit in B: Save and open keeps it
    add_note(&mut h, 1920);
    h.app.files.dialog = Some(super::project_files::FileDialog::Open);
    h.frame(vec![]);
    h.frame(vec![]);
    let recent = h.drawn_nodes().into_iter().find(|(t, _)| *t == a.display().to_string()).map(|(_, r)| r).expect("the Recent entry");
    h.click_at(recent.center(), egui::PointerButton::Primary);
    click(&mut h, "Save and open");
    assert_eq!(h.app.project_path.as_deref(), Some(a.as_path()));
    assert_eq!(rawdawg_project::load_project_dir(&b).unwrap().patterns[0].lanes[0].notes.len(), 3, "B was saved with its edit");
    assert!(lock_file(&a).is_file() && !lock_file(&b).exists());
    // a project that is not dirty opens at once
    h.app.request_open(b.clone());
    assert_eq!(h.app.project_path.as_deref(), Some(b.as_path()));
    assert!(h.app.files.open_prompt.is_none());
    // a never-saved project: Save and open goes through Save As, then opens
    h.app.new_project();
    add_note(&mut h, 0);
    h.app.request_open(a.clone());
    click(&mut h, "Save and open");
    assert_eq!(h.app.files.dialog, Some(super::project_files::FileDialog::SaveAs));
    h.app.files.path_input = root.join("Fresh").display().to_string();
    click(&mut h, "Save as");
    assert!(root.join("Fresh.rawdawg/project.json").is_file());
    assert_eq!(h.app.project_path.as_deref(), Some(a.as_path()), "then the project asked for opened");
    let _ = std::fs::remove_dir_all(&root);
}

fn autosave_count(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir.join("recovery/autosave")).map_or(0, |r| r.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "json")).count())
}

/// Another rawDAWg (on another machine) holds `dir`.
fn foreign_lock(dir: &std::path::Path) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    let live = rawdawg_project::lock::LockInfo { pid: 1, host: "elsewhere".into(), acquired_unix_ms: now };
    std::fs::create_dir_all(dir.join("recovery")).unwrap();
    std::fs::write(lock_file(dir), serde_json::to_vec(&live).unwrap()).unwrap();
}

/// Crash autosave of a saved song: `notes` more notes than it was saved with.
fn crash_autosave(dir: &std::path::Path, notes: usize) {
    let mut h = Harness::new(None);
    assert!(h.app.open_project_checked(dir.to_path_buf()));
    let before = note_count(&h);
    for t in 0..notes {
        add_note(&mut h, 1000 + t as i64 * 120);
    }
    h.app.autosave_now(dir);
    assert_eq!(note_count(&h), before + notes, "the notes fit the pattern");
    crash(h);
}

#[test]
fn kept_autosaves_survive_exit_new_and_save_and_only_this_sessions_autosaves_go() {
    let root = std::env::temp_dir().join(format!("rawdawg-ui-keep-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = saved_song(&root, "K", 1, 5);
    crash_autosave(&dir, 2);
    assert_eq!(autosave_count(&dir), 1);
    // Keep saved version, then a clean exit: the kept autosave is still there and offered again
    let mut h = Harness::new(None);
    h.app.open_project_checked(dir.clone());
    assert_eq!(h.app.files.recovery.len(), 1);
    click(&mut h, "Keep saved version (keep autosaves)");
    assert!(h.app.files.recovery.is_empty());
    eframe::App::on_exit(&mut h.app);
    drop(h);
    assert_eq!(autosave_count(&dir), 1, "kept means kept");
    let mut h = Harness::new(None);
    h.app.open_project_checked(dir.clone());
    assert_eq!(h.app.files.recovery.len(), 1, "offered again");
    click(&mut h, "Keep saved version (keep autosaves)");
    // this session's own autosave goes with the save; the kept one stays
    add_note(&mut h, 0);
    h.app.autosave_now(&dir);
    assert_eq!(autosave_count(&dir), 2);
    h.app.save_project_checked();
    assert_eq!(autosave_count(&dir), 1, "only this session's autosave is removed");
    // the save is newer than the kept autosave: not offered as a crash any more
    drop(h);
    let mut h = Harness::new(None);
    h.app.open_project_checked(dir.clone());
    assert!(h.app.files.recovery.is_empty(), "{:?}", h.app.files.recovery);
    // New project and a discarded close leave it alone too
    add_note(&mut h, 0);
    h.app.autosave_now(&dir);
    h.app.new_project();
    assert_eq!(autosave_count(&dir), 1, "this session's autosave went, the kept one stayed");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_copy_open_read_only_neither_autosaves_nor_takes_the_other_ones_autosaves() {
    let root = std::env::temp_dir().join(format!("rawdawg-ui-readonly-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = saved_song(&root, "R", 1, 5);
    crash_autosave(&dir, 3); // the running one's autosave, as far as this window can tell
    foreign_lock(&dir);
    let mut h = Harness::new(None);
    assert!(h.app.open_project_checked(dir.clone()));
    assert!(h.app.files.read_only.is_some());
    assert!(h.app.files.recovery.is_empty(), "the running copy's autosaves are not a crash");
    add_note(&mut h, 0);
    h.app.files.last_autosave = None;
    h.frame(vec![]);
    assert_eq!(autosave_count(&dir), 1, "no autosave into the other copy's folder");
    h.app.new_project();
    eframe::App::on_exit(&mut h.app);
    assert_eq!(autosave_count(&dir), 1, "and nothing of it deleted");
    // taking the lock over (the other one crashed elsewhere): its autosave is offered
    let mut h = Harness::new(None);
    h.app.open_project_checked(dir.clone());
    h.app.take_over_lock();
    assert!(h.app.files.read_only.is_none());
    assert_eq!(h.app.files.recovery.len(), 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn save_as_a_failed_discard_and_open_and_a_failed_reopen_keep_what_they_must() {
    let root = std::env::temp_dir().join(format!("rawdawg-ui-session-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let a = saved_song(&root, "A", 1, 5);
    // Save As leaves no autosave of this session in the old folder
    let mut h = Harness::new(None);
    h.app.open_project_checked(a.clone());
    add_note(&mut h, 0);
    h.app.autosave_now(&a);
    assert_eq!(autosave_count(&a), 1);
    h.app.save_project_as(root.join("C"));
    assert_eq!(autosave_count(&a), 0, "the work was saved (elsewhere)");
    drop(h);
    // Discard and open a project that fails to load: nothing is discarded
    let bad = root.join("Bad.rawdawg");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("project.json"), b"{ broken").unwrap();
    let mut h = Harness::new(None);
    h.app.open_project_checked(a.clone());
    add_note(&mut h, 0);
    h.app.autosave_now(&a);
    h.app.request_open(bad.clone());
    click(&mut h, "Discard and open");
    assert_eq!(h.app.project_path.as_ref(), Some(&a));
    assert_eq!(note_count(&h), 2, "the edit is still here");
    assert_eq!(autosave_count(&a), 1, "and its autosave");
    assert!(h.app.files.open_prompt.is_some(), "the prompt stays");
    h.frame(vec![]);
    assert!(h.drawn_texts().iter().any(|t| t.starts_with("Could not open")), "and says why");
    click(&mut h, "Cancel");
    // reopening this project when its file is damaged keeps our lock on it
    let ours = rawdawg_project::lock::ProjectLock::read(&a).expect("locked");
    let manifest = std::fs::read(a.join("project.json")).unwrap();
    std::fs::write(a.join("project.json"), b"{ broken").unwrap();
    assert!(!h.app.open_project_checked(a.clone()));
    assert_eq!(rawdawg_project::lock::ProjectLock::read(&a), Some(ours), "still held by this window");
    std::fs::write(a.join("project.json"), manifest).unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn recovering_asks_before_replacing_edits_made_while_the_offer_was_open() {
    let root = std::env::temp_dir().join(format!("rawdawg-ui-confirm-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = saved_song(&root, "S", 1, 5);
    crash_autosave(&dir, 3);
    let mut h = Harness::new(None);
    h.app.open_project_checked(dir.clone());
    assert_eq!(h.app.files.recovery.len(), 1);
    add_note(&mut h, 0); // an edit while the offer is up
    click(&mut h, "Recover");
    assert_eq!(note_count(&h), 2, "not replaced yet");
    h.frame(vec![]);
    assert!(h.drawn_texts().iter().any(|t| t.contains("recovering replaces them")));
    click(&mut h, "Replace my changes and recover");
    assert_eq!(note_count(&h), 4, "the autosave's 1 + 3 notes");
    assert!(h.app.files.recovery.is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn opening_another_project_while_listening_to_an_autosave_plays_the_opened_one() {
    let root = std::env::temp_dir().join(format!("rawdawg-ui-listen-open-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = saved_song(&root, "L", 1, 5);
    let other = saved_song(&root, "O", 6, 5);
    crash_autosave(&dir, 3);
    let mut h = Harness::new(None);
    h.app.open_project_checked(dir.clone());
    h.app.autosave_now(&root.join("L.rawdawg"));
    assert!(h.app.files.last_autosave_note.is_some());
    click(&mut h, "Compare");
    click(&mut h, "Autosave");
    let notes = |p: &rawdawg_project::Project| p.patterns[0].lanes.iter().map(|l| l.notes.len()).sum::<usize>();
    assert_eq!(notes(&h.app.engine_project()), 4);
    assert!(h.app.open_project_checked(other.clone()));
    assert!(h.app.files.compare.is_none());
    assert_eq!(notes(&h.app.engine_project()), 6, "the opened project plays");
    assert!(h.app.files.last_autosave_note.is_none(), "the File menu does not show the other project's autosave");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn starting_on_the_marker_file_opens_the_folder_with_its_lock() {
    let root = std::env::temp_dir().join(format!("rawdawg-ui-marker-start-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = crate::prefs::absolute(&saved_song(&root, "M", 2, 5));
    let marker = dir.join(rawdawg_project::persist::PROJECT_MARKER);
    assert!(marker.is_file());
    let h = Harness::with_recovery_dir(Some(marker.to_str().unwrap()), None, root.join("unsaved"));
    assert_eq!(h.app.project_path.as_ref(), Some(&dir), "the folder, not the marker");
    assert!(h.app.files.read_only.is_none(), "{:?}", h.app.files.read_only);
    assert!(lock_file(&dir).is_file());
    assert_eq!(h.app.prefs.recent_projects.first(), Some(&dir));
    drop(h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn never_saved_projects_are_offered_after_the_opened_projects_own_offer() {
    let root = std::env::temp_dir().join(format!("rawdawg-ui-both-offers-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let recovery = root.join("unsaved");
    let dir = saved_song(&root, "B", 1, 5);
    crash_autosave(&dir, 2);
    let mut h = Harness::with_recovery_dir(None, None, recovery.clone());
    add_note(&mut h, 0);
    h.frame(vec![]);
    crash(h);
    let mut h = Harness::with_recovery_dir(Some(dir.to_str().unwrap()), None, recovery.clone());
    assert!(!h.app.files.recovery_unsaved && h.app.files.recovery.len() == 1, "the opened project's offer first");
    click(&mut h, "Keep saved version (keep autosaves)");
    h.frame(vec![]);
    assert!(h.app.files.recovery_unsaved && h.app.files.recovery.len() == 1, "then the never-saved one");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn preset_and_screenshot_runs_never_autosave_or_offer_recovery() {
    let root = std::env::temp_dir().join(format!("rawdawg-ui-preset-runs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let recovery = root.join("unsaved");
    let mut h = Harness::with_recovery_dir(None, None, recovery.clone());
    add_note(&mut h, 0);
    h.frame(vec![]);
    crash(h);
    let mut h = Harness::with_recovery_dir(None, Some("rack"), recovery.clone());
    assert!(h.app.files.recovery.is_empty(), "no offer in a preset run");
    add_note(&mut h, 0);
    h.frame(vec![]);
    assert!(h.app.unsaved_dir().is_none_or(|d| !d.exists()), "no autosave either");
    drop(h);
    let h = Harness::with_recovery_dir(None, None, recovery.clone());
    assert_eq!(h.app.files.recovery.len(), 1, "the crash is still there for a normal start");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn save_and_open_or_close_on_a_read_only_project_keeps_the_prompt_and_says_why() {
    let root = std::env::temp_dir().join(format!("rawdawg-ui-readonly-prompt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let a = saved_song(&root, "A", 1, 5);
    let b = saved_song(&root, "B", 1, 5);
    foreign_lock(&a);
    let mut h = Harness::new(None);
    h.app.open_project_checked(a.clone());
    add_note(&mut h, 0);
    h.app.request_open(b.clone());
    click(&mut h, "Save and open");
    assert!(h.app.files.open_prompt.is_some(), "the prompt stays");
    assert_eq!(h.app.project_path.as_ref(), Some(&a));
    h.frame(vec![]);
    assert!(h.drawn_texts().iter().any(|t| t.starts_with("Read-only")), "{:?}", h.app.files.prompt_note);
    click(&mut h, "Cancel");
    assert!(h.app.files.prompt_note.is_none() || h.app.files.open_prompt.is_none());
    h.app.files.close_prompt = true;
    click(&mut h, "Save and close");
    assert!(h.app.files.close_prompt && !h.app.files.closing, "not closed: nothing was saved");
    h.frame(vec![]);
    assert!(h.drawn_texts().iter().any(|t| t.starts_with("Read-only")));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_crash_in_a_reopened_project_is_offered_for_recovery() {
    let root = std::env::temp_dir().join(format!("rawdawg-ui-reopen-crash-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    // saved after 25 edits in an earlier session
    let dir = saved_song(&root, "Rand", 1, 25);
    let mut h = Harness::new(None);
    h.app.open_project_checked(dir.clone());
    assert_eq!(h.app.history.revision(), 25, "this session's edits number after the saved ones");
    assert!(!h.app.history.is_dirty());
    for t in 1..=3 {
        add_note(&mut h, t * 240);
    }
    h.app.autosave_now(&dir);
    drop(h); // the crash (no save, no clean exit)
    let mut h = Harness::new(None);
    h.app.open_project_checked(dir.clone());
    assert_eq!(h.app.files.recovery.len(), 1, "the autosave with 3 more notes is offered");
    h.frame(vec![]);
    assert!(h.drawn_texts().iter().any(|t| t == "Recover"));
    let c = h.app.files.recovery[0].clone();
    h.app.recover_from(&c);
    assert_eq!(note_count(&h), 4);
    assert!(h.app.history.revision() >= 28 && h.app.history.is_dirty(), "work after recovery is numbered above it too");
    let _ = std::fs::remove_dir_all(&root);
}

/// A crash: the process ends without running any destructor, so its lock stays behind.
fn crash(mut h: Harness) {
    std::mem::forget(h.app.files.lock.take());
    drop(h);
}

#[test]
fn a_crash_in_a_never_saved_project_is_offered_at_the_next_start() {
    let base = std::env::temp_dir().join(format!("rawdawg-ui-unsaved-crash-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let recovery = base.join("unsaved");
    // a new project, a few edits, autosaved into the per-user folder
    let mut h = Harness::with_recovery_dir(None, None, recovery.clone());
    assert!(h.app.files.recovery.is_empty(), "nothing to offer on a first start");
    for t in 0..4 {
        add_note(&mut h, t * 240);
    }
    h.frame(vec![]);
    let own = h.app.unsaved_dir().unwrap();
    assert!(own.starts_with(&recovery));
    assert_eq!(std::fs::read_dir(own.join("recovery/autosave")).unwrap().count(), 1, "a never-saved project autosaves");
    assert!(lock_file(&own).is_file(), "and holds its folder");
    crash(h);
    // the next start offers it, named with what it holds
    let mut h = Harness::with_recovery_dir(None, None, recovery.clone());
    assert!(h.app.files.recovery_unsaved && h.app.files.recovery.len() == 1);
    h.frame(vec![]);
    let texts = h.drawn_texts();
    assert!(texts.iter().any(|t| t.starts_with("rawDAWg did not close normally: a project was never saved")), "{texts:?}");
    assert!(texts.iter().any(|t| t.contains("1 channel, 4 notes, 0 clips")), "{texts:?}");
    assert!(h.node("Compare").is_none(), "nothing to compare a never-saved project with");
    let recover = h.node("Recover").unwrap();
    h.click_at(recover.center(), egui::PointerButton::Primary);
    assert_eq!(note_count(&h), 4, "the four notes are back");
    assert!(h.app.is_never_saved() && h.app.history.is_dirty());
    // Save asks where (an empty field); Save As keeps it, and the per-user folder is done with
    h.app.files.path_input = "/somewhere/typed/for/another/dialog".into();
    h.app.save_project_checked();
    assert_eq!(h.app.files.dialog, Some(super::project_files::FileDialog::SaveAs), "never into the hidden folder");
    assert!(h.app.files.path_input.is_empty(), "no stale path proposed");
    h.app.save_project_as(base.join("Kept"));
    assert!(!h.app.history.is_dirty() && base.join("Kept.rawdawg/project.json").is_file());
    assert!(!own.exists(), "the per-user folder is removed once the project is saved elsewhere");
    drop(h);
    let h = Harness::with_recovery_dir(None, None, recovery.clone());
    assert!(h.app.files.recovery.is_empty(), "nothing left to offer");
    drop(h);
    // work given up on purpose is not offered: New project, Discard and close, a clean exit
    let mut h = Harness::with_recovery_dir(None, None, recovery.clone());
    add_note(&mut h, 0);
    h.frame(vec![]);
    let discarded = h.app.unsaved_dir().unwrap();
    assert!(discarded.join("recovery/autosave").is_dir());
    h.app.new_project();
    assert!(!discarded.exists(), "New project removes the discarded work's folder");
    crash(h);
    let mut h = Harness::with_recovery_dir(None, None, recovery.clone());
    assert!(h.app.files.recovery.is_empty(), "New project discarded it");
    add_note(&mut h, 0);
    h.frame(vec![]);
    let closed = h.app.unsaved_dir().unwrap();
    h.app.history.mark_saved();
    eframe::App::on_exit(&mut h.app);
    assert!(!closed.exists(), "a clean exit removes it");
    drop(h);
    let h = Harness::with_recovery_dir(None, None, recovery.clone());
    assert!(h.app.files.recovery.is_empty(), "a clean exit leaves nothing to recover");
    drop(h);
    // another running rawDAWg's never-saved project is not taken for a crash
    let mut h = Harness::with_recovery_dir(None, None, recovery.clone());
    add_note(&mut h, 0);
    h.frame(vec![]);
    let other = h.app.unsaved_dir().unwrap();
    crash(h);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    let live = rawdawg_project::lock::LockInfo { pid: 1, host: "elsewhere".into(), acquired_unix_ms: now };
    std::fs::write(lock_file(&other), serde_json::to_vec(&live).unwrap()).unwrap();
    let h = Harness::with_recovery_dir(None, None, recovery.clone());
    assert!(h.app.files.recovery.is_empty(), "held by a live rawDAWg");
    drop(h);
    // a folder without a lock was closed by an older version: not a crash either
    std::fs::remove_file(lock_file(&other)).unwrap();
    let h = Harness::with_recovery_dir(None, None, recovery.clone());
    assert!(h.app.files.recovery.is_empty(), "no lock left behind, no crash");
    drop(h);
    // once that one is gone (its lock left behind by a process that ended), its work is
    // offered; Discard removes it for good
    let dead = rawdawg_project::lock::LockInfo { pid: std::process::id(), host: live_host(), acquired_unix_ms: now };
    std::fs::write(lock_file(&other), serde_json::to_vec(&dead).unwrap()).unwrap();
    let mut h = Harness::with_recovery_dir(None, None, recovery.clone());
    assert_eq!(h.app.files.recovery.len(), 1);
    h.frame(vec![]);
    let discard = h.node("Discard this project").unwrap();
    h.click_at(discard.center(), egui::PointerButton::Primary);
    assert!(!other.exists(), "its folder is gone");
    drop(h);
    let h = Harness::with_recovery_dir(None, None, recovery.clone());
    assert!(h.app.files.recovery.is_empty());
    let _ = std::fs::remove_dir_all(&base);
}

/// Draw two frames, then click the widget labelled `label`.
fn click(h: &mut Harness, label: &str) {
    h.frame(vec![]);
    h.frame(vec![]);
    let r = h.drawn_nodes().into_iter().find(|(t, _)| t == label).map(|(_, r)| r).unwrap_or_else(|| panic!("{label:?} not drawn"));
    h.click_at(r.center(), egui::PointerButton::Primary);
}

/// This machine's name as the lock records it.
fn live_host() -> String {
    std::env::var("HOSTNAME").or_else(|_| std::env::var("COMPUTERNAME")).unwrap_or_else(|_| "local".into())
}

#[test]
fn two_crashed_never_saved_projects_are_offered_apart_and_handled_one_by_one() {
    let base = std::env::temp_dir().join(format!("rawdawg-ui-unsaved-two-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let recovery = base.join("unsaved");
    let mut dirs = Vec::new();
    for notes in [2, 5] {
        let mut h = Harness::with_recovery_dir(None, None, recovery.clone());
        for t in 0..notes {
            add_note(&mut h, t * 240);
        }
        h.frame(vec![]);
        dirs.push(h.app.unsaved_dir().unwrap());
        crash(h);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // a third crashed before its first autosave, after an import wrote its working manifest
    let mut h = Harness::with_recovery_dir(None, None, recovery.clone());
    for t in 0..3 {
        add_note(&mut h, t * 240);
    }
    h.app.autosave_enabled = false;
    let third = h.app.working_dir().unwrap();
    assert!(third.join("project.json").is_file() && !third.join("recovery/autosave").exists());
    crash(h);
    let mut h = Harness::with_recovery_dir(None, None, recovery.clone());
    assert_eq!(h.app.files.recovery.len(), 3, "one line per project");
    assert_eq!(h.app.files.recovery[0].dir, third, "newest first");
    assert!(
        h.app.files.recovery_labels[0].contains("3 notes") && h.app.files.recovery_labels[1].contains("5 notes") && h.app.files.recovery_labels[2].contains("2 notes"),
        "{:?}",
        h.app.files.recovery_labels
    );
    h.frame(vec![]);
    // discard the chosen one only (the 5-note project)
    h.app.files.recovery_selected = 1;
    h.frame(vec![]);
    click(&mut h, "Discard this project");
    assert!(!dirs[1].exists() && dirs[0].exists() && third.exists());
    assert_eq!(h.app.files.recovery.len(), 2, "the others are still offered");
    // another window recovers the 2-note one first: this one refuses rather than share it
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    let taken = rawdawg_project::lock::LockInfo { pid: 1, host: "elsewhere".into(), acquired_unix_ms: now };
    std::fs::write(lock_file(&dirs[0]), serde_json::to_vec(&taken).unwrap()).unwrap();
    h.app.files.recovery_selected = 1;
    h.frame(vec![]);
    click(&mut h, "Recover");
    assert_eq!(note_count(&h), 0, "not recovered");
    assert!(h.app.status_line.contains("recovered in another rawDAWg window"), "{}", h.app.status_line);
    assert_eq!(h.app.files.recovery.len(), 1);
    // the manifest-only crash is recovered from its working manifest
    h.frame(vec![]);
    click(&mut h, "Recover");
    assert_eq!(note_count(&h), 3);
    assert_eq!(h.app.project_path.as_ref(), Some(&third));
    assert!(h.app.files.recovery.is_empty());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn splitting_audio_and_automation_clips_changes_nothing_you_hear() {
    use rawdawg_audio_engine::offline::{OfflineOptions, render_project};
    let fixture = PathBuf::from("../../fixtures/demo-song.rawdawg");
    let mut h = Harness::new(Some("../../fixtures/demo-song.rawdawg"));
    h.frame(vec![]);
    let render = |p: &rawdawg_project::Project| {
        let opts = OfflineOptions { mode: TransportMode::Song, tail_seconds: 0.0, project_dir: Some(fixture.clone()), ..OfflineOptions::default() };
        render_project(p, &opts).expect("render").0
    };
    let before = render(&h.app.project);
    let audio = h.app.project.clips.iter().find(|c| matches!(c.content, rawdawg_project::ClipContent::Audio { .. })).unwrap().clone();
    let auto = h.app.project.clips.iter().find(|c| matches!(c.content, rawdawg_project::ClipContent::Automation { .. })).unwrap().clone();
    let surface = h.rect(egui::Id::unique("playlist-surface"));
    let row = |h: &Harness, track| h.app.project.tracks.iter().position(|t| t.id == track).unwrap();
    // right-click › Split here, on the sample at beat 2 (0.5 s) and on the ramp at bar 4
    let split_here = |h: &mut Harness, tick: i64, row: usize| {
        let p = super::playlist::lane_point(&h.app.playlist, surface, tick, row);
        h.click_at(p, egui::PointerButton::Secondary);
        h.frame(vec![]);
        let entry = h.drawn_nodes().into_iter().find(|(t, _)| t.starts_with("Split here")).map(|(_, r)| r).expect("Split here");
        h.click_at(entry.center(), egui::PointerButton::Primary);
    };
    let audio_row = row(&h, audio.track_id);
    split_here(&mut h, 960, audio_row);
    let halves: Vec<rawdawg_project::Clip> = h.app.project.clips.iter().filter(|c| c.track_id == audio.track_id).cloned().collect();
    assert_eq!(halves.len(), 2, "split in two");
    let bounds = |c: &rawdawg_project::Clip| match &c.content {
        rawdawg_project::ClipContent::Audio { source_offset_frames, source_length_frames, fade_in_frames, fade_out_frames, .. } => {
            (*source_offset_frames, *source_length_frames, *fade_in_frames, *fade_out_frames)
        }
        _ => unreachable!(),
    };
    let (l, r) = if halves[0].start_tick.0 == 0 { (&halves[0], &halves[1]) } else { (&halves[1], &halves[0]) };
    assert_eq!(bounds(l), (0, 24_000, 480, 0), "the left half's sample ends at the split, it keeps the fade-in");
    assert_eq!(bounds(r), (24_000, 24_000, 0, 4_800), "the right half continues from there with the fade-out");
    let auto_row = row(&h, auto.track_id);
    split_here(&mut h, 3 * 3840, auto_row);
    let ramps: Vec<i64> = h
        .app
        .project
        .clips
        .iter()
        .filter_map(|c| match &c.content {
            rawdawg_project::ClipContent::Automation { source_offset_ticks, .. } => Some(source_offset_ticks.0),
            _ => None,
        })
        .collect();
    assert_eq!(ramps.len(), 2);
    assert!(ramps.contains(&0) && ramps.contains(&3840), "the second half of the ramp continues it: {ramps:?}");
    // the song renders the same as before the splits
    let after = render(&h.app.project);
    assert_eq!(before.len(), after.len());
    let worst = before.iter().zip(&after).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(worst < 1e-4, "splits are inaudible: largest difference {worst}");
    // a split after the sample has ended (the clip runs on in silence) only trims the clip
    h.app.undo();
    h.app.undo();
    let clips = h.app.project.clips.len();
    split_here(&mut h, 2400, audio_row);
    assert_eq!(h.app.project.clips.len(), clips, "nothing to the right but silence: no new clip");
    let trimmed = h.app.project.clips.iter().find(|c| c.id == audio.id).unwrap();
    assert_eq!(bounds(trimmed), (0, 48_000, 480, 4_800), "the whole sample, once");
    assert_eq!(render(&h.app.project), before, "still the same sound");
}

#[test]
fn splitting_a_44k_sample_inside_its_fades_changes_nothing_you_hear() {
    use rawdawg_audio_engine::offline::{OfflineOptions, render_project};
    use rawdawg_project::{ClipContent, Command};
    let root = std::env::temp_dir().join(format!("rawdawg-ui-split-fades-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    // one second of 220 Hz at 44.1 kHz, in a 48 kHz project
    let wav = root.join("tone44k.wav");
    {
        let spec = hound::WavSpec { channels: 1, sample_rate: 44_100, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(&wav, spec).unwrap();
        for i in 0..44_100 {
            w.write_sample(((i as f32 * 220.0 * std::f32::consts::TAU / 44_100.0).sin() * 16_000.0) as i16).unwrap();
        }
        w.finalize().unwrap();
    }
    let mut h = Harness::new(None);
    assert_eq!(h.app.project.sample_rate_hz.0, 48_000);
    let id = rawdawg_types::AssetId::from_hex(&h.app.import_asset_only(&wav).unwrap()).unwrap();
    let track = rawdawg_project::Track { id: rawdawg_types::TrackId::new(), name: "Audio".into(), color: None, mute: false, solo: false, height: None, mixer_id: None };
    let tid = track.id;
    h.app.apply(Command::AddTrack { track, index: None });
    let mut clip = super::playlist::audio_clip_for(&h.app, &id, tid, 0).expect("measured");
    assert_eq!(clip.length_ticks, Tick(1920), "one second is two beats at 120 BPM, read at the file's own rate");
    // 0.4 s fades in and out (source frames)
    if let ClipContent::Audio { fade_in_frames, fade_out_frames, .. } = &mut clip.content {
        (*fade_in_frames, *fade_out_frames) = (17_640, 17_640);
    }
    let cid = clip.id;
    h.app.apply(Command::AddClip { clip });
    let render = |h: &Harness| {
        let opts =
            OfflineOptions { mode: TransportMode::Song, tail_seconds: 0.0, end_tick: Some(Tick(1920)), project_dir: h.app.project_path.clone(), ..OfflineOptions::default() };
        render_project(&h.app.project, &opts).expect("render").0
    };
    let before = render(&h);
    assert!(rawdawg_dsp::math::rms(&before) > 0.05, "the tone plays");
    // split at 0.2 s (inside the fade-in), then the right part at 0.8 s (inside the fade-out)
    let split = |h: &mut Harness, id: rawdawg_types::ClipId, at: i64| {
        let c = h.app.project.clips.iter().find(|c| c.id == id).unwrap().clone();
        let mut cmds = Vec::new();
        super::playlist::split_clips_at(&mut h.app, &mut cmds, &[c], at);
        for cmd in cmds {
            h.app.apply(cmd);
        }
    };
    split(&mut h, cid, 384);
    let right = h.app.project.clips.iter().find(|c| c.track_id == tid && c.start_tick == Tick(384)).unwrap().id;
    split(&mut h, right, 1536);
    let parts: Vec<(i64, u64, u64, u64, u64, u64, u64)> = {
        let mut v: Vec<_> = h
            .app
            .project
            .clips
            .iter()
            .filter(|c| c.track_id == tid)
            .map(|c| match &c.content {
                ClipContent::Audio { source_offset_frames, source_length_frames, fade_in_frames, fade_in_offset_frames, fade_out_frames, fade_out_offset_frames, .. } => {
                    (c.start_tick.0, *source_offset_frames, *source_length_frames, *fade_in_frames, *fade_in_offset_frames, *fade_out_frames, *fade_out_offset_frames)
                }
                _ => unreachable!(),
            })
            .collect();
        v.sort();
        v
    };
    assert_eq!(
        parts,
        vec![
            (0, 0, 8_820, 17_640, 0, 0, 0),                     // the fade-in begins; it never reaches the fade-out
            (384, 8_820, 26_460, 17_640, 8_820, 17_640, 8_820), // continues the fade-in, reaches the fade-out's start
            (1536, 35_280, 8_820, 0, 0, 17_640, 0),             // the fade-in is over; the fade-out's last 0.2 s
        ],
        "cut at the file's own rate (44.1 kHz), each part playing its piece of the ramps"
    );
    let after = render(&h);
    assert_eq!(before.len(), after.len());
    let worst = before.iter().zip(&after).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(worst < 1e-3, "splits inside fades are inaudible: largest difference {worst}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn trimming_the_left_edge_stops_at_the_files_start_and_keeps_automation_in_place() {
    use rawdawg_project::{ClipContent, Command};
    let mut h = Harness::new(Some("../../fixtures/demo-song.rawdawg"));
    h.frame(vec![]);
    let audio = h.app.project.clips.iter().find(|c| matches!(c.content, ClipContent::Audio { .. })).unwrap().clone();
    let auto = h.app.project.clips.iter().find(|c| matches!(c.content, ClipContent::Automation { .. })).unwrap().clone();
    // move both clips a bar right, so their left edges have room to be dragged left
    for c in [&audio, &auto] {
        h.app.apply(Command::UpdateClip { clip: rawdawg_project::Clip { start_tick: Tick(c.start_tick.0 + 3840), ..c.clone() } });
    }
    let surface = h.rect(egui::Id::unique("playlist-surface"));
    let row = |h: &Harness, track| h.app.project.tracks.iter().position(|t| t.id == track).unwrap();
    let clip = |h: &Harness, id| h.app.project.clips.iter().find(|c| c.id == id).unwrap().clone();
    // the audio clip's left edge, dragged a whole bar left: it stops where the file starts
    let (start, arow) = (clip(&h, audio.id).start_tick.0, row(&h, audio.track_id));
    let from = super::playlist::lane_point(&h.app.playlist, surface, start, arow) + egui::vec2(2.0, 0.0);
    let to = super::playlist::lane_point(&h.app.playlist, surface, start - 3840, arow) + egui::vec2(2.0, 0.0);
    h.drag(from, to, egui::PointerButton::Primary);
    let a = clip(&h, audio.id);
    assert_eq!(a.start_tick.0, start, "the file starts there: the edge goes no further (the audio does not slide)");
    assert!(matches!(a.content, ClipContent::Audio { source_offset_frames: 0, .. }));
    // trimming an automation clip's start keeps its curve where it was in time
    let (astart, rrow) = (clip(&h, auto.id).start_tick.0, row(&h, auto.track_id));
    let from = super::playlist::lane_point(&h.app.playlist, surface, astart, rrow) + egui::vec2(2.0, 0.0);
    let to = super::playlist::lane_point(&h.app.playlist, surface, astart + 960, rrow) + egui::vec2(2.0, 0.0);
    h.drag(from, to, egui::PointerButton::Primary);
    let t = clip(&h, auto.id);
    assert_eq!(t.start_tick.0, astart + 960, "trimmed a beat");
    assert!(matches!(t.content, ClipContent::Automation { source_offset_ticks: Tick(960), .. }), "{:?}", t.content);
}

#[test]
fn transpose_and_voices_show_and_take_their_real_values() {
    let mut h = Harness::new(None);
    h.app.editor_view = EditorView::Synth;
    h.app.synth_panel.advanced = true;
    h.frame(vec![]);
    let saved = |h: &Harness| -> rawdawg_synthesizer::Patch { serde_json::from_value(h.app.project.channels[0].instrument.state.clone()).unwrap() };
    let label = h.scroll_into_view("Transpose", |h| h.node("Transpose"));
    let row: Vec<(String, egui::Rect)> = h.drawn_nodes().into_iter().filter(|(_, r)| (r.center().y - label.center().y).abs() < 6.0 && r.left() > label.right()).collect();
    assert!(row.iter().any(|(t, _)| t == "+0 st"), "the readout says +0 st: {row:?}");
    let value = row.iter().filter(|(t, _)| t == "0").map(|(_, r)| *r).next().expect("the slider's value box shows 0, not 48");
    // type 12 into it, as a user does
    h.click_at(value.center(), egui::PointerButton::Primary);
    h.frame(vec![]);
    for _ in 0..4 {
        h.key(egui::Key::Backspace, egui::Modifiers::NONE);
    }
    h.frame(vec![egui::Event::Text("12".into())]);
    h.key(egui::Key::Enter, egui::Modifiers::NONE);
    h.frame(vec![]);
    assert_eq!(saved(&h).tuning.transpose_semitones, 12, "an octave up, not three octaves down");
    let label = h.node("Transpose").unwrap();
    assert!(h.drawn_nodes().into_iter().any(|(t, r)| t == "+12 st" && (r.center().y - label.center().y).abs() < 6.0));
    // Voices reads 32, the voice count the project stores
    let voices = h.scroll_into_view("Voices", |h| h.node("Voices"));
    let row: Vec<String> = h.drawn_nodes().into_iter().filter(|(_, r)| (r.center().y - voices.center().y).abs() < 6.0).map(|(t, _)| t).collect();
    assert!(row.iter().any(|t| t == "32"), "{row:?}");
    assert!(!row.iter().any(|t| t == "31"));
}

#[test]
fn export_creates_a_folder_typed_with_the_name() {
    let root = std::env::temp_dir().join(format!("rawdawg-ui-export-folder-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let mut h = Harness::new(None);
    let (pid, cid) = (h.app.project.patterns[0].id, h.app.project.channels[0].id);
    h.app.apply(Command::AddNotes {
        pattern_id: pid,
        channel_id: cid,
        notes: vec![rawdawg_project::Note { id: rawdawg_types::NoteId::new(), tick: Tick(0), duration_ticks: Tick(480), pitch: 60, velocity: 0.8, channel: 0 }],
    });
    h.app.project_path = Some(root.clone());
    h.app.export.song = false;
    h.app.export.open = true;
    h.frame(vec![]);
    h.app.export.path = "mixes/bounces/final".into();
    // MIDI
    super::export::request_midi(&mut h.app);
    assert!(root.join("mixes/bounces/final.mid").is_file(), "{:?}", h.app.export.last_result);
    // audio, on its worker
    std::fs::remove_dir_all(root.join("mixes")).unwrap();
    h.app.export.tail_seconds = 0.0;
    super::export::request_audio(&mut h.app);
    for _ in 0..2000 {
        super::export::poll(&mut h.app);
        if !h.app.export.running() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let wav = root.join("mixes/bounces/final.wav");
    assert!(wav.is_file(), "{:?}", h.app.export.last_result);
    assert!(h.app.export.last_result.as_deref().is_some_and(|r| !r.starts_with("Export failed")), "{:?}", h.app.export.last_result);
    // stems into a folder that does not exist yet, next to the main file
    let run = |h: &mut Harness| {
        super::export::request_audio(&mut h.app);
        for _ in 0..2000 {
            super::export::poll(&mut h.app);
            if !h.app.export.running() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    h.app.export.path = "stems/take".into();
    h.app.export.stems = true;
    run(&mut h);
    let stems = std::fs::read_dir(root.join("stems/take-stems")).map(|r| r.count()).unwrap_or(0);
    assert!(root.join("stems/take.wav").is_file() && stems > 0, "{:?}", h.app.export.last_result);
    // a folder that cannot be made (a file is in the way): said so, nothing written
    std::fs::write(root.join("blocked"), b"a file").unwrap();
    h.app.export.stems = false;
    h.app.export.path = "blocked/mix".into();
    run(&mut h);
    assert!(h.app.export.last_result.as_deref().is_some_and(|r| r.contains("Could not create the folder")), "{:?}", h.app.export.last_result);
    super::export::request_midi(&mut h.app);
    assert!(h.app.export.last_result.as_deref().is_some_and(|r| r.contains("Could not create the folder")), "{:?}", h.app.export.last_result);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_project_takes_its_folder_name_and_can_be_renamed() {
    let root = std::env::temp_dir().join(format!("rawdawg-ui-rename-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let mut h = Harness::new(None);
    h.app.new_project();
    add_note(&mut h, 0);
    assert_eq!(h.app.project.name, "Untitled");
    // Save As names an untitled project after its folder; exports follow
    h.app.save_project_as(root.join("RandA"));
    assert_eq!(h.app.project.name, "RandA");
    assert_eq!(rawdawg_project::load_project_dir(&root.join("RandA.rawdawg")).unwrap().name, "RandA", "saved with the name");
    h.app.export.open = true;
    h.frame(vec![]);
    assert!(h.app.export.path.ends_with("RandA.wav"), "{}", h.app.export.path);
    // Save As again: a name that matched its folder follows the new folder
    h.app.save_project_as(root.join("RandB"));
    assert_eq!(h.app.project.name, "RandB");
    h.frame(vec![]);
    assert!(h.app.export.path.ends_with("RandB.wav"), "{}", h.app.export.path);
    // File › Rename project…: type, Enter; one undo step
    h.app.export.open = false;
    h.frame(vec![]);
    let file = h.node("File").unwrap();
    h.click_at(file.center(), egui::PointerButton::Primary);
    h.frame(vec![]);
    let item = h.drawn_nodes().into_iter().find(|(t, _)| t.starts_with("Rename project")).map(|(_, r)| r).expect("the menu item");
    h.click_at(item.center(), egui::PointerButton::Primary);
    h.frame(vec![]);
    let ctrl = egui::Modifiers { ctrl: true, command: true, ..Default::default() };
    h.key_event(egui::Key::A, true, false, ctrl);
    h.key_event(egui::Key::A, false, false, ctrl);
    h.frame(vec![egui::Event::ModifiersChanged(egui::Modifiers::NONE), egui::Event::Text("Night Drive".into())]);
    h.key(egui::Key::Enter, egui::Modifiers::NONE);
    assert_eq!(h.app.project.name, "Night Drive");
    assert!(h.app.files.rename.is_none(), "the window closed");
    // the export proposes the new name (it was not typed by the user)
    h.app.export.open = true;
    h.frame(vec![]);
    assert!(h.app.export.path.ends_with("Night Drive.wav"), "{}", h.app.export.path);
    // a path the user typed is kept through a rename
    h.app.export.path = root.join("mine").display().to_string();
    h.app.export.path_custom_for_tests();
    h.app.rename_project("Night Drive 2");
    h.frame(vec![]);
    assert!(h.app.export.path.ends_with("mine"), "{}", h.app.export.path);
    h.app.undo();
    h.app.export.open = false;
    // a name the user chose stays through Save As
    h.app.save_project_as(root.join("Elsewhere"));
    assert_eq!(h.app.project.name, "Night Drive");
    h.app.undo();
    assert_eq!(h.app.project.name, "RandB", "the rename is one undo step");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn opening_a_song_starts_it_at_bar_one() {
    let root = std::env::temp_dir().join(format!("rawdawg-ui-playhead-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let a = saved_song(&root, "A", 1, 1);
    let mut h = Harness::new(None);
    h.app.request_open(a.clone());
    h.app.seek(3 * 3840);
    h.frame(vec![]);
    assert_eq!(h.app.playhead_tick(), Some(3 * 3840));
    h.app.request_open(PathBuf::from("../../fixtures/demo-song.rawdawg"));
    assert_eq!(h.app.playhead_tick(), Some(0), "the new song starts at its beginning");
    h.frame(vec![]);
    let toolbar: Vec<String> = h.drawn_nodes().into_iter().filter(|(_, r)| r.center().y < 100.0).map(|(t, _)| t).collect();
    assert!(toolbar.iter().any(|t| t == "1:1"), "{toolbar:?}");
    h.app.seek(3840);
    h.app.new_project();
    assert_eq!(h.app.playhead_tick(), Some(0), "so does a new project");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn enter_confirms_and_escape_cancels_dialogs_and_the_effect_search() {
    let root = std::env::temp_dir().join(format!("rawdawg-ui-enter-esc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let mut h = Harness::new(None);
    add_note(&mut h, 0);
    // Ctrl+S on a new project: type a path, Enter saves
    h.key_event(egui::Key::S, true, false, egui::Modifiers { ctrl: true, command: true, ..Default::default() });
    h.key_event(egui::Key::S, false, false, egui::Modifiers::NONE);
    h.frame(vec![]);
    assert_eq!(h.app.files.dialog, Some(super::project_files::FileDialog::SaveAs));
    h.frame(vec![]);
    h.frame(vec![egui::Event::Text(root.join("EnterTest").display().to_string())]);
    h.key(egui::Key::Enter, egui::Modifiers::NONE);
    assert!(root.join("EnterTest.rawdawg/project.json").is_file(), "{:?} {:?}", h.app.files.path_input, h.app.files.error);
    assert!(h.app.files.dialog.is_none() && !h.app.history.is_dirty());
    // File › Open…: Escape closes it, also with the field unfocused (and it is not a panic)
    h.app.files.dialog = Some(super::project_files::FileDialog::Open);
    h.app.files.focus_field = true;
    h.frame(vec![]);
    h.key(egui::Key::Escape, egui::Modifiers::NONE);
    assert!(h.app.files.dialog.is_none(), "Escape cancelled Open");
    h.app.files.dialog = Some(super::project_files::FileDialog::Open);
    h.frame(vec![]);
    let heading = h.node("Playlist").unwrap();
    h.click_at(heading.center(), egui::PointerButton::Primary);
    h.key(egui::Key::Escape, egui::Modifiers::NONE);
    assert!(h.app.files.dialog.is_none(), "also when the field has no focus");
    // Enter in Open opens
    h.app.files.dialog = Some(super::project_files::FileDialog::Open);
    h.app.files.focus_field = true;
    h.app.files.path_input = String::new();
    h.frame(vec![]);
    h.frame(vec![egui::Event::Text("../../fixtures/demo-song.rawdawg".into())]);
    h.key(egui::Key::Enter, egui::Modifiers::NONE);
    assert!(h.app.project_path.as_ref().is_some_and(|p| p.ends_with("demo-song.rawdawg")), "{:?}", h.app.project_path);
    // Settings closes on Escape
    h.app.show_settings = true;
    h.frame(vec![]);
    h.key(egui::Key::Escape, egui::Modifiers::NONE);
    assert!(!h.app.show_settings);
    // the mixer's effect search: Enter adds the only match; it starts empty when reopened
    h.app.editor_view = EditorView::Mixer;
    h.app.mixer.selected = Some(h.app.project.mixer.master_id);
    h.frame(vec![]);
    let add = h.scroll_into_view("+ Add effect", |h| h.node("+ Add effect"));
    h.click_at(add.center(), egui::PointerButton::Primary);
    h.frame(vec![egui::Event::Text("Delay".into())]);
    let inserts = |h: &Harness| h.app.project.mixer_channel(h.app.project.mixer.master_id).unwrap().inserts.len();
    let before = inserts(&h);
    h.key(egui::Key::Enter, egui::Modifiers::NONE);
    assert_eq!(inserts(&h), before + 1, "Enter added the Delay");
    assert!(!h.app.mixer.add_open);
    h.app.editor_view = EditorView::Mixer;
    h.frame(vec![]);
    let add = h.scroll_into_view("+ Add effect", |h| h.node("+ Add effect"));
    h.click_at(add.center(), egui::PointerButton::Primary);
    h.frame(vec![]);
    assert_eq!(h.app.mixer.add_search_text(), "", "a fresh search, not \"DelayDelay\"");
    h.key(egui::Key::Escape, egui::Modifiers::NONE);
    assert!(!h.app.mixer.add_open, "Escape closes the search");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn enter_confirms_only_from_the_field_and_never_on_the_frame_a_dialog_opens_nor_in_new() {
    let mut h = Harness::new(None);
    add_note(&mut h, 0);
    // New: Enter does not throw the unsaved changes away
    h.app.files.dialog = Some(super::project_files::FileDialog::New);
    h.frame(vec![]);
    h.key(egui::Key::Enter, egui::Modifiers::NONE);
    assert_eq!(note_count(&h), 1, "Enter in New discards nothing");
    assert_eq!(h.app.files.dialog, Some(super::project_files::FileDialog::New));
    click(&mut h, "Cancel");
    // the Enter that opened a dialog (a menu entry chosen with the keyboard) does not confirm it
    h.app.history.mark_saved();
    h.app.files.path_input = "../../fixtures/demo-song.rawdawg".into();
    h.app.files.dialog = Some(super::project_files::FileDialog::Open);
    h.app.files.focus_field = true;
    h.key(egui::Key::Enter, egui::Modifiers::NONE);
    assert!(h.app.project_path.is_none(), "not opened by the Enter of the frame it appeared");
    // with the field unfocused, Enter does nothing either (it may belong to another widget)
    let heading = h.node("Playlist").unwrap();
    h.click_at(heading.center(), egui::PointerButton::Primary);
    h.app.files.dialog = Some(super::project_files::FileDialog::Open);
    h.frame(vec![]);
    h.frame(vec![]);
    h.key(egui::Key::Enter, egui::Modifiers::NONE);
    assert!(h.app.project_path.is_none(), "Enter outside the field");
    // from the field it opens
    let field = h.node("../../fixtures/demo-song.rawdawg").expect("the path field");
    h.click_at(field.center(), egui::PointerButton::Primary);
    h.key(egui::Key::Enter, egui::Modifiers::NONE);
    assert!(h.app.project_path.as_ref().is_some_and(|p| p.ends_with("demo-song.rawdawg")));
    // Rename keeps its own focus: opening it while Save As is pending does not steal that flag
    h.app.files.rename = Some("Song".into());
    h.app.files.focus_rename = true;
    h.app.files.focus_field = false;
    h.frame(vec![]);
    h.frame(vec![]);
    assert!(h.ctx.memory(|m| m.focused().is_some()), "the rename field has the keyboard");
}

#[test]
fn escape_goes_to_a_gesture_then_a_menu_then_the_dialog_and_otherwise_is_panic() {
    let mut h = Harness::new(None);
    h.app.editor_view = EditorView::PianoRoll;
    h.frame(vec![]);
    let surface = h.rect(egui::Id::unique("piano-roll-surface"));
    let p = super::piano_roll::grid_point(&h.app.piano_roll, surface, 960 + 100, 64.5);
    h.click_at(p, egui::PointerButton::Primary);
    assert_eq!(h.lane_notes().len(), 1);
    // a drag of the note in progress, with the Rename window open
    let from = super::piano_roll::grid_point(&h.app.piano_roll, surface, 960 + 60, 64.5);
    let to = super::piano_roll::grid_point(&h.app.piano_roll, surface, 1920 + 60, 66.5);
    h.move_to(from);
    h.frame(vec![egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }]);
    h.frame(vec![egui::Event::PointerMoved(to)]);
    h.frame(vec![egui::Event::PointerMoved(to)]);
    assert!(h.app.piano_roll.gesture_active());
    h.app.files.rename = Some("Song".into());
    h.frame(vec![]);
    h.frame(vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
    assert!(!h.app.piano_roll.gesture_active(), "the gesture is cancelled");
    assert_eq!(h.lane_notes()[0].tick, Tick(960), "the note stays where it was");
    assert!(h.app.files.rename.is_some(), "the dialog stays");
    h.frame(vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: false, repeat: false, modifiers: egui::Modifiers::NONE }]);
    h.frame(vec![egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }]);
    h.frame(vec![]);
    // an open menu takes Escape before the dialog
    let file = h.node("File").unwrap();
    h.click_at(file.center(), egui::PointerButton::Primary);
    assert!(egui::Popup::is_any_open(&h.ctx), "the File menu is open");
    h.key(egui::Key::Escape, egui::Modifiers::NONE);
    assert!(!egui::Popup::is_any_open(&h.ctx), "Escape closed the menu");
    assert!(h.app.files.rename.is_some(), "not the dialog");
    // holding Escape closes the frontmost dialog only, not the one under it as the key repeats
    h.app.show_settings = true;
    h.frame(vec![]);
    let esc = |pressed| egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE };
    h.frame(vec![esc(true)]);
    assert!(h.app.files.rename.is_none(), "a fresh press closes the frontmost");
    for _ in 0..3 {
        h.frame(vec![esc(true)]); // the platform's key repeat (egui marks it as a repeat)
    }
    h.frame(vec![esc(false)]);
    assert!(h.app.show_settings, "the repeats did not close Settings");
    h.key(egui::Key::Escape, egui::Modifiers::NONE);
    assert!(!h.app.show_settings, "the next press does");
    // with nothing to cancel it is the panic key: held notes are released
    h.app.held_notes.track(&TimedEvent::note_on(0, rawdawg_types::events::EventSource::Mouse, 0, 7, 60, 0.8));
    assert!(h.app.held_notes.contains(60));
    h.key(egui::Key::Escape, egui::Modifiers::NONE);
    assert!(!h.app.held_notes.contains(60), "Escape is panic");
}

#[test]
fn the_effect_search_takes_the_focus_once_and_escape_is_its_only_where_it_shows() {
    let mut h = Harness::new(None);
    h.app.editor_view = EditorView::Mixer;
    h.app.mixer.selected = Some(h.app.project.mixer.master_id);
    h.frame(vec![]);
    let add = h.scroll_into_view("+ Add effect", |h| h.node("+ Add effect"));
    h.click_at(add.center(), egui::PointerButton::Primary);
    assert!(h.ctx.memory(|m| m.focused().is_some()), "typed into at once");
    let inserts = |h: &Harness| h.app.project.mixer_channel(h.app.project.mixer.master_id).unwrap().inserts.len();
    let before = inserts(&h);
    // Enter on an empty search adds nothing
    h.key(egui::Key::Enter, egui::Modifiers::NONE);
    assert_eq!(inserts(&h), before);
    // clicking elsewhere leaves the field: it does not grab the keyboard back
    let heading = h.node("Playlist").unwrap();
    h.click_at(heading.center(), egui::PointerButton::Primary);
    h.frame(vec![]);
    assert!(h.ctx.memory(|m| m.focused().is_none()), "the focus stays where the user put it");
    // shown in another view, Escape is not swallowed by the hidden search
    h.app.editor_view = EditorView::Rack;
    h.app.show_settings = false;
    h.app.held_notes.track(&TimedEvent::note_on(0, rawdawg_types::events::EventSource::Mouse, 0, 7, 60, 0.8));
    h.key(egui::Key::Escape, egui::Modifiers::NONE);
    assert!(!h.app.held_notes.contains(60), "Escape was panic, not the hidden search");
}

#[test]
fn a_failed_autosave_says_so_in_the_file_menu() {
    let root = std::env::temp_dir().join(format!("rawdawg-ui-autosave-fail-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("file"), b"in the way").unwrap();
    let mut h = Harness::new(None);
    add_note(&mut h, 0);
    h.app.autosave_now(&root.join("file").join("Song.rawdawg"));
    assert!(matches!(h.app.files.last_autosave_note, Some(Err(_))));
    let file = h.node("File").unwrap();
    h.click_at(file.center(), egui::PointerButton::Primary);
    h.frame(vec![]);
    assert!(h.drawn_texts().iter().any(|t| t == "Autosave failed (hover for why)"), "{:?}", h.drawn_texts());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn escape_answers_the_unsaved_changes_prompts_with_cancel() {
    let root = std::env::temp_dir().join(format!("rawdawg-ui-esc-prompt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let b = saved_song(&root, "B", 1, 3);
    let mut h = Harness::new(None);
    add_note(&mut h, 0);
    h.app.request_open(b.clone());
    h.frame(vec![]);
    h.key(egui::Key::Escape, egui::Modifiers::NONE);
    assert!(h.app.files.open_prompt.is_none() && h.app.project_path.is_none(), "cancelled: nothing opened");
    assert_eq!(note_count(&h), 1, "nothing discarded");
    h.app.files.close_prompt = true;
    h.frame(vec![]);
    h.key(egui::Key::Escape, egui::Modifiers::NONE);
    assert!(!h.app.files.close_prompt && !h.app.files.closing, "cancelled: the window stays");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_import_after_an_autosave_in_a_never_saved_project_is_what_is_recovered() {
    let base = std::env::temp_dir().join(format!("rawdawg-ui-import-after-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let recovery = base.join("unsaved");
    let mut h = Harness::with_recovery_dir(None, None, recovery.clone());
    add_note(&mut h, 0);
    h.frame(vec![]); // the first autosave: one note
    let dir = h.app.unsaved_dir().unwrap();
    assert_eq!(autosave_count(&dir), 1);
    add_note(&mut h, 240);
    add_note(&mut h, 480);
    // an import gives it a working folder with a manifest (three notes), newer than the autosave
    std::thread::sleep(std::time::Duration::from_millis(20));
    let wav = base.join("hit.wav");
    {
        let spec = hound::WavSpec { channels: 1, sample_rate: 48_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(&wav, spec).unwrap();
        for i in 0..480 {
            w.write_sample(((i % 50) as i16 - 25) * 300).unwrap();
        }
        w.finalize().unwrap();
    }
    h.app.import_asset_only(&wav).unwrap();
    assert!(dir.join("project.json").is_file());
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(2);
    std::fs::File::options().write(true).open(dir.join("project.json")).unwrap().set_modified(later).unwrap();
    crash(h);
    let mut h = Harness::with_recovery_dir(None, None, recovery.clone());
    assert_eq!(h.app.files.recovery.len(), 1);
    assert!(h.app.files.recovery_labels[0].contains("3 notes"), "the newest version, not the older autosave: {:?}", h.app.files.recovery_labels);
    click(&mut h, "Recover");
    assert_eq!(note_count(&h), 3);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn the_file_menus_autosave_line_is_short_and_fits() {
    let root = std::env::temp_dir().join(format!("rawdawg-ui-autosave-line-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = saved_song(&root, "Long project folder name for the autosave line", 1, 1);
    let mut h = Harness::new(None);
    h.app.request_open(dir.clone());
    add_note(&mut h, 240);
    h.app.autosave_now(&dir);
    let file = h.node("File").unwrap();
    h.click_at(file.center(), egui::PointerButton::Primary);
    h.frame(vec![]);
    let nodes = h.drawn_nodes();
    let line = nodes.iter().find(|(t, _)| t.starts_with("Autosaved")).expect("the autosave line");
    assert!(line.0.ends_with(" ago"), "{:?}", line.0);
    assert!(!line.0.contains(std::path::MAIN_SEPARATOR), "no path in the line: {:?}", line.0);
    let export = nodes.iter().find(|(t, _)| t == "Export…").map(|(_, r)| *r).unwrap();
    assert!(line.1.right() <= export.right() + 1.0 && line.1.height() < 20.0, "one line inside the menu: {:?} vs {export:?}", line.1);
    let _ = std::fs::remove_dir_all(&root);
}

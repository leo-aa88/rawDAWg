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
        Self::with_preset(project_dir, None)
    }

    fn with_preset(project_dir: Option<&str>, ui_preset: Option<&str>) -> Self {
        let ctx = egui::Context::default();
        let opts = GuiOptions {
            open: project_dir.map(PathBuf::from),
            safe_mode: false,
            device: None,
            buffer: None,
            no_audio: true,
            screenshot: None,
            ui_preset: ui_preset.map(str::to_owned),
        };
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
    let mut h = Harness::new(Some("../../fixtures/demo-sampler.rawdawg"));
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

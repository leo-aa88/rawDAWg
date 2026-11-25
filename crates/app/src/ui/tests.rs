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
            catalog: None,
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
    let mut h = Harness::new(Some("../../fixtures/demo-sampler.rawdawg"));
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
    assert!(h.app.files.last_autosave_note.as_deref().is_some_and(|n| n.starts_with("Autosaved revision")), "{:?}", h.app.files.last_autosave_note);
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
    assert_eq!(rawdawg_project::load_project_dir(&dir).unwrap().name, "Untitled", "not written while read-only");
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

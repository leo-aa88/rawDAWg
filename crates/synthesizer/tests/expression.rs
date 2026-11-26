//! SYN-10 per-note expression: CLAP/VST3 note expressions, polyphonic pressure and MPE-style
//! per-channel CC 74 shape one note without touching the others.

use rawdawg_dsp::analysis::{dominant_frequency, features};
use rawdawg_dsp::math::rms;
use rawdawg_dsp::oscillator::BasicWave;
use rawdawg_dsp::processor::{PrepareSpec, ProcessContext, Processor, TransportView};
use rawdawg_dsp::{AudioBlock, BusLayout};
use rawdawg_synthesizer::Synth;
use rawdawg_synthesizer::patch::{Layer, ModRoute, ModSource, ModTarget, Patch};
use rawdawg_types::events::{EventKind, EventSource, NoteExpression, TimedEvent, pitch_note_id};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

struct Harness {
    synth: Synth,
    transport: TransportView,
}

impl Harness {
    fn new(patch: Patch) -> Self {
        let mut synth = Synth::new(patch);
        synth.prepare(&PrepareSpec::new(SR, BLOCK, BusLayout::Stereo)).unwrap();
        Self { synth, transport: TransportView::stopped(SR) }
    }

    /// Render `frames` (whole blocks), the events at the start of the first block.
    fn render(&mut self, frames: usize, events: &[TimedEvent]) -> (Vec<f32>, Vec<f32>) {
        let (mut out_l, mut out_r) = (Vec::with_capacity(frames), Vec::with_capacity(frames));
        let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
        for b in 0..frames.div_ceil(BLOCK) {
            let evs: &[TimedEvent] = if b == 0 { events } else { &[] };
            let mut outs: [&mut [f32]; 2] = [&mut l, &mut r];
            let mut ctx = ProcessContext { audio: AudioBlock::output_only(BLOCK, &mut outs), events: evs, transport: &self.transport };
            self.synth.process(&mut ctx);
            out_l.extend_from_slice(&l);
            out_r.extend_from_slice(&r);
        }
        (out_l, out_r)
    }
}

fn on(ch: u8, id: u32, pitch: u8) -> TimedEvent {
    TimedEvent::note_on(0, EventSource::Host, ch, id, pitch, 0.8)
}

fn expr(ch: u8, note_id: u32, expression: NoteExpression, value: f32) -> TimedEvent {
    TimedEvent::new(0, EventSource::Host, ch, EventKind::NoteExpression { note_id, expression, value })
}

fn hz(pitch: u8) -> f64 {
    440.0 * 2f64.powf((f64::from(pitch) - 69.0) / 12.0)
}

/// Amplitude of the component at `f` Hz (Goertzel, Hann window).
fn level_at(buf: &[f32], f: f64) -> f64 {
    let n = buf.len();
    let w = std::f64::consts::TAU * f / SR;
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for (i, x) in buf.iter().enumerate() {
        let win = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos();
        let s = f64::from(*x) * win + 2.0 * w.cos() * s1 - s2;
        s2 = s1;
        s1 = s;
    }
    (s1 * s1 + s2 * s2 - 2.0 * w.cos() * s1 * s2).sqrt() * 4.0 / n as f64
}

fn sine() -> Patch {
    Patch { layers: vec![Layer::basic(BasicWave::Sine)], ..Patch::default() }
}

#[test]
fn tuning_and_vibrato_move_only_the_addressed_note() {
    let mut h = Harness::new(sine());
    h.render(4800, &[on(0, 1, 57), on(0, 2, 64)]);
    let (before, _) = h.render(9600, &[]);
    let (a, b) = (level_at(&before, hz(57)), level_at(&before, hz(64)));
    assert!(a > 0.05 && b > 0.05, "both notes sound: {a} {b}");
    // note 1 up an octave
    h.render(2400, &[expr(0, 1, NoteExpression::Tuning, 12.0)]);
    let (after, _) = h.render(9600, &[]);
    assert!(level_at(&after, hz(57)) < a * 0.05, "note 1 left its pitch");
    assert!((level_at(&after, hz(69)) - a).abs() < a * 0.1, "and sounds an octave up");
    assert!((level_at(&after, hz(64)) - b).abs() < b * 0.05, "note 2 is untouched");
    // vibrato: the pitch of a lone note wobbles, a neighbour without vibrato does not
    let mut h = Harness::new(sine());
    h.render(4800, &[on(0, 1, 69), expr(0, 1, NoteExpression::Vibrato, 1.0)]);
    let pitches: Vec<f64> = (0..12).map(|_| dominant_frequency(&h.render(1024, &[]).0, SR)).collect();
    let (lo, hi) = pitches.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
    assert!(hi - lo > 15.0 && hi - lo < 60.0, "vibrato of about ±0.5 semitone: {pitches:?}");
}

#[test]
fn volume_expression_and_pan_shape_one_note() {
    let mut h = Harness::new(sine());
    h.render(4800, &[on(0, 1, 57), on(0, 2, 64)]);
    let (before, _) = h.render(9600, &[]);
    let (a, b) = (level_at(&before, hz(57)), level_at(&before, hz(64)));
    h.render(2400, &[expr(0, 2, NoteExpression::Volume, 0.0)]);
    let (muted, _) = h.render(9600, &[]);
    assert!(level_at(&muted, hz(64)) < b * 0.001, "volume 0 silences note 2");
    assert!((level_at(&muted, hz(57)) - a).abs() < a * 0.02, "note 1 is untouched");
    h.render(2400, &[expr(0, 1, NoteExpression::Volume, 2.0)]);
    let (louder, _) = h.render(9600, &[]);
    assert!((level_at(&louder, hz(57)) / a - 2.0).abs() < 0.05, "volume 2 is +6 dB");
    h.render(2400, &[expr(0, 1, NoteExpression::Expression, 0.5)]);
    let (half, _) = h.render(9600, &[]);
    assert!((level_at(&half, hz(57)) / a - 1.0).abs() < 0.05, "expression 0.5 halves it again");
    // pan: hard left
    let mut h = Harness::new(sine());
    h.render(4800, &[on(0, 1, 69)]);
    let (cl, cr) = h.render(4800, &[]);
    assert!((rms(&cl) - rms(&cr)).abs() < 1e-4, "centred by default");
    h.render(2400, &[expr(0, 1, NoteExpression::Pan, 0.0)]);
    let (l, r) = h.render(4800, &[]);
    assert!(rms(&r) < rms(&l) * 1e-3, "pan 0 is hard left: {} {}", rms(&l), rms(&r));
}

#[test]
fn expression_changes_are_ramped_without_clicks() {
    let max_step = |x: &[f32]| x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
    // each change fades one channel out (pan hard left fades the right one; the left gets louder)
    for (expression, value, right) in [(NoteExpression::Volume, 0.0, false), (NoteExpression::Pan, 0.0, true), (NoteExpression::Expression, 0.0, false)] {
        let mut h = Harness::new(sine());
        h.render(4800, &[on(0, 1, 69)]);
        let pick = |(l, r): (Vec<f32>, Vec<f32>)| if right { r } else { l };
        let pre = pick(h.render(4800, &[]));
        let natural = max_step(&pre);
        let post = pick(h.render(BLOCK * 2, &[expr(0, 1, expression, value)]));
        // include the boundary between the blocks, where an unramped change would jump
        let mut joined = vec![pre[pre.len() - 1]];
        joined.extend_from_slice(&post);
        // a 5 ms glide adds a little to the sine's own slope; a jump would be many times it
        assert!(max_step(&joined) <= natural * 1.25, "{expression:?}: {} vs a sine's own {natural}", max_step(&joined));
        assert!(post[post.len() - 64..].iter().all(|x| x.abs() < 1e-6), "{expression:?} reaches silence within two blocks");
    }
}

#[test]
fn pitch_ids_address_the_note_at_that_pitch_on_that_channel() {
    let mut h = Harness::new(sine());
    h.render(4800, &[on(0, 7, 57), on(1, 8, 64)]);
    let (before, _) = h.render(9600, &[]);
    let b = level_at(&before, hz(64));
    // wrong channel: nothing happens
    h.render(2400, &[expr(0, pitch_note_id(64), NoteExpression::Volume, 0.0)]);
    let (same, _) = h.render(9600, &[]);
    assert!((level_at(&same, hz(64)) - b).abs() < b * 0.02, "channel 0 has no note at 64");
    // right channel and pitch: the note with real id 8 is addressed
    h.render(2400, &[expr(1, pitch_note_id(64), NoteExpression::Volume, 0.0)]);
    let (muted, _) = h.render(9600, &[]);
    assert!(level_at(&muted, hz(64)) < b * 0.001);
    assert!(level_at(&muted, hz(57)) > 0.05, "the other note still sounds");
    // a real id from another source addresses nothing
    let other = TimedEvent::new(0, EventSource::TypingKeyboard, 0, EventKind::NoteExpression { note_id: 7, expression: NoteExpression::Volume, value: 0.0 });
    h.render(2400, &[other]);
    let (still, _) = h.render(9600, &[]);
    assert!(level_at(&still, hz(57)) > 0.05, "ids are per source");
}

#[test]
fn note_pressure_drives_the_aftertouch_route_of_its_note_only() {
    let mut patch = sine();
    // aftertouch closes the amp: pressure 1 = silent
    patch.mod_routes.push(ModRoute { source: ModSource::Aftertouch, target: ModTarget::Amp, depth: -1.0, enabled: true });
    let mut h = Harness::new(patch.clone());
    h.render(4800, &[on(0, 1, 57), on(0, 2, 64)]);
    let (before, _) = h.render(9600, &[]);
    let (a, b) = (level_at(&before, hz(57)), level_at(&before, hz(64)));
    let poly = TimedEvent::new(0, EventSource::Host, 0, EventKind::NotePressure { note_id: 1, value: 1.0 });
    h.render(2400, &[poly]);
    let (after, _) = h.render(9600, &[]);
    assert!(level_at(&after, hz(57)) < a * 0.01, "pressed note closes");
    assert!((level_at(&after, hz(64)) - b).abs() < b * 0.02, "its neighbour does not");
    // the note's own pressure wins over channel pressure; the other note follows the channel
    let channel = TimedEvent::new(0, EventSource::Host, 0, EventKind::ChannelPressure { value: 0.5 });
    h.render(2400, &[channel]);
    let (both, _) = h.render(9600, &[]);
    assert!(level_at(&both, hz(57)) < a * 0.01);
    assert!((level_at(&both, hz(64)) / b - 0.5).abs() < 0.05, "channel pressure 0.5 halves note 2");
    // CLAP's pressure expression is the same thing
    let mut h = Harness::new(patch);
    h.render(4800, &[on(0, 1, 57)]);
    h.render(2400, &[expr(0, 1, NoteExpression::Pressure, 1.0)]);
    let (pressed, _) = h.render(9600, &[]);
    assert!(level_at(&pressed, hz(57)) < a * 0.01);
}

#[test]
fn brightness_and_channel_timbre_move_the_filter_cutoff() {
    let mut patch = Patch { layers: vec![Layer::basic(BasicWave::Saw)], ..Patch::default() };
    patch.filter.cutoff_hz = 1000.0;
    let centroid = |events: &[TimedEvent]| {
        let mut h = Harness::new(patch.clone());
        h.render(4800, &[on(0, 1, 45)]);
        h.render(2400, events);
        features(&h.render(9600, &[]).0, SR).1
    };
    let neutral = centroid(&[]);
    let bright = centroid(&[expr(0, 1, NoteExpression::Brightness, 1.0)]);
    let dark = centroid(&[expr(0, 1, NoteExpression::Brightness, 0.0)]);
    assert!(bright > neutral * 1.3 && dark < neutral * 0.8, "dark {dark:.0} < neutral {neutral:.0} < bright {bright:.0} Hz");
    let cc = |ch: u8, value: f32| TimedEvent::new(0, EventSource::Host, ch, EventKind::Control { cc: 74, value });
    // CC 74 is opt-in: by default (old projects, MIDI files using CC 74 for anything) it does nothing
    let ignored = centroid(&[cc(0, 1.0)]);
    assert!((ignored - neutral).abs() < neutral * 0.02, "CC 74 ignored unless enabled: {ignored:.0} vs {neutral:.0}");
    // with "CC 74 → brightness (MPE)" on: CC 74 on the note's channel is brightness; on another
    // channel it does nothing
    patch.cc74_brightness = true;
    let centroid = |events: &[TimedEvent]| {
        let mut h = Harness::new(patch.clone());
        h.render(4800, &[on(0, 1, 45)]);
        h.render(2400, events);
        features(&h.render(9600, &[]).0, SR).1
    };
    let timbre = centroid(&[cc(0, 1.0)]);
    assert!((timbre - bright).abs() < bright * 0.02, "CC 74 = brightness: {timbre:.0} vs {bright:.0}");
    let elsewhere = centroid(&[cc(1, 1.0)]);
    assert!((elsewhere - neutral).abs() < neutral * 0.02);
    // the note's own brightness wins over the channel's
    let both = centroid(&[cc(0, 1.0), expr(0, 1, NoteExpression::Brightness, 0.0)]);
    assert!((both - dark).abs() < dark * 0.02);
}

#[test]
fn expression_resets_at_the_next_note_on() {
    let mut h = Harness::new(sine());
    h.render(4800, &[on(0, 1, 69), expr(0, 1, NoteExpression::Tuning, 7.0), expr(0, 1, NoteExpression::Volume, 0.3)]);
    let (bent, _) = h.render(4800, &[]);
    assert!((dominant_frequency(&bent, SR) - hz(76)).abs() < 2.0);
    h.render(24_000, &[TimedEvent::note_off(0, EventSource::Host, 0, 1, 69)]);
    h.render(4800, &[on(0, 2, 69)]);
    let (fresh, _) = h.render(4800, &[]);
    assert!((dominant_frequency(&fresh, SR) - 440.0).abs() < 1.0, "a new note starts in tune");
    assert!(rms(&fresh) > rms(&bent) * 2.0, "and at full volume");
}

#[test]
fn switching_cc74_brightness_off_returns_every_note_to_neutral() {
    use rawdawg_types::ParamId;
    let mut patch = Patch { layers: vec![Layer::basic(BasicWave::Saw)], cc74_brightness: true, ..Patch::default() };
    patch.filter.cutoff_hz = 1000.0;
    let mut h = Harness::new(patch.clone());
    h.render(4800, &[on(0, 1, 45), TimedEvent::new(0, EventSource::Host, 0, EventKind::Control { cc: 74, value: 1.0 })]);
    let bright = features(&h.render(9600, &[]).0, SR).1;
    let off = TimedEvent::new(0, EventSource::Host, 0, EventKind::Param { id: ParamId(rawdawg_synthesizer::params::CC74_BRIGHTNESS), value: 0.0 });
    h.render(2400, &[off]);
    let after = features(&h.render(9600, &[]).0, SR).1;
    patch.cc74_brightness = false;
    let mut plain = Harness::new(patch);
    plain.render(7200, &[on(0, 1, 45)]);
    let neutral = features(&plain.render(9600, &[]).0, SR).1;
    assert!(bright > neutral * 1.3, "on: {bright:.0} vs {neutral:.0}");
    assert!((after - neutral).abs() < neutral * 0.03, "off again: the sounding note is neutral ({after:.0} vs {neutral:.0})");
}

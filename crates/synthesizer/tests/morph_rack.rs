//! SYN-05: the morph rack crossfades corner sounds of any structure by an automatable XY
//! position, in sync on the same notes, within one synth's oscillator budget.

use rawdawg_dsp::analysis::dominant_frequency;
use rawdawg_dsp::math::rms;
use rawdawg_dsp::oscillator::BasicWave;
use rawdawg_dsp::processor::{PrepareSpec, ProcessContext, Processor, TransportView};
use rawdawg_dsp::{AudioBlock, BusLayout};
use rawdawg_rt::alloc_guard::{GuardAllocator, measure_rt};
use rawdawg_synthesizer::Synth;
use rawdawg_synthesizer::morph::compatible;
use rawdawg_synthesizer::morph_rack::{MorphRack, MorphRackState, PARAM_X, PARAM_Y};
use rawdawg_synthesizer::patch::{Generator, Layer, Patch, PluckSettings};
use rawdawg_types::ParamId;
use rawdawg_types::events::{EventKind, EventSource, TimedEvent};

#[global_allocator]
static GLOBAL: GuardAllocator = GuardAllocator;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

fn prepared(mut p: Box<dyn Processor>) -> Box<dyn Processor> {
    p.prepare(&PrepareSpec::new(SR, BLOCK, BusLayout::Stereo)).unwrap();
    p
}

/// Render `blocks` blocks; `events(b)` gives block b's events. Left then right channel.
fn render(p: &mut dyn Processor, blocks: usize, events: impl Fn(usize) -> Vec<TimedEvent>) -> (Vec<f32>, Vec<f32>) {
    let transport = TransportView::stopped(SR);
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let (mut out_l, mut out_r) = (Vec::new(), Vec::new());
    for b in 0..blocks {
        let evs = events(b);
        let mut outs: [&mut [f32]; 2] = [&mut l, &mut r];
        let mut ctx = ProcessContext { audio: AudioBlock::output_only(BLOCK, &mut outs), events: &evs, transport: &transport };
        p.process(&mut ctx);
        out_l.extend_from_slice(&l);
        out_r.extend_from_slice(&r);
    }
    (out_l, out_r)
}

fn note_on(pitch: u8) -> TimedEvent {
    TimedEvent::note_on(5, EventSource::Transport, 0, 1, pitch, 0.8)
}

fn note_off(pitch: u8) -> TimedEvent {
    TimedEvent::note_off(0, EventSource::Transport, 0, 1, pitch)
}

fn param(id: u32, value: f32) -> TimedEvent {
    TimedEvent::new(0, EventSource::Transport, 0, EventKind::Param { id: ParamId(id), value })
}

fn sine() -> Patch {
    Patch { layers: vec![Layer::basic(BasicWave::Sine)], ..Patch::default() }
}

/// A different structure: two layers, one of them a plucked string, an octave up.
fn pluck_pair() -> Patch {
    let mut pluck = Layer { generator: Generator::Pluck(PluckSettings::default()), ..Layer::basic(BasicWave::Saw) };
    pluck.coarse_semitones = 12;
    Patch { layers: vec![Layer::basic(BasicWave::Saw), pluck], ..Patch::default() }
}

fn rack(corners: [Patch; 4], x: f32, y: f32) -> Box<dyn Processor> {
    prepared(Box::new(MorphRack::new(MorphRackState::new(corners, x, y))))
}

fn max_diff(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

fn held(b: usize) -> Vec<TimedEvent> {
    if b == 0 { vec![note_on(57)] } else { vec![] }
}

#[test]
fn at_a_corner_the_rack_is_exactly_that_corner_sound() {
    let corners = [sine(), pluck_pair(), Patch::default(), sine()];
    for (i, (x, y)) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)].into_iter().enumerate() {
        let mut r = rack(corners.clone(), x, y);
        let mut solo = prepared(Box::new(Synth::new(corners[i].clone())));
        let (a, _) = render(r.as_mut(), 80, held);
        let (b, _) = render(solo.as_mut(), 80, held);
        assert!(rms(&b) > 0.01);
        assert_eq!(max_diff(&a, &b), 0.0, "corner {i} is played as it is");
    }
}

#[test]
fn incompatible_corners_crossfade_by_weight() {
    let (a, b) = (sine(), pluck_pair());
    assert!(!compatible(&[&a, &b, &a, &b]), "the corners really differ in structure");
    let mut r = rack([a.clone(), b.clone(), a.clone(), b.clone()], 0.25, 0.5);
    let mut sa = prepared(Box::new(Synth::new(a)));
    let mut sb = prepared(Box::new(Synth::new(b)));
    let (got, _) = render(r.as_mut(), 60, held);
    let (ra, _) = render(sa.as_mut(), 60, held);
    let (rb, _) = render(sb.as_mut(), 60, held);
    // x = 0.25: three quarters A (A and C), one quarter B (B and D)
    let want: Vec<f32> = ra.iter().zip(&rb).map(|(a, b)| 0.75 * a + 0.25 * b).collect();
    assert!(max_diff(&got, &want) < 1e-6, "parallel crossfade: {}", max_diff(&got, &want));
}

#[test]
fn x_automation_glides_while_the_held_note_keeps_sounding_in_every_corner() {
    // A: sine at the note, B: the same note an octave up (a different sound)
    let mut up = sine();
    up.layers[0].coarse_semitones = 12;
    let mut r = rack([sine(), up.clone(), sine(), up], 0.0, 0.0);
    // the note starts while B has no weight, then X moves to B
    let (out, _) = render(r.as_mut(), 120, |b| match b {
        0 => vec![note_on(57)],
        40 => vec![param(PARAM_X, 1.0)],
        _ => vec![],
    });
    let before = &out[20 * BLOCK..40 * BLOCK];
    let after = &out[80 * BLOCK..120 * BLOCK];
    assert!((dominant_frequency(before, SR) - 220.0).abs() < 1.0);
    assert!((dominant_frequency(after, SR) - 440.0).abs() < 1.0, "B plays the note that started before it had any weight");
    assert!(rms(after) > rms(before) * 0.7, "and it is still sounding");
    // the move glides (20 ms) instead of jumping
    let natural = before.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
    let edge = &out[40 * BLOCK - 1..44 * BLOCK];
    let step = edge.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
    // an octave-up sine is twice as steep; a jump would be far steeper still
    assert!(step < natural * 2.5, "no click when X moves: {step} vs {natural}");
    assert_eq!(r.param(ParamId(PARAM_X)), Some(1.0));
    assert_eq!(r.param(ParamId(PARAM_Y)), Some(0.0));
    // the rack's own parameters never reach the corner synths (Y = 1 would be a synth param id)
    let (y_moved, _) = render(r.as_mut(), 40, |b| if b == 0 { vec![param(PARAM_Y, 1.0)] } else { vec![] });
    assert!((dominant_frequency(&y_moved[20 * BLOCK..], SR) - 440.0).abs() < 1.0, "C/D at the bottom: still the sine pair");
}

#[test]
fn identical_corners_share_a_renderer_and_the_budget_is_enforced() {
    let r = MorphRack::new(MorphRackState::new([sine(), pluck_pair(), sine(), pluck_pair()], 0.5, 0.5));
    assert_eq!(r.renderers(), 2, "two distinct sounds, two renderers");
    assert_eq!(MorphRack::new(MorphRackState::new(std::array::from_fn(|_| sine()), 0.5, 0.5)).renderers(), 1);
    // four distinct heavy corners do not fit one synth's oscillator budget
    let heavy = |semis: i32| {
        let mut p = sine();
        p.max_voices = 64;
        p.layers[0].unison = 8;
        p.layers[0].coarse_semitones = semis;
        p
    };
    let state = MorphRackState::new([heavy(0), heavy(1), heavy(2), heavy(3)], 0.5, 0.5);
    let err = state.validate().unwrap_err();
    assert!(err.contains("oscillator voices"), "{err}");
    let mut too_big = MorphRack::new(state);
    assert!(too_big.prepare(&PrepareSpec::new(SR, BLOCK, BusLayout::Stereo)).is_err(), "refused, not approximated");
}

#[test]
fn state_round_trips_and_garbage_is_refused() {
    let r = MorphRack::new(MorphRackState::new([sine(), pluck_pair(), Patch::default(), sine()], 0.3, 0.8));
    let bytes = r.save_state();
    let mut back = MorphRack::default();
    back.load_state(&bytes).unwrap();
    assert_eq!(back.state(), r.state());
    assert!(back.load_state(b"{\"corners\": 3}").is_err());
    assert!(MorphRackState::from_json(br#"{"corners":[],"x":0.5,"y":0.5}"#).is_err());
}

#[test]
fn processing_is_allocation_free_with_notes_and_pad_moves() {
    let mut r = rack([sine(), pluck_pair(), Patch::default(), sine()], 0.5, 0.5);
    render(r.as_mut(), 4, |_| vec![]);
    let transport = TransportView::stopped(SR);
    let (mut l, mut rr) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let events: Vec<Vec<TimedEvent>> = (0..40)
        .map(|b| match b % 8 {
            0 => vec![note_on(50 + (b % 12) as u8), param(PARAM_X, (b % 5) as f32 / 4.0)],
            3 => vec![param(PARAM_Y, (b % 3) as f32 / 2.0)],
            6 => vec![note_off(50 + ((b - 6) % 12) as u8)],
            _ => vec![],
        })
        .collect();
    let (_, allocs) = measure_rt(|| {
        for evs in &events {
            let mut outs: [&mut [f32]; 2] = [&mut l, &mut rr];
            let mut ctx = ProcessContext { audio: AudioBlock::output_only(BLOCK, &mut outs), events: evs, transport: &transport };
            r.process(&mut ctx);
        }
    });
    assert_eq!(allocs, 0);
}

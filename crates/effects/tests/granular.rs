//! T-FX granular: density and size set the grain rate and length, grain pitch, freeze
//! keeps playing captured audio, reverse plays grains backwards, deterministic and
//! stereo-spread output, presets, RT safety, bounded voices.

use rawdawg_dsp::analysis::dominant_frequency;
use rawdawg_dsp::math::{peak, rms};
use rawdawg_dsp::processor::{PrepareSpec, ProcessContext, Processor, TransportView};
use rawdawg_dsp::{AudioBlock, BusLayout};
use rawdawg_effects::granular::{self, Granular};
use rawdawg_rt::alloc_guard::{GuardAllocator, measure_rt};
use rawdawg_types::ParamId;

#[global_allocator]
static GLOBAL: GuardAllocator = GuardAllocator;

const SR: f64 = 48_000.0;

fn run2(p: &mut dyn Processor, x: &[f32]) -> (Vec<f32>, Vec<f32>) {
    let n = x.len();
    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];
    let transport = TransportView::stopped(SR);
    let mut pos = 0;
    while pos < n {
        let frames = 256.min(n - pos);
        let inputs: [&[f32]; 2] = [&x[pos..pos + frames], &x[pos..pos + frames]];
        let (ol, orr) = (&mut out_l[pos..pos + frames], &mut out_r[pos..pos + frames]);
        let mut outputs: [&mut [f32]; 2] = [ol, orr];
        let mut ctx = ProcessContext { audio: AudioBlock { frames, inputs: &inputs, outputs: &mut outputs, sidechain: &[] }, events: &[], transport: &transport };
        p.process(&mut ctx);
        pos += frames;
    }
    (out_l, out_r)
}

fn run(p: &mut dyn Processor, x: &[f32]) -> Vec<f32> {
    run2(p, x).0
}

fn prepared() -> Box<dyn Processor> {
    let mut p = rawdawg_effects::make_effect(granular::TYPE_ID, None).unwrap();
    p.prepare(&PrepareSpec::new(SR, 256, BusLayout::Stereo)).unwrap();
    p
}

fn set(p: &mut dyn Processor, id: u32, phys: f64) {
    let d = p.descriptors().iter().find(|x| x.id.0 == id).copied().unwrap();
    assert!(p.set_param(ParamId(id), d.to_normalized(phys)), "param {id}");
}

fn sine(freq: f64, n: usize, amp: f32) -> Vec<f32> {
    (0..n).map(|i| (std::f64::consts::TAU * freq * i as f64 / SR).sin() as f32 * amp).collect()
}

/// Regular, non-jittered, centred grains from a constant input.
fn regular(size_ms: f64, density: f64) -> Box<dyn Processor> {
    let mut p = prepared();
    set(p.as_mut(), granular::P_SIZE, size_ms);
    set(p.as_mut(), granular::P_DENSITY, density);
    set(p.as_mut(), granular::P_POSITION, 100.0);
    set(p.as_mut(), granular::P_JITTER, 0.0);
    set(p.as_mut(), granular::P_PAN, 0.0);
    set(p.as_mut(), granular::P_SPREAD, 0.0);
    set(p.as_mut(), granular::P_MIX, 100.0);
    p.reset();
    p
}

#[test]
fn density_sets_the_grain_rate_and_size_the_grain_length() {
    let x = vec![1.0f32; 96_000];
    let mut p = regular(20.0, 5.0);
    let y = run(p.as_mut(), &x);
    // grains of a constant are window shapes: count bumps over the last 1.5 s (7–8 at 5/s)
    let seg = &y[24_000..];
    let onsets = seg.windows(2).filter(|w| w[0] < 0.3 && w[1] >= 0.3).count();
    assert!((7..=8).contains(&onsets), "5 grains/s over 1.5 s: {onsets} onsets");
    // each bump is about one grain long (20 ms → ~10 ms above half height for a Hann window)
    let first = seg.iter().position(|v| *v >= 0.3).unwrap();
    let width = seg[first..].iter().take_while(|v| **v >= 0.3).count() as f64 / SR * 1000.0;
    assert!((8.0..=14.0).contains(&width), "half-height width {width:.1} ms");
    assert!(peak(&y) <= 1.5);
}

#[test]
fn grain_pitch_shifts_the_dominant_frequency() {
    // non-overlapping grains (100 ms at 5/s): overlapping copies of a shifted periodic source
    // interfere by design (their phases differ by the spawn spacing), so each grain is judged alone
    for (st, expect) in [(12.0, 880.0), (-12.0, 220.0), (7.0, 440.0 * 2f64.powf(7.0 / 12.0))] {
        let mut p = regular(100.0, 5.0);
        set(p.as_mut(), granular::P_PITCH, st);
        set(p.as_mut(), granular::P_POSITION, 600.0);
        p.reset();
        let x = sine(440.0, 96_000, 0.5);
        let y = run(p.as_mut(), &x);
        let f = dominant_frequency(&y[60_000..60_000 + 16_384], SR);
        assert!((f / expect - 1.0).abs() < 0.02, "{st} st: {f:.1} Hz, expected {expect:.1}");
    }
}

#[test]
fn freeze_keeps_playing_the_captured_audio_and_unfrozen_grains_run_out() {
    let mut x = sine(440.0, 48_000, 0.5);
    x.extend(vec![0.0; 144_000]);
    let mut p = regular(100.0, 30.0);
    set(p.as_mut(), granular::P_POSITION, 300.0);
    set(p.as_mut(), granular::P_JITTER, 200.0);
    p.reset();
    let (a, b) = x.split_at(48_000);
    let mut y = run(p.as_mut(), a);
    set(p.as_mut(), granular::P_FREEZE, 1.0);
    y.extend(run(p.as_mut(), b));
    assert!(rms(&y[150_000..]) > 0.1, "frozen grains keep sounding: {}", rms(&y[150_000..]));
    let f = dominant_frequency(&y[150_000..150_000 + 16_384], SR);
    assert!((f - 440.0).abs() < 10.0, "frozen pitch {f:.1} Hz");
    let mut p = regular(100.0, 30.0);
    set(p.as_mut(), granular::P_POSITION, 300.0);
    set(p.as_mut(), granular::P_JITTER, 200.0);
    p.reset();
    let y = run(p.as_mut(), &x);
    assert!(rms(&y[150_000..]) < 1e-3, "unfrozen grains run out of audio: {}", rms(&y[150_000..]));
}

#[test]
fn reverse_plays_grains_backwards() {
    // a rising ramp over 3 s: forward grains rise, reversed grains fall
    let x: Vec<f32> = (0..144_000).map(|i| i as f32 / 144_000.0).collect();
    let slope = |reverse: f64| {
        let mut p = regular(400.0, 1.0);
        set(p.as_mut(), granular::P_POSITION, 500.0);
        set(p.as_mut(), granular::P_REVERSE, reverse);
        set(p.as_mut(), granular::P_WINDOW, 1.0);
        p.reset();
        let y = run(p.as_mut(), &x);
        // the grain spawned at 2 s peaks 200 ms later (triangle window); divide the window out
        // 60 ms either side of the peak and compare the recovered ramp end to start
        let peak_at = 96_000 + 9600;
        let w = |i: usize| 1.0 - (2.0 * ((i - 96_000) as f32 / 19_200.0) - 1.0).abs();
        let (i0, i1) = (peak_at - 2880, peak_at + 2880);
        y[i1] / w(i1) - y[i0] / w(i0)
    };
    assert!(slope(0.0) > 0.02, "forward grain rises: {}", slope(0.0));
    assert!(slope(100.0) < -0.02, "reversed grain falls: {}", slope(100.0));
}

#[test]
fn output_is_deterministic_and_pan_spread_widens_the_image() {
    let x = sine(220.0, 48_000, 0.5);
    let mut a = prepared();
    let mut b = prepared();
    for p in [a.as_mut(), b.as_mut()] {
        set(p, granular::P_JITTER, 300.0);
        set(p, granular::P_SPREAD, 5.0);
        set(p, granular::P_PAN, 100.0);
        p.reset();
    }
    let (al, ar) = run2(a.as_mut(), &x);
    let (bl, _) = run2(b.as_mut(), &x);
    assert_eq!(al, bl, "same seed, same grains");
    let diff = al.iter().zip(&ar).skip(8000).map(|(l, r)| (l - r).abs()).fold(0.0f32, f32::max);
    assert!(diff > 0.05, "pan spread makes the channels differ: {diff}");
    let mut p = prepared();
    set(p.as_mut(), granular::P_PAN, 0.0);
    set(p.as_mut(), granular::P_SPREAD, 0.0);
    p.reset();
    let (l, r) = run2(p.as_mut(), &x);
    assert!(l.iter().zip(&r).all(|(a, b)| (a - b).abs() < 1e-6), "no spread: identical channels");
}

#[test]
fn granular_presets_differ_voices_are_bounded_and_processing_is_allocation_free() {
    let presets = rawdawg_effects::presets(granular::TYPE_ID);
    assert!(presets.len() >= 4);
    let states: Vec<Vec<u8>> = presets.iter().map(|p| rawdawg_effects::preset_state(granular::TYPE_ID, p).unwrap()).collect();
    for i in 0..states.len() {
        for j in i + 1..states.len() {
            assert_ne!(states[i], states[j]);
        }
    }
    let x = sine(330.0, 8192, 0.5);
    for preset in presets {
        let mut p = rawdawg_effects::make_effect(granular::TYPE_ID, rawdawg_effects::preset_state(granular::TYPE_ID, preset).as_deref()).unwrap();
        p.prepare(&PrepareSpec::new(SR, 256, BusLayout::Stereo)).unwrap();
        let y = run(p.as_mut(), &x);
        assert!(y.iter().all(|v| v.is_finite()) && peak(&y) < 4.0, "{}", preset.name);
        let (_, allocs) = measure_rt(|| {
            for _ in 0..8 {
                let mut out_l = [0.0f32; 256];
                let mut out_r = [0.0f32; 256];
                let inputs: [&[f32]; 2] = [&x[..256], &x[..256]];
                let mut outputs: [&mut [f32]; 2] = [&mut out_l, &mut out_r];
                let transport = TransportView::stopped(SR);
                let mut ctx = ProcessContext { audio: AudioBlock { frames: 256, inputs: &inputs, outputs: &mut outputs, sidechain: &[] }, events: &[], transport: &transport };
                p.process(&mut ctx);
            }
        });
        assert_eq!(allocs, 0, "{} allocates", preset.name);
    }
    // maximum density with the longest grains never exceeds the voice pool
    let mut p = prepared();
    set(p.as_mut(), granular::P_DENSITY, 200.0);
    set(p.as_mut(), granular::P_SIZE, 500.0);
    p.reset();
    let y = run(p.as_mut(), &x);
    let g = p.as_any_mut().downcast_mut::<Granular>().unwrap();
    assert!(g.active_grains() <= granular::MAX_GRAINS);
    assert!(y.iter().all(|v| v.is_finite()) && peak(&y) < 4.0);
}

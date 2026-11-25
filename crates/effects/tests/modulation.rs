//! T-FX modulation: chorus vibrato rate and depth (free and tempo-synced), flanger comb
//! notches/resonances and bounded feedback, phaser notch placement per stage count and
//! stability, presets, RT safety.

use rawdawg_dsp::analysis::magnitude_spectrum;
use rawdawg_dsp::math::peak;
use rawdawg_dsp::processor::{PrepareSpec, ProcessContext, Processor, TransportView};
use rawdawg_dsp::{AudioBlock, BusLayout};
use rawdawg_effects::modulation::{chorus, flanger, phaser};
use rawdawg_rt::alloc_guard::{GuardAllocator, measure_rt};
use rawdawg_types::ParamId;

#[global_allocator]
static GLOBAL: GuardAllocator = GuardAllocator;

const SR: f64 = 48_000.0;

fn run_with(p: &mut dyn Processor, x: &[f32], tempo: f64) -> (Vec<f32>, Vec<f32>) {
    let n = x.len();
    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];
    let mut transport = TransportView::stopped(SR);
    transport.tempo_bpm = tempo;
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
    run_with(p, x, 120.0).0
}

fn prepared(type_id: &str) -> Box<dyn Processor> {
    let mut p = rawdawg_effects::make_effect(type_id, None).unwrap();
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

/// Impulse at the centre of the analysis window, where the window is flat (an impulse pair must
/// see equal weights for its cancellation to show).
fn impulse(n: usize) -> Vec<f32> {
    let mut x = vec![0.0f32; n];
    x[4096] = 1.0;
    x
}

/// Magnitude (dB) at `hz` over the first 8192 samples.
fn level_at(buf: &[f32], hz: f64) -> f32 {
    let spec = magnitude_spectrum(&buf[..8192]);
    let bin = (hz / SR * 8192.0).round() as usize;
    20.0 * spec[bin].max(1e-9).log10()
}

/// Demodulate a 1 kHz carrier: the unwrapped phase, sampled every `hop` samples, follows
/// `-ω·d(t)` for a delayed sine, so its swing gives the delay depth and its period the LFO rate.
fn delay_trajectory(y: &[f32], carrier_hz: f64, hop: usize) -> Vec<f64> {
    let w = std::f64::consts::TAU * carrier_hz / SR;
    let mut out = Vec::new();
    let mut prev = 0.0f64;
    let mut offset = 0.0f64;
    for (k, chunk) in y.chunks_exact(hop).enumerate() {
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (j, v) in chunk.iter().enumerate() {
            let t = (k * hop + j) as f64;
            re += *v as f64 * (w * t).cos();
            im -= *v as f64 * (w * t).sin();
        }
        let mut ph = im.atan2(re);
        if k > 0 {
            let mut d = ph - prev;
            while d > std::f64::consts::PI {
                d -= std::f64::consts::TAU;
            }
            while d < -std::f64::consts::PI {
                d += std::f64::consts::TAU;
            }
            offset += d;
            ph = offset;
        } else {
            offset = ph;
        }
        prev = im.atan2(re);
        // phase lag = ω·delay  →  delay in seconds
        out.push(-ph / w / SR);
    }
    out
}

/// Peak-to-peak swing (s) and oscillation frequency (Hz) of a trajectory sampled at SR/hop.
fn swing_and_rate(traj: &[f64], hop: usize) -> (f64, f64) {
    let skip = traj.len() / 8;
    let t = &traj[skip..];
    let (lo, hi) = t.iter().fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
    let mean = t.iter().sum::<f64>() / t.len() as f64;
    let crossings = t.windows(2).filter(|w| (w[0] - mean) * (w[1] - mean) < 0.0).count();
    let seconds = t.len() as f64 * hop as f64 / SR;
    (hi - lo, crossings as f64 / 2.0 / seconds)
}

fn chorus_vibrato(sync: f64, rate_hz: f64, depth_ms: f64, tempo: f64, shape: f64) -> (f64, f64) {
    let mut p = prepared(chorus::TYPE_ID);
    set(p.as_mut(), chorus::P_VOICES, 0.0);
    set(p.as_mut(), chorus::P_MIX, 100.0);
    set(p.as_mut(), chorus::P_SPREAD, 0.0);
    set(p.as_mut(), chorus::P_DELAY, 10.0);
    set(p.as_mut(), chorus::P_DEPTH, depth_ms);
    set(p.as_mut(), chorus::P_RATE, rate_hz);
    set(p.as_mut(), chorus::P_SYNC, sync);
    set(p.as_mut(), chorus::P_SHAPE, shape);
    p.reset();
    let x = sine(1000.0, 4 * SR as usize, 0.5);
    let (y, _) = run_with(p.as_mut(), &x, tempo);
    let traj = delay_trajectory(&y[2048..], 1000.0, 48);
    swing_and_rate(&traj, 48)
}

#[test]
fn chorus_vibrato_follows_rate_and_depth_free_and_synced() {
    // 2 Hz sine LFO, 4 ms depth: delay swings 4 ms peak to peak at 2 Hz
    let (swing, rate) = chorus_vibrato(0.0, 2.0, 4.0, 120.0, 0.0);
    assert!((swing * 1000.0 - 4.0).abs() < 0.4, "depth 4 ms measured {:.2} ms", swing * 1000.0);
    assert!((rate - 2.0).abs() < 0.15, "rate 2 Hz measured {rate:.3} Hz");
    // triangle at 0.5 Hz
    let (swing, rate) = chorus_vibrato(0.0, 0.5, 2.0, 120.0, 1.0);
    assert!((swing * 1000.0 - 2.0).abs() < 0.3, "triangle depth 2 ms measured {:.2} ms", swing * 1000.0);
    assert!((rate - 0.5).abs() < 0.15, "triangle rate measured {rate:.3} Hz");
    // sync 1/4 at 90 BPM = 1.5 Hz regardless of the free rate
    let (_, rate) = chorus_vibrato(9.0, 7.0, 3.0, 90.0, 0.0);
    assert!((rate - 1.5).abs() < 0.15, "synced 1/4 @ 90 BPM measured {rate:.3} Hz");
    // sync 1/8 at 120 BPM = 4 Hz
    let (_, rate) = chorus_vibrato(6.0, 0.1, 1.0, 120.0, 0.0);
    assert!((rate - 4.0).abs() < 0.2, "synced 1/8 @ 120 BPM measured {rate:.3} Hz");
}

#[test]
fn chorus_voices_spread_differ_between_channels_and_dry_at_zero_mix() {
    let x = sine(440.0, 24_000, 0.5);
    let mut p = prepared(chorus::TYPE_ID);
    set(p.as_mut(), chorus::P_VOICES, 2.0);
    set(p.as_mut(), chorus::P_SPREAD, 100.0);
    set(p.as_mut(), chorus::P_MIX, 100.0);
    p.reset();
    let (l, r) = run_with(p.as_mut(), &x, 120.0);
    let diff = l.iter().zip(&r).skip(4000).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(diff > 0.05, "full spread makes the channels differ: {diff}");
    assert!(l.iter().chain(&r).all(|v| v.is_finite() && v.abs() <= 1.0));
    let mut p = prepared(chorus::TYPE_ID);
    set(p.as_mut(), chorus::P_MIX, 0.0);
    p.reset();
    let y = run(p.as_mut(), &x);
    assert!(y.iter().zip(&x).all(|(a, b)| (a - b).abs() < 1e-6), "mix 0 is the dry signal");
}

fn static_flanger(delay_ms: f64, feedback: f64) -> Vec<f32> {
    let mut p = prepared(flanger::TYPE_ID);
    set(p.as_mut(), flanger::P_DEPTH, 0.0);
    set(p.as_mut(), flanger::P_DELAY, delay_ms);
    set(p.as_mut(), flanger::P_FEEDBACK, feedback);
    set(p.as_mut(), flanger::P_MIX, 50.0);
    set(p.as_mut(), flanger::P_SPREAD, 0.0);
    p.reset();
    run(p.as_mut(), &impulse(8192))
}

#[test]
fn flanger_static_comb_notches_and_feedback_resonances() {
    // 1 ms delay, no feedback: notches at 500 Hz and 1 500 Hz, full level at 1 kHz
    let y = static_flanger(1.0, 0.0);
    let (n500, p1000, n1500) = (level_at(&y, 500.0), level_at(&y, 1000.0), level_at(&y, 1500.0));
    assert!(p1000 - n500 > 25.0, "notch at 500 Hz: {n500} vs {p1000}");
    assert!(p1000 - n1500 > 25.0, "notch at 1 500 Hz: {n1500} vs {p1000}");
    // positive feedback sharpens peaks at multiples of 1 kHz
    let y = static_flanger(1.0, 90.0);
    assert!(level_at(&y, 1000.0) - level_at(&y, 1250.0) > 10.0, "resonance at 1 kHz with +90 % feedback");
    // negative feedback moves the resonances to odd multiples of 500 Hz
    let y = static_flanger(1.0, -90.0);
    assert!(level_at(&y, 500.0) - level_at(&y, 1000.0) > 10.0, "resonance at 500 Hz with −90 % feedback");
    assert!(y.iter().all(|v| v.is_finite()));
}

#[test]
fn flanger_feedback_stays_bounded_at_the_maximum() {
    let mut p = prepared(flanger::TYPE_ID);
    set(p.as_mut(), flanger::P_FEEDBACK, 95.0);
    set(p.as_mut(), flanger::P_DELAY, 0.3);
    set(p.as_mut(), flanger::P_DEPTH, 5.0);
    set(p.as_mut(), flanger::P_RATE, 3.0);
    set(p.as_mut(), flanger::P_MIX, 100.0);
    p.reset();
    // sustained broadband input for 4 s
    let mut seed = 0x1234_5678u32;
    let x: Vec<f32> = (0..4 * SR as usize)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed as f32 / u32::MAX as f32 - 0.5) * 0.5
        })
        .collect();
    let y = run(p.as_mut(), &x);
    assert!(y.iter().all(|v| v.is_finite()));
    assert!(peak(&y) <= 8.0, "bounded loop: peak {}", peak(&y));
    // and it decays once the input stops
    let tail = run(p.as_mut(), &vec![0.0; 2 * SR as usize]);
    assert!(peak(&tail[tail.len() - 4800..]) < peak(&y), "feedback below unity decays");
}

fn static_phaser(stages: f64, center: f64, feedback: f64) -> Vec<f32> {
    let mut p = prepared(phaser::TYPE_ID);
    set(p.as_mut(), phaser::P_DEPTH, 0.0);
    set(p.as_mut(), phaser::P_CENTER, center);
    set(p.as_mut(), phaser::P_STAGES, stages);
    set(p.as_mut(), phaser::P_FEEDBACK, feedback);
    set(p.as_mut(), phaser::P_MIX, 50.0);
    set(p.as_mut(), phaser::P_SPREAD, 0.0);
    p.reset();
    run(p.as_mut(), &impulse(8192))
}

#[test]
fn phaser_notches_follow_the_stage_count_and_feedback_is_stable() {
    // two stages: the single notch sits at the centre; far away the response is flat
    let y = static_phaser(0.0, 1000.0, 0.0);
    let (lo, notch, hi) = (level_at(&y, 100.0), level_at(&y, 1000.0), level_at(&y, 10_000.0));
    assert!(lo - notch > 30.0, "2 stages notch at centre: {notch} vs {lo}");
    assert!((lo - hi).abs() < 1.0, "flat away from the notch: {lo} vs {hi}");
    // four stages: notches at centre·tan(π/8) and centre·tan(3π/8), centre itself passes
    let y = static_phaser(1.0, 1000.0, 0.0);
    let (n1, c, n2) = (level_at(&y, 414.0), level_at(&y, 1000.0), level_at(&y, 2414.0));
    assert!(c - n1 > 20.0, "4 stages lower notch: {n1} vs {c}");
    assert!(c - n2 > 20.0, "4 stages upper notch: {n2} vs {c}");
    // twelve stages with maximum feedback: finite, bounded, decaying
    let mut p = prepared(phaser::TYPE_ID);
    set(p.as_mut(), phaser::P_STAGES, 5.0);
    set(p.as_mut(), phaser::P_FEEDBACK, 95.0);
    set(p.as_mut(), phaser::P_DEPTH, 100.0);
    set(p.as_mut(), phaser::P_RATE, 6.0);
    set(p.as_mut(), phaser::P_MIX, 100.0);
    p.reset();
    let x = sine(220.0, 4 * SR as usize, 0.5);
    let y = run(p.as_mut(), &x);
    assert!(y.iter().all(|v| v.is_finite()));
    assert!(peak(&y) <= 8.0, "bounded: {}", peak(&y));
    let tail = run(p.as_mut(), &vec![0.0; SR as usize]);
    assert!(peak(&tail[tail.len() - 4800..]) < 1e-3, "tail decays: {}", peak(&tail[tail.len() - 4800..]));
    // coefficients never leave the unit circle across the reachable sweep
    for hz in [1.0, 10.0, 100.0, 1000.0, 20_000.0, 30_000.0] {
        assert!(phaser::Phaser::coefficient(hz, 48_000.0).abs() < 1.0);
    }
}

#[test]
fn modulation_presets_differ_and_processing_is_allocation_free() {
    for type_id in [chorus::TYPE_ID, flanger::TYPE_ID, phaser::TYPE_ID] {
        let presets = rawdawg_effects::presets(type_id);
        assert!(presets.len() >= 3);
        let states: Vec<Vec<u8>> = presets.iter().map(|p| rawdawg_effects::preset_state(type_id, p).unwrap()).collect();
        for i in 0..states.len() {
            for j in i + 1..states.len() {
                assert_ne!(states[i], states[j], "{type_id} presets {i} and {j}");
            }
        }
        let x = sine(330.0, 4096, 0.5);
        for preset in presets {
            let mut p = rawdawg_effects::make_effect(type_id, rawdawg_effects::preset_state(type_id, preset).as_deref()).unwrap();
            p.prepare(&PrepareSpec::new(SR, 256, BusLayout::Stereo)).unwrap();
            let y = run(p.as_mut(), &x);
            assert!(y.iter().all(|v| v.is_finite()), "{type_id} preset {}", preset.name);
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
            assert_eq!(allocs, 0, "{type_id} preset {} allocates", preset.name);
        }
    }
}

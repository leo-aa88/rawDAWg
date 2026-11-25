//! T-FX dynamics: compressor static curve/timing/sidechain, limiter ceiling and
//! true-peak detection, gate hysteresis/hold/range.

use rawdawg_dsp::math::{peak, rms};
use rawdawg_dsp::processor::{PrepareSpec, ProcessContext, Processor, TransportView};
use rawdawg_dsp::{AudioBlock, BusLayout};
use rawdawg_effects::dynamics::{compressor, gate, limiter};
use rawdawg_rt::alloc_guard::{GuardAllocator, measure_rt};
use rawdawg_types::ParamId;

#[global_allocator]
static GLOBAL: GuardAllocator = GuardAllocator;

const SR: f64 = 48_000.0;

fn run_sc(p: &mut dyn Processor, left: &[f32], right: &[f32], sc: Option<(&[f32], &[f32])>) -> (Vec<f32>, Vec<f32>) {
    let n = left.len();
    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];
    let transport = TransportView::stopped(SR);
    let mut pos = 0;
    while pos < n {
        let frames = 256.min(n - pos);
        let inputs: [&[f32]; 2] = [&left[pos..pos + frames], &right[pos..pos + frames]];
        let scs: [&[f32]; 2] = match sc {
            Some((a, b)) => [&a[pos..pos + frames], &b[pos..pos + frames]],
            None => [&left[..0], &right[..0]],
        };
        let (ol, orr) = (&mut out_l[pos..pos + frames], &mut out_r[pos..pos + frames]);
        let mut outputs: [&mut [f32]; 2] = [ol, orr];
        let mut ctx = ProcessContext {
            audio: AudioBlock { frames, inputs: &inputs, outputs: &mut outputs, sidechain: if sc.is_some() { &scs } else { &[] } },
            events: &[],
            transport: &transport,
        };
        p.process(&mut ctx);
        pos += frames;
    }
    (out_l, out_r)
}

fn run(p: &mut dyn Processor, x: &[f32]) -> Vec<f32> {
    run_sc(p, x, x, None).0
}

fn sine(freq: f64, n: usize, amp: f32) -> Vec<f32> {
    (0..n).map(|i| (std::f64::consts::TAU * freq * i as f64 / SR).sin() as f32 * amp).collect()
}

fn prepared(type_id: &str) -> Box<dyn Processor> {
    let mut p = rawdawg_effects::make_effect(type_id, None).unwrap();
    p.prepare(&PrepareSpec::new(SR, 256, BusLayout::Stereo)).unwrap();
    p
}

fn set(p: &mut dyn Processor, id: u32, phys: f64) {
    let d = p.descriptors().iter().find(|x| x.id.0 == id).copied().unwrap();
    assert!(p.set_param(ParamId(id), d.to_normalized(phys)));
}

fn db(x: f32) -> f32 {
    20.0 * x.max(1e-9).log10()
}

#[test]
fn compressor_static_curve_matches_threshold_ratio_and_knee() {
    // level −6 dBFS peak, threshold −18, ratio 4, hard knee → 12 dB over → 9 dB of reduction → −15 dBFS
    let mut c = prepared(compressor::TYPE_ID);
    set(c.as_mut(), compressor::P_THRESHOLD, -18.0);
    set(c.as_mut(), compressor::P_RATIO, 4.0);
    set(c.as_mut(), compressor::P_KNEE, 0.0);
    set(c.as_mut(), compressor::P_ATTACK, 1.0);
    set(c.as_mut(), compressor::P_RELEASE, 50.0);
    let x = sine(1000.0, 48_000, 0.501);
    let y = run(c.as_mut(), &x);
    let out_db = db(peak(&y[24_000..]));
    assert!((out_db + 15.0).abs() < 0.5, "expected −15 dBFS, got {out_db}");
    assert!((c.meter(0).unwrap() - 9.0).abs() < 0.6, "GR meter {:?}", c.meter(0));
    // below threshold: untouched
    let q = sine(1000.0, 24_000, 0.05);
    let y = run(c.as_mut(), &q);
    assert!((db(peak(&y[12_000..])) - db(0.05)).abs() < 0.2);
    // soft knee halves the reduction exactly at threshold
    set(c.as_mut(), compressor::P_KNEE, 12.0);
    let g = compressor::Compressor::static_gain_db(-18.0, -18.0, 4.0, 12.0);
    assert!((g - (-(1.0 - 0.25) * 6.0 * 6.0 / 24.0)).abs() < 1e-5, "{g}");
    assert_eq!(compressor::Compressor::static_gain_db(-40.0, -18.0, 4.0, 12.0), 0.0);
    assert!((compressor::Compressor::static_gain_db(0.0, -18.0, 4.0, 12.0) + 13.5).abs() < 1e-4);
    // makeup adds gain
    set(c.as_mut(), compressor::P_KNEE, 0.0);
    set(c.as_mut(), compressor::P_MAKEUP, 6.0);
    let y = run(c.as_mut(), &x);
    assert!((db(peak(&y[24_000..])) + 9.0).abs() < 0.5);
}

#[test]
fn compressor_attack_release_follow_their_times_and_rms_is_slower() {
    let mut c = prepared(compressor::TYPE_ID);
    set(c.as_mut(), compressor::P_THRESHOLD, -20.0);
    set(c.as_mut(), compressor::P_RATIO, 10.0);
    set(c.as_mut(), compressor::P_KNEE, 0.0);
    set(c.as_mut(), compressor::P_ATTACK, 10.0);
    set(c.as_mut(), compressor::P_RELEASE, 100.0);
    // step: quiet then loud (−6 dBFS) for 0.5 s then quiet
    let mut x = sine(1000.0, 72_000, 0.01);
    for (i, v) in x.iter_mut().enumerate().take(48_000).skip(24_000) {
        *v = (std::f64::consts::TAU * 1000.0 * i as f64 / SR).sin() as f32 * 0.5;
    }
    let y = run(c.as_mut(), &x);
    // ~10 ms after the step the gain has moved ≈63 % of the way (one time constant)
    let final_db = db(peak(&y[40_000..48_000]));
    let at_10ms = db(peak(&y[24_000 + 480..24_000 + 528]));
    let start_db = db(0.5);
    let frac = (start_db - at_10ms) / (start_db - final_db);
    assert!(frac > 0.5 && frac < 0.85, "attack progress after one time constant: {frac}");
    // release: 100 ms after the loud part ends the gain is most of the way back up
    let tail_env = db(peak(&y[48_000 + 4800..48_000 + 5300]) / 0.01);
    assert!(tail_env > -4.0, "release recovers within ~one time constant: {tail_env} dB");
    // RMS detector reacts more slowly to a 1 ms burst than peak
    let mut burst = vec![0.0f32; 24_000];
    for (i, v) in burst.iter_mut().enumerate().take(12_048).skip(12_000) {
        *v = if i % 2 == 0 { 0.9 } else { -0.9 };
    }
    let mut cp = prepared(compressor::TYPE_ID);
    set(cp.as_mut(), compressor::P_THRESHOLD, -30.0);
    set(cp.as_mut(), compressor::P_RATIO, 10.0);
    set(cp.as_mut(), compressor::P_ATTACK, 0.1);
    let yp = run(cp.as_mut(), &burst);
    let mut cr = prepared(compressor::TYPE_ID);
    set(cr.as_mut(), compressor::P_THRESHOLD, -30.0);
    set(cr.as_mut(), compressor::P_RATIO, 10.0);
    set(cr.as_mut(), compressor::P_ATTACK, 0.1);
    set(cr.as_mut(), compressor::P_DETECTOR, 1.0);
    let yr = run(cr.as_mut(), &burst);
    assert!(
        peak(&yr[12_000..12_048]) > peak(&yp[12_000..12_048]) * 1.5,
        "RMS lets the burst through more than peak: {} vs {}",
        peak(&yr[12_000..12_048]),
        peak(&yp[12_000..12_048])
    );
}

#[test]
fn compressor_external_sidechain_ducks_the_program() {
    let mut c = prepared(compressor::TYPE_ID);
    set(c.as_mut(), compressor::P_THRESHOLD, -30.0);
    set(c.as_mut(), compressor::P_RATIO, 8.0);
    set(c.as_mut(), compressor::P_ATTACK, 1.0);
    set(c.as_mut(), compressor::P_RELEASE, 30.0);
    set(c.as_mut(), compressor::P_SIDECHAIN, 1.0);
    let prog = sine(440.0, 48_000, 0.1); // −20 dBFS: never above threshold by itself
    let mut key = vec![0.0f32; 48_000];
    for (i, v) in key.iter_mut().enumerate().take(36_000).skip(24_000) {
        *v = (std::f64::consts::TAU * 60.0 * i as f64 / SR).sin() as f32 * 0.8;
    }
    let (y, _) = run_sc(c.as_mut(), &prog, &prog, Some((&key, &key)));
    let before = rms(&y[12_000..23_000]);
    let during = rms(&y[26_000..35_000]);
    assert!(during < before * 0.3, "sidechain ducks the program: {before} → {during}");
    // without the sidechain flag the key is ignored
    set(c.as_mut(), compressor::P_SIDECHAIN, 0.0);
    let (y2, _) = run_sc(c.as_mut(), &prog, &prog, Some((&key, &key)));
    assert!((rms(&y2[26_000..35_000]) - rms(&y2[12_000..23_000])).abs() < 0.005);
}

#[test]
fn limiter_holds_the_ceiling_reports_latency_and_true_peak_sees_intersample_peaks() {
    let mut l = prepared(limiter::TYPE_ID);
    set(l.as_mut(), limiter::P_CEILING, -6.0);
    set(l.as_mut(), limiter::P_TRUE_PEAK, 0.0);
    set(l.as_mut(), limiter::P_RELEASE, 50.0);
    let x = sine(1000.0, 48_000, 0.9);
    let y = run(l.as_mut(), &x);
    let ceil = 10f32.powf(-6.0 / 20.0);
    // the ceiling parameter smooths over 10 ms; judge the steady state
    assert!(peak(&y[24_000..]) <= ceil * 1.001, "sample peaks never exceed the ceiling: {}", peak(&y[24_000..]));
    assert!(peak(&y[24_000..]) > ceil * 0.95, "limits at the ceiling, not below: {}", peak(&y[24_000..]));
    assert_eq!(l.latency_frames(), 48, "1 ms lookahead at 48 kHz");
    // 0.9 (−0.9 dBFS) held at −6 dBFS → 5.1 dB of gain reduction on the meter
    assert!((l.meter(0).unwrap() - 5.1).abs() < 0.4, "GR meter ≈ 5.1 dB: {:?}", l.meter(0));
    // a high-frequency sine at ±1 sample peaks is louder between samples (≈ +3 dB at fs/4 with 45° phase)
    let n = 24_000;
    let hf: Vec<f32> = (0..n).map(|i| (std::f64::consts::PI / 2.0 * i as f64 + std::f64::consts::FRAC_PI_4).sin() as f32 * 0.7).collect();
    assert!((peak(&hf) - 0.495).abs() < 0.01, "sample peak is 0.7·sin(45°)");
    let mut sp = prepared(limiter::TYPE_ID);
    set(sp.as_mut(), limiter::P_CEILING, 0.0);
    set(sp.as_mut(), limiter::P_TRUE_PEAK, 0.0);
    run(sp.as_mut(), &hf);
    let sample_peak_db = sp.meter(1).unwrap();
    let mut tp = prepared(limiter::TYPE_ID);
    set(tp.as_mut(), limiter::P_CEILING, 0.0);
    set(tp.as_mut(), limiter::P_TRUE_PEAK, 1.0);
    run(tp.as_mut(), &hf);
    let true_peak_db = tp.meter(1).unwrap();
    assert!(true_peak_db - sample_peak_db > 2.0, "true-peak estimate finds the inter-sample maximum: {sample_peak_db} dB sample vs {true_peak_db} dBTP");
    assert!((true_peak_db - db(0.7)).abs() < 0.5, "estimated dBTP ≈ the analog peak of 0.7: {true_peak_db}");
    // and limits it: true-peak mode at ceiling −3 dB brings the estimate down to the ceiling
    set(tp.as_mut(), limiter::P_CEILING, -3.0);
    let _ = run(tp.as_mut(), &hf);
    let y = run(tp.as_mut(), &hf);
    let mut det = limiter::TruePeakDetector::new();
    let mut tpk = 0.0f32;
    for v in &y[4_000..] {
        tpk = tpk.max(det.push(*v));
    }
    assert!(db(tpk) < -3.0 + 0.3, "output true peak ≤ ceiling: {} dBTP", db(tpk));
}

#[test]
fn gate_opens_above_threshold_uses_hysteresis_hold_and_range() {
    let mut g = prepared(gate::TYPE_ID);
    set(g.as_mut(), gate::P_THRESHOLD, -20.0);
    set(g.as_mut(), gate::P_HYSTERESIS, 6.0);
    set(g.as_mut(), gate::P_ATTACK, 0.5);
    set(g.as_mut(), gate::P_HOLD, 50.0);
    set(g.as_mut(), gate::P_RELEASE, 20.0);
    set(g.as_mut(), gate::P_RANGE, -80.0);
    // −40 dB noise floor (closed), −10 dB burst (opens), −23 dB (between close and open: stays open), then floor
    let mut x = sine(500.0, 96_000, 0.01);
    for (i, v) in x.iter_mut().enumerate() {
        let amp = if (24_000..36_000).contains(&i) {
            0.316
        } else if (36_000..60_000).contains(&i) {
            0.0708
        } else {
            0.01
        };
        *v = (std::f64::consts::TAU * 500.0 * i as f64 / SR).sin() as f32 * amp;
    }
    let y = run(g.as_mut(), &x);
    assert!(peak(&y[10_000..23_000]) < 0.001, "closed below threshold: {}", peak(&y[10_000..23_000]));
    assert!(peak(&y[26_000..35_000]) > 0.3, "open during the burst");
    assert!(peak(&y[40_000..59_000]) > 0.06, "hysteresis keeps it open at −23 dB (close point is −26 dB): {}", peak(&y[40_000..59_000]));
    // hold: stays open 50 ms after the level drops, then closes within a few release constants
    assert!(peak(&y[60_000..61_500]) > 0.005, "hold keeps it open for 50 ms");
    assert!(peak(&y[70_000..90_000]) < 0.001, "closed again after hold + release: {}", peak(&y[70_000..90_000]));
    // range limits the attenuation
    set(g.as_mut(), gate::P_RANGE, -12.0);
    let y = run(g.as_mut(), &x);
    let closed = peak(&y[10_000..23_000]);
    assert!((db(closed / 0.01) + 12.0).abs() < 1.0, "range −12 dB: {}", db(closed / 0.01));
}

#[test]
fn dynamics_presets_differ_state_round_trips_and_process_is_allocation_free() {
    for type_id in [compressor::TYPE_ID, limiter::TYPE_ID, gate::TYPE_ID] {
        let presets = rawdawg_effects::presets(type_id);
        assert!(presets.len() >= 3, "{type_id}");
        let states: Vec<Vec<u8>> = presets.iter().map(|p| rawdawg_effects::preset_state(type_id, p).unwrap()).collect();
        for i in 0..states.len() {
            for j in i + 1..states.len() {
                assert_ne!(states[i], states[j], "{type_id}: presets {i} and {j} identical");
            }
        }
        let mut p = prepared(type_id);
        p.load_state(&states[0]).unwrap();
        let mut q = prepared(type_id);
        q.load_state(&p.save_state()).unwrap();
        for d in p.descriptors() {
            assert_eq!(p.param(d.id), q.param(d.id));
        }
        let x = sine(220.0, 4096, 0.5);
        run(p.as_mut(), &x);
        let (_, allocs) = measure_rt(|| {
            for _ in 0..8 {
                let mut out_l = [0.0f32; 256];
                let mut out_r = [0.0f32; 256];
                let inputs: [&[f32]; 2] = [&x[..256], &x[..256]];
                let mut outputs: [&mut [f32]; 2] = [&mut out_l, &mut out_r];
                let transport = TransportView::stopped(SR);
                let mut ctx = ProcessContext { audio: AudioBlock { frames: 256, inputs: &inputs, outputs: &mut outputs, sidechain: &[] }, events: &[], transport: &transport };
                p.process(&mut ctx);
                p.reset();
            }
        });
        assert_eq!(allocs, 0, "{type_id} allocates on the process path");
    }
}

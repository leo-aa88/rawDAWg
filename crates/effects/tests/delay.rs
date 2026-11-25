//! T-FX delay: echo timing/feedback, tempo sync, ping-pong, multitap, reverse latency, tape bounds.

use rawdawg_dsp::math::peak;
use rawdawg_dsp::processor::{PrepareSpec, ProcessContext, Processor, TransportView};
use rawdawg_dsp::{AudioBlock, BusLayout};
use rawdawg_effects::delay::{self, Delay};
use rawdawg_rt::alloc_guard::{GuardAllocator, measure_rt};
use rawdawg_types::ParamId;

#[global_allocator]
static GLOBAL: GuardAllocator = GuardAllocator;

const SR: f64 = 48_000.0;

fn run_t(p: &mut dyn Processor, left: &[f32], right: &[f32], bpm: f64) -> (Vec<f32>, Vec<f32>) {
    let n = left.len();
    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];
    let transport = TransportView { tempo_bpm: bpm, ..TransportView::stopped(SR) };
    let mut pos = 0;
    while pos < n {
        let frames = 256.min(n - pos);
        let inputs: [&[f32]; 2] = [&left[pos..pos + frames], &right[pos..pos + frames]];
        let (ol, orr) = (&mut out_l[pos..pos + frames], &mut out_r[pos..pos + frames]);
        let mut outputs: [&mut [f32]; 2] = [ol, orr];
        let mut ctx = ProcessContext { audio: AudioBlock { frames, inputs: &inputs, outputs: &mut outputs, sidechain: &[] }, events: &[], transport: &transport };
        p.process(&mut ctx);
        pos += frames;
    }
    (out_l, out_r)
}

fn prepared() -> Box<dyn Processor> {
    let mut p = rawdawg_effects::make_effect(delay::TYPE_ID, None).unwrap();
    p.prepare(&PrepareSpec::new(SR, 256, BusLayout::Stereo)).unwrap();
    p
}

fn set(p: &mut dyn Processor, id: u32, phys: f64) {
    let d = p.descriptors().iter().find(|x| x.id.0 == id).copied().unwrap();
    assert!(p.set_param(ParamId(id), d.to_normalized(phys)), "param {id}");
}

/// A 2 ms burst at the start of an otherwise silent buffer.
fn burst(n: usize, amp: f32) -> Vec<f32> {
    (0..n).map(|i| if i < 96 { amp * (std::f32::consts::TAU * 1000.0 * i as f32 / SR as f32).sin() } else { 0.0 }).collect()
}

fn window_peak(buf: &[f32], center: usize, half: usize) -> f32 {
    peak(&buf[center.saturating_sub(half)..(center + half).min(buf.len())])
}

fn clean(p: &mut dyn Processor) {
    set(p, delay::P_DAMP, 20_000.0);
    set(p, delay::P_HIGHPASS, 20.0);
    set(p, delay::P_OFFSET, 0.0);
    set(p, delay::P_MIX, 100.0);
}

#[test]
fn stereo_delay_echoes_at_the_set_time_and_decays_by_the_feedback() {
    let mut p = prepared();
    clean(p.as_mut());
    set(p.as_mut(), delay::P_MODE, 0.0);
    set(p.as_mut(), delay::P_TIME, 250.0);
    set(p.as_mut(), delay::P_FEEDBACK, 50.0);
    let x = burst(72_000, 0.2);
    let (l, r) = run_t(p.as_mut(), &x, &x, 120.0);
    let e1 = window_peak(&l, 12_000 + 48, 200);
    let e2 = window_peak(&l, 24_000 + 48, 200);
    let e3 = window_peak(&l, 36_000 + 48, 200);
    assert!(e1 > 0.15, "first echo at 250 ms: {e1}");
    assert!((e2 / e1 - 0.5).abs() < 0.06, "feedback 50 %: {}", e2 / e1);
    assert!((e3 / e2 - 0.5).abs() < 0.08, "{}", e3 / e2);
    assert!(peak(&l[2_000..11_000]) < 1e-3, "silence between burst and echo");
    assert_eq!(l, r, "equal channels in stereo mode with no offset");
    // a positive offset delays the right channel further
    set(p.as_mut(), delay::P_OFFSET, 20.0);
    p.reset();
    let (l2, r2) = run_t(p.as_mut(), &x, &x, 120.0);
    assert!(window_peak(&l2, 12_000 + 48, 200) > 0.15 && window_peak(&r2, 12_000 + 48, 100) < 0.02, "R delayed by 20 %");
    assert!(window_peak(&r2, 14_400 + 48, 200) > 0.15);
    assert_eq!(p.latency_frames(), 0);
}

#[test]
fn tempo_sync_follows_the_transport_tempo() {
    let mut p = prepared();
    clean(p.as_mut());
    set(p.as_mut(), delay::P_FEEDBACK, 0.0);
    set(p.as_mut(), delay::P_SYNC, 9.0); // 1/4
    let x = burst(60_000, 0.2);
    let (l, _) = run_t(p.as_mut(), &x, &x, 120.0);
    assert!(window_peak(&l, 24_000 + 48, 200) > 0.15, "quarter at 120 BPM = 500 ms");
    assert!(window_peak(&l, 16_000, 400) < 1e-3);
    p.reset();
    let (l, _) = run_t(p.as_mut(), &x, &x, 90.0);
    assert!(window_peak(&l, 32_000 + 48, 200) > 0.15, "quarter at 90 BPM = 667 ms");
    let d = p.as_any_mut().downcast_mut::<Delay>().unwrap();
    assert!((d.base_time_ms(120.0) - 500.0).abs() < 1e-3);
    set(p.as_mut(), delay::P_SYNC, 8.0); // 1/8 dotted = 375 ms at 120
    let d = p.as_any_mut().downcast_mut::<Delay>().unwrap();
    assert!((d.base_time_ms(120.0) - 375.0).abs() < 1e-3);
}

#[test]
fn ping_pong_alternates_channels_and_multitap_places_taps() {
    let mut p = prepared();
    clean(p.as_mut());
    set(p.as_mut(), delay::P_MODE, 1.0);
    set(p.as_mut(), delay::P_TIME, 200.0);
    set(p.as_mut(), delay::P_FEEDBACK, 70.0);
    let x = burst(60_000, 0.2);
    let (l, r) = run_t(p.as_mut(), &x, &x, 120.0);
    let (t1, t2) = (9_600 + 48, 19_200 + 48);
    assert!(window_peak(&l, t1, 200) > 0.1 && window_peak(&r, t1, 150) < 0.01, "first repeat left only: L {} R {}", window_peak(&l, t1, 200), window_peak(&r, t1, 150));
    assert!(window_peak(&r, t2, 200) > 0.05 && window_peak(&l, t2, 150) < 0.01, "second repeat right only");
    // multitap: taps at 0.25× and 0.5× of 400 ms, −6 and −12 dB, centre pan
    let mut m = prepared();
    clean(m.as_mut());
    set(m.as_mut(), delay::P_MODE, 3.0);
    set(m.as_mut(), delay::P_TIME, 400.0);
    set(m.as_mut(), delay::P_FEEDBACK, 0.0);
    for t in 0..delay::MAX_TAPS {
        set(m.as_mut(), delay::tap_param(t, delay::T_ON), if t < 2 { 1.0 } else { 0.0 });
        set(m.as_mut(), delay::tap_param(t, delay::T_PAN), 0.0);
    }
    set(m.as_mut(), delay::tap_param(0, delay::T_TIME), 0.25);
    set(m.as_mut(), delay::tap_param(0, delay::T_GAIN), -6.0);
    set(m.as_mut(), delay::tap_param(1, delay::T_TIME), 0.5);
    set(m.as_mut(), delay::tap_param(1, delay::T_GAIN), -12.0);
    let (l, r) = run_t(m.as_mut(), &x, &x, 120.0);
    let a = window_peak(&l, 4_800 + 48, 200);
    let b = window_peak(&l, 9_600 + 48, 200);
    assert!(a > 0.05 && b > 0.02, "taps present: {a} {b}");
    assert!((20.0 * (b / a).log10() + 6.0).abs() < 1.0, "tap gains 6 dB apart: {}", 20.0 * (b / a).log10());
    assert!(window_peak(&l, 7_200, 1_000) < 1e-3, "nothing between the taps");
    assert!((window_peak(&r, 4_800 + 48, 200) - a).abs() < 0.01, "centre pan");
}

#[test]
fn reverse_plays_segments_backwards_and_reports_its_latency() {
    let mut p = prepared();
    clean(p.as_mut());
    set(p.as_mut(), delay::P_MODE, 4.0);
    set(p.as_mut(), delay::P_TIME, 100.0); // 4800-frame segments
    set(p.as_mut(), delay::P_FEEDBACK, 0.0);
    p.reset(); // snap the parameter smoothers
    assert_eq!(p.latency_frames(), 4_800);
    // a rising ramp during the first segment, then silence
    let x: Vec<f32> = (0..20_000).map(|i| if i < 4_800 { i as f32 / 4_800.0 } else { 0.0 }).collect();
    let (l, _) = run_t(p.as_mut(), &x, &x, 120.0);
    // the second segment (4800..9600) plays the first one reversed: a falling ramp
    assert!((l[4_800] - 1.0).abs() < 0.01 && (l[4_800 + 2_400] - 0.5).abs() < 0.01 && l[9_599].abs() < 0.01, "{} {} {}", l[4_800], l[7_200], l[9_599]);
    assert!(peak(&l[10_000..]) < 1e-3, "feedback 0: one reversed copy only");
    assert!(matches!(p.tail(), rawdawg_dsp::processor::Tail::Frames(_)));
}

#[test]
fn tape_mode_modulates_and_stays_bounded_at_maximum_feedback() {
    let mut p = prepared();
    clean(p.as_mut());
    set(p.as_mut(), delay::P_MODE, 2.0);
    set(p.as_mut(), delay::P_TIME, 100.0);
    set(p.as_mut(), delay::P_FEEDBACK, 98.0);
    set(p.as_mut(), delay::P_DRIVE, 24.0);
    set(p.as_mut(), delay::P_WOW_DEPTH, 4.0);
    set(p.as_mut(), delay::P_WOW_RATE, 2.0);
    let x = burst(96_000, 0.9);
    let (l, _) = run_t(p.as_mut(), &x, &x, 120.0);
    assert!(l.iter().all(|v| v.is_finite()));
    assert!(peak(&l) < 2.0, "feedback loop bounded by the soft clip: {}", peak(&l));
    assert!(peak(&l[80_000..]) > 0.01, "near-unity feedback keeps repeating");
    // wow moves the repeats: repeat positions are not all exactly 4800 apart
    let mut onsets = Vec::new();
    let mut i = 200;
    while i < 40_000 {
        if l[i].abs() > 0.05 && peak(&l[i.saturating_sub(300)..i]) < 0.02 {
            onsets.push(i);
            i += 2_000;
        } else {
            i += 1;
        }
    }
    let spacings: Vec<i64> = onsets.windows(2).map(|w| w[1] as i64 - w[0] as i64).collect();
    assert!(spacings.len() >= 3 && spacings.iter().any(|s| (s - 4_800).abs() > 10), "wow modulates the repeat spacing: {spacings:?}");
    assert!(matches!(p.tail(), rawdawg_dsp::processor::Tail::Unknown));
}

#[test]
fn delay_presets_cover_every_mode_and_process_is_allocation_free() {
    let presets = rawdawg_effects::presets(delay::TYPE_ID);
    let mut modes = std::collections::HashSet::new();
    let mut states = Vec::new();
    for pr in presets {
        let bytes = rawdawg_effects::preset_state(delay::TYPE_ID, pr).unwrap();
        let mut d = prepared();
        d.load_state(&bytes).unwrap();
        modes.insert(d.as_any_mut().downcast_mut::<Delay>().unwrap().mode());
        states.push(bytes);
    }
    assert_eq!(modes.len(), 5, "a preset for each mode");
    for i in 0..states.len() {
        for j in i + 1..states.len() {
            assert_ne!(states[i], states[j]);
        }
    }
    for mode in 0..5 {
        let mut p = prepared();
        set(p.as_mut(), delay::P_MODE, mode as f64);
        set(p.as_mut(), delay::P_TIME, 150.0);
        let x = burst(4096, 0.5);
        run_t(p.as_mut(), &x, &x, 120.0);
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
        assert_eq!(allocs, 0, "mode {mode} allocates");
    }
}

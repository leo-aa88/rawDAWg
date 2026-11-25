//! T-FX distortion: harmonic generation per mode, bitcrush levels and rate, oversampling
//! reduces aliasing, DC blocking, per-band null and band isolation, presets, RT safety.

use rawdawg_dsp::analysis::magnitude_spectrum;
use rawdawg_dsp::math::{peak, rms};
use rawdawg_dsp::processor::{PrepareSpec, ProcessContext, Processor, TransportView};
use rawdawg_dsp::{AudioBlock, BusLayout};
use rawdawg_effects::distortion::{multiband, single};
use rawdawg_rt::alloc_guard::{GuardAllocator, measure_rt};
use rawdawg_types::ParamId;

#[global_allocator]
static GLOBAL: GuardAllocator = GuardAllocator;

const SR: f64 = 48_000.0;

fn run(p: &mut dyn Processor, x: &[f32]) -> Vec<f32> {
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
    out_l
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

/// Magnitude (dB) of the spectrum at `hz` relative to the bin maximum, over a 8192-sample window.
fn level_at(buf: &[f32], hz: f64) -> f32 {
    let spec = magnitude_spectrum(&buf[..8192]);
    let bin = (hz / SR * 8192.0).round() as usize;
    let v = spec[bin.saturating_sub(1)..=bin + 1].iter().fold(0.0f32, |m, x| m.max(*x));
    20.0 * v.max(1e-9).log10()
}

fn neutral(p: &mut dyn Processor, mode: f64, drive_db: f64) {
    set(p, single::P_MODE, mode);
    set(p, single::P_DRIVE, drive_db);
    set(p, single::P_BIAS, 0.0);
    set(p, single::P_TONE, 20_000.0);
    set(p, single::P_OUTPUT, 0.0);
    set(p, single::P_MIX, 100.0);
    set(p, single::P_OVERSAMPLE, 2.0);
    p.reset();
}

#[test]
fn clip_modes_add_odd_harmonics_and_tube_bias_adds_even_ones() {
    let x = sine(1000.0, 24_000, 0.5);
    // hard clip at 20 dB drive: strong 3rd harmonic
    let mut hard = prepared(single::TYPE_ID);
    neutral(hard.as_mut(), 3.0, 20.0);
    let y = run(hard.as_mut(), &x);
    let (f1, f3, f2) = (level_at(&y[8_000..], 1000.0), level_at(&y[8_000..], 3000.0), level_at(&y[8_000..], 2000.0));
    assert!(f1 - f3 < 20.0, "hard clip: 3rd harmonic strong ({f1} vs {f3})");
    assert!(f1 - f2 > 40.0, "symmetric clip: no even harmonics ({f1} vs {f2})");
    assert!(peak(&y) <= 1.2, "band-limited (oversampled) hard clip overshoots only slightly: {}", peak(&y));
    let mut exact = prepared(single::TYPE_ID);
    neutral(exact.as_mut(), 3.0, 20.0);
    set(exact.as_mut(), single::P_OVERSAMPLE, 0.0);
    exact.reset();
    let ye = run(exact.as_mut(), &x);
    // the clip itself is exact; the 10 Hz DC blocker tilts the flat tops of a 1 kHz square by ~3 %
    assert!(peak(&ye) <= 1.035, "1× hard clip stays within the DC-blocker tilt: {}", peak(&ye));
    // tube with bias: even harmonics appear, DC is blocked
    let mut tube = prepared(single::TYPE_ID);
    neutral(tube.as_mut(), 0.0, 12.0);
    set(tube.as_mut(), single::P_BIAS, 0.8);
    tube.reset();
    let y = run(tube.as_mut(), &x);
    let (f1, f2) = (level_at(&y[8_000..], 1000.0), level_at(&y[8_000..], 2000.0));
    assert!(f1 - f2 < 30.0, "tube bias: 2nd harmonic present ({f1} vs {f2})");
    let dc = y[8_000..].iter().sum::<f32>() / 16_000.0;
    assert!(dc.abs() < 0.01, "DC blocked: {dc}");
    // mix 0 → dry null (the oversampling filters are not in the dry path)
    set(tube.as_mut(), single::P_MIX, 0.0);
    tube.reset();
    let y = run(tube.as_mut(), &x);
    assert!(y.iter().zip(&x).skip(1_000).all(|(a, b)| (a - b).abs() < 1e-5));
    // every non-crush mode distorts something
    for mode in 0..5 {
        let mut p = prepared(single::TYPE_ID);
        neutral(p.as_mut(), mode as f64, 24.0);
        let y = run(p.as_mut(), &x);
        let thd_like = level_at(&y[8_000..], 1000.0) - level_at(&y[8_000..], 3000.0).max(level_at(&y[8_000..], 2000.0));
        assert!(thd_like < 40.0 && y.iter().all(|v| v.is_finite()), "mode {mode} adds harmonics: {thd_like}");
    }
}

#[test]
fn bitcrush_quantizes_levels_and_holds_samples() {
    let mut p = prepared(single::TYPE_ID);
    neutral(p.as_mut(), 5.0, 0.0);
    set(p.as_mut(), single::P_BITS, 0.0); // 2 bits → 4 levels on the full scale (levels at ±0.5, ±1 → values -1, -0.5, 0, 0.5, 1 after rounding)
    set(p.as_mut(), single::P_RATE_DIV, 0.0);
    p.reset();
    let x = sine(200.0, 9_600, 0.95);
    let y = run(p.as_mut(), &x);
    let mut levels: Vec<i32> = y[1_000..].iter().map(|v| (v * 1000.0).round() as i32).collect();
    levels.sort_unstable();
    levels.dedup();
    assert!(levels.len() <= 5, "2-bit: at most five output levels, got {levels:?}");
    // rate reduction by 8 holds each value for 8 samples
    set(p.as_mut(), single::P_BITS, 14.0);
    set(p.as_mut(), single::P_RATE_DIV, 7.0);
    p.reset();
    let x = sine(1000.0, 4_096, 0.9);
    let y = run(p.as_mut(), &x);
    let mut runs = 0;
    let mut i = 512;
    while i + 8 < 4_000 {
        if y[i..i + 8].iter().all(|v| (v - y[i]).abs() < 1e-6) {
            runs += 1;
        }
        i += 8;
    }
    assert!(runs > 400, "sample-and-hold runs of 8: {runs}");
    assert_eq!(p.latency_frames(), 0, "bitcrush adds no latency");
}

#[test]
fn oversampling_reduces_aliasing_of_a_clipped_high_tone() {
    // 7 kHz hard-clipped: harmonics at 21 kHz fold to 27 kHz → aliased to 21 k…; check energy at a non-harmonic bin (e.g. 6 kHz = 48−42 alias of the 6th harmonic)
    let x = sine(7_000.0, 24_000, 0.7);
    let alias_db = |os: f64| {
        let mut p = prepared(single::TYPE_ID);
        neutral(p.as_mut(), 3.0, 24.0);
        set(p.as_mut(), single::P_OVERSAMPLE, os);
        p.reset();
        let y = run(p.as_mut(), &x);
        // 5th harmonic 35 kHz aliases to 13 kHz at 1×; measure there
        level_at(&y[8_000..], 13_000.0) - level_at(&y[8_000..], 7_000.0)
    };
    let (a1, a4) = (alias_db(0.0), alias_db(2.0));
    assert!(a4 < a1 - 12.0, "4× oversampling lowers the aliased component: 1× {a1} dB, 4× {a4} dB");
    let mut p = prepared(single::TYPE_ID);
    neutral(p.as_mut(), 3.0, 24.0);
    assert!(p.latency_frames() > 0, "oversampling reports its filter delay");
}

#[test]
fn multiband_is_a_null_without_drive_and_distorts_only_the_driven_band() {
    let mut m = prepared(multiband::TYPE_ID);
    set(m.as_mut(), multiband::P_OUTPUT, 0.0);
    set(m.as_mut(), multiband::P_MIX, 100.0);
    for b in 0..multiband::BANDS {
        set(m.as_mut(), multiband::band_param(b, multiband::B_DRIVE), 0.0);
        set(m.as_mut(), multiband::band_param(b, multiband::B_GAIN), 0.0);
    }
    m.reset();
    // recombination at unity: tones across the spectrum keep their level within 0.5 dB
    for hz in [60.0, 300.0, 1000.0, 3000.0, 9000.0] {
        let x = sine(hz, 24_000, 0.3);
        let y = run(m.as_mut(), &x);
        let g = 20.0 * (rms(&y[12_000..]) / rms(&x[12_000..])).log10();
        assert!(g.abs() < 0.5, "{hz} Hz recombines at unity: {g} dB");
    }
    // drive only the high band: a 100 Hz tone stays clean, a 6 kHz tone gets harmonics
    set(m.as_mut(), multiband::P_MODE, 3.0);
    set(m.as_mut(), multiband::band_param(3, multiband::B_DRIVE), 24.0);
    m.reset();
    let lo = run(m.as_mut(), &sine(100.0, 24_000, 0.4));
    let f3 = level_at(&lo[8_000..], 300.0) - level_at(&lo[8_000..], 100.0);
    assert!(f3 < -40.0, "low band untouched: 3rd harmonic at {f3} dB");
    let hi = run(m.as_mut(), &sine(6_000.0, 24_000, 0.4));
    let h3 = level_at(&hi[8_000..], 18_000.0) - level_at(&hi[8_000..], 6_000.0);
    assert!(h3 > -25.0, "high band driven: 3rd harmonic at {h3} dB");
}

#[test]
fn distortion_presets_differ_and_processing_is_allocation_free() {
    for type_id in [single::TYPE_ID, multiband::TYPE_ID] {
        let presets = rawdawg_effects::presets(type_id);
        assert!(presets.len() >= 3);
        let states: Vec<Vec<u8>> = presets.iter().map(|p| rawdawg_effects::preset_state(type_id, p).unwrap()).collect();
        for i in 0..states.len() {
            for j in i + 1..states.len() {
                assert_ne!(states[i], states[j]);
            }
        }
        let x = sine(330.0, 4096, 0.5);
        for mode in 0..5 {
            let mut p = prepared(type_id);
            if type_id == single::TYPE_ID {
                neutral(p.as_mut(), mode as f64, 12.0);
            } else {
                set(p.as_mut(), multiband::P_MODE, mode as f64);
                set(p.as_mut(), multiband::band_param(1, multiband::B_DRIVE), 12.0);
            }
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
                }
            });
            assert_eq!(allocs, 0, "{type_id} mode {mode} allocates");
        }
    }
}

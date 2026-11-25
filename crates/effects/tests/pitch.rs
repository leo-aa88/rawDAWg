//! T-FX pitch/formant: pitch shift within ±10 cents on steady tones, duration preserved,
//! unity when idle (latency exact), formant shift measured on a synthetic vowel, formant
//! preservation, presets, RT safety.

use rawdawg_dsp::analysis::{fundamental_frequency, magnitude_spectrum};
use rawdawg_dsp::biquad::{Biquad, BiquadKind, Coeffs};
use rawdawg_dsp::math::peak;
use rawdawg_dsp::processor::{PrepareSpec, ProcessContext, Processor, TransportView};
use rawdawg_dsp::{AudioBlock, BusLayout};
use rawdawg_effects::pitch::{self, PitchShift};
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

fn prepared() -> Box<dyn Processor> {
    let mut p = rawdawg_effects::make_effect(pitch::TYPE_ID, None).unwrap();
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

fn cents(a: f64, b: f64) -> f64 {
    1200.0 * (a / b).log2()
}

/// Impulse train at `f0` through two resonators: a synthetic vowel with formants at 700 and 1 200 Hz.
fn vowel(f0: f64, n: usize) -> Vec<f32> {
    let period = SR / f0;
    let mut f1 = Biquad::new();
    let mut f2 = Biquad::new();
    f1.coeffs = Coeffs::design(BiquadKind::BandPass, 700.0, 10.0, 0.0, SR as f32);
    f2.coeffs = Coeffs::design(BiquadKind::BandPass, 1200.0, 10.0, 0.0, SR as f32);
    let mut next = 0.0f64;
    (0..n)
        .map(|i| {
            let x = if i as f64 >= next {
                next += period;
                1.0
            } else {
                0.0
            };
            (f1.process(x) + 0.6 * f2.process(x)) * 0.8
        })
        .collect()
}

/// Every harmonic `k·f0` for k in `ks` stands at least 20 dB above the half-way bins
/// around it: the harmonic spacing is f0, i.e. the pitch is f0.
fn harmonics_present(buf: &[f32], f0: f64, ks: std::ops::RangeInclusive<usize>) -> bool {
    let spec = magnitude_spectrum(&buf[..16_384]);
    let bin = |hz: f64| (hz / SR * 16_384.0).round() as usize;
    ks.into_iter().all(|k| {
        let h = spec[bin(k as f64 * f0)];
        let between = spec[bin((k as f64 + 0.5) * f0)].max(spec[bin((k as f64 - 0.5) * f0)]);
        h > between * 10.0
    })
}

/// Level (dB) of the strongest of the two harmonics of `f0` bracketing `hz`.
fn region_level(buf: &[f32], f0: f64, hz: f64) -> f64 {
    let spec = magnitude_spectrum(&buf[..16_384]);
    let bin = |k: f64| (k * f0 / SR * 16_384.0).round() as usize;
    let k = (hz / f0).floor();
    let m = spec[bin(k)].max(spec[bin(k + 1.0)]);
    20.0 * (m as f64).max(1e-12).log10()
}

/// Harmonic index (k·f0) with the strongest magnitude between 300 Hz and 3 kHz.
fn strongest_harmonic(buf: &[f32], f0: f64) -> usize {
    let spec = magnitude_spectrum(&buf[..16_384]);
    let bin = |hz: f64| (hz / SR * 16_384.0).round() as usize;
    (3..=27).max_by(|a, b| spec[bin(*a as f64 * f0)].partial_cmp(&spec[bin(*b as f64 * f0)]).unwrap()).unwrap()
}

#[test]
fn pitch_shift_lands_within_ten_cents_on_steady_tones() {
    for (semis, cent, expect) in [
        (7.0, 0.0, 440.0 * 2f64.powf(7.0 / 12.0)),
        (-12.0, 0.0, 220.0),
        (12.0, 0.0, 880.0),
        (0.0, 50.0, 440.0 * 2f64.powf(50.0 / 1200.0)),
        (3.0, -30.0, 440.0 * 2f64.powf(2.7 / 12.0)),
    ] {
        let mut p = prepared();
        set(p.as_mut(), pitch::P_PITCH, semis);
        set(p.as_mut(), pitch::P_CENTS, cent);
        p.reset();
        let x = sine(440.0, 48_000, 0.5);
        let y = run(p.as_mut(), &x);
        let f = fundamental_frequency(&y[16_000..16_000 + 16_384], SR, 0.5);
        assert!(cents(f, expect).abs() < 10.0, "{semis} st {cent} ct: expected {expect:.2} Hz, measured {f:.2} Hz ({:.1} cents)", cents(f, expect));
        assert!(y.iter().all(|v| v.is_finite()) && peak(&y) <= 0.75, "bounded: {}", peak(&y));
        let steady = &y[16_000..];
        let (lo, hi) = (rawdawg_dsp::math::rms(&steady[..8000]), rawdawg_dsp::math::rms(&steady[steady.len() - 8000..]));
        assert!((lo / hi - 1.0).abs() < 0.2, "steady level: {lo} vs {hi}");
    }
}

#[test]
fn pitch_shift_preserves_duration_and_idle_settings_are_unity_after_the_reported_latency() {
    // a 600 ms burst stays 600 ms long after shifting by a fifth
    let mut x = vec![0.0f32; 96_000];
    for (i, v) in sine(440.0, 28_800, 0.5).into_iter().enumerate() {
        x[4800 + i] = v;
    }
    let mut p = prepared();
    set(p.as_mut(), pitch::P_PITCH, 7.0);
    p.reset();
    let y = run(p.as_mut(), &x);
    let latency = p.latency_frames() as usize;
    let active: Vec<usize> = y.iter().enumerate().filter(|(_, v)| v.abs() > 0.05).map(|(i, _)| i).collect();
    let (first, last) = (active[0], *active.last().unwrap());
    let len_ms = (last - first) as f64 / SR * 1000.0;
    assert!((len_ms - 600.0).abs() < 40.0, "burst length {len_ms:.1} ms");
    assert!((first as i64 - (4800 + latency) as i64).abs() < 2400, "burst starts after the reported latency: {first} vs {}", 4800 + latency);
    // pitch 0, formant 0: the output is the input delayed by exactly the reported latency
    let mut p = prepared();
    p.reset();
    let mut seed = 0x9e37_79b9u32;
    let noise: Vec<f32> = (0..48_000)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed as f32 / u32::MAX as f32 - 0.5) * 0.8
        })
        .collect();
    let y = run(p.as_mut(), &noise);
    let latency = p.latency_frames() as usize;
    assert!(latency > 0);
    let err = (8000..40_000).map(|i| (y[i + latency] - noise[i]).abs()).fold(0.0f32, f32::max);
    assert!(err < 2e-3, "unity within {err} after {latency} frames");
}

#[test]
fn formant_shift_moves_the_vowel_resonance_but_not_the_pitch_and_preserve_keeps_it() {
    let x = vowel(110.0, 96_000);
    let base = strongest_harmonic(&x[40_000..], 110.0);
    assert!((6..=7).contains(&base), "synthetic vowel peaks near 700 Hz: harmonic {base}");
    // the first formant (700 Hz) lands at 700·ratio: that region gets louder than it was in the
    // input and the 700 Hz region gets quieter (cases chosen so the second formant does not
    // move into either region)
    for st in [-5.0, 7.0] {
        let mut p = prepared();
        set(p.as_mut(), pitch::P_FORMANT, st);
        p.reset();
        let y = run(p.as_mut(), &x);
        let target = 700.0 * 2f64.powf(st / 12.0);
        let arrival = region_level(&y[40_000..], 110.0, target) - region_level(&x[40_000..], 110.0, target);
        let departure = region_level(&y[40_000..], 110.0, 700.0) - region_level(&x[40_000..], 110.0, 700.0);
        assert!(arrival > 4.0, "formant {st:+} st: {target:.0} Hz region rose by {arrival:.1} dB");
        assert!(departure < -3.0, "formant {st:+} st: 700 Hz region fell by {departure:.1} dB");
        assert!(harmonics_present(&y[40_000..], 110.0, 5..=12), "formant {st:+} st keeps the pitch: harmonics every 110 Hz");
        assert!(y.iter().all(|v| v.is_finite()));
    }
    // an octave either way: the first formant's new region gains energy (the second formant
    // moves into the old region at −12 st, so only the arrival is judged here)
    for st in [-12.0, 12.0] {
        let mut p = prepared();
        set(p.as_mut(), pitch::P_FORMANT, st);
        p.reset();
        let y = run(p.as_mut(), &x);
        let target = 700.0 * 2f64.powf(st / 12.0);
        let arrival = region_level(&y[40_000..], 110.0, target) - region_level(&x[40_000..], 110.0, target);
        assert!(arrival > 3.0, "formant {st:+} st: {target:.0} Hz region rose by {arrival:.1} dB");
    }
    // pitch +12 without preservation doubles the resonance frequency as well
    let mut p = prepared();
    set(p.as_mut(), pitch::P_PITCH, 12.0);
    p.reset();
    let y = run(p.as_mut(), &x);
    let plain = strongest_harmonic(&y[40_000..], 220.0);
    assert!((6..=7).contains(&plain), "octave up moves the formant: harmonic {plain} of 220 Hz");
    // with preservation the resonance stays near 700 Hz (harmonic 3 of 220 Hz)
    let mut p = prepared();
    set(p.as_mut(), pitch::P_PITCH, 12.0);
    set(p.as_mut(), pitch::P_PRESERVE, 1.0);
    p.reset();
    let y = run(p.as_mut(), &x);
    let kept = strongest_harmonic(&y[40_000..], 220.0);
    assert!((3..=4).contains(&kept), "preserve keeps the formant: harmonic {kept} of 220 Hz");
    assert!(harmonics_present(&y[40_000..], 220.0, 2..=6), "pitch doubled: harmonics every 220 Hz");
    assert!(!harmonics_present(&y[40_000..], 110.0, 5..=12), "no energy on the odd multiples of 110 Hz");
}

#[test]
fn pitch_presets_differ_and_processing_is_allocation_free() {
    let presets = rawdawg_effects::presets(pitch::TYPE_ID);
    assert!(presets.len() >= 5);
    let states: Vec<Vec<u8>> = presets.iter().map(|p| rawdawg_effects::preset_state(pitch::TYPE_ID, p).unwrap()).collect();
    for i in 0..states.len() {
        for j in i + 1..states.len() {
            assert_ne!(states[i], states[j]);
        }
    }
    let x = sine(330.0, 4096, 0.5);
    for preset in presets {
        let mut p = rawdawg_effects::make_effect(pitch::TYPE_ID, rawdawg_effects::preset_state(pitch::TYPE_ID, preset).as_deref()).unwrap();
        p.prepare(&PrepareSpec::new(SR, 256, BusLayout::Stereo)).unwrap();
        let y = run(p.as_mut(), &x);
        assert!(y.iter().all(|v| v.is_finite()), "{}", preset.name);
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
    let p = PitchShift::new();
    assert!(p.pitch_ratio() > 0.0);
}

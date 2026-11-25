//! T-FX spectral: null at neutral settings after exactly the reported latency, freeze
//! sustains the captured tone, frequency shift lands on the quantised bin, spectral gate
//! removes the noise floor, tilt slope, blur smears a click, presets, RT safety.

use rawdawg_dsp::analysis::{dominant_frequency, magnitude_spectrum};
use rawdawg_dsp::math::{peak, rms};
use rawdawg_dsp::processor::{PrepareSpec, ProcessContext, Processor, TransportView};
use rawdawg_dsp::{AudioBlock, BusLayout};
use rawdawg_effects::spectral::{self, Spectral};
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
    let mut p = rawdawg_effects::make_effect(spectral::TYPE_ID, None).unwrap();
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

fn noise(n: usize, amp: f32) -> Vec<f32> {
    let mut seed = 0x1234_5678u32;
    (0..n)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed as f32 / u32::MAX as f32 - 0.5) * 2.0 * amp
        })
        .collect()
}

fn level_at(buf: &[f32], hz: f64) -> f32 {
    let spec = magnitude_spectrum(&buf[..16_384]);
    let bin = (hz / SR * 16_384.0).round() as usize;
    let v = spec[bin - 2..=bin + 2].iter().fold(0.0f32, |m, x| m.max(*x));
    20.0 * v.max(1e-9).log10()
}

#[test]
fn neutral_settings_are_a_null_after_exactly_the_reported_latency() {
    let mut p = prepared();
    p.reset();
    let x = noise(48_000, 0.4);
    let y = run(p.as_mut(), &x);
    let latency = p.latency_frames() as usize;
    assert_eq!(latency, spectral::FRAME as u32 as usize);
    let err = (4000..40_000).map(|i| (y[i + latency] - x[i]).abs()).fold(0.0f32, f32::max);
    assert!(err < 1e-4, "null within {err}");
}

#[test]
fn freeze_sustains_the_captured_tone_after_the_input_stops() {
    let mut p = prepared();
    p.reset();
    let mut x = sine(440.0, 48_000, 0.5);
    x.extend(vec![0.0; 72_000]);
    // feed 0.75 s of tone, then switch freeze on while the tone still plays
    let (a, b) = x.split_at(36_000);
    let mut y = run(p.as_mut(), a);
    set(p.as_mut(), spectral::P_FREEZE, 1.0);
    y.extend(run(p.as_mut(), b));
    let tail = &y[90_000..];
    assert!(rms(tail) > 0.15, "frozen tone keeps sounding: rms {}", rms(tail));
    let f = dominant_frequency(&tail[..16_384], SR);
    assert!((f - 440.0).abs() < 10.0, "frozen pitch {f:.1} Hz");
    // without freeze the output is silent after the latency + frame
    let mut p = prepared();
    p.reset();
    let y = run(p.as_mut(), &x);
    assert!(rms(&y[100_000..]) < 1e-4, "unfrozen output decays: {}", rms(&y[100_000..]));
}

#[test]
fn frequency_shift_moves_a_tone_by_the_quantised_amount_and_stays_clean() {
    let mut p = prepared();
    set(p.as_mut(), spectral::P_SHIFT, 500.0);
    p.reset();
    let x = sine(440.0, 72_000, 0.5);
    let y = run(p.as_mut(), &x);
    let quantised = {
        let m = p.as_any_mut().downcast_mut::<Spectral>().unwrap();
        m.shift_hz() as f64
    };
    assert!((quantised - 500.0).abs() <= SR / spectral::FRAME as f64 / 2.0 + 1e-3, "shift quantised to the bin width: {quantised}");
    let f = dominant_frequency(&y[40_000..40_000 + 16_384], SR);
    assert!((f - (440.0 + quantised)).abs() < 3.0, "shifted tone at {f:.1} Hz, expected {:.1}", 440.0 + quantised);
    // the original tone is gone and the level is kept
    assert!(level_at(&y[40_000..], 440.0 + quantised) - level_at(&y[40_000..], 440.0) > 40.0, "original removed");
    assert!((rms(&y[40_000..]) / rms(&x[40_000..]) - 1.0).abs() < 0.15, "level kept");
}

#[test]
fn spectral_gate_removes_the_noise_floor_and_tilt_slopes_the_spectrum() {
    // tone at -6 dB over noise at -40 dB: a -20 dB gate leaves the tone and kills the noise
    let tone = sine(1000.0, 72_000, 0.5);
    let n = noise(72_000, 0.01);
    let x: Vec<f32> = tone.iter().zip(&n).map(|(a, b)| a + b).collect();
    let mut p = prepared();
    set(p.as_mut(), spectral::P_THRESHOLD, -20.0);
    p.reset();
    let y = run(p.as_mut(), &x);
    let floor_in = level_at(&x[40_000..], 3000.0);
    let floor_out = level_at(&y[40_000..], 3000.0);
    assert!(floor_in - floor_out > 25.0, "noise floor gated: {floor_in:.1} → {floor_out:.1} dB");
    assert!((level_at(&y[40_000..], 1000.0) - level_at(&x[40_000..], 1000.0)).abs() < 1.0, "tone passes");
    // +6 dB/oct tilt: 4 kHz vs 250 Hz differ by 4 octaves × 6 dB = 24 dB
    let two = {
        let a = sine(250.0, 72_000, 0.3);
        let b = sine(4000.0, 72_000, 0.3);
        a.iter().zip(&b).map(|(x, y)| x + y).collect::<Vec<_>>()
    };
    let mut p = prepared();
    set(p.as_mut(), spectral::P_TILT, 6.0);
    p.reset();
    let y = run(p.as_mut(), &two);
    let d_in = level_at(&two[40_000..], 4000.0) - level_at(&two[40_000..], 250.0);
    let d_out = level_at(&y[40_000..], 4000.0) - level_at(&y[40_000..], 250.0);
    assert!((d_out - d_in - 24.0).abs() < 1.5, "tilt: {:.1} dB over four octaves", d_out - d_in);
}

#[test]
fn blur_smears_a_click_over_time_and_robot_output_is_finite() {
    let mut x = vec![0.0f32; 72_000];
    for v in x.iter_mut().skip(24_000).take(48) {
        *v = 0.8;
    }
    let spread = |y: &[f32]| {
        let active: Vec<usize> = y.iter().enumerate().filter(|(_, v)| v.abs() > 0.01).map(|(i, _)| i).collect();
        (active.last().copied().unwrap_or(0) as i64 - active.first().copied().unwrap_or(0) as i64) as f64 / SR * 1000.0
    };
    let mut p = prepared();
    p.reset();
    let plain = spread(&run(p.as_mut(), &x));
    let mut p = prepared();
    set(p.as_mut(), spectral::P_BLUR, 90.0);
    p.reset();
    let blurred = spread(&run(p.as_mut(), &x));
    assert!(plain < 60.0, "plain click spans {plain:.1} ms");
    assert!(blurred > 200.0, "blurred click spans {blurred:.1} ms");
    let mut p = prepared();
    set(p.as_mut(), spectral::P_ROBOT, 1.0);
    p.reset();
    let y = run(p.as_mut(), &sine(330.0, 48_000, 0.5));
    assert!(y.iter().all(|v| v.is_finite()) && peak(&y) < 2.0 && rms(&y[8000..]) > 0.05);
}

#[test]
fn spectral_presets_differ_and_processing_is_allocation_free() {
    let presets = rawdawg_effects::presets(spectral::TYPE_ID);
    assert!(presets.len() >= 4);
    let states: Vec<Vec<u8>> = presets.iter().map(|p| rawdawg_effects::preset_state(spectral::TYPE_ID, p).unwrap()).collect();
    for i in 0..states.len() {
        for j in i + 1..states.len() {
            assert_ne!(states[i], states[j]);
        }
    }
    let x = sine(330.0, 4096, 0.5);
    for preset in presets {
        let mut p = rawdawg_effects::make_effect(spectral::TYPE_ID, rawdawg_effects::preset_state(spectral::TYPE_ID, preset).as_deref()).unwrap();
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
}

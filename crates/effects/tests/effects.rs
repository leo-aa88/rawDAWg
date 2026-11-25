//! T-FX subset: parametric EQ and stereo processor behaviour, state, presets, RT safety.

use rawdawg_dsp::math::{peak, rms};
use rawdawg_dsp::processor::{PrepareSpec, ProcessContext, Processor, TransportView};
use rawdawg_dsp::{AudioBlock, BusLayout};
use rawdawg_effects::{eq, stereo};
use rawdawg_rt::alloc_guard::{GuardAllocator, measure_rt};
use rawdawg_types::ParamId;
use rawdawg_types::events::{EventKind, EventSource, TimedEvent};

#[global_allocator]
static GLOBAL: GuardAllocator = GuardAllocator;

const SR: f64 = 48_000.0;

/// Run a stereo effect over `input` (both channels identical unless `right` given), block size 256.
fn run(p: &mut dyn Processor, left: &[f32], right: &[f32], events: &[TimedEvent]) -> (Vec<f32>, Vec<f32>) {
    let n = left.len();
    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];
    let transport = TransportView::stopped(SR);
    let mut pos = 0;
    let mut first = true;
    while pos < n {
        let frames = 256.min(n - pos);
        let inputs: [&[f32]; 2] = [&left[pos..pos + frames], &right[pos..pos + frames]];
        let (ol, orr) = (&mut out_l[pos..pos + frames], &mut out_r[pos..pos + frames]);
        let mut outputs: [&mut [f32]; 2] = [ol, orr];
        let ev: &[TimedEvent] = if first { events } else { &[] };
        let mut ctx = ProcessContext { audio: AudioBlock { frames, inputs: &inputs, outputs: &mut outputs, sidechain: &[] }, events: ev, transport: &transport };
        p.process(&mut ctx);
        first = false;
        pos += frames;
    }
    (out_l, out_r)
}

fn sine(freq: f64, n: usize, amp: f32) -> Vec<f32> {
    (0..n).map(|i| (std::f64::consts::TAU * freq * i as f64 / SR).sin() as f32 * amp).collect()
}

fn prepared(type_id: &str) -> Box<dyn Processor> {
    let mut p = rawdawg_effects::make_effect(type_id, None).unwrap();
    p.prepare(&PrepareSpec::new(SR, 256, BusLayout::Stereo)).unwrap();
    p
}

#[test]
fn eq_bell_boost_and_response_plot_agree_with_the_rendered_gain() {
    let mut p = prepared(eq::TYPE_ID);
    // band 1 (bell, 1 kHz by default): +9 dB, Q 2; disable the default high-pass
    let d = p.descriptors();
    let set = |p: &mut dyn Processor, id: u32, phys: f64| {
        let desc = d.iter().find(|x| x.id.0 == id).unwrap();
        assert!(p.set_param(ParamId(id), desc.to_normalized(phys)));
    };
    set(p.as_mut(), eq::band_param(0, eq::B_ENABLED), 0.0);
    set(p.as_mut(), eq::band_param(1, eq::B_GAIN), 9.0);
    set(p.as_mut(), eq::band_param(1, eq::B_Q), 2.0);
    set(p.as_mut(), eq::band_param(1, eq::B_FREQ), 1000.0);
    let x = sine(1000.0, 48_000, 0.25);
    let (l, r) = run(p.as_mut(), &x, &x, &[]);
    let gain_db = 20.0 * (peak(&l[24_000..]) / 0.25).log10();
    assert!((gain_db - 9.0).abs() < 0.3, "rendered bell gain {gain_db} dB");
    assert_eq!(l, r, "identical channels stay identical");
    let eq_ref = p.as_any_mut().downcast_mut::<eq::ParametricEq>().unwrap();
    assert!((eq_ref.response_db(1000.0) - 9.0).abs() < 0.1);
    assert!(eq_ref.response_db(100.0).abs() < 0.3, "far from the bell the plot is flat");
    // an off-band tone is untouched
    let x2 = sine(100.0, 24_000, 0.25);
    let (l2, _) = run(p.as_mut(), &x2, &x2, &[]);
    let g2 = 20.0 * (peak(&l2[12_000..]) / 0.25).log10();
    assert!(g2.abs() < 0.4, "{g2}");
}

#[test]
fn eq_filters_are_stable_under_random_parameters_and_flat_eq_is_a_null() {
    let mut p = prepared(eq::TYPE_ID);
    let d = p.descriptors();
    let mut rng = rawdawg_dsp::rng::DeterministicRng::new(0xE0, 1);
    let noise: Vec<f32> = (0..8192).map(|_| rng.bipolar_f32() * 0.5).collect();
    for _ in 0..200 {
        for desc in d {
            p.set_param(desc.id, rng.unit_f32());
        }
        let (l, _) = run(p.as_mut(), &noise, &noise, &[]);
        assert!(l.iter().all(|v| v.is_finite()), "non-finite output");
        assert!(peak(&l) < 1000.0, "runaway gain");
    }
    // flat: every band off, output 0 dB → bit-exact pass-through
    let mut p = prepared(eq::TYPE_ID);
    for b in 0..eq::BANDS {
        p.set_param(ParamId(eq::band_param(b, eq::B_ENABLED)), 0.0);
    }
    p.reset();
    let (l, _) = run(p.as_mut(), &noise, &noise, &[]);
    let err = l.iter().zip(&noise).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(err < 1e-6, "flat EQ is a null: {err}");
    assert_eq!(p.latency_frames(), 0);
}

#[test]
fn eq_params_arrive_as_events_state_round_trips_and_presets_differ() {
    let mut p = prepared(eq::TYPE_ID);
    let d = p.descriptors();
    let gain = d.iter().find(|x| x.id.0 == eq::band_param(1, eq::B_GAIN)).unwrap();
    let ev = TimedEvent::new(0, EventSource::Transport, 0, EventKind::Param { id: gain.id, value: gain.to_normalized(-12.0) });
    let x = sine(1000.0, 48_000, 0.25);
    let (l, _) = run(p.as_mut(), &x, &x, &[ev]);
    let g = 20.0 * (peak(&l[24_000..]) / 0.25).log10();
    assert!((g + 12.0).abs() < 0.4, "{g}");
    let state = p.save_state();
    let mut q = prepared(eq::TYPE_ID);
    q.load_state(&state).unwrap();
    assert_eq!(q.param(gain.id), p.param(gain.id));
    assert!(q.load_state(b"{\"version\": 99, \"params\": {}}").is_err());
    assert!(q.load_state(b"garbage").is_err());
    // unknown keys ignored, missing keep defaults
    q.load_state(b"{\"version\": 1, \"params\": {\"nope\": 0.1}}").unwrap();
    assert_eq!(q.param(gain.id), Some(gain.default_normalized));
    let presets = rawdawg_effects::presets(eq::TYPE_ID);
    assert!(presets.len() >= 3);
    let states: Vec<Vec<u8>> = presets.iter().map(|pr| rawdawg_effects::preset_state(eq::TYPE_ID, pr).unwrap()).collect();
    for i in 0..states.len() {
        for j in i + 1..states.len() {
            assert_ne!(states[i], states[j], "presets {} and {} are identical", presets[i].name, presets[j].name);
        }
    }
    // telephone preset really narrows the band
    let tel = presets.iter().find(|p| p.name == "Telephone").unwrap();
    let mut t = prepared(eq::TYPE_ID);
    t.load_state(&rawdawg_effects::preset_state(eq::TYPE_ID, tel).unwrap()).unwrap();
    let lo = sine(60.0, 48_000, 0.25);
    let (l, _) = run(t.as_mut(), &lo, &lo, &[]);
    assert!(20.0 * (peak(&l[24_000..]) / 0.25).log10() < -20.0, "60 Hz is cut by the telephone preset");
}

#[test]
fn stereo_processor_width_mono_pan_and_correlation() {
    let mut p = prepared(stereo::TYPE_ID);
    let d = p.descriptors();
    let set = |p: &mut dyn Processor, id: u32, phys: f64| {
        let desc = d.iter().find(|x| x.id.0 == id).unwrap();
        p.set_param(ParamId(id), desc.to_normalized(phys));
    };
    // long enough for the 20 ms parameter smoothers to settle completely
    let l_in = sine(440.0, 48_000, 0.5);
    let r_in = sine(660.0, 48_000, 0.5);
    const SETTLED: usize = 24_000;
    // unity: pass-through
    let (l, r) = run(p.as_mut(), &l_in, &r_in, &[]);
    assert!(l.iter().zip(&l_in).skip(2000).all(|(a, b)| (a - b).abs() < 1e-5));
    assert!(r.iter().zip(&r_in).skip(2000).all(|(a, b)| (a - b).abs() < 1e-5));
    // width 0 → both channels equal the mid
    set(p.as_mut(), stereo::P_WIDTH, 0.0);
    let (l, r) = run(p.as_mut(), &l_in, &r_in, &[]);
    assert!(l.iter().zip(&r).skip(SETTLED).all(|(a, b)| (a - b).abs() < 1e-4), "width 0 is mono");
    let _ = run(p.as_mut(), &l_in, &r_in, &[]); // the 300 ms correlation window forgets the ramp
    assert!((p.meter(0).unwrap() - 1.0).abs() < 0.02, "correlation of a mono signal is +1: {:?}", p.meter(0));
    // width 200 % doubles the side signal
    set(p.as_mut(), stereo::P_WIDTH, 200.0);
    let (l, r) = run(p.as_mut(), &l_in, &r_in, &[]);
    let side_in: Vec<f32> = l_in.iter().zip(&r_in).map(|(a, b)| 0.5 * (a - b)).collect();
    let side_out: Vec<f32> = l.iter().zip(&r).map(|(a, b)| 0.5 * (a - b)).collect();
    let ratio = rms(&side_out[SETTLED..]) / rms(&side_in[SETTLED..]);
    assert!((ratio - 2.0).abs() < 0.05, "{ratio}");
    // invert R of a mono signal → correlation −1
    set(p.as_mut(), stereo::P_WIDTH, 100.0);
    set(p.as_mut(), stereo::P_PHASE_R, 1.0);
    let _ = run(p.as_mut(), &l_in, &l_in, &[]);
    let _ = run(p.as_mut(), &l_in, &l_in, &[]);
    assert!((p.meter(0).unwrap() + 1.0).abs() < 0.05, "{:?}", p.meter(0));
    set(p.as_mut(), stereo::P_PHASE_R, 0.0);
    // pan hard left on a mono source: right is silent, left carries +3 dB (equal-power law)
    set(p.as_mut(), stereo::P_PAN, -1.0);
    let (l, r) = run(p.as_mut(), &l_in, &l_in, &[]);
    assert!(peak(&r[SETTLED..]) < 1e-3);
    let g = 20.0 * (peak(&l[SETTLED..]) / 0.5).log10();
    assert!((g - 3.01).abs() < 0.1, "{g}");
    // mono switch
    set(p.as_mut(), stereo::P_PAN, 0.0);
    set(p.as_mut(), stereo::P_MONO, 1.0);
    let (l, r) = run(p.as_mut(), &l_in, &r_in, &[]);
    assert!(l.iter().zip(&r).skip(SETTLED).all(|(a, b)| (a - b).abs() < 1e-4));
}

#[test]
fn effects_process_path_is_allocation_free() {
    for type_id in [eq::TYPE_ID, stereo::TYPE_ID] {
        let mut p = prepared(type_id);
        let x = sine(220.0, 4096, 0.3);
        run(p.as_mut(), &x, &x, &[]);
        let d = p.descriptors();
        let events: Vec<TimedEvent> = d.iter().take(6).map(|desc| TimedEvent::new(0, EventSource::Transport, 0, EventKind::Param { id: desc.id, value: 0.3 })).collect();
        let (_, allocs) = measure_rt(|| {
            for _ in 0..16 {
                let (l, r) = (x.as_slice(), x.as_slice());
                let mut out_l = [0.0f32; 256];
                let mut out_r = [0.0f32; 256];
                let inputs: [&[f32]; 2] = [&l[..256], &r[..256]];
                let mut outputs: [&mut [f32]; 2] = [&mut out_l, &mut out_r];
                let transport = TransportView::stopped(SR);
                let mut ctx = ProcessContext { audio: AudioBlock { frames: 256, inputs: &inputs, outputs: &mut outputs, sidechain: &[] }, events: &events, transport: &transport };
                p.process(&mut ctx);
                p.reset();
            }
        });
        assert_eq!(allocs, 0, "{type_id} allocates on the process path");
    }
}

#[test]
fn every_catalogue_entry_instantiates_and_every_effect_is_listed() {
    for e in rawdawg_effects::CATALOG.iter() {
        let p = rawdawg_effects::make_effect(e.type_id, None).unwrap_or_else(|err| panic!("{}: {err}", e.type_id));
        assert!(!p.descriptors().is_empty(), "{} has parameters", e.type_id);
        for preset in rawdawg_effects::presets(e.type_id) {
            assert!(rawdawg_effects::preset_state(e.type_id, preset).is_some(), "{}: preset {} serializes", e.type_id, preset.name);
        }
    }
    // every type with presets is in the catalogue (menus can reach it)
    for id in [
        rawdawg_effects::eq::TYPE_ID,
        rawdawg_effects::stereo::TYPE_ID,
        rawdawg_effects::delay::TYPE_ID,
        rawdawg_effects::modulation::chorus::TYPE_ID,
        rawdawg_effects::modulation::flanger::TYPE_ID,
        rawdawg_effects::modulation::phaser::TYPE_ID,
        rawdawg_effects::reverb::TYPE_ID,
        rawdawg_effects::convolution::TYPE_ID,
        rawdawg_effects::distortion::single::TYPE_ID,
        rawdawg_effects::distortion::multiband::TYPE_ID,
        rawdawg_effects::dynamics::compressor::TYPE_ID,
        rawdawg_effects::dynamics::limiter::TYPE_ID,
        rawdawg_effects::dynamics::gate::TYPE_ID,
    ] {
        assert!(rawdawg_effects::info(id).is_some_and(|i| i.listed), "{id} is listed in the catalogue");
    }
}

//! T-FX cross-cutting gate: every catalogue effect × every factory preset × impulse,
//! sweep, step and chord stimuli renders finite and bounded; random racks (chains of
//! randomly chosen effects with randomised parameters at full chaos) stay finite too.

use rawdawg_dsp::math::peak;
use rawdawg_dsp::processor::{PrepareSpec, ProcessContext, Processor, TransportView};
use rawdawg_dsp::rng::DeterministicRng;
use rawdawg_dsp::{AudioBlock, BusLayout};
use rawdawg_effects::rack::{RackLocks, randomize};
use rawdawg_types::ParamId;

const SR: f64 = 48_000.0;
const N: usize = 48_000;

fn run_chain(chain: &mut [Box<dyn Processor>], x: &[f32]) -> (Vec<f32>, Vec<f32>) {
    let mut cur_l = x.to_vec();
    let mut cur_r = x.to_vec();
    let transport = TransportView::stopped(SR);
    for p in chain.iter_mut() {
        let mut out_l = vec![0.0f32; x.len()];
        let mut out_r = vec![0.0f32; x.len()];
        let mut pos = 0;
        while pos < x.len() {
            let frames = 256.min(x.len() - pos);
            let inputs: [&[f32]; 2] = [&cur_l[pos..pos + frames], &cur_r[pos..pos + frames]];
            let (ol, orr) = (&mut out_l[pos..pos + frames], &mut out_r[pos..pos + frames]);
            let mut outputs: [&mut [f32]; 2] = [ol, orr];
            let mut ctx = ProcessContext { audio: AudioBlock { frames, inputs: &inputs, outputs: &mut outputs, sidechain: &[] }, events: &[], transport: &transport };
            p.process(&mut ctx);
            pos += frames;
        }
        cur_l = out_l;
        cur_r = out_r;
    }
    (cur_l, cur_r)
}

fn stimuli() -> Vec<(&'static str, Vec<f32>)> {
    let impulse = {
        let mut v = vec![0.0f32; N];
        v[1000] = 0.9;
        v
    };
    let sweep: Vec<f32> = (0..N)
        .map(|i| {
            let t = i as f64 / SR;
            let f = 20.0 * (1000.0f64).powf(t); // 20 Hz → 20 kHz over one second
            let phase = std::f64::consts::TAU * 20.0 * ((1000.0f64).powf(t) - 1.0) / (1000.0f64).ln();
            let _ = f;
            (phase.sin() * 0.5) as f32
        })
        .collect();
    let step: Vec<f32> = (0..N).map(|i| if (8000..28_000).contains(&i) { 0.7 } else { 0.0 }).collect();
    let chord: Vec<f32> = (0..N)
        .map(|i| {
            let t = i as f64 / SR;
            let env = if t < 0.6 { 1.0 } else { (-(t - 0.6) * 8.0).exp() };
            ([261.63, 329.63, 392.0, 493.88].iter().map(|f: &f64| (std::f64::consts::TAU * f * t).sin()).sum::<f64>() * 0.2 * env) as f32
        })
        .collect();
    vec![("impulse", impulse), ("sweep", sweep), ("step", step), ("chord", chord)]
}

#[test]
fn every_effect_preset_renders_every_stimulus_finite_and_bounded() {
    let stims = stimuli();
    let mut count = 0;
    for info in rawdawg_effects::CATALOG.iter().filter(|e| e.listed) {
        let presets = rawdawg_effects::presets(info.type_id);
        let states: Vec<Option<Vec<u8>>> = std::iter::once(None).chain(presets.iter().map(|p| rawdawg_effects::preset_state(info.type_id, p))).collect();
        for (si, state) in states.iter().enumerate() {
            for (name, x) in &stims {
                let mut p = rawdawg_effects::make_effect(info.type_id, state.as_deref()).unwrap();
                p.prepare(&PrepareSpec::new(SR, 256, BusLayout::Stereo)).unwrap();
                let mut chain = vec![p];
                let (l, r) = run_chain(&mut chain, x);
                let label = format!("{} preset #{si} on {name}", info.type_id);
                assert!(l.iter().chain(&r).all(|v| v.is_finite()), "{label}: nonfinite output");
                // bounded: no effect turns a sub-unity stimulus into more than +18 dB
                let pk = peak(&l).max(peak(&r));
                assert!(pk < 8.0, "{label}: peak {pk}");
                count += 1;
            }
        }
    }
    assert!(count > 200, "{count} renders");
}

#[test]
fn random_racks_at_full_chaos_stay_finite() {
    let listed: Vec<&str> = rawdawg_effects::CATALOG.iter().filter(|e| e.listed).map(|e| e.type_id).collect();
    let stims = stimuli();
    let locks = RackLocks::default();
    for seed in 0..12u64 {
        let mut rng = DeterministicRng::new(seed, 0xf00d);
        let len = 2 + (rng.next_u32() as usize % 3);
        let mut chain: Vec<Box<dyn Processor>> = Vec::new();
        let mut names = Vec::new();
        for _ in 0..len {
            let type_id = listed[rng.next_u32() as usize % listed.len()];
            let mut p = rawdawg_effects::make_effect(type_id, None).unwrap();
            p.prepare(&PrepareSpec::new(SR, 256, BusLayout::Stereo)).unwrap();
            let current: Vec<(ParamId, f32)> = p.descriptors().iter().filter_map(|d| p.param(d.id).map(|v| (d.id, v))).collect();
            for (id, v) in randomize(p.descriptors(), &current, &mut rng, 100.0, &locks) {
                p.set_param(id, v);
            }
            p.reset();
            names.push(type_id);
            chain.push(p);
        }
        let (name, x) = &stims[seed as usize % stims.len()];
        let (l, r) = run_chain(&mut chain, x);
        assert!(l.iter().chain(&r).all(|v| v.is_finite()), "rack {names:?} on {name}: nonfinite");
        let pk = peak(&l).max(peak(&r));
        assert!(pk < 16.0, "rack {names:?} on {name}: peak {pk}");
    }
}

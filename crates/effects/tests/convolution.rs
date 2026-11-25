//! T-FX convolution: exactness against direct convolution, latency, mapping,
//! resampling, trim/normalize, length cap, allocation-free processing.

use rawdawg_dsp::math::peak;
use rawdawg_dsp::processor::{PrepareSpec, ProcessContext, Processor, TransportView};
use rawdawg_dsp::{AudioBlock, BusLayout};
use rawdawg_effects::convolution::{self, Convolution, ImpulseResponse, PARTITION};
use rawdawg_rt::alloc_guard::{GuardAllocator, measure_rt};
use rawdawg_types::ParamId;
use std::sync::Arc;

#[global_allocator]
static GLOBAL: GuardAllocator = GuardAllocator;

const SR: f64 = 48_000.0;

fn run(p: &mut dyn Processor, left: &[f32], right: &[f32], block: usize) -> (Vec<f32>, Vec<f32>) {
    let n = left.len();
    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];
    let transport = TransportView::stopped(SR);
    let mut pos = 0;
    while pos < n {
        let frames = block.min(n - pos);
        let inputs: [&[f32]; 2] = [&left[pos..pos + frames], &right[pos..pos + frames]];
        let (ol, orr) = (&mut out_l[pos..pos + frames], &mut out_r[pos..pos + frames]);
        let mut outputs: [&mut [f32]; 2] = [ol, orr];
        let mut ctx = ProcessContext { audio: AudioBlock { frames, inputs: &inputs, outputs: &mut outputs, sidechain: &[] }, events: &[], transport: &transport };
        p.process(&mut ctx);
        pos += frames;
    }
    (out_l, out_r)
}

fn set(p: &mut dyn Processor, id: u32, phys: f64) {
    let d = p.descriptors().iter().find(|x| x.id.0 == id).copied().unwrap();
    assert!(p.set_param(ParamId(id), d.to_normalized(phys)));
}

fn conv(ir: ImpulseResponse, normalize: bool, trim: bool) -> Convolution {
    let mut c = Convolution::new();
    c.prepare(&PrepareSpec::new(SR, 256, BusLayout::Stereo)).unwrap();
    set(&mut c, convolution::P_MIX, 100.0);
    set(&mut c, convolution::P_NORMALIZE, if normalize { 1.0 } else { 0.0 });
    set(&mut c, convolution::P_TRIM, if trim { 1.0 } else { 0.0 });
    set(&mut c, convolution::P_LOWCUT, 20.0);
    set(&mut c, convolution::P_HIGHCUT, 20_000.0);
    c.reset();
    c.set_ir(Some(Arc::new(ir)));
    c
}

fn direct(x: &[f32], h: &[f32]) -> Vec<f32> {
    let mut y = vec![0.0f32; x.len()];
    for (i, yi) in y.iter_mut().enumerate() {
        let mut acc = 0.0;
        for (k, hk) in h.iter().enumerate() {
            if i >= k {
                acc += x[i - k] * hk;
            }
        }
        *yi = acc;
    }
    y
}

fn noise(n: usize, seed: u64) -> Vec<f32> {
    let mut rng = rawdawg_dsp::rng::DeterministicRng::new(seed, 1);
    (0..n).map(|_| rng.bipolar_f32() * 0.5).collect()
}

#[test]
fn partitioned_convolution_matches_direct_convolution_after_its_latency() {
    // an IR spanning several partitions with a decaying noise tail
    let h: Vec<f32> = noise(1_500, 7).iter().enumerate().map(|(i, v)| v * (-(i as f32) / 400.0).exp()).collect();
    let mut c = conv(ImpulseResponse { name: "ir".into(), sample_rate: 48_000, channels: vec![h.clone()] }, false, false);
    assert_eq!(c.latency_frames(), PARTITION as u32);
    let x = noise(6_000, 3);
    for block in [256usize, 100, 37, 512] {
        c.reset();
        let (l, r) = run(&mut c, &x, &x, block);
        let d = direct(&x, &h);
        let lat = PARTITION;
        let err = (0..x.len() - lat).map(|i| (l[i + lat] - d[i]).abs()).fold(0.0f32, f32::max);
        assert!(err < 1e-4, "block {block}: max error vs direct convolution {err}");
        assert_eq!(l, r, "mono IR feeds both channels");
    }
    // the filters at their extremes are transparent enough; mix 0 is dry (delayed by nothing)
    set(&mut c, convolution::P_MIX, 0.0);
    c.reset();
    let (l, _) = run(&mut c, &x, &x, 256);
    assert!(l.iter().zip(&x).skip(2_000).all(|(a, b)| (a - b).abs() < 1e-3));
}

#[test]
fn stereo_ir_maps_channels_and_predelay_gain_apply() {
    let hl = vec![1.0f32, 0.0, 0.0];
    let hr = vec![0.0f32, 0.0, 0.5];
    let mut c = conv(ImpulseResponse { name: "st".into(), sample_rate: 48_000, channels: vec![hl, hr] }, false, false);
    let mut x = vec![0.0f32; 2_000];
    x[100] = 1.0;
    let (l, r) = run(&mut c, &x, &x, 256);
    let lat = PARTITION;
    assert!((l[100 + lat] - 1.0).abs() < 1e-4 && l[102 + lat].abs() < 1e-4, "left IR on the left channel");
    assert!((r[102 + lat] - 0.5).abs() < 1e-4 && r[100 + lat].abs() < 1e-4, "right IR on the right channel");
    // pre-delay 10 ms and +6 dB
    set(&mut c, convolution::P_PREDELAY, 10.0);
    set(&mut c, convolution::P_GAIN, 6.0);
    c.reset();
    let (l, _) = run(&mut c, &x, &x, 256);
    let at = 100 + lat + 480;
    assert!((l[at] - 1.995).abs() < 0.02, "pre-delayed and boosted: {}", l[at]);
    assert!(c.info().is_some_and(|i| i.channels == 2 && !i.resampled && !i.truncated));
}

#[test]
fn resampling_trim_normalize_and_length_cap() {
    // 44.1 kHz IR: 441 samples → 480 at 48 kHz
    let ir = ImpulseResponse { name: "44k".into(), sample_rate: 44_100, channels: vec![vec![1.0f32; 441]] };
    let c = conv(ir, false, false);
    let info = c.info().unwrap().clone();
    assert!(info.resampled && (info.used_frames as i64 - 480).abs() <= 1, "{info:?}");
    // trim drops trailing silence; normalize makes the energy unity
    let mut h = vec![0.0f32; 4_000];
    h[0] = 2.0;
    h[1] = 2.0;
    let c = conv(ImpulseResponse { name: "t".into(), sample_rate: 48_000, channels: vec![h.clone()] }, true, true);
    let info = c.info().unwrap().clone();
    assert_eq!(info.used_frames, PARTITION, "trimmed down to one partition");
    let mut c2 = conv(ImpulseResponse { name: "t".into(), sample_rate: 48_000, channels: vec![h] }, true, false);
    let mut x = vec![0.0f32; 1_000];
    x[0] = 1.0;
    let (l, _) = run(&mut c2, &x, &x, 256);
    let e: f32 = l.iter().map(|v| v * v).sum();
    assert!((e - 1.0).abs() < 1e-3, "normalized IR has unity energy: {e}");
    // longer than 30 s is truncated and flagged
    let long = ImpulseResponse { name: "long".into(), sample_rate: 48_000, channels: vec![vec![0.001f32; 31 * 48_000]] };
    let c = conv(long, false, false);
    let info = c.info().unwrap().clone();
    assert!(info.truncated && info.used_frames == 30 * 48_000);
    // no IR: silent wet path, finite output
    let mut none = Convolution::new();
    none.prepare(&PrepareSpec::new(SR, 256, BusLayout::Stereo)).unwrap();
    set(&mut none, convolution::P_MIX, 100.0);
    none.reset();
    let (l, _) = run(&mut none, &x, &x, 256);
    assert!(peak(&l) < 1e-6 && !none.has_ir());
}

#[test]
fn state_keeps_the_asset_reference_and_process_is_allocation_free() {
    let mut c = conv(ImpulseResponse { name: "n".into(), sample_rate: 48_000, channels: vec![noise(20_000, 11)] }, true, true);
    c.set_ir_asset_id(Some("a".repeat(64)));
    let state = c.save_state();
    let mut d = Convolution::new();
    d.load_state(&state).unwrap();
    assert_eq!(d.ir_asset_id(), Some("a".repeat(64).as_str()));
    assert_eq!(d.param(ParamId(convolution::P_MIX)), c.param(ParamId(convolution::P_MIX)));
    let x = noise(4096, 5);
    run(&mut c, &x, &x, 256);
    let (_, allocs) = measure_rt(|| {
        for _ in 0..8 {
            let mut out_l = [0.0f32; 256];
            let mut out_r = [0.0f32; 256];
            let inputs: [&[f32]; 2] = [&x[..256], &x[..256]];
            let mut outputs: [&mut [f32]; 2] = [&mut out_l, &mut out_r];
            let transport = TransportView::stopped(SR);
            let mut ctx = ProcessContext { audio: AudioBlock { frames: 256, inputs: &inputs, outputs: &mut outputs, sidechain: &[] }, events: &[], transport: &transport };
            c.process(&mut ctx);
        }
    });
    assert_eq!(allocs, 0, "convolution allocates on the process path");
    assert!(rawdawg_effects::presets(convolution::TYPE_ID).len() >= 3);
}

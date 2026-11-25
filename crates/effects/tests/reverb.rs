//! T-FX reverb: RT60 per algorithm, density, decay shape, shimmer, stability, presets.

use rawdawg_dsp::math::{peak, rms};
use rawdawg_dsp::processor::{PrepareSpec, ProcessContext, Processor, TransportView};
use rawdawg_dsp::{AudioBlock, BusLayout};
use rawdawg_effects::reverb::{self, Reverb};
use rawdawg_rt::alloc_guard::{GuardAllocator, measure_rt};
use rawdawg_types::ParamId;

#[global_allocator]
static GLOBAL: GuardAllocator = GuardAllocator;

const SR: f64 = 48_000.0;

fn run(p: &mut dyn Processor, left: &[f32], right: &[f32]) -> (Vec<f32>, Vec<f32>) {
    let n = left.len();
    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];
    let transport = TransportView::stopped(SR);
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
    let mut p = rawdawg_effects::make_effect(reverb::TYPE_ID, None).unwrap();
    p.prepare(&PrepareSpec::new(SR, 256, BusLayout::Stereo)).unwrap();
    p
}

fn set(p: &mut dyn Processor, id: u32, phys: f64) {
    let d = p.descriptors().iter().find(|x| x.id.0 == id).copied().unwrap();
    assert!(p.set_param(ParamId(id), d.to_normalized(phys)), "param {id}");
}

/// Wet-only, neutral settings for measurement.
fn neutral(p: &mut dyn Processor, mode: f64, decay: f64) {
    set(p, reverb::P_MODE, mode);
    set(p, reverb::P_DECAY, decay);
    set(p, reverb::P_MIX, 100.0);
    set(p, reverb::P_DAMP, 20_000.0);
    set(p, reverb::P_LOWCUT, 20.0);
    set(p, reverb::P_PREDELAY, 0.0);
    set(p, reverb::P_MODULATION, 0.0);
    for b in 0..4 {
        set(p, reverb::P_SHAPE0 + b, 1.0);
    }
    p.reset();
}

fn impulse(n: usize) -> Vec<f32> {
    let mut x = vec![0.0f32; n];
    x[0] = 1.0;
    x
}

/// RT60 estimate from the Schroeder backward integration of the energy (−5 … −35 dB slope).
fn rt60(ir: &[f32]) -> f32 {
    let mut edc = vec![0.0f64; ir.len()];
    let mut acc = 0.0f64;
    for i in (0..ir.len()).rev() {
        acc += f64::from(ir[i]) * f64::from(ir[i]);
        edc[i] = acc;
    }
    let total = edc[0].max(1e-20);
    let db: Vec<f64> = edc.iter().map(|e| 10.0 * (e / total).max(1e-30).log10()).collect();
    let t5 = db.iter().position(|d| *d <= -5.0).unwrap_or(0);
    let t35 = db.iter().position(|d| *d <= -35.0).unwrap_or(db.len() - 1);
    // linear fit between the two points, extrapolated to 60 dB
    let slope = 30.0 / ((t35 - t5).max(1) as f64 / SR);
    (60.0 / slope) as f32
}

fn centroid(buf: &[f32]) -> f64 {
    let (_, c, _) = rawdawg_dsp::analysis::features(buf, SR);
    c
}

#[test]
fn room_hall_cathedral_decay_close_to_the_set_rt60_and_differ() {
    let mut tails = Vec::new();
    for (mode, rt) in [(0.0, 0.8), (1.0, 1.5), (2.0, 2.5)] {
        let mut p = prepared();
        neutral(p.as_mut(), mode, rt);
        let n = (rt * 3.0 * SR) as usize;
        let (l, _) = run(p.as_mut(), &impulse(n), &impulse(n));
        let measured = rt60(&l);
        assert!((measured / rt as f32 - 1.0).abs() < 0.35, "mode {mode}: set {rt} s, measured {measured} s");
        assert!(l.iter().all(|v| v.is_finite()));
        tails.push(l);
    }
    // the three algorithms are different programs, not renamed copies
    for i in 0..3 {
        for j in i + 1..3 {
            let n = 20_000;
            let diff = tails[i][..n].iter().zip(&tails[j][..n]).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            assert!(diff > 0.05, "modes {i} and {j} too similar");
        }
    }
}

/// Echo density (Abel–Huang): fraction of samples above the window's standard deviation;
/// ≈0.32 for a fully dense (Gaussian) tail, lower while echoes are still sparse.
fn echo_density(buf: &[f32]) -> f32 {
    let m = buf.iter().map(|v| v * v).sum::<f32>() / buf.len() as f32;
    let sd = m.sqrt().max(1e-12);
    buf.iter().filter(|v| v.abs() > sd).count() as f32 / buf.len() as f32
}

#[test]
fn plate_is_denser_than_room_and_spring_rings() {
    let mut room = prepared();
    neutral(room.as_mut(), 0.0, 1.5);
    let (lr, _) = run(room.as_mut(), &impulse(48_000), &impulse(48_000));
    let mut plate = prepared();
    neutral(plate.as_mut(), 3.0, 1.5);
    set(plate.as_mut(), reverb::P_DIFFUSION, 90.0);
    plate.reset();
    let (lp, _) = run(plate.as_mut(), &impulse(48_000), &impulse(48_000));
    let mut hall = prepared();
    neutral(hall.as_mut(), 1.0, 1.5);
    let (lh, _) = run(hall.as_mut(), &impulse(48_000), &impulse(48_000));
    // density builds fastest in the plate, then the room, slowest in the hall; all are dense by 100 ms
    let (w0, w1) = (480, 2_400); // 10–50 ms
    let (dp, dr, dh) = (echo_density(&lp[w0..w1]), echo_density(&lr[w0..w1]), echo_density(&lh[w0..w1]));
    assert!(dp > dr && dr > dh, "early echo density plate {dp} > room {dr} > hall {dh}");
    assert!(echo_density(&lp[4_800..9_600]) > 0.25 && echo_density(&lr[4_800..9_600]) > 0.25, "fully dense tails after 100 ms");
    let mut spring = prepared();
    neutral(spring.as_mut(), 4.0, 1.5);
    let (ls, _) = run(spring.as_mut(), &impulse(48_000), &impulse(48_000));
    assert!(rms(&ls[2_000..20_000]) > 0.005, "spring produces a tail");
    assert!(peak(&ls) < 4.0 && ls.iter().all(|v| v.is_finite()));
    // the spring loop repeats at roughly 40 ms × size: autocorrelation peak near 1920 samples
    let seg = &ls[4_800..24_000];
    let ac = |lag: usize| seg.iter().zip(&seg[lag..]).map(|(a, b)| a * b).sum::<f32>();
    let base = ac(0);
    let at = ac(1_920) / base;
    let off = ac(1_300) / base;
    assert!(at > off, "spring loop periodicity visible: {at} vs {off}");
}

#[test]
fn decay_shape_shortens_or_lengthens_bands_and_stays_stable() {
    // high band multiplier 0.3: highs vanish faster than in the neutral hall
    let mut flat = prepared();
    neutral(flat.as_mut(), 1.0, 2.0);
    let (lf, _) = run(flat.as_mut(), &impulse(96_000), &impulse(96_000));
    let mut dark = prepared();
    neutral(dark.as_mut(), 1.0, 2.0);
    set(dark.as_mut(), reverb::P_SHAPE0 + 3, 0.3);
    dark.reset();
    let (ld, _) = run(dark.as_mut(), &impulse(96_000), &impulse(96_000));
    let (a, b) = (24_000, 36_000); // 0.5–0.75 s into the tail
    assert!(centroid(&ld[a..b]) < centroid(&lf[a..b]) * 0.7, "high band decays faster: {} vs {}", centroid(&ld[a..b]), centroid(&lf[a..b]));
    // multipliers at 2.0 everywhere with a 30 s decay and no damping: bounded output over 12 s of noise then silence
    let mut wash = prepared();
    neutral(wash.as_mut(), 2.0, 30.0);
    for band in 0..4 {
        set(wash.as_mut(), reverb::P_SHAPE0 + band, 2.0);
    }
    set(wash.as_mut(), reverb::P_DIFFUSION, 100.0);
    set(wash.as_mut(), reverb::P_SIZE, 200.0);
    wash.reset();
    let mut rng = rawdawg_dsp::rng::DeterministicRng::new(0x4E4B, 1);
    let mut x: Vec<f32> = (0..(12.0 * SR) as usize).map(|_| rng.bipolar_f32() * 0.5).collect();
    x.extend(std::iter::repeat_n(0.0, (5.0 * SR) as usize));
    let (lw, _) = run(wash.as_mut(), &x, &x);
    assert!(lw.iter().all(|v| v.is_finite()));
    assert!(peak(&lw) < 12.0, "network stays bounded: peak {}", peak(&lw));
    let late = rms(&lw[lw.len() - 48_000..]);
    let early = rms(&lw[(12.0 * SR) as usize..(12.0 * SR) as usize + 48_000]);
    assert!(late <= early * 1.05, "energy does not grow after the input stops: {early} → {late}");
}

#[test]
fn shimmer_raises_the_tail_spectrum_over_time() {
    let mut hall = prepared();
    neutral(hall.as_mut(), 1.0, 4.0);
    let note: Vec<f32> = (0..96_000).map(|i| if i < 9_600 { (std::f32::consts::TAU * 220.0 * i as f32 / SR as f32).sin() * 0.5 } else { 0.0 }).collect();
    let (lh, _) = run(hall.as_mut(), &note, &note);
    let mut shim = prepared();
    neutral(shim.as_mut(), 5.0, 4.0);
    set(shim.as_mut(), reverb::P_SHIMMER_PITCH, 12.0);
    set(shim.as_mut(), reverb::P_SHIMMER_AMOUNT, 70.0);
    shim.reset();
    let (ls, _) = run(shim.as_mut(), &note, &note);
    let (a, b) = (48_000, 72_000);
    assert!(centroid(&ls[a..b]) > centroid(&lh[a..b]) * 1.4, "shimmer centroid {} vs hall {}", centroid(&ls[a..b]), centroid(&lh[a..b]));
    assert!(ls.iter().all(|v| v.is_finite()) && peak(&ls) < 4.0);
    let r = shim.as_any_mut().downcast_mut::<Reverb>().unwrap();
    assert_eq!(r.mode(), 5);
}

#[test]
fn mix_zero_is_a_null_presets_cover_modes_and_processing_is_allocation_free() {
    let mut p = prepared();
    set(p.as_mut(), reverb::P_MIX, 0.0);
    p.reset();
    let x: Vec<f32> = (0..8192).map(|i| (i as f32 * 0.37).sin() * 0.4).collect();
    let (l, _) = run(p.as_mut(), &x, &x);
    let err = l.iter().zip(&x).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(err < 1e-6, "dry null: {err}");
    let presets = rawdawg_effects::presets(reverb::TYPE_ID);
    let mut modes = std::collections::HashSet::new();
    let mut states = Vec::new();
    for pr in presets {
        let bytes = rawdawg_effects::preset_state(reverb::TYPE_ID, pr).unwrap();
        let mut d = prepared();
        d.load_state(&bytes).unwrap();
        modes.insert(d.as_any_mut().downcast_mut::<Reverb>().unwrap().mode());
        states.push(bytes);
    }
    assert_eq!(modes.len(), 6, "every reverb type has a preset");
    for i in 0..states.len() {
        for j in i + 1..states.len() {
            assert_ne!(states[i], states[j]);
        }
    }
    for mode in 0..6 {
        let mut p = prepared();
        neutral(p.as_mut(), mode as f64, 2.0);
        run(p.as_mut(), &x, &x);
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

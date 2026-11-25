//! Four-corner XY morphing (SYN-05). Compatible topologies interpolate in the
//! descriptor's perceptual (normalized) domain; waveform cycles blend in the
//! time domain with aligned phase (same length, same seam); discrete choices
//! follow the heaviest corner with hysteresis handled by the caller.

use crate::params::DESCRIPTORS;
use crate::patch::{Generator, Patch};
use rawdawg_dsp::processor::Processor;
use rawdawg_dsp::wavetable::CYCLE_LEN;
use rawdawg_types::params::ParamMapping;

/// Bilinear corner weights for (x, y) clamped to [0,1]: (1-x)(1-y), x(1-y), (1-x)y, xy.
pub fn weights(x: f32, y: f32) -> [f32; 4] {
    let x = if x.is_finite() { x.clamp(0.0, 1.0) } else { 0.0 };
    let y = if y.is_finite() { y.clamp(0.0, 1.0) } else { 0.0 };
    [(1.0 - x) * (1.0 - y), x * (1.0 - y), (1.0 - x) * y, x * y]
}

/// Whether four patches share a morphable structure.
pub fn compatible(corners: &[&Patch; 4]) -> bool {
    let a = corners[0];
    corners.iter().all(|p| {
        p.layers.len() == a.layers.len()
            && p.layers.iter().zip(&a.layers).all(|(x, y)| {
                x.generator.kind_index() == y.generator.kind_index()
                    && match (&x.generator, &y.generator) {
                        (Generator::Wavetable { frames: fa, .. }, Generator::Wavetable { frames: fb, .. }) => fa.len() == fb.len(),
                        (Generator::Additive(ax), Generator::Additive(ay)) => ax.amplitudes.len() == ay.amplitudes.len(),
                        _ => true,
                    }
            })
            && p.mod_routes.len() == a.mod_routes.len()
            && p.graph.same_topology(&a.graph)
    })
}

fn blend4(vals: [f32; 4], w: [f32; 4]) -> f32 {
    vals[0] * w[0] + vals[1] * w[1] + vals[2] * w[2] + vals[3] * w[3]
}

/// Morph four compatible corners at (x, y). Returns `None` for incompatible corners.
pub fn morph(corners: &[&Patch; 4], x: f32, y: f32) -> Option<Patch> {
    if !compatible(corners) {
        return None;
    }
    let w = weights(x, y);
    // exact corners reproduce the corner patch
    for (i, wi) in w.iter().enumerate() {
        if (*wi - 1.0).abs() < 1e-6 {
            return Some(corners[i].clone());
        }
    }
    let heaviest = (0..4).max_by(|a, b| w[*a].partial_cmp(&w[*b]).unwrap_or(std::cmp::Ordering::Equal)).unwrap_or(0);
    let mut out = corners[heaviest].clone();
    let synths: Vec<crate::synth::Synth> = corners.iter().map(|p| crate::synth::Synth::new((*p).clone())).collect();
    let mut target = crate::synth::Synth::new(out.clone());
    for d in DESCRIPTORS.iter() {
        if d.flags.read_only {
            continue;
        }
        let vals: Option<Vec<f32>> = synths.iter().map(|s| s.param(d.id)).collect();
        let Some(vals) = vals else { continue };
        let v = match d.mapping {
            ParamMapping::Toggle | ParamMapping::Discrete { .. } => vals[heaviest],
            _ => blend4([vals[0], vals[1], vals[2], vals[3]], w),
        };
        target.set_param(d.id, v);
    }
    out = target.patch().clone();
    // modular graph: same topology (checked above), interpolate node parameters
    if !out.graph.is_empty() {
        let graphs: Vec<&crate::graph::Graph> = corners.iter().map(|p| &p.graph).collect();
        crate::graph::morph_params(&graphs, &w, &mut out.graph);
    }
    // waveforms: time-domain blend with aligned phase
    for (li, layer) in out.layers.iter_mut().enumerate() {
        match &mut layer.generator {
            Generator::Wavetable { cycle, frames, .. } => {
                let srcs: Vec<&Generator> = corners.iter().map(|p| &p.layers[li].generator).collect();
                *cycle = blend_cycle(&srcs, w, None);
                for (fi, f) in frames.iter_mut().enumerate() {
                    *f = blend_cycle(&srcs, w, Some(fi));
                }
            }
            Generator::Granular { source, .. } => {
                let srcs: Vec<&Generator> = corners.iter().map(|p| &p.layers[li].generator).collect();
                *source = blend_cycle(&srcs, w, None);
            }
            Generator::Additive(a) => {
                for k in 0..a.amplitudes.len() {
                    let vals: [f32; 4] = std::array::from_fn(|c| match &corners[c].layers[li].generator {
                        Generator::Additive(x) => x.amplitudes.get(k).copied().unwrap_or(0.0),
                        _ => 0.0,
                    });
                    a.amplitudes[k] = blend4(vals, w).clamp(0.0, 1.0);
                }
            }
            _ => {}
        }
    }
    out.name = format!("Morph {:.2},{:.2}", x, y);
    Some(out)
}

fn blend_cycle(srcs: &[&Generator], w: [f32; 4], frame: Option<usize>) -> Vec<f32> {
    let get = |g: &Generator, i: usize| -> f32 {
        match (g, frame) {
            (Generator::Wavetable { frames, .. }, Some(fi)) if fi < frames.len() => frames[fi][i],
            (Generator::Wavetable { cycle, frames, .. }, _) => frames.first().map(|f| f[i]).unwrap_or(cycle[i]),
            (Generator::Granular { source, .. }, _) => source[i],
            _ => 0.0,
        }
    };
    (0..CYCLE_LEN).map(|i| (0..4).map(|c| get(srcs[c], i) * w[c]).sum::<f32>().clamp(-1.5, 1.5)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audition::{AuditionSpec, render_patch};
    use crate::mutate::mutate_once;
    use crate::patch::Locks;

    #[test]
    fn weights_sum_to_one_and_corners_are_exact() {
        for (x, y) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0), (0.3, 0.7), (0.5, 0.5)] {
            let w = weights(x, y);
            assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        }
        assert_eq!(weights(2.0, -1.0), [0.0, 1.0, 0.0, 0.0]);
        assert_eq!(weights(f32::NAN, 0.5), [0.5, 0.0, 0.5, 0.0]);
    }

    #[test]
    fn morph_reproduces_corners_and_is_smooth_between() {
        let base = Patch::default();
        let locks = Locks::default();
        let c: Vec<Patch> = (0..4).map(|i| mutate_once(&base, 100 + i, 30.0, &locks)).collect();
        let corners = [&c[0], &c[1], &c[2], &c[3]];
        assert!(compatible(&corners));
        assert_eq!(morph(&corners, 0.0, 0.0).unwrap(), c[0]);
        assert_eq!(morph(&corners, 1.0, 0.0).unwrap(), c[1]);
        assert_eq!(morph(&corners, 0.0, 1.0).unwrap(), c[2]);
        assert_eq!(morph(&corners, 1.0, 1.0).unwrap(), c[3]);
        let aud = AuditionSpec { hold_seconds: 0.15, tail_seconds: 0.05, ..Default::default() };
        let mut prev: Option<Vec<f32>> = None;
        for step in 0..=10 {
            let x = step as f32 / 10.0;
            let m = morph(&corners, x, x).unwrap();
            m.validate().unwrap();
            let buf = render_patch(&m, &aud).unwrap();
            assert!(buf.iter().all(|s| s.is_finite()));
            if let Some(p) = &prev {
                let (_, ca, _) = rawdawg_dsp::analysis::features(p, aud.sample_rate);
                let (_, cb, _) = rawdawg_dsp::analysis::features(&buf, aud.sample_rate);
                assert!((ca / cb).log2().abs() < 1.5, "step {step}: centroid jump {ca} -> {cb}");
            }
            prev = Some(buf);
        }
        // midpoint cutoff is the average of the corners in normalized (log) domain
        let d = DESCRIPTORS.iter().find(|d| d.id.0 == crate::params::FILTER_CUTOFF).unwrap();
        let mid = morph(&corners, 0.5, 0.5).unwrap();
        let expect: f32 = c.iter().map(|p| d.to_normalized(f64::from(p.filter.cutoff_hz))).sum::<f32>() / 4.0;
        assert!((d.to_normalized(f64::from(mid.filter.cutoff_hz)) - expect).abs() < 1e-3);
    }

    #[test]
    fn incompatible_corners_are_reported_not_faked() {
        let a = Patch::default();
        let mut b = Patch::default();
        b.layers.push(crate::patch::Layer::basic(rawdawg_dsp::oscillator::BasicWave::Saw));
        assert!(morph(&[&a, &b, &a, &a], 0.5, 0.5).is_none());
        assert!(!compatible(&[&a, &b, &a, &a]));
    }
}

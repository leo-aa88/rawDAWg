//! Deterministic randomization, evolution and breeding (SYN-03/SYN-04, DSP.md §4).
//!
//! A candidate is a pure function of (parent, seed, chaos, locks). Chaos tiers:
//! low perturbs a few continuous parameters in their perceptual (normalized)
//! domain; mid also reshapes waveforms, envelopes, modulation and unison; high
//! may replace generator topology within the layer/cost limits. Locked
//! categories/parameters are never touched; a locked layer structure is kept.

use crate::audition::{AuditionSpec, Rejection, SoundFeatures, validate_candidate};
use crate::params::{self, DESCRIPTORS, ParamBlock};
use crate::patch::{Generator, Layer, Locks, MAX_LAYERS, MAX_MOD_ROUTES, ModRoute, ModSource, ModTarget, Patch};
use rawdawg_dsp::processor::Processor;
use rawdawg_dsp::rng::DeterministicRng;
use rawdawg_dsp::wavetable::CYCLE_LEN;
use rawdawg_types::ParamId;
use rawdawg_types::params::{ParamCategory, ParamDescriptor, ParamMapping};

/// Bumped whenever the mutation algorithm changes (stored with presets).
pub const ALGORITHM_VERSION: u32 = 1;
/// Bounded candidate attempts before falling back to a smaller mutation.
pub const MAX_ATTEMPTS: u32 = 8;

#[derive(Clone, Debug, PartialEq)]
pub struct MutationSpec {
    pub seed: u64,
    /// 0..100.
    pub chaos: f32,
    pub locks: Locks,
    /// Validate by offline audition (always true in the app; tests may skip for speed).
    pub audition: Option<AuditionSpec>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MutationResult {
    pub patch: Patch,
    pub features: Option<SoundFeatures>,
    /// Attempts consumed (1 = first candidate accepted).
    pub attempts: u32,
    /// Chaos actually used (reduced on fallback).
    pub chaos_used: f32,
}

fn mutable_descriptors(patch: &Patch, locks: &Locks) -> Vec<&'static ParamDescriptor> {
    DESCRIPTORS
        .iter()
        .filter(|d| !d.flags.read_only && !d.flags.hidden && !locks.is_param_locked(d))
        .filter(|d| match params::decode(d.id.0) {
            Some(ParamBlock::Layer { layer, .. }) => layer < patch.layers.len(),
            Some(ParamBlock::Gen { layer, offset }) => patch.layers.get(layer).is_some_and(|l| gen_offset_applies(l.generator.kind_index(), offset)),
            Some(ParamBlock::Mod { route, .. }) => route < patch.mod_routes.len(),
            Some(ParamBlock::Global) => !matches!(d.id.0, params::MAX_VOICES | params::VOICE_MODE),
            _ => true,
        })
        .collect()
}

fn gen_offset_applies(kind: usize, offset: u32) -> bool {
    match kind {
        2 => offset <= params::G_FM_ALGORITHM,
        3 => matches!(offset, params::G_ADD_STRETCH | params::G_ADD_DETUNED),
        4 => (params::G_GRAIN_SIZE..=params::G_GRAIN_WINDOW).contains(&offset),
        5 => offset >= params::G_PLUCK_DAMPING,
        _ => false,
    }
}

/// Safe normalized ranges for a few parameters where the full range is unmusical at random.
fn safe_range(d: &ParamDescriptor) -> (f32, f32) {
    match d.id.0 {
        params::MASTER_GAIN => (d.to_normalized(-18.0), d.to_normalized(-6.0)),
        params::FILTER_CUTOFF => (d.to_normalized(120.0), 1.0),
        params::FILTER_RESONANCE => (0.0, d.to_normalized(8.0)),
        params::AMP_ATTACK | params::FENV_ATTACK => (0.0, d.to_normalized(2.0)),
        params::AMP_RELEASE | params::FENV_RELEASE => (d.to_normalized(0.02), d.to_normalized(4.0)),
        params::AMP_DECAY | params::FENV_DECAY => (d.to_normalized(0.01), d.to_normalized(4.0)),
        params::PORTAMENTO => (0.0, d.to_normalized(0.3)),
        _ => match params::decode(d.id.0) {
            Some(ParamBlock::Layer { offset: params::L_LEVEL, .. }) => (d.to_normalized(-18.0), d.to_normalized(3.0)),
            Some(ParamBlock::Layer { offset: params::L_COARSE, .. }) => (d.to_normalized(36.0), d.to_normalized(60.0)), // -12..+12 st
            Some(ParamBlock::Lfo { offset: params::LFO_RATE, .. }) => (0.0, d.to_normalized(12.0)),
            Some(ParamBlock::Gen { offset: params::G_GRAIN_DENSITY, .. }) => (d.to_normalized(8.0), d.to_normalized(120.0)),
            _ => (0.0, 1.0),
        },
    }
}

fn perturb(d: &ParamDescriptor, current: f32, amount: f32, rng: &mut DeterministicRng) -> f32 {
    let (lo, hi) = safe_range(d);
    match d.mapping {
        ParamMapping::Toggle => {
            if rng.chance(f64::from(amount) * 0.5) {
                if current >= 0.5 { 0.0 } else { 1.0 }
            } else {
                current
            }
        }
        ParamMapping::Discrete { steps } => {
            if rng.chance(f64::from(amount)) {
                let span = ((steps as f32 - 1.0) * amount).max(1.0);
                let cur = d.to_physical(current) as f32;
                let next = (cur + (rng.bipolar_f32() * span).round()).clamp(0.0, steps as f32 - 1.0);
                d.to_normalized(f64::from(next)).clamp(lo, hi)
            } else {
                current
            }
        }
        _ => {
            let delta = rng.gaussian() as f32 * 0.3 * amount;
            (current + delta).clamp(lo.min(hi), hi.max(lo))
        }
    }
}

/// Mutate a waveform cycle in the spectral domain (harmonic scaling/tilt) and time domain.
fn mutate_cycle(cycle: &[f32], amount: f32, rng: &mut DeterministicRng) -> Vec<f32> {
    let mut h = crate::sculpt::analyze(cycle);
    let n = h.magnitude.len();
    // random harmonic scaling with a smooth tilt and a few accents
    let tilt = rng.bipolar_f32() * amount * 1.5;
    for k in 1..n.min(256) {
        let kf = k as f32;
        let scale = 2f32.powf(tilt * (kf.log2() / 8.0)) * (1.0 + rng.bipolar_f32() * amount * 0.6).max(0.05);
        h.magnitude[k] = (h.magnitude[k] * scale).min(1.5);
    }
    for _ in 0..((amount * 6.0) as usize) {
        let k = 1 + rng.below(24) as usize;
        if k < n {
            h.magnitude[k] = (h.magnitude[k] + rng.unit_f32() * amount * 0.8).min(1.5);
        }
    }
    if rng.chance(f64::from(amount) * 0.5) {
        for k in 1..n.min(64) {
            h.phase[k] += rng.bipolar_f32() * amount * std::f32::consts::PI;
        }
    }
    let mut out = crate::sculpt::synthesize(&h);
    // light time-domain fold at high amounts for new character
    if amount > 0.5 && rng.chance(0.5) {
        let drive = 1.0 + (amount - 0.5) * 4.0;
        for s in &mut out {
            let x = *s * drive;
            let t = (x + 1.0).rem_euclid(4.0);
            *s = if t < 2.0 { t - 1.0 } else { 3.0 - t };
        }
    }
    // peak-normalize so levels stay comparable
    let peak = rawdawg_dsp::math::peak(&out).max(1e-3);
    for s in &mut out {
        *s = (*s / peak * 0.95).clamp(-1.0, 1.0);
    }
    out
}

/// Produce one deterministic candidate (no validation).
pub fn mutate_once(parent: &Patch, seed: u64, chaos: f32, locks: &Locks) -> Patch {
    let chaos = if chaos.is_finite() { chaos.clamp(0.0, 100.0) } else { 35.0 };
    if chaos <= 0.0 {
        return parent.clone();
    }
    let amount = chaos / 100.0;
    let mut rng = DeterministicRng::new(seed, 0x5EED);
    let mut p = parent.clone();
    let topology_free = !locks.is_category_locked(ParamCategory::Topology);
    let wave_free = !locks.is_category_locked(ParamCategory::Wave);

    // High chaos: structural changes first so parameter passes apply to the new structure.
    if amount > 0.7 && topology_free {
        if rng.chance(0.35) && p.layers.len() > 1 {
            let i = rng.below(p.layers.len() as u32) as usize;
            p.layers.remove(i);
        } else if rng.chance(0.4) && p.layers.len() < MAX_LAYERS {
            let kind = rng.below(6) as usize;
            let mut l = Layer::with_generator(Generator::default_of_kind(kind));
            l.level_db = -6.0;
            l.coarse_semitones = [0, 0, 0, 12, -12, 7][rng.below(6) as usize];
            p.layers.push(l);
        }
        for l in &mut p.layers {
            if rng.chance(0.3) {
                let kind = rng.below(6) as usize;
                if kind != l.generator.kind_index() {
                    l.generator = match (kind, &l.generator) {
                        // keep the editable wave when converting to a granular source
                        (4, Generator::Wavetable { cycle, .. }) => Generator::Granular { settings: Default::default(), source: cycle.clone() },
                        _ => Generator::default_of_kind(kind),
                    };
                }
            }
        }
    }
    // Mid chaos: modulation routes and unison.
    if amount > 0.35 && !locks.is_category_locked(ParamCategory::Modulation) {
        let n_new = rng.below(1 + (amount * 3.0) as u32) as usize;
        for _ in 0..n_new {
            if p.mod_routes.len() >= MAX_MOD_ROUTES {
                break;
            }
            let source = ModSource::from_index(rng.below(ModSource::ALL.len() as u32) as usize);
            let target = ModTarget::from_index(rng.below(ModTarget::ALL.len() as u32) as usize);
            if target == ModTarget::Pitch && locks.is_category_locked(ParamCategory::Pitch) {
                continue;
            }
            p.mod_routes.push(ModRoute { source, target, depth: rng.bipolar_f32() * amount * 0.6, enabled: true });
        }
        for (i, lfo) in p.lfos.iter_mut().enumerate() {
            if rng.chance(0.4) {
                lfo.depth = (lfo.depth + rng.unit_f32() * amount).clamp(0.0, 1.0);
                lfo.rate_hz = (lfo.rate_hz * 2f32.powf(rng.bipolar_f32() * 3.0 * amount)).clamp(0.05, 12.0);
                lfo.shape = crate::patch::LfoShape::from_index(rng.below(5) as usize);
                lfo.per_voice = i % 2 == 1;
            }
        }
    }
    // Waveform reshaping (Wave category).
    if amount > 0.2 && wave_free {
        for l in &mut p.layers {
            if !rng.chance(0.8) {
                continue;
            }
            match &mut l.generator {
                Generator::Wavetable { cycle, frames, .. } => {
                    *cycle = mutate_cycle(cycle, amount, &mut rng);
                    for f in frames.iter_mut() {
                        *f = mutate_cycle(f, amount, &mut rng);
                    }
                }
                Generator::Granular { source, .. } => *source = mutate_cycle(source, amount, &mut rng),
                Generator::Additive(a) => {
                    for k in 0..a.amplitudes.len() {
                        a.amplitudes[k] = (a.amplitudes[k] * (1.0 + rng.bipolar_f32() * amount) + if rng.chance(0.1) { rng.unit_f32() * amount } else { 0.0 }).clamp(0.0, 1.0);
                    }
                }
                _ => {}
            }
        }
    }
    // Modular graph node parameters (topology is never changed by mutation; locked nodes stay).
    if !p.graph.is_empty() {
        crate::graph::mutate_params(&mut p.graph, amount, &mut rng, &|c| locks.is_category_locked(c));
    }
    // Continuous/discrete parameter perturbation through the descriptors.
    let mut synth = crate::synth::Synth::new(p.clone());
    let candidates = mutable_descriptors(&p, locks);
    let count = if amount < 0.35 { 1 + (amount * 12.0) as usize } else { (candidates.len() as f32 * amount * 0.5) as usize + 3 };
    let mut picked: Vec<usize> = Vec::new();
    for _ in 0..count.min(candidates.len()) {
        let i = rng.below(candidates.len() as u32) as usize;
        if !picked.contains(&i) {
            picked.push(i);
        }
    }
    for i in picked {
        let d = candidates[i];
        if let Some(cur) = synth.param(d.id) {
            let n = perturb(d, cur, amount, &mut rng);
            synth.set_param(d.id, n);
        }
    }
    let mut out = synth.patch().clone();
    out.mutation.seed = seed;
    out.mutation.chaos = chaos;
    out.mutation.locks = locks.clone();
    out.mutation.algorithm_version = ALGORITHM_VERSION;
    // keep the default safety: never above 0 dB master unless the parent already was
    if locks.is_category_locked(ParamCategory::Safety) {
        out.master_db = out.master_db.min(parent.master_db.max(-10.0));
    }
    out
}

/// Mutate with bounded validation attempts; falls back to smaller chaos, then to the parent.
pub fn mutate(parent: &Patch, spec: &MutationSpec) -> MutationResult {
    if spec.chaos <= 0.0 {
        return MutationResult { patch: parent.clone(), features: None, attempts: 0, chaos_used: 0.0 };
    }
    let mut chaos = spec.chaos;
    for attempt in 1..=MAX_ATTEMPTS {
        let seed = spec.seed.wrapping_add(u64::from(attempt - 1) * 0x9E37);
        let candidate = mutate_once(parent, seed, chaos, &spec.locks);
        if candidate.validate().is_ok() {
            match &spec.audition {
                None => return MutationResult { patch: candidate, features: None, attempts: attempt, chaos_used: chaos },
                Some(a) => match validate_candidate(&candidate, a) {
                    Ok(f) => return MutationResult { patch: candidate, features: Some(f), attempts: attempt, chaos_used: chaos },
                    Err(Rejection::Silent) | Err(Rejection::TooLoud) | Err(Rejection::NonFinite) | Err(Rejection::Invalid(_)) => {}
                },
            }
        }
        chaos *= 0.6;
    }
    MutationResult { patch: parent.clone(), features: None, attempts: MAX_ATTEMPTS, chaos_used: 0.0 }
}

/// Deterministic descendant `index` of `parent` at `generation`.
pub fn evolve(parent: &Patch, base_seed: u64, generation: u32, index: u32, chaos: f32, locks: &Locks, audition: Option<AuditionSpec>) -> MutationResult {
    let seed = rawdawg_dsp::rng::mix64(base_seed ^ (u64::from(generation) << 32) ^ u64::from(index));
    mutate(parent, &MutationSpec { seed, chaos, locks: locks.clone(), audition })
}

/// Blend two parents. `a` is the active parent whose locks are honored and whose
/// structure wins when the parents are incompatible.
pub fn breed(a: &Patch, b: &Patch, seed: u64, locks: &Locks) -> Patch {
    let mut rng = DeterministicRng::new(seed, 0xB4EED);
    let mut child = a.clone();
    let mut synth_a = crate::synth::Synth::new(a.clone());
    let synth_b = crate::synth::Synth::new(b.clone());
    // per-layer structure: same kind → blend; different → pick a parent's layer
    for i in 0..child.layers.len() {
        let Some(lb) = b.layers.get(i) else { continue };
        let la = &child.layers[i];
        let compatible = la.generator.kind_index() == lb.generator.kind_index();
        if !compatible {
            if rng.chance(0.5) && !locks.is_category_locked(ParamCategory::Topology) {
                child.layers[i] = lb.clone();
            }
            continue;
        }
        if !locks.is_category_locked(ParamCategory::Wave) {
            let t = rng.unit_f32();
            match (&mut child.layers[i].generator, &lb.generator) {
                (Generator::Wavetable { cycle, .. }, Generator::Wavetable { cycle: cb, .. }) => {
                    *cycle = blend_cycles(cycle, cb, t);
                }
                (Generator::Granular { source, .. }, Generator::Granular { source: sb, .. }) => *source = blend_cycles(source, sb, t),
                (Generator::Additive(x), Generator::Additive(y)) => {
                    for (k, amp) in x.amplitudes.iter_mut().enumerate() {
                        if let Some(yb) = y.amplitudes.get(k) {
                            *amp = *amp + (yb - *amp) * t;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    if !locks.is_category_locked(ParamCategory::Topology) && b.layers.len() > child.layers.len() && rng.chance(0.5) {
        for l in b.layers.iter().skip(child.layers.len()) {
            if child.layers.len() < MAX_LAYERS {
                child.layers.push(l.clone());
            }
        }
    }
    // modular graph: identical topology → blend node parameters, otherwise inherit one parent's graph
    if child.graph.same_topology(&b.graph) {
        let t = rng.unit_f32();
        let graphs = [&a.graph, &b.graph];
        crate::graph::morph_params(&graphs, &[1.0 - t, t], &mut child.graph);
    } else if !locks.is_category_locked(ParamCategory::Topology) && rng.chance(0.5) {
        child.graph = b.graph.clone();
    }
    // continuous parameters: lerp with a random t per parameter; discrete: pick a parent
    let _ = synth_a.set_patch(child.clone());
    for d in DESCRIPTORS.iter() {
        if d.flags.read_only || locks.is_param_locked(d) {
            continue;
        }
        let (Some(va), Some(vb)) = (synth_a.param(d.id), synth_b.param(d.id)) else { continue };
        if (va - vb).abs() < 1e-6 {
            continue;
        }
        let v = match d.mapping {
            ParamMapping::Toggle | ParamMapping::Discrete { .. } => {
                if rng.chance(0.5) {
                    va
                } else {
                    vb
                }
            }
            _ => va + (vb - va) * rng.unit_f32(),
        };
        synth_a.set_param(d.id, v);
    }
    let mut out = synth_a.patch().clone();
    // modulation routes: union up to the limit
    if !locks.is_category_locked(ParamCategory::Modulation) {
        for r in &b.mod_routes {
            if out.mod_routes.len() < MAX_MOD_ROUTES && !out.mod_routes.iter().any(|x| x.source == r.source && x.target == r.target) && rng.chance(0.5) {
                out.mod_routes.push(*r);
            }
        }
    }
    out.mutation.seed = seed;
    out.mutation.locks = locks.clone();
    out.mutation.algorithm_version = ALGORITHM_VERSION;
    out.name = format!("{} × {}", a.name, b.name);
    out
}

fn blend_cycles(a: &[f32], b: &[f32], t: f32) -> Vec<f32> {
    (0..CYCLE_LEN).map(|i| a[i] + (b.get(i).copied().unwrap_or(0.0) - a[i]) * t).collect()
}

/// Parameter ids touched between two patches (for UI highlighting / tests).
pub fn changed_params(a: &Patch, b: &Patch) -> Vec<ParamId> {
    let sa = crate::synth::Synth::new(a.clone());
    let sb = crate::synth::Synth::new(b.clone());
    DESCRIPTORS.iter().filter(|d| sa.param(d.id).zip(sb.param(d.id)).is_some_and(|(x, y)| (x - y).abs() > 1e-6)).map(|d| d.id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audition::{AuditionSpec, render_patch};

    fn spec(seed: u64, chaos: f32) -> MutationSpec {
        MutationSpec { seed, chaos, locks: Locks::default(), audition: None }
    }

    #[test]
    fn same_seed_same_patch_and_chaos_zero_is_identity() {
        let parent = Patch::default();
        let a = mutate(&parent, &spec(7, 50.0)).patch;
        let b = mutate(&parent, &spec(7, 50.0)).patch;
        assert_eq!(a, b);
        let c = mutate(&parent, &spec(8, 50.0)).patch;
        assert_ne!(a, c);
        let z = mutate(&parent, &spec(7, 0.0));
        assert_eq!(z.patch, parent);
        assert_eq!(z.attempts, 0);
    }

    #[test]
    fn default_locks_preserve_root_pitch_safety_and_routing() {
        let parent = Patch::default();
        let mut locks = Locks::default();
        locks.set_param(params::FILTER_CUTOFF, true);
        for seed in 0..60u64 {
            let child = mutate_once(&parent, seed, 100.0, &locks);
            assert!(child.validate().is_ok(), "seed {seed}: {:?}", child.validate());
            assert_eq!(child.tuning, parent.tuning, "seed {seed}: tuning locked");
            for (i, l) in child.layers.iter().enumerate() {
                if let Some(pl) = parent.layers.get(i) {
                    assert_eq!(l.coarse_semitones, pl.coarse_semitones, "seed {seed} layer {i} coarse");
                    assert_eq!(l.fine_cents, pl.fine_cents, "seed {seed} layer {i} fine");
                }
                if let Generator::Fm(s) = &l.generator {
                    assert_eq!(s.operators[0].ratio, 1.0, "seed {seed}: carrier ratio is pitch-locked");
                }
            }
            assert_eq!(child.filter.cutoff_hz, parent.filter.cutoff_hz, "seed {seed}: per-param lock");
            assert!(child.master_db <= -6.0, "seed {seed}: safety keeps master bounded ({})", child.master_db);
            assert!(!child.mod_routes.iter().any(|r| r.target == ModTarget::Pitch), "seed {seed}: no pitch modulation routes while pitch is locked");
        }
    }

    #[test]
    fn thousand_seeds_stay_valid_and_finite() {
        let parent = Patch::default();
        let locks = Locks::default();
        for seed in 0..1000u64 {
            let chaos = (seed % 101) as f32;
            let child = mutate_once(&parent, seed, chaos, &locks);
            assert!(child.validate().is_ok(), "seed {seed} chaos {chaos}: {:?}", child.validate());
            assert!(child.layers.len() <= MAX_LAYERS);
            assert!(child.to_json().len() < 4_000_000);
        }
        // a sample of them must render finite audio
        for seed in (0..1000u64).step_by(97) {
            let child = mutate_once(&parent, seed, 80.0, &locks);
            let buf = render_patch(&child, &AuditionSpec { hold_seconds: 0.1, tail_seconds: 0.05, ..Default::default() }).unwrap();
            assert!(buf.iter().all(|s| s.is_finite()), "seed {seed}");
        }
    }

    #[test]
    fn default_chaos_mutations_differ_perceptibly_and_are_not_silent() {
        let parent = Patch::default();
        let aud = AuditionSpec { hold_seconds: 0.25, tail_seconds: 0.1, ..Default::default() };
        let parent_f = SoundFeatures::of(&render_patch(&parent, &aud).unwrap(), aud.sample_rate);
        let mut different = 0;
        let n = 30;
        for seed in 0..n {
            let r = mutate(&parent, &MutationSpec { seed, chaos: 35.0, locks: Locks::default(), audition: Some(aud) });
            let f = r.features.expect("validated candidate carries features");
            assert!(f.rms >= 0.003, "seed {seed}: silent candidate accepted");
            if f.distance(&parent_f) > 0.08 || !changed_params(&parent, &r.patch).is_empty() && f.distance(&parent_f) > 0.03 {
                different += 1;
            }
        }
        assert!(different * 10 >= n * 9, "only {different}/{n} default-chaos mutations differ perceptibly");
    }

    #[test]
    fn evolution_descendants_are_distinct_and_deterministic() {
        let parent = Patch::default();
        let locks = Locks::default();
        let gen1: Vec<Patch> = (0..9).map(|i| evolve(&parent, 42, 1, i, 35.0, &locks, None).patch).collect();
        let again: Vec<Patch> = (0..9).map(|i| evolve(&parent, 42, 1, i, 35.0, &locks, None).patch).collect();
        assert_eq!(gen1, again);
        for i in 0..9 {
            for j in (i + 1)..9 {
                assert_ne!(gen1[i], gen1[j], "descendants {i} and {j} identical");
            }
            assert_ne!(gen1[i], parent);
        }
        let gen2 = evolve(&gen1[3], 42, 2, 0, 35.0, &locks, None).patch;
        assert_ne!(gen2, gen1[3]);
    }

    #[test]
    fn breed_blends_and_honors_locks() {
        let a = Patch::default();
        let mut b = mutate_once(&a, 99, 60.0, &Locks::default());
        b.filter.cutoff_hz = 400.0;
        b.name = "B".into();
        let child = breed(&a, &b, 1, &Locks::default());
        assert!(child.validate().is_ok());
        assert!(child.filter.cutoff_hz <= 20_000.0 && child.filter.cutoff_hz >= 400.0);
        assert_eq!(child.tuning, a.tuning, "pitch lock from the active parent");
        let same = breed(&a, &b, 1, &Locks::default());
        assert_eq!(child, same, "deterministic");
        let mut locks = Locks::default();
        locks.set_param(params::FILTER_CUTOFF, true);
        let locked = breed(&a, &b, 2, &locks);
        assert_eq!(locked.filter.cutoff_hz, a.filter.cutoff_hz);
    }

    #[test]
    fn locked_topology_keeps_layer_structure_at_high_chaos() {
        let mut parent = Patch::default();
        parent.layers.push(Layer::with_generator(Generator::Fm(Default::default())));
        let mut locks = Locks::default();
        locks.set_category(ParamCategory::Topology, true);
        for seed in 0..40u64 {
            let child = mutate_once(&parent, seed, 100.0, &locks);
            assert_eq!(child.layers.len(), 2, "seed {seed}");
            assert_eq!(child.layers[1].generator.kind_index(), 2, "seed {seed}");
        }
    }
}

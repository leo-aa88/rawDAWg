//! Rack tools (FX-03): deterministic randomisation and four-corner morphing of effect
//! parameter sets. Both are pure functions over `(ParamId, normalized)` lists, so the app,
//! the offline renderer and tests share them; the app applies the results as ordinary
//! parameter edits (one undo step per action).

use rawdawg_dsp::rng::DeterministicRng;
use rawdawg_types::ParamId;
use rawdawg_types::params::{ParamCategory, ParamDescriptor, ParamMapping};

/// Which parameters a rack action may touch.
#[derive(Clone, Debug, PartialEq)]
pub struct RackLocks {
    pub categories: Vec<ParamCategory>,
    pub params: Vec<u32>,
}

impl Default for RackLocks {
    fn default() -> Self {
        // wet/dry, output levels and routing stay put unless unlocked on purpose
        Self { categories: vec![ParamCategory::Mix, ParamCategory::Safety, ParamCategory::Routing], params: vec![] }
    }
}

impl RackLocks {
    pub fn is_locked(&self, d: &ParamDescriptor) -> bool {
        self.params.contains(&d.id.0) || self.categories.contains(&d.category)
    }
    pub fn set_category(&mut self, c: ParamCategory, locked: bool) {
        self.categories.retain(|x| *x != c);
        if locked {
            self.categories.push(c);
        }
    }
}

fn is_discrete(d: &ParamDescriptor) -> bool {
    matches!(d.mapping, ParamMapping::Toggle | ParamMapping::Discrete { .. })
}

/// Randomise one effect's parameters. `chaos` 0–100: 0 returns no edits; low chaos nudges
/// a few continuous parameters in their normalised (perceptual) domain; medium chaos moves
/// more of them further and may flip non-topology switches; high chaos may also change
/// topology choices (modes, stage counts). Locked categories/parameters are never touched.
/// Deterministic for a given `rng` state.
pub fn randomize(descriptors: &[ParamDescriptor], current: &[(ParamId, f32)], rng: &mut DeterministicRng, chaos: f32, locks: &RackLocks) -> Vec<(ParamId, f32)> {
    let chaos = if chaos.is_finite() { chaos.clamp(0.0, 100.0) } else { 0.0 };
    if chaos <= 0.0 {
        return Vec::new();
    }
    let c = chaos / 100.0;
    let candidates: Vec<&ParamDescriptor> = descriptors.iter().filter(|d| !d.flags.read_only && !d.flags.hidden && !locks.is_locked(d)).collect();
    if candidates.is_empty() {
        return Vec::new();
    }
    let value_of = |id: ParamId| current.iter().find(|(i, _)| *i == id).map(|(_, v)| *v);
    // how many parameters move: a few at low chaos, all of them at full chaos
    let count = ((candidates.len() as f32 * (0.15 + 0.85 * c)).ceil() as usize).clamp(1, candidates.len());
    let mut order: Vec<usize> = (0..candidates.len()).collect();
    for i in (1..order.len()).rev() {
        let j = (rng.next_u32() as usize) % (i + 1);
        order.swap(i, j);
    }
    let mut out = Vec::with_capacity(count);
    for &ci in order.iter().take(count) {
        let d = candidates[ci];
        let Some(cur) = value_of(d.id) else { continue };
        let topology = d.category == ParamCategory::Topology;
        let v = if is_discrete(d) {
            // switches move at medium chaos, topology only at high chaos
            let allowed = if topology { c >= 0.7 } else { c >= 0.35 };
            if !allowed || rng.unit_f32() > c {
                continue;
            }
            match d.mapping {
                ParamMapping::Toggle => {
                    if cur >= 0.5 {
                        0.0
                    } else {
                        1.0
                    }
                }
                ParamMapping::Discrete { steps } => {
                    let idx = (rng.next_u32() as usize) % steps.max(1) as usize;
                    d.to_normalized(idx as f64)
                }
                _ => cur,
            }
        } else {
            // perceptual nudge: ±(0.05 + 0.45·c) of the normalised range, bounded to [0, 1]
            let amount = 0.05 + 0.45 * c;
            (cur + rng.bipolar_f32() * amount).clamp(0.0, 1.0)
        };
        if (v - cur).abs() > 1e-6 {
            out.push((d.id, v));
        }
    }
    out
}

/// Bilinear corner weights for (x, y) clamped to [0,1]: (1-x)(1-y), x(1-y), (1-x)y, xy.
pub fn weights(x: f32, y: f32) -> [f32; 4] {
    let x = if x.is_finite() { x.clamp(0.0, 1.0) } else { 0.0 };
    let y = if y.is_finite() { y.clamp(0.0, 1.0) } else { 0.0 };
    [(1.0 - x) * (1.0 - y), x * (1.0 - y), (1.0 - x) * y, x * y]
}

/// Morph four parameter snapshots of the same effect at (x, y): continuous parameters blend
/// in the normalised domain, switches and choices follow the heaviest corner, exact corners
/// reproduce that corner. Parameters missing from any corner are skipped.
pub fn morph(descriptors: &[ParamDescriptor], corners: [&[(ParamId, f32)]; 4], x: f32, y: f32) -> Vec<(ParamId, f32)> {
    let w = weights(x, y);
    // ties go to the first corner (max_by alone would pick the last)
    let heaviest = (0..4).rev().max_by(|a, b| w[*a].partial_cmp(&w[*b]).unwrap_or(std::cmp::Ordering::Equal)).unwrap_or(0);
    let mut out = Vec::with_capacity(descriptors.len());
    for d in descriptors.iter().filter(|d| !d.flags.read_only) {
        let vals: Option<Vec<f32>> = corners.iter().map(|c| c.iter().find(|(i, _)| *i == d.id).map(|(_, v)| *v)).collect();
        let Some(vals) = vals else { continue };
        let v = if is_discrete(d) || (w[heaviest] - 1.0).abs() < 1e-6 { vals[heaviest] } else { vals[0] * w[0] + vals[1] * w[1] + vals[2] * w[2] + vals[3] * w[3] };
        out.push((d.id, v.clamp(0.0, 1.0)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reverb;
    use rawdawg_dsp::processor::Processor;

    fn snapshot(p: &dyn Processor) -> Vec<(ParamId, f32)> {
        p.descriptors().iter().filter_map(|d| p.param(d.id).map(|v| (d.id, v))).collect()
    }

    #[test]
    fn zero_chaos_is_a_no_op_and_locks_hold() {
        let p = reverb::Reverb::new();
        let snap = snapshot(&p);
        let mut rng = DeterministicRng::new(1, 2);
        assert!(randomize(&reverb::DESCRIPTORS, &snap, &mut rng, 0.0, &RackLocks::default()).is_empty());
        let edits = randomize(&reverb::DESCRIPTORS, &snap, &mut rng, 100.0, &RackLocks::default());
        assert!(!edits.is_empty());
        for (id, _) in &edits {
            let d = reverb::DESCRIPTORS.iter().find(|d| d.id == *id).unwrap();
            assert_ne!(d.category, ParamCategory::Mix, "mix parameters are locked by default");
        }
        let mut all = RackLocks::default();
        for c in
            [ParamCategory::Effects, ParamCategory::Modulation, ParamCategory::Topology, ParamCategory::Pitch, ParamCategory::Filter, ParamCategory::Wave, ParamCategory::Envelope]
        {
            all.set_category(c, true);
        }
        assert!(randomize(&reverb::DESCRIPTORS, &snap, &mut rng, 100.0, &all).is_empty(), "everything locked: nothing moves");
    }

    #[test]
    fn randomize_is_deterministic_and_chaos_scales_the_change() {
        let p = reverb::Reverb::new();
        let snap = snapshot(&p);
        let a = randomize(&reverb::DESCRIPTORS, &snap, &mut DeterministicRng::new(7, 7), 50.0, &RackLocks::default());
        let b = randomize(&reverb::DESCRIPTORS, &snap, &mut DeterministicRng::new(7, 7), 50.0, &RackLocks::default());
        assert_eq!(a, b);
        let spread = |chaos: f32| -> f32 {
            (0..16)
                .map(|seed| {
                    randomize(&reverb::DESCRIPTORS, &snap, &mut DeterministicRng::new(seed, 1), chaos, &RackLocks::default())
                        .iter()
                        .map(|(id, v)| (v - snap.iter().find(|(i, _)| i == id).unwrap().1).abs())
                        .sum::<f32>()
                })
                .sum::<f32>()
                / 16.0
        };
        assert!(spread(10.0) < spread(50.0) && spread(50.0) < spread(100.0), "{} {} {}", spread(10.0), spread(50.0), spread(100.0));
        // topology (reverb type) only moves at high chaos
        let mode_moves = |chaos: f32| {
            (0..32)
                .any(|seed| randomize(&reverb::DESCRIPTORS, &snap, &mut DeterministicRng::new(seed, 3), chaos, &RackLocks::default()).iter().any(|(id, _)| id.0 == reverb::P_MODE))
        };
        assert!(!mode_moves(50.0) && mode_moves(100.0));
    }

    #[test]
    fn morph_corners_are_exact_and_the_middle_blends() {
        let base = snapshot(&reverb::Reverb::new());
        let mut corners: Vec<Vec<(ParamId, f32)>> = (0..4).map(|_| base.clone()).collect();
        let decay = reverb::DESCRIPTORS.iter().find(|d| d.id.0 == reverb::P_DECAY).unwrap();
        for (i, c) in corners.iter_mut().enumerate() {
            let slot = c.iter_mut().find(|(id, _)| *id == decay.id).unwrap();
            slot.1 = i as f32 * 0.25;
            let mode = c.iter_mut().find(|(id, _)| id.0 == reverb::P_MODE).unwrap();
            mode.1 = if i == 3 { 1.0 } else { 0.0 };
        }
        let refs = [&corners[0][..], &corners[1][..], &corners[2][..], &corners[3][..]];
        let at = |x: f32, y: f32| morph(&reverb::DESCRIPTORS, refs, x, y);
        let get = |m: &[(ParamId, f32)], id: u32| m.iter().find(|(i, _)| i.0 == id).unwrap().1;
        assert_eq!(get(&at(0.0, 0.0), reverb::P_DECAY), 0.0);
        assert_eq!(get(&at(1.0, 1.0), reverb::P_DECAY), 0.75);
        assert!((get(&at(0.5, 0.5), reverb::P_DECAY) - 0.375).abs() < 1e-6);
        assert_eq!(get(&at(0.5, 0.5), reverb::P_MODE), 0.0, "discrete follows the heaviest corner (ties → first)");
        assert_eq!(get(&at(0.9, 0.9), reverb::P_MODE), 1.0);
        let w = weights(0.25, 0.75);
        assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-6);
    }
}

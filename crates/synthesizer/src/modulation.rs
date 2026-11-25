//! LFOs and the modulation matrix (control-rate evaluation).

use crate::patch::{Lfo, LfoShape, MAX_LFOS, ModRoute, ModSource, ModTarget};
use rawdawg_dsp::rng::DeterministicRng;

/// One LFO's running state (global or per voice).
#[derive(Clone, Copy, Debug)]
pub struct LfoState {
    phase: f64,
    held: f32,
    last_cycle: u64,
    last: f32,
}

impl Default for LfoState {
    fn default() -> Self {
        Self { phase: 0.0, held: 0.0, last_cycle: u64::MAX, last: 0.0 }
    }
}

impl LfoState {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Advance by `samples` at `sample_rate` and return the bipolar value (-1..1).
    #[inline]
    pub fn advance(&mut self, lfo: &Lfo, samples: usize, sample_rate: f64, tempo_bpm: f64, rng: &mut DeterministicRng) -> f32 {
        let rate = if lfo.sync_beats > 0.0 { tempo_bpm / 60.0 * f64::from(lfo.sync_beats) } else { f64::from(lfo.rate_hz) };
        let inc = (rate / sample_rate).clamp(0.0, 0.5);
        self.phase += inc * samples as f64;
        let cycle = self.phase.floor() as u64;
        let p = (self.phase - self.phase.floor()) as f32;
        let v = match lfo.shape {
            LfoShape::Sine => (p * std::f32::consts::TAU).sin(),
            LfoShape::Triangle => {
                if p < 0.5 {
                    4.0 * p - 1.0
                } else {
                    3.0 - 4.0 * p
                }
            }
            LfoShape::Saw => 2.0 * p - 1.0,
            LfoShape::Square => {
                if p < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            LfoShape::SampleHold => {
                if cycle != self.last_cycle {
                    self.last_cycle = cycle;
                    self.held = rng.bipolar_f32();
                }
                self.held
            }
        };
        self.last = v * lfo.depth;
        self.last
    }

    /// Most recent value returned by [`advance`](Self::advance).
    pub fn value(&self) -> f32 {
        self.last
    }
}

/// Modulation source values for one voice at one control tick.
#[derive(Clone, Copy, Debug, Default)]
pub struct ModInputs {
    pub lfo: [f32; MAX_LFOS],
    pub amp_env: f32,
    pub filter_env: f32,
    pub velocity: f32,
    /// -1..1 across the keyboard (C4 = 0).
    pub key: f32,
    pub mod_wheel: f32,
    pub aftertouch: f32,
    pub random: f32,
}

impl ModInputs {
    #[inline]
    fn source(&self, s: ModSource) -> f32 {
        match s {
            ModSource::Lfo1 => self.lfo[0],
            ModSource::Lfo2 => self.lfo[1],
            ModSource::Lfo3 => self.lfo[2],
            ModSource::Lfo4 => self.lfo[3],
            ModSource::AmpEnv => self.amp_env,
            ModSource::FilterEnv => self.filter_env,
            ModSource::Velocity => self.velocity,
            ModSource::KeyTrack => self.key,
            ModSource::ModWheel => self.mod_wheel,
            ModSource::Aftertouch => self.aftertouch,
            ModSource::Random => self.random,
        }
    }
}

/// Summed modulation per target (normalized units; see `ModTarget` docs).
#[derive(Clone, Copy, Debug, Default)]
pub struct ModOutputs {
    pub values: [f32; 13],
}

impl ModOutputs {
    #[inline]
    pub fn get(&self, t: ModTarget) -> f32 {
        self.values[t.index()]
    }
    /// Evaluate all routes. Bounded by `MAX_MOD_ROUTES`; values clamped to ±2.
    #[inline]
    pub fn evaluate(routes: &[ModRoute], inputs: &ModInputs) -> Self {
        let mut out = Self::default();
        for r in routes.iter().filter(|r| r.enabled) {
            let v = inputs.source(r.source) * r.depth;
            let slot = &mut out.values[r.target.index()];
            *slot = (*slot + v).clamp(-2.0, 2.0);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lfo_rate_and_shapes() {
        let lfo = Lfo { shape: LfoShape::Saw, rate_hz: 1.0, sync_beats: 0.0, depth: 1.0, per_voice: false };
        let mut st = LfoState::default();
        let mut rng = DeterministicRng::new(1, 1);
        // half a cycle at 1 Hz / 48k = 24000 samples → saw at phase 0.5 = 0
        let v = st.advance(&lfo, 24_000, 48_000.0, 120.0, &mut rng);
        assert!(v.abs() < 1e-4, "{v}");
        let v = st.advance(&lfo, 12_000, 48_000.0, 120.0, &mut rng);
        assert!((v - 0.5).abs() < 1e-4, "{v}");
        // beat sync: 1 cycle per beat at 120 BPM = 2 Hz
        let synced = Lfo { sync_beats: 1.0, shape: LfoShape::Square, ..lfo };
        let mut st2 = LfoState::default();
        assert_eq!(st2.advance(&synced, 1000, 48_000.0, 120.0, &mut rng), 1.0);
        assert_eq!(st2.advance(&synced, 12_000, 48_000.0, 120.0, &mut rng), -1.0);
        let sh = Lfo { shape: LfoShape::SampleHold, rate_hz: 10.0, ..lfo };
        let mut st3 = LfoState::default();
        let a = st3.advance(&sh, 100, 48_000.0, 120.0, &mut rng);
        let b = st3.advance(&sh, 100, 48_000.0, 120.0, &mut rng);
        assert_eq!(a, b, "held within a cycle");
        let c = st3.advance(&sh, 4800, 48_000.0, 120.0, &mut rng);
        assert_ne!(a, c, "new value on the next cycle");
        assert!(c.abs() <= 1.0);
    }

    #[test]
    fn matrix_sums_and_clamps() {
        let routes = [
            ModRoute { source: ModSource::Velocity, target: ModTarget::Cutoff, depth: 0.5, enabled: true },
            ModRoute { source: ModSource::ModWheel, target: ModTarget::Cutoff, depth: 1.0, enabled: true },
            ModRoute { source: ModSource::Lfo1, target: ModTarget::Pitch, depth: -1.0, enabled: true },
            ModRoute { source: ModSource::Lfo1, target: ModTarget::Amp, depth: 1.0, enabled: false },
        ];
        let inputs = ModInputs { velocity: 1.0, mod_wheel: 1.0, lfo: [0.25, 0.0, 0.0, 0.0], ..Default::default() };
        let out = ModOutputs::evaluate(&routes, &inputs);
        assert!((out.get(ModTarget::Cutoff) - 1.5).abs() < 1e-6);
        assert!((out.get(ModTarget::Pitch) + 0.25).abs() < 1e-6);
        assert_eq!(out.get(ModTarget::Amp), 0.0);
        let big = [ModRoute { source: ModSource::ModWheel, target: ModTarget::Amp, depth: 1.0, enabled: true }; 16];
        let out = ModOutputs::evaluate(&big, &inputs);
        assert_eq!(out.get(ModTarget::Amp), 2.0);
    }
}

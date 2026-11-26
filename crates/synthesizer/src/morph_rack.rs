//! Morph rack (SYN-05, DSP.md "XY morph"): four corner sounds of any structure play the same
//! notes in parallel and are crossfaded by the pad position. X and Y are ordinary automatable
//! parameters. Corners holding the same sound share one renderer; a renderer whose weight is
//! zero, that has no sounding voice and receives no event, skips rendering. The whole rack must
//! fit the oscillator budget of one synth (CPU admission), otherwise it is refused with a
//! message instead of being approximated.

use crate::morph::weights;
use crate::patch::Patch;
use crate::synth::Synth;
use rawdawg_dsp::processor::{PrepareError, PrepareSpec, ProcessContext, Processor, Tail};
use rawdawg_dsp::smooth::Smoother;
use rawdawg_dsp::{AudioBlock, BusLayout};
use rawdawg_types::events::EventKind;
use rawdawg_types::limits::EngineLimits;
use rawdawg_types::params::{ParamCategory, ParamDescriptor, ParamMapping, ParamUnit};
use rawdawg_types::{ParamId, ids::native_types};
use serde::{Deserialize, Serialize};

pub const TYPE_ID: &str = native_types::MORPH;
pub const DISPLAY_NAME: &str = "Morph rack";
/// Ids no synth parameter uses (the synth ignores unknown ids), so the rack can hand its events
/// to the corner synths unchanged.
pub const PARAM_X: u32 = 9001;
pub const PARAM_Y: u32 = 9002;
/// Pad movements glide over this time (automation and gestures alike), so they never click.
const XY_SMOOTHING_S: f32 = 0.02;

pub static DESCRIPTORS: [ParamDescriptor; 2] = [
    ParamDescriptor::continuous(PARAM_X, "x", "Morph X (A/C ↔ B/D)", ParamUnit::None, ParamMapping::Linear { min: 0.0, max: 1.0 }, 0.5, ParamCategory::Modulation)
        .with_smoothing(20.0),
    ParamDescriptor::continuous(PARAM_Y, "y", "Morph Y (A/B ↔ C/D)", ParamUnit::None, ParamMapping::Linear { min: 0.0, max: 1.0 }, 0.5, ParamCategory::Modulation)
        .with_smoothing(20.0),
];

/// The saved state: corners A (top-left), B (top-right), C (bottom-left), D (bottom-right)
/// and the pad position.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct MorphRackState {
    pub corners: [Patch; 4],
    pub x: f32,
    pub y: f32,
}

impl MorphRackState {
    pub fn new(corners: [Patch; 4], x: f32, y: f32) -> Self {
        Self { corners, x: x.clamp(0.0, 1.0), y: y.clamp(0.0, 1.0) }
    }

    /// Which renderer plays each corner: identical corners share the first one's.
    pub fn owners(&self) -> [usize; 4] {
        let mut owner = [0usize; 4];
        let mut distinct = 0;
        for i in 0..4 {
            match (0..i).find(|&j| self.corners[j] == self.corners[i]) {
                Some(j) => owner[i] = owner[j],
                None => {
                    owner[i] = distinct;
                    distinct += 1;
                }
            }
        }
        owner
    }

    /// Oscillator voices the rack can run at once (voices × unison over the distinct corners).
    pub fn oscillator_cost(&self) -> u32 {
        let owner = self.owners();
        (0..4)
            .filter(|&i| owner[..i].iter().all(|&o| o != owner[i]))
            .map(|i| {
                let p = &self.corners[i];
                p.layers.iter().filter(|l| l.enabled).map(|l| u32::from(l.unison)).sum::<u32>() * p.max_voices
            })
            .sum()
    }

    /// The most oscillator voices a rack may need: what one synth may use.
    pub fn oscillator_budget() -> u32 {
        EngineLimits::DEFAULT.max_voice_oscillators * 4
    }

    pub fn validate(&self) -> Result<(), String> {
        for (name, c) in ["A", "B", "C", "D"].iter().zip(&self.corners) {
            c.validate().map_err(|e| format!("corner {name}: {e}"))?;
        }
        if !self.x.is_finite() || !self.y.is_finite() || !(0.0..=1.0).contains(&self.x) || !(0.0..=1.0).contains(&self.y) {
            return Err("pad position out of range".into());
        }
        let (cost, budget) = (self.oscillator_cost(), Self::oscillator_budget());
        if cost > budget {
            return Err(format!(
                "the corners together need {cost} oscillator voices and at most {budget} fit: lower their voice count or unison, or use the same sound in more corners"
            ));
        }
        Ok(())
    }

    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).expect("morph rack state serializes")
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self, String> {
        let s: Self = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        s.validate()?;
        Ok(s)
    }
}

impl Default for MorphRackState {
    fn default() -> Self {
        Self::new(std::array::from_fn(|_| Patch::default()), 0.5, 0.5)
    }
}

pub struct MorphRack {
    state: MorphRackState,
    owner: [usize; 4],
    synths: Vec<Synth>,
    x: Smoother,
    y: Smoother,
    /// Per-renderer output buffers (left, right), sized in `prepare`.
    buffers: Vec<[Vec<f32>; 2]>,
    sample_rate: f64,
    prepared: bool,
}

impl MorphRack {
    pub fn new(state: MorphRackState) -> Self {
        let owner = state.owners();
        let distinct = owner.iter().copied().max().map_or(1, |m| m + 1);
        let synths = (0..distinct).map(|r| Synth::new(state.corners[owner.iter().position(|&o| o == r).unwrap_or(0)].clone())).collect();
        let (x, y) = (state.x, state.y);
        Self {
            state,
            owner,
            synths,
            x: Smoother::new(x, XY_SMOOTHING_S, 48_000.0),
            y: Smoother::new(y, XY_SMOOTHING_S, 48_000.0),
            buffers: Vec::new(),
            sample_rate: 48_000.0,
            prepared: false,
        }
    }

    pub fn state(&self) -> &MorphRackState {
        &self.state
    }

    /// How many renderers run (distinct corner sounds).
    pub fn renderers(&self) -> usize {
        self.synths.len()
    }

    /// Weight of each renderer at the pad position (identical corners add up).
    fn renderer_weights(&self, x: f32, y: f32) -> [f32; 4] {
        let w = weights(x, y);
        let mut r = [0.0f32; 4];
        for (corner, weight) in w.iter().enumerate() {
            r[self.owner[corner]] += weight;
        }
        r
    }
}

impl Default for MorphRack {
    fn default() -> Self {
        Self::new(MorphRackState::default())
    }
}

impl Processor for MorphRack {
    fn type_id(&self) -> &'static str {
        TYPE_ID
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    /// Notes keep sounding across a recompile when the corners did not change.
    fn migrate_from(&mut self, old: &mut dyn Processor) {
        let Some(old) = old.as_any_mut().downcast_mut::<MorphRack>() else { return };
        if !old.prepared || !self.prepared || old.sample_rate != self.sample_rate || old.state.corners != self.state.corners {
            return;
        }
        for (n, o) in self.synths.iter_mut().zip(old.synths.iter_mut()) {
            n.migrate_from(o);
        }
        self.x.snap(old.x.current());
        self.y.snap(old.y.current());
        self.x.set_target(self.state.x);
        self.y.set_target(self.state.y);
    }

    fn descriptors(&self) -> &'static [ParamDescriptor] {
        &DESCRIPTORS
    }

    fn prepare(&mut self, spec: &PrepareSpec) -> Result<(), PrepareError> {
        if !spec.is_valid() {
            return Err(PrepareError::UnsupportedSampleRate);
        }
        if spec.layout != BusLayout::Stereo {
            return Err(PrepareError::UnsupportedLayout);
        }
        self.state.validate().map_err(PrepareError::InvalidState)?;
        for s in &mut self.synths {
            s.prepare(spec)?;
        }
        self.buffers = (0..self.synths.len()).map(|_| [vec![0.0; spec.max_frames], vec![0.0; spec.max_frames]]).collect();
        self.sample_rate = spec.sample_rate;
        let sr = spec.sample_rate as f32;
        self.x = Smoother::new(self.state.x, XY_SMOOTHING_S, sr);
        self.y = Smoother::new(self.state.y, XY_SMOOTHING_S, sr);
        self.prepared = true;
        Ok(())
    }

    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let frames = ctx.audio.frames;
        ctx.audio.clear_outputs();
        if !self.prepared || ctx.audio.outputs.len() < 2 || frames > self.buffers.first().map_or(0, |b| b[0].len()) {
            return;
        }
        // the rack's own parameters; everything else goes to every corner
        // the rack's own parameters; the corners get every event as it is (they ignore the
        // rack's parameter ids), so nothing is copied and nothing can be lost
        let mut has_events = false;
        for ev in ctx.events {
            match ev.kind {
                EventKind::Param { id, value } if id.0 == PARAM_X || id.0 == PARAM_Y => {
                    self.set_param(id, value);
                }
                _ => has_events = true,
            }
        }
        let start = self.renderer_weights(self.x.current(), self.y.current());
        let end = self.renderer_weights(self.x.target(), self.y.target());
        let settled = self.x.is_settled() && self.y.is_settled();
        // weights per sample (the smoothed pad position), shared by all renderers
        let (first, rest) = ctx.audio.outputs.split_at_mut(1);
        let out_l: &mut [f32] = &mut first[0][..frames];
        let out_r: &mut [f32] = &mut rest[0][..frames];
        for (r, synth) in self.synths.iter_mut().enumerate() {
            let silent_and_weightless = start[r] == 0.0 && end[r] == 0.0 && settled && synth.stats().active_voices == 0 && !has_events;
            if silent_and_weightless {
                continue;
            }
            let [bl, br] = &mut self.buffers[r];
            let mut outs: [&mut [f32]; 2] = [&mut bl[..frames], &mut br[..frames]];
            let mut sub = ProcessContext { audio: AudioBlock::output_only(frames, &mut outs), events: ctx.events, transport: ctx.transport };
            synth.process(&mut sub);
        }
        // mix: each sample's weights from the gliding pad position
        for i in 0..frames {
            let (x, y) = (self.x.next(), self.y.next());
            let w = self.renderer_weights(x, y);
            let (mut l, mut r) = (0.0f32, 0.0f32);
            for (k, buf) in self.buffers.iter().enumerate() {
                if w[k] != 0.0 {
                    l += buf[0][i] * w[k];
                    r += buf[1][i] * w[k];
                }
            }
            out_l[i] = l;
            out_r[i] = r;
        }
        // a skipped renderer's buffer holds an old block: only weightless ones are skipped, and
        // their weight is zero here, so stale samples never reach the output
    }

    fn reset(&mut self) {
        for s in &mut self.synths {
            s.reset();
        }
    }

    fn tail(&self) -> Tail {
        let mut longest = 0u32;
        for s in &self.synths {
            match s.tail() {
                Tail::None => {}
                Tail::Frames(n) => longest = longest.max(n),
                Tail::Unknown => return Tail::Unknown,
                Tail::Infinite => return Tail::Infinite,
            }
        }
        if longest == 0 { Tail::None } else { Tail::Frames(longest) }
    }

    fn active_voices(&self) -> u32 {
        self.synths.iter().map(|s| s.stats().active_voices).sum()
    }

    fn param(&self, id: ParamId) -> Option<f32> {
        match id.0 {
            PARAM_X => Some(self.state.x),
            PARAM_Y => Some(self.state.y),
            _ => None,
        }
    }

    fn set_param(&mut self, id: ParamId, normalized: f32) -> bool {
        if !normalized.is_finite() {
            return false;
        }
        let v = normalized.clamp(0.0, 1.0);
        match id.0 {
            PARAM_X => {
                self.state.x = v;
                self.x.set_target(v);
            }
            PARAM_Y => {
                self.state.y = v;
                self.y.set_target(v);
            }
            _ => return false,
        }
        true
    }

    fn save_state(&self) -> Vec<u8> {
        self.state.to_json()
    }

    fn load_state(&mut self, bytes: &[u8]) -> Result<(), String> {
        let state = MorphRackState::from_json(bytes)?;
        *self = Self::new(state);
        Ok(())
    }
}

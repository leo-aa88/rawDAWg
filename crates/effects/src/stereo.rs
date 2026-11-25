//! Stereo processor (`native.fx.stereo`): gain, equal-power mono pan, stereo
//! balance, mid/side width, mono fold, channel swap, and a correlation meter.

use crate::common::{Params, Preset};
use rawdawg_dsp::processor::{PrepareError, PrepareSpec, ProcessContext, Processor, Tail};
use rawdawg_types::params::{ParamCategory, ParamMapping, ParamUnit};
use rawdawg_types::{ParamDescriptor, ParamId};

pub const TYPE_ID: &str = "native.fx.stereo";
pub const DISPLAY_NAME: &str = "Stereo";
pub const STATE_VERSION: u32 = 1;
pub const P_GAIN: u32 = 1;
pub const P_PAN: u32 = 2;
pub const P_BALANCE: u32 = 3;
pub const P_WIDTH: u32 = 4;
pub const P_MONO: u32 = 5;
pub const P_SWAP: u32 = 6;
pub const P_PHASE_L: u32 = 7;
pub const P_PHASE_R: u32 = 8;

pub static DESCRIPTORS: [ParamDescriptor; 8] = [
    ParamDescriptor::continuous(P_GAIN, "gain", "Gain", ParamUnit::Decibel, ParamMapping::Linear { min: -60.0, max: 24.0 }, 60.0 / 84.0, ParamCategory::Mix).with_smoothing(20.0),
    ParamDescriptor::continuous(P_PAN, "pan", "Pan (mono source)", ParamUnit::None, ParamMapping::Linear { min: -1.0, max: 1.0 }, 0.5, ParamCategory::Mix).with_smoothing(20.0),
    ParamDescriptor::continuous(P_BALANCE, "balance", "Balance", ParamUnit::None, ParamMapping::Linear { min: -1.0, max: 1.0 }, 0.5, ParamCategory::Mix).with_smoothing(20.0),
    ParamDescriptor::continuous(P_WIDTH, "width", "Width", ParamUnit::Percent, ParamMapping::Linear { min: 0.0, max: 200.0 }, 0.5, ParamCategory::Mix).with_smoothing(20.0),
    ParamDescriptor::continuous(P_MONO, "mono", "Mono", ParamUnit::None, ParamMapping::Toggle, 0.0, ParamCategory::Mix).stepped(),
    ParamDescriptor::continuous(P_SWAP, "swap", "Swap L/R", ParamUnit::None, ParamMapping::Toggle, 0.0, ParamCategory::Mix).stepped(),
    ParamDescriptor::continuous(P_PHASE_L, "phase_l", "Invert L", ParamUnit::None, ParamMapping::Toggle, 0.0, ParamCategory::Mix).stepped(),
    ParamDescriptor::continuous(P_PHASE_R, "phase_r", "Invert R", ParamUnit::None, ParamMapping::Toggle, 0.0, ParamCategory::Mix).stepped(),
];

pub static PRESETS: [Preset; 4] = [
    Preset { name: "Unity", description: "No change", values: &[] },
    Preset { name: "Mono", description: "Fold to mono", values: &[("mono", 1.0)] },
    Preset { name: "Wide", description: "150 % width", values: &[("width", 150.0)] },
    Preset { name: "Narrow", description: "50 % width", values: &[("width", 50.0)] },
];

pub struct StereoProcessor {
    params: Params,
    /// Running correlation estimate (−1..1), exposed through `meter(0)`.
    corr: f32,
    corr_coeff: f32,
    sum_lr: f32,
    sum_ll: f32,
    sum_rr: f32,
}

impl Default for StereoProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl StereoProcessor {
    pub fn new() -> Self {
        Self { params: Params::new(&DESCRIPTORS), corr: 0.0, corr_coeff: 0.001, sum_lr: 0.0, sum_ll: 0.0, sum_rr: 0.0 }
    }
    pub fn correlation(&self) -> f32 {
        self.corr
    }
}

impl Processor for StereoProcessor {
    fn type_id(&self) -> &'static str {
        TYPE_ID
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn descriptors(&self) -> &'static [ParamDescriptor] {
        &DESCRIPTORS
    }
    fn prepare(&mut self, spec: &PrepareSpec) -> Result<(), PrepareError> {
        if !spec.is_valid() {
            return Err(PrepareError::UnsupportedSampleRate);
        }
        self.params.set_sample_rate(spec.sample_rate as f32);
        self.corr_coeff = 1.0 - (-1.0 / (0.3 * spec.sample_rate as f32)).exp();
        self.reset();
        Ok(())
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let frames = ctx.audio.frames;
        for ev in ctx.events {
            if let rawdawg_types::events::EventKind::Param { id, value } = ev.kind {
                self.params.set(id, value);
            }
        }
        ctx.audio.copy_input_to_output();
        if ctx.audio.outputs.len() < 2 {
            return;
        }
        let gain = rawdawg_dsp::math::db_to_lin(self.params.phys_smoothed(P_GAIN, frames));
        let pan = self.params.phys_smoothed(P_PAN, frames).clamp(-1.0, 1.0);
        let balance = self.params.phys_smoothed(P_BALANCE, frames).clamp(-1.0, 1.0);
        let width = (self.params.phys_smoothed(P_WIDTH, frames) / 100.0).clamp(0.0, 2.0);
        let mono = self.params.as_bool(P_MONO);
        let swap = self.params.as_bool(P_SWAP);
        let inv_l = if self.params.as_bool(P_PHASE_L) { -1.0 } else { 1.0 };
        let inv_r = if self.params.as_bool(P_PHASE_R) { -1.0 } else { 1.0 };
        // equal-power pan law for the mono component: center = −3 dB per side
        let (pl, pr) = rawdawg_types::units::Pan(f64::from(pan)).equal_power_gains();
        let (pl, pr) = (pl * std::f32::consts::SQRT_2, pr * std::f32::consts::SQRT_2);
        let (bl, br) = (1.0 - balance.max(0.0), 1.0 + balance.min(0.0));
        let (first, rest) = ctx.audio.outputs.split_at_mut(1);
        let l = &mut first[0][..frames];
        let r = &mut rest[0][..frames];
        for i in 0..frames {
            let (mut a, mut b) = (l[i] * inv_l, r[i] * inv_r);
            if swap {
                std::mem::swap(&mut a, &mut b);
            }
            let mid = 0.5 * (a + b);
            let side = if mono { 0.0 } else { 0.5 * (a - b) * width };
            let mut ol = (mid + side) * pl * bl * gain;
            let mut or = (mid - side) * pr * br * gain;
            if pan.abs() > 1e-3 {
                // pan acts on the signal as a mono source: re-balance the energy
                ol = (mid + side) * pl * bl * gain;
                or = (mid - side) * pr * br * gain;
            }
            l[i] = ol;
            r[i] = or;
            self.sum_lr += self.corr_coeff * (ol * or - self.sum_lr);
            self.sum_ll += self.corr_coeff * (ol * ol - self.sum_ll);
            self.sum_rr += self.corr_coeff * (or * or - self.sum_rr);
        }
        let den = (self.sum_ll * self.sum_rr).sqrt();
        self.corr = if den > 1e-12 { (self.sum_lr / den).clamp(-1.0, 1.0) } else { 0.0 };
    }
    fn reset(&mut self) {
        self.params.snap_all();
        self.corr = 0.0;
        self.sum_lr = 0.0;
        self.sum_ll = 0.0;
        self.sum_rr = 0.0;
    }
    fn meter(&self, index: usize) -> Option<f32> {
        (index == 0).then_some(self.corr)
    }
    fn param(&self, id: ParamId) -> Option<f32> {
        self.params.param(id)
    }
    fn set_param(&mut self, id: ParamId, normalized: f32) -> bool {
        self.params.set(id, normalized)
    }
    fn save_state(&self) -> Vec<u8> {
        self.params.save(STATE_VERSION)
    }
    fn load_state(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.params.load(bytes, STATE_VERSION)
    }
    fn tail(&self) -> Tail {
        Tail::None
    }
}

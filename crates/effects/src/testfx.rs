//! Hidden test effect (`native.fx.test-latency`): delays by a fixed number of frames
//! and reports that latency. Used by the latency-compensation tests; never listed
//! in menus.

use crate::common::Params;
use rawdawg_dsp::delayline::DelayLine;
use rawdawg_dsp::processor::{PrepareError, PrepareSpec, ProcessContext, Processor, Tail};
use rawdawg_types::params::{ParamCategory, ParamMapping, ParamUnit};
use rawdawg_types::{ParamDescriptor, ParamId};

pub const TYPE_ID: &str = "native.fx.test-latency";
pub const P_LATENCY: u32 = 1;
pub static DESCRIPTORS: [ParamDescriptor; 1] =
    [ParamDescriptor::continuous(P_LATENCY, "latency", "Latency (frames)", ParamUnit::None, ParamMapping::Discrete { steps: 4097 }, 0.0, ParamCategory::Mix).stepped().hidden()];

pub struct LatencyTest {
    params: Params,
    lines: [DelayLine; 2],
    latency: u32,
}

impl Default for LatencyTest {
    fn default() -> Self {
        Self::new()
    }
}

impl LatencyTest {
    pub fn new() -> Self {
        Self { params: Params::new(&DESCRIPTORS), lines: [DelayLine::new(4096), DelayLine::new(4096)], latency: 0 }
    }
}

impl Processor for LatencyTest {
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
        self.latency = self.params.as_index(P_LATENCY) as u32;
        self.reset();
        Ok(())
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let frames = ctx.audio.frames;
        ctx.audio.copy_input_to_output();
        for (c, out) in ctx.audio.outputs.iter_mut().enumerate().take(2) {
            self.lines[c].delay_block(&mut out[..frames], self.latency as usize);
        }
    }
    fn reset(&mut self) {
        for l in &mut self.lines {
            l.reset();
        }
    }
    fn latency_frames(&self) -> u32 {
        self.latency
    }
    fn tail(&self) -> Tail {
        Tail::Frames(self.latency)
    }
    fn param(&self, id: ParamId) -> Option<f32> {
        self.params.param(id)
    }
    fn set_param(&mut self, id: ParamId, normalized: f32) -> bool {
        // latency is fixed after prepare (changing it would need a re-prepare)
        self.params.set(id, normalized)
    }
    fn save_state(&self) -> Vec<u8> {
        self.params.save(1)
    }
    fn load_state(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.params.load(bytes, 1)?;
        self.latency = self.params.as_index(P_LATENCY) as u32;
        Ok(())
    }
}

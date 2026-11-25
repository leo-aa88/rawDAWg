//! Native processing contract shared by the app, offline renderer and plugin
//! adapters (PLUGIN_API.md §1).

use crate::buffer::{AudioBlock, BusLayout};
use rawdawg_types::ParamDescriptor;
use rawdawg_types::events::TimedEvent;
use serde::{Deserialize, Serialize};

/// Non-RT preparation parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PrepareSpec {
    pub sample_rate: f64,
    /// Maximum `frames` any later `process` call will pass.
    pub max_frames: usize,
    pub layout: BusLayout,
}

impl PrepareSpec {
    pub fn new(sample_rate: f64, max_frames: usize, layout: BusLayout) -> Self {
        Self { sample_rate, max_frames, layout }
    }
    pub fn is_valid(&self) -> bool {
        self.sample_rate.is_finite() && self.sample_rate >= 8000.0 && self.sample_rate <= 384_000.0 && self.max_frames >= 1 && self.max_frames <= 65_536
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PrepareError {
    #[error("unsupported sample rate")]
    UnsupportedSampleRate,
    #[error("unsupported bus layout")]
    UnsupportedLayout,
    #[error("block size too large (max {max})")]
    BlockTooLarge { max: usize },
    #[error("resource budget exceeded: {0}")]
    BudgetExceeded(String),
    #[error("invalid state: {0}")]
    InvalidState(String),
}

/// Transport information visible to processors during `process`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransportView {
    pub playing: bool,
    /// Absolute frame of the first sample in this block (may be negative during count-in).
    pub block_start_frame: i64,
    pub tempo_bpm: f64,
    /// Musical position of the block start in quarter notes (fractional).
    pub position_quarters: f64,
    pub meter_numerator: u8,
    pub meter_denominator: u8,
    pub sample_rate: f64,
}

impl TransportView {
    pub fn stopped(sample_rate: f64) -> Self {
        Self { playing: false, block_start_frame: 0, tempo_bpm: 120.0, position_quarters: 0.0, meter_numerator: 4, meter_denominator: 4, sample_rate }
    }
}

/// Everything a processor sees during one bounded `process` call.
pub struct ProcessContext<'a> {
    pub audio: AudioBlock<'a>,
    /// Sorted by `frame`; every `frame < audio.frames`.
    pub events: &'a [TimedEvent],
    pub transport: &'a TransportView,
}

/// Tail length after input stops.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tail {
    /// No tail (e.g. gain).
    None,
    /// Finite tail in frames.
    Frames(u32),
    /// Depends on state in a way that cannot be bounded (e.g. feedback near unity).
    Unknown,
    /// Produces output indefinitely (e.g. generator, freeze).
    Infinite,
}

/// The native processor contract.
///
/// `prepare` is non-RT and may allocate. `process`, `reset`, `latency_frames`
/// and `tail` are RT-safe: bounded, allocation-free, never panicking on valid
/// input. State save/load and descriptors are non-RT.
pub trait Processor: Send {
    /// Stable type identifier (`native.synth`, `native.fx.reverb`, ...).
    fn type_id(&self) -> &'static str;
    /// Parameter catalog (static, no allocation).
    fn descriptors(&self) -> &'static [ParamDescriptor];
    fn prepare(&mut self, spec: &PrepareSpec) -> Result<(), PrepareError>;
    fn process(&mut self, ctx: &mut ProcessContext<'_>);
    fn reset(&mut self);
    fn latency_frames(&self) -> u32 {
        0
    }
    fn tail(&self) -> Tail {
        Tail::None
    }
    /// Number of currently sounding voices (instruments), for status display.
    fn active_voices(&self) -> u32 {
        0
    }
    /// Current normalized value of a parameter (non-RT query).
    fn param(&self, id: rawdawg_types::ParamId) -> Option<f32>;
    /// Set the base normalized value of a parameter (non-RT path; the RT path
    /// uses `EventKind::Param` events).
    fn set_param(&mut self, id: rawdawg_types::ParamId, normalized: f32) -> bool;
    /// Serialize state (non-RT). Versioned opaque bytes (JSON for native).
    fn save_state(&self) -> Vec<u8>;
    /// Restore state (non-RT). Invalid state leaves current state untouched.
    fn load_state(&mut self, bytes: &[u8]) -> Result<(), String>;
}

/// Helper: find the descriptor for an id in a static slice.
pub fn find_descriptor(descs: &'static [ParamDescriptor], id: rawdawg_types::ParamId) -> Option<&'static ParamDescriptor> {
    descs.iter().find(|d| d.id == id)
}

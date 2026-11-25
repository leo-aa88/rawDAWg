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
pub trait Processor: Send + std::any::Any {
    /// Stable type identifier (`native.synth`, `native.fx.reverb`, ...).
    fn type_id(&self) -> &'static str;
    /// Downcast support for state migration between instances of the same type.
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
    /// RT-safe, bounded: take over live state (voices, queues, tables) from `old`,
    /// an instance of the same type prepared with the same spec. Default: nothing.
    fn migrate_from(&mut self, _old: &mut dyn Processor) {}
    /// `true` when the live audio state lives outside the plan and follows the main-thread
    /// instance regardless of the project's stored state (hosted plugins): the compiler then
    /// always migrates such a node instead of restarting it. Default: `false`.
    fn carries_live_state(&self) -> bool {
        false
    }
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
    /// Optional meter values (gain reduction in dB, correlation, …) read by the UI
    /// off-thread; `None` when the index is not provided.
    fn meter(&self, _index: usize) -> Option<f32> {
        None
    }
    /// Current normalized value of a parameter (non-RT query).
    fn param(&self, id: rawdawg_types::ParamId) -> Option<f32>;
    /// Display text the processor itself supplies for `normalized` (non-RT, main thread).
    /// Hosted plugins answer with their own formatting; `None` means the caller formats
    /// with the descriptor ([`display_value`]). Default: `None`.
    fn param_text(&self, _id: rawdawg_types::ParamId, _normalized: f32) -> Option<String> {
        None
    }
    /// Set the base normalized value of a parameter (non-RT path; the RT path
    /// uses `EventKind::Param` events).
    fn set_param(&mut self, id: rawdawg_types::ParamId, normalized: f32) -> bool;
    /// Serialize state (non-RT). Versioned opaque bytes (JSON for native).
    fn save_state(&self) -> Vec<u8>;
    /// Restore state (non-RT). Invalid state leaves current state untouched.
    fn load_state(&mut self, bytes: &[u8]) -> Result<(), String>;
}

/// Readout for a parameter value: the processor's own text when it supplies one (hosted
/// plugins), otherwise the descriptor's formatting.
pub fn display_value(p: &dyn Processor, d: &ParamDescriptor, normalized: f32) -> String {
    p.param_text(d.id, normalized).unwrap_or_else(|| d.format(normalized))
}

/// Make text from untrusted code safe to show in one label: control characters become
/// spaces, surrounding whitespace is trimmed, and at most `max_chars` characters are kept
/// (an ellipsis marks the cut). Empty results are `None`.
pub fn sanitize_display_text(text: &str, max_chars: usize) -> Option<String> {
    let cleaned: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let t = cleaned.trim();
    if t.is_empty() {
        return None;
    }
    if t.chars().count() <= max_chars {
        return Some(t.to_owned());
    }
    let mut cut: String = t.chars().take(max_chars.saturating_sub(1)).collect();
    cut.push('…');
    Some(cut)
}

/// Helper: find the descriptor for an id in a static slice.
pub fn find_descriptor(descs: &'static [ParamDescriptor], id: rawdawg_types::ParamId) -> Option<&'static ParamDescriptor> {
    descs.iter().find(|d| d.id == id)
}

#[cfg(test)]
mod tests {
    use super::sanitize_display_text;

    #[test]
    fn display_text_from_plugins_is_cleaned_and_bounded() {
        assert_eq!(sanitize_display_text("  -6.0 dB ", 32).as_deref(), Some("-6.0 dB"));
        assert_eq!(sanitize_display_text("a\nb\tc\u{7}", 32).as_deref(), Some("a b c"));
        assert_eq!(sanitize_display_text(" \n\t", 32), None);
        assert_eq!(sanitize_display_text("", 32), None);
        let long = "x".repeat(100);
        let cut = sanitize_display_text(&long, 10).unwrap();
        assert_eq!(cut.chars().count(), 10);
        assert!(cut.ends_with('…'));
        // multi-byte characters are cut on character boundaries
        assert_eq!(sanitize_display_text("ééééé", 3).as_deref(), Some("éé…"));
    }
}

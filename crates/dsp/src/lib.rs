//! Custom DSP for rawDAWg. Independent of UI, devices and plugin SDKs.
//!
//! Real-time rules (ARCHITECTURE.md §3) apply to every `process`/`reset`
//! implementation in this crate: no allocation, locking, I/O or panics.

// DSP generators expose `next()` producing one sample; they are deliberately not
// `Iterator`s (infinite, stateful, parameterized per call).
#![allow(clippy::should_implement_trait)]

pub mod analysis;
pub mod biquad;
pub mod buffer;
pub mod delayline;
pub mod envelope;
pub mod filter;
pub mod limiter;
pub mod math;
pub mod oscillator;
pub mod processor;
pub mod rng;
pub mod smooth;
pub mod wavetable;

pub use buffer::{AudioBlock, AudioBuffer, BusLayout};
pub use processor::{PrepareError, PrepareSpec, ProcessContext, Processor, Tail, TransportView};

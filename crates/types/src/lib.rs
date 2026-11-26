//! Core vocabulary shared by every rawDAWg crate.
//!
//! This crate deliberately has no dependency on the GUI, audio devices, file
//! formats or plugin SDKs. It defines:
//!
//! * stable identifiers ([`ids`]),
//! * musical and audio units with checked arithmetic ([`units`]),
//! * immutable timed events used by the sequencer and processors ([`events`]),
//! * parameter descriptors with normalized/physical mapping ([`params`]),
//! * engine budgets ([`limits`]).

pub mod b64;
pub mod events;
pub mod ids;
pub mod limits;
pub mod params;
pub mod units;

pub use events::{EventKind, EventSource, TimedEvent};
pub use ids::*;
pub use params::{ParamDescriptor, ParamFlags, ParamId, ParamMapping, ParamUnit};
pub use units::{Bpm, Db, Frames, PPQ, Pan, SampleRate, Tick, UsPerQuarter};

/// Product display brand (case-sensitive). Fixed by the specification.
pub const PRODUCT_NAME: &str = "rawDAWg";
/// Product tagline. Fixed by the specification.
pub const TAGLINE: &str = "Less thinking. More noise.";
/// Executable / package identifier.
pub const PACKAGE_NAME: &str = "rawdawg";
/// Project directory suffix (including the dot).
pub const PROJECT_SUFFIX: &str = ".rawdawg";
/// Preset exchange file suffix (including the dot).
pub const PRESET_SUFFIX: &str = ".rawdawgpreset";
/// Custom MIME type for project folders / markers.
pub const PROJECT_MIME: &str = "application/x-rawdawg-project";
/// Application version from the workspace manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

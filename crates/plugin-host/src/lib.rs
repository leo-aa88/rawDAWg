//! rawDAWg third-party plugin hosting (PLUGIN_API.md §3, PLUG-02/03).
//!
//! - [`catalog`]: what was found where, fingerprints, quarantine, user roots.
//! - [`scan`]: discovery in standard/user directories and the isolated scanner
//!   subprocess (timeout, bounded output, crash/timeout quarantine).
//! - [`describe`]: in-process module description, run inside the scanner child.
//! - [`clap`] / [`vst3`]: the in-process bridges (main-thread instance + realtime half).
//! - [`host`]: the clack host handlers rawDAWg presents to plugins.
//! - [`marker`]: crash marker telling the next start which plugins were live.
//!
//! Native plugin code executes as the user; scanner isolation is crash
//! containment, not a security sandbox.

/// Base64 for opaque state (moved to `rawdawg-types`; re-exported for existing callers).
pub use rawdawg_types::b64;
pub mod catalog;
pub mod clap;
pub mod describe;
pub mod host;
pub mod hosted;
pub mod main_thread;
pub mod marker;
pub mod scan;
pub mod vst3;

pub use catalog::{Catalog, Format, PluginDescription, QuarantineReason, QuarantineRecord, ScannedLibrary};
pub use clap::{ClapInstance, ClapProcessor, ModuleCache};
pub use hosted::{HostedInstance, hosted_mut, is_hosted};
pub use scan::{ScanOptions, ScanOutcome, ScanReport};
pub use vst3::{Vst3Instance, Vst3ModuleCache, Vst3Processor};

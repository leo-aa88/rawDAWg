//! Format-neutral plugin export contract (PLUGIN_API.md §1–2, PLUG-01).
//!
//! - [`exported`]: which native processors ship as plugins, under which stable
//!   external ids (CLAP ids, VST3 class ids, feature/category tags) and names.
//! - [`wrap`]: the effect wrapper every format adapter shares — latency-aligned
//!   true bypass, wet/dry mix with a delayed dry path, the two wrapper parameters
//!   and the versioned state envelope — so a preset saved in one host loads in the
//!   other and in the app.
//!
//! No format ABI code lives here; `plugin-clap` and `plugin-vst3` are thin adapters.

pub mod exported;
pub mod wrap;

pub use exported::{ExportedPlugin, Kind, exported_plugins};
pub use wrap::{EffectWrapper, STATE_KEY, WRAP_BYPASS, WRAP_MIX};

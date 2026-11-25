//! rawDAWg flagship synthesizer (`native.synth`).
//!
//! M1 scope: layered bandlimited oscillators (basic waves and an editable
//! wavetable cycle), per-voice ADSR amplitude and filter envelopes, SVF filter,
//! poly/mono/legato voice modes, voice stealing with preallocated fade tails,
//! source-isolated note registry with sustain, pitch bend, tuning.
//! Later milestones add the remaining generators, modulation and mutation.

pub mod params;
pub mod patch;
pub mod registry;
pub mod sculpt;
pub mod synth;
pub mod tables;
pub mod voice;

pub use patch::{Generator, Layer, Patch, VoiceMode};
pub use synth::Synth;
pub use tables::{TableInbox, TableSender, TableUpdate, table_channel};

/// Stable native type identifier.
pub const TYPE_ID: &str = rawdawg_types::ids::native_types::SYNTH;
/// Display name used in the app and exported plugins.
pub const DISPLAY_NAME: &str = "rawDAWg Synth";

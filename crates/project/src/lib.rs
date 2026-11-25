//! Editable project model (PROJECT_FORMAT.md). Pure data: no DSP objects,
//! no runtime pointers, no device or GUI dependencies.

pub mod assets;
pub mod commands;
pub mod model;
pub mod persist;
pub mod preset;
pub mod validate;

pub use commands::{Command, CommandError, History};
pub use model::*;
pub use persist::{PersistError, load_project_dir, save_project_dir};
pub use validate::{ValidationError, validate};

/// Current schema version written by this build.
pub const SCHEMA_VERSION: u32 = 1;
/// Maximum accepted `project.json` size in bytes.
pub const MAX_PROJECT_JSON_BYTES: usize = 64 * 1024 * 1024;

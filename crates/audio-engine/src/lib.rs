//! Audio engine: compiles a project into a prepared, bounded render plan and
//! renders it either in real time (behind `RenderSource`) or offline to a file.
//! The same `Engine::render_block` is used by both paths.

pub mod assets;
pub mod commands;
pub mod engine;
pub mod offline;
pub mod plan;
pub mod record;
pub mod schedule;

pub use assets::{AssetResolver, NoAssets, ProjectAssetCache};
pub use commands::{EngineCommand, RecordedEvent, TransportMode};
pub use engine::{Engine, EngineHandle, EngineStatus};
pub use offline::{OfflineOptions, RenderStats, render_project, render_project_to_wav};
pub use plan::{CompileError, CompileOutput, Plan, compile, fingerprint_state};
pub use record::Take;

//! rawDAWg shared UI (ADR 0006).
//!
//! - [`theme`]: the dark-only visual tokens used by the app and every first-party editor.
//! - [`params`]: parameter widgets shared by the app's panels and the plugin editors.
//! - [`raster`]: a software painter for egui output (no GPU).
//! - [`window`]: native child / top-level windows (X11 on Linux, Win32 on Windows).
//! - [`editor`]: window events → egui → painter → window, ticked on the host's UI thread.
//! - [`plugin_editor`]: the editor rawDAWg's exported CLAP/VST3 plugins show in other hosts.

pub mod editor;
pub mod params;
pub mod plugin_editor;
pub mod raster;
pub mod theme;
pub mod window;

pub use editor::{Editor, EditorUi, Tick, render_offscreen};
pub use plugin_editor::{ParamPort, PluginEditorUi};

//! Crash marker: while third-party plugins are loaded in-process, a small file
//! lists them. A marker present at startup means the previous session ended
//! without a clean shutdown while those plugins were live; the app then offers
//! safe mode (PLUGIN_API.md §3).

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    /// Stable plugin ids (`clap:<id>`) that were loaded when the marker was written.
    pub plugins: Vec<String>,
    pub written_at_unix: i64,
}

impl Marker {
    pub fn read(path: &Path) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        if bytes.len() > 1024 * 1024 {
            return None;
        }
        serde_json::from_slice(&bytes).ok()
    }

    pub fn write(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let bytes = serde_json::to_vec(self).map_err(|e| e.to_string())?;
        std::fs::write(path, bytes).map_err(|e| e.to_string())
    }

    /// Remove the marker (clean shutdown or user acknowledgement).
    pub fn clear(path: &Path) {
        let _ = std::fs::remove_file(path);
    }
}

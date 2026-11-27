//! Per-user preferences (OS config dir). Never required to open a project.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preferences {
    pub audio_device_id: Option<String>,
    pub sample_rate_hz: Option<u32>,
    pub buffer_frames: Option<u32>,
    #[serde(default)]
    pub midi_enabled: Vec<String>,
    #[serde(default)]
    pub typing_octave: i32,
    #[serde(default = "default_velocity")]
    pub typing_velocity: f32,
    #[serde(default)]
    pub recent_projects: Vec<PathBuf>,
    /// Count-in before recording, in bars (0 = none).
    #[serde(default = "default_count_in")]
    pub count_in_bars: u32,
    /// Recording offset in milliseconds (positive = recorded notes are moved earlier).
    #[serde(default)]
    pub record_offset_ms: f32,
    /// Open an input device for recording (off until the owner enables it).
    #[serde(default)]
    pub input_enabled: bool,
    pub input_device_id: Option<String>,
    #[serde(default)]
    pub input_channels: rawdawg_audio_io::capture::InputChannels,
    /// Route the live input into the mixer (off by default; beware of feedback).
    #[serde(default)]
    pub monitor_input: bool,
    /// Browser favorites: asset ids and effect type ids.
    #[serde(default)]
    pub favorites: Vec<String>,
}

fn default_count_in() -> u32 {
    1
}

fn default_velocity() -> f32 {
    0.8
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            audio_device_id: None,
            sample_rate_hz: None,
            buffer_frames: None,
            midi_enabled: vec![],
            typing_octave: 0,
            typing_velocity: default_velocity(),
            recent_projects: vec![],
            count_in_bars: 1,
            record_offset_ms: 0.0,
            input_enabled: false,
            input_device_id: None,
            input_channels: rawdawg_audio_io::capture::InputChannels::default(),
            monitor_input: false,
            favorites: Vec::new(),
        }
    }
}

impl Preferences {
    pub fn path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", rawdawg_types::PACKAGE_NAME).map(|d| d.config_dir().join("preferences.json"))
    }

    pub fn load() -> Self {
        let Some(p) = Self::path() else { return Self::default() };
        let mut prefs: Self = match std::fs::read(&p) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                log::warn!("preferences unreadable ({e}); using defaults");
                Self::default()
            }),
            Err(_) => Self::default(),
        };
        prefs.normalize_recent();
        prefs
    }

    pub fn save(&self) -> Result<(), String> {
        let p = Self::path().ok_or("no config directory available")?;
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
    }

    /// Put a project first in the recent list, as an absolute path (a relative one would only
    /// open again from the folder rawDAWg was started in).
    pub fn remember_project(&mut self, path: PathBuf) {
        let path = absolute(&path);
        self.recent_projects.retain(|p| p != &path);
        self.recent_projects.insert(0, path);
        self.recent_projects.truncate(10);
    }

    /// Entries saved before paths were made absolute: keep the ones that still resolve from here
    /// (as absolute paths), drop the ones that cannot be found.
    pub fn normalize_recent(&mut self) {
        let mut seen = Vec::new();
        for p in std::mem::take(&mut self.recent_projects) {
            let p = if p.is_absolute() {
                p
            } else if p.exists() {
                absolute(&p)
            } else {
                continue;
            };
            if !seen.contains(&p) {
                seen.push(p);
            }
        }
        self.recent_projects = seen;
    }
}

/// `path` as an absolute path (relative to the current folder), with `.` and `..` folded so one
/// folder has one spelling in the recent list. Symlinks are kept as they are.
pub fn absolute(path: &std::path::Path) -> PathBuf {
    use std::path::Component;
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

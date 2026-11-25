//! Plugin catalog: what was found where, with fingerprints and quarantine records
//! (PLUGIN_API.md §3). Stored in the per-user preferences directory, never in projects.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const CATALOG_VERSION: u32 = 1;
/// Default per-plugin scan timeout (PLUGIN_API.md §3).
pub const DEFAULT_TIMEOUT_SECS: u64 = 15;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    Clap,
    Vst3,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::Clap => "CLAP",
            Format::Vst3 => "VST3",
        }
    }
    /// File extension of a module of this format (case-insensitive match).
    pub fn extension(self) -> &'static str {
        match self {
            Format::Clap => "clap",
            Format::Vst3 => "vst3",
        }
    }
}

/// Cheap change detection for a module file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    pub size: u64,
    pub mtime_unix: i64,
}

impl Fingerprint {
    pub fn of(path: &Path) -> Option<Self> {
        let md = std::fs::metadata(path).ok()?;
        let mtime = md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0);
        Some(Self { size: md.len(), mtime_unix: mtime })
    }
}

/// One plugin inside a module, as reported by the isolated scanner.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginDescription {
    pub format: Format,
    pub plugin_id: String,
    pub name: String,
    #[serde(default)]
    pub vendor: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub features: Vec<String>,
    pub is_instrument: bool,
    pub is_effect: bool,
    #[serde(default)]
    pub audio_inputs: u32,
    #[serde(default)]
    pub audio_outputs: u32,
    #[serde(default)]
    pub main_input_channels: u32,
    #[serde(default)]
    pub main_output_channels: u32,
    #[serde(default)]
    pub note_inputs: u32,
    #[serde(default)]
    pub param_count: u32,
    #[serde(default)]
    pub has_gui: bool,
    /// Problems met while describing (instantiation failed, …); the plugin stays listed.
    #[serde(default)]
    pub notes: Vec<String>,
}

/// A scanned module and the plugins it contains.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScannedLibrary {
    pub path: PathBuf,
    pub fingerprint: Fingerprint,
    pub plugins: Vec<PluginDescription>,
    pub scanned_at_unix: i64,
    /// CLAP version string reported by the entry (`"1.2.3"`), when known.
    #[serde(default)]
    pub api_version: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QuarantineReason {
    /// The scanner process died (signal or non-zero exit without a report).
    Crash { exit_code: Option<i32>, stderr: String },
    /// The scanner did not finish within the timeout.
    Timeout { secs: u64 },
    /// The module loaded but is not a usable plugin (no entry, init failed, …).
    LoadFailed { message: String },
    /// The scanner produced output we could not parse (or too much of it).
    InvalidOutput { message: String },
}

impl QuarantineReason {
    pub fn summary(&self) -> String {
        match self {
            QuarantineReason::Crash { exit_code: Some(c), .. } => format!("scanner crashed (exit code {c})"),
            QuarantineReason::Crash { exit_code: None, .. } => "scanner crashed (killed by a signal)".to_owned(),
            QuarantineReason::Timeout { secs } => format!("scan timed out after {secs} s"),
            QuarantineReason::LoadFailed { message } => format!("load failed: {message}"),
            QuarantineReason::InvalidOutput { message } => format!("invalid scanner output: {message}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuarantineRecord {
    pub path: PathBuf,
    pub fingerprint: Option<Fingerprint>,
    pub reason: QuarantineReason,
    pub at_unix: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    pub version: u32,
    #[serde(default)]
    pub libraries: Vec<ScannedLibrary>,
    #[serde(default)]
    pub quarantine: Vec<QuarantineRecord>,
    /// User-added search roots (in addition to the standard locations).
    #[serde(default)]
    pub user_paths: Vec<PathBuf>,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// When the last full scan finished (0 = never).
    #[serde(default)]
    pub last_scan_unix: i64,
}

fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_SECS
}

impl Default for Catalog {
    fn default() -> Self {
        Self { version: CATALOG_VERSION, libraries: Vec::new(), quarantine: Vec::new(), user_paths: Vec::new(), timeout_secs: DEFAULT_TIMEOUT_SECS, last_scan_unix: 0 }
    }
}

pub fn now_unix() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Maximum catalog file we accept (defensive; a catalog is a few hundred KiB at most).
const MAX_CATALOG_BYTES: u64 = 16 * 1024 * 1024;

impl Catalog {
    /// Load, or start empty when absent/invalid (an unreadable catalog is logged, not fatal).
    pub fn load(path: &Path) -> Self {
        let Ok(md) = std::fs::metadata(path) else { return Self::default() };
        if md.len() > MAX_CATALOG_BYTES {
            log::warn!("plugin catalog {} is too large ({} bytes); starting empty", path.display(), md.len());
            return Self::default();
        }
        match std::fs::read(path).map_err(|e| e.to_string()).and_then(|b| serde_json::from_slice::<Catalog>(&b).map_err(|e| e.to_string())) {
            Ok(c) if c.version == CATALOG_VERSION => c,
            Ok(c) => {
                log::warn!("plugin catalog version {} unsupported; rescanning", c.version);
                Self { user_paths: c.user_paths, timeout_secs: c.timeout_secs, ..Self::default() }
            }
            Err(e) => {
                log::warn!("plugin catalog {} unreadable ({e}); starting empty", path.display());
                Self::default()
            }
        }
    }

    /// Atomic write (temp file + rename) next to `path`.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }

    /// Every plugin of every scanned module (quarantined modules have none).
    pub fn plugins(&self) -> impl Iterator<Item = (&ScannedLibrary, &PluginDescription)> {
        self.libraries.iter().flat_map(|l| l.plugins.iter().map(move |p| (l, p)))
    }

    /// Resolve a plugin by stable id (and format), preferring the module at `path_hint`.
    pub fn find(&self, format: Format, plugin_id: &str, path_hint: Option<&Path>) -> Option<(&ScannedLibrary, &PluginDescription)> {
        let mut matches = self.plugins().filter(|(_, p)| p.format == format && p.plugin_id == plugin_id);
        match path_hint {
            Some(hint) => {
                let all: Vec<_> = matches.collect();
                all.iter().find(|(l, _)| l.path == hint).or_else(|| all.first()).copied()
            }
            None => matches.next(),
        }
    }

    pub fn is_quarantined(&self, path: &Path) -> bool {
        self.quarantine.iter().any(|q| q.path == path)
    }

    pub fn quarantined(&self, path: &Path) -> Option<&QuarantineRecord> {
        self.quarantine.iter().find(|q| q.path == path)
    }

    /// Whether `path` needs (re)scanning: unknown, or its fingerprint changed. Quarantined
    /// modules are rescanned only when their file changed (user retry clears the record).
    pub fn needs_scan(&self, path: &Path) -> bool {
        let current = Fingerprint::of(path);
        if let Some(q) = self.quarantined(path) {
            return q.fingerprint.is_some() && current.is_some() && q.fingerprint != current;
        }
        match self.libraries.iter().find(|l| l.path == path) {
            Some(l) => Some(l.fingerprint) != current,
            None => true,
        }
    }

    pub fn insert_library(&mut self, lib: ScannedLibrary) {
        self.quarantine.retain(|q| q.path != lib.path);
        self.libraries.retain(|l| l.path != lib.path);
        self.libraries.push(lib);
        self.libraries.sort_by(|a, b| a.path.cmp(&b.path));
    }

    pub fn insert_quarantine(&mut self, rec: QuarantineRecord) {
        self.libraries.retain(|l| l.path != rec.path);
        self.quarantine.retain(|q| q.path != rec.path);
        self.quarantine.push(rec);
    }

    /// User action "retry": drop the quarantine record so the next scan tries again.
    pub fn clear_quarantine(&mut self, path: &Path) -> bool {
        let before = self.quarantine.len();
        self.quarantine.retain(|q| q.path != path);
        before != self.quarantine.len()
    }

    /// Forget a module entirely (scanned or quarantined).
    pub fn remove(&mut self, path: &Path) -> bool {
        let before = self.libraries.len() + self.quarantine.len();
        self.libraries.retain(|l| l.path != path);
        self.quarantine.retain(|q| q.path != path);
        before != self.libraries.len() + self.quarantine.len()
    }

    /// Drop entries whose module no longer exists on disk.
    pub fn prune_missing(&mut self) -> usize {
        let before = self.libraries.len() + self.quarantine.len();
        self.libraries.retain(|l| l.path.is_file());
        self.quarantine.retain(|q| q.path.is_file());
        before - (self.libraries.len() + self.quarantine.len())
    }
}

//! Asset resolution for instrument compilation. Decoding happens here, off the
//! audio thread; processors receive immutable `Arc`s.

use rawdawg_project::Project;
use rawdawg_sampler::SampleAsset;
use rawdawg_sampler::sf2::SoundFont;
use rawdawg_types::AssetId;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Resolve project assets by content id. Implementations must be cheap on
/// repeated calls (cache) and never panic on missing/corrupt files.
pub trait AssetResolver {
    /// Resident decoded sample, or `None` when missing, corrupt, or too long to live in
    /// memory (then `stream_info` describes it for streaming playback).
    fn sample(&self, id: &AssetId) -> Option<Arc<SampleAsset>>;
    fn soundfont(&self, id: &AssetId) -> Option<Arc<SoundFont>>;
    /// Rate/channels/length and path of an audio asset for streaming (long files).
    fn stream_info(&self, _id: &AssetId) -> Option<rawdawg_sampler::stream::StreamInfo> {
        None
    }
    /// Human-readable problems encountered while resolving (missing/corrupt).
    fn problems(&self) -> Vec<String> {
        vec![]
    }
}

/// Resolver with no assets (tests, projects without media).
pub struct NoAssets;

impl AssetResolver for NoAssets {
    fn sample(&self, _: &AssetId) -> Option<Arc<SampleAsset>> {
        None
    }
    fn soundfont(&self, _: &AssetId) -> Option<Arc<SoundFont>> {
        None
    }
}

/// Longest asset decoded whole (per asset): 2 minutes at the file's rate.
pub const RESIDENT_MAX_SECONDS: f64 = 120.0;
/// Total resident decoded audio across assets (frames × channels), ≈ 230 MB of f32.
pub const RESIDENT_BUDGET_SAMPLES: usize = 60_000_000;

/// Decodes and caches a project folder's assets.
pub struct ProjectAssetCache {
    project_dir: Option<PathBuf>,
    paths: HashMap<AssetId, PathBuf>,
    names: HashMap<AssetId, String>,
    samples: Mutex<HashMap<AssetId, Option<Arc<SampleAsset>>>>,
    fonts: Mutex<HashMap<AssetId, Option<Arc<SoundFont>>>>,
    infos: Mutex<HashMap<AssetId, Option<rawdawg_sampler::stream::StreamInfo>>>,
    problems: Mutex<Vec<String>>,
}

impl ProjectAssetCache {
    pub fn new(project_dir: Option<PathBuf>) -> Self {
        Self {
            project_dir,
            paths: HashMap::new(),
            names: HashMap::new(),
            samples: Mutex::new(HashMap::new()),
            fonts: Mutex::new(HashMap::new()),
            infos: Mutex::new(HashMap::new()),
            problems: Mutex::new(Vec::new()),
        }
    }

    /// Point the cache at a (possibly new) project; keeps decoded data for ids that still resolve.
    pub fn sync(&mut self, project_dir: Option<PathBuf>, project: &Project) {
        if self.project_dir != project_dir {
            self.samples.lock().map(|mut m| m.clear()).ok();
            self.fonts.lock().map(|mut m| m.clear()).ok();
            self.infos.lock().map(|mut m| m.clear()).ok();
            self.project_dir = project_dir;
        }
        self.paths.clear();
        self.names.clear();
        if let Some(dir) = &self.project_dir {
            for a in &project.assets {
                if let Some(p) = rawdawg_project::assets::asset_path(dir, a) {
                    self.paths.insert(a.id.clone(), p);
                    self.names.insert(a.id.clone(), a.source_name.clone());
                }
            }
        }
        self.problems.lock().map(|mut p| p.clear()).ok();
    }

    /// Register a decoded sample directly (e.g. right after import) to avoid a second decode.
    pub fn insert_sample(&self, id: AssetId, asset: Arc<SampleAsset>) {
        if let Ok(mut m) = self.samples.lock() {
            m.insert(id, Some(asset));
        }
    }

    pub fn project_dir(&self) -> Option<&PathBuf> {
        self.project_dir.as_ref()
    }

    fn note(&self, msg: String) {
        if let Ok(mut p) = self.problems.lock()
            && !p.contains(&msg)
        {
            p.push(msg);
        }
    }
}

impl AssetResolver for ProjectAssetCache {
    fn sample(&self, id: &AssetId) -> Option<Arc<SampleAsset>> {
        if let Ok(m) = self.samples.lock()
            && let Some(v) = m.get(id)
        {
            return v.clone();
        }
        // resident limits: per-asset length and the total budget (long files stream instead)
        if let Some(info) = self.stream_info(id) {
            let seconds = info.frames as f64 / f64::from(info.sample_rate.max(1));
            let used: usize = self.samples.lock().map(|m| m.values().flatten().map(|a| a.frames() * a.channels.len()).sum()).unwrap_or(0);
            let need = info.frames as usize * usize::from(info.channels.clamp(1, 2));
            if seconds > RESIDENT_MAX_SECONDS || used + need > RESIDENT_BUDGET_SAMPLES {
                self.note(format!(
                    "{} ({seconds:.0} s) is streamed from disk rather than loaded into memory",
                    info.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
                ));
                return None;
            }
        }
        let result = match self.paths.get(id) {
            Some(path) => match rawdawg_sampler::decode_file(path) {
                Ok(a) => Some(match self.names.get(id) {
                    Some(n) if !n.is_empty() => Arc::new(SampleAsset { name: n.clone(), sample_rate: a.sample_rate, channels: a.channels.clone() }),
                    _ => a,
                }),
                Err(e) => {
                    self.note(format!("sample {} could not be decoded: {e}", path.display()));
                    None
                }
            },
            None => {
                self.note(format!("sample {} is missing from the project (relink needed)", &id.as_hex()[..12]));
                None
            }
        };
        if let Ok(mut m) = self.samples.lock() {
            m.insert(id.clone(), result.clone());
        }
        result
    }

    fn stream_info(&self, id: &AssetId) -> Option<rawdawg_sampler::stream::StreamInfo> {
        if let Ok(m) = self.infos.lock()
            && let Some(v) = m.get(id)
        {
            return v.clone();
        }
        let result = self.paths.get(id).and_then(|path| match rawdawg_sampler::stream::probe(path) {
            Ok(info) => Some(info),
            Err(e) => {
                self.note(format!("{} could not be probed: {e}", path.display()));
                None
            }
        });
        if let Ok(mut m) = self.infos.lock() {
            m.insert(id.clone(), result.clone());
        }
        result
    }

    fn soundfont(&self, id: &AssetId) -> Option<Arc<SoundFont>> {
        if let Ok(m) = self.fonts.lock()
            && let Some(v) = m.get(id)
        {
            return v.clone();
        }
        let result = match self.paths.get(id) {
            Some(path) => match std::fs::read(path).map_err(|e| e.to_string()).and_then(|bytes| rawdawg_sampler::sf2::parse(&bytes).map_err(|e| e.to_string())) {
                Ok(f) => {
                    for w in f.warnings() {
                        self.note(format!("SoundFont {}: {w}", f.name));
                    }
                    Some(Arc::new(f))
                }
                Err(e) => {
                    self.note(format!("SoundFont {} could not be loaded: {e}", path.display()));
                    None
                }
            },
            None => {
                self.note(format!("SoundFont {} is missing from the project (relink needed)", &id.as_hex()[..12]));
                None
            }
        };
        if let Ok(mut m) = self.fonts.lock() {
            m.insert(id.clone(), result.clone());
        }
        result
    }

    fn problems(&self) -> Vec<String> {
        self.problems.lock().map(|p| p.clone()).unwrap_or_default()
    }
}

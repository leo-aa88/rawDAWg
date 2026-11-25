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
    fn sample(&self, id: &AssetId) -> Option<Arc<SampleAsset>>;
    fn soundfont(&self, id: &AssetId) -> Option<Arc<SoundFont>>;
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

/// Decodes and caches a project folder's assets.
pub struct ProjectAssetCache {
    project_dir: Option<PathBuf>,
    paths: HashMap<AssetId, PathBuf>,
    names: HashMap<AssetId, String>,
    samples: Mutex<HashMap<AssetId, Option<Arc<SampleAsset>>>>,
    fonts: Mutex<HashMap<AssetId, Option<Arc<SoundFont>>>>,
    problems: Mutex<Vec<String>>,
}

impl ProjectAssetCache {
    pub fn new(project_dir: Option<PathBuf>) -> Self {
        Self { project_dir, paths: HashMap::new(), names: HashMap::new(), samples: Mutex::new(HashMap::new()), fonts: Mutex::new(HashMap::new()), problems: Mutex::new(Vec::new()) }
    }

    /// Point the cache at a (possibly new) project; keeps decoded data for ids that still resolve.
    pub fn sync(&mut self, project_dir: Option<PathBuf>, project: &Project) {
        if self.project_dir != project_dir {
            self.samples.lock().map(|mut m| m.clear()).ok();
            self.fonts.lock().map(|mut m| m.clear()).ok();
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

//! Directory project persistence with atomic manifest replacement.

use crate::model::Project;
use crate::validate::{ValidationError, validate};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const PROJECT_FILE: &str = "project.json";
/// Fixed-name marker written into every saved project folder so desktops can associate a
/// *file* with rawDAWg (folders cannot be associated by suffix). Opening it resolves to the
/// sibling `project.json`; its content is informational only and never read as a path.
pub const PROJECT_MARKER: &str = "project.rawdawg";
const MARKER_TEXT: &str = "rawDAWg project. Open this file (or the folder) with rawDAWg; the project itself is project.json next to it.\n";
pub const ASSETS_DIR: &str = "assets/sha256";
pub const PLUGIN_STATE_DIR: &str = "plugin-state";
pub const RECOVERY_DIR: &str = "recovery";
pub const CACHE_DIR: &str = "cache";
const PREVIOUS_MANIFEST: &str = "recovery/project.previous.json";

#[derive(Debug, thiserror::Error)]
pub enum PersistError {
    #[error("I/O error at {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("{0} already exists and is not empty")]
    Collision(PathBuf),
    #[error("project file too large ({bytes} bytes, max {max})")]
    TooLarge { bytes: u64, max: usize },
    #[error("project.json is not valid JSON: {0}")]
    Json(String),
    #[error("project is invalid: {0}")]
    Invalid(#[from] ValidationError),
    #[error("{0} is not a project directory (missing project.json)")]
    NotAProject(PathBuf),
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> PersistError + '_ {
    move |source| PersistError::Io { path: path.to_path_buf(), source }
}

/// Resolve a user-supplied path (directory, `project.json`, or marker) to the project directory.
pub fn resolve_project_dir(path: &Path) -> Result<PathBuf, PersistError> {
    if path.is_dir() {
        if path.join(PROJECT_FILE).is_file() {
            return Ok(path.to_path_buf());
        }
        return Err(PersistError::NotAProject(path.to_path_buf()));
    }
    if path.file_name().is_some_and(|n| n == PROJECT_FILE || n == PROJECT_MARKER)
        && let Some(parent) = path.parent()
        && path.is_file()
        && parent.join(PROJECT_FILE).is_file()
    {
        return Ok(parent.to_path_buf());
    }
    Err(PersistError::NotAProject(path.to_path_buf()))
}

/// Load and validate a project. Never rewrites anything on disk.
pub fn load_project_dir(path: &Path) -> Result<Project, PersistError> {
    let dir = resolve_project_dir(path)?;
    let file = dir.join(PROJECT_FILE);
    let meta = fs::metadata(&file).map_err(io(&file))?;
    if meta.len() > crate::MAX_PROJECT_JSON_BYTES as u64 {
        return Err(PersistError::TooLarge { bytes: meta.len(), max: crate::MAX_PROJECT_JSON_BYTES });
    }
    let bytes = fs::read(&file).map_err(io(&file))?;
    let mut project = parse_project(&bytes)?;
    // large plugin states saved beside the manifest; unreadable ones stay references and the
    // instance opens as a stand-in (crate::plugin_state)
    let _ = crate::plugin_state::inline(&dir, &mut project);
    Ok(project)
}

/// Parse and validate project JSON bytes.
pub fn parse_project(bytes: &[u8]) -> Result<Project, PersistError> {
    if bytes.len() > crate::MAX_PROJECT_JSON_BYTES {
        return Err(PersistError::TooLarge { bytes: bytes.len() as u64, max: crate::MAX_PROJECT_JSON_BYTES });
    }
    // Check the schema version first so a newer file produces a clear message.
    #[derive(serde::Deserialize)]
    struct Head {
        schema_version: u32,
    }
    let head: Head = serde_json::from_slice(bytes).map_err(|e| PersistError::Json(e.to_string()))?;
    if head.schema_version > crate::SCHEMA_VERSION {
        return Err(ValidationError::UnsupportedSchema { found: head.schema_version, supported: crate::SCHEMA_VERSION }.into());
    }
    let project: Project = if head.schema_version < crate::SCHEMA_VERSION {
        // older manifest: migrate a copy in memory; the file is rewritten only on save
        let (v, _) = migrate_json(bytes)?;
        serde_json::from_value(v).map_err(|e| PersistError::Json(e.to_string()))?
    } else {
        serde_json::from_slice(bytes).map_err(|e| PersistError::Json(e.to_string()))?
    };
    validate(&project)?;
    Ok(project)
}

/// Serialize a project to canonical pretty JSON.
pub fn project_to_json(p: &Project) -> Vec<u8> {
    let mut v = serde_json::to_vec_pretty(p).expect("project model always serializes");
    v.push(b'\n');
    v
}

/// Save atomically: write temp → fsync → keep previous manifest in recovery → rename.
/// On any failure before the final rename the existing project stays intact.
pub fn save_project_dir(dir: &Path, p: &Project) -> Result<(), PersistError> {
    validate(p)?;
    for sub in [ASSETS_DIR, PLUGIN_STATE_DIR, RECOVERY_DIR, CACHE_DIR] {
        let d = dir.join(sub);
        fs::create_dir_all(&d).map_err(io(&d))?;
    }
    let on_disk = crate::plugin_state::spill(dir, p)?;
    let bytes = project_to_json(on_disk.as_ref().unwrap_or(p));
    let target = dir.join(PROJECT_FILE);
    let tmp = dir.join(format!("{PROJECT_FILE}.tmp-{}", std::process::id()));
    {
        let mut f = fs::File::create(&tmp).map_err(io(&tmp))?;
        f.write_all(&bytes).map_err(io(&tmp))?;
        f.sync_all().map_err(io(&tmp))?;
    }
    // verify what we wrote parses
    if let Err(e) = fs::read(&tmp).map_err(io(&tmp)).and_then(|b| parse_project(&b)) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if target.exists() {
        let prev = dir.join(PREVIOUS_MANIFEST);
        if let Err(e) = fs::copy(&target, &prev) {
            let _ = fs::remove_file(&tmp);
            return Err(PersistError::Io { path: prev, source: e });
        }
    }
    // std::fs::rename replaces an existing destination on both Unix and Windows
    // (MoveFileEx with MOVEFILE_REPLACE_EXISTING).
    if let Err(e) = fs::rename(&tmp, &target) {
        let _ = fs::remove_file(&tmp);
        return Err(PersistError::Io { path: target, source: e });
    }
    // association marker (best effort: a read-only marker never fails the save)
    let marker = dir.join(PROJECT_MARKER);
    if !marker.is_file() {
        let _ = fs::write(&marker, MARKER_TEXT);
    }
    crate::plugin_state::prune_unreferenced(dir);
    #[cfg(unix)]
    {
        if let Ok(d) = fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

/// Path of the previous manifest kept by the last save, if any.
pub fn previous_manifest_path(dir: &Path) -> PathBuf {
    dir.join(PREVIOUS_MANIFEST)
}

/// Autosaves live here, newest `AUTOSAVE_KEEP` kept.
pub const AUTOSAVE_DIR: &str = "recovery/autosave";
pub const AUTOSAVE_KEEP: usize = 10;

/// Write an autosave manifest (`<unix ms>-<revision>.json`, atomic) and prune the ring.
/// Never touches `project.json` or the dirty state.
pub fn autosave(dir: &Path, p: &Project) -> Result<PathBuf, PersistError> {
    validate(p)?;
    let d = dir.join(AUTOSAVE_DIR);
    fs::create_dir_all(&d).map_err(io(&d))?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|t| t.as_millis() as u64).unwrap_or(0);
    let target = d.join(format!("{now:013}-{:08}.json", p.revision));
    let tmp = d.join(format!("{now:013}-{:08}.json.tmp-{}", p.revision, std::process::id()));
    let on_disk = crate::plugin_state::spill(dir, p)?;
    {
        let mut f = fs::File::create(&tmp).map_err(io(&tmp))?;
        f.write_all(&project_to_json(on_disk.as_ref().unwrap_or(p))).map_err(io(&tmp))?;
        f.sync_all().map_err(io(&tmp))?;
    }
    fs::rename(&tmp, &target).map_err(|e| PersistError::Io { path: target.clone(), source: e })?;
    // prune: keep the newest AUTOSAVE_KEEP by name (timestamp-ordered)
    let mut names: Vec<PathBuf> = fs::read_dir(&d).map_err(io(&d))?.flatten().map(|e| e.path()).filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json")).collect();
    names.sort();
    while names.len() > AUTOSAVE_KEEP {
        let old = names.remove(0);
        let _ = fs::remove_file(old);
    }
    crate::plugin_state::prune_unreferenced(dir);
    Ok(target)
}

/// A manifest that may be newer than the saved project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryCandidate {
    pub path: PathBuf,
    pub revision: u64,
    pub saved_unix_ms: u64,
    /// `true` for autosaves, `false` for the previous-manifest backup.
    pub autosave: bool,
}

/// Autosaves (newest first) with work the saved manifest lacks: what to offer after a crash.
/// An autosave counts when its revision is above the manifest's or it was written after the
/// manifest (revisions restarted at 0 on open in earlier versions), and its content differs
/// from the manifest (bookkeeping aside). Without a manifest (a never-saved project) every
/// autosave counts. Unparseable files are skipped.
pub fn recovery_candidates(dir: &Path) -> Vec<RecoveryCandidate> {
    let saved = load_project_dir(dir).ok();
    let saved_rev = saved.as_ref().map_or(0, |p| p.revision);
    let saved_ms = fs::metadata(dir.join(PROJECT_FILE)).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64);
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(dir.join(AUTOSAVE_DIR)) {
        for e in rd.flatten() {
            let path = e.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
            if !name.ends_with(".json") {
                continue;
            }
            let Ok(bytes) = fs::read(&path) else { continue };
            let Ok(p) = parse_project(&bytes) else { continue };
            let ts = name.split('-').next().and_then(|t| t.parse::<u64>().ok()).unwrap_or(0);
            let newer = p.revision > saved_rev || saved_ms.is_none_or(|ms| ts > ms);
            let differs = saved.as_ref().is_none_or(|s| !crate::compare::describe_changes(s, &p).is_empty());
            if newer && differs {
                out.push(RecoveryCandidate { path, revision: p.revision, saved_unix_ms: ts, autosave: true });
            }
        }
    }
    out.sort_by(|a, b| b.saved_unix_ms.cmp(&a.saved_unix_ms).then(b.revision.cmp(&a.revision)));
    out
}

/// The project folder a recovery candidate belongs to (`<dir>/recovery/autosave/<file>`).
pub fn candidate_project_dir(c: &RecoveryCandidate) -> Option<PathBuf> {
    Some(c.path.parent()?.parent()?.parent()?.to_path_buf())
}

/// Remove every autosave (after a successful save, or when the user discards them).
pub fn clear_autosaves(dir: &Path) {
    if let Ok(rd) = fs::read_dir(dir.join(AUTOSAVE_DIR)) {
        for e in rd.flatten() {
            let _ = fs::remove_file(e.path());
        }
    }
}

/// Save As: an independent project folder at `dst` (must not exist or be empty) with the
/// manifest and every referenced asset and plugin-state file copied from `src`. The source
/// is never modified. Returns the project as saved (same ids; the name is kept).
pub fn save_as(src: Option<&Path>, dst: &Path, p: &Project) -> Result<(), PersistError> {
    if dst.exists() {
        let non_empty = fs::read_dir(dst).map(|mut d| d.next().is_some()).unwrap_or(true);
        if non_empty {
            return Err(PersistError::Collision(dst.to_path_buf()));
        }
    }
    if let Some(s) = src
        && fs::canonicalize(s).ok() == fs::canonicalize(dst).ok()
        && dst.exists()
    {
        return Err(PersistError::Collision(dst.to_path_buf()));
    }
    fs::create_dir_all(dst).map_err(io(dst))?;
    for sub in [ASSETS_DIR, PLUGIN_STATE_DIR, RECOVERY_DIR, CACHE_DIR] {
        let d = dst.join(sub);
        fs::create_dir_all(&d).map_err(io(&d))?;
    }
    if let Some(src) = src {
        for a in &p.assets {
            if let Some(from) = crate::assets::asset_path(src, a)
                && from.is_file()
            {
                let to = dst.join(&a.path);
                if let Some(parent) = to.parent() {
                    fs::create_dir_all(parent).map_err(io(parent))?;
                }
                fs::copy(&from, &to).map_err(io(&to))?;
            }
        }
        let ps = src.join(PLUGIN_STATE_DIR);
        if let Ok(rd) = fs::read_dir(&ps) {
            for e in rd.flatten() {
                let to = dst.join(PLUGIN_STATE_DIR).join(e.file_name());
                fs::copy(e.path(), &to).map_err(io(&to))?;
            }
        }
    }
    save_project_dir(dst, p)
}

/// Migrate an older manifest to the current schema, one version at a time, on a copy.
/// Returns the migrated JSON and the version it started from. v1 is the first schema, so
/// anything older is treated as v1 with defaults (documented); the hook is where future
/// steps go.
pub fn migrate_json(bytes: &[u8]) -> Result<(serde_json::Value, u32), PersistError> {
    let mut v: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| PersistError::Json(e.to_string()))?;
    let from = v.get("schema_version").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
    if from > crate::SCHEMA_VERSION {
        return Err(ValidationError::UnsupportedSchema { found: from, supported: crate::SCHEMA_VERSION }.into());
    }
    let mut version = from;
    while version < crate::SCHEMA_VERSION {
        // v0 → v1: the pre-release shape is v1 without the explicit version/revision fields
        if let Some(obj) = v.as_object_mut() {
            obj.insert("schema_version".into(), serde_json::json!(version + 1));
            obj.entry("revision").or_insert(serde_json::json!(0));
        }
        version += 1;
    }
    Ok((v, from))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rawdawg-test-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn save_load_round_trip_and_structure() {
        let root = tmpdir("roundtrip");
        let dir = root.join("Song.rawdawg");
        let p = Project::new_default("Song", serde_json::json!({"patch": 1}));
        save_project_dir(&dir, &p).unwrap();
        assert!(dir.join("project.json").is_file());
        assert!(dir.join("assets/sha256").is_dir());
        assert!(dir.join("recovery").is_dir());
        let loaded = load_project_dir(&dir).unwrap();
        assert_eq!(loaded, p);
        let via_file = load_project_dir(&dir.join("project.json")).unwrap();
        assert_eq!(via_file, p);
        // second save keeps previous manifest
        let mut p2 = p.clone();
        p2.name = "Song 2".into();
        save_project_dir(&dir, &p2).unwrap();
        assert!(previous_manifest_path(&dir).is_file());
        let prev = parse_project(&fs::read(previous_manifest_path(&dir)).unwrap()).unwrap();
        assert_eq!(prev.name, "Song");
        assert_eq!(load_project_dir(&dir).unwrap().name, "Song 2");
        assert!(fs::read_dir(&dir).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().contains(".tmp-")));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_newer_schema_and_garbage_without_touching_disk() {
        let root = tmpdir("reject");
        let dir = root.join("X.rawdawg");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("project.json"), b"{\"schema_version\": 2}").unwrap();
        let err = load_project_dir(&dir).unwrap_err();
        assert!(matches!(err, PersistError::Invalid(ValidationError::UnsupportedSchema { found: 2, .. })), "{err}");
        fs::write(dir.join("project.json"), b"nope").unwrap();
        assert!(matches!(load_project_dir(&dir), Err(PersistError::Json(_))));
        assert_eq!(fs::read(dir.join("project.json")).unwrap(), b"nope");
        assert!(matches!(load_project_dir(&root), Err(PersistError::NotAProject(_))));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn invalid_project_is_not_saved() {
        let root = tmpdir("invalid");
        let dir = root.join("X.rawdawg");
        let mut p = Project::new_default("X", serde_json::json!({}));
        save_project_dir(&dir, &p).unwrap();
        p.ppq = 1;
        assert!(save_project_dir(&dir, &p).is_err());
        assert_eq!(load_project_dir(&dir).unwrap().ppq, 960);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn deeply_nested_json_is_rejected_not_crashing() {
        let nested = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
        assert!(matches!(parse_project(nested.as_bytes()), Err(PersistError::Json(_))));
    }
}

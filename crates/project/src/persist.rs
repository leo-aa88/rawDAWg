//! Directory project persistence with atomic manifest replacement.

use crate::model::Project;
use crate::validate::{ValidationError, validate};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const PROJECT_FILE: &str = "project.json";
pub const ASSETS_DIR: &str = "assets/sha256";
pub const PLUGIN_STATE_DIR: &str = "plugin-state";
pub const RECOVERY_DIR: &str = "recovery";
pub const CACHE_DIR: &str = "cache";
const PREVIOUS_MANIFEST: &str = "recovery/project.previous.json";

#[derive(Debug, thiserror::Error)]
pub enum PersistError {
    #[error("I/O error at {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
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
    if path.file_name().is_some_and(|n| n == PROJECT_FILE)
        && let Some(parent) = path.parent()
        && path.is_file()
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
    parse_project(&bytes)
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
    let project: Project = serde_json::from_slice(bytes).map_err(|e| PersistError::Json(e.to_string()))?;
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
    let bytes = project_to_json(p);
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

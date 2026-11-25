//! Content-addressed asset import into a project folder (PROJECT_FORMAT.md §3).
//! The original file is never modified; the copy lives under
//! `assets/sha256/<hash>.<ext>` and is referenced by hash.

use crate::model::{Asset, AssetKind};
use crate::persist::ASSETS_DIR;
use rawdawg_types::AssetId;
use std::path::{Path, PathBuf};

/// Maximum accepted asset size (2 GiB; larger files are rejected explicitly).
pub const MAX_ASSET_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum AssetError {
    #[error("I/O error at {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("file is too large ({0} bytes)")]
    TooLarge(u64),
    #[error("unsupported file type .{0}")]
    Unsupported(String),
    #[error("hash mismatch after copy (disk error?)")]
    HashMismatch,
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> AssetError + '_ {
    move |source| AssetError::Io { path: path.to_path_buf(), source }
}

/// Kind by extension; `None` for files we do not import.
pub fn kind_for_extension(ext: &str) -> Option<AssetKind> {
    match ext.to_ascii_lowercase().as_str() {
        "wav" | "flac" | "mp3" => Some(AssetKind::Audio),
        "sf2" => Some(AssetKind::Sf2),
        "mid" | "midi" => Some(AssetKind::Midi),
        _ => None,
    }
}

/// Hash a file's content (streaming).
pub fn hash_file(path: &Path) -> Result<AssetId, AssetError> {
    use sha2::Digest;
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(io(path))?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf).map_err(io(path))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest: [u8; 32] = hasher.finalize().into();
    Ok(AssetId::from_digest(&digest))
}

/// Copy `source` into the project's managed asset store and return its record.
/// Importing the same content twice returns the same id and does not copy again.
pub fn import_asset(project_dir: &Path, source: &Path) -> Result<Asset, AssetError> {
    let ext = source.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let kind = kind_for_extension(&ext).ok_or_else(|| AssetError::Unsupported(ext.clone()))?;
    let meta = std::fs::metadata(source).map_err(io(source))?;
    if meta.len() > MAX_ASSET_BYTES {
        return Err(AssetError::TooLarge(meta.len()));
    }
    let id = hash_file(source)?;
    let dir = project_dir.join(ASSETS_DIR);
    std::fs::create_dir_all(&dir).map_err(io(&dir))?;
    let file_name = format!("{}.{ext}", id.as_hex());
    let dest = dir.join(&file_name);
    if !dest.exists() {
        let tmp = dir.join(format!("{file_name}.tmp-{}", std::process::id()));
        std::fs::copy(source, &tmp).map_err(io(&tmp))?;
        if hash_file(&tmp)? != id {
            let _ = std::fs::remove_file(&tmp);
            return Err(AssetError::HashMismatch);
        }
        std::fs::rename(&tmp, &dest).map_err(io(&dest))?;
    }
    Ok(Asset {
        id,
        path: format!("{ASSETS_DIR}/{file_name}"),
        kind,
        source_name: source.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        original_rate_hz: None,
        channels: None,
        frames: None,
        bytes: meta.len(),
    })
}

/// Absolute path of an asset inside a project folder (validated relative path).
pub fn asset_path(project_dir: &Path, asset: &Asset) -> Option<PathBuf> {
    if !crate::validate::is_safe_relative_path(&asset.path) {
        return None;
    }
    Some(project_dir.join(&asset.path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_is_content_addressed_and_idempotent() {
        let root = std::env::temp_dir().join(format!("rawdawg-assets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("tone.wav");
        std::fs::write(&src, b"RIFFfakewavdata").unwrap();
        let project = root.join("P.rawdawg");
        let a = import_asset(&project, &src).unwrap();
        assert_eq!(a.kind, AssetKind::Audio);
        assert!(a.path.starts_with("assets/sha256/") && a.path.ends_with(".wav"));
        assert_eq!(a.source_name, "tone.wav");
        assert!(asset_path(&project, &a).unwrap().is_file());
        let b = import_asset(&project, &src).unwrap();
        assert_eq!(a.id, b.id);
        assert!(std::fs::read(&src).unwrap() == b"RIFFfakewavdata", "source untouched");
        assert!(matches!(import_asset(&project, &root.join("missing.wav")), Err(AssetError::Io { .. })));
        std::fs::write(root.join("x.exe"), b"nope").unwrap();
        assert!(matches!(import_asset(&project, &root.join("x.exe")), Err(AssetError::Unsupported(_))));
        let _ = std::fs::remove_dir_all(&root);
    }
}

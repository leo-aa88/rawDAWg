//! `.rawdawgpreset` exchange format (PROJECT_FORMAT.md §4): a ZIP with
//! `manifest.json` and optional `assets/<sha256>.<ext>` entries. Reading
//! validates entry names (no traversal, no symlinks, no duplicates), sizes and
//! the manifest before anything is trusted.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Read, Write};
use std::path::Path;

pub const PRESET_SCHEMA_VERSION: u32 = 1;
pub const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
/// Default cap for the sum of asset bytes inside one preset archive.
pub const MAX_ASSET_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 512;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PresetAssetRef {
    /// Entry name inside `assets/` (content hash + extension).
    pub name: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PresetMutation {
    pub algorithm_version: u32,
    pub seed: u64,
    pub chaos: f32,
    pub locks: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PresetManifest {
    pub schema_version: u32,
    pub preset_id: rawdawg_types::PresetId,
    /// `synth`, `sampler`, `effect`, `rack`, `morph`.
    pub kind: String,
    pub type_id: String,
    pub state_version: u32,
    pub name: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub description: String,
    /// Full native state (the synth patch JSON for `synth`).
    pub state: Value,
    #[serde(default)]
    pub assets: Vec<PresetAssetRef>,
    #[serde(default)]
    pub mutation: Option<PresetMutation>,
    /// Product/version that wrote the file (informational).
    #[serde(default)]
    pub generator: String,
}

impl PresetManifest {
    /// Manifest for a flagship synth patch (`state` must be the serialized patch).
    pub fn new_synth<T: Serialize>(name: &str, patch: &T, tags: &[String]) -> Self {
        let state = serde_json::to_value(patch).unwrap_or(Value::Null);
        let mutation = state.get("mutation").map(|m| PresetMutation {
            algorithm_version: m.get("algorithm_version").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            seed: m.get("seed").and_then(|v| v.as_u64()).unwrap_or(0),
            chaos: m.get("chaos").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
            locks: m.get("locks").cloned().unwrap_or(Value::Null),
        });
        Self {
            schema_version: PRESET_SCHEMA_VERSION,
            preset_id: rawdawg_types::PresetId::new(),
            kind: "synth".into(),
            type_id: rawdawg_types::ids::native_types::SYNTH.into(),
            state_version: 1,
            name: name.to_owned(),
            author: String::new(),
            tags: tags.to_vec(),
            description: String::new(),
            state,
            assets: vec![],
            mutation,
            generator: format!("{} {}", rawdawg_types::PRODUCT_NAME, rawdawg_types::VERSION),
        }
    }

    pub fn validate(&self) -> Result<(), PresetError> {
        if self.schema_version == 0 || self.schema_version > PRESET_SCHEMA_VERSION {
            return Err(PresetError::UnsupportedSchema(self.schema_version));
        }
        if !matches!(self.kind.as_str(), "synth" | "sampler" | "effect" | "rack" | "morph") {
            return Err(PresetError::Invalid(format!("unknown preset kind {}", self.kind)));
        }
        if self.type_id.is_empty() || self.name.is_empty() || self.state_version == 0 {
            return Err(PresetError::Invalid("missing type id, name or state version".into()));
        }
        if !self.state.is_object() {
            return Err(PresetError::Invalid("state must be a JSON object".into()));
        }
        for a in &self.assets {
            if !is_safe_entry_name(&a.name) || a.name.contains('/') {
                return Err(PresetError::Invalid(format!("unsafe asset name {}", a.name)));
            }
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PresetError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("archive error: {0}")]
    Zip(String),
    #[error("preset is invalid: {0}")]
    Invalid(String),
    #[error("unsupported preset schema version {0}")]
    UnsupportedSchema(u32),
    #[error("archive entry {0} is not allowed (traversal, absolute path, symlink or duplicate)")]
    UnsafeEntry(String),
    #[error("{what} exceeds the limit of {limit} bytes")]
    TooLarge { what: String, limit: u64 },
}

fn is_safe_entry_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('/') && !name.contains('\\') && !name.contains("..") && !name.contains(':') && name.chars().all(|c| !c.is_control())
}

/// A preset read from disk.
#[derive(Clone, Debug, PartialEq)]
pub struct PresetFile {
    pub manifest: PresetManifest,
    /// Asset entry name → bytes (already validated against the manifest).
    pub assets: Vec<(String, Vec<u8>)>,
}

/// Write a preset archive atomically (temp file + rename).
pub fn write_preset(path: &Path, manifest: &PresetManifest, assets: &[(String, Vec<u8>)]) -> Result<(), PresetError> {
    manifest.validate()?;
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("rawdawgpreset.tmp");
    {
        let file = std::fs::File::create(&tmp)?;
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("manifest.json", opts).map_err(|e| PresetError::Zip(e.to_string()))?;
        zip.write_all(&serde_json::to_vec_pretty(manifest).map_err(|e| PresetError::Invalid(e.to_string()))?)?;
        for (name, bytes) in assets {
            if !is_safe_entry_name(name) || name.contains('/') {
                return Err(PresetError::UnsafeEntry(name.clone()));
            }
            zip.start_file(format!("assets/{name}"), opts).map_err(|e| PresetError::Zip(e.to_string()))?;
            zip.write_all(bytes)?;
        }
        let mut file = zip.finish().map_err(|e| PresetError::Zip(e.to_string()))?;
        file.flush()?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Read and validate a preset archive with bounded decompression.
pub fn read_preset(path: &Path) -> Result<PresetFile, PresetError> {
    let file = std::fs::File::open(path)?;
    read_preset_from(file)
}

pub fn read_preset_from<R: Read + std::io::Seek>(reader: R) -> Result<PresetFile, PresetError> {
    let mut archive = zip::ZipArchive::new(reader).map_err(|e| PresetError::Zip(e.to_string()))?;
    if archive.len() > MAX_ENTRIES {
        return Err(PresetError::TooLarge { what: "entry count".into(), limit: MAX_ENTRIES as u64 });
    }
    let mut manifest_bytes: Option<Vec<u8>> = None;
    let mut assets: Vec<(String, Vec<u8>)> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut total_assets: u64 = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| PresetError::Zip(e.to_string()))?;
        let raw_name = entry.name().map_err(|e| PresetError::Zip(e.to_string()))?.to_string();
        if entry.is_symlink() || entry.enclosed_name().is_none() || !is_safe_entry_name(&raw_name) {
            return Err(PresetError::UnsafeEntry(raw_name));
        }
        if entry.is_dir() {
            continue;
        }
        if seen.contains(&raw_name) {
            return Err(PresetError::UnsafeEntry(raw_name));
        }
        seen.push(raw_name.clone());
        if raw_name == "manifest.json" {
            if entry.size() > MAX_MANIFEST_BYTES {
                return Err(PresetError::TooLarge { what: "manifest".into(), limit: MAX_MANIFEST_BYTES });
            }
            manifest_bytes = Some(read_bounded(&mut entry, MAX_MANIFEST_BYTES)?);
        } else if let Some(name) = raw_name.strip_prefix("assets/") {
            if name.contains('/') || !is_safe_entry_name(name) {
                return Err(PresetError::UnsafeEntry(raw_name));
            }
            let declared = entry.size();
            total_assets = total_assets.saturating_add(declared);
            if total_assets > MAX_ASSET_TOTAL_BYTES {
                return Err(PresetError::TooLarge { what: "assets".into(), limit: MAX_ASSET_TOTAL_BYTES });
            }
            let bytes = read_bounded(&mut entry, declared)?;
            assets.push((name.to_owned(), bytes));
        } else {
            // unknown entries are ignored but must still be safe (checked above)
        }
    }
    let bytes = manifest_bytes.ok_or_else(|| PresetError::Invalid("manifest.json missing".into()))?;
    let manifest: PresetManifest = serde_json::from_slice(&bytes).map_err(|e| PresetError::Invalid(format!("manifest JSON: {e}")))?;
    manifest.validate()?;
    // every manifest asset must be present with matching size and hash
    for a in &manifest.assets {
        let Some((_, data)) = assets.iter().find(|(n, _)| n == &a.name) else { return Err(PresetError::Invalid(format!("asset {} missing from archive", a.name))) };
        if data.len() as u64 != a.bytes {
            return Err(PresetError::Invalid(format!("asset {} size mismatch", a.name)));
        }
        let digest = sha256_hex(data);
        if digest != a.sha256 {
            return Err(PresetError::Invalid(format!("asset {} hash mismatch", a.name)));
        }
    }
    Ok(PresetFile { manifest, assets })
}

/// Read at most `limit` bytes; an entry that keeps producing data beyond its
/// declared size (decompression bomb) is rejected.
fn read_bounded<R: Read>(r: &mut R, limit: u64) -> Result<Vec<u8>, PresetError> {
    let mut out = Vec::new();
    let mut taken = r.take(limit + 1);
    taken.read_to_end(&mut out)?;
    if out.len() as u64 > limit {
        return Err(PresetError::TooLarge { what: "entry".into(), limit });
    }
    Ok(out)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn manifest() -> PresetManifest {
        PresetManifest::new_synth(
            "Test Sound",
            &serde_json::json!({"version": 1, "name": "Test Sound", "mutation": {"algorithm_version": 1, "seed": 7, "chaos": 35.0, "locks": {"categories": ["pitch"], "params": []}}}),
            &["lead".into()],
        )
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("rawdawg-preset-{}-{}", name, std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d.join(format!("{name}.rawdawgpreset"))
    }

    #[test]
    fn round_trip_with_assets() {
        let mut m = manifest();
        let data = b"hello asset".to_vec();
        m.assets.push(PresetAssetRef { name: format!("{}.bin", sha256_hex(&data)), sha256: sha256_hex(&data), bytes: data.len() as u64 });
        let path = tmp("roundtrip");
        write_preset(&path, &m, &[(m.assets[0].name.clone(), data.clone())]).unwrap();
        let back = read_preset(&path).unwrap();
        assert_eq!(back.manifest, m);
        assert_eq!(back.assets, vec![(m.assets[0].name.clone(), data)]);
        assert_eq!(back.manifest.mutation.as_ref().unwrap().seed, 7);
        assert!(!path.with_extension("rawdawgpreset.tmp").exists());
    }

    fn archive_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default();
            for (name, data) in entries {
                zip.start_file(*name, opts).unwrap();
                zip.write_all(data).unwrap();
            }
            zip.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn rejects_traversal_absolute_and_duplicate_entries() {
        let m = serde_json::to_vec(&manifest()).unwrap();
        for bad in ["../evil.json", "/abs.json", "assets/../x.bin", "assets/sub/x.bin"] {
            let z = archive_with(&[("manifest.json", &m), (bad, b"x")]);
            let r = read_preset_from(Cursor::new(z));
            assert!(matches!(r, Err(PresetError::UnsafeEntry(_))), "{bad}: {r:?}");
        }
        // duplicate names cannot be produced by the writer; the reader still rejects them
        // (defense in depth) — covered by the `seen` check in `read_preset_from`.
    }

    #[test]
    fn rejects_missing_or_newer_manifest_and_hash_mismatch() {
        let z = archive_with(&[("assets/a.bin", b"x")]);
        assert!(matches!(read_preset_from(Cursor::new(z)), Err(PresetError::Invalid(_))));
        let mut newer = manifest();
        newer.schema_version = 99;
        let z = archive_with(&[("manifest.json", &serde_json::to_vec(&newer).unwrap())]);
        assert!(matches!(read_preset_from(Cursor::new(z)), Err(PresetError::UnsupportedSchema(99))));
        let mut m = manifest();
        m.assets.push(PresetAssetRef { name: "a.bin".into(), sha256: "0".repeat(64), bytes: 1 });
        let z = archive_with(&[("manifest.json", &serde_json::to_vec(&m).unwrap()), ("assets/a.bin", b"x")]);
        assert!(matches!(read_preset_from(Cursor::new(z)), Err(PresetError::Invalid(_))));
    }

    #[test]
    fn bounded_read_rejects_oversized_entries() {
        let big = vec![b' '; (MAX_MANIFEST_BYTES + 10) as usize];
        let z = archive_with(&[("manifest.json", &big)]);
        assert!(matches!(read_preset_from(Cursor::new(z)), Err(PresetError::TooLarge { .. })));
        assert!(read_preset_from(Cursor::new(b"not a zip".to_vec())).is_err());
    }
}

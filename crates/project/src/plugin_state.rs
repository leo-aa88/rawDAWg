//! Large opaque plugin state kept beside the manifest (PROJECT_FORMAT.md §1:
//! `plugin-state/<hash>.bin`; §3: hash/path of opaque bytes, 64 MiB per instance).
//!
//! In memory, a third-party instance's `state` is the host's JSON envelope, with each opaque
//! blob as a base64 string in a field whose name ends in `_b64` (`clap_state_b64`,
//! `vst3_component_b64`, `vst3_controller_b64`). On save, a blob larger than
//! [`SPILL_ABOVE_BYTES`] is written once to `plugin-state/<sha256>.bin` and the field
//! becomes a reference:
//!
//! ```json
//! "clap_state_b64": { "plugin_state_file": "plugin-state/<64 hex>.bin", "bytes": 1048576 }
//! ```
//!
//! Loading reads every reference back (size-bounded, name checked against the SHA-256 of
//! the content) so the in-memory model is the same as before the save. A file that is
//! missing or damaged leaves its reference in place; [`unresolved_reference`] reports it,
//! the app keeps the instance as a stand-in, and saving writes the reference unchanged, so
//! nothing is lost if the file comes back.
//!
//! Blobs are content-addressed: an unchanged state is not written again. Files no manifest
//! refers to any more are removed by [`prune_unreferenced`] (only names of the form
//! `<64 hex>.bin`, and only when every manifest in the project could be read).

use crate::model::{Instance, PluginFormat, Project};
use crate::persist::{AUTOSAVE_DIR, PLUGIN_STATE_DIR, PROJECT_FILE, PersistError, previous_manifest_path};
use rawdawg_types::b64;
use serde_json::Value;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;

/// Blobs larger than this (decoded bytes) are stored in `plugin-state/`; smaller ones stay
/// inline so small projects remain one readable file.
pub const SPILL_ABOVE_BYTES: usize = 16 * 1024;
/// Largest opaque state accepted per blob (PROJECT_FORMAT.md: 64 MiB per instance).
pub const MAX_STATE_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// Key of a reference object that replaces an inline blob.
pub const FILE_REF_KEY: &str = "plugin_state_file";

fn external_instances(p: &Project) -> impl Iterator<Item = &Instance> {
    p.channels.iter().map(|c| &c.instrument).chain(p.mixer.channels.iter().flat_map(|m| m.inserts.iter())).filter(|i| i.format != PluginFormat::Native)
}

fn external_instances_mut(p: &mut Project) -> impl Iterator<Item = &mut Instance> {
    p.channels.iter_mut().map(|c| &mut c.instrument).chain(p.mixer.channels.iter_mut().flat_map(|m| m.inserts.iter_mut())).filter(|i| i.format != PluginFormat::Native)
}

/// `plugin-state/<64 lowercase hex>.bin`, nothing else (no traversal, no other names).
fn hash_of_ref(path: &str) -> Option<&str> {
    let name = path.strip_prefix(PLUGIN_STATE_DIR)?.strip_prefix('/')?.strip_suffix(".bin")?;
    (name.len() == 64 && name.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))).then_some(name)
}

fn reference(state_field: &Value) -> Option<(&str, u64)> {
    let o = state_field.as_object()?;
    Some((o.get(FILE_REF_KEY)?.as_str()?, o.get("bytes")?.as_u64()?))
}

/// The first blob reference in an instance's state that is still a file reference (its file
/// was missing or damaged when the project was loaded), if any.
pub fn unresolved_reference(state: &Value) -> Option<&str> {
    state.as_object()?.iter().filter(|(k, _)| k.ends_with("_b64")).find_map(|(_, v)| reference(v).map(|(p, _)| p))
}

/// Write `bytes` to `plugin-state/<sha256>.bin` unless a file of that name and size exists.
fn write_blob(dir: &Path, bytes: &[u8]) -> Result<String, PersistError> {
    let rel = format!("{PLUGIN_STATE_DIR}/{}.bin", crate::preset::sha256_hex(bytes));
    let target = dir.join(&rel);
    if fs::metadata(&target).is_ok_and(|m| m.len() == bytes.len() as u64) {
        return Ok(rel);
    }
    let parent = dir.join(PLUGIN_STATE_DIR);
    fs::create_dir_all(&parent).map_err(|source| PersistError::Io { path: parent.clone(), source })?;
    let tmp = parent.join(format!(".partial-{}", std::process::id()));
    let io = |source| PersistError::Io { path: tmp.clone(), source };
    {
        let mut f = fs::File::create(&tmp).map_err(io)?;
        f.write_all(bytes).map_err(io)?;
        f.sync_all().map_err(io)?;
    }
    fs::rename(&tmp, &target).map_err(|source| PersistError::Io { path: target.clone(), source })?;
    Ok(rel)
}

/// The project as it is written to disk: large inline blobs moved to `plugin-state/` under
/// `dir`. `None` when nothing needed moving (write `p` itself).
pub fn spill(dir: &Path, p: &Project) -> Result<Option<Project>, PersistError> {
    let large = |v: &Value| v.as_str().is_some_and(|s| s.len() / 4 * 3 > SPILL_ABOVE_BYTES);
    let any = external_instances(p).any(|i| i.state.as_object().is_some_and(|o| o.iter().any(|(k, v)| k.ends_with("_b64") && large(v))));
    if !any {
        return Ok(None);
    }
    let mut out = p.clone();
    for inst in external_instances_mut(&mut out) {
        let Some(obj) = inst.state.as_object_mut() else { continue };
        for (key, v) in obj.iter_mut() {
            if !key.ends_with("_b64") || !large(v) {
                continue;
            }
            // a blob that does not decode, or is not in canonical form (whitespace, odd
            // padding), stays inline as it is, so the saved and reloaded model are identical
            let text = v.as_str().unwrap_or_default();
            let Ok(bytes) = b64::decode(text) else { continue };
            if b64::encode(&bytes) != text {
                continue;
            }
            let rel = write_blob(dir, &bytes)?;
            *v = serde_json::json!({ FILE_REF_KEY: rel, "bytes": bytes.len() });
        }
    }
    Ok(Some(out))
}

fn read_blob(dir: &Path, rel: &str, bytes: u64) -> Result<Vec<u8>, String> {
    let hash = hash_of_ref(rel).ok_or_else(|| format!("{rel} is not a plugin-state file name"))?;
    if bytes > MAX_STATE_FILE_BYTES {
        return Err(format!("{rel} is {bytes} bytes, more than the {MAX_STATE_FILE_BYTES}-byte limit"));
    }
    let path = dir.join(rel);
    let f = fs::File::open(&path).map_err(|e| format!("{rel}: {e}"))?;
    let mut data = Vec::new();
    f.take(MAX_STATE_FILE_BYTES + 1).read_to_end(&mut data).map_err(|e| format!("{rel}: {e}"))?;
    if data.len() as u64 != bytes {
        return Err(format!("{rel} has {} bytes, expected {bytes}", data.len()));
    }
    if crate::preset::sha256_hex(&data) != hash {
        return Err(format!("{rel} is damaged (its content does not match its name)"));
    }
    Ok(data)
}

/// Read every blob reference back into the state (after loading a manifest from `dir`).
/// Returns one message per reference that could not be read; those stay references.
pub fn inline(dir: &Path, p: &mut Project) -> Vec<String> {
    let mut problems = Vec::new();
    for inst in external_instances_mut(p) {
        let name = if inst.display_name.is_empty() { inst.type_id.as_str().to_owned() } else { inst.display_name.clone() };
        let Some(obj) = inst.state.as_object_mut() else { continue };
        for (key, v) in obj.iter_mut() {
            if !key.ends_with("_b64") {
                continue;
            }
            let Some((rel, bytes)) = reference(v) else { continue };
            match read_blob(dir, rel, bytes) {
                Ok(data) => *v = Value::String(b64::encode(&data)),
                Err(e) => problems.push(format!("{name}: saved state not readable ({e})")),
            }
        }
    }
    problems
}

/// Every `plugin-state/…` path a manifest refers to (blob references and the older
/// `opaque_state_path` field alike: any string starting with `plugin-state/`).
fn collect_refs(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) if s.starts_with("plugin-state/") => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|x| collect_refs(x, out)),
        Value::Object(o) => o.values().for_each(|x| collect_refs(x, out)),
        _ => {}
    }
}

/// Remove `plugin-state/<64 hex>.bin` files that neither `project.json`, the previous
/// manifest nor any autosave refers to. Does nothing if any of those manifests exists but
/// cannot be read as JSON. Returns the number of files removed.
pub fn prune_unreferenced(dir: &Path) -> usize {
    let mut manifests = vec![dir.join(PROJECT_FILE), previous_manifest_path(dir)];
    if let Ok(rd) = fs::read_dir(dir.join(AUTOSAVE_DIR)) {
        manifests.extend(rd.flatten().map(|e| e.path()).filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json")));
    }
    let mut refs = Vec::new();
    for m in manifests {
        if !m.exists() {
            continue;
        }
        let Some(v) = fs::read(&m).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else { return 0 };
        collect_refs(&v, &mut refs);
    }
    let mut removed = 0;
    let Ok(rd) = fs::read_dir(dir.join(PLUGIN_STATE_DIR)) else { return 0 };
    for e in rd.flatten() {
        let Some(name) = e.file_name().to_str().map(str::to_owned) else { continue };
        let rel = format!("{PLUGIN_STATE_DIR}/{name}");
        if hash_of_ref(&rel).is_some() && !refs.contains(&rel) && fs::remove_file(e.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Instance, PluginFormat};

    fn tmpdir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("rawdawg-pstate-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn project_with_state(blob: &[u8], small: &[u8]) -> Project {
        let mut p = Project::new_default("t", serde_json::json!({}));
        let mut inst = Instance::external(PluginFormat::Vst3, "ABCD", "Big Synth", "Vendor", "1.0", None);
        inst.state =
            serde_json::json!({ "format": "vst3", "plugin_id": "ABCD", "vst3_component_b64": b64::encode(blob), "vst3_controller_b64": b64::encode(small), "note": "kept" });
        p.channels[0].instrument = inst;
        p
    }

    /// The same project (same ids) with other blob contents.
    fn restate(p: &Project, blob: &[u8]) -> Project {
        let mut q = p.clone();
        q.channels[0].instrument.state["vst3_component_b64"] = Value::String(b64::encode(blob));
        q
    }

    fn blob(n: usize, seed: u8) -> Vec<u8> {
        (0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
    }

    #[test]
    fn recovery_compares_large_states_by_content_not_by_reference() {
        let dir = tmpdir("recovery");
        let mut p = project_with_state(&blob(64 * 1024, 3), b"small");
        p.revision = 4;
        crate::persist::save_project_dir(&dir, &p).unwrap();
        // the same work, autosaved later: its large state is a file reference like the manifest's
        let mut same = p.clone();
        same.revision = 9;
        crate::persist::autosave(&dir, &same).unwrap();
        assert!(crate::persist::recovery_candidates(&dir).is_empty(), "nothing the saved project lacks");
        // another sound in the plugin: offered
        let mut other = restate(&p, &blob(64 * 1024, 4));
        other.revision = 10;
        crate::persist::autosave(&dir, &other).unwrap();
        let c = crate::persist::recovery_candidates(&dir);
        assert_eq!(c.len(), 1, "{c:?}");
        let loaded = crate::persist::load_candidate(&c[0]).unwrap();
        assert_eq!(loaded.channels[0].instrument.state, other.channels[0].instrument.state, "inlined from the folder");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn large_blobs_move_to_files_and_come_back_identically() {
        let dir = tmpdir("roundtrip");
        let big = blob(200_000, 1);
        let p = project_with_state(&big, b"tiny");
        let disk = spill(&dir, &p).unwrap().expect("something spilled");
        let state = &disk.channels[0].instrument.state;
        let (rel, bytes) = reference(&state["vst3_component_b64"]).expect("reference");
        assert_eq!(bytes, 200_000);
        assert_eq!(rel, format!("plugin-state/{}.bin", crate::preset::sha256_hex(&big)));
        assert_eq!(fs::read(dir.join(rel)).unwrap(), big, "raw bytes, not base64");
        assert!(state["vst3_controller_b64"].is_string(), "small blobs stay inline");
        assert_eq!(state["note"], "kept");
        assert_eq!(unresolved_reference(state), Some(rel));
        let mut back = disk.clone();
        assert!(inline(&dir, &mut back).is_empty());
        assert_eq!(back, p, "the loaded model equals the model before saving");
        assert_eq!(unresolved_reference(&back.channels[0].instrument.state), None);
        // nothing large: nothing to do
        assert!(spill(&dir, &project_with_state(b"small", b"tiny")).unwrap().is_none());
        // non-canonical base64 (line breaks) stays inline, byte for byte
        let mut odd = p.clone();
        let wrapped: String = b64::encode(&big).as_bytes().chunks(76).map(|c| format!("{}\n", std::str::from_utf8(c).unwrap())).collect();
        odd.channels[0].instrument.state["vst3_component_b64"] = Value::String(wrapped.clone());
        let disk = spill(&dir, &odd).unwrap().unwrap_or_else(|| odd.clone());
        assert_eq!(disk.channels[0].instrument.state["vst3_component_b64"], Value::String(wrapped));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_damaged_or_forged_files_stay_references() {
        let dir = tmpdir("damaged");
        let big = blob(50_000, 2);
        let disk = spill(&dir, &project_with_state(&big, b"x")).unwrap().unwrap();
        let rel = reference(&disk.channels[0].instrument.state["vst3_component_b64"]).unwrap().0.to_owned();
        // damaged: same size, different content
        let mut bad = big.clone();
        bad[100] ^= 0xFF;
        fs::write(dir.join(&rel), &bad).unwrap();
        let mut back = disk.clone();
        let problems = inline(&dir, &mut back);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("Big Synth") && problems[0].contains("damaged"), "{problems:?}");
        assert_eq!(back, disk, "the reference is kept as it was");
        // missing
        fs::remove_file(dir.join(&rel)).unwrap();
        let mut back = disk.clone();
        assert!(inline(&dir, &mut back)[0].contains(&rel));
        assert_eq!(back, disk);
        // forged references never leave plugin-state/ and respect the size limit
        for forged in [
            serde_json::json!({ FILE_REF_KEY: "../project.json", "bytes": 10 }),
            serde_json::json!({ FILE_REF_KEY: "plugin-state/../../etc/passwd", "bytes": 10 }),
            serde_json::json!({ FILE_REF_KEY: format!("plugin-state/{}.bin", "A".repeat(64)), "bytes": 10 }),
            serde_json::json!({ FILE_REF_KEY: rel.clone(), "bytes": MAX_STATE_FILE_BYTES + 1 }),
        ] {
            let mut p = disk.clone();
            p.channels[0].instrument.state["vst3_component_b64"] = forged.clone();
            assert_eq!(inline(&dir, &mut p).len(), 1, "{forged}");
            assert_eq!(p.channels[0].instrument.state["vst3_component_b64"], forged);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unchanged_blobs_are_not_rewritten_and_unreferenced_ones_are_pruned() {
        let dir = tmpdir("prune");
        let first = blob(40_000, 3);
        let p1 = project_with_state(&first, b"x");
        let p = |b: &[u8]| restate(&p1, b);
        crate::persist::save_project_dir(&dir, &p1).unwrap();
        let rel1 = format!("plugin-state/{}.bin", crate::preset::sha256_hex(&first));
        let written = fs::metadata(dir.join(&rel1)).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        crate::persist::save_project_dir(&dir, &p1).unwrap();
        assert_eq!(fs::metadata(dir.join(&rel1)).unwrap().modified().unwrap(), written, "content-addressed: not written again");
        // a user file in plugin-state/ that is not a blob name is never touched
        fs::write(dir.join("plugin-state/readme.txt"), "mine").unwrap();
        // the state changes twice: the first blob is still named by the previous manifest
        let second = blob(40_000, 4);
        crate::persist::save_project_dir(&dir, &p(&second)).unwrap();
        assert!(dir.join(&rel1).is_file(), "kept while the previous manifest refers to it");
        let third = blob(40_000, 5);
        crate::persist::save_project_dir(&dir, &p(&third)).unwrap();
        assert!(!dir.join(&rel1).exists(), "no manifest refers to the first blob any more");
        let rel2 = format!("plugin-state/{}.bin", crate::preset::sha256_hex(&second));
        let rel3 = format!("plugin-state/{}.bin", crate::preset::sha256_hex(&third));
        assert!(dir.join(&rel2).is_file() && dir.join(&rel3).is_file());
        assert!(dir.join("plugin-state/readme.txt").is_file());
        // an autosave keeps its blob alive too
        let fourth = blob(40_000, 6);
        crate::persist::autosave(&dir, &p(&fourth)).unwrap();
        let rel4 = format!("plugin-state/{}.bin", crate::preset::sha256_hex(&fourth));
        assert!(dir.join(&rel4).is_file());
        assert_eq!(prune_unreferenced(&dir), 0);
        // an unreadable manifest stops pruning altogether
        let ghost = dir.join("plugin-state").join(format!("{}.bin", "0".repeat(64)));
        fs::write(&ghost, b"orphan").unwrap();
        fs::write(dir.join(AUTOSAVE_DIR).join("9999999999999-00000000.json"), b"{ not json").unwrap();
        assert_eq!(prune_unreferenced(&dir), 0);
        assert!(ghost.is_file());
        fs::remove_file(dir.join(AUTOSAVE_DIR).join("9999999999999-00000000.json")).unwrap();
        assert_eq!(prune_unreferenced(&dir), 1);
        assert!(!ghost.exists());
        // the saved project loads with every blob inline again
        let loaded = crate::persist::load_project_dir(&dir).unwrap();
        assert_eq!(loaded, p(&third));
        let _ = fs::remove_dir_all(&dir);
    }
}

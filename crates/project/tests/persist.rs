//! T-DATA: autosave ring and recovery candidates, Save As (assets copied, collisions
//! refused, source untouched), advisory lock, asset relink with reference rewriting and
//! undo, migrations hook, moved-folder reload.

use rawdawg_project::lock::{LockInfo, ProjectLock, is_live};
use rawdawg_project::persist::{AUTOSAVE_KEEP, clear_autosaves, migrate_json};
use rawdawg_project::{Asset, AssetKind, Clip, ClipContent, Command, History, Project, Track, autosave, load_project_dir, recovery_candidates, save_as, save_project_dir};
use rawdawg_types::units::{Db, Tick};
use rawdawg_types::{AssetId, ClipId, TrackId};

fn tmp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("rawdawg-persist-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn wav(path: &std::path::Path, seed: u32) {
    let spec = hound::WavSpec { channels: 1, sample_rate: 48_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for i in 0..4800u32 {
        w.write_sample(((i.wrapping_mul(seed) % 2000) as i16) - 1000).unwrap();
    }
    w.finalize().unwrap();
}

#[test]
fn autosave_ring_keeps_the_newest_and_recovery_offers_only_newer_revisions() {
    let root = tmp("autosave");
    let dir = root.join("A.rawdawg");
    let mut p = Project::new_default("auto", serde_json::json!({}));
    p.revision = 5;
    save_project_dir(&dir, &p).unwrap();
    assert!(recovery_candidates(&dir).is_empty());
    for r in 1..=(AUTOSAVE_KEEP as u64 + 3) {
        p.revision = r;
        autosave(&dir, &p).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let files = std::fs::read_dir(dir.join("recovery/autosave")).unwrap().count();
    assert_eq!(files, AUTOSAVE_KEEP, "ring pruned to the newest {AUTOSAVE_KEEP}");
    let cands = recovery_candidates(&dir);
    assert!(!cands.is_empty());
    assert!(cands.iter().all(|c| c.revision > 5), "only revisions above the saved one: {cands:?}");
    assert_eq!(cands[0].revision, AUTOSAVE_KEEP as u64 + 3, "newest first");
    assert!(cands.windows(2).all(|w| w[0].revision >= w[1].revision));
    let recovered = load_project_dir(cands[0].path.parent().unwrap().parent().unwrap().parent().unwrap()).unwrap();
    assert_eq!(recovered.revision, 5, "the saved manifest is untouched by autosaves");
    let from_auto = rawdawg_project::persist::parse_project(&std::fs::read(&cands[0].path).unwrap()).unwrap();
    assert_eq!(from_auto.revision, AUTOSAVE_KEEP as u64 + 3);
    clear_autosaves(&dir);
    assert!(recovery_candidates(&dir).is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn save_as_copies_referenced_assets_refuses_collisions_and_leaves_the_source_alone() {
    let root = tmp("saveas");
    let src = root.join("Src.rawdawg");
    let mut p = Project::new_default("song", serde_json::json!({}));
    save_project_dir(&src, &p).unwrap();
    let w = root.join("one.wav");
    wav(&w, 7);
    let asset = rawdawg_project::assets::import_asset(&src, &w).unwrap();
    p.assets.push(asset.clone());
    // an asset entry whose file is missing is carried as a placeholder, not an error
    p.assets.push(Asset {
        id: AssetId::from_hex(&"ab".repeat(32)).unwrap(),
        path: format!("assets/sha256/{}.wav", "ab".repeat(32)),
        kind: AssetKind::Audio,
        source_name: "gone.wav".into(),
        original_rate_hz: None,
        channels: None,
        frames: None,
        bytes: 0,
    });
    save_project_dir(&src, &p).unwrap();
    let src_manifest = std::fs::read(src.join("project.json")).unwrap();
    let dst = root.join("Copy.rawdawg");
    save_as(Some(&src), &dst, &p).unwrap();
    assert!(dst.join(&asset.path).is_file(), "referenced asset copied");
    let reloaded = load_project_dir(&dst).unwrap();
    assert_eq!(reloaded.project_id, p.project_id);
    assert_eq!(reloaded.assets.len(), 2);
    assert_eq!(std::fs::read(src.join("project.json")).unwrap(), src_manifest, "source manifest untouched");
    // the copy is independent: changing it does not touch the source
    let mut copy = reloaded.clone();
    copy.name = "changed".into();
    save_project_dir(&dst, &copy).unwrap();
    assert_eq!(load_project_dir(&src).unwrap().name, "song");
    // collisions: a non-empty folder is refused and left alone
    let marker = dst.join("project.json");
    let before = std::fs::read(&marker).unwrap();
    assert!(matches!(save_as(Some(&src), &dst, &p), Err(rawdawg_project::PersistError::Collision(_))));
    assert_eq!(std::fs::read(&marker).unwrap(), before);
    // an unsaved project (no source folder) saves without assets to copy
    let fresh = root.join("Fresh.rawdawg");
    save_as(None, &fresh, &Project::new_default("fresh", serde_json::json!({}))).unwrap();
    assert!(fresh.join("project.json").is_file());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn moved_project_folder_loads_identically_with_portable_paths() {
    let root = tmp("move");
    let src = root.join("Here.rawdawg");
    let mut p = Project::new_default("portable", serde_json::json!({}));
    save_project_dir(&src, &p).unwrap();
    let w = root.join("two.wav");
    wav(&w, 11);
    let asset = rawdawg_project::assets::import_asset(&src, &w).unwrap();
    p.assets.push(asset.clone());
    let tid = TrackId::new();
    p.tracks.push(Track { id: tid, name: "A".into(), color: None, mute: false, solo: false, height: None, mixer_id: None });
    p.clips.push(Clip {
        id: ClipId::new(),
        track_id: tid,
        start_tick: Tick(0),
        length_ticks: Tick(960),
        muted: false,
        content: ClipContent::Audio {
            asset_id: asset.id.clone(),
            source_offset_frames: 0,
            source_length_frames: 4800,
            gain_db: Db(0.0),
            fade_in_frames: 0,
            fade_out_frames: 0,
            playback_rate: 1.0,
        },
    });
    save_project_dir(&src, &p).unwrap();
    let json = String::from_utf8(std::fs::read(src.join("project.json")).unwrap()).unwrap();
    assert!(!json.contains('\\') && !json.contains(root.to_string_lossy().as_ref()), "no absolute or backslash paths in the manifest");
    // move the whole folder somewhere else
    let moved = root.join("elsewhere").join("There.rawdawg");
    std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
    std::fs::rename(&src, &moved).unwrap();
    let q = load_project_dir(&moved).unwrap();
    assert_eq!(q, p, "model identical after the move");
    assert!(rawdawg_project::assets::asset_path(&moved, &q.assets[0]).unwrap().is_file(), "asset resolves relative to the new folder");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn lock_is_advisory_detects_stale_holders_and_can_be_taken_over() {
    let root = tmp("lock");
    let dir = root.join("L.rawdawg");
    save_project_dir(&dir, &Project::new_default("l", serde_json::json!({}))).unwrap();
    let lock = ProjectLock::acquire(&dir, false).unwrap();
    assert!(dir.join("recovery/lock.json").is_file());
    // our own pid is never "live" for a second acquire in the same process: reacquire works
    let again = ProjectLock::acquire(&dir, false);
    assert!(again.is_ok(), "same process re-acquires");
    drop(again);
    drop(lock);
    // a foreign live-looking lock (another host, fresh) is reported as held
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    let foreign = LockInfo { pid: 1, host: "other-machine".into(), acquired_unix_ms: now };
    std::fs::write(dir.join("recovery/lock.json"), serde_json::to_vec(&foreign).unwrap()).unwrap();
    assert!(is_live(&foreign));
    assert!(matches!(ProjectLock::acquire(&dir, false), Err(rawdawg_project::lock::LockError::Held(_))));
    let taken = ProjectLock::acquire(&dir, true).unwrap();
    assert_eq!(ProjectLock::read(&dir).unwrap(), taken.info);
    drop(taken);
    assert!(!dir.join("recovery/lock.json").exists(), "released on drop");
    // a stale lock (very old) is reclaimed without force
    let stale = LockInfo { pid: 1, host: "other-machine".into(), acquired_unix_ms: now.saturating_sub(7 * 3600 * 1000) };
    std::fs::write(dir.join("recovery/lock.json"), serde_json::to_vec(&stale).unwrap()).unwrap();
    assert!(!is_live(&stale));
    assert!(ProjectLock::acquire(&dir, false).is_ok());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn relink_with_different_content_rewrites_every_reference_and_undoes() {
    let root = tmp("relink");
    let dir = root.join("R.rawdawg");
    let mut p = Project::new_default("r", serde_json::json!({}));
    save_project_dir(&dir, &p).unwrap();
    let (a, b) = (root.join("a.wav"), root.join("b.wav"));
    wav(&a, 3);
    wav(&b, 5);
    let old = rawdawg_project::assets::import_asset(&dir, &a).unwrap();
    let new = rawdawg_project::assets::import_asset(&dir, &b).unwrap();
    assert_ne!(old.id, new.id);
    p.assets.push(old.clone());
    let tid = TrackId::new();
    p.tracks.push(Track { id: tid, name: "A".into(), color: None, mute: false, solo: false, height: None, mixer_id: None });
    p.clips.push(Clip {
        id: ClipId::new(),
        track_id: tid,
        start_tick: Tick(0),
        length_ticks: Tick(960),
        muted: false,
        content: ClipContent::Audio {
            asset_id: old.id.clone(),
            source_offset_frames: 0,
            source_length_frames: 4800,
            gain_db: Db(0.0),
            fade_in_frames: 0,
            fade_out_frames: 0,
            playback_rate: 1.0,
        },
    });
    // sampler state and a convolution insert reference the same asset
    p.channels[0].instrument = rawdawg_project::Instance::native(rawdawg_types::ids::native_types::SAMPLER, serde_json::json!({ "asset_id": old.id.to_string(), "name": "s" }));
    let master = p.mixer.master_id;
    let conv = rawdawg_project::Instance::native("native.fx.convolution", serde_json::json!({ "version": 1, "params": {}, "ir_asset": old.id.to_string() }));
    p.mixer.channels.iter_mut().find(|m| m.id == master).unwrap().inserts.push(conv);
    let mut history = History::new();
    history.apply(&mut p, Command::ReplaceAsset { old: old.id.clone(), new: new.clone() }).unwrap();
    let refs = |p: &Project| {
        let clip = matches!(&p.clips[0].content, ClipContent::Audio { asset_id, .. } if *asset_id == new.id);
        let sampler = p.channels[0].instrument.state["asset_id"] == serde_json::json!(new.id.to_string());
        let ir = p.mixer.channels.iter().find(|m| m.id == master).unwrap().inserts[0].state["ir_asset"] == serde_json::json!(new.id.to_string());
        (clip, sampler, ir)
    };
    assert_eq!(refs(&p), (true, true, true), "clip, instrument and insert references rewritten");
    assert_eq!(p.assets[0].id, new.id);
    rawdawg_project::validate(&p).unwrap();
    history.undo(&mut p).unwrap();
    assert_eq!(refs(&p), (false, false, false));
    assert_eq!(p.assets[0].id, old.id);
    assert!(history.apply(&mut p, Command::ReplaceAsset { old: new.id.clone(), new: old.clone() }).is_err(), "unknown old id is refused");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn older_manifests_are_migrated_in_memory_and_newer_ones_refused() {
    let p = Project::new_default("m", serde_json::json!({}));
    let mut v: serde_json::Value = serde_json::from_slice(&rawdawg_project::persist::project_to_json(&p)).unwrap();
    v["schema_version"] = serde_json::json!(0);
    v.as_object_mut().unwrap().remove("revision");
    let bytes = serde_json::to_vec(&v).unwrap();
    let (migrated, from) = migrate_json(&bytes).unwrap();
    assert_eq!(from, 0);
    assert_eq!(migrated["schema_version"], serde_json::json!(rawdawg_project::SCHEMA_VERSION));
    assert_eq!(migrated["revision"], serde_json::json!(0));
    let parsed = rawdawg_project::persist::parse_project(&bytes).unwrap();
    assert_eq!(parsed.name, "m");
    v["schema_version"] = serde_json::json!(99);
    assert!(rawdawg_project::persist::parse_project(&serde_json::to_vec(&v).unwrap()).is_err());
}

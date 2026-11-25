//! T-PLUGIN (host side, in-process parts): catalog persistence and operations,
//! bounded discovery with symlink loops, and description of the built rawDAWg CLAP module.

use rawdawg_plugin_host::catalog::{Fingerprint, PluginDescription, QuarantineReason, QuarantineRecord, ScannedLibrary, now_unix};
use rawdawg_plugin_host::scan::{MAX_DEPTH, enumerate};
use rawdawg_plugin_host::{Catalog, Format};
use std::path::{Path, PathBuf};

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rawdawg-pluginhost-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Build (cached) and locate the rawDAWg CLAP module in this profile's target directory.
pub fn built_clap_module() -> PathBuf {
    let status = std::process::Command::new(env!("CARGO")).args(["build", "-q", "-p", "rawdawg-plugin-clap"]).status().expect("cargo build");
    assert!(status.success(), "building the CLAP module");
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap();
    let name = if cfg!(target_os = "windows") { "rawdawg_clap.dll" } else { "librawdawg_clap.so" };
    let p = dir.join(name);
    assert!(p.is_file(), "{}", p.display());
    p
}

fn desc(id: &str) -> PluginDescription {
    PluginDescription {
        format: Format::Clap,
        plugin_id: id.into(),
        name: id.into(),
        vendor: "v".into(),
        version: "1".into(),
        description: String::new(),
        features: vec!["audio-effect".into()],
        is_instrument: false,
        is_effect: true,
        audio_inputs: 1,
        audio_outputs: 1,
        main_input_channels: 2,
        main_output_channels: 2,
        note_inputs: 0,
        param_count: 3,
        has_gui: false,
        notes: vec![],
    }
}

#[test]
fn catalog_round_trips_and_resolves_by_id_and_fingerprint() {
    let dir = temp_dir("catalog");
    let module = dir.join("a.clap");
    std::fs::write(&module, b"not really a plugin").unwrap();
    let fp = Fingerprint::of(&module).unwrap();
    let mut cat = Catalog::default();
    assert!(cat.needs_scan(&module), "unknown module needs a scan");
    cat.insert_library(ScannedLibrary {
        path: module.clone(),
        fingerprint: fp,
        plugins: vec![desc("x.one"), desc("x.two")],
        scanned_at_unix: now_unix(),
        api_version: "1.2.3".into(),
    });
    assert!(!cat.needs_scan(&module), "unchanged fingerprint → no rescan");
    assert_eq!(cat.plugins().count(), 2);
    let (lib, p) = cat.find(Format::Clap, "x.two", None).expect("found by id");
    assert_eq!(lib.path, module);
    assert_eq!(p.name, "x.two");
    assert!(cat.find(Format::Vst3, "x.two", None).is_none(), "format is part of the key");
    // the same id in two modules: the path hint picks, otherwise the first wins
    let other = dir.join("b.clap");
    std::fs::write(&other, b"another").unwrap();
    cat.insert_library(ScannedLibrary {
        path: other.clone(),
        fingerprint: Fingerprint::of(&other).unwrap(),
        plugins: vec![desc("x.two")],
        scanned_at_unix: now_unix(),
        api_version: String::new(),
    });
    assert_eq!(cat.find(Format::Clap, "x.two", Some(&other)).unwrap().0.path, other);
    assert_eq!(cat.find(Format::Clap, "x.two", Some(Path::new("/nowhere.clap"))).unwrap().0.path, module, "unknown hint falls back to the first match");
    // persistence
    let file = dir.join("plugins.json");
    cat.save(&file).unwrap();
    let loaded = Catalog::load(&file);
    assert_eq!(loaded, cat);
    // a changed file needs rescanning
    std::fs::write(&module, b"not really a plugin, but longer").unwrap();
    assert!(cat.needs_scan(&module));
    // quarantine replaces the library entry; retry clears; rescan only after a change
    cat.insert_quarantine(QuarantineRecord { path: module.clone(), fingerprint: Fingerprint::of(&module), reason: QuarantineReason::Timeout { secs: 15 }, at_unix: now_unix() });
    assert!(cat.is_quarantined(&module));
    assert_eq!(cat.plugins().count(), 1, "quarantined module contributes no plugins");
    assert!(!cat.needs_scan(&module), "quarantined and unchanged → left alone");
    assert!(cat.clear_quarantine(&module));
    assert!(cat.needs_scan(&module), "retry → scanned again");
    cat.insert_quarantine(QuarantineRecord {
        path: module.clone(),
        fingerprint: Fingerprint::of(&module),
        reason: QuarantineReason::Crash { exit_code: Some(134), stderr: String::new() },
        at_unix: now_unix(),
    });
    assert_eq!(cat.quarantined(&module).unwrap().reason.summary(), "scanner crashed (exit code 134)");
    // vanished files are pruned
    std::fs::remove_file(&module).unwrap();
    assert_eq!(cat.prune_missing(), 1);
    assert!(!cat.is_quarantined(&module));
    assert!(cat.remove(&other));
    assert_eq!(cat.plugins().count(), 0);
    // unreadable / unsupported catalog files start empty but keep nothing dangerous
    std::fs::write(&file, b"{ not json").unwrap();
    assert_eq!(Catalog::load(&file), Catalog::default());
    std::fs::write(&file, serde_json::json!({"version": 99, "user_paths": ["/x"], "timeout_secs": 7}).to_string()).unwrap();
    let c = Catalog::load(&file);
    assert_eq!(c.user_paths, vec![PathBuf::from("/x")]);
    assert_eq!(c.timeout_secs, 7);
    assert!(c.libraries.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn discovery_is_recursive_bounded_and_loop_safe() {
    let dir = temp_dir("enumerate");
    std::fs::create_dir_all(dir.join("vendor/sub")).unwrap();
    std::fs::write(dir.join("Top.CLAP"), b"x").unwrap();
    std::fs::write(dir.join("vendor/one.clap"), b"x").unwrap();
    std::fs::write(dir.join("vendor/sub/two.clap"), b"x").unwrap();
    std::fs::write(dir.join("vendor/readme.txt"), b"x").unwrap();
    std::fs::write(dir.join("vendor/other.vst3"), b"x").unwrap();
    // a directory named like a CLAP module is not a module (CLAP modules are files)
    std::fs::create_dir_all(dir.join("vendor/folder.clap")).unwrap();
    std::fs::write(dir.join("vendor/folder.clap/inner.clap"), b"x").unwrap();
    // deeper than MAX_DEPTH: skipped
    let mut deep = dir.join("deep");
    for i in 0..=MAX_DEPTH + 1 {
        deep = deep.join(format!("d{i}"));
    }
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(deep.join("toodeep.clap"), b"x").unwrap();
    #[cfg(unix)]
    {
        // symlink loop: vendor/loop → vendor
        std::os::unix::fs::symlink(dir.join("vendor"), dir.join("vendor/loop")).unwrap();
        // and a symlinked second root to the same tree: visited once
    }
    let found = enumerate(&[dir.clone(), dir.join("vendor"), PathBuf::from("/definitely/not/here")], Format::Clap);
    let names: Vec<String> = found.iter().map(|p| p.strip_prefix(&dir).unwrap().to_string_lossy().replace('\\', "/")).collect();
    assert_eq!(names, vec!["Top.CLAP", "vendor/folder.clap/inner.clap", "vendor/one.clap", "vendor/sub/two.clap"], "{names:?}");
    let vst = enumerate(std::slice::from_ref(&dir), Format::Vst3);
    assert_eq!(vst.len(), 1);
    assert!(vst[0].ends_with("other.vst3"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn describes_the_built_rawdawg_module_in_process() {
    let module = built_clap_module();
    // SAFETY: our own freshly built module.
    let lib = unsafe { rawdawg_plugin_host::describe::describe_clap_library(&module) }.expect("describe");
    assert_eq!(lib.path, module);
    assert_eq!(lib.plugins.len(), 17);
    assert!(lib.api_version.starts_with("1."), "{}", lib.api_version);
    let synth = lib.plugins.iter().find(|p| p.plugin_id == "rawdawg.synth").expect("synth");
    assert!(synth.is_instrument && !synth.is_effect);
    assert_eq!((synth.audio_inputs, synth.audio_outputs, synth.note_inputs), (0, 1, 1));
    assert_eq!(synth.main_output_channels, 2);
    assert!(synth.param_count > 20);
    assert_eq!(synth.vendor, "rawDAWg");
    assert!(!synth.has_gui, "no GUI extension yet");
    let comp = lib.plugins.iter().find(|p| p.plugin_id == "rawdawg.fx.compressor").expect("compressor");
    assert!(comp.is_effect && !comp.is_instrument);
    assert_eq!((comp.audio_inputs, comp.audio_outputs), (2, 1), "main + sidechain");
    assert_eq!(comp.name, "rawDAWg Compressor");
    assert!(comp.features.iter().any(|f| f == "compressor"));
    assert!(lib.plugins.iter().all(|p| p.notes.is_empty()), "every plugin instantiated during description");
    // a non-module file is a load failure, not a panic
    let dir = temp_dir("describe");
    let bogus = dir.join("bogus.clap");
    std::fs::write(&bogus, b"MZ not a shared object").unwrap();
    // SAFETY: the file is not executable code; the loader rejects it.
    let err = unsafe { rawdawg_plugin_host::describe::describe_clap_library(&bogus) }.expect_err("rejected");
    assert!(err.contains("not a loadable CLAP module"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

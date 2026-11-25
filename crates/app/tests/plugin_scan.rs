//! T-PLUGIN / PLUG-03: the isolated scanner. The real `rawdawg` binary describes
//! modules in a child process; hangs, crashes and unloadable files are quarantined
//! with a bounded timeout, and the `--scan-plugins` CLI refreshes a catalog file.

use rawdawg_plugin_host::catalog::QuarantineReason;
use rawdawg_plugin_host::scan::{ScanOptions, ScanOutcome, TEST_BEHAVIOUR_ENV, refresh, scan_library};
use rawdawg_plugin_host::{Catalog, Format};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const EXE: &str = env!("CARGO_BIN_EXE_rawdawg");
/// Tests that touch process environment variables run one at a time.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rawdawg-scan-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn built_clap_module() -> PathBuf {
    let status = std::process::Command::new(env!("CARGO")).args(["build", "-q", "-p", "rawdawg-plugin-clap"]).status().expect("cargo build");
    assert!(status.success());
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap();
    let name = if cfg!(target_os = "windows") { "rawdawg_clap.dll" } else { "librawdawg_clap.so" };
    let p = dir.join(name);
    assert!(p.is_file(), "{}", p.display());
    p
}

fn opts(timeout_secs: u64) -> ScanOptions {
    ScanOptions::new(PathBuf::from(EXE), timeout_secs)
}

#[test]
fn scanner_child_describes_the_rawdawg_module() {
    let _g = ENV_LOCK.lock().unwrap();
    let module = built_clap_module();
    match scan_library(&module, Format::Clap, &opts(30)) {
        ScanOutcome::Scanned(lib) => {
            assert_eq!(lib.path, module);
            assert_eq!(lib.plugins.len(), 17);
            assert!(lib.plugins.iter().any(|p| p.plugin_id == "rawdawg.fx.reverb" && p.name == "rawDAWg Reverb" && p.is_effect));
            assert!(lib.plugins.iter().any(|p| p.plugin_id == "rawdawg.synth" && p.is_instrument && p.note_inputs == 1));
        }
        ScanOutcome::Quarantined(q) => panic!("quarantined: {}", q.reason.summary()),
    }
}

#[test]
fn unloadable_files_and_missing_paths_are_quarantined_as_load_failures() {
    let _g = ENV_LOCK.lock().unwrap();
    let dir = temp_dir("loadfail");
    let bogus = dir.join("bogus.clap");
    std::fs::write(&bogus, b"this is not a shared object").unwrap();
    match scan_library(&bogus, Format::Clap, &opts(30)) {
        ScanOutcome::Quarantined(q) => {
            assert!(matches!(q.reason, QuarantineReason::LoadFailed { .. }), "{:?}", q.reason);
            assert!(q.fingerprint.is_some());
        }
        ScanOutcome::Scanned(_) => panic!("garbage described as a plugin"),
    }
    match scan_library(&dir.join("absent.clap"), Format::Clap, &opts(30)) {
        ScanOutcome::Quarantined(q) => assert!(matches!(q.reason, QuarantineReason::LoadFailed { ref message } if message.contains("not found"))),
        ScanOutcome::Scanned(_) => panic!(),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hanging_scanner_is_killed_at_the_timeout() {
    let _g = ENV_LOCK.lock().unwrap();
    let module = built_clap_module();
    // the hook is read by the child only; set it for this process's children
    // SAFETY: environment changes are serialized by ENV_LOCK within this test binary.
    unsafe { std::env::set_var(TEST_BEHAVIOUR_ENV, "hang") };
    let started = Instant::now();
    let outcome = scan_library(&module, Format::Clap, &opts(1));
    // SAFETY: see above.
    unsafe { std::env::remove_var(TEST_BEHAVIOUR_ENV) };
    let elapsed = started.elapsed();
    match outcome {
        ScanOutcome::Quarantined(q) => assert!(matches!(q.reason, QuarantineReason::Timeout { secs: 1 }), "{:?}", q.reason),
        ScanOutcome::Scanned(_) => panic!("a hanging child must not produce a library"),
    }
    assert!(elapsed >= Duration::from_secs(1) && elapsed < Duration::from_secs(8), "killed promptly: {elapsed:?}");
}

#[test]
fn crashing_scanner_is_quarantined_as_crash() {
    let _g = ENV_LOCK.lock().unwrap();
    let module = built_clap_module();
    // SAFETY: serialized by ENV_LOCK.
    unsafe { std::env::set_var(TEST_BEHAVIOUR_ENV, "crash") };
    let outcome = scan_library(&module, Format::Clap, &opts(30));
    // SAFETY: see above.
    unsafe { std::env::remove_var(TEST_BEHAVIOUR_ENV) };
    match outcome {
        ScanOutcome::Quarantined(q) => assert!(matches!(q.reason, QuarantineReason::Crash { .. }), "{:?}", q.reason),
        ScanOutcome::Scanned(_) => panic!("a crashing child must not produce a library"),
    }
}

#[test]
fn refresh_scans_user_roots_once_and_honours_retry() {
    let _g = ENV_LOCK.lock().unwrap();
    let dir = temp_dir("refresh");
    let module = built_clap_module();
    let copy = dir.join("rawDAWg.clap");
    std::fs::copy(&module, &copy).unwrap();
    std::fs::write(dir.join("broken.clap"), b"nope").unwrap();
    let mut catalog = Catalog { user_paths: vec![dir.clone()], ..Catalog::default() };
    // the standard roots may hold real plugins on a developer machine; restrict to our dir
    let old_home = std::env::var_os("HOME");
    let old_clap = std::env::var_os("CLAP_PATH");
    // SAFETY: serialized by ENV_LOCK; restored below.
    unsafe {
        std::env::set_var("HOME", dir.join("home"));
        std::env::set_var("CLAP_PATH", "");
    }
    let mut seen = Vec::new();
    let report = refresh(&mut catalog, Format::Clap, &opts(30), |p, _, _| seen.push(p.to_path_buf()));
    assert_eq!(report.scanned, 1, "{report:?}");
    assert_eq!(report.quarantined, 1, "{report:?}");
    assert!(seen.iter().any(|p| p == &copy) && seen.iter().any(|p| p.ends_with("broken.clap")));
    assert_eq!(catalog.plugins().count(), 17);
    assert!(catalog.is_quarantined(&dir.join("broken.clap")));
    assert!(catalog.last_scan_unix > 0);
    // second refresh: nothing changed → nothing rescanned, quarantine kept
    let report2 = refresh(&mut catalog, Format::Clap, &opts(30), |_, _, _| {});
    assert_eq!((report2.scanned, report2.quarantined, report2.unchanged), (0, 0, 2), "{report2:?}");
    // user retry: cleared record is scanned again (and fails again)
    assert!(catalog.clear_quarantine(&dir.join("broken.clap")));
    let report3 = refresh(&mut catalog, Format::Clap, &opts(30), |_, _, _| {});
    assert_eq!(report3.quarantined, 1);
    // removing the file prunes it
    std::fs::remove_file(dir.join("broken.clap")).unwrap();
    let report4 = refresh(&mut catalog, Format::Clap, &opts(30), |_, _, _| {});
    assert_eq!(report4.pruned, 1);
    assert!(catalog.quarantine.is_empty());
    // SAFETY: serialized by ENV_LOCK.
    unsafe {
        match old_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        match old_clap {
            Some(c) => std::env::set_var("CLAP_PATH", c),
            None => std::env::remove_var("CLAP_PATH"),
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scan_plugins_cli_writes_the_given_catalog_and_reports_json() {
    let _g = ENV_LOCK.lock().unwrap();
    let dir = temp_dir("cli");
    let module = built_clap_module();
    let root = dir.join("clap");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::copy(&module, root.join("rawDAWg.clap")).unwrap();
    let catalog_file = dir.join("plugins.json");
    let out = std::process::Command::new(EXE)
        .args(["--scan-plugins", "--catalog"])
        .arg(&catalog_file)
        .env("CLAP_PATH", &root)
        .env("HOME", dir.join("home"))
        .output()
        .expect("run rawdawg --scan-plugins");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON report");
    assert_eq!(report["scanned"], 1, "{report}");
    let plugins = report["plugins"].as_array().unwrap();
    assert_eq!(plugins.len(), 17);
    assert!(plugins.iter().any(|p| p["id"] == "rawdawg.fx.delay" && p["effect"] == true && p["name"] == "rawDAWg Delay"));
    assert!(catalog_file.is_file(), "catalog written where asked, not in the user's config");
    let catalog = Catalog::load(&catalog_file);
    assert_eq!(catalog.plugins().count(), 17);
    let _ = std::fs::remove_dir_all(&dir);
}

fn built_vst3_bundle(dir: &std::path::Path) -> PathBuf {
    let status = std::process::Command::new(env!("CARGO")).args(["build", "-q", "-p", "rawdawg-plugin-vst3"]).status().expect("cargo build");
    assert!(status.success());
    let exe = std::env::current_exe().unwrap();
    let target = exe.parent().unwrap().parent().unwrap();
    let name = if cfg!(target_os = "windows") { "rawdawg_vst3.dll" } else { "librawdawg_vst3.so" };
    let arch = if cfg!(target_os = "windows") { "x86_64-win" } else { "x86_64-linux" };
    let inner = dir.join("rawDAWg.vst3").join("Contents").join(arch);
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::copy(target.join(name), inner.join(if cfg!(target_os = "windows") { "rawDAWg.vst3" } else { "rawDAWg.so" })).unwrap();
    dir.join("rawDAWg.vst3")
}

#[test]
fn scanner_child_describes_a_vst3_bundle_and_the_cli_scans_both_formats() {
    let _g = ENV_LOCK.lock().unwrap();
    let dir = temp_dir("vst3");
    let bundle = built_vst3_bundle(&dir);
    match scan_library(&bundle, Format::Vst3, &opts(30)) {
        ScanOutcome::Scanned(lib) => {
            assert_eq!(lib.plugins.len(), 17);
            assert!(lib.plugins.iter().all(|p| p.format == Format::Vst3));
            assert!(lib.plugins.iter().any(|p| p.name == "rawDAWg Synth" && p.is_instrument && p.note_inputs == 1));
            assert!(lib.plugins.iter().any(|p| p.name == "rawDAWg Gate" && p.is_effect && p.audio_inputs == 2));
        }
        ScanOutcome::Quarantined(q) => panic!("quarantined: {}", q.reason.summary()),
    }
    // the CLI scans CLAP and VST3 roots: ~/.vst3 under a temporary HOME holds the bundle
    let home = dir.join("home");
    let vst3_root = home.join(".vst3");
    std::fs::create_dir_all(&vst3_root).unwrap();
    let installed = built_vst3_bundle(&vst3_root);
    assert!(installed.is_dir());
    let clap_root = dir.join("clap");
    std::fs::create_dir_all(&clap_root).unwrap();
    std::fs::copy(built_clap_module(), clap_root.join("rawDAWg.clap")).unwrap();
    let catalog_file = dir.join("plugins.json");
    let out = std::process::Command::new(EXE).args(["--scan-plugins", "--catalog"]).arg(&catalog_file).env("CLAP_PATH", &clap_root).env("HOME", &home).output().expect("run");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON report");
    assert_eq!(report["scanned"], 2, "{report}");
    let plugins = report["plugins"].as_array().unwrap();
    assert_eq!(plugins.len(), 34);
    assert_eq!(plugins.iter().filter(|p| p["format"] == "vst3").count(), 17);
    let _ = std::fs::remove_dir_all(&dir);
}

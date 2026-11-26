//! Hosted editors of rawDAWg's own exported CLAP and VST3 plugins open, stay alive while the
//! host services them, close and reopen, on every platform with a display (X11 on Linux, the
//! desktop on Windows, where the editors tick from Win32 timers in the message loop). Input is
//! covered by the Linux scenario (`clap_editor.rs`, `vst3_editor.rs`).
//!
//! Needs a display, so it is `#[ignore]`d in plain `cargo test` and run by `cargo xtask
//! verify` (check GUI-02).

use rawdawg_plugin_host::HostedInstance;
use rawdawg_plugin_host::clap::{ClapInstance, ModuleCache};
use rawdawg_plugin_host::vst3::{Vst3Instance, Vst3ModuleCache, describe_vst3_module};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn target_dir() -> PathBuf {
    std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().to_path_buf()
}

fn build(package: &str) {
    let status = std::process::Command::new(env!("CARGO")).args(["build", "-q", "-p", package]).status().expect("cargo build");
    assert!(status.success());
}

fn service(inst: &mut dyn HostedInstance, ms: u64) {
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_millis(ms) {
        #[cfg(target_os = "windows")]
        rawdawg_ui::window::pump_messages();
        assert!(inst.editor_idle().open, "the editor closed by itself");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn lifecycle(inst: &mut dyn HostedInstance, title: &str) {
    assert!(inst.has_editor(), "{title}: an embeddable editor is offered");
    for round in 0..2 {
        inst.open_editor(title).unwrap_or_else(|e| panic!("{title} round {round}: {e}"));
        assert!(inst.editor_is_open());
        assert!(inst.editor_window_id().is_some_and(|id| id != 0));
        service(inst, 400);
        inst.close_editor();
        assert!(!inst.editor_is_open());
        assert!(inst.editor_window_id().is_none());
    }
    // the plugin still works after its editor came and went
    assert!(!inst.save_state().is_empty());
}

#[test]
#[ignore = "needs a display server; run by `cargo xtask verify` (GUI-02)"]
fn hosted_clap_and_vst3_editors_open_stay_alive_close_and_reopen() {
    build("rawdawg-plugin-clap");
    build("rawdawg-plugin-vst3");
    let (clap_name, vst3_name, arch, inner) = if cfg!(target_os = "windows") {
        ("rawdawg_clap.dll", "rawdawg_vst3.dll", "x86_64-win", "rawDAWg.vst3")
    } else {
        ("librawdawg_clap.so", "librawdawg_vst3.so", "x86_64-linux", "rawDAWg.so")
    };

    // SAFETY: our own freshly built module.
    let clap_lib = unsafe { rawdawg_plugin_host::describe::describe_clap_library(&target_dir().join(clap_name)) }.expect("describe CLAP");
    let mut clap_cache = ModuleCache::default();
    // SAFETY: described above.
    let clap_module = unsafe { clap_cache.load(&clap_lib.path) }.expect("load CLAP");
    let desc = clap_lib.plugins.iter().find(|p| p.plugin_id == "rawdawg.fx.delay").unwrap().clone();
    let mut clap = ClapInstance::new(clap_module, &desc).expect("CLAP instance");
    lifecycle(&mut clap, "rawDAWg Delay (CLAP)");

    let base = std::env::temp_dir().join(format!("rawdawg-editor-lifecycle-{}", std::process::id()));
    let dir = base.join("rawDAWg.vst3").join("Contents").join(arch);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(target_dir().join(vst3_name), dir.join(inner)).unwrap();
    // SAFETY: our own freshly built module.
    let vst3_lib = unsafe { describe_vst3_module(&base.join("rawDAWg.vst3")) }.expect("describe VST3");
    let mut vst3_cache = Vst3ModuleCache::default();
    // SAFETY: described above.
    let vst3_module = unsafe { vst3_cache.load(&vst3_lib.path) }.expect("load VST3");
    let desc = vst3_lib.plugins.iter().find(|p| p.name == "rawDAWg Delay").unwrap().clone();
    let mut vst3 = Vst3Instance::new(vst3_module, &desc).expect("VST3 instance");
    lifecycle(&mut vst3, "rawDAWg Delay (VST3)");
}

//! rawDAWg hosting the editor of its own exported VST3 plugin on a real display (X11): the
//! host's `IPlugFrame` offers `Linux::IRunLoop`, the plugin's view registers its timer there and
//! paints into a child of the host's window, real X input events drag a slider, the edit comes
//! back through `IComponentHandler::performEdit` and the host sees it. Close and reopen work.
//!
//! Needs an X server, so it is `#[ignore]`d in plain `cargo test` and run by
//! `cargo xtask verify` (check GUI-02) when one is available.

#![cfg(target_os = "linux")]

mod common;

use rawdawg_plugin_host::vst3::{Vst3Instance, Vst3ModuleCache, describe_vst3_module};
use std::path::PathBuf;

fn bundle() -> PathBuf {
    let status = std::process::Command::new(env!("CARGO")).args(["build", "-q", "-p", "rawdawg-plugin-vst3"]).status().expect("cargo build");
    assert!(status.success());
    let exe = std::env::current_exe().unwrap();
    let module = exe.parent().unwrap().parent().unwrap().join("librawdawg_vst3.so");
    let base = std::env::temp_dir().join(format!("rawdawg-vst3editor-{}", std::process::id()));
    let dir = base.join("rawDAWg.vst3").join("Contents").join("x86_64-linux");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(&module, dir.join("rawDAWg.so")).unwrap();
    base.join("rawDAWg.vst3")
}

#[test]
#[ignore = "needs an X11 display; run by `cargo xtask verify` (GUI-02)"]
fn rawdawg_hosts_its_exported_vst3_editor_and_receives_its_edits() {
    let path = bundle();
    // SAFETY: our own freshly built module.
    let library = unsafe { describe_vst3_module(&path) }.expect("describe");
    let mut cache = Vst3ModuleCache::default();
    // SAFETY: described above.
    let module = unsafe { cache.load(&library.path) }.expect("load");
    let desc = library.plugins.iter().find(|p| p.name == "rawDAWg Delay").unwrap().clone();
    let mut inst = Vst3Instance::new(module, &desc).expect("instantiate");
    common::editor_scenario(&mut inst, "rawdawg.fx.delay", "rawDAWg Delay (VST3)");
}

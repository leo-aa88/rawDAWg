//! rawDAWg hosting the editor of its own exported CLAP plugin on a real display (X11): the
//! host offers `timer-support`, the plugin's editor paints from those timers into a child of
//! the host's window, real X input events drag a slider in it, the edit reaches the plugin's
//! parameters and the host sees the change. Close and reopen work.
//!
//! Needs an X server, so it is `#[ignore]`d in plain `cargo test` and run by
//! `cargo xtask verify` (check GUI-02) when one is available.

#![cfg(target_os = "linux")]

mod common;

use rawdawg_plugin_host::clap::{ClapInstance, ModuleCache};
use std::path::PathBuf;

fn built_clap_module() -> PathBuf {
    let status = std::process::Command::new(env!("CARGO")).args(["build", "-q", "-p", "rawdawg-plugin-clap"]).status().expect("cargo build");
    assert!(status.success());
    let exe = std::env::current_exe().unwrap();
    exe.parent().unwrap().parent().unwrap().join("librawdawg_clap.so")
}

#[test]
#[ignore = "needs an X11 display; run by `cargo xtask verify` (GUI-02)"]
fn rawdawg_hosts_its_exported_clap_editor_and_receives_its_edits() {
    let module_path = built_clap_module();
    // SAFETY: our own freshly built module.
    let library = unsafe { rawdawg_plugin_host::describe::describe_clap_library(&module_path) }.expect("describe");
    let mut cache = ModuleCache::default();
    // SAFETY: described above.
    let module = unsafe { cache.load(&library.path) }.expect("load");
    let desc = library.plugins.iter().find(|p| p.plugin_id == "rawdawg.fx.delay").unwrap().clone();
    assert!(desc.has_gui, "the scan reports an editor");
    let mut inst = ClapInstance::new(module, &desc).expect("instantiate");
    common::editor_scenario(&mut inst, "rawdawg.fx.delay", "rawDAWg Delay (CLAP)");
}

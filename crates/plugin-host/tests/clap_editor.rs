//! rawDAWg hosting the editor of its own exported CLAP plugin on a real display (X11):
//! the host offers `timer-support`, the plugin's editor paints from those timers into a child
//! of the host's window, real X input events drag a slider in it, the edit reaches the
//! plugin's parameters and the host sees the change. Close and reopen work.
//!
//! Needs an X server, so it is `#[ignore]`d in plain `cargo test` and run by
//! `cargo xtask verify` (check GUI-02) when one is available.

#![cfg(target_os = "linux")]

use rawdawg_dsp::processor::Processor;
use rawdawg_plugin_host::clap::{ClapInstance, ModuleCache};
use rawdawg_types::{ParamDescriptor, ParamId};
use rawdawg_ui::plugin_editor::{DEFAULT_SIZE, ParamPort, PluginEditorUi};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ButtonPressEvent, ConnectionExt as _, EventMask, ImageFormat, KeyButMask, MOTION_NOTIFY_EVENT, MapState, Motion, MotionNotifyEvent,
};

fn built_clap_module() -> PathBuf {
    let status = std::process::Command::new(env!("CARGO")).args(["build", "-q", "-p", "rawdawg-plugin-clap"]).status().expect("cargo build");
    assert!(status.success());
    let exe = std::env::current_exe().unwrap();
    exe.parent().unwrap().parent().unwrap().join("librawdawg_clap.so")
}

/// The same parameters and values, to find where the plugin's editor draws its first slider
/// (layout is deterministic: same widgets, fonts, size and scale).
struct MirrorPort(Vec<ParamDescriptor>, Vec<f32>);

impl ParamPort for MirrorPort {
    fn descriptors(&self) -> Vec<ParamDescriptor> {
        self.0.clone()
    }
    fn value(&self, id: ParamId) -> Option<f32> {
        self.0.iter().position(|d| d.id == id).map(|i| self.1[i])
    }
    fn begin_edit(&self, _: ParamId) {}
    fn edit(&self, _: ParamId, _: f32) {}
    fn end_edit(&self, _: ParamId) {}
}

fn first_slider(name: &str, descs: Vec<ParamDescriptor>, values: Vec<f32>) -> (egui::Rect, ParamId) {
    let first_continuous = descs
        .iter()
        .find(|d| matches!(d.mapping, rawdawg_types::ParamMapping::Linear { .. } | rawdawg_types::ParamMapping::Log { .. } | rawdawg_types::ParamMapping::LogWithZero { .. }))
        .unwrap()
        .id;
    let mut ui = PluginEditorUi::new(name, MirrorPort(descs, values));
    let ctx = egui::Context::default();
    rawdawg_ui::theme::apply(&ctx);
    ctx.enable_accesskit();
    let mut slider = None;
    for _ in 0..2 {
        let input =
            egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(DEFAULT_SIZE.0 as f32, DEFAULT_SIZE.1 as f32))), ..Default::default() };
        let mut out = ctx.run_ui(input, |root| rawdawg_ui::EditorUi::ui(&mut ui, root));
        out.textures_delta.clear();
        slider = out.platform_output.accesskit_update.and_then(|u| {
            u.nodes
                .iter()
                .filter(|(_, n)| n.role() == egui::accesskit::Role::Slider)
                .filter_map(|(_, n)| n.bounds())
                .min_by(|a, b| a.y0.total_cmp(&b.y0))
                .map(|b| egui::Rect::from_min_max(egui::pos2(b.x0 as f32, b.y0 as f32), egui::pos2(b.x1 as f32, b.y1 as f32)))
        });
    }
    (slider.expect("a slider is drawn"), first_continuous)
}

fn idle_for(inst: &mut ClapInstance, ms: u64) -> bool {
    let mut edited = false;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_millis(ms) {
        let r = inst.editor_idle();
        assert!(r.open, "editor closed unexpectedly");
        edited |= r.edited;
        std::thread::sleep(Duration::from_millis(5));
    }
    edited
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
    assert!(inst.has_editor(), "embedded X11 editor offered to a host with timer support");

    // the plugin lays out its own (native) descriptors, which carry step labels the hosted
    // descriptors do not: lay out the same ones here
    let exported = rawdawg_plugin_api::exported_plugins().into_iter().find(|p| p.clap_id == "rawdawg.fx.delay").unwrap();
    let wrapper = rawdawg_plugin_api::wrap::wrapper_descriptors(exported.is_synth(), exported.has_mix);
    let descs: Vec<ParamDescriptor> = exported.make().unwrap().descriptors().iter().filter(|d| !d.flags.hidden).chain(wrapper.iter()).copied().collect();
    let values: Vec<f32> = descs.iter().map(|d| inst.param(d.id).unwrap()).collect();
    let (slider, param) = first_slider(&desc.name, descs, values);
    let before = inst.param(param).unwrap();

    inst.open_editor("rawDAWg Delay").expect("open editor");
    let top = inst.editor_window_id().unwrap() as u32;
    let (conn, _) = x11rb::connect(None).expect("display");
    let t0 = Instant::now();
    let child = loop {
        inst.editor_idle();
        let tree = conn.query_tree(top).unwrap().reply().unwrap();
        let viewable = conn.get_window_attributes(top).unwrap().reply().unwrap().map_state == MapState::VIEWABLE;
        if viewable && let Some(&c) = tree.children.first() {
            break c;
        }
        assert!(t0.elapsed() < Duration::from_secs(10), "the plugin's editor window never appeared");
        std::thread::sleep(Duration::from_millis(10));
    };
    idle_for(&mut inst, 400);
    // the host window shows the plugin's editor (painted by the plugin from host timers)
    let img = conn.get_image(ImageFormat::Z_PIXMAP, top, 0, 0, DEFAULT_SIZE.0 as u16, DEFAULT_SIZE.1 as u16, !0).unwrap().reply().unwrap();
    let panel = rawdawg_ui::theme::PANEL;
    let panel_px = img.data.as_chunks::<4>().0.iter().filter(|p| p[2] == panel.r() && p[1] == panel.g() && p[0] == panel.b()).count();
    assert!(panel_px > (DEFAULT_SIZE.0 * DEFAULT_SIZE.1 / 3) as usize, "the editor's panel fills the window ({panel_px} px)");

    // evidence: RAWDAWG_EDITOR_SNAPSHOT=<file.ppm> saves what the X server shows
    if let Some(path) = std::env::var_os("RAWDAWG_EDITOR_SNAPSHOT") {
        let mut ppm = format!("P6 {} {} 255\n", DEFAULT_SIZE.0, DEFAULT_SIZE.1).into_bytes();
        for p in img.data.as_chunks::<4>().0 {
            ppm.extend_from_slice(&[p[2], p[1], p[0]]);
        }
        std::fs::write(path, ppm).unwrap();
    }
    // drag the slider with real X input events sent to the plugin's window
    let send = |ev: [u8; 32], mask: EventMask| {
        conn.send_event(false, child, mask, ev).unwrap();
        conn.flush().unwrap();
    };
    let root = conn.setup().roots[0].root;
    let (x, y) = (slider.center().x as i16, slider.center().y as i16);
    let press = |kind: u8, x: i16, state: KeyButMask| ButtonPressEvent {
        response_type: kind,
        detail: 1,
        sequence: 0,
        time: 0,
        root,
        event: child,
        child: x11rb::NONE,
        root_x: x,
        root_y: y,
        event_x: x,
        event_y: y,
        state,
        same_screen: true,
    };
    send(press(BUTTON_PRESS_EVENT, x, KeyButMask::default()).into(), EventMask::BUTTON_PRESS);
    idle_for(&mut inst, 100);
    let mut edited = false;
    for k in 1..=6 {
        let m = MotionNotifyEvent {
            response_type: MOTION_NOTIFY_EVENT,
            detail: Motion::NORMAL,
            sequence: 0,
            time: 0,
            root,
            event: child,
            child: x11rb::NONE,
            root_x: x + 12 * k,
            root_y: y,
            event_x: x + 12 * k,
            event_y: y,
            state: KeyButMask::BUTTON1,
            same_screen: true,
        };
        send(m.into(), EventMask::POINTER_MOTION);
        edited |= idle_for(&mut inst, 60);
    }
    send(press(BUTTON_RELEASE_EVENT, x + 72, KeyButMask::BUTTON1).into(), EventMask::BUTTON_RELEASE);
    edited |= idle_for(&mut inst, 300);
    let after = inst.param(param).unwrap();
    assert!(edited, "the host saw the editor's edit");
    assert!(after > before + 0.01, "dragging right raised the parameter: {before} -> {after}");
    let state: serde_json::Value = serde_json::from_slice(&inst.save_state()).unwrap();
    assert_eq!(state["format"], "clap", "the edit is part of the plugin state the host saves");

    // close and reopen
    inst.close_editor();
    assert!(!inst.editor_is_open());
    conn.flush().unwrap();
    assert!(conn.get_window_attributes(top).unwrap().reply().is_err(), "the host window is gone");
    inst.open_editor("rawDAWg Delay").expect("reopen");
    idle_for(&mut inst, 200);
    assert!(inst.editor_is_open());
    drop(inst); // closes the editor before the plugin goes
}

//! Plugin editor windows on a real display (X11 on Linux, Win32 on Windows).
//!
//! These need a display server, so they are `#[ignore]`d in plain `cargo test` and run by
//! `cargo xtask verify` (check GUI-02) when one is available: WSLg/Xorg/Xvfb on Linux, the
//! desktop session on Windows. A run without a display reports the check BLOCKED instead of
//! passing.

use rawdawg_types::params::ParamCategory;
use rawdawg_types::{ParamDescriptor, ParamId, ParamMapping, ParamUnit};
use rawdawg_ui::plugin_editor::{DEFAULT_SIZE, ParamPort, PluginEditorUi};
use rawdawg_ui::window::{NativeWindow, Parent};
use rawdawg_ui::{Editor, theme};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

static DESCRIPTORS: [ParamDescriptor; 2] = [
    ParamDescriptor::continuous(1, "gain", "Gain", ParamUnit::Decibel, ParamMapping::Linear { min: -24.0, max: 24.0 }, 0.5, ParamCategory::Effects),
    ParamDescriptor::continuous(2, "on", "Enabled", ParamUnit::None, ParamMapping::Toggle, 1.0, ParamCategory::Effects),
];

#[derive(Clone, Default)]
struct Port(Rc<RefCell<Vec<String>>>);

impl ParamPort for Port {
    fn descriptors(&self) -> Vec<ParamDescriptor> {
        DESCRIPTORS.to_vec()
    }
    fn value(&self, id: ParamId) -> Option<f32> {
        DESCRIPTORS.iter().find(|d| d.id == id).map(|d| d.default_normalized)
    }
    fn begin_edit(&self, id: ParamId) {
        self.0.borrow_mut().push(format!("begin {}", id.0));
    }
    fn edit(&self, id: ParamId, n: f32) {
        self.0.borrow_mut().push(format!("edit {} {n:.2}", id.0));
    }
    fn end_edit(&self, id: ParamId) {
        self.0.borrow_mut().push(format!("end {}", id.0));
    }
}

fn tick_until(editor: &mut Editor, what: &str, mut done: impl FnMut(&mut Editor) -> bool) {
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(10) {
        #[cfg(target_os = "windows")]
        rawdawg_ui::window::pump_messages();
        editor.tick();
        if done(editor) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for: {what}");
}

#[test]
#[ignore = "needs a display server; run by `cargo xtask verify` (GUI-02)"]
fn editor_draws_into_a_child_of_a_host_window_resizes_and_closes_with_its_parent() {
    // the "host": a top-level window, as rawDAWg uses for third-party editors
    let host = NativeWindow::open(Parent::TopLevel, 700, 520, "rawDAWg editor test").expect("display available");
    let parent = if cfg!(target_os = "windows") { Parent::Win32(host.raw_id() as isize) } else { Parent::X11(host.raw_id() as u32) };
    let ui = PluginEditorUi::new("Test Plugin", Port::default());
    let mut editor = Editor::open(parent, DEFAULT_SIZE, 1.0, "editor", Box::new(ui)).expect("open editor");
    assert_eq!(editor.logical_size(), DEFAULT_SIZE);
    tick_until(&mut editor, "first frame", |e| e.painter().size() == (640, 480));
    // the window manager maps the host window asynchronously; once viewable, the expose that
    // follows makes the editor draw again
    #[cfg(target_os = "linux")]
    tick_until(&mut editor, "viewable editor", |e| e.window().is_viewable());
    for _ in 0..20 {
        editor.tick();
        std::thread::sleep(Duration::from_millis(10));
    }
    // what the display server shows is exactly what the painter drew
    let drawn = editor.painter().pixels().to_vec();
    assert!(drawn.iter().any(|&p| p != 0), "the frame has content");
    let panel = u32::from(theme::PANEL.r()) << 16 | u32::from(theme::PANEL.g()) << 8 | u32::from(theme::PANEL.b());
    assert!(drawn.iter().filter(|&&p| p == panel).count() > drawn.len() / 3, "mostly the dark panel colour");
    #[cfg(target_os = "linux")]
    {
        editor.window().sync().unwrap();
        let shown = editor.window().read_back().unwrap();
        assert_eq!(shown.len(), drawn.len());
        let same = shown.iter().zip(&drawn).filter(|(a, b)| a == b).count();
        assert_eq!(same, drawn.len(), "{} of {} pixels differ from the painted frame", drawn.len() - same, drawn.len());
    }
    #[cfg(target_os = "windows")]
    {
        for (x, y) in [(5u32, 5u32), (320, 240), (600, 400)] {
            assert_eq!(editor.window().read_pixel(x, y), Some(drawn[(y * 640 + x) as usize]), "pixel {x},{y}");
        }
    }
    // resize: the next frame has the new size
    editor.set_logical_size(500, 300).unwrap();
    tick_until(&mut editor, "resized frame", |e| e.painter().size() == (500, 300));
    // HiDPI: twice the pixels for the same logical size
    editor.set_scale(2.0);
    editor.set_logical_size(500, 300).unwrap();
    tick_until(&mut editor, "2x frame", |e| e.painter().size() == (1000, 600));
    assert_eq!(editor.logical_size(), (500, 300));
    // the host destroys its window: the editor notices and reports closed
    drop(host);
    tick_until(&mut editor, "parent destruction", |e| e.is_closed());
}

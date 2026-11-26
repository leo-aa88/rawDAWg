//! The plugin editor (CLAP `gui` extension, PLUGIN_API.md §2, ADR 0006).
//!
//! The editor is `rawdawg_ui::PluginEditorUi` in a native child window of the host's parent
//! (X11 on Linux, Win32 on Windows). It is ticked on the host's main thread by CLAP
//! `timer-support`. On Linux a host without `timer-support` is told the GUI is unsupported,
//! so it falls back to its generic controls instead of showing a frozen editor. On Windows,
//! without host timers, a Win32 timer on the editor window ticks it from the host's message
//! loop.
//!
//! Edits made in the editor update the main-thread state at once, and are queued in
//! [`PendingEdits`] for the audio thread to apply (as sample-accurate parameter events) and to
//! report to the host as CLAP output events with gestures, so the host records automation.
//! While the plugin is inactive, the main-thread `params.flush` reports them instead.

use crate::{Core, MainThread};
use clack_extensions::gui::{AspectRatioStrategy, GuiApiType, GuiConfiguration, GuiResizeHints, GuiSize, PluginGuiImpl, Window};
use clack_extensions::params::HostParams;
use clack_extensions::timer::{HostTimer, PluginTimerImpl, TimerId};
use clack_plugin::prelude::*;
use rawdawg_types::{ParamDescriptor, ParamId};
use rawdawg_ui::plugin_editor::{DEFAULT_SIZE, MIN_SIZE, ParamPort, PluginEditorUi};
use rawdawg_ui::window::Parent;
use rawdawg_ui::{Editor, Tick};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

pub(crate) const BEGIN: u8 = 1;
pub(crate) const VALUE: u8 = 2;
pub(crate) const END: u8 = 4;

/// Editor repaint / input rate (ms between host timer ticks).
const TICK_MS: u32 = 16;

/// Latest editor edit per parameter, lock-free: written on the main thread, taken on the
/// audio thread (or the main thread while inactive; CLAP never runs both flushes at once).
pub(crate) struct PendingEdits {
    ids: Vec<u32>,
    values: Vec<AtomicU32>,
    flags: Vec<AtomicU8>,
    any: AtomicBool,
}

impl PendingEdits {
    pub(crate) fn new(ids: impl Iterator<Item = u32>) -> Self {
        let mut ids: Vec<u32> = ids.collect();
        ids.sort_unstable();
        ids.dedup();
        Self { values: ids.iter().map(|_| AtomicU32::new(0)).collect(), flags: ids.iter().map(|_| AtomicU8::new(0)).collect(), ids, any: AtomicBool::new(false) }
    }

    /// Record an edit (`BEGIN`, `VALUE` or `END`) of `id`.
    pub(crate) fn push(&self, id: u32, flag: u8, value: f32) {
        let Ok(i) = self.ids.binary_search(&id) else { return };
        if flag & VALUE != 0 {
            self.values[i].store(value.to_bits(), Ordering::Release);
        }
        self.flags[i].fetch_or(flag, Ordering::AcqRel);
        self.any.store(true, Ordering::Release);
    }

    pub(crate) fn pending(&self) -> bool {
        self.any.load(Ordering::Acquire)
    }

    /// Take every pending edit: `f(id, flags, latest value)`. Allocation-free.
    pub(crate) fn drain(&self, mut f: impl FnMut(u32, u8, f32)) {
        if !self.any.swap(false, Ordering::AcqRel) {
            return;
        }
        for (i, id) in self.ids.iter().enumerate() {
            let flags = self.flags[i].swap(0, Ordering::AcqRel);
            if flags != 0 {
                f(*id, flags, f32::from_bits(self.values[i].load(Ordering::Acquire)));
            }
        }
    }
}

/// The editor's view of the plugin's parameters.
pub(crate) struct ClapPort {
    core: Arc<Core>,
    descriptors: Vec<ParamDescriptor>,
}

impl ParamPort for ClapPort {
    fn descriptors(&self) -> Vec<ParamDescriptor> {
        self.descriptors.clone()
    }
    fn value(&self, id: ParamId) -> Option<f32> {
        self.core.value(id.0)
    }
    fn begin_edit(&self, id: ParamId) {
        self.core.edits.push(id.0, BEGIN, 0.0);
    }
    fn edit(&self, id: ParamId, normalized: f32) {
        let n = normalized.clamp(0.0, 1.0);
        if !self.core.set_wrapper_param(id.0, n)
            && let Ok(mut main) = self.core.main.lock()
        {
            main.set_param(id, n);
        }
        self.core.edits.push(id.0, VALUE, n);
    }
    fn end_edit(&self, id: ParamId) {
        self.core.edits.push(id.0, END, 0.0);
    }
}

/// Editor state of one plugin instance (main thread only).
#[derive(Default)]
pub(crate) struct GuiState {
    created: bool,
    scale: f32,
    /// Size in points.
    size: (u32, u32),
    editor: Option<Rc<RefCell<Editor>>>,
    timer: Option<TimerId>,
}

#[cfg(target_os = "linux")]
fn platform_api() -> GuiApiType<'static> {
    GuiApiType::X11
}

#[cfg(target_os = "windows")]
fn platform_api() -> GuiApiType<'static> {
    GuiApiType::WIN32
}

impl MainThread<'_> {
    fn host_timer(&self) -> Option<HostTimer> {
        self.host.get_extension::<HostTimer>()
    }

    fn physical(&self, logical: (u32, u32), scale: f32) -> GuiSize {
        GuiSize { width: (logical.0 as f32 * scale).round() as u32, height: (logical.1 as f32 * scale).round() as u32 }
    }

    /// Tick the editor and tell the host about edits waiting to be reported.
    fn tick_editor(&self) -> Option<Tick> {
        let editor = self.gui.borrow().editor.clone()?;
        let tick = editor.borrow_mut().tick();
        if self.shared.edits.pending()
            && let Some(params) = self.host.get_extension::<HostParams>()
        {
            params.request_flush(&self.host.shared());
        }
        Some(tick)
    }
}

impl PluginGuiImpl for MainThread<'_> {
    fn is_api_supported(&self, configuration: GuiConfiguration) -> bool {
        configuration.api_type == platform_api() && !configuration.is_floating && (cfg!(target_os = "windows") || self.host_timer().is_some())
    }

    fn get_preferred_api(&self) -> Option<GuiConfiguration<'_>> {
        let c = GuiConfiguration { api_type: platform_api(), is_floating: false };
        self.is_api_supported(c).then_some(c)
    }

    fn create(&self, configuration: GuiConfiguration) -> Result<(), PluginError> {
        if !self.is_api_supported(configuration) {
            return Err(PluginError::Message("this editor needs an embedded x11 / win32 window and, on Linux, host timer support"));
        }
        let mut gui = self.gui.borrow_mut();
        *gui = GuiState { created: true, scale: 1.0, size: DEFAULT_SIZE, editor: None, timer: None };
        Ok(())
    }

    fn destroy(&self) {
        let (timer, editor) = {
            let mut gui = self.gui.borrow_mut();
            gui.created = false;
            (gui.timer.take(), gui.editor.take())
        };
        if let (Some(id), Some(t)) = (timer, self.host_timer()) {
            let _ = t.unregister_timer(&self.host, id);
        }
        // dropping the editor ends a gesture still in progress and destroys the window
        drop(editor);
        if self.shared.edits.pending()
            && let Some(params) = self.host.get_extension::<HostParams>()
        {
            params.request_flush(&self.host.shared());
        }
    }

    fn set_scale(&self, scale: f64) -> Result<(), PluginError> {
        let scale = scale as f32;
        if !scale.is_finite() || scale <= 0.25 {
            return Err(PluginError::Message("unsupported scale"));
        }
        let mut gui = self.gui.borrow_mut();
        gui.scale = scale.min(4.0);
        if let Some(e) = &gui.editor {
            let mut e = e.borrow_mut();
            e.set_scale(scale);
            let _ = e.set_logical_size(gui.size.0, gui.size.1);
        }
        Ok(())
    }

    fn get_size(&self) -> Option<GuiSize> {
        let gui = self.gui.borrow();
        gui.created.then(|| self.physical(gui.size, gui.scale))
    }

    fn can_resize(&self) -> bool {
        true
    }

    fn get_resize_hints(&self) -> Option<GuiResizeHints> {
        Some(GuiResizeHints { can_resize_horizontally: true, can_resize_vertically: true, strategy: AspectRatioStrategy::Disregard })
    }

    fn adjust_size(&self, size: GuiSize) -> Option<GuiSize> {
        let scale = self.gui.borrow().scale.max(0.25);
        let min = self.physical(MIN_SIZE, scale);
        Some(GuiSize { width: size.width.max(min.width), height: size.height.max(min.height) })
    }

    fn set_size(&self, size: GuiSize) -> Result<(), PluginError> {
        let mut gui = self.gui.borrow_mut();
        let scale = gui.scale.max(0.25);
        gui.size = (((size.width as f32 / scale).round() as u32).max(MIN_SIZE.0), ((size.height as f32 / scale).round() as u32).max(MIN_SIZE.1));
        if let Some(e) = &gui.editor {
            e.borrow_mut().set_logical_size(gui.size.0, gui.size.1).map_err(|_| PluginError::Message("resize failed"))?;
        }
        Ok(())
    }

    fn set_parent(&self, window: Window) -> Result<(), PluginError> {
        // `c_ulong` is 32 bits on Windows and 64 on Linux: only the platform's branch is built
        #[cfg(target_os = "windows")]
        let parent = Parent::Win32(window.as_win32_hwnd().ok_or(PluginError::Message("expected a win32 parent window"))? as isize);
        #[cfg(not(target_os = "windows"))]
        let parent = Parent::X11(window.as_x11_handle().ok_or(PluginError::Message("expected an x11 parent window"))? as u32);
        let (size, scale) = {
            let gui = self.gui.borrow();
            if !gui.created {
                return Err(PluginError::Message("set_parent before create"));
            }
            (gui.size, gui.scale)
        };
        let wrapper = rawdawg_plugin_api::wrap::wrapper_descriptors(self.shared.is_synth, self.shared.has_mix);
        let descriptors: Vec<ParamDescriptor> = {
            let main = self.shared.main.lock().map_err(|_| PluginError::Message("state lock poisoned"))?;
            main.descriptors().iter().filter(|d| !d.flags.hidden).chain(wrapper.iter()).copied().collect()
        };
        let port = ClapPort { core: self.shared.core.clone(), descriptors };
        let ui = PluginEditorUi::new(self.shared.exported.name.clone(), port);
        let editor = Editor::open(parent, size, scale, &self.shared.exported.name, Box::new(ui)).map_err(|_| PluginError::Message("cannot open the editor window"))?;
        let editor = Rc::new(RefCell::new(editor));
        let timer = match self.host_timer() {
            Some(t) => Some(t.register_timer(&self.host, TICK_MS).map_err(|_| PluginError::Message("host refused the editor timer"))?),
            None => {
                // only reachable on Windows (is_api_supported): tick from a Win32 timer
                #[cfg(target_os = "windows")]
                {
                    let weak = Rc::downgrade(&editor);
                    editor.borrow_mut().window_mut().set_timer(
                        TICK_MS,
                        Box::new(move || {
                            if let Some(e) = weak.upgrade()
                                && let Ok(mut e) = e.try_borrow_mut()
                            {
                                e.tick();
                            }
                        }),
                    );
                }
                None
            }
        };
        let mut gui = self.gui.borrow_mut();
        gui.editor = Some(editor);
        gui.timer = timer;
        Ok(())
    }

    fn set_transient(&self, _window: Window) -> Result<(), PluginError> {
        Err(PluginError::Message("the editor is embedded only"))
    }

    fn show(&self) -> Result<(), PluginError> {
        if let Some(e) = &self.gui.borrow().editor {
            e.borrow_mut().set_visible(true).map_err(|_| PluginError::Message("show failed"))?;
        }
        Ok(())
    }

    fn hide(&self) -> Result<(), PluginError> {
        if let Some(e) = &self.gui.borrow().editor {
            e.borrow_mut().set_visible(false).map_err(|_| PluginError::Message("hide failed"))?;
        }
        Ok(())
    }
}

impl PluginTimerImpl for MainThread<'_> {
    fn on_timer(&self, timer_id: TimerId) {
        if self.gui.borrow().timer == Some(timer_id) {
            self.tick_editor();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_edits_keep_the_latest_value_and_every_gesture_flag() {
        let e = PendingEdits::new([7u32, 3, 9, 3].into_iter());
        assert!(!e.pending());
        e.push(3, BEGIN, 0.0);
        e.push(3, VALUE, 0.25);
        e.push(3, VALUE, 0.75);
        e.push(3, END, 0.0);
        e.push(9, VALUE, 1.0);
        e.push(42, VALUE, 1.0); // unknown ids are ignored
        assert!(e.pending());
        let mut seen = Vec::new();
        e.drain(|id, flags, v| seen.push((id, flags, v)));
        assert_eq!(seen, vec![(3, BEGIN | VALUE | END, 0.75), (9, VALUE, 1.0)]);
        assert!(!e.pending());
        let mut again = 0;
        e.drain(|_, _, _| again += 1);
        assert_eq!(again, 0, "drained once");
    }
}

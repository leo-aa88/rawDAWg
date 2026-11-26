//! The plugin editor (VST3 `IPlugView`, PLUGIN_API.md §2, ADR 0006).
//!
//! The same `rawdawg_ui::PluginEditorUi` as the CLAP export, in a native child window of the
//! host's parent (`X11EmbedWindowID` on Linux, `HWND` on Windows). On Linux it is ticked by a
//! timer registered with the host's `Linux::IRunLoop` (taken from the `IPlugFrame`, as the
//! VST3 SDK specifies); a host without a run loop gets `kResultFalse` from `attached` rather
//! than a frozen editor. On Windows a Win32 timer on the editor window ticks it from the host's
//! message loop.
//!
//! Edits go to the host through `IComponentHandler::beginEdit / performEdit / endEdit` (the
//! host then sends them to the processor and records automation), and update the
//! controller's own values.

use crate::ControllerState;
use rawdawg_types::{ParamDescriptor, ParamId};
use rawdawg_ui::Editor;
use rawdawg_ui::plugin_editor::{DEFAULT_SIZE, MIN_SIZE, ParamPort, PluginEditorUi};
use rawdawg_ui::window::Parent;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
#[cfg(target_os = "linux")]
use vst3::Steinberg::Linux::IRunLoopTrait as _;
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComRef, ComWrapper};

/// Editor repaint / input rate (ms).
const TICK_MS: u32 = 16;

#[cfg(target_os = "linux")]
const PLATFORM: &std::ffi::CStr = c"X11EmbedWindowID";
#[cfg(target_os = "windows")]
const PLATFORM: &std::ffi::CStr = c"HWND";

/// The editor's view of the controller's parameters.
struct Vst3Port {
    state: Rc<ControllerState>,
}

impl Vst3Port {
    fn handler(&self) -> Option<ComPtr<IComponentHandler>> {
        // cloned out so a host calling back into the controller finds nothing borrowed
        self.state.handler.borrow().clone()
    }
}

impl ParamPort for Vst3Port {
    fn descriptors(&self) -> Vec<ParamDescriptor> {
        self.state.visible.iter().map(|d| **d).collect()
    }
    fn value(&self, id: ParamId) -> Option<f32> {
        self.state.value(id.0).map(|v| v as f32)
    }
    fn begin_edit(&self, id: ParamId) {
        if let Some(h) = self.handler() {
            // SAFETY: a live handler the host gave us.
            unsafe { h.beginEdit(id.0) };
        }
    }
    fn edit(&self, id: ParamId, normalized: f32) {
        let v = f64::from(normalized.clamp(0.0, 1.0));
        self.state.set_normalized(id.0, v);
        if let Some(h) = self.handler() {
            // SAFETY: as above.
            unsafe { h.performEdit(id.0, v) };
        }
    }
    fn end_edit(&self, id: ParamId) {
        if let Some(h) = self.handler() {
            // SAFETY: as above.
            unsafe { h.endEdit(id.0) };
        }
    }
}

/// Ticks the editor from the host's run loop (Linux).
#[cfg(target_os = "linux")]
struct TimerHandler {
    editor: std::rc::Weak<RefCell<Editor>>,
}

#[cfg(target_os = "linux")]
impl Class for TimerHandler {
    type Interfaces = (Linux::ITimerHandler,);
}

#[cfg(target_os = "linux")]
impl Linux::ITimerHandlerTrait for TimerHandler {
    unsafe fn onTimer(&self) {
        if let Some(e) = self.editor.upgrade()
            && let Ok(mut e) = e.try_borrow_mut()
        {
            e.tick();
        }
    }
}

pub struct PlugView {
    state: Rc<ControllerState>,
    editor: RefCell<Option<Rc<RefCell<Editor>>>>,
    frame: RefCell<Option<ComPtr<IPlugFrame>>>,
    /// Size in points.
    size: Cell<(u32, u32)>,
    scale: Cell<f32>,
    #[cfg(target_os = "linux")]
    run_loop: RefCell<Option<(ComPtr<Linux::IRunLoop>, ComWrapper<TimerHandler>)>>,
}

impl Class for PlugView {
    type Interfaces = (IPlugView, IPlugViewContentScaleSupport);
}

impl PlugView {
    /// A new view for the host (one reference, owned by the caller).
    pub fn create(state: Rc<ControllerState>) -> *mut IPlugView {
        let view = PlugView {
            state,
            editor: RefCell::new(None),
            frame: RefCell::new(None),
            size: Cell::new(DEFAULT_SIZE),
            scale: Cell::new(1.0),
            #[cfg(target_os = "linux")]
            run_loop: RefCell::new(None),
        };
        ComWrapper::new(view).to_com_ptr::<IPlugView>().map_or(std::ptr::null_mut(), |p| p.into_raw())
    }

    fn physical(&self, logical: (u32, u32)) -> (i32, i32) {
        let s = self.scale.get();
        ((logical.0 as f32 * s).round() as i32, (logical.1 as f32 * s).round() as i32)
    }

    fn detach(&self) {
        #[cfg(target_os = "linux")]
        if let Some((run_loop, handler)) = self.run_loop.borrow_mut().take()
            && let Some(h) = handler.to_com_ptr::<Linux::ITimerHandler>()
        {
            // SAFETY: the handler registered in `attached`.
            unsafe { run_loop.unregisterTimer(h.as_ptr()) };
        }
        // dropping the editor ends a gesture in progress and destroys the window
        self.editor.borrow_mut().take();
    }
}

impl Drop for PlugView {
    fn drop(&mut self) {
        self.detach();
    }
}

fn is_platform(t: FIDString) -> bool {
    // SAFETY: a NUL-terminated string from the host (checked for null).
    !t.is_null() && unsafe { std::ffi::CStr::from_ptr(t) } == PLATFORM
}

impl IPlugViewTrait for PlugView {
    unsafe fn isPlatformTypeSupported(&self, r#type: FIDString) -> tresult {
        if is_platform(r#type) { kResultTrue } else { kResultFalse }
    }

    unsafe fn attached(&self, parent: *mut std::ffi::c_void, r#type: FIDString) -> tresult {
        if parent.is_null() || !is_platform(r#type) || self.editor.borrow().is_some() {
            return kResultFalse;
        }
        #[cfg(target_os = "linux")]
        let run_loop = match self.frame.borrow().as_ref().and_then(|f| f.cast::<Linux::IRunLoop>()) {
            Some(r) => r,
            None => return kResultFalse,
        };
        #[cfg(target_os = "linux")]
        let parent = Parent::X11(parent as usize as u32);
        #[cfg(target_os = "windows")]
        let parent = Parent::Win32(parent as isize);
        let ui = PluginEditorUi::new(self.state.plugin.name.clone(), Vst3Port { state: self.state.clone() });
        let Ok(editor) = Editor::open(parent, self.size.get(), self.scale.get(), &self.state.plugin.name, Box::new(ui)) else { return kResultFalse };
        let editor = Rc::new(RefCell::new(editor));
        #[cfg(target_os = "linux")]
        {
            let handler = ComWrapper::new(TimerHandler { editor: Rc::downgrade(&editor) });
            let Some(h) = handler.to_com_ptr::<Linux::ITimerHandler>() else { return kResultFalse };
            // SAFETY: a live handler; it stays registered until `removed`.
            if unsafe { run_loop.registerTimer(h.as_ptr(), u64::from(TICK_MS)) } != kResultOk {
                return kResultFalse;
            }
            *self.run_loop.borrow_mut() = Some((run_loop, handler));
        }
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
        *self.editor.borrow_mut() = Some(editor);
        kResultOk
    }

    unsafe fn removed(&self) -> tresult {
        self.detach();
        kResultOk
    }

    unsafe fn onWheel(&self, _distance: f32) -> tresult {
        // input arrives through the editor's own window
        kResultFalse
    }

    unsafe fn onKeyDown(&self, _key: char16, _key_code: int16, _modifiers: int16) -> tresult {
        kResultFalse
    }

    unsafe fn onKeyUp(&self, _key: char16, _key_code: int16, _modifiers: int16) -> tresult {
        kResultFalse
    }

    unsafe fn getSize(&self, size: *mut ViewRect) -> tresult {
        if size.is_null() {
            return kInvalidArgument;
        }
        let (w, h) = self.physical(self.size.get());
        // SAFETY: checked for null; the host owns the rect.
        unsafe { *size = ViewRect { left: 0, top: 0, right: w, bottom: h } };
        kResultOk
    }

    unsafe fn onSize(&self, new_size: *mut ViewRect) -> tresult {
        // SAFETY: the host's rect, checked for null.
        let Some(r) = (unsafe { new_size.as_ref() }) else { return kInvalidArgument };
        let s = self.scale.get();
        let w = (((r.right - r.left).max(1) as f32 / s).round() as u32).max(MIN_SIZE.0);
        let h = (((r.bottom - r.top).max(1) as f32 / s).round() as u32).max(MIN_SIZE.1);
        self.size.set((w, h));
        if let Some(e) = self.editor.borrow().as_ref() {
            let _ = e.borrow_mut().set_logical_size(w, h);
        }
        kResultOk
    }

    unsafe fn onFocus(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn setFrame(&self, frame: *mut IPlugFrame) -> tresult {
        // SAFETY: a live frame (or null) from the host; `to_com_ptr` takes a reference.
        *self.frame.borrow_mut() = unsafe { ComRef::from_raw(frame) }.map(|f| f.to_com_ptr());
        kResultOk
    }

    unsafe fn canResize(&self) -> tresult {
        kResultTrue
    }

    unsafe fn checkSizeConstraint(&self, rect: *mut ViewRect) -> tresult {
        // SAFETY: the host's rect, checked for null.
        let Some(r) = (unsafe { rect.as_mut() }) else { return kInvalidArgument };
        let (min_w, min_h) = self.physical(MIN_SIZE);
        r.right = r.left + (r.right - r.left).max(min_w);
        r.bottom = r.top + (r.bottom - r.top).max(min_h);
        kResultTrue
    }
}

impl IPlugViewContentScaleSupportTrait for PlugView {
    unsafe fn setContentScaleFactor(&self, factor: f32) -> tresult {
        if !factor.is_finite() || factor <= 0.25 {
            return kInvalidArgument;
        }
        self.scale.set(factor.min(4.0));
        if let Some(e) = self.editor.borrow().as_ref() {
            let mut e = e.borrow_mut();
            e.set_scale(factor);
            let (w, h) = self.size.get();
            let _ = e.set_logical_size(w, h);
        }
        kResultOk
    }
}

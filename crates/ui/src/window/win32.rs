//! Win32 child and top-level windows for plugin editors.
//!
//! The window class is registered per loaded module (its name carries the module address),
//! so two copies of the plugin DLL in one process never share a window procedure. Messages
//! are dispatched by the host's own message loop on its UI thread; the window procedure only
//! queues events (and runs the repaint timer callback) for the editor to consume.
//!
//! # Safety
//! All Win32 calls take handles this module created. The per-window state lives in a `Box`
//! whose address is stored in `GWLP_USERDATA`; it outlives the window: `Drop` destroys the
//! window (which clears the pointer in `WM_DESTROY`) before the box is freed.

use super::{Key, Modifiers, MouseButton, Parent, WindowError, WindowEvent};
use std::cell::{Cell, RefCell};
use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, DIB_RGB_COLORS, EndPaint, GetDC, GetPixel, PAINTSTRUCT, ReleaseDC, SetDIBitsToDevice};
use windows_sys::Win32::System::LibraryLoader::{GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleHandleExW};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

/// `WM_MOUSELEAVE` (0x02A3) lives in windows-sys' `Win32_UI_Controls` module; defined here so
/// the match below compares against a constant instead of binding a catch-all variable.
const WM_MOUSELEAVE: u32 = 0x02A3;

struct State {
    events: RefCell<Vec<WindowEvent>>,
    timer: RefCell<Option<Box<dyn FnMut()>>>,
    tracking: Cell<bool>,
    high_surrogate: Cell<u16>,
    alive: Cell<bool>,
}

pub struct Win32Window {
    hwnd: HWND,
    state: Box<State>,
    width: u32,
    height: u32,
}

fn err(what: &str) -> WindowError {
    WindowError::new(format!("Win32: {what} failed (error {})", unsafe { windows_sys::Win32::Foundation::GetLastError() }))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// This module's instance handle (the plugin DLL, not the host executable).
fn module_instance() -> HINSTANCE {
    let mut h: HINSTANCE = std::ptr::null_mut();
    // SAFETY: the address is a function of this module; the out pointer is valid.
    unsafe { GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, wndproc as *const u16, &mut h) };
    h
}

fn class_name(instance: HINSTANCE) -> Vec<u16> {
    wide(&format!("rawDAWgEditor{:x}", instance as usize))
}

fn register_class(instance: HINSTANCE) -> Vec<u16> {
    let name = class_name(instance);
    // SAFETY: a plain class description; registering twice fails harmlessly.
    unsafe {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: std::ptr::null_mut(),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: name.as_ptr(),
            hIconSm: std::ptr::null_mut(),
        };
        RegisterClassExW(&wc);
    }
    name
}

fn modifiers() -> Modifiers {
    // SAFETY: reads the calling thread's keyboard state.
    unsafe { Modifiers { shift: GetKeyState(0x10) < 0, ctrl: GetKeyState(0x11) < 0, alt: GetKeyState(0x12) < 0 } }
}

fn xy(lparam: LPARAM) -> (f32, f32) {
    (f32::from((lparam & 0xFFFF) as u16 as i16), f32::from(((lparam >> 16) & 0xFFFF) as u16 as i16))
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: see the module documentation; the pointer is either null or a live State.
    unsafe {
        if msg == WM_NCCREATE {
            let cs = lparam as *const CREATESTRUCTW;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*cs).lpCreateParams as isize);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const State;
        if state.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let s = &*state;
        let push = |e: WindowEvent| s.events.borrow_mut().push(e);
        let button = |b: MouseButton, pressed: bool| {
            if pressed {
                SetFocus(hwnd);
                SetCapture(hwnd);
            } else {
                ReleaseCapture();
            }
            let (x, y) = xy(lparam);
            push(WindowEvent::PointerMoved(x, y));
            push(WindowEvent::Button { button: b, pressed, modifiers: modifiers() });
        };
        match msg {
            WM_MOUSEMOVE => {
                if !s.tracking.get() {
                    let mut t = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    TrackMouseEvent(&mut t);
                    s.tracking.set(true);
                }
                let (x, y) = xy(lparam);
                push(WindowEvent::PointerMoved(x, y));
            }
            WM_MOUSELEAVE => {
                s.tracking.set(false);
                push(WindowEvent::PointerLeft);
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => button(MouseButton::Left, true),
            WM_LBUTTONUP => button(MouseButton::Left, false),
            WM_RBUTTONDOWN | WM_RBUTTONDBLCLK => button(MouseButton::Right, true),
            WM_RBUTTONUP => button(MouseButton::Right, false),
            WM_MBUTTONDOWN | WM_MBUTTONDBLCLK => button(MouseButton::Middle, true),
            WM_MBUTTONUP => button(MouseButton::Middle, false),
            WM_MOUSEWHEEL => push(WindowEvent::Scroll(0.0, f32::from(((wparam >> 16) & 0xFFFF) as u16 as i16) / 120.0)),
            WM_MOUSEHWHEEL => push(WindowEvent::Scroll(f32::from(((wparam >> 16) & 0xFFFF) as u16 as i16) / 120.0, 0.0)),
            WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP => {
                let pressed = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
                let key = match wparam {
                    0x08 => Key::Backspace,
                    0x09 => Key::Tab,
                    0x0D => Key::Enter,
                    0x1B => Key::Escape,
                    0x25 => Key::Left,
                    0x26 => Key::Up,
                    0x27 => Key::Right,
                    0x28 => Key::Down,
                    0x24 => Key::Home,
                    0x23 => Key::End,
                    0x2E => Key::Delete,
                    _ => Key::Other,
                };
                if key != Key::Other {
                    push(WindowEvent::Key { key, pressed, modifiers: modifiers() });
                }
                if msg == WM_SYSKEYDOWN || msg == WM_SYSKEYUP {
                    return DefWindowProcW(hwnd, msg, wparam, lparam);
                }
            }
            WM_CHAR => {
                let unit = wparam as u16;
                if (0xD800..0xDC00).contains(&unit) {
                    s.high_surrogate.set(unit);
                } else {
                    let units = if (0xDC00..0xE000).contains(&unit) { vec![s.high_surrogate.replace(0), unit] } else { vec![unit] };
                    if let Ok(text) = String::from_utf16(&units)
                        && text.chars().all(|c| !c.is_control())
                    {
                        push(WindowEvent::Text(text));
                    }
                }
            }
            WM_GETDLGCODE => return DLGC_WANTALLKEYS as LRESULT,
            WM_SETFOCUS => push(WindowEvent::FocusChanged(true)),
            WM_KILLFOCUS => push(WindowEvent::FocusChanged(false)),
            WM_SIZE => push(WindowEvent::Resized((lparam & 0xFFFF) as u32, ((lparam >> 16) & 0xFFFF) as u32)),
            WM_ERASEBKGND => return 1,
            WM_PAINT => {
                let mut ps: PAINTSTRUCT = std::mem::zeroed();
                BeginPaint(hwnd, &mut ps);
                EndPaint(hwnd, &ps);
                push(WindowEvent::Redraw);
            }
            WM_TIMER => {
                // take the callback out while it runs: it may borrow this window's events
                let cb = s.timer.borrow_mut().take();
                if let Some(mut cb) = cb {
                    cb();
                    if s.alive.get() {
                        s.timer.borrow_mut().get_or_insert(cb);
                    }
                }
            }
            WM_CLOSE => push(WindowEvent::CloseRequested),
            WM_DESTROY => {
                s.alive.set(false);
                push(WindowEvent::Destroyed);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
        }
        0
    }
}

impl Win32Window {
    /// Open a child of `parent` (an `HWND`) or a new top-level window.
    pub fn open(parent: Parent, width: u32, height: u32, title: &str) -> Result<Self, WindowError> {
        let instance = module_instance();
        let class = register_class(instance);
        let state =
            Box::new(State { events: RefCell::new(Vec::new()), timer: RefCell::new(None), tracking: Cell::new(false), high_surrogate: Cell::new(0), alive: Cell::new(true) });
        let title_w = wide(title);
        let (w, h) = (width.max(1) as i32, height.max(1) as i32);
        // SAFETY: valid class, instance and state pointer (the box outlives the window).
        let hwnd = unsafe {
            match parent {
                Parent::Win32(p) => CreateWindowExW(
                    0,
                    class.as_ptr(),
                    title_w.as_ptr(),
                    WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                    0,
                    0,
                    w,
                    h,
                    p as HWND,
                    std::ptr::null_mut(),
                    instance,
                    &*state as *const State as *const core::ffi::c_void,
                ),
                Parent::TopLevel => {
                    let style = (WS_OVERLAPPEDWINDOW & !(WS_THICKFRAME | WS_MAXIMIZEBOX)) | WS_VISIBLE | WS_CLIPCHILDREN;
                    let mut r = RECT { left: 0, top: 0, right: w, bottom: h };
                    AdjustWindowRectEx(&mut r, style, 0, 0);
                    CreateWindowExW(
                        0,
                        class.as_ptr(),
                        title_w.as_ptr(),
                        style,
                        CW_USEDEFAULT,
                        CW_USEDEFAULT,
                        r.right - r.left,
                        r.bottom - r.top,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        instance,
                        &*state as *const State as *const core::ffi::c_void,
                    )
                }
                Parent::X11(_) => return Err(WindowError::new("an X11 parent cannot hold a Win32 window")),
            }
        };
        if hwnd.is_null() {
            return Err(err("CreateWindowExW"));
        }
        Ok(Self { hwnd, state, width: width.max(1), height: height.max(1) })
    }

    /// The `HWND` (a parent for a hosted plugin's editor).
    pub fn raw_id(&self) -> u64 {
        self.hwnd as usize as u64
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Run `tick` every `interval_ms` on the UI thread, from the host's message loop.
    pub fn set_timer(&mut self, interval_ms: u32, tick: Box<dyn FnMut()>) {
        *self.state.timer.borrow_mut() = Some(tick);
        // SAFETY: our window.
        unsafe { SetTimer(self.hwnd, 1, interval_ms, None) };
    }

    pub fn poll(&mut self) -> Vec<WindowEvent> {
        let events = std::mem::take(&mut *self.state.events.borrow_mut());
        for e in &events {
            if let WindowEvent::Resized(w, h) = e {
                self.width = (*w).max(1);
                self.height = (*h).max(1);
            }
        }
        events
    }

    pub fn present(&mut self, pixels: &[u32], width: u32, height: u32) -> Result<(), WindowError> {
        if !self.state.alive.get() || width == 0 || height == 0 || pixels.len() < (width * height) as usize {
            return Ok(());
        }
        // SAFETY: our window's DC; the bitmap header describes `pixels` exactly (top-down).
        unsafe {
            let mut bmi: BITMAPINFO = std::mem::zeroed();
            bmi.bmiHeader = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            };
            let dc = GetDC(self.hwnd);
            if dc.is_null() {
                return Err(err("GetDC"));
            }
            SetDIBitsToDevice(dc, 0, 0, width, height, 0, 0, 0, height, pixels.as_ptr() as *const core::ffi::c_void, &bmi, DIB_RGB_COLORS);
            ReleaseDC(self.hwnd, dc);
        }
        Ok(())
    }

    pub fn set_size(&mut self, width: u32, height: u32) -> Result<(), WindowError> {
        let (w, h) = (width.max(1), height.max(1));
        // SAFETY: our window.
        let ok = unsafe { SetWindowPos(self.hwnd, std::ptr::null_mut(), 0, 0, w as i32, h as i32, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE) };
        if ok == 0 {
            return Err(err("SetWindowPos"));
        }
        self.width = w;
        self.height = h;
        Ok(())
    }

    pub fn set_visible(&mut self, visible: bool) -> Result<(), WindowError> {
        // SAFETY: our window.
        unsafe { ShowWindow(self.hwnd, if visible { SW_SHOW } else { SW_HIDE }) };
        Ok(())
    }

    /// One pixel as `0x00RRGGBB`, read from the screen (verification only).
    pub fn read_pixel(&self, x: u32, y: u32) -> Option<u32> {
        // SAFETY: our window's DC.
        unsafe {
            let dc = GetDC(self.hwnd);
            if dc.is_null() {
                return None;
            }
            let c = GetPixel(dc, x as i32, y as i32);
            ReleaseDC(self.hwnd, dc);
            (c != 0xFFFF_FFFF).then_some(((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF))
        }
    }
}

impl Drop for Win32Window {
    fn drop(&mut self) {
        if self.state.alive.get() {
            // SAFETY: our window; WM_DESTROY clears the state pointer before the box goes.
            unsafe {
                KillTimer(self.hwnd, 1);
                DestroyWindow(self.hwnd);
            }
        }
    }
}

/// Dispatch the calling thread's pending messages (tests; hosts run their own loop).
pub fn pump_messages() {
    // SAFETY: standard message loop on the calling thread.
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

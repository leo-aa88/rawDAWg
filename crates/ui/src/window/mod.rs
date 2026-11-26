//! Native windows for plugin editors, without a windowing framework (ADR 0006).
//!
//! - Linux: X11 through x11rb's pure-Rust connection (one connection per window, independent
//!   of the host's). Works under XWayland. Wayland-native embedding is not offered.
//! - Windows: Win32 child or top-level windows of this module's own window class.
//!
//! A window only collects input and presents finished frames; [`crate::editor`] turns events
//! into egui input and paints.

#[cfg(target_os = "windows")]
mod win32;
#[cfg(target_os = "linux")]
mod x11;

#[cfg(target_os = "windows")]
pub use win32::{Win32Window as NativeWindow, pump_messages};
#[cfg(target_os = "linux")]
pub use x11::X11Window as NativeWindow;

/// Where an editor window goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Parent {
    /// An X11 window id (CLAP `x11` API, VST3 `X11EmbedWindowID`).
    X11(u32),
    /// A Win32 `HWND` (CLAP `win32` API, VST3 `HWND`).
    Win32(isize),
    /// A new top-level window (rawDAWg hosting a third-party editor).
    TopLevel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// Keys the editors understand; printable input arrives as [`WindowEvent::Text`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Backspace,
    Delete,
    Enter,
    Escape,
    Tab,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Other,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum WindowEvent {
    /// Pointer position in physical pixels relative to the window.
    PointerMoved(f32, f32),
    PointerLeft,
    Button {
        button: MouseButton,
        pressed: bool,
        modifiers: Modifiers,
    },
    /// Wheel steps (positive: up / right).
    Scroll(f32, f32),
    Key {
        key: Key,
        pressed: bool,
        modifiers: Modifiers,
    },
    Text(String),
    /// New size in physical pixels.
    Resized(u32, u32),
    /// The window content must be drawn again.
    Redraw,
    FocusChanged(bool),
    /// The user asked to close a top-level window.
    CloseRequested,
    /// The window is gone (for example its parent was destroyed by the host).
    Destroyed,
}

/// Errors creating or using a native window.
#[derive(Debug)]
pub struct WindowError(pub String);

impl std::fmt::Display for WindowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for WindowError {}

impl WindowError {
    pub(crate) fn new(e: impl std::fmt::Display) -> Self {
        Self(e.to_string())
    }
}

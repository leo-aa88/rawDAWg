//! X11 windows through x11rb's pure-Rust connection (no libX11/libxcb needed).
//!
//! Each window owns its own connection to the X server, so its events never mix with the
//! host's. Frames are uploaded with `PutImage` in `ZPixmap` format at the screen's root depth
//! (24-bit TrueColor, 32 bits per pixel); a window is created with the root visual and its own
//! colormap so a host parent with a different visual (for example 32-bit ARGB) still works.

use super::{Key, Modifiers, MouseButton, Parent, WindowError, WindowEvent};
use std::os::fd::{AsRawFd, RawFd};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ButtonPressEvent, ClientMessageEvent, ColormapAlloc, ConfigureWindowAux, ConnectionExt as _, CreateGCAux, CreateWindowAux, EventMask, ImageFormat, ImageOrder,
    InputFocus, KeyButMask, PropMode, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

pub struct X11Window {
    conn: RustConnection,
    window: u32,
    gc: u32,
    colormap: u32,
    depth: u8,
    msb_first: bool,
    width: u32,
    height: u32,
    wm_delete: u32,
    min_keycode: u8,
    keysyms_per_code: u8,
    keysyms: Vec<u32>,
    events: Vec<WindowEvent>,
    destroyed: bool,
}

fn err(e: impl std::fmt::Display) -> WindowError {
    WindowError::new(format!("X11: {e}"))
}

impl X11Window {
    /// Open a window: a child of `parent` (an X11 window id) or a new top-level window.
    pub fn open(parent: Parent, width: u32, height: u32, title: &str) -> Result<Self, WindowError> {
        let parent_id = match parent {
            Parent::X11(id) => Some(id),
            Parent::TopLevel => None,
            Parent::Win32(_) => return Err(WindowError::new("a Win32 parent cannot hold an X11 window")),
        };
        let (conn, screen_num) = x11rb::connect(None).map_err(err)?;
        let setup = conn.setup();
        let screen = setup.roots.get(screen_num).ok_or_else(|| err("no screen"))?.clone();
        let depth = screen.root_depth;
        let bpp = setup.pixmap_formats.iter().find(|f| f.depth == depth).map(|f| f.bits_per_pixel).unwrap_or(0);
        let visual =
            screen.allowed_depths.iter().flat_map(|d| d.visuals.iter()).find(|v| v.visual_id == screen.root_visual).cloned().ok_or_else(|| err("root visual not found"))?;
        if bpp != 32 || visual.red_mask != 0xFF_0000 || visual.green_mask != 0xFF00 || visual.blue_mask != 0xFF {
            return Err(err(format!("unsupported screen format (depth {depth}, {bpp} bits per pixel)")));
        }
        let msb_first = setup.image_byte_order == ImageOrder::MSB_FIRST;
        let (min_keycode, max_keycode) = (setup.min_keycode, setup.max_keycode);
        let map = conn.get_keyboard_mapping(min_keycode, max_keycode - min_keycode + 1).map_err(err)?.reply().map_err(err)?;
        let colormap = conn.generate_id().map_err(err)?;
        conn.create_colormap(ColormapAlloc::NONE, colormap, screen.root, screen.root_visual).map_err(err)?;
        let window = conn.generate_id().map_err(err)?;
        let mask = EventMask::EXPOSURE
            | EventMask::STRUCTURE_NOTIFY
            | EventMask::BUTTON_PRESS
            | EventMask::BUTTON_RELEASE
            | EventMask::POINTER_MOTION
            | EventMask::ENTER_WINDOW
            | EventMask::LEAVE_WINDOW
            | EventMask::KEY_PRESS
            | EventMask::KEY_RELEASE
            | EventMask::FOCUS_CHANGE;
        let aux = CreateWindowAux::new().background_pixel(0x0017_191E).border_pixel(0).colormap(colormap).event_mask(mask);
        conn.create_window(
            depth,
            window,
            parent_id.unwrap_or(screen.root),
            0,
            0,
            width.max(1) as u16,
            height.max(1) as u16,
            0,
            WindowClass::INPUT_OUTPUT,
            screen.root_visual,
            &aux,
        )
        .map_err(err)?
        .check()
        .map_err(err)?;
        let gc = conn.generate_id().map_err(err)?;
        conn.create_gc(gc, window, &CreateGCAux::new().graphics_exposures(0)).map_err(err)?;
        let wm_protocols = conn.intern_atom(false, b"WM_PROTOCOLS").map_err(err)?.reply().map_err(err)?.atom;
        let wm_delete = conn.intern_atom(false, b"WM_DELETE_WINDOW").map_err(err)?.reply().map_err(err)?.atom;
        if parent_id.is_none() {
            let utf8 = conn.intern_atom(false, b"UTF8_STRING").map_err(err)?.reply().map_err(err)?.atom;
            let net_name = conn.intern_atom(false, b"_NET_WM_NAME").map_err(err)?.reply().map_err(err)?.atom;
            conn.change_property8(PropMode::REPLACE, window, AtomEnum::WM_NAME, AtomEnum::STRING, title.as_bytes()).map_err(err)?;
            conn.change_property8(PropMode::REPLACE, window, net_name, utf8, title.as_bytes()).map_err(err)?;
            conn.change_property8(PropMode::REPLACE, window, AtomEnum::WM_CLASS, AtomEnum::STRING, b"rawdawg\0rawDAWg\0").map_err(err)?;
            conn.change_property32(PropMode::REPLACE, window, wm_protocols, AtomEnum::ATOM, &[wm_delete]).map_err(err)?;
        }
        conn.map_window(window).map_err(err)?;
        conn.flush().map_err(err)?;
        Ok(Self {
            conn,
            window,
            gc,
            colormap,
            depth,
            msb_first,
            width: width.max(1),
            height: height.max(1),
            wm_delete,
            min_keycode,
            keysyms_per_code: map.keysyms_per_keycode,
            keysyms: map.keysyms,
            events: Vec::new(),
            destroyed: false,
        })
    }

    /// The X11 window id (a parent for a hosted plugin's editor).
    pub fn raw_id(&self) -> u64 {
        u64::from(self.window)
    }

    /// The connection's socket: readable when events are waiting (CLAP posix-fd support).
    pub fn fd(&self) -> RawFd {
        self.conn.stream().as_raw_fd()
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn modifiers(state: KeyButMask) -> Modifiers {
        Modifiers { shift: state.contains(KeyButMask::SHIFT), ctrl: state.contains(KeyButMask::CONTROL), alt: state.contains(KeyButMask::MOD1) }
    }

    fn keysym(&self, keycode: u8, shift: bool) -> u32 {
        let per = usize::from(self.keysyms_per_code.max(1));
        let base = usize::from(keycode.saturating_sub(self.min_keycode)) * per;
        let plain = self.keysyms.get(base).copied().unwrap_or(0);
        let shifted = self.keysyms.get(base + 1).copied().filter(|&s| s != 0).unwrap_or(plain);
        if shift { shifted } else { plain }
    }

    fn button(&mut self, e: &ButtonPressEvent, pressed: bool) {
        let modifiers = Self::modifiers(e.state);
        let ev = match e.detail {
            1 => WindowEvent::Button { button: MouseButton::Left, pressed, modifiers },
            2 => WindowEvent::Button { button: MouseButton::Middle, pressed, modifiers },
            3 => WindowEvent::Button { button: MouseButton::Right, pressed, modifiers },
            4 if pressed => WindowEvent::Scroll(0.0, 1.0),
            5 if pressed => WindowEvent::Scroll(0.0, -1.0),
            6 if pressed => WindowEvent::Scroll(1.0, 0.0),
            7 if pressed => WindowEvent::Scroll(-1.0, 0.0),
            _ => return,
        };
        if pressed {
            // clicking an embedded editor gives it the keyboard (for typing values)
            let _ = self.conn.set_input_focus(InputFocus::PARENT, self.window, x11rb::CURRENT_TIME);
        }
        self.events.push(WindowEvent::PointerMoved(f32::from(e.event_x), f32::from(e.event_y)));
        self.events.push(ev);
    }

    /// Collect pending events without blocking.
    pub fn poll(&mut self) -> Vec<WindowEvent> {
        loop {
            let ev = match self.conn.poll_for_event() {
                Ok(Some(ev)) => ev,
                Ok(None) => break,
                Err(_) => {
                    // the connection is gone (X server closed): the window is too
                    if !self.destroyed {
                        self.destroyed = true;
                        self.events.push(WindowEvent::Destroyed);
                    }
                    break;
                }
            };
            match ev {
                Event::Expose(e) if e.count == 0 => self.events.push(WindowEvent::Redraw),
                Event::ConfigureNotify(e) if e.window == self.window => {
                    let (w, h) = (u32::from(e.width), u32::from(e.height));
                    if (w, h) != (self.width, self.height) {
                        self.width = w.max(1);
                        self.height = h.max(1);
                        self.events.push(WindowEvent::Resized(self.width, self.height));
                    }
                }
                Event::MotionNotify(e) => self.events.push(WindowEvent::PointerMoved(f32::from(e.event_x), f32::from(e.event_y))),
                Event::LeaveNotify(_) => self.events.push(WindowEvent::PointerLeft),
                Event::ButtonPress(e) => self.button(&e, true),
                Event::ButtonRelease(e) => self.button(&e, false),
                Event::KeyPress(e) | Event::KeyRelease(e) => {
                    let pressed = matches!(ev, Event::KeyPress(_));
                    let modifiers = Self::modifiers(e.state);
                    let sym = self.keysym(e.detail, modifiers.shift);
                    let key = match sym {
                        0xFF08 => Key::Backspace,
                        0xFFFF => Key::Delete,
                        0xFF0D | 0xFF8D => Key::Enter,
                        0xFF1B => Key::Escape,
                        0xFF09 => Key::Tab,
                        0xFF51 => Key::Left,
                        0xFF52 => Key::Up,
                        0xFF53 => Key::Right,
                        0xFF54 => Key::Down,
                        0xFF50 => Key::Home,
                        0xFF57 => Key::End,
                        _ => Key::Other,
                    };
                    if key != Key::Other {
                        self.events.push(WindowEvent::Key { key, pressed, modifiers });
                    } else if pressed
                        && !modifiers.ctrl
                        && let Some(c) = keysym_char(sym)
                    {
                        self.events.push(WindowEvent::Text(c.to_string()));
                    }
                }
                Event::FocusIn(_) => self.events.push(WindowEvent::FocusChanged(true)),
                Event::FocusOut(_) => self.events.push(WindowEvent::FocusChanged(false)),
                Event::ClientMessage(ClientMessageEvent { data, .. }) if data.as_data32()[0] == self.wm_delete => self.events.push(WindowEvent::CloseRequested),
                Event::DestroyNotify(e) if e.window == self.window => {
                    self.destroyed = true;
                    self.events.push(WindowEvent::Destroyed);
                }
                _ => {}
            }
        }
        std::mem::take(&mut self.events)
    }

    /// Show a finished frame (`0x00RRGGBB` pixels, `width` × `height`).
    pub fn present(&mut self, pixels: &[u32], width: u32, height: u32) -> Result<(), WindowError> {
        if self.destroyed || width == 0 || height == 0 || pixels.len() < (width * height) as usize {
            return Ok(());
        }
        let row_bytes = width as usize * 4;
        let max_bytes = self.conn.maximum_request_bytes().saturating_sub(64).max(row_bytes);
        let rows_per_request = (max_bytes / row_bytes).max(1);
        let mut bytes = Vec::with_capacity(rows_per_request * row_bytes);
        let mut y = 0usize;
        while y < height as usize {
            let rows = rows_per_request.min(height as usize - y);
            bytes.clear();
            for &p in &pixels[y * width as usize..(y + rows) * width as usize] {
                bytes.extend_from_slice(&if self.msb_first { p.to_be_bytes() } else { p.to_le_bytes() });
            }
            self.conn.put_image(ImageFormat::Z_PIXMAP, self.window, self.gc, width as u16, rows as u16, 0, y as i16, 0, self.depth, &bytes).map_err(err)?;
            y += rows;
        }
        self.conn.flush().map_err(err)
    }

    pub fn set_size(&mut self, width: u32, height: u32) -> Result<(), WindowError> {
        let (w, h) = (width.max(1), height.max(1));
        self.conn.configure_window(self.window, &ConfigureWindowAux::new().width(w).height(h)).map_err(err)?;
        self.conn.flush().map_err(err)?;
        self.width = w;
        self.height = h;
        Ok(())
    }

    pub fn set_visible(&mut self, visible: bool) -> Result<(), WindowError> {
        if visible {
            self.conn.map_window(self.window).map_err(err)?;
        } else {
            self.conn.unmap_window(self.window).map_err(err)?;
        }
        self.conn.flush().map_err(err)
    }

    /// Read the window's pixels back from the server (verification only).
    pub fn read_back(&self) -> Result<Vec<u32>, WindowError> {
        let reply = self.conn.get_image(ImageFormat::Z_PIXMAP, self.window, 0, 0, self.width as u16, self.height as u16, !0).map_err(err)?.reply().map_err(err)?;
        Ok(reply.data.as_chunks::<4>().0.iter().map(|b| if self.msb_first { u32::from_be_bytes(*b) } else { u32::from_le_bytes(*b) } & 0xFF_FFFF).collect())
    }

    /// Whether the window and all its ancestors are mapped (only then can it be read back).
    pub fn is_viewable(&self) -> bool {
        self.conn.get_window_attributes(self.window).ok().and_then(|c| c.reply().ok()).is_some_and(|a| a.map_state == x11rb::protocol::xproto::MapState::VIEWABLE)
    }

    /// Wait until the server has processed everything sent so far (tests).
    pub fn sync(&self) -> Result<(), WindowError> {
        self.conn.get_input_focus().map_err(err)?.reply().map_err(err)?;
        Ok(())
    }
}

/// Latin-1 and the common Unicode keysym range to a character.
fn keysym_char(sym: u32) -> Option<char> {
    match sym {
        0x20..=0x7E | 0xA0..=0xFF => char::from_u32(sym),
        0x0100_0000..=0x0110_FFFF => char::from_u32(sym - 0x0100_0000),
        _ => None,
    }
}

impl Drop for X11Window {
    fn drop(&mut self) {
        // after a host destroyed the parent these fail harmlessly (errors arrive as events
        // nobody reads any more)
        if !self.destroyed {
            let _ = self.conn.destroy_window(self.window);
        }
        let _ = self.conn.free_gc(self.gc);
        let _ = self.conn.free_colormap(self.colormap);
        let _ = self.conn.flush();
    }
}

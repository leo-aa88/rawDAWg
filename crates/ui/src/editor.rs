//! A plugin editor: native window events → egui → software painter → window (ADR 0006).
//!
//! Everything runs on the thread that calls [`Editor::tick`], which is the host's main / UI
//! thread: CLAP `timer-support` or the VST3 Linux run loop on Linux, a Win32 timer dispatched
//! by the host's message loop on Windows. Ticks are cheap when nothing changed: egui runs only
//! after input, a resize, an expose, or when it asked for a repaint.

use crate::raster::SoftwarePainter;
use crate::window::{Key, Modifiers, MouseButton, NativeWindow, Parent, WindowError, WindowEvent};
use egui::{Pos2, Rect, pos2, vec2};
use std::time::{Duration, Instant};

/// What an editor draws.
pub trait EditorUi {
    fn ui(&mut self, ui: &mut egui::Ui);
}

/// Result of one [`Editor::tick`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tick {
    /// A frame was drawn and presented.
    pub painted: bool,
    /// The window was closed or destroyed (by the user or by its parent going away).
    pub closed: bool,
}

pub struct Editor {
    ctx: egui::Context,
    painter: SoftwarePainter,
    window: NativeWindow,
    ui: Box<dyn EditorUi>,
    scale: f32,
    start: Instant,
    events: Vec<egui::Event>,
    pointer: Option<Pos2>,
    modifiers: Modifiers,
    focused: bool,
    repaint_at: Option<Instant>,
    dirty: bool,
    closed: bool,
}

fn to_egui_modifiers(m: Modifiers) -> egui::Modifiers {
    egui::Modifiers { alt: m.alt, ctrl: m.ctrl, shift: m.shift, mac_cmd: false, command: m.ctrl }
}

impl Editor {
    /// Open the editor window under `parent` with a logical size (points) and a content scale.
    pub fn open(parent: Parent, logical: (u32, u32), scale: f32, title: &str, ui: Box<dyn EditorUi>) -> Result<Self, WindowError> {
        let scale = if scale.is_finite() && scale > 0.25 { scale.min(4.0) } else { 1.0 };
        let window = NativeWindow::open(parent, (logical.0 as f32 * scale).round() as u32, (logical.1 as f32 * scale).round() as u32, title)?;
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx);
        Ok(Self {
            ctx,
            painter: SoftwarePainter::new(),
            window,
            ui,
            scale,
            start: Instant::now(),
            events: Vec::new(),
            pointer: None,
            modifiers: Modifiers::default(),
            focused: true,
            repaint_at: Some(Instant::now()),
            dirty: true,
            closed: false,
        })
    }

    pub fn window(&self) -> &NativeWindow {
        &self.window
    }

    pub fn window_mut(&mut self) -> &mut NativeWindow {
        &mut self.window
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Size in points.
    pub fn logical_size(&self) -> (u32, u32) {
        let (w, h) = self.window.size();
        ((w as f32 / self.scale).round() as u32, (h as f32 / self.scale).round() as u32)
    }

    pub fn set_logical_size(&mut self, w: u32, h: u32) -> Result<(), WindowError> {
        self.window.set_size((w as f32 * self.scale).round() as u32, (h as f32 * self.scale).round() as u32)?;
        self.dirty = true;
        Ok(())
    }

    pub fn set_scale(&mut self, scale: f32) {
        if scale.is_finite() && scale > 0.25 {
            self.scale = scale.min(4.0);
            self.dirty = true;
        }
    }

    pub fn set_visible(&mut self, visible: bool) -> Result<(), WindowError> {
        self.window.set_visible(visible)?;
        self.dirty |= visible;
        Ok(())
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    fn translate(&mut self, e: WindowEvent) {
        let s = self.scale;
        match e {
            WindowEvent::PointerMoved(x, y) => {
                let p = pos2(x / s, y / s);
                self.pointer = Some(p);
                self.events.push(egui::Event::PointerMoved(p));
            }
            WindowEvent::PointerLeft => {
                self.pointer = None;
                self.events.push(egui::Event::PointerGone);
            }
            WindowEvent::Button { button, pressed, modifiers } => {
                self.modifiers = modifiers;
                let Some(pos) = self.pointer else { return };
                let button = match button {
                    MouseButton::Left => egui::PointerButton::Primary,
                    MouseButton::Right => egui::PointerButton::Secondary,
                    MouseButton::Middle => egui::PointerButton::Middle,
                };
                self.events.push(egui::Event::PointerButton { pos, button, pressed, modifiers: to_egui_modifiers(modifiers) });
            }
            WindowEvent::Scroll(dx, dy) => self.events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: vec2(dx, dy),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
                modifiers: to_egui_modifiers(self.modifiers),
            }),
            WindowEvent::Key { key, pressed, modifiers } => {
                self.modifiers = modifiers;
                let key = match key {
                    Key::Backspace => egui::Key::Backspace,
                    Key::Delete => egui::Key::Delete,
                    Key::Enter => egui::Key::Enter,
                    Key::Escape => egui::Key::Escape,
                    Key::Tab => egui::Key::Tab,
                    Key::Left => egui::Key::ArrowLeft,
                    Key::Right => egui::Key::ArrowRight,
                    Key::Up => egui::Key::ArrowUp,
                    Key::Down => egui::Key::ArrowDown,
                    Key::Home => egui::Key::Home,
                    Key::End => egui::Key::End,
                    Key::Other => return,
                };
                self.events.push(egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: to_egui_modifiers(modifiers) });
            }
            WindowEvent::Text(t) => self.events.push(egui::Event::Text(t)),
            WindowEvent::Resized(..) | WindowEvent::Redraw => self.dirty = true,
            WindowEvent::FocusChanged(f) => {
                self.focused = f;
                self.events.push(egui::Event::WindowFocused(f));
            }
            WindowEvent::CloseRequested | WindowEvent::Destroyed => self.closed = true,
        }
    }

    /// Handle pending input and draw if anything changed. Call at the host's timer rate.
    pub fn tick(&mut self) -> Tick {
        for e in self.window.poll() {
            self.translate(e);
        }
        if self.closed {
            return Tick { painted: false, closed: true };
        }
        let due = self.repaint_at.is_some_and(|t| Instant::now() >= t);
        if self.events.is_empty() && !self.dirty && !due {
            return Tick { painted: false, closed: false };
        }
        self.frame();
        Tick { painted: true, closed: false }
    }

    fn frame(&mut self) {
        let (w, h) = self.window.size();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(w as f32 / self.scale, h as f32 / self.scale))),
            time: Some(self.start.elapsed().as_secs_f64()),
            focused: self.focused,
            max_texture_side: Some(4096),
            events: std::mem::take(&mut self.events),
            ..Default::default()
        };
        self.ctx.set_pixels_per_point(self.scale);
        let ui = &mut self.ui;
        let out = self.ctx.run_ui(input, |root| ui.ui(root));
        self.dirty = false;
        let delay = out.viewport_output.get(&egui::ViewportId::ROOT).map_or(Duration::MAX, |v| v.repaint_delay);
        self.repaint_at = (delay < Duration::from_secs(3600)).then(|| Instant::now() + delay);
        let prims = self.ctx.tessellate(out.shapes, out.pixels_per_point);
        self.painter.set_textures(&out.textures_delta);
        self.painter.paint(w as usize, h as usize, crate::theme::BACKGROUND, &prims, out.pixels_per_point);
        self.painter.finish_textures(out.textures_delta);
        let _ = self.window.present(self.painter.pixels(), w, h);
    }

    /// The last frame drawn (verification).
    pub fn painter(&self) -> &SoftwarePainter {
        &self.painter
    }
}

/// Draw one frame of `ui` without any window (tests and headless evidence).
pub fn render_offscreen(ui: &mut dyn EditorUi, logical: (u32, u32), scale: f32, events: Vec<egui::Event>) -> SoftwarePainter {
    let ctx = egui::Context::default();
    crate::theme::apply(&ctx);
    ctx.set_pixels_per_point(scale);
    let mut painter = SoftwarePainter::new();
    // two passes: egui lays out on the first frame and settles sizes on the second
    for events in [Vec::new(), events] {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(logical.0 as f32, logical.1 as f32))),
            max_texture_side: Some(4096),
            events,
            ..Default::default()
        };
        let out = ctx.run_ui(input, |root| ui.ui(root));
        let prims = ctx.tessellate(out.shapes, out.pixels_per_point);
        painter.set_textures(&out.textures_delta);
        painter.paint((logical.0 as f32 * scale).round() as usize, (logical.1 as f32 * scale).round() as usize, crate::theme::BACKGROUND, &prims, out.pixels_per_point);
        painter.finish_textures(out.textures_delta);
    }
    painter
}

# ADR 0006 — Plugin editors: software-rendered egui in native child windows

Status: accepted · Date: 2026-11-26

## Context

PLUGIN_API.md asks for real editors in rawDAWg's exported CLAP and VST3 plugins: native child
window embedding, keyboard and mouse, resize and DPI, host parent destruction, show / hide /
reopen, X11 (also under XWayland) on Linux, Win32 on Windows. The editor should reuse the app's
widgets and must not be a blank window advertised as working. The host side must embed
third-party editors with main-thread callbacks, timers and resizing.

The app draws with egui through eframe, winit and wgpu. None of that can be reused as is inside
a plugin:

- winit allows one event loop per process. A plugin loaded into a host that already runs one,
  including rawDAWg hosting its own plugins, cannot create another.
- Creating a GPU device and surface inside arbitrary hosts is fragile: drivers, GPU-less
  machines and remote X sessions all behave differently.
- The usual Rust plugin windowing crate (baseview) is distributed only as a git dependency,
  which the dependency policy (crates.io only, pinned) rules out.

## Decision

A new crate, `rawdawg-ui`, shared by the app and the plugins:

- **Software painter** (`raster.rs`): draws egui's tessellated triangle meshes into a `0x00RRGGBB`
  framebuffer with premultiplied-alpha blending in sRGB space, texture sampling (nearest or
  bilinear) and a top-left fill rule, so shared triangle edges are not blended twice. No GPU.
- **Native windows** (`window/`): X11 through x11rb's pure-Rust connection (no libX11/libxcb),
  one connection per window, frames uploaded with `PutImage` at the root visual with an own
  colormap so a 32-bit ARGB host parent works too. Win32 child and top-level windows with a
  window class per loaded module, frames shown with `SetDIBitsToDevice`.
- **Editor** (`editor.rs`): window events become egui input, egui runs only after input, resize,
  expose or a requested repaint, and the frame is painted and presented. It is ticked on the
  host's UI thread: CLAP `timer-support` or the VST3 Linux `IRunLoop` on Linux, and a Win32
  timer dispatched by the host's message loop on Windows. A CLAP host on Linux without
  `timer-support` is told the GUI is unsupported, so no frozen editor is advertised.
- **Shared widgets** (`params.rs`, `theme.rs`): the dark theme and the parameter controls
  (switch, named choices, step slider, slider) are used both by the app's generic panels and by
  the plugin editor (`plugin_editor.rs`). Edits go through a format-neutral `ParamPort` with
  begin / edit / end gestures, which hosts use to record automation.

## Alternatives considered

- **wgpu inside the plugin** with egui-wgpu: needs a raw display pointer (Xlib or libxcb) and a
  GPU device in every host process; GPU-less and remote sessions would show nothing.
- **OpenGL through a context library**: the same driver exposure, and on Linux a
  libX11/libxcb dependency.
- **baseview / egui-baseview**: proven, but git-only dependencies.
- **Floating plugin-owned windows only**: VST3 has no floating mode, and CLAP hosts prefer
  embedded editors.

## Consequences

- An editor costs CPU time proportional to its pixel count when it repaints; at 640 × 480 a
  full frame is a few milliseconds, and idle editors do not repaint.
- Text and shapes look like the app's because the same egui tessellation and fonts are used;
  colours match to within rounding (egui's GPU backends blend in linear space for some steps).
- Wayland-native embedding is not offered (PLUGIN_API.md allows X11/XWayland).
- Verification works without a display for drawing (`render_offscreen`) and needs a display
  server only for the window tests (`crates/ui/tests/editor_window.rs`, check GUI-02), which
  run under WSLg / Xorg / Xvfb on Linux and the desktop session on Windows.

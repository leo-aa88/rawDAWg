# ADR 0001 — Toolchain, dependency pins and license findings (M0)

Status: accepted · Date: 2026-11-24

## Decision

- Rust **1.99.0** stable pinned in `rust-toolchain.toml` (rustfmt + clippy components), edition **2024**, workspace resolver 3. Windows target `x86_64-pc-windows-msvc`, Linux `x86_64-unknown-linux-gnu`.
- Exact dependency versions pinned through `Cargo.lock` (committed). No git dependencies. Workspace-level versions in `Cargo.toml`.

| Crate | Version | License | Role | Notes |
|---|---|---|---|---|
| cpal | 0.18.2 | Apache-2.0 | audio I/O | `DeviceTrait::description()/id()` API; optional features `jack`, `pipewire`, `asio` exposed as app features |
| eframe / egui / egui-wgpu | 0.37.0 | MIT OR Apache-2.0 | GUI | wgpu 30.0.2 backend; `App::ui` + `logic` API; `Panel::{top,bottom,left}` |
| wgpu | 30.0.2 | MIT OR Apache-2.0 | GPU | pulled by eframe |
| rtrb | 0.4.1 | MIT OR Apache-2.0 | SPSC queues | wrapped by `rawdawg-rt` |
| rustfft / realfft | 6.4.1 / 3.5.1 | MIT OR Apache-2.0 / MIT | FFT infrastructure | wavetable build, analysis |
| rubato | 5.1.0 | MIT OR Apache-2.0 | resampling | M5 |
| hound | 3.5.1 | Apache-2.0 | WAV read/write | export + fixtures |
| symphonia | 0.6.1 | **MPL-2.0** | decode WAV/FLAC/MP3 | file-level weak copyleft; see licensing note |
| flacenc | 0.5.1 | Apache-2.0 | FLAC encoder (M5) | pure Rust, verified encoder (decoder support ≠ encoder) |
| midir / midly | 0.11.1 / 0.5.3 | MIT / Unlicense | MIDI I/O, SMF | M1/M3 |
| clack-plugin / clack-host / clack-extensions / clap-sys | 0.2.0 / 0.5.0 | MIT OR Apache-2.0 | CLAP ABI | clack 0.2.0 is on crates.io (no git dep needed) |
| vst3 (coupler-rs) | 0.3.0 | MIT OR Apache-2.0 | VST3 COM bindings | bindings generated from the VST3 SDK headers; Steinberg SDK 3.8 terms are MIT; **trademark/bundle obligations reviewed at M7** |
| serde / serde_json / uuid / sha2 | current | MIT OR Apache-2.0 | persistence | |
| rand_chacha / rand_core | 0.10 | MIT OR Apache-2.0 | deterministic RNG (ChaCha8) | pinned algorithm id `chacha8-v1` |
| clap | 4.6.10 | MIT OR Apache-2.0 | CLI | |
| proptest | 1.12.0 | MIT OR Apache-2.0 | property tests | dev |
| libloading | 0.9.1 | ISC | module loading (tests, host) | |
| audio_thread_priority (via cpal `realtime` feature, not enabled) | — | MPL-2.0 | — | not used at M0 |

`assert_no_alloc` (BSD-1-Clause) was considered and **not** used; `rawdawg-rt` ships its own instrumented global allocator with a thread-local RT flag (T-RT), avoiding the extra license and giving counting semantics.

## Licensing findings

- The repository already contains an **AGPL-3.0** `LICENSE` placed by the owner. The workspace manifests declare `license = "AGPL-3.0-only"` to match the file. This is recorded, not decided here: the final application license remains an owner decision (CLAUDE.md). All runtime dependencies are permissive or MPL-2.0 (file-level), which is compatible with AGPL-3.0 distribution and would also be compatible with a permissive application license.
- `cargo deny check licenses` passes with the allow-list in `deny.toml`; MPL-2.0 is explicitly listed so any new copyleft dependency fails the audit until reviewed.
- VST3: the `vst3` crate is MIT/Apache; Steinberg's SDK 3.8+ is MIT, but using the VST3 trademark/compatibility logo and bundle naming follows Steinberg's trademark terms — owner review before publication (M7/M8).
- ASIO: **not** enabled. `cpal/asio` requires the Steinberg ASIO SDK on the build machine (restricted terms, user-supplied). An `asio` cargo feature is wired but unverified; the WASAPI build is the Windows baseline (ARCHITECTURE.md §8).
- No JUCE, webview, or external DAW engine appears in the dependency graph (`deny.toml` bans `juce`, `wry`, `tauri`, `webview2-com`, `openssl-sys`).

## Build prerequisites (verified on this machine)

Ubuntu: `libasound2-dev` (ALSA, required), `libjack-jackd2-dev` (optional `jack` feature), `libxkbcommon-dev`, `libwayland-dev` and X11/xcb dev packages for winit, `pkg-config`, a C toolchain. PipeWire feature requires `libpipewire-0.3-dev` ≥ what `pipewire` 0.10 needs (unverified on Ubuntu 22.04 here; CI runs 24.04). Windows: MSVC Build Tools; no extra SDKs for the WASAPI baseline.

## Alternatives considered

- `nih-plug` for plugin export: rejected (framework owns DSP/GUI structure; thin adapters preferred per PLUGIN_API.md).
- `vst3-sys` (RustAudio): older bindings with GPL-3.0 licensing; `vst3` (coupler-rs) chosen for MIT/Apache terms and SDK-generated completeness.
- `claxon` for FLAC: decoder only; `flacenc` chosen for encoding.

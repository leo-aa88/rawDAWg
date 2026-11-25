# rawDAWg — PROGRESS

Live checkpoint for the implementation agent. Facts only; see `docs/verification/requirements.md` for the per-requirement evidence table and `docs/adr/` for decisions.

## Current state

- **Milestone:** M0 (bootstrap and risk reduction) **complete** on Ubuntu; M1 (audio engine and first playable instrument) **in progress** — engine, synth, typing keyboard and headless render are implemented and tested; MIDI input (midir) and the device settings UI are the next M1 tasks.
- **Environment used:** Ubuntu 22.04 (WSL2, WSLg), Rust 1.99.0, 16 cores. Audio output available through WSLg's PulseAudio bridge via ALSA (`Default Audio Device`), which is *not* a real Ubuntu desktop/hardware device. No Windows machine, no display screenshots captured, no MIDI hardware, no independent plugin hosts or validators installed.
- **Build label:** intermediate build; not a release candidate.

## Commands and results (this machine, 2026-11-24)

| Command | Result |
|---|---|
| `cargo fmt --all -- --check` | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 (after lint fixes; see git diff) |
| `cargo test --workspace` | 145 tests, 0 failures (types 23, transport 10, rt 8+3, dsp 39, synthesizer 7+18, project 18, audio-io 4, audio-engine 1+10, plugin-clap 2, plugin-vst3 2) |
| `cargo build --workspace --release --locked` | exit 0 (3 m 08 s); `target/release/rawdawg`, `librawdawg_clap.so`, `librawdawg_vst3.so` |
| `cargo xtask fixtures` | wrote `fixtures/demo.rawdawg` + 4 WAVs |
| `cargo xtask verify --suite all` | HL-01..07 PASS, PL-01..02 PASS, PL-03/04/05 BLOCKED, HW-01 BLOCKED, GUI-01 BLOCKED → `target/verification/verify-all.json` |
| `cargo xtask render --project fixtures/demo.rawdawg --output target/verification/demo.wav` | 144000 frames @ 48 kHz, peak 0.340, ~167× realtime |
| `cargo run -p rawdawg -- --audio-smoke` | opened ALSA default 48 kHz F32 stereo, 134 callbacks / 68608 frames, 1 xrun (heuristic), engine peak 0.154, exit 0 |
| `cargo run -p rawdawg -- --list-audio-devices` | ALSA host, 3 devices (null, default, …), JSON |
| `cargo deny check licenses advisories bans sources` | licenses ok, bans ok, sources ok; advisories ok with 3 documented ignores (unmaintained transitive zbus 5.x crates from eframe/accesskit) |
| `cargo run --release -p xtask -- bench-audio --seconds 30` | 4 synths × 8 voices (32 voices), 48 kHz/256: p50 0.197 ms, p95 0.248 ms, p99 0.339 ms, max 0.659 ms vs 5.333 ms deadline (p99 = 6 %); 25.8× realtime offline. M1 workload subset, this WSL2 machine, not the M8 reference workload → `target/verification/bench-audio.json` |
| `./target/release/rawdawg --audio-smoke` | ALSA default 48 kHz F32, 132 callbacks, 0 xruns, max callback 0.207 ms, peak 0.154 |
| `./target/release/rawdawg --headless-render fixtures/demo.rawdawg --output … --format pcm24` | 144000 frames, peak 0.340, 0 clipped, ~230× realtime |
| `cargo xtask package --format deb` | refuses: scheduled for M8 |

## Implemented (M0 + M1 subset)

- Workspace: `crates/{types,transport,rt,dsp,synthesizer,project,audio-engine,audio-io,plugin-clap,plugin-vst3,app}`, `xtask`, `.cargo/config.toml` alias, pinned toolchain, `Cargo.lock`, `deny.toml`, `about.toml`, CI workflow (Ubuntu 24.04 + Windows 2022, not yet executed).
- Types/units/events/parameter descriptors; tempo/meter maps with exact i128 time; half-open block scheduling verified (T-TIME subset).
- RT primitives: bounded SPSC queues with overflow counters, publish/retire plan handoff that refuses swaps without retirement capacity, atomics for status, instrumented allocator for T-RT.
- DSP: mipmapped wavetable pipeline (ADR 0003), basic oscillators, ADSR, SVF, smoothers, lookahead peak limiter, deterministic ChaCha8 RNG, analysis helpers.
- Synth `native.synth`: layers, editable cycle generator, envelopes, filter, poly/mono/legato, stealing with fade tails, sustain/source isolation, pitch bend, tuning, 54 stable parameter descriptors, JSON state.
- Project model schema v1 + validation + invertible commands with gestures/undo/redo/revision + atomic directory persistence.
- Engine: project → plan compiler (channels, mixer order, solo, schedules), RT render with transport (play/stop/pause/seek/loop/mode), metronome, master protection, status atomics; offline renderer and WAV writer sharing the render path.
- Audio I/O: CPAL enumeration, negotiation (requested vs actual), stream with edge conversion, chunking of oversized callbacks, xrun heuristic, failure state.
- App: CLI (`--headless-render`, `--safe-mode`, `--open`, `--list-audio-devices`, `--audio-smoke`, `--no-audio`, `--device`, `--buffer`), dark-only eframe window with toolbar/browser/rack/status skeleton, typing keyboard driving the engine, undo/redo, audio retry.
- Plugins: CLAP export of the synth (`rawdawg.synth`, "rawDAWg Synth") with notes/audio/params/state, verified by loading the built `.so` as a host; VST3 module with factory + gain component/controller verified through `GetPluginFactory`.

## Decisions (ADRs)

- 0001 toolchain, pins, licenses (repo LICENSE is AGPL-3.0 placed by owner; final license still an owner decision).
- 0002 RT ownership and crate layout deviations (`rt` crate added; `plugin-api` folded into `dsp::processor`; sequencer/mixer inside `audio-engine` for now).
- 0003 mipmapped wavetables replace PolyBLEP (31 dB measured < 35 dB gate).

## Open gates (not passed; do not mark as done)

| Gate | Status | How to close |
|---|---|---|
| Windows build/test (PLAT-01, QUAL-03) | NOT RUN | Run CI workflow on `windows-2022` or build locally with MSVC: `cargo test --workspace --locked`. |
| Ubuntu 24.04 build/test | NOT RUN (22.04 used) | CI job `Ubuntu 24.04`; also verify `pipewire` feature builds there. |
| HW-01 real audio devices (PLAT-02) | BLOCKED | On real hardware: `rawdawg --list-audio-devices`, `rawdawg --audio-smoke --device <id> --buffer 256`; record device, backend, rate, buffer, xruns, duration in `docs/verification/hardware.md`. Repeat for WASAPI (Windows) and ALSA/PipeWire/JACK (Ubuntu). |
| GUI-01 manual GUI (PLAT-03, QUAL-04) | BLOCKED | Launch `rawdawg` on a display at 1024×720 / 1440×900 / 1920×1080, 100/150/200 %; confirm dark-only surfaces, typing keyboard plays within 10 s, screenshots to `docs/verification/screenshots/`. |
| PL-03 clap-validator | BLOCKED | Install clap-validator; `cargo xtask verify --suite plugins` runs it automatically when on PATH. |
| PL-04 VST3 validator | BLOCKED (M7) | Needs SDK validator build and `.vst3` bundle layout. |
| PL-05 independent hosts | BLOCKED (M7) | Load exported CLAP/VST3 in an independent host per OS; document versions/paths. |
| ASIO build (PLAT-02) | NOT RUN | Requires user-supplied Steinberg ASIO SDK and license review; `--features asio` is wired but unverified. |
| Packaging (PLAT-04) | NOT RUN | M8. |
| Reference-workload benchmark (QUAL-04, TESTING §4) | PARTIAL | M1 subset measured above; full workload (samplers, EQ/comp per track, send reverbs, limiter, ≥5 min) at M8 on reference hardware. |

## Known issues / risks

- `TransportView` meter fields are constant 4/4 until the meter map is consulted per block (M3).
- Pattern-mode scheduling uses the tempo at tick 0 (per spec) but the UI shows only the first tempo point.
- Channel pan is applied as a stereo balance derived from equal-power gains; stereo width option deferred (M4).
- The xrun detector is a callback-gap heuristic; backend-reported underruns are not surfaced by cpal 0.18 uniformly.
- WSLg audio device is a bridge; latency/xrun numbers from this machine are not hardware evidence.

## Next action

M1 continuation, in order:
1. `rawdawg-midi` crate: midir input adapter with monotonic timestamps → engine `Live` events (source `MidiDevice(n)`), device list/enable in the UI, hotplug/reconnect, release-on-disconnect; T-MIDI mock-timestamp tests.
2. Settings > Audio in the GUI: device/rate/buffer selection with requested vs actual, nearest-supported suggestions, no silent preference changes.
3. On-screen keyboard and channel-rack note audition; parameter edits as `SetInstanceState` gestures with live preview through `Param` events.
4. Release build, `cargo xtask bench-audio`, then run the M1 gate list (T-RT/T-TIME/T-OSC/T-VOICE/T-IO/T-MIDI subsets) and update the evidence table.
Then M2 (sculpting canvas with live table handoff, spectral paint, mutation/evolution/morph, remaining generators, sampler/SF2).

# rawDAWg — PROGRESS

Live checkpoint for the implementation agent. Facts only; see `docs/verification/requirements.md` for the per-requirement evidence table and `docs/adr/` for decisions.

## Current state

- **Milestone:** M0 complete; M1 implemented (hardware gates open); **M2 (flagship experimental synth) in progress** — sculpting canvas with live table handoff, all six generator types, unison, LFOs/mod matrix, randomize/chaos/locks, evolution/breed, XY morph (compatible structures), factory presets + `.rawdawgpreset`, sampler and SF2. Remaining M2 items: modular graph editor with bounded expression nodes (SYN-07), motion-frame editor UI (model and playback exist), incompatible-topology morph crossfade, gesture-to-automation recording (M3 lanes). See "M2 additions" and "Open gates".
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

## M2 additions (2026-11-25)

Commits 88cc71d…3c2a26d. 202 automated tests pass (`cargo test --workspace`), clippy clean, `cargo xtask verify --suite headless` HL-01…HL-08 PASS.

- Processor carry-over across plan swaps (`Processor::migrate_from`, fingerprinted instance states): parameter edits, gain/pan previews and preset loads never cut held notes unless the structure changes.
- Live wavetable handoff: bounded table queue into running voices, 8 ms crossfade, retirement off-thread, stale builds ignored. Waveform canvas with seven brushes (circular indices, seam wrap, radius/strength, wheel), spectrum painting (±0.5 dB verified), live output scope, one undo entry per stroke.
- Generators: FM/PM (4 operators, four algorithms, explicit one-sample feedback), additive (harmonic via table, detuned bank ≤32 partials, stretch), granular (phase-coherent playhead, 8 grains/voice bounded, windows), plucked string (Karplus–Strong, fractional delay, loop-filter group-delay compensation), wavetable motion frames (2–16, position modulatable), unison 1–8 with detune/spread and 1/√n gain.
- Modulation: 4 LFOs (5 shapes, Hz or beat-synced, global or per-voice), 16-route matrix (11 sources incl. mod wheel/aftertouch/random, 13 targets), control-rate evaluation every 16 samples. 238 stable parameter descriptors.
- Randomize/Chaos/Locks (categories + per-parameter, defaults keep root pitch/safety/routing), bounded audition validation (silent/non-finite/too-loud rejected, chaos back-off), 3×3 evolution with history/favorites/compare, breeding, XY morph pad with live updates and freeze; T-MUTATE/T-MORPH tests (determinism, 1000 seeds, ≥90 % perceptible difference, locks, corner exactness, smooth midpoints, incompatible reported).
- 29 original factory presets (9 unusual) across all eight characters, tested audible/finite/pairwise distinct; `.rawdawgpreset` ZIP format with manifest/assets, traversal/symlink/bomb protection; preset browser (search, character, favorites, preview/load, save, import/export).
- Sampler (`native.sampler`) and SoundFont player (`native.sf2`) with symphonia decoding, content-addressed asset import, engine asset cache with surfaced problems, SF2 parser with explicit supported-generator list and warnings; T-SAMPLER tests; demo-sampler fixture and HL-08.
- Evidence screenshots: `docs/verification/screenshots/m2-*.png` (self-captured under WSLg).

## M1 additions (2026-11-25)

- `crates/midi`: midir input manager (ports, enable/disable, hotplug rescan, disconnect → release), bounded queue from the device thread, byte-stream parser with running status/SysEx skipping, per-device FIFO note identity, poly pressure routing. Tests: 6 (parser, registry, graceful no-backend).
- App: menu bar (File/Edit/View/Help), transport with bar:beat and metronome toggle, shortcuts (Space, Esc, Ctrl+Z/Ctrl+Shift+Z/Ctrl+Y, Ctrl+S, F6/F7), Settings window (output device/rate/buffer with nearest-supported hints and requested-vs-actual, MIDI device toggles with connection state and message counters, typing map), preferences in the OS config dir (device, rate, buffer, MIDI devices, octave/velocity, recent projects), channel rack with select/mute/solo/gain/pan gestures, synth panel (simple/advanced parameter sliders with live `Param` preview and one `SetInstanceState` undo entry per gesture, reset to Init), on-screen piano (click/drag, Mouse source), `--screenshot <png>` self-capture for GUI evidence.
- Audio I/O: pure negotiation policy with fake-device tests, rate-limited stream error reporting, early-failure detection window, one-time automatic fallback to the device-default buffer when a fixed buffer is refused at run time (preferences untouched, status explains).
- Evidence: `docs/verification/screenshots/m1-wslg-1440x900.png` (self-captured under WSLg, GLES backend; dark-only surfaces, synth panel, keyboard, honest "Audio unavailable" status).
- Environment note: the WSLg PulseAudio bridge accepted a 1024-frame fixed buffer and device-default streams early in the session (`--audio-smoke` exit 0, 0 xruns), then started failing every PCM with `snd_pcm_avail_delay: I/O error (5)` after repeated fixed-256 failures; `pactl info` still answers. This is bridge flakiness, not a device under our control; the app shows "Audio unavailable" with Retry and keeps editing/offline render working. Real-device evidence remains BLOCKED.

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
| MIDI-01 physical MIDI keyboard (IO-01) | BLOCKED | No MIDI backend/ports here (no ALSA sequencer). On real hardware: Settings > MIDI, enable the device, play; verify note on/off, velocity, sustain, bend, disconnect release. |
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

M2 remainder, then M3:
1. SYN-07: typed modular graph (per-voice then global), compiler with port/cycle/cost checks, explicit delayed feedback, bounded expression nodes (parser → bytecode, 256 instructions / 64 slots), advanced graph editor; T-GRAPH tests.
2. Motion-frame editor (add/remove/reorder frames, position scrub) on the canvas; incompatible-topology morph via bounded parallel corner instances with crossfade and CPU admission.
3. M2 gate pass: run the T-OSC/VOICE/SCULPT/MUTATE/MORPH/GRAPH/SAMPLER subsets, update the evidence table, commit the checkpoint.
4. M3: pattern/song transport UI, step rack and piano roll (same note model), snap/selection/clipboard/velocity/scale/chords, metronome count-in, tempo/meter editing, live recording with timestamp mapping and quantization, playlist with clips, MIDI learn.
Known limitations carried forward: MIDI timestamps are recorded but events are applied at the next block (sample-accurate placement arrives with recording in M3); morph gesture points are recorded in memory but automation lanes do not exist yet (M3).

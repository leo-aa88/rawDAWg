# rawDAWg — Requirement evidence (live)

Status values: **PASS**, **FAIL**, **BLOCKED**, **NOT RUN**, or **PARTIAL** (implemented subset with passing evidence; remaining scope listed). Evidence paths are relative to the repository; `target/verification/` artifacts are produced by `cargo xtask verify`.

Build under test: workspace at the commit recorded in `PROGRESS.md`; Rust 1.99.0; Ubuntu 22.04 (WSL2) locally. Windows evidence comes only from CI (`.github/workflows/ci.yml`) and is NOT RUN until the workflow has executed on a Windows runner.

| ID | Status | Milestone | Evidence / notes |
|---|---|---|---|
| PLAT-01 | PARTIAL | M0 | Native Rust workspace builds and tests on Ubuntu x86-64 (`cargo test --workspace`: 145 tests pass). Windows build: NOT RUN locally (no MSVC here); CI job `Windows 2022` defined, not yet executed. |
| PLAT-02 | PARTIAL | M0/M1 | CPAL enumeration (`rawdawg --list-audio-devices`), negotiation (rate/channel/format, requested vs actual in `ActualConfig`), stream open, xrun heuristic, no-device/failed states in UI with Retry. Evidence: `rawdawg --audio-smoke` on this machine → ALSA default device 48 kHz F32, 134 callbacks, peak 0.154 (WSLg PulseAudio bridge, **not** a real Ubuntu desktop device). Real-device matrix: BLOCKED (HW-01). |
| PLAT-03 | PARTIAL | M0 | Dark-only visuals forced every frame (`crates/app/src/theme.rs`), no theme toggle anywhere. Manual screenshot verification: BLOCKED (GUI-01). |
| PLAT-04 | NOT RUN | M8 | `cargo xtask package` exists and refuses with an explicit message; no packaging yet. |
| PLAT-05 | PASS (design) | M0 | No network, account or telemetry code; `deny.toml` bans `openssl-sys`; all operation offline. Re-verified at each milestone. |
| PLAT-06 | PASS | M0 | No JUCE/external DAW engine/webview in the graph (`cargo deny check bans`; `deny.toml` bans `juce`, `wry`, `tauri`, `webview2-com`). Custom DSP in `crates/dsp`, `crates/synthesizer`. |
| CORE-01 | PARTIAL | M1 | Play/stop/pause/seek, pattern/song mode, loop with truncation + retrigger, tempo map with step changes, metronome accent on bar. Tests: `crates/audio-engine/tests/engine.rs`, `crates/transport/src/lib.rs`. Missing: UI transport beyond play/stop/panic, count-in, meter changes in `TransportView`. |
| CORE-02 | PARTIAL | M1 | Channel model, mute via command/UI, gain/pan/mixer assignment in model and engine. Missing: step grid UI, add/delete/rename UI (M3). |
| CORE-03 | NOT RUN | M3 | Note model + commands (`AddNotes/RemoveNotes/UpdateNotes`) tested; no piano roll UI yet. |
| CORE-04 | NOT RUN | M3/M5 | Clip model + song schedule (`song_mode_renders_clips_with_tempo_map`); no playlist UI. |
| CORE-05 | PARTIAL | M1 | Mixer channels, routing order, master, solo logic, master protection limiter (`PeakLimiter`). Missing: inserts, sends, meters UI, latency compensation (M4). |
| CORE-06 | NOT RUN | M3 | Browser panel lists instruments/devices only. |
| CORE-07 | PARTIAL | M0 | Command model with inverses, gestures (one undo entry, cancel restores exactly), revision/dirty tracking: `crates/project/src/commands.rs` tests. UI Undo/Redo buttons wired. Save/open via CLI/`--open`; Save/Save-as UI and autosave: M6. |
| CORE-08 | NOT RUN | M3/M4 | Automation model validated; no playback/recording yet. |
| IO-01 | NOT RUN | M1 | midir not wired yet (next task). Source isolation and sustain handling tested at the synth level. |
| IO-02 | PARTIAL | M1 | Typing keyboard in the GUI: default Z–/ map, octave ±, velocity, no capture while a text field is focused, release on focus loss/disable, OS key repeat ignored, release uses original pitch. Manual verification: BLOCKED (GUI-01). |
| IO-03 | NOT RUN | M3 | |
| IO-04 | NOT RUN | M5 | |
| IO-05 | NOT RUN | M2/M5 | |
| IO-06 | PARTIAL | M0/M1 | Headless WAV export (16/24-bit PCM with TPDF dither for 16, 32-bit float), same engine as playback, no normalization, range/tail options: `wav_export_round_trips_sample_count_and_content`, `rawdawg --headless-render`. FLAC/MIDI/stems/progress: M5. |
| SYN-01 | PARTIAL | M1 | Layers (1–4) of bandlimited basic waves or an editable wavetable cycle, per-layer level/pan/tune/phase, ADSR amp + filter envelopes, SVF filter with key tracking. Tests: `crates/synthesizer/tests/synth.rs`, `crates/dsp`. Missing: FM/PM, additive, granular, physical model, unison, LFOs/modulation (M2). |
| SYN-02 | PARTIAL | M0 | Wavetable pipeline (cycle → DC removal → FFT → harmonic truncation → mips → Hermite playback) implemented and tested (`crates/dsp/src/wavetable.rs`). Brushes, canvas and live handoff: M2. |
| SYN-03..08 | NOT RUN | M2 | Deterministic RNG framework (`chacha8-v1`) in place. |
| SYN-09 | NOT RUN | M2 | |
| SYN-10 | PARTIAL | M1 | Voice allocation (idle → oldest released → oldest active), stealing with preallocated fade tails (click bound tested), poly/mono/legato, tuning, pitch bend, sustain per source/channel, no hanging notes after release, all-notes-off/reset. Per-note expression: M2. |
| FX-01..03 | NOT RUN | M4 | |
| PLUG-01 | PARTIAL | M0 | Real CLAP module (`librawdawg_clap.so`, id `rawdawg.synth`, name `rawDAWg Synth`) wrapping the flagship synth: note ports, stereo out, params, state. Host round trip through the built library renders 440 Hz and round-trips state (`crates/plugin-clap/tests`). VST3 module (`librawdawg_vst3.so`) exports `GetPluginFactory` with a gain effect exercising component/processor/controller/state paths (`crates/plugin-vst3/tests`). Windows binaries: NOT RUN. Effects export, GUI, validators: M7. |
| PLUG-02 | NOT RUN | M7 | |
| PLUG-03 | BLOCKED | M7 | clap-validator not installed here (PL-03); VST3 validator needs SDK build (PL-04). |
| DATA-01 | PARTIAL | M0 | Schema v1 structs, validation (duplicates, references, limits, traversal, cycles), atomic save with previous-manifest backup, size/nesting caps, newer-schema rejection: `crates/project` tests, HL-06. Assets/recovery/locking: M6. |
| DATA-02 | PARTIAL | M0 | Deterministic renders (HL-05), save/load render equivalence (HL-06), synth state round trip. Cross-OS comparison: NOT RUN. |
| QUAL-01 | PARTIAL | M0/M1 | Zero allocations on owned render path under traffic (`render_block_is_allocation_free_under_traffic`, `process_path_is_allocation_free`), variable blocks 32…2048 incl. odd sizes, six sample rates (HL-03/HL-04), f32 processing, i128 exact timing. Full stress (30 min, RSS) : M8. |
| QUAL-02 | PARTIAL | M0/M1 | Bandlimiting ≥35 dB (ADR 0003), smoothing, NaN/Inf sanitize + counters, master lookahead limiter with explicit unprotected offline option. Oversampled nonlinearities, DC blocking on effects: M4. |
| QUAL-03 | PARTIAL | M0 | 145 automated tests; `cargo xtask verify` report (`target/verification/verify-all.json`); CI workflow for Ubuntu 24.04 + Windows 2022 defined, NOT RUN yet; `cargo deny` licenses pass. |
| QUAL-04 | NOT RUN | M8 | |

## Test-group status (TESTING.md §3)

| Group | Status | Where |
|---|---|---|
| T-RT | PARTIAL (PASS for owned synth + engine paths) | `crates/rt/tests/alloc_guard.rs`, `crates/synthesizer/tests/synth.rs`, `crates/audio-engine/tests/engine.rs` |
| T-TIME | PARTIAL (PASS: beat/bar frames, tempo changes, 1 h, half-open blocks, rounding oracle, loop/seek/stop) | `crates/transport`, `crates/audio-engine/tests/engine.rs` |
| T-OSC | PASS for implemented oscillators (A4 ±0.5 Hz at six rates, alias ≥35 dB, continuity, DC, seam) | `crates/dsp/src/{oscillator,wavetable}.rs` |
| T-VOICE | PARTIAL (PASS: polyphony, stealing, legato, sustain, source isolation, release, determinism) | `crates/synthesizer/tests/synth.rs` |
| T-GRAPH | NOT RUN (M4) | |
| T-DATA | PARTIAL (round trip, malformed/newer schema, nesting bomb, atomic save) | `crates/project/src/persist.rs` |
| T-EXPORT | PARTIAL (WAV formats, sample counts, content) | `crates/audio-engine/tests/engine.rs` |
| T-PLUGIN | PARTIAL (ABI lifecycle, process, params, state through built modules) | `crates/plugin-clap/tests`, `crates/plugin-vst3/tests` |
| T-IO | PARTIAL (negotiation logic, enumeration never panics) | `crates/audio-io` |
| T-SCULPT, T-MUTATE, T-MORPH, T-FX, T-SAMPLER, T-MIDI | NOT RUN | later milestones |

## Manual / platform gates currently open

See `PROGRESS.md` → "Open gates" for HW-01, GUI-01, PL-03/04/05, Windows CI and packaging.

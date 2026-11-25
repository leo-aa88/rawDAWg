# rawDAWg — Requirement evidence (live)

Status values: **PASS**, **FAIL**, **BLOCKED**, **NOT RUN**, or **PARTIAL** (implemented subset with passing evidence; remaining scope listed). Evidence paths are relative to the repository; `target/verification/` artifacts are produced by `cargo xtask verify`.

Build under test: workspace at the commit recorded in `PROGRESS.md`; Rust 1.99.0; Ubuntu 22.04 (WSL2) locally. Windows evidence comes only from CI (`.github/workflows/ci.yml`) and is NOT RUN until the workflow has executed on a Windows runner.

| ID | Status | Milestone | Evidence / notes |
|---|---|---|---|
| PLAT-01 | PARTIAL | M0 | Native Rust workspace builds and tests on Ubuntu x86-64 (`cargo test --workspace`: 145 tests pass). Windows build: NOT RUN locally (no MSVC here); CI job `Windows 2022` defined, not yet executed. |
| PLAT-02 | PARTIAL | M0/M1 | CPAL enumeration, pure negotiation policy with fake-device tests (stereo/f32 preference, nearest-supported hints), explicit device/rate/buffer controls in Settings with requested vs actual, rate-limited error reporting, automatic one-time fallback to the device buffer (preferences untouched), no-device/failed states with Retry. Evidence: `rawdawg --audio-smoke` → ALSA default 48 kHz F32, 0 xruns (WSLg PulseAudio bridge, **not** real hardware; the bridge later wedged, see PROGRESS.md). Real-device matrix: BLOCKED (HW-01). |
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
| IO-01 | PARTIAL | M1 | midir adapter with device list/enable/rescan/disconnect release, running-status parser, FIFO note identity, CC/bend/pressure; events routed to the selected channel with source `MidiDevice(n)`. Tests: `crates/midi`. Physical keyboard: BLOCKED (MIDI-01). Sample-accurate timestamp placement: M3. |
| IO-02 | PARTIAL | M1 | Typing keyboard (Z–/ map, octave ±, velocity, no capture in text fields or with Ctrl/Alt, release on focus loss/disable, OS repeat ignored, release uses original pitch) and on-screen piano (click/drag, Mouse source). Screenshot evidence `docs/verification/screenshots/m1-wslg-1440x900.png`; interactive verification: BLOCKED (GUI-01). |
| IO-03 | NOT RUN | M3 | |
| IO-04 | NOT RUN | M5 | |
| IO-05 | PARTIAL | M2 | WAV/FLAC/MP3 decode and SF2 import into the project with explicit unsupported-file errors; originals never modified. SMF import: M5. |
| IO-06 | PARTIAL | M0/M1 | Headless WAV export (16/24-bit PCM with TPDF dither for 16, 32-bit float), same engine as playback, no normalization, range/tail options: `wav_export_round_trips_sample_count_and_content`, `rawdawg --headless-render`. FLAC/MIDI/stems/progress: M5. |
| SYN-01 | PASS (automated) | M2 | Wavetable (2–16 motion frames), subtractive basics, FM/PM 4-op, additive (≤128 partials, detuned bank), granular, plucked string; 1–4 layers, unison 1–8, level/pan/tuning/phase, amp+filter envelopes, SVF, 4 LFOs, 16-route matrix. Tests: `crates/synthesizer/src/generators.rs`, `tests/synth.rs` (pitch per generator, unison, vibrato/mod wheel, allocation-free). Listening evaluation: manual, open. |
| SYN-02 | PASS (automated) | M2 | Canvas edits the stored cycle (not a scope); Draw/Deform/Smooth/Sharpen/Stretch/Distort/Noise on circular indices with seam wrap, radius/strength/wheel; live table handoff with 8 ms crossfade and no retrigger; separate live scope. Tests: `sculpt.rs` (T-SCULPT), `tests/synth.rs::live_table_update_*`. Interactive p95 ≤50 ms latency measurement: NOT RUN (needs display+device session). |
| SYN-03 | PASS (automated) | M2 | Randomize button + Chaos + Locks (categories/per-param, defaults safety/pitch/routing), deterministic seed, bounded attempts with audition validation, chaos 0 identity, undo exact. Tests: `mutate.rs`. |
| SYN-04 | PASS (automated) | M2 | 3×3 deterministic descendants per (parent, seed, generation, index), cached thumbnails/tags, click previews live, Use/Cancel/Favorite/Compare parent, history back/forward, Breed two. Tests: descendants distinct/deterministic, breed locks. |
| SYN-05 | PARTIAL | M2 | Four-corner bilinear morph for compatible structures (normalized-domain params, time-domain waveforms, heaviest-corner discrete), live updates, freeze, gesture points recorded. Tests: corners exact, midpoints smooth. Incompatible-topology crossfade and automation lanes: open. |
| SYN-06 | PARTIAL | M2 | Spectrum tab paints harmonic magnitudes (±0.5 dB verified), erase, dB view; motion frames modelled and modulatable (`Motion Morph` preset). Phase lane and frame editor UI: open. |
| SYN-07 | PASS (automated, GUI manual open) | M2 | Typed voice/global graphs (audio/stereo/control/event ports, 22 node kinds), compiler with type/cycle/limit/cost checks, feedback only through Delay nodes (≥ 64-sample quantum), bounded expression nodes (256 instr / 64 slots, checked domains), editor with palette/search, typed port drag, error highlight, last playable graph. Tests: `crates/synthesizer/tests/graph.rs` (13), `expr.rs` unit tests (4), HL-09. Screenshot `m2-graph-editor.png`. Manual GUI interaction check: open (GUI-01). |
| SYN-08 | PASS (automated) | M2 | 29 original presets across all characters (9 unusual), browser with search/favorites/audition, save/rename via save-as, `.rawdawgpreset` import/export with validation. Tests: `presets.rs`, `project/src/preset.rs`. |
| SYN-09 | PASS (automated) | M2 | Sampler (one-shot/loop, root pitch, start/end, loop crossfade, ADSR, polyphony) and SF2 player (bank/program, key/vel zones, global zones, loops, envelope, generators list, warnings) with symphonia decode and content-addressed import; drag/drop and path import create channels. Tests: `crates/sampler/tests`, HL-08. |
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
| T-GRAPH | PARTIAL | Synth graph part PASS: type mismatch and instant cycles rejected, explicit delayed feedback only, deterministic topology, per-voice vs global modulation, invalid expression rejection and bounded stack/instruction counts (`tests/graph.rs`). Mixer part (summed impulse with latency compensation, pre/post/sidechain, bypass alignment): M4. |
| T-DATA | PARTIAL (round trip, malformed/newer schema, nesting bomb, atomic save) | `crates/project/src/persist.rs` |
| T-EXPORT | PARTIAL (WAV formats, sample counts, content) | `crates/audio-engine/tests/engine.rs` |
| T-PLUGIN | PARTIAL (ABI lifecycle, process, params, state through built modules) | `crates/plugin-clap/tests`, `crates/plugin-vst3/tests` |
| T-IO | PARTIAL (fake-device negotiation across formats/channels/rates, nearest-supported reporting, queue saturation counters, enumeration never panics, edge conversion saturation) | `crates/audio-io`, `crates/rt` |
| T-MIDI | PARTIAL (byte parser: running status, velocity-0 off, FIFO identity, SysEx/realtime, controllers; disconnect release) | `crates/midi` |
| T-SCULPT | PASS (brush locality/seam/determinism, +6 dB harmonic ±0.5 dB, live update without retrigger) | `crates/synthesizer/src/sculpt.rs`, `tests/synth.rs` |
| T-MUTATE | PASS (determinism, chaos 0, locks, 1000 seeds valid/finite, ≥90 % perceptible, descendants, breed) | `crates/synthesizer/src/mutate.rs` |
| T-MORPH | PARTIAL (corners exact, weights sum 1, smooth midpoints, incompatible reported; crossfade/record-replay open) | `crates/synthesizer/src/morph.rs` |
| T-SAMPLER | PASS (bounds, loop, pitch/duration, rate conversion, SF2 zones/bank/loop/warnings/malformed) | `crates/sampler/tests/sampler.rs` |
| T-FX | NOT RUN (M4) | |

## Manual / platform gates currently open

See `PROGRESS.md` → "Open gates" for HW-01, GUI-01, PL-03/04/05, Windows CI and packaging.

# rawDAWg — Testing and release acceptance

## Branding acceptance

Verify rawDAWg capitalization and tagline in application identity surfaces (window title/About, first-party plugin names, installer/desktop entry), `rawdawg`/`rawdawg.exe` binary, `rawdawg` DEB package, `.rawdawg` project and `.rawdawgpreset` preset round trips, and project opening through supported platform associations. No obsolete neutral product identifiers in user-visible metadata.

## 1. Verification rules

Use deterministic generated fixtures (sine, impulse, sweep, noise, chord/MIDI, stereo/mono samples, minimal SF2, impulse responses, plugin test binaries, representative saved projects). No copyrighted sample assets without distribution rights. Run pure engine tests without devices; fake clock/device tests are labeled simulated. Hardware, GUI and external host tests require real execution and evidence. A command that only compiles is not behavioral verification. Every requirement ID has an automated test where feasible and/or manual test, plus evidence linked in `docs/verification/requirements.md`. Status values PASS, FAIL, BLOCKED, NOT RUN; include OS, build hash, command, versions, log path, reason and reproducible steps. Do not present planned tests as results.

## 2. Required developer commands

Implement cargo alias `xtask` in `.cargo/config.toml` and document exact platform prerequisites at M0. Commands below are deliverables (not preexisting executables):

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --release --locked
cargo xtask fixtures
cargo xtask verify --suite headless
cargo xtask render --project fixtures/demo.rawdawg --output target/verification/demo.wav
cargo xtask bench-audio
cargo xtask verify --suite plugins
cargo xtask package --format deb       # Ubuntu only
cargo xtask package --format msi       # Windows only
cargo xtask package --format portable  # Windows only
```

`verify` must fail nonzero for failed required tests, report blocked/skipped separately and fail the full release verification if gates remain missing. Add supported CLI `rawdawg --headless-render <project> --output <file>` and `--safe-mode` and `--open <path>`; parse errors explicit, no GUI/audio device for headless. CI on Windows and Ubuntu runs fmt/clippy/tests/build/headless render and native packaging when implemented, features in explicit matrix (do not force all-features if incompatible or SDK gated); ASIO builds and optional host backends separately when dependencies available. Upload CI evidence/test artifacts and unsigned package artifacts only through authorized CI workflow; no automatic public releases. Pin actions; run dependency audit and license report (cargo-deny/audit or equivalent, document exceptions). Use sanitizers/Miri where feasible on unsafe primitives, fuzz file parsers/graph/ABI validation with bounded fixtures. Never install random downloaded binaries solely for tests.

## 3. Numerical and real-time tests

Use independently computed expected results, not the algorithm under test as oracle. Default deterministic fixture at 48 kHz; exercise all six rates with blocks 32/64/127/128/256/511/1024/2048 and irregular final blocks. Compare block partition invariance for native deterministic paths (max absolute 1e-5 or justified algorithm-specific tolerance), repeat same render identical where supported; cross-platform normalized RMS error ≤1e-4 for simple fixtures, document larger thresholds for FFT/chaotic nonlinear paths; never promise external plugin determinism. Establish numerical thresholds before interpreting failures; changes require evidence and rationale.

| ID | Acceptance check |
|---|---|
| T-RT | Instrument test allocator with thread-local RT flag; zero alloc/realloc/free in owned warmed render/capture path including note on/off, modulation, buffer sizes, prepared plan/table swaps, queue overflow and retirement. Stress GUI/MIDI traffic and graph edits. Audit locks/I/O/panic and unsafe. Third-party path separate. |
| T-TIME | 120 BPM beat=24,000 frames at 48k, bar=96,000; tempo changes, triplets, meter changes, 1-hour scheduling, boundary events/loops/seek, variable blocks; scheduled offsets exact to defined rounding (≤1 frame vs rational oracle); no drift or duplicate/drop at boundaries. Note overlap/sustain/source tests. |
| T-OSC | A4 frequency 440 ±0.5 Hz on 2 s steady render, oscillator amplitude/phase continuity, pitch bend, DC handling, bandlimited saw/square/table high-pitch alias test (nonharmonic spectral energy at least 35 dB below fundamental for controlled fixture), interpolation/table wrap and swap, FFT/IFFT error and mip changes. Measure appropriate fixtures at every rate. |
| T-VOICE | Polyphony, voice stealing, unison, legato, sustain, repeated pitch, source isolation, note release, 64 voice admission, finite output, deterministic note/noise seeds. After all notes off and finite release no sustained accidental sound. |
| T-SCULPT | Each brush changes intended samples, seam wraps, undo restores exact stored state; held note changes audio without reattack (nontrivial spectral/RMS delta), no nonfinite samples. Spectrum edit a selected harmonic by +6 dB verifies magnitude change ±0.5 dB below Nyquist; motion frames endpoints and interpolation. |
| T-MUTATE | Same seed/patch/locks/amount yields same patch; chaos 0 identical; locked fields/graph survive, invalid graph rejected, 1000 bounded seeds no NaN or runaway, ≥90% of default-chaos valid mutations on representative patches differ perceptibly in features; no accidental silent candidates under default filter. Undo exact. Nine real descendants and breed locks. |
| T-MORPH | At each XY corner equals corner render after reset, weights sum 1, midpoints smooth, incompatible topology crossfade and latency alignment, CPU admission error, record/replay gesture, freeze/reload equivalent. |
| T-GRAPH | Reject type mismatch/instant cycles, permit explicit delayed feedback only, deterministic topology, summed impulse with latency compensation aligned within 1 frame, correct pre/post/sidechain, bypass alignment, sample-vs-global modulation, invalid expression rejection and bounded stack/instruction counts. |
| T-FX | Each family/mode exercised with impulse/sweep/step/chord. EQ target gain ±0.5 dB in steady test, delay taps within 1 frame, feedback decays at stable setting, reverb RT60 ±20% on calibrated mode, distinct reverb responses, convolution equals direct convolution within tolerance for small IR, compressor steady gain ±1 dB vs transfer curve and sidechain action, limiter ceiling +0.3 dB tolerance vs independent ≥8x reconstruction for test set (true peak mode), gate hold/threshold, distortion harmonic response, modulation FX rate, pitch shift within ±10 cents on steady tone and preserved duration, formant shift measured on synthetic vowel, spectral unity reconstruction/bypass compensated, stereo mono/width behavior. Test each preset and random rack for finite output. |
| T-SAMPLER | One-shot/loop bounds and interpolation, pitch/octave duration, envelope, sample rate conversion, SF2 key/velocity zones, bank/program, loop, unsupported feature warning, malformed RIFF rejection. |
| T-IO | Fake devices negotiate formats/channels/rates, no-device/offline, disconnect, queue saturation, xrun status; loopback capture correct duration/alignment with known simulated latency; test FIFO drift and lost frames. Real device separately. |
| T-DATA | Round-trip saved project and presets, UUIDs/automation/plugin opaque bytes, render equivalence after move to different directory/OS, missing asset relink, malformed/unsupported schemas, migrations, traversal/zip bombs, atomic save fault injection, autosave recovery, recording journal/disk full and save-as without modifying source. |
| T-MIDI | SMF 0/1 import, format 1 export round-trip note timing/tempo/meter/CC/program; PPQ conversion and overlapping notes; SMPTE division either explicitly supported or rejected with actionable error; physical MIDI mock timestamp, computer key repeat/focus loss and note identity. |
| T-EXPORT | WAV/FLAC decode back exact sample count, correct depth/rate, dither only requested/appropriate for PCM, range/tail cancel, stems naming and timing, no implicit normalization; compare against engine playback fixture. MIDI output valid parser and round trip. |
| T-PLUGIN | CLAP/VST3 validators, ABI lifecycle, variable blocks and event offsets, state/preset round trip, editor resize/close, latency/sidechain, independent host/plugin checks, scanner crash/timeout quarantine and safe mode. |

## 4. Performance budget and reference workload

Reference target selected for acceptance: 64-bit desktop/laptop with ≥4 modern physical CPU cores, 16 GB RAM, GPU supporting chosen wgpu backend; record exact CPU, OS, backend, driver and power settings rather than claim all such systems pass. Performance numbers are targets to measure, not existing benchmarks. Warm caches and use release build. At 48 kHz/256 frames deadline = 5.333 ms. Measure ≥5 min for default fixture: 8 tracks, four native synths playing 8 voices each (default wavetable/subtractive), four sample/audio tracks, EQ/comp on each, two send reverbs, master limiter; total 32 concurrent synth notes and normal automation. Target p99 owned callback ≤50% deadline (2.67 ms), max ≤90% during steady workload, zero observed underruns on reference hardware under normal conditions. Device/OS interference separately noted. 128-frame target additionally measure p99 and xruns; higher rates and 64/1024 buffers tested, not same full workload guarantee. Engine should fail admission or flag overload, not stall indefinitely. Stress test 30 min mixed edits, record/play/seek, graph swaps, sample import and UI interaction; track RSS growth after warmup (<10% or justified cache bound), no panic/hang/stuck notes. UI target p95 frame ≤16.7 ms at normal view, ≥30 FPS dense playlist (100 tracks/10k clips/notes using visible culling); waveform mouse-to-audible p95 ≤50 ms default patch at 256 buffer (instrumented queue/render plus hardware latency reported separately). Note-on latency measured, display hardware contribution; do not claim sub-ms hardware latency. Benchmark spectral/granular/4-corner morph separately with bounded cost. Use offline elapsed time/real-time factor and callback distributions; no fake CPU values.

## 5. End-to-end workflows (automated semantic flows + actual GUI)

| ID | Steps and expected result |
|---|---|
| E2E-01 | Clean launch, dark default, default synth; typing keyboard and physical MIDI note/chord, audible play; sculpt while held, randomize, lock pitch, evolve 2 generations, breed, morph and record gesture. Undo/redo and compare audible changes. |
| E2E-02 | Create drum steps and melodic pattern, piano roll note/chord/velocity edits, record MIDI with count-in/overdub, arrange repeats/make unique/trim, tempo change, loop/seek; export and inspect timing/audio. |
| E2E-03 | Record mono instrument or line input, stereo loopback, monitor off/on, stop creates clip; edit trims/fades, layer with MIDI, verify alignment using loopback and offset calibration; disconnect/restore. |
| E2E-04 | Import WAV, FLAC, MP3, SF2 and type 0/1 MIDI; route instruments and playback; sample preview, missing assets warning; export WAV/FLAC/MIDI and stems. |
| E2E-05 | Build insert chain, EQ drag, sidechain compressor, hall and convolution, latency-bearing effect, parallel paths, automate parameters/MIDI learn, randomize rack, morph, bypass, undo; no clipping or misrouting. |
| E2E-06 | Save/reopen full project; autosave/forced termination recovery, move project to other OS, missing plugin/asset preservation, relink; compare native audio renders. |
| E2E-07 | Scan/host independent CLAP and VST3 instruments/effects; automate, open editor, save/reopen, latency and missing-plugin handling. Crash scanner fixture quarantined, safe mode skips. Export own synth and effect into independent hosts, play and sculpt, save/reopen, validator all exports. |
| E2E-08 | Install from Windows MSI/portable and Ubuntu deb in clean environment; launch offline, open association, device/midi selection, DPI and window resize, no white owned UI, uninstall preserves user projects. No forbidden distribution formats. |

## 6. Hardware/platform release matrix

For each platform store device names, OS exact version, CPU, sample rate requested/actual, buffer requested/actual, observed xruns, test duration, driver/API and logs. Use a real audio interface (Focusrite Scarlett Solo or comparable; owner hardware not assumed available to agent), physical MIDI keyboard if available, headphones/safe monitoring and loopback for measured latency. Test 44.1/48/96/192 kHz on hardware where supported; other supported rates headless and device only where offered. Verify Windows WASAPI (shared, exclusive if supported), ASIO-capable build with actual ASIO device/SDK terms; Ubuntu ALSA and PipeWire, JACK compatibility with actual server/backend. Record unsupported configurations explicitly. On Ubuntu test X11 and Wayland/XWayland as supported by plugin editor; on Windows DX12/Vulkan as supported. At least one independent host per plugin format per OS and independent instrument/effect per format/OS; validators all exported types. Third-party GUI/host limitations documented. Screenshots of native UI at scales in UI_UX.md, manual workflow results and audio artifacts. If environment is headless, no Windows, lacks device/hosts, mark those gates BLOCKED/NOT RUN and provide exact manual commands/steps for owner; do not claim release pass.

## 7. Release gate

All specification IDs mapped to evidence, all automated tests pass on both targets, required hardware and independent-host manual gates pass, actual packages install/uninstall, native project portability verified, dependency/SDK licensing and assets audited, no critical unresolved bugs or fake features, docs and help match implementation. The rawDAWg brand is fixed; name clearance, legal publisher identity, license, ASIO distribution terms, signing and publication require owner review; unsigned local builds permitted, not represented as public-ready. `PROGRESS.md` must list every remaining gate explicitly. Release candidate can be generated with open manual gates only if labeled unverified; full v1 completion cannot be claimed.

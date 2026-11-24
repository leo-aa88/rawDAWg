# rawDAWg — Architecture and engineering contracts

Normative for PLAT, CORE, IO, QUAL, DATA and PLUG requirements. A native Rust workspace; no JUCE, external DAW engine or browser runtime. Prefer straightforward code over an elaborate abstraction that delays working audio.

## Product identifiers

Product name and display brand: **rawDAWg** (case-sensitive); tagline: **Less thinking. More noise.** Repository name: `rawDAWg`; executable/Cargo binary and Ubuntu package: `rawdawg` (`rawdawg.exe` on Windows); project directory suffix: `.rawdawg`; preset exchange suffix: `.rawdawgpreset`. Use `rawdawg` for platform-specific application storage and configuration names, `rawdawg.desktop` for the Linux desktop entry, `application/x-rawdawg-project` for the custom project MIME type, and `rawdawg-<version>-<arch>` as the distribution artifact basename (appropriate platform extension). Keep the native processor IDs and other internal contracts stable; use brand-prefixed external plugin identifiers as specified in `PLUGIN_API.md`. The legal publisher and signing identity remain undecided.

## 1. Dependencies and build policy

| Concern | Default choice / constraint |
|---|---|
| Toolchain | Stable Rust, edition 2024 if supported by pinned toolchain; MSVC x86_64 Windows; GNU x86_64 Linux |
| Native GUI | eframe/egui + wgpu, native winit events, custom painter widgets for piano roll/playlist/graph |
| Audio I/O | CPAL, explicit backend capability and sample format negotiation |
| MIDI | midir, midly for SMF; monotonic timestamp mapping |
| Queues | rtrb or equivalent bounded SPSC queue; vetted atomics for snapshots |
| DSP | Custom algorithms; rustfft/realfft, optional SIMD; use numerical libraries as infrastructure |
| Audio files | Symphonia (decode), hound or equivalent (WAV), verified FLAC encoder, rubato (resampling) |
| SF2 | Verify a license-compatible parser or implement documented SF2 parsing; implement playback/voice handling locally |
| Serialization | serde, serde_json, UUID stable IDs, SHA-256 for asset content IDs |
| Plugin ABI | clack / clap-sys, coupler-rs/vst3-rs or official SDK thin adapter; no external DAW/plugin DSP engine |
| Tests/build | cargo, rustfmt, clippy, proptest, criterion or equivalent, cargo xtask, GitHub Actions |

These are implementation defaults, not claims that every feature exists in any one version. M0 must verify API compatibility, MSRV, platform build requirements and license/SPDX of the exact selected versions. Pin Rust toolchain, Cargo.lock, CI actions and SDK revisions; keep default builds reproducible. Avoid unnecessary nightly or C++ code (small FFI/build shims permissible only where a documented ABI requires it). No copyleft/restricted dependency or SDK redistribution without review. License of application itself remains undecided.

## 2. Workspace and allowed dependencies

Create crates as they gain real functionality, not all as empty stubs. Suggested stable logical boundaries (agent may combine small crates with ADR; preserve directions):

```text
crates/
  types/            # IDs, units, immutable events, parameter descriptors
  project/          # model, commands, validation, migration, persistence
  transport/        # time maps and sample/beat conversion
  sequencer/        # pattern compilation, clip/automation events
  dsp/              # processing traits, buffers, filters, FFT, resampling
  synthesizer/      # voice engine, patches, native instrument algorithms
  effects/          # native effect algorithms
  sampler/          # native sample and SF2 instrument
  mixer/            # graph planner, routing and latency compensation
  plugin-api/       # native parameter/state contracts and adapter interfaces
  audio-engine/     # compiled realtime render plan and offline render
  audio-io/         # CPAL device/clock/format adapters
  midi/             # midir adapter, SMF parser/writer
  plugin-host/      # catalog, scan subprocess, CLAP/VST3 bridges
  plugin-clap/      # exported CLAP binaries and editor bridge
  plugin-vst3/      # exported VST3 binaries and editor bridge
  ui/               # native widgets, commands and editors
  app/              # composition root, preferences, coordinator, app CLI
  xtask/            # verification, fixture generation, packaging
assets/             # owned or redistributable fonts/icons/presets only
fixtures/           # generated/licensed deterministic test assets
packaging/          # native MSI and deb scripts/manifests
.github/workflows/  # Windows and Ubuntu CI
 docs/adr/          # rationale for material choices
 docs/verification/ # evidence and reports
```

`types`, `dsp` and `transport` must not depend on app/UI, devices, file I/O or format SDKs. `synthesizer`, `sampler`, `effects` depend on `dsp/types` and expose the native processing contract. `audio-engine` depends on renderable processors, mixer and compiled sequencer, not on OS or GUI. `project` owns editable semantic data, not active DSP objects; compiler converts it into a prepared plan. `ui` emits commands; it never directly owns active processors. `app` wires dependencies, including runtime plugin factory injection. Plugin binaries depend on reusable DSP and editor widgets, never the full DAW app. Keep dependency graph acyclic. Restrict unsafe to audited buffer/FFI interfaces and tested ownership primitives.

## 3. Runtime ownership and thread model

```text
 UI / OS event loop ---> bounded input/control queues ---> RT render callback ---> CPAL output
         |                          ^                        |             |
         v                          |                        v             v
 Project coordinator -------- prepared plan queue      retired objects   meters/status
         |                          ^                        |             |
         +---- worker pool ---------+                        +----> worker/coordinator
                 |       |
                 |       +-- asset decoding, FFT/table builds, render/export, save
                 +---------- graph compilation, plugin scanning process

 MIDI callback ---> bounded timestamped input queue ---> RT render callback
 CPAL input callback ---> preallocated frame FIFO ---> output/monitor/record dispatcher
 RT capture queue ---> disk writer worker ---> durable recording asset
```

- UI owns view state and command emission; coordinator owns mutable project model, undo history and command validation. Serialization works from an immutable snapshot created off RT. Keep input response independent of slow disk/scans.
- Output audio callback is sole owner of live graph/DSP mutable state; no UI locks or allocation/deallocation (including last Arc drop), blocking, log formatting, OS/file I/O, thread creation or panics. Realtime input callbacks obey same rules; no callback waits on another. Third-party processing cannot be proven to obey ours; isolate scan and clearly disclose runtime risk.
- Control changes use bounded SPSC paths (separate producer queues or a non-RT fan-in coordinator), stable parameter IDs, sample timestamps. Coalesce latest continuous knob/brush updates off RT; never coalesce note-offs away. On queue overflow count and surface it, schedule source-specific all-notes-off/panic via reserved atomic flag, reject graph changes visibly rather than silently losing commands. Counters/meters are atomics or bounded copied snapshots, relaxed where safe; no synchronous UI wait on audio.
- Off-thread prepare graph, sample buffers, tables, FFT plans, effect delay storage, lookup arrays, sorted schedule and plugin activation. Publish via bounded handoff at block boundary; retire old plan/state to worker through capacity-guaranteed queue. Do not swap if retirement queue lacks capacity. Carry over compatible node state by stable ID where possible using bounded preallocated migration; otherwise fade/reset with documented behavior. Crossfade topology/table changes under controlled CPU limits; use hard cut only for explicit stop/panic or unavoidable device recovery.
- Worker thread compiles heavy changes; only latest generation may publish, so stale work cannot overwrite a newer gesture. Build wave tables asynchronously and preview latest ready version, coalescing pointer movement; do not wait for worker inside callback. Use immutable asset handles backed by storage guaranteed to remain resident until RT retirement.
- Input and output devices can have independent clocks. Default to a single duplex device where available; for independent clocks use bounded FIFO and adaptive asynchronous resampling, track drift and under/overflow. Do not promise sample-perfect alignment between unsynchronized hardware.
- Offline renderer uses a separate owned instance of the same engine, processing blocks deterministically; may run faster than realtime and does not open audio hardware. Plugins requiring realtime rendering must explicitly opt into real-time export or report unsupported offline mode.

## 4. Time and event semantics

Project time is integer ticks (PPQ = 960). Tempo is positive microseconds per quarter, represented as integer; default 500000 (120 BPM). Time signatures are numerator/denominator powers of two (positive numerator, denom 1/2/4/8/16/32); meter changes on bar boundaries, tempo changes on tick boundaries; constant tempo segments in v1. Tempo range 20–400 BPM; meters 1–32 beats. Use signed 64-bit positions and checked arithmetic; nonnegative project ticks. Precompute cumulative rational segment times (i128 or verified equivalent) and derive frame index by rounding absolute elapsed time once, not accumulating per-block rounding errors. Preserve project musical positions when sample rate changes. Avoid accumulating drift; render events in half-open [start_frame, end_frame). Use stable event tie-break: resets/all-notes-off, note-off, controller/parameter changes, note-on, then deterministic source/ID order. Zero-duration notes invalid; imported short notes become at least one tick or event-level sample when scheduled with note-off not earlier than note-on. Document quantization at frame boundary.

Events carry source ID, channel (0–15), note ID (distinct overlapping equal pitches), pitch 0–127, velocity normalized 0–1, frame offset, bend, CC and expression. Normalize MIDI note-on velocity 0 to note-off. Source-specific note registry prevents stuck or cross-source releases; sustain defers release per channel/source. MIDI 1 overlapping same-channel notes match FIFO unless the format provides a note ID. Track transport playback and live audition separately. Stop/seek/loop boundaries release scheduled notes and reset/reconstruct active events per documented chase rule (seek chases persistent controller state; no retroactive attack on notes already spanning seek by default, optional future note chase). On loop, explicitly truncate scheduled notes crossing end, re-trigger at loop start if note starts there; live keyboard unaffected until global panic/stop policy. Pattern mode loops selected pattern; song mode schedules clip instances with offsets and repeat boundaries. Tempo/meter maps apply across song; pattern mode uses tempo at playhead start and active meter, no song clip events. Metronome accent on bar start, optionally excluded from export (default excluded).

Use actual monotonic input timestamps, calibrate to callback stream clock and current absolute frame; map past events to next available frame and count late events. Record original timestamps and compensate reported/measured latency, with user-adjustable offsets. Clip/pattern quantization edits ticks only on request. Timing of recorded notes and controllers is derived from the capture tempo map, not UI frame rate. Sample-accurate parameter automation native; interpolate step/linear between explicit points. For continuous parameters combine automation + modulation in documented normalized domain; parameter smoothing does not delay note events. Follow adapter accuracy limits for external plugins.

## 5. Processing buffers and graph

Audio is f32 planar, stereo main bus, mono converted explicitly; preallocate scratch buffers and use `frames` within max capacity, not fixed callback length. Default project sample rate 48 kHz, supported rates per QUAL-01, device default preferred when no preference. Preferred block 256 frames; allow 32/64/128/256/512/1024/2048 and device-negotiated actual values; split oversized callbacks into prepared chunks without dropping frames. Validate actual channel layouts (select first stereo pair or explicit channel mapping). Convert device sample types at edge with correct scaling/saturation. Internal summation f32 with f64 time/math where useful. No device clock assumed for offline rendering.

Graph model: instrument/audio sources → channel inserts → fader/pan → pre/post sends/bus inserts → master. Parallel paths have explicit summing and gain. Main pan is equal-power mono pan; stereo uses balance (document behavior and optional width). Inserts preserve wet/dry phase via latency compensation. Graph compiler checks port types and acyclic instantaneous dependencies, prepares deterministic topological order and buffer liveness reuse. Feedback permitted only via explicit delay node with at least one prepared processing quantum (fixed internal feedback quantum, default 64 samples, independent of callback chunks); gain bounded below unity by default, explicit nonlinear stability clamp, maximum energy/finite protection, visual feedback edge and declared latency. Other cycles rejected with cycle path. No unbounded recursive evaluation. Delay-compensation longest-path algorithm on acyclic routing: at summing points align upstream latencies, including dry/parallel and sidechain paths; master output latency published for UI/export. Intentional feedback delay is not compensated away. Recompile after latency notification; crossfade or pause safely, surface status.

Suggested initial supported budgets (configurable with rebuild outside RT): 128 mixer channels, 512 processing nodes/project, 64 voices/synth (32 default), 4 oscillator layers/voice, 8 unison voices/layer (total voice-oscillator budget enforced), 4096 scheduled events/quantum, 4096 control messages/queue, 8192-frame max backend scratch, 2 seconds maximum compensated path latency. Enforce explicit bounds before compilation; provide useful error when exceeded, never silently truncate. Test extremes without requiring that every possible expensive configuration can run on all hardware. Overload counter and xrun status; keep callback responsive; no hidden automatic quality degradation. User can raise buffer or reduce polyphony/oversampling.

## 6. Audio device and recording lifecycle

On startup enumerate CPAL hosts/devices and supported configs off RT. Default Windows WASAPI, Ubuntu default PipeWire where built/supported else ALSA/default sound server; expose explicit ALSA/JACK/PipeWire selections where available. ASIO feature-build separate if SDK/license requires; no dependency on ASIO for baseline startup. Validate actual driver, device format, channels, rate, block size. Display requested vs actual. Unsupported configuration offers nearest supported options, never silently changes stored preferences. No-device mode opens editor and offline renderer with visible “Audio unavailable”; no automatic demo playback pretending to be hardware. On device error/disconnect stop safely, release notes, preserve project and offer retry/default. Reopen only after successful prepare. Set priority via supported safe APIs where available, avoid requiring privileged commands. Input monitoring off until explicitly enabled; protect feedback and avoid doubling hardware direct monitoring (explain).

Capture: selected channels converted to float, stamped with stream/project frame; fixed queue of blocks consumed by disk writer to temporary WAV plus recovery journal. On queue full flag overflow, insert explicit silence for lost timestamp ranges or stop recording with recoverable partial take; report error, never misalign silently. Close/finalize on stop; attach immutable asset to project via command, offset alignment using reported/loopback calibrated latency. Record original sample rate and raw data. Long clips/assets streamed via bounded read-ahead ring/cache and prefetch for seek; worker decode/resample, RT uses already ready blocks. On cache starvation output silence for missing frames, flag dropout; offline waits outside RT for data. For short samples resident data is fine with explicit memory limit. No giant unbounded decoded allocations. Handle >4 GB recording/export via RF64 or split recoverable takes with clear documented limit; choose explicit validated strategy at M5. Disk full and permission failures retain partial recoverable files.

## 7. UI command model and project state

Every persistent edit is a typed command with validation and inverse/transaction snapshot. `begin_gesture` → preview updates → `commit_gesture` produces one undo entry, cancel restores original. Use stable object IDs, never UI indices as persistent identity. Audio graph compilation receives project revision; reject stale results. Save records a snapshot revision; dirty flag clears only if current revision matches saved revision. Undo/redo does not implicitly rewind transport; changes compile and apply at safe boundary. Protect against object deletion while notes held, stop/release associated voices and automation routes. Plugin opaque state capture follows threading rules; snapshots not taken in callback except plugin ABI explicitly requires a bounded audio-thread operation.

## 8. Platform integration and packaging

Both OSs build on every milestone. CI runs native Windows x64 and Ubuntu 24.04 runner; release builds tested on actual installed OS with display/audio. Windows MSVC and DX12/Vulkan wgpu; Ubuntu X11 and Wayland wgpu/Vulkan with XWayland fallback for compatible external editors. Detect GPU/backend error and offer a clear supported recovery; no webview. Specify minimum GPU/runtime requirements from testing, not guesses.

Use CPAL build docs for system libs. Ubuntu `libasound2-dev` and optional PipeWire/JACK dev deps according to pinned release; packaging derives runtime dependencies using native tooling. Windows ASIO SDK version/license must be reviewed separately from VST3 MIT license. For restricted SDK terms do not bundle or auto-fetch proprietary SDK without approval; provide a documented user-supplied SDK feature build, track ASIO full release gate. No license decision invented for app. Verify no JUCE dependency transitively.

Package app with MSI (native Windows installer using WiX or compatible tool) and portable directory containing EXE plus required redistributable assets/runtime (not falsely advertised as single binary). Ubuntu `.deb` only for installed package (e.g. cargo-deb or native dpkg tooling): proper dependency declarations, desktop entry, icon, project MIME association, no root execution of app, no unapproved auto-install of plugin binaries. Plugins as separate optional format artifacts/install choices; system-wide install requires owner/user action. Source build via Cargo on both. Native project folder open association may use its `project.json` or marker file and CLI path. Config/log data OS-specific per-user dirs; portable option explicit, never assume installation directory writable. Clean uninstall preserves projects and user content. Package checksums/SBOM/third-party notices; unsigned local packages clearly identified; signing and publication owner-controlled.

# rawDAWg — Implementation roadmap

Follow this dependency order. Each milestone delivers an executable or headless vertical slice, tests and evidence. Implement platform CI from day one, not as a final port. Do not skip a milestone simply because a later UI can be mocked. Independent later work can proceed with hardware/manual gates still open, but never mark those gates passed. Maintain `PROGRESS.md`, `docs/verification/requirements.md`, ADRs and a working build at every checkpoint.

## M0 — Bootstrap and risk reduction

- [ ] Inspect repository; record initial state, apply the fixed rawDAWg identity (repository `rawDAWg`, binary/package `rawdawg`, project `.rawdawg`), pin toolchain and exact compatible deps, verify their licenses and SDK terms. Set up Rust workspace only with needed crates, cargo alias, Windows/Ubuntu CI, fmt/clippy/tests/build, fixture generator and initial verification commands.
- [ ] Minimal native dark eframe/wgpu window, headless CLI and device enumeration on both; prove CPAL output and GUI integration separately, test no-device mode. Create actual adapter spike for CLAP/VST3 bindings (small test fixture, not full host); review ASIO SDK and optional feature build. Record risks and alternative bindings if necessary.
- [ ] Define IDs/units, command model, initial project types/schema, real-time ownership strategy, tests for queues and sample/time conversions.
- Gate: both platform build/test CI or explicitly blocked external execution, local headless render/driver smoke, no prohibited deps; M0 ADRs contain license and build findings. IDs PLAT-01/03/05/06, QUAL-03 partial.

## M1 — Audio engine and first playable instrument

- [ ] Audio I/O config, prepared callback, queues/retirement, transport clock, sample scheduling, f32 buffers, stereo output, mixer minimum, headless WAV render; real audio device and no-device recovery.
- [ ] Implement bandlimited basic oscillator, polyphony, envelope/filter, note registry, on-screen and typing keys, physical MIDI event adapter, device settings. Play C/A4 and chord; show status and meter. No fake playback.
- [ ] Basic project command model and undo for instrument/parameter changes; defaults and safe audition/master output. Test note timing, variable blocks, allocation guard, queue overload and device failure.
- Gate: T-RT/T-TIME/T-OSC/T-VOICE/T-IO/T-MIDI relevant subset; playable app + offline tone/chord fixture. IDs PLAT-02, QUAL-01/02, SYN-01/10, IO-01/02 partial, CORE-01 partial.

## M2 — Flagship experimental synth

- [ ] Source waveform sculpt tools and actual audible updates while notes held, mipmaps/crossfade, spectral painting, motion frames, live scope, basic synth editor with keyboard. All tools wired and undoable.
- [ ] Add FM/PM, additive, granular, physical model, layers/unison, arbitrary envelopes/LFOs/modulation, graph compiler and bounded expression nodes; typed delayed feedback; limits and cost checks.
- [ ] Deterministic randomize/chaos/locks, evolution 9 variants/preview/history/breed, four-corner morph compatible/incompatible, gesture recording data and freeze; native preset model/browser with original factory presets.
- [ ] Native sampler and SF2 implementation; decode/resample assets off-thread, sample loop tests, SF2 fixture and unsupported generator warnings. Persist patch/preset state; hook shared parameter descriptors.
- Gate: T-OSC/VOICE/SCULPT/MUTATE/MORPH/GRAPH/SAMPLER plus E2E-01 subset. SYN-01..10; IO-05 partial, DATA-02 partial. Checkpoint: experimental instrument standalone within app, independently audible and tested.

## M3 — Sequencer and composition UI

- [ ] Pattern/song transport, step rack and piano roll same note model, real mouse editing, snap/selection/clipboard/velocity/scale/chords, pattern chooser, metronome/count-in, tempo/meter maps, note/CC chase and loop semantics.
- [ ] Live MIDI and typing recording, timestamp mapping, quantization/overdub, expression and MIDI learn; input source isolation, shortcut/focus safety.
- [ ] Playlist with reusable pattern clips, audio/automation model, browser and common workspace layout, drag/drop and make unique; timeline UI and command undo. Audio clip playback/edit refined M5.
- Gate: T-TIME/T-MIDI plus E2E-02 note/pattern steps, UI input tests and manual layout. CORE-01/02/03/04/06/07/08 partial, IO-01/02/03, PLAT-03. Checkpoint: compose and arrange MIDI song in actual UI.

## M4 — Mixer and full native effects

- [ ] Complete graph routing and compensation, mixer UI, insert chains, bus/send/sidechain/parallel, fader/pan/solo/mute/meters. Compile graph off-thread, bounded delayed feedback and bypass.
- [ ] Implement effects individually in vertical slices: EQ, compressor, limiter, delay, reverbs (all modes), distortion, chorus/phaser/flanger/gate, stereo, pitch/formant, granular, spectral. Each has actual DSP, presets, test and dark simple editor before proceeding.
- [ ] Reverb decay drawing, delay taps, per-band distortion, rack randomization/morph, modulation. Automation clips/parameter recording/MIDI learn integrated end-to-end. Include safety and latency tests.
- Gate: T-FX all named modes, T-GRAPH/T-RT, E2E-05. CORE-05/08, FX-01/02/03, QUAL-02. Checkpoint: arrange and mix song with real native effect suite.

## M5 — Audio clips, recording, import/export

- [ ] Capture devices/FIFO/writer, mono/stereo/monitoring, latency calibration, recording journal and recovery. Long audio read-ahead/cache, independent clock handling, clip trimming/fades/gain/rate, waveform thumbnails, sample browser previews.
- [ ] WAV/FLAC/MP3 decode, SMF 0/1 and SF2 import, metadata and errors; MIDI program mapping. Export WAV/FLAC/MIDI and stems with options, progress/cancel, dither where applicable, no hidden normalization. Define stem scope: per track/channel or selected bus tap post inserts/fader before master; include separate return stems if chosen; document shared sends/sidechain mean stems need not sum to master. All stem files share exact render timeline/length, sanitized unique names.
- Gate: T-IO/T-EXPORT/T-MIDI/T-SAMPLER, E2E-03/04, actual device tests where accessible. CORE-04/06, IO-04/05/06, PLAT-02. Checkpoint: record external audio and export finished song.

## M6 — Persistence, recovery and automation completion

- [ ] Full project schema/validation, assets/hash paths, atomic save/save-as, autosave and crash recovery, dirty revisions and file locking; native presets portable, graph/state migrations and missing asset handling.
- [ ] Finish persistent undo/redo for every edit, automation read/touch/write with explicit arming and orphan handling, effect/synth presets and MIDI mappings. Demo project and local help.
- [ ] Verify moving projects between Windows/Ubuntu; simulate missing third-party state before host implementation, preserve opaque blob.
- Gate: T-DATA/T-EXPORT and E2E-06 native paths; DATA-01/02, CORE-07/08, QUAL-03. Checkpoint: save, recover, transfer and reopen full native project.

## M7 — CLAP and VST3 export and hosting

- [ ] Stable native processing API; CLAP flagship export first, validator, independent host and actual editor. Export all native effect families; shared wrapper not copy-paste DSP.
- [ ] CLAP scanner process, catalog/quarantine, generic and native GUI, host event/bus/parameter/state/latency handling; test independent plugins. Safe mode and missing plugin placeholders.
- [ ] Repeat export and host paths for VST3 using vetted bindings, lifecycle/component/controller, parameter queues, GUI and validators. Windows + Ubuntu binary artifacts, correct bundle structures and IDs; full state portability and user-facing installation instructions.
- [ ] Validate each exported plugin type; independent host test for flagship and representative native effect per format/OS, independent third-party instrument/effect per format/OS. Verify plugin assets and host GUI teardown, latency/sidechain and automation. Record gaps accurately.
- Gate: T-PLUGIN, E2E-07 plus relevant T-DATA/RT/FX, PLUG-01/02/03. Checkpoint: actual interoperable plugins and hosting, not just native presets or self-host fixture.

## M8 — Release hardening and distribution

- [ ] Full test matrix, performance benchmarks/optimization from measurements, stress/fuzz/recovery, keyboard accessibility/DPI/usability, polish and clear error states, verify both OS drivers and actual MIDI hardware. Fix regressions; no silent degradation or hidden failures.
- [ ] Windows MSI + portable EXE directory, Ubuntu native deb only, offline installation/launch, dependency manifests, app icons/desktop and associations, uninstall preservation, source build instructions, optional plugin install packages. Generate SBOM, license notices and checksums, no AppImage/Snap/Flatpak. Owner review license, signing, legal publisher identity and name clearance; no publication without authorization.
- [ ] Run all automated and manual E2Es, actual package smoke on clean machines, complete traceability evidence, list remaining gates. Provide quickstart and troubleshooting.
- Gate: all IDs, all T and E2E checks in TESTING.md and release criteria, PLAT-04, QUAL-03/04. Checkpoint: verified v1 only if all mandatory gates pass; otherwise explicit release candidate with blockers.

## Requirements traceability index

| Requirement group | Implementation milestones | Principal evidence |
|---|---|---|
| PLAT-01..06 | M0/M1/M5/M7/M8 | CI, T-IO, E2E-08, dependency audit, hardware matrix |
| CORE-01..04 | M1/M3/M5 | T-TIME/T-MIDI, E2E-02/03 |
| CORE-05..08 | M3/M4/M6 | T-GRAPH/T-DATA, E2E-02/05/06 |
| IO-01..03 | M1/M3 | T-MIDI/T-VOICE, E2E-01/02 |
| IO-04..06 | M2/M5 | T-IO/T-SAMPLER/T-EXPORT, E2E-03/04 |
| SYN-01..10 | M1/M2 | T-OSC/VOICE/SCULPT/MUTATE/MORPH/GRAPH/SAMPLER, E2E-01 |
| FX-01..03 | M4 | T-FX/T-GRAPH, E2E-05 |
| PLUG-01..03 | M0/M7 | T-PLUGIN, E2E-07, validators/independent host evidence |
| DATA-01..02 | M2/M6/M7 | T-DATA, E2E-06/07 |
| QUAL-01..04 | M0–M8 | T-RT, numerical tests, CI, benchmarks, actual hardware/GUI matrix |

Expand this into one row per ID in the live evidence file; group mapping is a navigation aid, not permission to mark a group pass from one test.

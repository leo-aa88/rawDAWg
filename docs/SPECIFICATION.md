# rawDAWg — Product specification

Revision 1.1 | Normative requirements: MUST / SHALL. Defaults may be tuned by the owner later.

## Product identity

Product name and display brand: **rawDAWg** (case-sensitive); tagline: **Less thinking. More noise.** Repository name: `rawDAWg`; executable/Cargo binary and Ubuntu package: `rawdawg` (`rawdawg.exe` on Windows); project directory suffix: `.rawdawg`; preset exchange suffix: `.rawdawgpreset`. The brand is fixed; use original visual assets and retain the existing functional requirements.

## 1. Product and audience

A local-first, pattern-based DAW for musicians familiar with FL Studio, with an unusually exploratory instrument-building workflow. Users can compose a conventional song, record guitar/bass/vocals, load a MIDI file, mix it, or create an unfamiliar timbre in seconds without learning synthesis terminology. The flagship synth must be useful as an independent instrument in other DAWs. No proprietary look, logo, assets or code from FL Studio are to be copied; familiar concepts and interaction patterns are the inspiration.

Primary journey: launch → play with physical MIDI or typing keyboard → draw/drag a waveform while notes sound → randomize or evolve an interesting patch → record notes → arrange patterns → add reverb/other effects → save → reopen → export audio/MIDI. Second journey: open an existing MIDI project, choose instruments, edit, mix, render. Third: record from an audio interface and arrange/edit takes. Fourth: load the exported instrument in a different host or load a third-party plugin in this DAW.

## 2. Platform and product boundaries

| ID | Requirement |
|---|---|
| PLAT-01 | Native x86-64 Rust desktop app on Windows 11 and Ubuntu 24.04 LTS (chosen minimum test matrix); no platform is an afterthought. |
| PLAT-02 | CPAL-backed device selection and I/O; WASAPI and supported ASIO drivers on Windows; ALSA and PipeWire/JACK compatibility on Ubuntu. Explicit sample-rate/buffer/device controls, sensible default, clear fallback and recovery. |
| PLAT-03 | Dark interface only, no light/system theme toggle, including all owned plugin editors and in-app dialogs. OS/third-party dialogs outside our control are exceptions. |
| PLAT-04 | Native MSI and portable Windows build; native `.deb` Ubuntu package only. Source build supported. No AppImage/Snap/Flatpak. |
| PLAT-05 | Offline operation after installation; no account, telemetry, internet-dependent authorization, cloud backend or browser delivery. |
| PLAT-06 | No JUCE or external DAW engine; custom Rust DSP; thin OS/plugin ABI adapters allowed. |

Out of v1: web/mobile/macOS, multiplayer, cloud sync, video timeline, notation editor, surround/Atmos, ARA, VST2/AU/LV2, arbitrary native user scripts, arbitrary zero-delay feedback, distributed processing, auto-pitch-correction editor, commercial sample content. These exclusions do not remove any agreed feature. The editor is stereo-first, with mono input/clip support. Audio-rate modulation and bounded delayed feedback are in scope.

## 3. Core workflow

| ID | Requirement |
|---|---|
| CORE-01 | Transport: play, stop, pause, seek, loop, pattern/song mode, metronome and count-in; tempo map with step changes, time-signature map, bar/beat ruler, sample-accurate scheduling. |
| CORE-02 | Channel rack: add/delete/rename/reorder/duplicate instruments and samples, mute/solo, per-channel gain/pan/mixer assignment, editable step patterns (default 16 steps, adjustable subdivision/length), velocity and note pitch. |
| CORE-03 | Piano roll: keyboard ruler, grid, draw/select/move/resize/delete, multiple notes, box selection, duplicate, clipboard, adjustable snap, transpose, velocity lane, scale highlight/snap, chord insertion, quantize with strength. |
| CORE-04 | Arrangement: multiple tracks, reusable pattern clips, audio clips, automation clips; move/copy/split/trim/resize, loop, snap, clone pattern independently, mute/solo, zoom/scroll, clip gain and fades, non-destructive editing. |
| CORE-05 | Mixer: channels, groups/buses, master, stereo gain/pan/mute/solo, meters, peak hold, clip indicators, insert chains, reorder/bypass, pre/post sends, sidechain, parallel paths, latency compensation and feedback rules. |
| CORE-06 | Browser: instruments, effects, samples, projects, presets, search, favorites, drag-and-drop and audio preview at safe level. Native asset paths and local content. |
| CORE-07 | Shared undo/redo for persistent edits including gestures, mutations, routing and automation. Save/open/new/save-as, dirty status, recoverable autosave. No undo for transport/monitoring/meters. |
| CORE-08 | Parameter automation: draw/edit curves, step/linear interpolation, record supported parameters, MIDI learn, read/touch/write with explicit arming; sample-accurate where format permits. |

## 4. Musical input and audio

| ID | Requirement |
|---|---|
| IO-01 | Physical MIDI controllers via midir: select inputs, hotplug/reconnect, note on/off, velocity, channel, pitch bend, CC, sustain, channel/poly pressure when available; record and audition. No fixed controller brand. |
| IO-02 | Typing keyboard as piano, polyphonic within hardware rollover limits, adjustable octave and velocity, enable toggle, no capture in text fields; release notes on focus loss or disable. Mouse audition via piano ruler or synth keyboard. |
| IO-03 | Live note recording into pattern/piano roll, overdub/loop, optional quantization after capture, count-in, note duration and expression; preserve raw timing until user requests quantization. |
| IO-04 | Audio recording: select mono/stereo physical inputs, arm, monitoring (off by default), input meters, record to recoverable file, align with measured/reported latency and adjustable offset, create clips on stop. No disk I/O in callback. |
| IO-05 | Import WAV/FLAC/MP3, Standard MIDI File type 0/1, and SF2; handle unsupported files explicitly. Audio resamples off-thread or using bounded prepared streaming path, never mutates original. MIDI import preserves tempo, meter, channel/program and supported controller data. |
| IO-06 | Offline export: WAV (16/24-bit PCM and 32-bit float), FLAC (16/24-bit), MIDI type 1, individual stems, selectable range/rate, optional tails, cancel/progress; MP3 import only. Export uses same engine as playback; no hidden normalization. |

## 5. First-party instruments

| ID | Requirement |
|---|---|
| SYN-01 | Flagship polyphonic synth with wavetable, subtractive, FM/phase modulation, additive, granular, and a physical-model oscillator (at least plucked string). Multiple oscillator layers, noise, mix/unison, filter, envelope and modulation. |
| SYN-02 | Directly editable cyclic waveform under cursor: draw, deform, smooth, sharpen, stretch, distort/noise; brush size/strength, visible result, real audible preview while a note/chord is held. Automatic bandlimiting and smoothing. |
| SYN-03 | One-click randomize, adjustable chaos, selective locks, deterministic seed, per-operation undo, useful range-aware defaults and full-patch randomization including topology/effects. Preserve root musical pitch by default. |
| SYN-04 | Evolution view: generate nine descendants of a selected sound, instant/cached audition, choose one, breed two, evolve again, history and favorites. Audible variants must really differ. |
| SYN-05 | Four-corner XY morph between presets, smooth transitions, gesture recording into automation, sensible interpolation or crossfade of incompatible structures. |
| SYN-06 | Spectral painting: manipulate harmonic magnitudes and optional phase; add/erase/smooth, waveform updated and audible; support waveform evolution across keyframes/position. |
| SYN-07 | Advanced modular editor: typed audio/control/event routing, arbitrary compatible acyclic routes, bounded explicit delayed feedback, per-voice/global modulation and user-defined bounded mathematical DSP nodes. Optional, never necessary for basic use. |
| SYN-08 | Preset browser: original presets by character (bass, pad, lead, keys, percussive, texture, noise, experimental), audition, save/favorite/rename; import/export self-contained user preset including assets. |
| SYN-09 | Native sample instrument (one-shot/loop, pitch, start/end, gain/envelope) and SF2 player (bank/preset, key/velocity zones, loop, envelopes, supported generators/modulators, percussion bank). Clear unsupported SF2 feature notice instead of silent approximation. |
| SYN-10 | Safe voice allocation/stealing, mono/legato/poly modes, tuning, pitch bend, sustain, per-note expression where available; bounded CPU and no hanging notes. |

## 6. Native effects

| ID | Requirement |
|---|---|
| FX-01 | Native effect suite: room/hall/plate/spring/cathedral/shimmer/convolution reverb; stereo/ping-pong/tape/multitap/reverse delay; compressor, lookahead limiter with true-peak mode, parametric EQ, distortion (tube/tape/clip/wavefold/bitcrush), chorus, phaser, flanger, noise gate, pitch/formant shifter, granular processor, spectral processor, stereo width/pan/mid-side processor. |
| FX-02 | Every effect has immediately usable presets, bypass, wet/dry where meaningful, safe defaults, simple controls and optional advanced mode; reset, automation, undo/redo, meters as appropriate. |
| FX-03 | Experimental effect tools: editable reverb decay shape, movable delay taps, per-band distortion painting, morph and randomize racks, parallel routing and modulation. |

## 7. Plugin interoperability and persistence

| ID | Requirement |
|---|---|
| PLUG-01 | Native synth/effect core independent of host formats; export flagship synth and native effect suite as actual CLAP and VST3 plugin binaries/bundles for both OSs (not just project presets); reusable wrappers. Include sampler/SF2 within internal instrument rack; standalone sampler wrappers are optional v1 extension. |
| PLUG-02 | Host native third-party CLAP and VST3 instruments/effects: scanning, catalog, insert, GUI or generic parameter view, automation, MIDI/audio, state save/restore, bypass, latency, missing plugin handling. No cross-OS binary compatibility promise. |
| PLUG-03 | Validate plugin lifecycle, state, timing, buses and GUI in validators and independent hosts; isolate scanning, quarantine failures, warn about in-process plugin execution and support safe-mode startup. |
| DATA-01 | Versioned portable project folder with JSON metadata and binary assets, stable IDs, migrations, relative paths, atomic metadata writes, autosave recovery, and missing-asset/plugin placeholders preserving state. |
| DATA-02 | Save/load native patches, effect presets, rack presets, mutation seeds and graph topology deterministically; native projects render equivalently after transfer across OS within numerical tolerance. |

## 8. Quality and constraints

| ID | Requirement |
|---|---|
| QUAL-01 | Real-time path bounded and free of our own allocations, deallocation, blocking, I/O and panics; variable block sizes and sample rates 44.1, 48, 88.2, 96, 176.4, 192 kHz where hardware supports. 32-bit floating-point audio internally, accurate timing. |
| QUAL-02 | Anti-aliasing, parameter smoothing, DC/NaN/Inf handling, stable feedback, sensible audition gain, master output protection; provide true bypass and unprotected offline float option explicitly. |
| QUAL-03 | Automated unit/integration, DSP numerical, serialization, engine render, plugin and GUI interaction tests; CI Windows + Ubuntu, reproducible pinned builds and native packaging. |
| QUAL-04 | Real-world responsive interface, accessible labels, DPI scaling, no unnecessary dialogs, actual manual verification on both targets and separate plugin hosts. Performance targets in TESTING.md. |

## 9. Release definition

MVP is not v1. All IDs above must pass their tests or remain explicitly open; no “implemented” checkmarks for decorative controls, placeholder algorithms, untested OS claims or merely compiled plugins. Every feature must be documented with user-facing instructions, and failures must surface actionable messages. The application name is rawDAWg; final license, legal publisher identity and signing are owner decisions. Local development artifacts may remain unsigned with clear warnings. No public distribution before owner authorization and license audit.

# rawDAWg — implementation specification

Revision 1.1 · 24 November 2026 · Ready for repository initialization

## Purpose

Build rawDAWg, a complete native desktop digital audio workstation for Windows and Ubuntu in Rust, without JUCE. The interface is permanently dark, immediately recognizable to an FL Studio user, and designed to make conventional composition easy and extreme sound experimentation almost effortless. The flagship synthesizer is also a real VST3 and CLAP instrument.

**Maximum expressiveness, minimum cognitive overhead.** The user should spend time listening, not reasoning about synthesis.

## Product identity

Product name and display brand: **rawDAWg** (case-sensitive); tagline: **Less thinking. More noise.** Repository name: `rawDAWg`; executable/Cargo binary and Ubuntu package: `rawdawg` (`rawdawg.exe` on Windows); project directory suffix: `.rawdawg`; preset exchange suffix: `.rawdawgpreset`.

## Start here

1. Extract this directory into the root of a new Git repository (or copy its contents into the intended existing repository). Keep the filenames unchanged.
2. Open Claude Code in that repository. Give it the complete text of `BUILD_PROMPT.md`. Claude Code should automatically pick up `CLAUDE.md` as repository instructions.
3. The agent starts at M0 in `ROADMAP.md`, creates implementation and evidence, and advances only when the relevant engineering gates pass. A context reset is not a reason to restart: resume using the committed `PROGRESS.md` and actual code.
4. Do not pre-create skeleton crates or install arbitrary tools just to satisfy these documents; M0 chooses compatible pinned versions and records them. Inspect diffs and hardware/host validation evidence before shipping.

These files are a specification, not a DAW implementation. No compiled application, plugins, or test results are included. Full delivery is a multi-milestone engineering project; a single agent session is not assumed to finish it.

## Document map

| File | Authority |
|---|---|
| `CLAUDE.md` | Persistent agent rules and workflow |
| `SPECIFICATION.md` | Product requirements and release scope (requirement IDs) |
| `ARCHITECTURE.md` | Runtime boundaries, thread ownership, platform and distribution decisions |
| `DSP.md` | Synthesizer, instruments, effects, mutation and signal processing contracts |
| `UI_UX.md` | Screens, default layout, gestures, visual and input conventions |
| `PLUGIN_API.md` | Native processing contract; VST3/CLAP export and host requirements |
| `PROJECT_FORMAT.md` | Versioned project, preset and persistence schemas |
| `TESTING.md` | Acceptance gates, numerical checks, manual validation and release evidence |
| `ROADMAP.md` | Ordered implementation milestones and requirements traceability |
| `BUILD_PROMPT.md` | Paste-ready initial Claude Code prompt and resume prompt |
| `REFERENCES.md` | Upstream dependency and standards references |

## Fixed decisions and implementation defaults

Fixed: Rust; native application; Windows and Ubuntu both first-class; dark only; FL Studio-inspired pattern workflow; custom DSP, no JUCE; free experimentation, real-time mouse sculpting and sound randomization; MIDI and typing keyboard; audio recording, conventional effects; VST3 and CLAP interoperability; `.deb` only for packaged Ubuntu distribution. No AppImage, Snap, Flatpak, browser version, accounts, telemetry or cloud service.

The following are specification-author defaults to make an agent implementation unambiguous, not additional user-supplied preferences: x86-64 Windows 11 and Ubuntu 24.04 LTS as minimum verified targets; 120 BPM/4/4/48 kHz initial project; configurable processing limits; directory-based project format; default in-process third-party plugin processing with separate scanner; native Windows MSI and portable directory with EXE. The product name is fixed; final application license, code-signing credentials, and legal publisher identity remain owner decisions. Name availability and trademark clearance have not been verified. No public release or remote publishing is authorized by this specification.

ASIO requires separately verified SDK/redistribution terms and actual hardware testing. A WASAPI build is the unconditional Windows baseline; the ASIO-capable build is also a full-release target but cannot be called verified until licensing and hardware checks pass. Ubuntu must have actual ALSA and PipeWire/JACK interoperability tests; do not claim all audio drivers work from compilation alone.

The term “release” in these files means the complete agreed v1 scope, not the first MVP. The agent may produce intermediate usable builds, but must not mark v1 complete before the full acceptance gates pass.

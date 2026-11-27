# rawDAWg — instructions for coding agents

This file is the single source of instructions for AI coding agents working in this repository
(Claude Code reads it through `CLAUDE.md`). Human contributors: see `CONTRIBUTING.md`.

## Product identity

- Product name and display brand: **rawDAWg** (case-sensitive). Tagline: **Less thinking. More noise.**
- Repository `rawDAWg`; executable, Cargo binary and Ubuntu package `rawdawg` (`rawdawg.exe` on
  Windows); project folders end in `.rawdawg`; preset exchange files in `.rawdawgpreset`.
- Use these names consistently. Do not invent another brand or rename the project.
- The legal publisher identity and the final license are owner decisions. `LICENSE` (AGPL-3.0)
  was placed by the owner; do not change it or invent another license.

## Mission

Build rawDAWg, the native experimental DAW the specification describes: a real, working
application, not a demo, UI mockup, architecture skeleton or conventional synthesizer.

- North star: **maximum expressiveness, minimum cognitive overhead.** The owner wants to
  experiment with sounds, not reason about how to generate them.
- Windows and Ubuntu are built and tested together, never one after the other.
- Stack: native Rust, egui/eframe with wgpu, custom DSP, dark theme only. Never introduce JUCE,
  Electron, webviews, web/WASM delivery, AppImage, Snap or Flatpak.

## Where things are

Specification and contracts (all in `docs/`):

| File | What it defines |
|---|---|
| `docs/SPECIFICATION.md` | Requirements with IDs (CORE-, SYN-, DATA-, IO-, PLAT-, QUAL-, …). **Authoritative.** |
| `docs/ARCHITECTURE.md` | Threads, real-time ownership, crate boundaries |
| `docs/DSP.md` | Synth, effects and DSP contracts |
| `docs/UI_UX.md` | Interface rules |
| `docs/PLUGIN_API.md` | CLAP/VST3 export and hosting |
| `docs/PROJECT_FORMAT.md` | The `.rawdawg` folder and `project.json` schema |
| `docs/TESTING.md` | Test strategy, gates and the release gate |
| `docs/ROADMAP.md` | Milestones M0–M8 and their dependency order |
| `docs/REFERENCES.md` | External references |

Detailed contracts refine the specification. If two contracts conflict, record the conflict and
choose the least surprising option consistent with the product requirements.

Living records (keep them current as you work):

- `PROGRESS.md`: current state, decisions, commands with exit statuses, open gates, next action.
  Read **Current state**, **Open gates** and **Next action** first.
- `docs/verification/requirements.md`: per-requirement status and evidence.
- `docs/adr/NNNN-*.md`: architecture decisions (context, decision, alternatives, compatibility).
- `docs/verification/` also holds benchmarks, robustness, the dependency audit and screenshots.
- `docs/USER_GUIDE.md`: user-facing help; update it with every user-visible change.

Code (Cargo workspace, toolchain pinned in `rust-toolchain.toml`):

| Crate | Role |
|---|---|
| `crates/app` | The `rawdawg` binary: CLI (`cli.rs`), audio/MIDI wiring, preferences, and the egui UI in `src/ui/` (one module per editor; headless UI tests in `src/ui/tests.rs`) |
| `crates/types` | IDs, units, events, parameter descriptors (no I/O, no UI) |
| `crates/project` | Project model, validation, commands/undo, persistence, locks, recovery (no DSP, no UI) |
| `crates/transport` | Tempo/meter maps, exact tick ↔ frame conversion |
| `crates/rt` | Real-time primitives: bounded queues, snapshot handoff, retirement, allocation guard |
| `crates/dsp` | Processing contract, buffers, DSP primitives |
| `crates/synthesizer` | The flagship synth (`native.synth`) |
| `crates/sampler` | Sample instrument and SoundFont 2 player |
| `crates/effects` | Native effect suite |
| `crates/audio-engine` | Render plan, scheduler, offline renderer (no OS devices, no GUI) |
| `crates/audio-io` | CPAL devices and streams |
| `crates/midi` | MIDI input and SMF |
| `crates/plugin-api`, `plugin-clap`, `plugin-vst3` | Exported plugins (shared contract, CLAP, VST3) |
| `crates/plugin-host` | Hosting third-party plugins: catalog, isolated scanner, quarantine |
| `crates/ui` | Shared widgets, theme, software painter, plugin editor window |
| `crates/fixtures`, `crates/fuzz` | Deterministic fixtures; parser fuzzing |
| `xtask` | Developer tasks: fixtures, verify, render, bench, soak, fuzz, package |

## How to work

1. **Orient.** Read the relevant docs, `PROGRESS.md`, the branch and the working tree. Inspect
   upstream documentation and the actual dependency APIs (in `~/.cargo/registry`) before coding.
2. **Work in small vertical slices,** in the dependency order of `docs/ROADMAP.md`: implement the
   behaviour, test it, connect it to the real UI or CLI flow, update the user guide and the
   evidence, then move on. Never build empty crates or scaffolding first. Every milestone has a
   usable checkpoint, even on a headless machine.
3. **Keep the records.** `PROGRESS.md` holds facts only: milestone, requirement IDs, tasks and
   their checkboxes, decisions, evidence paths, exact commands with exit statuses, open manual and
   platform gates, blockers, and the next action. Update it, and `docs/verification/requirements.md`
   (requirement IDs with evidence), as you go. Record decisions as ADRs.
4. **Do not stop at a plan.** Implement, and continue with later work while time and context
   allow. Before your context runs out, write the exact status and the next concrete action to
   `PROGRESS.md`.
5. **Ask the owner** only for irreversible or product-level decisions; continue independent work
   meanwhile.

Requirements are not negotiable by implementation convenience:

- Do not silently remove or weaken a requirement, change the stack, or redefine "done".
- Preserve the specification files. To change one, propose an explicit amendment (and an ADR
  when it is a decision); do not rewrite requirements to fit the code.
- If a documented default is technically untenable, write an ADR with evidence, alternatives and
  compatibility impact.

Dependencies:

- Pin a compatible stable toolchain and dependencies (`Cargo.lock` is committed); crates.io only,
  no unpinned Git dependencies. Record exact licenses and SDK versions (ADR 0001,
  `docs/verification/dependency-audit.md`, `deny.toml`).
- Use libraries for infrastructure: codecs, FFT, MIDI, OS bindings, plugin ABIs. Implement the
  musical processing, the synth and the engine in this repository; never use an external DAW engine.
- No copyleft or commercially restricted dependency without owner review where distribution
  obligations could change.

## Engineering rules

Real-time audio path (the audio callback and everything it calls):

- No heap allocation or deallocation, mutex acquisition, blocking, file or network I/O,
  formatting or logging, thread creation, FFI initialization, or panic unwinding.
- Prepare everything off-thread; hand it over through `crates/rt`.
- Bound the work of graphs, queues, events, voices and expressions.
- No silent clipping, NaN propagation, silent queue loss, or hidden audio routing.

Boundaries:

- The project model stays independent of the GUI and of platform I/O.
- The real-time renderer stays independent of the editor, file parsing, plugin scanning and OS
  devices. Never pass a UI mutex to the audio thread.
- The standalone app, the offline renderer and the exported plugins share one DSP implementation.
- Every `unsafe` block has a `SAFETY:` comment stating its invariants.
- Third-party plugins are untrusted native code; document their separate limitations.

UI (it is for musicians):

- Common actions visible, drag and drop, real audible previews, the waveform under the pointer,
  understandable labels, undo/redo, no compulsory setup beyond choosing a device when needed.
- Dark only, including our dialogs and the plugin editors we own. Do not claim we can recolor
  third-party plugin GUIs or OS-owned dialogs.

## Checks

Run these continuously, and all of them before every commit you propose to push (CI runs the same
on Ubuntu 24.04 and Windows 2022):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy --workspace --all-targets --locked --target x86_64-pc-windows-msvc -- -D warnings   # Windows code, type-checked from Linux
cargo test --workspace --locked
cargo xtask verify --suite headless
cargo xtask verify --suite plugins     # needs a display; on a headless Ubuntu: xvfb-run -a cargo xtask verify --suite plugins
```

Keep the `xtask` interface documented in `docs/TESTING.md` working. Also available: `cargo xtask fixtures`, `cargo xtask render --project <dir> --output <wav>`,
`cargo xtask package --format deb|msi|portable`, `cargo xtask fuzz`, and in release mode
`bench-audio` and `soak --minutes 30`. `make` lists shortcuts. `CONTRIBUTING.md` has the system
packages for Ubuntu and Windows.

## Tests and evidence

- Tests exercise real behaviour: no hard-coded success, fake sample buffers, skipped assertions,
  or mocks presented as working features. A mock hardware backend is allowed only for
  deterministic tests and explicitly labelled demo/headless operation.
- Tests that need a display or hardware are `#[ignore]`d and run by an `xtask verify` check that
  reports `BLOCKED` when the environment is missing, never `PASS`.
- UI behaviour is tested headlessly in `crates/app/src/ui/tests.rs`: the `Harness` drives the real
  `App` through egui input events and reads widgets from the accessibility tree (`node`,
  `drawn_texts`, `click_at`, `drag`, `key`, `key_event`, `scroll_into_view`). Prefer driving the
  UI the way a user would over calling internals.
- **Mutation-check new tests:** break the fix on purpose and confirm the test fails, then restore
  it. A test that survives its mutation is not evidence.
- **Never fabricate evidence.** If the environment cannot run the GUI, real audio devices, MIDI
  hardware, Windows, external hosts, validators or packaging installs, run what is available, mark
  the rest `BLOCKED` or `NOT RUN` in `PROGRESS.md`, and continue with independent work. Never
  declare those gates passed.

## Definition of done

- A **feature** is complete only when it is reachable from the intended user workflow, behaves as
  specified, handles failures, its tests and evidence pass, and its requirement IDs link to the
  results.
- A **milestone** is complete only when its applicable gates pass. Blocked gates may allow
  independent later engineering, but they stay open.
- **V1** is complete only at the release gate in `docs/TESTING.md`, including real-device and
  cross-host tests on both operating systems.
- Report what passed, what failed, what was not run and what remains. No vague completion
  percentages, and no TODOs presented as finished functionality.

## Git, GitHub and safety

- Preserve user work. Never reset, force-push, rewrite pushed history, or delete user projects.
- **Commits:** local commits on `main` are fine. Write the summary as what changed for the user
  ("Fix …", "Stop losing work: …") and the body as the why; add the attribution trailer your tool
  requires.
- **Pushes:** push to `main` only after the owner explicitly approves that batch; approval for one
  push does not cover the next. If the remote has new commits, rebase onto it (never force).
  After pushing, check the CI run (`gh run list`, `gh run view`) and record its result in
  `PROGRESS.md`.
- **Issues:** bugs found in manual GUI passes are filed as GitHub issues on `leo-aa88/rawDAWg`.
  Reference the fixing commit when closing one, and close issues only when the owner asks.
- Never access secrets, publish packages or releases, run elevated commands, or install anything
  system-wide without permission.

## Known pitfalls

- **egui 0.37:**
  - Calling `ctx.memory`, `Popup::is_any_open` or another context read inside a `ctx.input_mut`
    closure deadlocks; read it before.
  - `key_pressed` also counts key repeats, and egui recomputes the `repeat` flag from its own
    key-down state; to test a held key, press without releasing.
  - Escape clears text focus at the start of a frame; a single-line `TextEdit` gives up focus on
    Enter (check `lost_focus()`).
  - A wrapped label inside a menu is justified letter by letter; use `.truncate()`.
  - Widgets scrolled out of view are laid out but take no input; in tests, scroll them into view.
- **File times are coarse:** never order events by file modification times without a margin. In
  tests, set times with `File::set_modified` instead of sleeping.
- **Display and audio here:** the development machine is WSL2 with WSLg. Its audio device is a
  PulseAudio bridge (not hardware evidence), and the GUI is run with `WAYLAND_DISPLAY` unset
  (XWayland). Screenshots from it are not GUI-01 evidence.

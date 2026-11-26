## What this changes

<!-- One or two sentences. Link the issue if there is one: "Closes #123". -->

## Why

<!-- The problem or requirement (IDs from docs/SPECIFICATION.md, for example PLUG-02). -->

## How it was tested

<!-- Paste the commands you ran and their actual output. Say what you could not run. -->

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [ ] `cargo test --workspace --locked`
- [ ] `cargo xtask verify --suite headless` and `--suite plugins`, if the engine, files, plugins or packaging changed
- [ ] Windows type-check (`--target x86_64-pc-windows-msvc`), if platform code changed
- [ ] Listened / looked: describe what you checked by ear or on screen, on which OS and audio device

## Not run

<!-- Hardware, display, Windows, independent hosts or validators you could not check. Write
"none" otherwise. These stay open gates; never mark them passed without running them. -->

## Checklist

- [ ] Nothing new on the audio thread allocates, locks, blocks, does I/O, logs or can panic.
- [ ] Every new `unsafe` block has a `SAFETY:` comment stating its invariants.
- [ ] Tests exercise the real behaviour (no hard-coded success, fake buffers or skipped assertions).
- [ ] `docs/USER_GUIDE.md`, `PROGRESS.md` and `docs/verification/requirements.md` are updated where behaviour changed; decisions have an ADR.
- [ ] New dependencies are from crates.io, pinned, and pass `cargo deny check`.
- [ ] No copyrighted material that is not ours (sample libraries, plugins, other people's music) is added.

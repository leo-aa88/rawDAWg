# Contributing to rawDAWg

Thanks for your interest. rawDAWg is a native experimental DAW with one goal: **maximum
expressiveness, minimum cognitive overhead**. Correct audio, honest evidence and a calm real-time
thread matter more than feature count. Please read this page before opening an issue or a pull
request.

## Ground rules

- **The specification is the contract.** `docs/SPECIFICATION.md` (requirement IDs) and the other
  documents in `docs/` define what rawDAWg is. Do not silently weaken or remove a requirement to
  fit the code: propose an amendment, or record a decision in `docs/adr/` with evidence,
  alternatives and compatibility impact.
- **Stack.** Native Rust, egui/wgpu, custom DSP, dark theme only, Windows and Ubuntu. Changes
  that introduce JUCE, Electron, webviews, web/WASM delivery, AppImage, Snap or Flatpak will not
  be accepted. Musical processing, the synth and the engine are implemented in this repository;
  libraries are fine for infrastructure (codecs, FFT, MIDI, OS bindings, plugin ABIs).
- **Real-time safety.** On the audio thread: no allocation or deallocation, no locks, no blocking,
  no file or network I/O, no formatting or logging, no thread creation, no panics. Prepare
  everything off-thread. No silent clipping, NaN propagation, queue loss or hidden routing.
- **`unsafe` is documented.** Every `unsafe` block states its invariants in a `SAFETY:` comment.
- **Tests exercise real behaviour.** No hard-coded success, fake sample buffers, skipped
  assertions or mocks presented as working features. A test that needs hardware or a display is
  `#[ignore]`d and run by a verification check that reports `BLOCKED` when the environment is
  missing, never `PASS`.
- **Never fabricate evidence.** Report what passed, what failed and what was not run. Platform,
  hardware, GUI and independent-host gates stay open until they have really been checked.
- **Dependencies.** crates.io only, pinned in `Cargo.lock`, checked by `cargo deny` against
  `deny.toml`. Copyleft or commercially restricted licences need the maintainer's review first.
- Be respectful. This project follows the [Code of Conduct](CODE_OF_CONDUCT.md).

## Reporting issues

Use the issue forms: **Bug report**, **Feature request**, **Question** or **Plugin
compatibility**. Security problems go through private reporting, not public issues (see
[SECURITY.md](SECURITY.md)). Please do not attach copyrighted material you do not own (commercial
sample libraries, other people's projects or recordings).

## Development setup

Requirements: the toolchain pinned in `rust-toolchain.toml` (installed automatically by
`rustup`). On Ubuntu 24.04:

```bash
sudo apt install build-essential pkg-config libasound2-dev libjack-jackd2-dev libudev-dev \
  libxkbcommon-dev libwayland-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
  libgtk-3-dev lintian xvfb
```

On Windows: the MSVC Build Tools, and the WiX v5 CLI for the MSI (`dotnet tool install --global
wix --version 5.0.2`).

The checks CI runs, in order:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --release --locked
cargo xtask fixtures
cargo xtask verify --suite headless
cargo xtask verify --suite plugins          # on Ubuntu without a desktop: xvfb-run -a cargo xtask verify --suite plugins
cargo xtask render --project fixtures/demo.rawdawg --output target/verification/demo.wav
cargo xtask package --format deb            # Ubuntu; --format msi / --format portable on Windows
```

With GNU Make, `make` lists shortcuts (`make test`, `make release`, `make ci`, `make run`).

Platform code should also be type-checked for the other OS. From Linux:

```bash
rustup target add x86_64-pc-windows-msvc
cargo clippy --workspace --all-targets --locked --target x86_64-pc-windows-msvc -- -D warnings
```

Longer runs, when your change touches them: `cargo xtask fuzz` (parsers),
`cargo run --release -p xtask -- bench-audio` (performance) and
`cargo run --release -p xtask -- soak --minutes 30` (stability).

## Pull requests

- Keep a pull request to one vertical slice: the behaviour, its tests, its place in the UI or
  CLI, and the user-facing documentation (`docs/USER_GUIDE.md`) together.
- Fill in the pull request template, including the commands you ran and their actual output.
- CI must be green on Ubuntu and Windows.
- Update `PROGRESS.md` and `docs/verification/requirements.md` with the evidence for the
  requirement IDs you touched; add an ADR under `docs/adr/` for decisions.
- Commit messages say what changed and why, in the imperative or as a summary of the change.

## Releases

Releases are cut by the maintainer with the **Release** workflow (Actions → Release → Run
workflow, choosing a semver bump) or by pushing a `vX.Y.Z` tag. The workflow only releases a
commit that passed CI, stamps the version from the tag, builds and smoke-tests the Ubuntu `.deb`
and the Windows MSI and portable zip, and publishes them with checksums. It never commits to
`main`.

## License

The project is distributed under the terms in [LICENSE](LICENSE). Contributions are accepted under
the same terms.

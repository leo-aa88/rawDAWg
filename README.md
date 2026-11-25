# rawDAWg

**Less thinking. More noise.**

rawDAWg is a native experimental digital audio workstation for Windows and Ubuntu, written in Rust (egui/wgpu, custom DSP, CPAL audio, CLAP/VST3 export and hosting). The product and engineering contract lives in [`docs/`](docs/README.md); progress and open gates are tracked in [`PROGRESS.md`](PROGRESS.md).

> Status: **intermediate build (M0 complete, M1 in progress)** — playable default synth with typing keyboard, real-time engine, headless render, CLAP export of the synth, VST3 ABI fixture. Not a release.

## Build

Pinned toolchain (`rust-toolchain.toml`, Rust 1.99.0). Ubuntu prerequisites: `libasound2-dev libxkbcommon-dev libwayland-dev pkg-config build-essential` (plus `libjack-jackd2-dev` for `--features jack`). Windows: MSVC Build Tools.

```sh
cargo build --workspace --release --locked
cargo test --workspace --locked
cargo xtask fixtures
cargo xtask verify --suite headless
cargo run --release -p rawdawg
```

See [`docs/USER_GUIDE.md`](docs/USER_GUIDE.md) for the CLI and keyboard map, [`docs/TESTING.md`](docs/TESTING.md) for acceptance gates and [`docs/verification/requirements.md`](docs/verification/requirements.md) for the live evidence table.

## License

The repository carries the `LICENSE` file placed by the owner (AGPL-3.0). The final application license, legal publisher identity and signing are owner decisions; no public distribution is authorized by this repository state.

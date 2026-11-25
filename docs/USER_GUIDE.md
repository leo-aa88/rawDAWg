# rawDAWg — User guide (intermediate build, M0/M1)

**rawDAWg** — *Less thinking. More noise.* This guide covers what the current build actually does. Features listed in the specification but not yet implemented are marked as such inside the app.

## Starting

```sh
cargo run --release -p rawdawg            # GUI with the default audio device
cargo run --release -p rawdawg -- --help  # all options
```

Options:

| Flag | Effect |
|---|---|
| `--open <PATH>` | Open a project folder (`Something.rawdawg`) or its `project.json`. |
| `--no-audio` | Start without opening an audio device (editing and offline render still work). |
| `--device <ID>` / `--buffer <FRAMES>` | Choose the output device (ids from `--list-audio-devices`) and request a buffer size. The status bar shows the actual negotiated configuration. |
| `--safe-mode` | Start without loading third-party plugins (placeholders preserve project state). Third-party hosting arrives in M7; the flag is honored and shown. |
| `--list-audio-devices` | Print hosts and devices as JSON and exit. |
| `--audio-smoke` | Open the output device, play a 1 s A4 from the synth, print callback/xrun statistics and exit. |
| `--headless-render <PROJECT> --output <FILE.wav>` | Render without GUI or audio device. Options: `--mode pattern|song`, `--format pcm16|pcm24|float32`, `--tail <s>`, `--sample-rate <Hz>`, `--start-tick`, `--end-tick`, `--unprotected` (bypass master protection). |

Exit codes: 0 ok, 2 usage/IO error, 3 render reported engine health counters, 4 no usable device, 5 device opened but audio did not flow.

## Playing

The window opens with a playable **rawDAWg Synth** channel. Hold keys on the **typing keyboard** (toggle in the toolbar):

```
 S D   G H J   L ;      ← black keys
Z X C V B N M , . /     ← white keys: C D E F G A B C D E
```

`Octave −/+` shifts the base octave (default C4), `Velocity` sets note velocity. Keys are ignored while a text field has focus or when Ctrl/Alt is held; releasing focus releases the notes. **Panic** sends all-notes-off.

Transport: **Play** runs the selected pattern in a loop (pattern mode), **Stop** returns to the start. The status bar shows master meters, limiter activity, voice count, device, sample rate, CPU load and xruns.

## Projects

A project is a folder ending in `.rawdawg` containing `project.json`, `assets/`, `plugin-state/`, `recovery/` and `cache/`. Saving is atomic: the previous `project.json` is kept in `recovery/project.previous.json`. The fixture project `fixtures/demo.rawdawg` renders a one-bar motif:

```sh
cargo xtask fixtures
cargo xtask render --project fixtures/demo.rawdawg --output target/verification/demo.wav
```

## Plugins

The build produces real plugin binaries in `target/<profile>/`:

- `librawdawg_clap.so` / `rawdawg_clap.dll` — CLAP, id `rawdawg.synth`, display name **rawDAWg Synth** (notes in, stereo out, parameters, state). Rename/copy to `rawDAWg.clap` for hosts that require the extension.
- `librawdawg_vst3.so` / `rawdawg_vst3.dll` — VST3 module exporting **rawDAWg Gain** (M0 ABI fixture; the synth and effects follow in M7, together with the `.vst3` bundle layout).

Validation with `clap-validator`, the VST3 validator and independent hosts is tracked as open gates in `PROGRESS.md`.

## Troubleshooting

- **"Audio unavailable"** in the status bar: no output device could be opened. Check `--list-audio-devices`, then **Retry audio**. Everything except playback keeps working.
- **xruns** increasing: raise the buffer (`--buffer 512`) or reduce polyphony (synth `Voices` parameter).
- On Ubuntu, ALSA is the baseline; JACK and PipeWire backends are compile-time features (`--features jack` / `pipewire`) that need the corresponding development packages.

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

### Modular graph (Advanced)

Tick **Advanced** in the synth panel and open **Modular graph**. The **voice graph** runs once per note between the layers and the filter/amp stage; the **global graph** runs once on the summed voices before the master gain. Both start as **Off** (the sound passes straight through) — press **Start graph** to get the fixed in/out nodes connected directly, then **+ Add node** (type to search). Drag from an output dot to an input dot to connect; colours mark the port type (teal audio, blue stereo, yellow control, pink event) and a mono output may feed a stereo input. Drag titles to move nodes, right-click a node or the small handle on a connection to delete it, scroll to pan and Ctrl+scroll to zoom. Every change is applied and audible immediately and is one undo step. If a change is not valid (a type mismatch, an instantaneous cycle, a budget overrun or an expression error) the editor shows the reason in red, highlights the offending edge or nodes and keeps the last valid graph playing. Feedback is only possible through a **Delay (feedback)** node, which reads at least 64 samples behind and limits the loop gain; such connections are drawn in orange. The **Expression** node evaluates a bounded formula per sample (`x`/`in1..in4` audio inputs, `ctrl1..ctrl4` control inputs or the node's own values, `t`, `phase`, `sr`; `+ - * /`, comparisons, `min max abs sin cos tanh exp log clamp select`), limited to 256 instructions; invalid divisions or logarithms give a finite value and are counted in the status line. The fixture project `fixtures/demo-graph.rawdawg` shows a complete example.

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

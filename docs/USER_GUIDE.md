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

## Sequencing

**Pattern / Song** in the second toolbar row chooses what Play runs: the selected pattern in a loop, or the playlist. **Loop** repeats the pattern, the loop range or the song. The pattern selector creates (`+`), renames and resizes patterns (`…`: 1/2/4/8 bars, step size 1/4…1/32).

**Channel rack (F6)**: each channel shows step buttons for the selected pattern. Click toggles a step, drag paints, right-click sets the step's velocity and pitch. Notes that are not on the grid show an orange marker and a count; they are never moved by the step view.

**Piano roll (F7)**: click in the grid to draw a note (drag while drawing to set its length), drag a note to move it, drag its right edge to resize, right-click to delete, right-drag to erase. Shift adds to the selection, Ctrl+drag (or the Select tool) box-selects. Delete removes, Ctrl+D duplicates after the selection, Ctrl+C/X/V copy/cut/paste at the playhead, arrows nudge by the grid or a semitone (Shift: octave), Escape cancels a gesture. Snap offers bar, beat, 1/2…1/32 and triplets (Alt bypasses). **Chord** inserts chords at the pointer, **Scale** highlights a scale and **Scale snap** keeps notes on it, **Quantize…** moves the selected (or all) notes toward the grid with a strength slider and an audible preview; nothing changes until you press Apply. The velocity lane below the grid draws or ramps velocities; an expression lane appears when the lane holds recorded pitch bend, CCs or sustain.

**Playlist**: `+ Track` adds a track; click in a lane to place the selected pattern (drag to paint repeats), drag clips to move them (Ctrl copies), drag their edges to trim/resize, S or "Split at playhead" splits, "Make unique" clones the pattern so later edits stay local, right-click a clip for mute/loop/make unique/open/split/delete. Drag on the ruler to set the loop (and export) range; click it to seek. Tempo and time-signature markers above the lanes are editable (click) and addable (right-click the marker row).

**Recording**: arm **Rec**, choose a count-in (0–2 bars) and press Play or Space. Everything you play on the typing keyboard or a MIDI device while the transport runs is recorded into the selected pattern (looping overdubs on every pass); in Song mode a new "Take" pattern is placed as a clip where you started. Raw timing is kept; the recorded notes are selected afterwards so **Quantize…** can snap them when you want. The ms offset compensates input latency.

**Automation**: right-click any parameter → "Create automation clip". Curves live on an Automation track and play in Song mode; the Automation view edits points (click adds, drag moves, right-click deletes; per point Linear or Step) in the parameter's own units. Modes: Read plays, Touch writes while you hold a control during playback, Write latches the last value until you stop. The morph pad's "Write gesture as automation" bakes a recorded XY movement into curves. **MIDI learn** is in the same context menu: pick it, move a controller, done.

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

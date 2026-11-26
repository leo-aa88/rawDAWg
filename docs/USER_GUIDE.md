# rawDAWg — User guide

**rawDAWg** — *Less thinking. More noise.* This guide describes what the current build (0.1.0, release candidate work in progress) actually does. Open gates and known limits are listed in `PROGRESS.md`.

## Quickstart (ten minutes)

1. **Install and start.** Install the package for your system (see **Installing** below) or build from source, then start rawDAWg. It opens a new project with one playable **rawDAWg Synth** channel. If no audio device can be opened, the status bar says **Audio unavailable**: everything except playback still works.
2. **Play.** Hold `Z X C V B N M` (white keys) and `S D G H J` (black keys) on your computer keyboard, or play a MIDI keyboard after enabling it in **Settings › MIDI input**.
3. **Find a sound.** In the synth panel, click a preset to hear it and double-click to load it, or press **New generation from current** in the evolve section to hear nine variations of the current sound. Undo (Ctrl+Z) always takes a change back.
4. **Make a pattern.** Open the **Channel rack** (F6) and click steps, or the **Piano roll** (F7) and draw notes. Press **Play** (Space): pattern mode loops the selected pattern.
5. **Add effects.** Open the **Mixer** (F9), pick the channel's strip, press **+ Add effect** and type a name (try *Reverb* or *Delay*). Click the effect to open its editor.
6. **Arrange.** Switch to **Song**, add a playlist track with **+ Track**, and click in its lane to place the pattern. Drop an audio file on a lane to use it as an audio clip.
7. **Save and export.** **Ctrl+S** saves the project (a folder ending in `.rawdawg`). **File › Export…** renders WAV or FLAC, with optional stems.

## Starting

```sh
rawdawg                          # installed package: GUI with the default audio device
cargo run --release -p rawdawg   # from source
rawdawg --help                   # all options
```

Options:

| Flag | Effect |
|---|---|
| `--open <PATH>` | Open a project folder (`Something.rawdawg`), its `project.json` or its `project.rawdawg` marker; or import a `.rawdawgpreset` into a new project. |
| `--no-audio` | Start without opening an audio device (editing and offline render still work). |
| `--device <ID>` / `--buffer <FRAMES>` | Choose the output device (ids from `--list-audio-devices`) and request a buffer size. The status bar shows the actual negotiated configuration. |
| `--safe-mode` | Start without loading any third-party plugin; projects that use them keep their state in placeholders. |
| `--scan-plugins` | Scan the plugin folders into the catalog (each module in its own process) and print a JSON report. |
| `--list-audio-devices` | Print hosts and devices as JSON and exit. |
| `--audio-smoke` / `--capture-smoke` | Open the output (play a 1 s A4) or the input (capture 1 s), print the statistics and exit. |
| `--headless-render <PROJECT> --output <FILE>` | Render without GUI or audio device to `.wav` or `.flac`. Options: `--mode pattern|song`, `--format pcm16|pcm24|float32|flac16|flac24`, `--stems <DIR>`, `--tail <s>`, `--sample-rate <Hz>`, `--start-tick`, `--end-tick`, `--unprotected` (bypass master protection). |

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

**Browser**: the search box filters samples and effects; mark anything *+fav* to keep it at the top. *play* auditions a sample at a safe level through the master (audio must be on); clicking an effect adds it to the mixer channel selected in the Mixer view.

**Audio clips**: drop a `.wav`, `.flac` or `.mp3` onto a playlist lane and it becomes an audio clip right there (dropped anywhere else it becomes a sampler channel instead). Samples already in the project are listed in the browser: click one, then click a lane to place it (Esc cancels). Audio clips play in Song mode into the mixer channel chosen in the track header (Master by default), so inserts and sends apply to them like to anything else. Drag the right edge to shorten a clip, the left edge to trim its start; right-click it for gain and fade in/out. Tempo changes move clips but never stretch them.

## Mixer and effects

**Mixer (F9)**: the master is pinned on the left, other channels scroll. Each strip has mute, solo, pan, a fader with a dB readout (double-click resets), stereo meters with peak hold and a red clip bar (click to reset) and an output selector; `+ Bus` adds a channel. Pick a strip to edit its **inserts** (`+ Add effect`, then search; bypass, wet/dry, up/down, remove; the reported latency is shown and compensated automatically so parallel paths stay aligned) and its **sends** (target, level, pre/post-fader, sidechain). Every channel in the rack chooses its mixer channel next to its name. The output latency created by effects is shown at the top of the mixer.

**Effects**: click an insert to open its editor. Each effect has presets, On/Bypassed, a Reset button, simple controls and an Advanced numeric view; right-click any control for MIDI learn or an automation clip. The **Parametric EQ** shows its response: click the plot to add a bell, drag a band to change frequency and gain, use the wheel over a band for Q, right-click a band to remove it, and choose each band's type below. The **Stereo** effect handles gain, pan, balance, width (mid/side), mono, channel swap and phase, and shows the stereo correlation. **Compressor**, **Limiter** and **Gate** show their gain reduction; the compressor and gate can listen to a sidechain send (enable "External sidechain" and send another channel to this bus with the sidechain switch on), and the limiter's true-peak mode watches inter-sample peaks (its lookahead is reported as latency and compensated). The **Delay** offers stereo, ping-pong, tape (wow and drive), multitap and reverse modes with a time in ms or synced to the tempo; in multitap mode drag the taps on the canvas (wheel pans, right-click toggles a tap), and reverse mode reports its segment length as latency. The **Reverb** has room, hall, cathedral, plate, spring and shimmer types; its decay-shape curve lets you drag each frequency band to ring longer or shorter than the decay time. **Convolution** uses an impulse response from your project assets (pick one, or type a path and press Import IR); long responses cost CPU. **Distortion** has tube (turn up Bias for an asymmetric, even-harmonic grit), tape, soft clip, hard clip, fold and bitcrush modes; raise Oversampling to tame aliasing on the clip and fold modes (the bitcrusher aliases on purpose: lower Bit depth and raise Rate divider for lo-fi). **Per-band distortion** splits the signal into four bands: drag across the Drive, Mix or Gain row on the canvas to paint each band (one undo step per stroke) and move the split frequencies below; untouched bands pass through unchanged. **Chorus**, **Flanger** and **Phaser** share a rate (or a tempo-synced division), sine/triangle shape and a stereo spread: the chorus layers up to four delayed voices, the flanger's feedback can go negative for a hollower comb, and the phaser sweeps 2 to 12 stages around its centre frequency. **Pitch / formant** shifts the pitch by semitones and cents without changing the length, moves the formants on their own (Chipmunk/Giant), or keeps them in place while the pitch moves (Preserve formants); it adds about 43 ms of latency, which the mixer compensates. **Spectral** works on the sound's spectrum: Freeze holds what is playing right now for as long as you like, Blur smears it over time, the gate keeps only the loudest partials, Tilt brightens or darkens per octave, Frequency shift slides every partial by the same number of hertz (inharmonic), and Robotise throws the phases away. **Granular** cuts the last two seconds of audio into grains: set their size and how many per second, how far back they are taken from (Position) and how much that wanders (Jitter), shift or scatter their pitch, play some backwards, spread them across the stereo field, and Freeze the buffer to keep playing what was captured.

**Presets**: every effect editor has *Preset file…* to save its settings as a `.rawdawgpreset` or load one; the mixer's *Rack preset…* saves a channel's whole insert chain and loads it onto any channel. Plugins from other vendors that are not available on this machine stay in the project as silent (instruments) or pass-through (effects) placeholders with their settings intact.

**Rack tools** (mixer, below the strips): **Randomize rack** changes every effect on the selected channel at once, as far as the **Chaos** slider allows; the **Locks** menu protects categories (wet/dry and output levels are locked by default), and one Undo takes the whole change back. **Morph**: store the rack's settings in corners A–D, then drag on the pad to blend between them (a drag is one undo step). **+ Parallel branch** makes a bus that receives a copy of the channel and sums back into the same output, so you can build a parallel effect path; the mixer keeps both paths aligned even when one of them adds latency. To move an effect parameter rhythmically, right-click it and pick **Modulate with an LFO clip**: a sine, triangle, square or saw automation clip appears, which you can edit like any other automation. Soloing a channel keeps the channels that feed its sidechain running (they stay out of the mix but still key the compressor).

## Recording audio

Open **Settings › Input (recording)**, tick *Open an input device*, choose the device and whether you record one input (mono) or a pair, then Apply. Press **R** in a playlist track's header to arm it, switch on **Rec**, and press Play in Song mode: the input is recorded from the moment the transport runs (after the count-in) and placed on that track as an audio clip when you stop, lined up with what you heard using the reported latencies plus the *Recording offset* (record the metronome once through a cable or a microphone and set the offset so the clicks land on the beats). Monitoring the input through the master is off by default; do not switch it on while your interface already monitors directly, and keep speakers away from microphones. If rawDAWg stops unexpectedly during a take, the next open rebuilds the unfinished recording onto a “Recovered” track.

## Export and MIDI files

**File › Export…** renders the song (or the selected pattern) with the same engine you hear: pick WAV (16/24-bit or float) or FLAC (16/24-bit), the sample rate, a tail, and tick **Stems** for one file per mixer channel next to the mix. The export runs in the background with a progress bar and a Cancel button; when it is done you see the length, the peak and how many samples clipped (lower the master or export float if that number is not zero). Nothing is normalized. **Export MIDI (type 1)** writes the arrangement as a Standard MIDI File. Dropping a `.mid` file on the window imports it: every MIDI track becomes a synth channel, the notes become a pattern placed on a new playlist track, and an empty project also takes the file's tempo and meter.

## Projects

A project is a folder ending in `.rawdawg` with `project.json` and the media you imported (copied in, named by content). **File › New…** starts an empty project in a working folder; **Save As…** writes a complete, independent copy to the folder you type (assets included, the previous folder stays as it was); **Open…** takes a folder or its `project.json` and lists recent projects; **Ctrl+S** saves in place. While you have unsaved changes rawDAWg autosaves every two minutes into the project's `recovery` folder; if it ever stops unexpectedly, the next open offers to recover those changes (or keep the saved version, or discard the autosaves). A project opened in two places at once is read-only in the second until you take over the lock. If a sample file is missing from the folder the browser says so and lets you relink it: the same file restores it, a different file replaces it everywhere (undoable). Closing with unsaved changes asks whether to save.

## Plugins

**Third-party CLAP and VST3 plugins.** Open **Settings › Plugins** and press **Scan plugin folders**. rawDAWg looks in the standard locations (CLAP: `~/.clap`, `/usr/lib/clap`, `/usr/local/lib/clap` and `CLAP_PATH` on Ubuntu, `%COMMONPROGRAMFILES%\CLAP` and `%LOCALAPPDATA%\Programs\Common\CLAP` on Windows; VST3: `~/.vst3`, `/usr/lib/vst3`, `/usr/local/lib/vst3`, `%COMMONPROGRAMFILES%\VST3`, `%LOCALAPPDATA%\Programs\Common\VST3`) plus the extra folders you add there. VST3 plugins are found as `.vst3` bundles or bare modules; only VST3 plugins that accept stereo 32-bit float buses are hosted (others stay stand-ins with the reason shown). Every module is examined in a **separate scanner process** with a timeout (15 s by default, adjustable): a module that crashes, hangs or fails to load is **quarantined** and listed with the reason; **retry** scans it again next time, **remove** forgets it. The catalog lives in your preferences folder (`plugins.json`), not in projects.

Scanned plugins appear in the browser under **Plugins** (search by name, vendor or id) and in the mixer's **+ Add effect** list with a `(CLAP)` or `(VST3)` suffix. Clicking an effect adds it as an insert on the selected mixer channel; clicking an instrument replaces the selected channel's instrument (undo restores the old one). The effect/instrument panel shows the plugin's parameters in rawDAWg's generic editor, with the plugin's own value text (for example `322.2 ms` or `-6.0 dB`, as the plugin formats it) and a named list for parameters with a few choices (up to 64); automation lanes show the same text. **Bypass** keeps the chain's latency aligned, automation and MIDI learn work as for native effects. A plugin's own state is saved inside the project (larger states as files in the project's `plugin-state/` folder; keep that folder with the project); a project that references a plugin you do not have opens with a **stand-in** (effects pass audio through, instruments stay silent) and keeps the state untouched until the plugin is available again.

Third-party plugins run inside rawDAWg as native code: the scanner isolation is crash containment for discovery, not a sandbox. If rawDAWg ends without a clean shutdown while plugins were loaded, the next start says so in Settings › Plugins; restart with `--safe-mode` to open projects without loading any third-party plugin (they are listed but never instantiated). Plugin blocks that produce errors or non-finite output are muted and counted in Settings › Plugins. Plugin GUIs are not available yet (parameters are edited in rawDAWg's generic panel).

`rawdawg --scan-plugins [--catalog FILE]` runs the same scan from the command line and prints a JSON report.

**rawDAWg as a plugin.** The build produces real plugin binaries in `target/<profile>/`:

- `librawdawg_clap.so` / `rawdawg_clap.dll` — one CLAP module containing **rawDAWg Synth** (`rawdawg.synth`: notes in, stereo out) and every effect family as its own plugin (`rawdawg.fx.eq`, `rawdawg.fx.compressor`, `rawdawg.fx.limiter`, `rawdawg.fx.gate`, `rawdawg.fx.distortion`, `rawdawg.fx.multiband`, `rawdawg.fx.delay`, `rawdawg.fx.chorus`, `rawdawg.fx.flanger`, `rawdawg.fx.phaser`, `rawdawg.fx.pitch`, `rawdawg.fx.granular`, `rawdawg.fx.spectral`, `rawdawg.fx.reverb`, `rawdawg.fx.convolution`, `rawdawg.fx.stereo`; display names **rawDAWg Parametric EQ**, **rawDAWg Reverb**, …). Effects take stereo in/out, report latency and tail, follow the host tempo, and add **Bypass** and **Mix** parameters. Parameters with whole-number steps (modes, sync divisions, voice counts, lookahead) are published as those steps, so every choice is reachable in other hosts; the rest are 0–1 values with rawDAWg's readout text; the compressor and gate expose a stereo **Sidechain** input. Rename/copy the module to `rawDAWg.clap` for hosts that require the extension. Effect presets saved from the app load as plugin state. The convolution plugin has no impulse response outside a rawDAWg project yet (it passes the dry signal).
- `librawdawg_vst3.so` / `rawdawg_vst3.dll` — the same 17 plugins as a VST3 module (stable class ids recorded in `crates/plugin-api`): stereo main in/out, aux **Sidechain** bus for the compressor and gate, event bus for the synth, sample-accurate parameter queues, process context (tempo, transport, time signature), latency and tail, the same **Bypass**/**Mix** wrapper parameters and the same state envelope as the CLAP version (a preset saved in one format loads in the other). `cargo xtask verify` lays it out as `target/verification/bundles/rawDAWg.vst3/Contents/<arch>/` with a `moduleinfo.json`; copy that folder into your VST3 directory. No custom editor yet: hosts show their generic parameter view.

Validation with `clap-validator`, the VST3 validator and independent hosts is tracked as open gates in `PROGRESS.md`.

## Installing

- **Ubuntu 24.04**: `sudo dpkg -i rawdawg-<version>-x86_64.deb` (dependencies: libasound2, glibc). Installs `rawdawg`, a desktop entry that opens projects (double-click the `project.rawdawg` file inside a project folder) and `.rawdawgpreset` presets (double-click imports the sound into a new project), and the rawDAWg CLAP/VST3 plugins into `/usr/lib/clap` and `/usr/lib/vst3` for other hosts. `sudo apt remove rawdawg` keeps your projects and preferences.
- **Windows 11**: run `rawdawg-<version>-x86_64.msi` (per-machine, unsigned: Windows will warn), or unzip the portable build and run `rawdawg.exe` from the folder. The installer registers `.rawdawg` (the `project.rawdawg` marker in each project folder) and `.rawdawgpreset`, and puts the plugins into the common CLAP/VST3 folders.
- **From source**: see `README.md` (pinned toolchain, `cargo build --workspace --release --locked`).

A project is a folder (`Song.rawdawg/`); every save writes a small `project.rawdawg` marker into it, and that file is what the desktop opens. **File › Open**, the recent-projects list and `rawdawg --open <folder>` work too. rawDAWg works offline; nothing phones home.

## Help

**Help › User guide (offline)** shows this guide inside the app.

## Troubleshooting

- **"Audio unavailable"** in the status bar: no output device could be opened. Check `rawdawg --list-audio-devices`, pick a device in **Settings › Audio**, then **Apply** (or **Start audio** in the status bar). Everything except playback keeps working.
- **Crackles or rising xrun count**: raise the buffer (**Settings › Audio**, or `--buffer 512`), lower the synth's voice count, or bypass heavy effects (Reverb, Spectral and Pitch cost the most; Convolution with a long impulse response too). Under WSL or a virtual machine, occasional slow blocks come from the environment as well.
- **No sound from a MIDI keyboard**: enable the device in **Settings › MIDI input** (on Ubuntu the user must be allowed to open ALSA sequencer devices; under WSL there is usually no MIDI at all). The status bar's MIDI counter shows how many messages arrive.
- **Notes ring on after Stop**: long synth releases keep sounding until they fade; **Panic** silences everything at once.
- **rawDAWg closed unexpectedly**: reopen the project; if autosaved changes are newer than the saved file, rawDAWg offers to recover them. An unfinished audio take is rebuilt onto a “Recovered” track. If third-party plugins were loaded at the time, **Settings › Plugins** says which; start with `--safe-mode` if the crash repeats.
- **"Project is open elsewhere" / read-only**: the project is locked by another rawDAWg window (or by one that crashed). Close the other window, or take over the lock when asked.
- **A sample is missing**: the browser marks it; **Relink** and pick the same file (or a replacement, applied everywhere and undoable). The clip or sampler stays silent until then.
- **A plugin shows as a stand-in**: it is not in the catalog, quarantined, or you started in safe mode. The reason is in the status line and the panel; scan your plugin folders in **Settings › Plugins**. Its state stays in the project untouched. If the reason says its *saved state file is missing or damaged*, the file in the project's `plugin-state/` folder is gone or changed (for example the project was copied without that folder): put the folder back and reopen; rawDAWg never replaces the saved state with defaults. The same applies when a plugin cannot restore its saved state.
- **A plugin module is quarantined**: it crashed, hung or did not load during the scan. **retry** scans it again next time; nothing from it is loaded until then.
- **Double-clicking a project folder does not open it**: folders cannot be associated with an app; open the `project.rawdawg` file inside the folder instead (installed packages register it), or use **File › Open…**.
- On Ubuntu, ALSA is the baseline; JACK and PipeWire backends are compile-time features (`--features jack` / `pipewire`) that need the corresponding development packages.

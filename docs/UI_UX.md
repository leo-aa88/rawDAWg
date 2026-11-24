# rawDAWg — UI and interaction specification

## Brand

Use the exact wordmark `rawDAWg`, optionally highlighting `DAW` in the existing accent color. Tagline: “Less thinking. More noise.” Use the name in the window title, About dialog, first-party plugin editors and project metadata surfaces where appropriate. Original, simple waveform-style icon; dark-only UI remains mandatory. Keep branding unobtrusive and never let it displace musical controls. The proposed wordmark treatment is a design default, not a required final logo asset.

## 1. Non-negotiable principles

Dark only. Familiar FL Studio-style workflow, original visual assets. Visible, self-explanatory controls, minimal technical jargon in normal use, no setup wizard or empty modular graph before sound can be heard. Advanced functionality is progressively disclosed, not removed. Every visible action must work; do not show inert controls as if implemented. Every parameter or gesture should affect sound audibly when applicable. No light theme or system theme selection. Native OS dialogs and third-party plugin editors may use their own theme; our application chrome and first-party plugin editor remain dark.

Default workspace at 1440×900 logical px (responsive down to 1024×720, with scroll/collapsible panes). Native egui/wgpu, crisp vector widgets, no photorealistic knobs, no decorative animation that competes with sound editing. Support OS scaling, 100–200% UI scaling and keyboard navigation. Suggested tokens (chosen defaults, may refine contrast without changing dark-only): background #17191E, panels #20232A, raised #2B3039, borders #3D444F, text #E7EAF0, secondary #AAB2C0, accent #87C8A2, warning #E6B76E, error #E67D7D. Use track colors for identity, not status alone. Text contrast at least 4.5:1 for normal text where practical; controls have clear hover/focus/disabled states, textual labels/tooltips and accessible labels. Maintain 12–14 logical px readable text and minimum 24 px targets for common controls, except dense musical grids with zoom. Use original icons/font with audited licensing. No icon-only unfamiliar actions.

## 2. Main layout

```text
┌──────────────────────────────────────────────────────────────────────┐
│ File Edit View Help | Play Stop Record | Pattern / Song | BPM | Loop  │
│                     | Position | Metronome | Undo Redo | CPU | Audio  │
├──────────────┬───────────────────────────────────────────────────────┤
│ BROWSER      │ PLAYLIST                                              │
│ Search       │ Bars  1       2       3       4      ...              │
│ Instruments  │ Drums [ Pattern ] [ Pattern ]                          │
│ Effects      │ Bass  [ Pattern ] [ Pattern ]                          │
│ Samples      │ Voice             [ Audio clip ]                     │
│ Presets      │ FX    [ Automation    ]                                │
│ Projects     ├───────────────────────────────────────────────────────┤
│              │ CHANNEL RACK / PIANO ROLL / MIXER (switchable)          │
│ Preview      │ Kick  ■□□□ ■□□□ ■□□□ ■□□□                              │
│              │ Synth □□□□ □□□□ □□□□ □□□□                              │
├──────────────┴───────────────────────────────────────────────────────┤
│ Status: Ready | 48 kHz | 256 frames | saved | master meters           │
└──────────────────────────────────────────────────────────────────────┘
```

Top toolbar permanent: transport controls, record arm status, BPM, song/pattern, loop, metronome, time/bar display, current pattern selector, undo/redo, audio/CPU health, shortcuts to Channel Rack/Piano Roll/Playlist/Mixer. Browser left can collapse/resize. Playlist main; editor below can maximize, hide, resize or switch; mixer/editor popouts optional native windows after stable base. Remember layout but always provide Reset layout. First launch opens playable default synth with empty pattern, drum tracks optional demo; no automatic audio. Device defaults chosen if available, visible status and non-modal setup prompt if unavailable. Include self-contained original demo project and examples accessible from File menu. Empty project creation must not require naming until save.

## 3. Browser and channel rack

Browser sections expandable, search filters names/tags, favorites, local directories; lazy tree scan, never block UI. Click preset/instrument shows brief description and preview button, drag to channel rack/playlist/mixer highlights valid target. Preview note/phrase at safe gain, stop prior preview or on escape; do not keep playing when leaving browser unless explicitly requested. Missing files and incompatible plugin entries visibly disabled with reason. No automatic remote download.

Rack rows: channel name/instrument button, mute/solo LED with state label, gain/pan, mixer target, step buttons in alternating groups of four, adjustable steps/subdivision. Left click toggles step; drag paints on/off; secondary click opens velocity/pitch or piano roll context. Selected channel routes live MIDI/typing notes; opening instrument doesn't unexpectedly change MIDI channel unless selected. Double-click name edits label; click instrument opens editor; context menu duplicate, delete, move, route, replace (preserve notes), show piano roll. Reusable pattern contains per-channel note/step lanes; deleting a channel prompts only if data loss, undo restores. Step grid and piano roll are two views of same note model: off-grid notes appear as markers/summary, must not be silently quantized by switching view.

## 4. Piano roll

Keyboard vertical, C/octave labels, timeline bars/beats horizontal, notes as color rectangles with velocity shade and selected outline. Draw with left click in empty grid; drag note body moves; edge handles resize; right click deletes note under cursor; right-drag erase; Shift adds selection, Ctrl/command modifier per OS consistent; box selection tool/shortcut; Alt temporarily bypasses snap; wheel scroll vertical, Ctrl+wheel zoom around pointer, Shift+wheel horizontal (configurable). Escape cancels active gesture; delete removes selection; Ctrl+D duplicates by selection length or grid rule shown in tooltip; copy/cut/paste relative to playhead; arrow keys nudge by grid, up/down semitone; modifier octave. Snap choices off, bar, beat, 1/2, 1/4, 1/8, 1/16, 1/32 and triplets (notation labels relative to whole note; bar/beat distinct). Default 1/16. Audition on note insertion/move and keyboard ruler, release on mouse-up/focus loss. Velocity lane bottom, multi-edit draw/ramp; note length and velocity editable numerically optionally. Scale highlight (root/mode) default off; scale snap separate explicit toggle; chord menu major/minor/seventh etc. Insert chord at pointer, preserve manual editing. Quantize dialog with strength and preview; raw recording unchanged until committed. MIDI expression lanes for pitch bend/CC/sustain and pressure when captured.

## 5. Physical and typing MIDI

Settings > MIDI: list devices, enable/disable, channel filter, default selected instrument routing, optional fixed channel mapping, reconnect/disconnect status, input activity. MIDI learn on compatible parameter via context menu: arm, move CC, confirm mapping, remove/edit, collision policy explicit, prevent MIDI feedback loops. Record captures timestamped notes/velocity/controllers; count-in, overdub, loop, optional post-quantize. On disconnect release only affected source. Sustain honored during playback/recording. `Panic / All notes off` visible in transport/help, shortcut configurable.

Typing keyboard toggle near transport. Default lower row physical key mapping: Z=C, S=C#, X=D, D=D#, C=E, V=F, G=F#, B=G, H=G#, N=A, J=A#, M=B, comma=next C, L=C#, period=D, semicolon=D#, slash=E. Display map in settings and help; default base octave C4 (MIDI 60); adjustable octave ±, velocity default 0.8 with toolbar controls. Use physical positions by default and show actual localized key labels; allow remap for other layouts. On key-down ignore OS repeat; key-up releases stored original note even if octave/selection changes; focus loss/disable releases typing notes. No capture while text editing, modal input, or Ctrl/Alt shortcut active. Space transport, Ctrl+Z undo, Ctrl+Shift+Z redo, Ctrl+S save, Ctrl+O open, Ctrl+N new, Ctrl+Shift+S save-as; F5 playlist, F6 rack, F7 piano roll, F9 mixer (familiar defaults, configurable and conflicts validated). Global shortcuts do not trigger notes. Physical MIDI remains live when a text field is focused unless modal safety action explicitly disables audition. On-screen piano shows live and scheduled notes with distinct indicators, clickable notes.

## 6. Playlist and mixer

Playlist rows named/colorable, clip thumbnails with note miniatures or cached audio waveform; drag clips along timeline, lane changes, split at cursor, trim/resize edges (non-destructive), loop handle, gain/fade handles on audio clips; snap toggle and grid same semantics as piano roll. Drag pattern from selector or brush-paint repeats; context `Make unique` clones pattern contents and assigns new ID. Automation clips draw handles, point insertion/deletion, step/linear interpolation, target label and range. Tempo/meter timeline markers editable with validation. Time selection sets loop/export bounds. Clip overlaps sum by default (show overlap); optional comping not v1. Waveform thumbnails decimated off-thread by zoom; no huge per-frame allocations from audio files. File drop resolves target and imports safely.

Mixer: master pinned, horizontally scrollable strips with label, mute/solo, pan, fader with dB readout, stereo meters, peak/clip and gain reduction where relevant; double click resets gain/pan. Selected channel shows insert list with drag reorder, bypass, wet/dry where appropriate, click editor. Add effect by browser drag or searchable menu; delete with undo. Sends/routing view shows destinations and amounts, pre/post switch, sidechain; parallel rack graph optional advanced tab. Prevent cycles with explanation. Solo routing keeps required downstream buses and sidechain dependencies. Record arm/input monitor visible on audio tracks or channel input strips. Meter UI snapshots ≤60 Hz and decoupled from callback; status displays xrun/device/CPU issues without modal interruptions.

## 7. Flagship synthesizer editor (main differentiator)

```text
┌────────────────────────────────────────────────────────────────┐
│ Synth | < Preset > | Favorite | Save | Undo Redo | Advanced    │
│ Waveform / Spectrum / Motion      [Layer 1] [+ layer]         │
│ ┌──────────────────────────────────────────────────────────┐  │
│ │        draggable, editable waveform; brush cursor        │  │
│ └──────────────────────────────────────────────────────────┘  │
│ Draw  Deform  Smooth  Sharpen  Stretch  Distort  Noise         │
│ Size  ━━━━   Strength ━━━━     Reset wave                      │
│ [ Randomize sound ]   Chaos ━━━━   [ Locks ]                   │
│ [ Evolve ] [ Breed ] [ XY Morph ]                              │
│ Attack | Release | Brightness | Motion | Width | Drive         │
│ Live scope / spectrum | Output meter                           │
│ ░█░█░░█░█░█░░█░█░░█░█░█░  playable keyboard                   │
└────────────────────────────────────────────────────────────────┘
```

First screen playable. Waveform is primary canvas, not scope-only. Draw/deform etc. immediately visible; pointer radius/strength preview. Drag vertically changes amplitude, stretch tool changes phase; status tip one sentence, not DSP jargon. Brush controls accessible without leaving canvas. When another oscillator mode selected, canvas edits its applicable source waveform or switches to clearly labeled partial/operator/grain view (never silently edits unused table); default oscillator waveform tool always audible. Separate live scope to distinguish source cycle and actual output. Spectrum mode paints harmonic bars, phase optional, Motion shows editable keyframes and position slider. Right-click parameter: reset, lock/unlock for randomization, randomize only this, create automation, MIDI learn, value entry. One undo per completed gesture, audible updates during drag. Hold MIDI chord and continue manipulating without interrupting notes. Choose preset while playing with safe crossfade/release semantics. Dark-only editor reusable in plugin exports (host-owned chrome exception).

Randomize button primary; Chaos slider 0–100 beside it. Lock popover with categories and per-parameter locks, clear visual lock state, reset defaults. Sound verbs (Melt/Shatter/Corrupt/Grow/Twist/Scatter/Haunt) may be additional macro preset actions with actual distinct DSP and tooltip; not replacements for understandable normal controls. Morph XY panel four preset slots drag/drop, mouse pad, preview, record gesture, freeze preset. Evolution 3×3 preview cards, selected parent, previous/next generation, compare, favorite, Breed two selected and Evolve selected; always include audio preview (no automatic 9 simultaneous sounds). Show progress if generating, cancel; choose Use sound or Cancel. Advanced toggle reveals oscillator mode/layers, filter/ADSR, LFO, modulation matrix, graph editor, bounded expression nodes, numeric controls. Advanced graph supports drag ports, create/search nodes, delete edges/nodes, pan/zoom, explicit typed ports/color+label, error highlight and last playable graph. Basic screen remains usable without ever entering advanced mode.

## 8. Effects UX, settings and file flows

Every effect opens with presets, simple controls, mix/bypass/reset, level/gain reduction as applicable; advanced toggle for detailed parameters. Reverb type selector and decay curve; delay visual taps; EQ draggable points with frequency response; compressor meter + transfer curve; limiter ceiling and reduction; spectral/per-band distortion canvas; rack randomize and morph UI as synth. All editable controls support mouse drag, wheel/keyboard and optional numeric entry; standardized units and reset. Tooltips concise, not mandatory. Avoid modal dialogs except destructive irreversible actions, unsaved close, device choice when needed and import/export options. Export dialog: scope/rate/format/depth/tails/stems/path, progress/cancel, show clipped samples and completion path. File operations off UI thread, preserve edits on failure. Preferences: Audio/MIDI/Shortcuts/Appearance (scale/layout only; no theme switch)/Paths/Plugin scan. Use native OS open/save dialog or fully dark custom dialog; no white theme within owned UI. User help offline includes keyboard map, five-minute walkthrough, recording setup, synth experiments and troubleshooting.

## 9. Usability acceptance

On clean install (with supported device) owner can play a note within 10 s of launch after window ready, randomize once with one click, sculpt waveform with one drag while note sounds, and record into piano roll without reading a manual. First-time FL Studio user can locate rack/roll/playlist/mixer in one click each. Test 1024×720, 1440×900, 1920×1080 at 100/150/200% scaling as applicable; no clipped critical controls, scroll allowed. Test keyboard focus, stuck-note prevention, undo boundaries, drag-and-drop target feedback, accessibility labels and plugin editor resize. Verify screenshots on Windows and Ubuntu; no bright in-app surfaces. See TESTING.md for performance gates and actual manual evidence.

# rawDAWg — Project, preset and persistence format

## 1. Storage model

Local-first directory project, default suffix `.rawdawg` (fixed rawDAWg project suffix). User chooses folder location; no network or hidden absolute path dependency. `project.json` at root is entry point, with owned immutable content-addressed assets. OS file association can target marker/project.json via `application/x-rawdawg-project` MIME or `.rawdawg` directory open action where supported. If using marker file for associations, it must be deterministic and resolve sibling project.json without arbitrary path traversal. Save As creates independent project folder with required assets; never modifies original. Optional portable ZIP project export is not mandatory. Use relative portable paths normalized with `/`, reject traversal, absolute paths and unsafe symlinks for managed assets. External user sample browser paths may remain local preferences; once a sample is imported into a project copy/reference owned content inside it, not a mutable external file dependency.

```text
Song.rawdawg/
  project.json
  assets/sha256/<64-lowercase-hex>.<ext>
  plugin-state/<hash>.bin
  recovery/                 # autosave manifests, recording journals
  cache/                    # peaks, rendered previews; disposable
```

Assets immutable and hash-addressed; store original media (WAV/FLAC/MP3/SF2/IR) or recorded WAV, metadata and decoding strategy. Caches derived, never required to reopen. Store waveform arrays in JSON for small data or asset binary LE f32 with declared count/version/hash for larger; validate dimensions and finite values. Use UUIDs for project/track/pattern/clip/channel/instance/note/point/node IDs, generated once and preserved across edits, migrations and saves; `Make unique` duplicates with new child IDs and updates references. Parameter IDs are plugin stable IDs, not display text. All durations/positions ticks unless explicitly suffixed `_frames`, `_seconds`, `_hz`, `_db`; timebase PPQ 960. Tempo positive integer us/qn. Enums tagged stable strings. JSON UTF-8, no NaN/Inf. No OS-specific separators in references.

## 2. Schema v1 (illustrative JSON, normative field semantics)

Exact Rust structs/schema to be written and checked at M0/M6; use sample values as model shape, not hardcoded IDs. The following is a minimal empty-pattern project, with default master and instrument.

```json
{
  "schema_version": 1,
  "project_id": "00000000-0000-4000-8000-000000000001",
  "name": "Untitled",
  "ppq": 960,
  "sample_rate_hz": 48000,
  "tempo": [{"tick": 0, "us_per_quarter": 500000}],
  "meter": [{"tick": 0, "numerator": 4, "denominator": 4}],
  "assets": [],
  "channels": [{"id": "00000000-0000-4000-8000-000000000002", "name": "Synth", "instrument": {"type_id": "native.synth", "state_version": 1, "state": {}}, "gain_db": 0, "pan": 0, "mute": false, "mixer_id": "00000000-0000-4000-8000-000000000003"}],
  "patterns": [{"id": "00000000-0000-4000-8000-000000000004", "name": "Pattern 1", "length_ticks": 3840, "lanes": []}],
  "tracks": [],
  "clips": [],
  "mixer": {"master_id": "00000000-0000-4000-8000-000000000003", "channels": [{"id": "00000000-0000-4000-8000-000000000003", "name": "Master", "gain_db": 0, "pan": 0, "inserts": [], "sends": []}]},
  "automation": [],
  "midi_mappings": [],
  "loop": null
}
```

`native.synth` empty state above is shorthand for default patch only in illustrative example; production save must serialize full canonical patch including version. `schema_version` integer, reject newer unsupported version with read-only/error rather than destructive downgrade. Migrations one version at a time on copy; validate before/after, backup original, never rewrite on open unless save requested. Unknown extension fields preserve in explicit `extensions` objects where supported; unknown processor type state kept as opaque. Model validation must reject malformed required fields, duplicates, invalid references, bad ticks/lengths, incompatible graph ports and unbounded resource requests; recover missing media/plugin as placeholder, not malformed structure. Enforce configurable maximum JSON bytes/nesting, assets count/size, graph/voice/event limits before allocating. Never use `unwrap` on untrusted file. Ensure project model can be round-tripped without any audio devices.

## 3. Entity details

- `asset`: hash ID, relative path, type, source filename label, original rate/channels/frame count where applicable, optional decoded format; verify hash on import/relink or background integrity check. Missing assets retain ID/reference and metadata, show warning, audio silent until relink. Relink match hash by default; user confirms replacement with different content.
- `channel`: stable ID, name/color, instrument instance ID/type/state, gain dB, pan [-1,1], mute/solo, mixer ID. Native instrument state inline or asset refs, third-party instance metadata/state as below. No runtime pointers.
- `pattern`: ID, name/color, positive length ticks, per-channel lanes of notes and controller/expression events, step grid metadata (steps/subdivision). Note: ID, tick, duration_ticks>0, pitch 0–127, velocity (0,1], midi channel 0–15, optional expression points relative to note. Controller events tick, channel, CC/value or pitch bend/pressure normalized with original MIDI semantics. Same underlying data for step grid/piano roll. Pattern notes within length; resize explicit truncate/extend choice. Preserve overlapping notes and deterministic ordering.
- `track`: ID, name/color, mute/solo, optional audio/mixer input assignment. `clip`: ID, track_id, start_tick, length_ticks, type tag (`pattern`, `audio`, `automation`); pattern clip pattern_id and source_offset_ticks, loop flag; audio clip asset_id, source_offset_frames and source_length_frames, gain_db, fades (seconds or source frames explicit), playback rate (default 1), anchor mode. Amendment (ADR 0008): optional `fade_in_offset_frames` / `fade_out_offset_frames` (source frames, default 0, omitted when 0) say how far into its fade-in ramp a clip starts and how much of its fade-out ramp lies after its end, so the halves of a clip split inside a fade continue the same ramp. Audio anchored at musical start tick by default, source duration fixed seconds; tempo changes reposition start and recompute displayed end, do not automatically time-stretch; v1 supports explicit playback rate/resample and non-destructive trimming, no implicit warp. Persist actual source bounds and derive displayed ticks consistently; don't persist contradictory redundant lengths (for audio, length_ticks derived cache or omit). Automation clip curve_id and source offset, looping documented. Overlaps sum.
- `mixer`: master_id, channels with stable IDs, gain/pan/mute/solo, ordered insert instances (bypass, wet/dry, native/third-party state), sends with target_id, level, pre/post/sidechain port, explicit graph node/edge routing for advanced rack. Validate cycles per architecture. Master protection state saved; disabling requires explicit user intent.
- `automation`: curve ID, target `{instance_id, param_id}`, points with ID, tick, normalized value, interpolation to next point (`step`/`linear`), automation mode. No dangling target silently retargeted; disabled orphan retained until user repairs/deletes. Tempo/meter stored separately. `midi_mappings`: device persistent fingerprint or any input, channel, CC, target, min/max/invert; missing device retains mapping and shows disconnected.
- `plugin instance`: ID, format `native`/`clap`/`vst3`, type/class ID, vendor/version/display name, saved bus layout, stable param values, state_version for native or hash/path of opaque bytes for external; optional path hint only, resolve by catalog ID on other OS. External opaque state never parsed as trusted JSON or executed by loader; enforce max state bytes (default 64 MiB per instance, explicit error if exceeded). If missing plugin preserve state losslessly and user can replace or relink. Save capture outside RT per plugin ABI. Never serialize raw native pointers/handles.

## 4. Native preset schema

`.rawdawgpreset` self-contained ZIP or directory using manifest + hash assets, choose ZIP for exchange (validate entries, decompression limits, traversal, duplicates, reject symlinks). JSON manifest: schema_version, preset_id, kind (`synth`, `sampler`, `effect`, `rack`, `morph`), type_id, state_version, name, author optional, tags, description, full state, assets, mutation `{algorithm_version, seed, chaos, locks}`, graph nodes/edges stable IDs, waveform frames/arrays, oscillator/filter/envelope/LFO/modulator descriptors and values, effect chain, optional XY corner snapshots and position. Evolution history is project-local bounded undo/branch state; selected patch and seed always persisted; keep recent history in user/project UI data without making it required to load sound. Native presets compatible with plugin state and app. Original factory presets read-only, save edited copy with new ID. No implicit external content download. Store large assets managed with explicit references/embedding choice; portability requires all needed assets present or clear relink prompt. Validate archive limits (e.g. manifest 16 MiB, asset total 2 GiB default) and test zip bombs/traversal; allow large user assets via managed project folder instead.

## 5. Atomic saves, autosave and recording recovery

Save snapshot revision and necessary plugin state; write new assets to temporary file in same volume, verify, fsync as appropriate, rename to content hash, then write complete manifest temp, flush/sync and atomically replace original using platform-appropriate replace semantics (Windows cannot assume Unix rename-over-existing). Keep previous valid manifest backup/recovery; fsync parent on Unix where available, document durability limits by filesystem. If any error before commit, old project remains readable and dirty flag stays. Never remove referenced asset during save. Garbage collection only explicit user action with preview and no deletion of recovery/undo referenced content. Project locking/advisory single-writer with clear warning/read-only if already open; stale locks recoverable, do not overwrite another writer. Save As handles collisions with explicit confirmation; never overwrite project blindly.

Autosave default every 2 min if dirty, bounded ring (e.g. 10 revisions), separate recovery folder; unsaved new projects in OS per-user recovery dir. Autosave never marks normal save clean; on crash/open detect newer valid recovery and offer compare/recover/discard. Record temp WAV with journal including actual frames/rate/channels and timestamp; finalize header from data on recovery; retain partial if disk full. Never delete recovered data until user explicitly discards. Project portable paths, preferences (device names, MIDI devices, buffer, layouts, recent projects, plugin catalog/quarantine) stored separately in OS per-user settings, not essential for reopening. Persist typing key map and UI scale, no theme preference. Record migrations and compatibility tests with actual fixture files. 

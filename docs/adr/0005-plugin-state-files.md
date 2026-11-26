# ADR 0005 — Large opaque plugin state in `plugin-state/` files

Status: accepted · Date: 2026-11-25 · Refines ADR 0004 ("Opaque state")

## Context

ADR 0004 stored a hosted plugin's opaque state inline in `project.json` as base64. That is fine
for small states, but PROJECT_FORMAT.md allows 64 MiB per instance and lists
`plugin-state/<hash>.bin` in the project layout. Inline, such a state costs 85 MB of base64 in
the manifest and again in every autosave (the ring keeps ten), and the manifest itself is capped
at 64 MiB. rawDAWg's own hosted synth already produces 47 KB of state with the default patch.

Two further problems were found while doing this:

- the app hosted a plugin whose saved state failed to restore with **default** settings and only
  logged a warning; the next parameter edit then committed that default state over the user's
  saved one. PLUGIN_API.md §3 asks for a placeholder that keeps the bytes;
- the plugin host treated a state field that is not a string as "no state" and loaded nothing.

## Decision

- **Where.** The spill happens in `rawdawg-project` (`plugin_state.rs`) at save time; the
  in-memory model, the undo history, the engine and the host keep the inline form. Loading
  (`load_project_dir`, and recovery of an autosave) reads the files back, so the loaded model
  equals the model before saving.
- **What.** In a third-party instance's state, every string field whose name ends in `_b64`
  (`clap_state_b64`, `vst3_component_b64`, `vst3_controller_b64`) holding canonical base64 of
  more than 16 KiB is written as raw bytes to `plugin-state/<sha256>.bin` and replaced by
  `{"plugin_state_file": "plugin-state/<64 hex>.bin", "bytes": N}`. Smaller states stay inline,
  so small projects stay one readable file. Non-canonical base64 stays inline unchanged.
- **Integrity.** Files are content-addressed and written atomically; an existing file of the same
  name and size is not rewritten. On load the reference must name `plugin-state/<64 lowercase
  hex>.bin` exactly (no traversal), the size must match and be at most 64 MiB, and the SHA-256 of
  the content must equal the name.
- **Missing or damaged file.** The reference stays in the state; the instance is reported as
  unavailable ("its saved state file … is missing or damaged") and stays a stand-in; saving
  writes the reference unchanged; when the file is back the plugin is hosted with its state
  again. A state that the plugin itself fails to restore also keeps a stand-in with the reason
  instead of a default-state plugin; the host returns an explicit error for a non-inline field.
- **Clean-up.** After each save and autosave, `plugin-state/<64 hex>.bin` files that neither
  `project.json`, the previous manifest nor any autosave refers to are removed. Any other file
  name is left alone, and nothing is removed if one of those manifests cannot be read.
- `opaque_state_path` keeps its meaning (a whole-instance state file, preserved and copied) and
  any manifest string starting with `plugin-state/` counts as a reference for the clean-up.

## Alternatives considered

- Always spill every external state: makes every small project a directory of blobs and breaks
  the "one readable manifest" property for the common case.
- Spill the base64 text instead of the bytes: 33% larger files and a hash of an encoding rather
  than of the state.
- Use the single `opaque_state_path` field: VST3 has two blobs (component and controller).
- Reuse an unchanged-name file only after re-hashing it: a full SHA-256 of a 64 MiB state on
  every autosave (~130 ms on the main thread); load verifies the hash instead.

## Compatibility

Projects saved with inline states load unchanged. A project with references opened by a build
older than this decision would see an object where the host expected a string; no such build was
distributed. The format is additive and recorded here and in `docs/USER_GUIDE.md`.

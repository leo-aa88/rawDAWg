# ADR 0008 — Fades that continue across a split, and autosaves owned by a session

Status: accepted · Date: 2026-11-27 · Amends PROJECT_FORMAT.md (audio clips, recovery)

## Context

A review of the fixes for issues #23–#33 found two problems that need a decision rather than a
local fix.

1. **Splitting inside a fade.** Audio clip fades are linear ramps over `fade_in_frames` /
   `fade_out_frames` source frames, measured from each clip's own start and end. When a split
   falls inside a fade, neither half can play its piece of the ramp: the left half's fade-in got
   clamped to its length (a faster ramp), the right half started at full level (a jump), and the
   same at the fade-out. The split was audible.
2. **Whose autosaves are deleted.** Exit, New project, Discard and Save deleted every autosave in
   the project folder, including the autosaves of a crash the user had just chosen to *keep*
   ("Keep saved version (keep autosaves)"), and a window that had opened the project read-only
   beside a running copy deleted that copy's autosaves.

## Decision

- **Fade offsets.** Audio clips gain two optional fields, `fade_in_offset_frames` and
  `fade_out_offset_frames` (source frames, default 0, omitted when 0, so existing files are
  byte-for-byte unchanged and no schema version change is needed). The first says how far into
  its fade-in ramp a clip starts; the second how much of its fade-out ramp lies after the clip's
  end. The engine's gain at output frame `local` is
  `(local + in_offset) / fade_in` while that is below 1, and
  `(len − local + out_offset) / fade_out` likewise. A split gives the right half
  `in_offset + cut` and the left half `out_offset + rest`; a ramp a half no longer reaches is
  dropped (fade 0). Editing a fade in the clip menu makes it the clip's own whole fade (offset 0).
  Validation bounds both like the other frame counts.
- **Session autosaves.** A window remembers the autosave files it wrote (and those it took over
  by recovering). Save, Save As, New project, Discard and a clean exit delete only those. Crash
  autosaves the user kept stay on disk; edits after an open are numbered above every autosave in
  the folder, so a later save is newer than all of them and they are no longer offered (they leave
  through the ten-file ring). A window that opened the project read-only (another running copy
  holds the lock) neither autosaves nor offers that copy's autosaves; taking the lock over offers
  them. A never-saved project's per-user folder is deleted when its work is saved elsewhere,
  discarded or closed cleanly; a folder is offered after a crash only when it still holds the lock
  of a process that has ended.

## Alternatives

- *Fade start/end gains* (store the ramp's level at each end): same information, but it ties the
  ramp's slope to the clip's length and breaks when a half is trimmed again; offsets keep the ramp
  where it was in time.
- *No new fields; put a short crossfade at the cut*: changes the sound, which a split must not.
- *Delete every autosave on save (as before)*: simple, but breaks the promise of "keep autosaves"
  and lets a read-only window delete another copy's work.

## Compatibility

Older rawDAWg builds ignore the two fields (they are not read as unknown-field errors), so they
play a split-inside-a-fade clip with the old, audible ramps; nothing else changes. The recovery
rules change no file format.

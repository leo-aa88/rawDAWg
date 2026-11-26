# ADR 0007 — Pattern automation and the morph rack instrument

Status: accepted · Date: 2026-11-26 · Extends PROJECT_FORMAT.md (`automation`, instruments)

## Context

CORE-08 asks for automation that also plays in pattern mode, and SYN-05 for a bounded parallel
crossfade of corners whose structures differ, with X/Y automated separately (DSP.md, "XY morph").
Until now, automation curves played only through automation clips in Song mode, and the morph
pad existed only in the UI: incompatible corners were refused and the pad position could not be
automated.

## Decision

- **Pattern automation.** `AutomationCurve` gains an optional `pattern_id` (omitted when absent,
  so existing projects are unchanged and older readers ignore nothing they relied on). With a
  pattern id, the curve's ticks are relative to the pattern's start and it plays whenever the
  pattern plays: looping in pattern mode, and inside every clip of that pattern in song mode,
  entering at the clip's source offset and starting again at each repetition (the same rule as
  the pattern's notes). Automation clips never play a pattern curve. Validation refuses a curve
  whose pattern does not exist; removing a pattern removes its curves in the same undo step, and
  Make unique copies them under new ids.
- **Looped curves restart.** For every looped placement (automation clips and pattern clips) the
  curve's start value is emitted at each repetition. Before, a looped automation clip only
  re-emitted the points after the first one, so a sweep that ended high stayed high.
- **Morph rack.** A new native instrument type `native.morph` with state
  `{ "corners": [patch A, B, C, D], "x": 0…1, "y": 0…1 }` (synth patches as for `native.synth`).
  Parameters 1 (X) and 2 (Y) are ordinary instrument parameters, so automation, Touch/Write
  capture and MIDI learn target them like any other. The distinct corners together must fit the
  oscillator budget of one synth (voices × unison ≤ 4 × `max_voice_oscillators`); a larger rack is
  refused at validation and `prepare` with a message, never approximated.

## Alternatives

- *Pattern curves as automation clips inside patterns* (a second clip kind): more model and UI
  for the same behaviour.
- *Morph by interpolating per control block on the audio thread*: needs patch allocation and only
  works for compatible corners; the rack is explicit and works for any corners. Compatible
  corners keep the parameter-interpolating pad (preview, freeze, gesture baking).

## Compatibility

Projects without pattern curves or morph racks serialize exactly as before. A project using either
needs this version to play them; an older version fails on the unknown instrument type
(`native.morph`) or ignores `pattern_id` and would play the curve only through clips (it has none).

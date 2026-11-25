# ADR 0003 — Bandlimiting via mipmapped wavetables instead of PolyBLEP

Status: accepted · Date: 2026-11-24

## Context

T-OSC requires nonharmonic (alias) energy at least 35 dB below the fundamental on a controlled high-pitch fixture. A 2-sample PolyBLEP saw measured **31.0 dB** at C7 (2093 Hz, 48 kHz) with the Blackman-Harris analysis in `rawdawg_dsp::analysis`, failing the gate.

## Decision

All periodic basic waves are read from **mipmapped tables** (`rawdawg_dsp::wavetable`): the canonical editable cycle is 2048 samples; playback tables are 4096 samples per level with harmonics truncated to 512 ≫ level (10 levels), rebuilt off-thread by FFT, read with 4-point Hermite interpolation at the finest level whose top harmonic stays below Nyquist. Variable-width pulse is synthesized as the difference of two bandlimited saws offset by the pulse width (the exact DC-free pulse). Measured alias ratios: saw/square/triangle ≥ 35 dB at 2093, 4186 and 8372 Hz; A4 within ±0.5 Hz at all six rates.

The same table pipeline is the SYN-02 sculpting pipeline (cycle → DC removal → FFT → harmonic truncation → mips), so the editable waveform and the "basic" waves share one bandlimiting mechanism and one playback path.

## Consequences

- Mip level selection is a hard switch (no blend between levels): a fast pitch sweep steps the top octave of harmonics near Nyquist. Deemed inaudible for v1; a tapered-mip crossfade is a possible refinement.
- Very low notes keep at most 512 harmonics (e.g. 30 Hz → harmonics above 15.4 kHz omitted). Measured as acceptable; raise `MAX_HARMONIC`/`TABLE_LEN` if listening tests disagree.
- PolyBLEP code was removed rather than kept as an unqualified path.

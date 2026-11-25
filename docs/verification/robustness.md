# Robustness: parser fuzzing and stress soak (M8 slice 3)

TESTING.md asks for fuzzing of file parsers, graph and ABI validation with bounded fixtures, and a
30-minute stress soak with memory tracking. This page records what ran, what it found, and what
was fixed. Machine: 12th Gen Intel Core i7-12650H laptop, Ubuntu 22.04.4 under WSL2.

## Fuzzing

`crates/fuzz` runs deterministic, seeded mutation campaigns: `cargo xtask fuzz [--target NAME]
[--iterations N] [--seconds S] [--seed X]`, and `--replay FILE` re-runs one saved input. There is
no coverage instrumentation, because cargo-fuzz needs a nightly toolchain and this project pins
stable. Instead the mutators are structure-aware for JSON formats. Campaigns run as debug builds,
so integer overflow panics instead of silently wrapping.

Each target must not panic, and checks properties beyond that:

| Target | Input | Properties checked |
|---|---|---|
| project | `project.json` (JSON-aware) | parse → validate; a valid project saves and reloads unchanged; compiles and renders pattern and song mode with finite output |
| preset | `.rawdawgpreset` archive bytes | archive limits hold; synth/effect/rack contents load and render finite |
| preset-manifest | preset manifest JSON packed into a valid archive | as above, reaching manifest validation instead of dying at the zip CRC check |
| smf | Standard MIDI File bytes | imported notes have valid ticks, durations and pitches; tempo positive |
| sf2 | SoundFont bytes | parsed presets play finite audio |
| audio | WAV/FLAC bytes | decoded audio has a rate, one or two equal-length channels, finite samples |
| expr | synth expression text | output finite and within ±8 for inputs including NaN and ±∞ |
| expr-grammar | random expression trees from the grammar (every operator and function with its arity, extreme constants, occasional damage) | as expr; 78% of inputs compile, against 3% for byte mutations of text |
| patch | synth patch JSON | a patch that validates also loads in the synth; plays finite audio |
| effect-state | effect state JSON for every effect | loads, processes finite audio, and its saved state loads again |
| plugin-envelope | CLAP/VST3 state envelope JSON | non-finite mix refused; processor state loads and plays finite |
| midi-messages | raw MIDI bytes | decoded note, controller and bend values stay in range |

Every input that ever failed is kept in `fixtures/fuzz-regressions/<target>/` and replayed by
`crates/fuzz/tests/campaigns.rs` on every test run, next to a short 300-input campaign per target.

### Findings and fixes

Findings 1–5 came from two campaigns, both on debug builds: seed 1 with 2,000 inputs per target,
then seed 7 with 30,000. Finding 6 came from the first run of the later `expr-grammar` target.

| # | Target | What failed | Cause | Fix |
|---|---|---|---|---|
| 1 | project | A valid project changed after save and reload | `serde_json`'s default float parser is not exact: a waveform sample lost one unit in the last place on every load | Enabled `serde_json`'s `float_roundtrip` feature workspace-wide; test `floats_in_saved_state_round_trip_exactly` (fails without the feature, passes with it) |
| 2 | midi-messages | Note-on with velocity byte `0x80` became a note-on with velocity 0 | Velocity was compared with 0 before masking to 7 bits | Mask data bytes first, so it is a note-off; unit test in `rawdawg-types` |
| 3 | project | Overflow panic in `Note::end_tick` during validation | Validation added tick + duration before checking either was in range | Every tick-valued field (tempo, meter, patterns, notes, controllers, automation, clips, loop) is checked against `MAX_TICK` = 2⁴⁰; audio clip frame fields against `MAX_CLIP_FRAMES` = 2³⁷; end positions saturate |
| 4 | smf | Overflow panic inside `midly` 0.5.3 | A header claiming SMPTE timing with frame-rate byte `0x80` makes the library negate `i8::MIN` | SMPTE timing is unsupported anyway, so the header bit is checked before the library runs; any other parser panic becomes an import error |
| 5 | audio | Overflow panic inside `symphonia-format-riff` 0.6.1 | An implausible channel count in a WAV header makes the library shift by 32 or more | All decoder entry points (whole-file, in-memory, streaming open/seek/decode) contain panics and return "the audio decoder failed on malformed data" |
| 6 | expr-grammar | Panic in the expression evaluator for `clamp` with a NaN bound (for example `clamp(0.5, x, x)` when an input is NaN) | `f32::clamp` panics on NaN bounds; the evaluator only reordered reversed bounds. **The evaluator runs on the audio thread** inside the synth's modular graph | A NaN bound leaves that side open; unit test in `expr.rs`. An audit of every other `clamp` with computed bounds in the real-time crates found no other hazard (bounds constant, ordered, or guarded) |

Findings 4 and 5 are bugs in third-party crates. In release builds they wrap silently rather than
panic, but other panics in those libraries would still reach us, so containment stays either way.
They have not been reported upstream, because filing an external report is the owner's call.

### Clean campaigns after the fixes

Seed 1234, 60,000 mutated inputs per target (660,099 in total), debug build: **0 findings**.

The same seed with 20,000 inputs per target, counting how many inputs got past each parser to the
deeper checks:

| Target | Inputs | Parsed | Parsed share | Seconds | Slowest input |
|---|---|---|---|---|---|
| project | 20,005 | 12,986 | 64.9% | 46.8 | 13.4 ms |
| preset | 20,003 | 296 | 1.5% | 0.3 | 2.5 ms |
| preset-manifest | 20,003 | 11,478 | 57.4% | 16.3 | 5.0 ms |
| smf | 20,002 | 14,921 | 74.6% | 0.5 | 1.3 ms |
| sf2 | 20,001 | 4,788 | 23.9% | 0.4 | 0.5 ms |
| audio | 20,003 | 19,686 | 98.4% | 4.6 | 1.5 ms |
| expr | 20,009 | 579 | 2.9% | 0.0 | 0.2 ms |
| patch | 20,008 | 6,045 | 30.2% | 28.3 | 6.5 ms |
| effect-state | 20,048 | 10,765 | 53.7% | 2.8 | 1.9 ms |
| plugin-envelope | 20,016 | 19,964 | 99.7% | 2.0 | 1.0 ms |
| midi-messages | 20,001 | 20,001 | 100% | 0.0 | 0.0 ms |
| expr-grammar (after finding 6, seed 9) | 300,008 | 234,821 | 78.3% | 1.0 | 0.4 ms |

Two targets are shallow. Raw preset archives rarely survive the zip checksum, so the
`preset-manifest` target exists to reach the same code at 57%. Expression text breaks under byte
mutations (2.9%), so the `expr-grammar` target generates valid trees instead; it found finding 6
within its first inputs. Deep nesting (2,040 parentheses, 815 nested calls) is checked on a 2 MB
stack: it compiles or is rejected by the size and instruction limits, without stack overflow.
The slowest project input in the 60,000 run took 553 ms to parse, compile and render, which is slow
but bounded.

## Stress soak

`cargo run --release -p xtask -- soak --minutes 30 [--seed N]` builds the benchmark reference
project (four synths, four samplers, EQ and compressor per track, two reverb sends, master
limiter, automation). The engine renders on its own thread, paced to real time at 48 kHz and 256
frames. The main thread plays the editor's role, roughly 14 actions a second:

- note edits that keep each lane between 32 and 96 notes;
- synth and effect parameter changes;
- adding and removing effects (at most 12 added) and imported-sample channels (at most 6);
- mixer gains and tempo changes;
- undo and redo;
- transport moves (play, stop, seek, mode, loop);
- live notes and recording toggles.

Each edit goes through the undo history, then a recompile and plan swap. Every 60 s it stops,
releases everything and waits up to the longest synth release currently set plus 2 s for the
voice count to reach zero. It samples resident memory and, through a counting allocator, live heap
bytes every 5 s.

### Result (seed 11, 30 minutes, release build)

| Measure | Target | Result |
|---|---|---|
| Actions | | 24,608 over 1,803 s of audio |
| Stuck notes | none | 27 of 27 checks reached zero voices (longest wait 21.2 s, for a 21.3 s release) |
| Non-finite output, dropped events, command or record overflows, stream dropouts | none | none |
| Compile errors, refused plan swaps | none | none |
| Resident memory after the 5-minute warm-up | growth under 10% | 104.5 → 110.2 MB (+5.5%) |
| Live heap after warm-up | no upward trend | 55–80 MB, trend −16.5 MB/hour |
| Blocks that missed real-time pace | none | 27 of about 211,000; worst render 10.9 ms (deadline 5.33 ms) |

The late blocks match the benchmark's unexplained spikes on this WSL2 laptop (see
`benchmarks.md`); they are open until measured on bare metal.

### Earlier runs and what they showed

The first two 30-minute runs failed their memory and stuck-note checks. The data showed both were
flaws in the soak, not in rawDAWg:

1. **The song kept growing.** Note edits added more than they removed (192 → about 7,000 notes),
   so memory and render load rose with content: +61% resident memory and 413 late blocks.
2. **Effects piled up through undo.** The soak capped what it added using its own list, but undo
   restored removed items without tracking them, so effects accumulated past the cap. Live heap
   grew by about 42 MB/hour while the project file barely changed. Four stop checks also failed
   because a random edit had set a 20 s release while the check waited a fixed 8 s.

The soak now caps note counts per lane, counts added items from the project itself, waits for
the longest release, and records live heap next to resident memory so retention and real growth
can be told apart.

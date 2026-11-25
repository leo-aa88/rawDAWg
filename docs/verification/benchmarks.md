# Reference benchmarks (TESTING.md "Performance", QUAL-04)

Offline engine measurements from `cargo run --release -p xtask -- bench-audio --seconds 300`
(source: `xtask/src/bench.rs`; raw report: `target/verification/bench-audio.json`). These are
block-render times of the engine on a normal-priority thread, **not** device callback timings.
Real callback distributions, xruns and note-on latency on real audio hardware remain **HW-01
(BLOCKED)**.

## Machine and conditions

| Item | Value |
|---|---|
| CPU | 12th Gen Intel Core i7-12650H (8 cores, 16 threads), laptop |
| OS | Ubuntu 22.04.4 LTS under WSL2, kernel 5.15.167.4-microsoft-standard-WSL2 |
| Build | release (`opt-level 3`, thin LTO, line-table debug info), commit `06dc6f2` + benchmark changes |
| Thread | one ordinary thread, no real-time priority, power settings not controlled |
| Date | 2026-11-25 |

This is **not** the TESTING.md reference target: that calls for Ubuntu 24.04 or Windows 11 on
bare metal with recorded power settings. Treat these numbers as engineering data, not acceptance.

## Workload

TESTING.md default fixture, built in code (`bench::reference_project`), played in song mode at
48 kHz:

- four native synths (default patch), each holding an 8-note chord retriggered every bar (32 held
  notes, plus release overlap);
- four sampler tracks playing the generated fixtures (sine, noise, sweep, impulse) on every beat;
- every track on its own mixer channel with parametric EQ and compressor, two post-fader sends to a
  hall and a plate reverb bus, master limiter insert plus master protection;
- automation of synth 1's filter cutoff and of an EQ band gain across the whole song.

Output is audible (RMS 0.085) and finite in every run. Peak sounding voices 68 (32 held synth notes,
releases and sampler hits). One second of warm-up is excluded.

## Results: reference workload

Deadline at 48 kHz is 5.333 ms for 256 frames and 2.667 ms for 128 frames.

| Run | Block | Realtime factor | p50 ms | p99 ms | max ms | p99 / deadline | max / deadline | Plan compile ms |
|---|---|---|---|---|---|---|---|---|
| 1 | 256 | 7.3x | 0.695 | 1.229 | 3.525 | 23% | 66% | 20.6 |
| 2 | 256 | 7.3x | 0.687 | 1.203 | 4.005 | 23% | 75% | 16.0 |
| 3 | 256 | 7.1x | 0.703 | 1.374 | 9.108 | 26% | 171% | 21.3 |
| 4 | 256 | 7.1x | 0.700 | 1.259 | 6.328 | 24% | 119% | 18.7 |
| 1 | 128 | 7.2x | 0.349 | 0.638 | 2.397 | 24% | 90% | 15.6 |
| 2 | 128 | 6.7x | 0.366 | 0.830 | 4.487 | 31% | 168% | 12.8 |
| 3 | 128 | 6.7x | 0.367 | 0.820 | 3.392 | 31% | 127% | 17.3 |
| 4 | 128 | 7.2x | 0.347 | 0.667 | 2.697 | 25% | 101% | 15.7 |

256-frame runs cover 300 s of audio; 128-frame runs cover 60 s. Run 3's figures come from console
output only; its JSON was overwritten by run 4.

Against the TESTING.md targets at 256 frames:

- **p99 ≤ 50% of the deadline: met** in all four runs (23–26%).
- **max ≤ 90% of the deadline: not met** in runs 3 and 4 (171%, 119%). Run 4 had one block over
  the deadline in 56 250.

### What the slow blocks are

Run 4 also recorded each block's on-CPU time (`CLOCK_THREAD_CPUTIME_ID`). For every block above
50% of the deadline, on-CPU time equals wall time within 0.01 ms, so the guest kernel did not
deschedule the thread. That does not rule out hypervisor preemption under WSL2, which can be
counted as on-CPU time in the guest.

The workload repeats every 8 s. The slow blocks fall at different positions in that loop: 3.98 s
(three consecutive blocks, one event), 3.49, 6.97, 2.47, 0.72, 3.76 and 1.00 s. They occur in only a
few of the 37 repetitions, so they are not a fixed cost of a code path in this workload. Several sit
next to note-on beats, which are the heavier blocks. **The cause is open.** CPU frequency changes
and the virtualised environment are suspected but not shown. The next step is the same benchmark on
bare-metal Ubuntu 24.04 with a real-time-priority thread, then HW-01 on a real device.

The first version of the on-CPU measurement read `/proc/thread-self/schedstat`. On this kernel that
file advances in 10 ms ticks, so those readings were discarded.

## Results: single units (256 frames, run 4)

Each listed effect alone on a stereo sine-plus-noise signal, 5 s per unit:

| Effect | mean µs | p99 µs | p99 / deadline |
|---|---|---|---|
| Parametric EQ | 3.1 | 4.6 | 0.1% |
| Compressor | 5.0 | 5.6 | 0.1% |
| Limiter | 17.9 | 24.8 | 0.5% |
| Gate | 2.2 | 2.4 | 0.0% |
| Distortion | 26.5 | 61.8 | 1.2% |
| Per-band distortion | 10.3 | 11.7 | 0.2% |
| Delay | 6.3 | 10.3 | 0.2% |
| Chorus | 20.6 | 37.6 | 0.7% |
| Flanger | 9.3 | 26.5 | 0.5% |
| Phaser | 15.5 | 23.8 | 0.4% |
| Pitch / formant | 27.0 | 62.0 | 1.2% |
| Granular | 14.8 | 25.5 | 0.5% |
| Spectral | 35.4 | 103.8 | 1.9% |
| Reverb | 58.7 | 110.6 | 2.1% |
| Convolution | 1.5 | 2.4 | 0.0% |
| Stereo | 0.8 | 0.8 | 0.0% |

The convolution figure has **no impulse response loaded**, so it measures pass-through only.
Convolution with an IR is exercised in E2E-05 but not timed here yet.

Default synth patch, one instance:

| Notes requested | Sounding | mean µs | p99 µs | p99 / deadline |
|---|---|---|---|---|
| 1 | 1 | 12.1 | 18.9 | 0.4% |
| 8 | 8 | 86.0 | 135.2 | 2.5% |
| 32 | 32 | 309.0 | 545.8 | 10.2% |
| 64 | 32 | 309.8 | 522.6 | 9.8% |

The default patch's polyphony is 32 voices per instance, so 64 requested notes sound as 32.

## Not measured yet

- UI frame times, dense-playlist frame rate, waveform mouse-to-audible latency (need a display
  session: GUI-01).
- 30-minute stress soak with resident-memory tracking (M8 slice 3).
- 4-corner morph cost, convolution with a long IR, 64- and 1024-frame buffers, rates above 48 kHz.
- Windows numbers.

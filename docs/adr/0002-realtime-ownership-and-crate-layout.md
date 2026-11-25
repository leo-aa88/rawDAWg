# ADR 0002 — Real-time ownership strategy and crate layout (M0)

Status: accepted · Date: 2026-11-24

## Crate layout versus ARCHITECTURE.md §2

Implemented crates: `types`, `transport`, `rt`, `dsp`, `synthesizer`, `project`, `audio-engine`, `audio-io`, `plugin-clap`, `plugin-vst3`, `app` (binary `rawdawg`), `xtask`. Deviations, as permitted with an ADR:

- **`rt` (new)**: bounded SPSC queue wrapper with overflow counters, publish/retire handoff and the RT allocation guard. Pulled out of `dsp` so the engine, audio-io and tests share it without depending on DSP code.
- **`plugin-api` folded into `dsp::processor`** for now: the `Processor` trait, `PrepareSpec`, `ProcessContext`, `Tail`, parameter/state APIs. Direction preserved (synth/effects depend on `dsp`, adapters depend on processors). A separate crate is split out if the API grows format-specific adapters.
- `sequencer` and `mixer` responsibilities currently live in `audio-engine::{schedule, plan}`; they will be separated when the playlist/automation compiler (M3) and routing/latency compensation (M4) grow.

## Ownership model (implemented)

- The audio callback owns `Engine` and its live `Plan` (processors, buffers, schedules). Nothing else touches them.
- UI/MIDI → engine: `ControlQueue<EngineCommand>` (rtrb, 4096 slots); overflow is counted in `EngineStatus::command_overflows`, never silently dropped. Live events are staged per channel in preallocated `Vec`s that never grow on the RT thread (`EventStage::push` refuses and counts when full).
- Coordinator → engine: `Handoff<Plan>` (depth 4). `try_swap` only installs a new plan if the retirement queue has capacity; the old plan is sent back and dropped on the worker (`EngineHandle::collect_retired`). Refused swaps are counted. No `Arc` drop can happen on the RT thread.
- Status: atomics only (`EngineStatus`, `StreamStatus`), read by the UI at ≤60 Hz; the UI never waits on audio.
- Allocation guard: `rawdawg_rt::alloc_guard::GuardAllocator` (feature `alloc-guard`) counts allocations while a thread-local RT scope is active. T-RT tests in `synthesizer` and `audio-engine` assert zero allocations across note on/off, parameter events, pedal, bend, seek, panic and all block sizes 32…2048.

## Time model

Ticks (PPQ 960), tempo as integer µs/quarter, exact i128 cumulative tick-microseconds per tempo segment; frame positions are rounded once from the rational value (`TempoMap::tick_to_frame`). Half-open block scheduling `[start, end)` is verified to schedule every tick exactly once for block sizes 32…2048 (including 127 and 511).

## Known limitations at M0

- `Engine::render_block` reads `Instant::now()` twice per block for load measurement (vDSO clock on Linux; acceptable, revisit if it shows in profiles).
- Metronome and transport position are implemented in the engine but the `TransportView` meter fields are constant 4/4 until the meter map is consulted per block (M3).

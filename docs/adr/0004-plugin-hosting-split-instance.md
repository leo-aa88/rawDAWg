# ADR 0004 — Hosted plugins: one instance, split between the UI mirror and the realtime plan

Status: accepted · Date: 2026-11-25

## Context

The engine owns every processor inside an immutable realtime `Plan` (`Box<dyn Processor>`, `Send`), while the app keeps a main-thread "mirror" per instance for parameter editing and state commits. Native processors are cheap and deterministic, so a mirror is simply a second copy. A hosted CLAP plugin is one native instance whose main-thread functions (parameters, state, activation, GUI) must run on the thread that created it; the audio half (`clap_plugin.process`) must run on the audio thread. `clack-host` encodes this: `PluginInstance` is `!Send`, `StartedPluginAudioProcessor` is `Send`.

Two plugin instances per project instance (mirror + RT) would double every third-party instance, and the mirror would not see state the plugin changes internally (GUI edits, loaded files).

## Decision

- **One plugin instance per project instance** (CLAP and VST3 alike; `Vst3Instance`/`Vst3Processor` mirror the CLAP pair, with the VST3 component kept active on the main thread while its `IAudioProcessor` pointer is out on the RT thread). `ClapInstance` (plugin-host) owns the main-thread half and *is* the app's mirror (`other_mirrors`). `ClapProcessor` owns the audio half inside the plan. `ClapInstance::activate_for_rt` (called during plan compilation on the UI thread) activates the plugin and hands the started audio processor to a `ClapProcessor`; when a plan is replaced the new shell takes the running processor over in `Plan::migrate_from` (no re-activation, no audible gap); when a plan is dropped (always off the RT thread via `collect_retired`), `Drop` returns the processor through a channel and the main thread stops and deactivates it.
- **`Processor::carries_live_state()`** marks such shells: the compiler always migrates them instead of restarting on a state change; the project's stored state is applied to the main-thread instance (`load_state` while active, which CLAP permits).
- **`MainThreadCell<T>`** (plugin-host) carries the `!Send` `PluginInstance` inside the `Send` mirror map. It records the creating thread and returns the value only on that thread; dropping it elsewhere leaks instead of destroying off-thread. The single `unsafe impl Send` is documented there; misuse yields `None` (a clear failure), never a cross-thread plugin call.
- **Opaque state** is stored inline in the project JSON as `{format, plugin_id, clap_state_b64}` (base64 of the bytes the plugin produced). The bytes are never parsed by rawDAWg. Large states are written to `plugin-state/` files at save time (ADR 0005); the per-instance cap is 64 MiB as specified.
- Hosted effects get no wrapper wet/dry (plugins carry their own); bypass and latency alignment come from the mixer insert as for native effects.

## Consequences

- Third-party code runs in-process on the UI and audio threads; the scanner's isolation (separate process, timeout, quarantine) protects discovery only. Non-finite output and `process` errors are muted and counted per instance (shown in Settings › Plugins), as PLUGIN_API.md §3 requires; a crash inside `process` is not containable.
- `ClapInstance::save_state` goes through a `RefCell` borrow of the instance (a `&self` API over a main-thread-only object); re-entrancy from a host callback would panic rather than alias.
- The RT shell's `Drop` sends through an mpsc channel; this only runs off the RT thread by construction (retired plans are collected on the UI thread).
- Parameter display for hosted plugins uses the plugin's own text (`value_to_text`, `getParamStringByValue`) since M8 slice 6.

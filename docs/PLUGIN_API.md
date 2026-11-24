# rawDAWg — Plugin architecture and interoperability

All first-party plugin metadata and editors use the rawDAWg display brand; do not rename third-party plugins.

## 1. Native API (format-independent)

Internal native processors are Rust types, not VST instances. Do not define an unstable Rust dynamic-library ABI. Built-ins statically linked and registered by stable namespaced IDs; native presets are data. External native-code interop goes through CLAP/VST3 adapters only. Same processor implementation shared by app, headless renderer and exported binaries. Parameter schema is versioned independent of UI. Example shape (illustrative Rust, adapt to borrow checker and actual types without changing guarantees):

```rust
pub struct PrepareSpec { pub sample_rate: f64, pub max_frames: usize, pub layout: BusLayout }
pub struct ProcessContext<'a> {
    pub audio: AudioBlock<'a>, // borrowed planar input/output; separate aux buses
    pub events: &'a [TimedEvent], // sorted offsets < frames
    pub transport: &'a TransportView,
}
pub trait Processor: Send {
    fn prepare(&mut self, spec: &PrepareSpec) -> Result<(), PrepareError>; // non-RT
    fn process(&mut self, ctx: &mut ProcessContext<'_>); // bounded, RT-safe
    fn reset(&mut self); // bounded, RT-safe
    fn latency_frames(&self) -> u32;
    fn tail(&self) -> Tail; // finite/unknown/infinite, no allocation
}
```

State save/restore, UI creation, parameter catalog, graph preparation and destruction are separate non-RT APIs with explicit thread contracts. Pass preallocated data to RT parameter update, notes/CC, expression and transport with sample offsets. Do not require allocation to query descriptors during process. `ParamId` stable numeric u32 within stable plugin type ID, never reuse removed IDs; native semantic ID string also stable. Map normalized [0,1] to physical range (linear/log/discrete, unit, default, smoothing, flags automatable/modulatable/stepped/hidden/read-only), display format and parse; validate finite values. Track read-only meters separately. Parameter changes tagged gesture begin/update/end for host automation; GUI, automation, MIDI and DSP modulation precedence documented (base value/automation plus modulation, clamp). Latency and tail changes notify coordinator/host off RT; enforce main-thread callbacks when required. Versioned opaque state payload with bounded read and migrations; include patch/asset references or embed required assets for exported presets, no absolute paths necessary for default presets. Reject invalid state without destroying current playable state.

## 2. Exported first-party plugins (PLUG-01)

Ship actual native Windows x64 and Linux x86_64 CLAP and VST3 binaries for flagship synth and all effect families in FX-01. Multiple algorithm modes can share one plugin type per family; stable IDs/class IDs and vendor string (display brand `rawDAWg`; legal publisher identity remains an owner decision). Use external CLAP IDs such as `rawdawg.synth` and `rawdawg.fx.reverb`, stable unique VST3 class IDs recorded in source, and display names such as `rawDAWg Synth` and `rawDAWg Reverb`; preserve these IDs across releases. Native internal processor IDs (e.g. `native.synth`) remain unchanged. Use clack/clap-sys and vetted VST3 bindings/SDK; no JUCE. Keep format-specific code thin; native processor handles audio. Use independent versioned state and shared first-party editor. Effects accept mono/stereo where supported, expose stereo main and sidechain where applicable, report supported configurations; instruments event input and stereo out. Unsupported arrangements return explicit error, not wrong channels. Fixed/default bus negotiation documented.

### CLAP

Implement entry/factory/descriptor and plugin lifecycle (`init`, `activate`, `start_processing`, `process`, `stop_processing`, `deactivate`, `destroy`) and thread annotations per pinned standard. Required supported extensions: audio ports/config (as applicable), note ports (instrument), params, state, latency, tail, GUI, host notifications, thread checks when host supplies; use per-note modulation/expressions where supported and advertised. Support parameter value/modulation events, MIDI dialect where negotiated, sample offsets, transport and note end notifications where required. `process` status/tail/sleep semantics accurate; wake for events. All pointers/counts validated at ABI boundary as far as safe, no Rust panics unwind across FFI. Native GUI embeds using platform handle per spec or supported floating GUI, supports resize and teardown on main thread. Do not advertise unsupported API/platform.

### VST3

Use verified SDK version (3.8+ MIT terms for matching SDK; bindings may carry independent terms). Factory exports stable processor/controller class IDs; lifecycle initialize/setupProcessing/setBusArrangements/setActive/setProcessing/process/terminate; implement component/controller state handoff and host callbacks; audio/event buses, param queues with offsets, normalized values, bypass, latency/tail, process context, supported sample-size negotiation (f32 required; reject f64 unless implemented), silence flags accurately. MIDI CC mapping/controller assignments and note expression only where format supports; do not claim CLAP's per-note modulation semantics are identical to VST3. `IPlugView` platform support and resize lifecycle per spec. COM-style reference counting and ownership correct; release on allowed thread, no unwinding. Use official validator, bundle format and architecture-specific folder names per pinned SDK. No VST2 support.

### Editor and asset portability

Reuse actual synth/effect UI widgets in plugin editor (not full DAW). Native child window embedding, keyboard/mouse event forwarding, resize/DPI, host parent destruction, show/hide/reopen; Linux X11/XWayland where required and documented, test on Ubuntu with supported host. Do not promise all Wayland-native host combinations. If custom GUI unsupported in host, generic host controls still operate; must not advertise a blank editor as working. State and preset portability across independent hosts/OS where same plugin ID/version; embed small required sample assets with bounded state size or copy to defined managed user data and show relink UI on missing large asset. Never silently substitute a different asset. Separate standalone GUI/preferences from host instance state.

## 3. Third-party host (PLUG-02/03)

Catalog search standard OS locations and user paths, optional rescan. Windows standard VST3/CLAP directories and user custom dirs; Ubuntu standard system/user dirs per format docs; no Wine. Recursively enumerate candidate bundles safely without following unbounded symlink loops; identify by canonical path, format, stable plugin ID/class ID, fingerprint (size/mtime/hash as appropriate), version, architecture and capabilities. Scan **in separate subprocess**, bounded timeout (default 15 s/plugin, configurable), no audio devices/network, bounded stdout/metadata, crash/timeout quarantine record. No automatic execution on drag from arbitrary downloads; scan configured paths on user action/default first launch with clear explanation. Native plugin code executes as user, scanner isolation is crash containment not security sandbox. User chooses retry/remove quarantine; changed fingerprint offers rescan. Safe-mode startup skip all third-party loading and keep project state placeholders; detect prior plugin crash marker. Never automatically install binaries or open remote links.

Host instances: load library and validate ABI; negotiate layout, activation, prepare on main/control thread per standard; process via adapter on RT, sample-offset notes/events/params, transport tempo/time/meter, latency/tail, aux sidechain, accurate channel layout. Generic parameter editor uses catalog and normalized values if plugin has no compatible custom GUI. Embed supported native GUI per OS/format; main-thread callbacks, timer and resize support per spec. Save opaque state with version/ID, restore on load; if unavailable or failed, keep opaque bytes and parameters and show missing/failed plugin placeholder. Effect placeholder passes dry with documented latency fallback; instrument placeholder silent; mixer displays warning and export requires explicit confirmation. Bypass preserves latency while active; reset, suspend, teardown off RT according to host ABI. Request latency changes trigger safe graph recompile; no synchronous graph rebuild in process. Track per-plugin processing errors, nonfinite output and deadline overruns, surface and allow bypass; host cannot guarantee protection against arbitrary native crash/hang during in-process RT execution. Runtime sandboxing is a future enhancement, not falsely claimed. Recover last autosave after crash.

Implement CLAP hosting/export first; VST3 second, with tests independently. Use binding library only for ABI glue, no embedded external DAW. Test self-hosting first-party binaries but also at least one independent instrument and effect per format/platform, and run exported synth/effect in at least one independent format-capable DAW per platform. Choose available legally redistributable fixtures or installed owner-approved hosts; document exact versions and plugin paths, never assume FL Studio exists natively on Ubuntu. Do not copy third-party plugin binaries into distribution. Validate all exported plugin types with format validators; actual synth/effect audio and state round-trip, not just discovery. See TESTING.md.

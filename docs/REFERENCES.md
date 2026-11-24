# rawDAWg — Upstream references and dependency verification

These are implementation references, not vendored code or a guarantee of selected library compatibility. At M0 verify the exact release APIs, OS build requirements, SPDX license and transitive dependency terms; pin versions and record findings in ADRs and Cargo.lock. URLs below are upstream project locations. CPAL, eframe, CLAP, clack, VST3 bindings and Steinberg's SDK information were checked during specification preparation (24 November 2026). Other links are suggested upstream starting points, not individually audited or tested. No dependency has been compiled as part of this documentation deliverable.

| Area | Upstream |
|---|---|
| Audio I/O and backend features | https://github.com/RustAudio/cpal |
| Native UI | https://github.com/emilk/egui ; https://github.com/gfx-rs/wgpu |
| MIDI | https://github.com/Boddlnagg/midir ; https://github.com/kovaxis/midly |
| Bounded queues | https://github.com/mgeier/rtrb |
| DSP utilities | https://github.com/ejmahler/RustFFT ; https://github.com/HEnquist/rubato |
| Audio decoding and WAV | https://github.com/pdeljanov/Symphonia ; https://github.com/ruuda/hound |
| CLAP ABI and extensions | https://github.com/free-audio/clap |
| CLAP Rust wrappers | https://github.com/prokopyl/clack |
| CLAP validation | https://github.com/free-audio/clap-validator |
| VST3 SDK | https://www.steinberg.net/developers/vstsdk/ ; https://github.com/steinbergmedia/vst3sdk |
| VST3 Rust bindings | https://github.com/coupler-rs/vst3-rs |
| Steinberg SDK/ASIO terms | https://www.steinberg.net/developers/ |
| MIDI standards | https://midi.org/ |
| Linux audio | https://pipewire.org/ ; https://jackaudio.org/ ; https://www.alsa-project.org/ |
| Packaging | https://wixtoolset.org/ ; https://github.com/kornelski/cargo-deb |
| Rust license/audit tooling | https://github.com/EmbarkStudios/cargo-deny ; https://github.com/RustSec/rustsec |
| Loudness / true peak reference | https://www.itu.int/rec/R-REC-BS.1770 |

## Important verification notes

- CPAL upstream documents Windows WASAPI/optional ASIO and Linux ALSA plus optional JACK/PipeWire/PulseAudio support. Features, device behavior and system deps vary by selected version; confirm in build and actual device tests. ASIO has its own terms; VST3 MIT terms do not apply automatically to ASIO. Do not bundle restricted SDK/runtime without review.
- eframe is egui's native application framework, with wgpu rendering support. Reusing widgets for a plugin editor still needs native host window integration; eframe standalone window alone is not embedded editor support.
- CLAP is an ABI with separate host and plugin roles. clack supplies wrappers for both. Use official headers and clap-validator for lifecycle validation. Verify advertised extensions against the exact standard.
- Steinberg's VST3 SDK 3.8 introduced MIT licensing in 2025. Verify the license of the exact SDK/binding version and trademark/redistribution obligations separately; older SDKs/bindings may have different terms. coupler-rs/vst3-rs is a candidate ABI binding, not a complete host/editor implementation.
- Verify the FLAC encoder independently (decoder support does not imply encoding); select a maintained license-compatible encoder or implement via a thin approved binding. Verify SF2 parser coverage and clearly document unsupported generators/modulators. Do not use bundled commercial samples or external DAW assets.
- Reference algorithms may be implemented from standard mathematical definitions and primary literature, but do not copy copyrighted implementations or bypass license obligations. Cite any adapted code with its license and notices. No final app license selected in this spec.

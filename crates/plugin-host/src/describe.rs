//! In-process description of a CLAP module: load the entry, list descriptors and
//! instantiate each plugin to read its ports, parameter count and GUI support.
//!
//! This runs inside the **scanner subprocess** ([`crate::scan`]); the parent never
//! loads an unknown module. Any crash or hang here is contained there.

use crate::catalog::{Format, PluginDescription, ScannedLibrary};
use crate::host::{MinimalHost, host_info};
use clack_extensions::audio_ports::{AudioPortInfoBuffer, PluginAudioPorts};
use clack_extensions::gui::PluginGui;
use clack_extensions::note_ports::PluginNotePorts;
use clack_extensions::params::PluginParams;
use clack_host::prelude::*;
use std::path::Path;

/// Upper bound on plugins we describe per module (a module with more is suspicious).
pub const MAX_PLUGINS_PER_MODULE: usize = 256;

fn cstr_owned(v: Option<&std::ffi::CStr>) -> String {
    v.map(|c| c.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Describe every plugin of the CLAP module at `path`.
///
/// # Safety
/// Loads and executes native code from `path`. Call only from the isolated scanner
/// process or on a module the user explicitly trusts.
pub unsafe fn describe_clap_library(path: &Path) -> Result<ScannedLibrary, String> {
    let fingerprint = crate::catalog::Fingerprint::of(path).ok_or_else(|| format!("cannot stat {}", path.display()))?;
    // SAFETY: forwarded from the caller's contract above.
    let entry = unsafe { PluginEntry::load(path) }.map_err(|e| format!("not a loadable CLAP module: {e}"))?;
    let api_version = format!("{}", entry.version());
    let factory = entry.get_plugin_factory().ok_or_else(|| "module has no plugin factory".to_owned())?;
    let mut plugins = Vec::new();
    for desc in factory.plugin_descriptors().take(MAX_PLUGINS_PER_MODULE) {
        let Some(id) = desc.id() else { continue };
        let features: Vec<String> = desc.features().map(|f| f.to_string_lossy().into_owned()).collect();
        let mut d = PluginDescription {
            format: Format::Clap,
            plugin_id: id.to_string_lossy().into_owned(),
            name: cstr_owned(desc.name()),
            vendor: cstr_owned(desc.vendor()),
            version: cstr_owned(desc.version()),
            description: cstr_owned(desc.description()),
            is_instrument: features.iter().any(|f| f == "instrument"),
            is_effect: features.iter().any(|f| f == "audio-effect"),
            features,
            audio_inputs: 0,
            audio_outputs: 0,
            main_input_channels: 0,
            main_output_channels: 0,
            note_inputs: 0,
            param_count: 0,
            has_gui: false,
            notes: Vec::new(),
        };
        match PluginInstance::<MinimalHost>::new(|_| crate::host::Shared::default(), |_| crate::host::MainThread::default(), &entry, id, &host_info()) {
            Ok(mut instance) => {
                let mut handle = instance.plugin_handle();
                if let Some(ports) = handle.get_extension::<PluginAudioPorts>() {
                    d.audio_inputs = ports.count(&handle, true);
                    d.audio_outputs = ports.count(&handle, false);
                    let mut buf = AudioPortInfoBuffer::new();
                    if let Some(info) = ports.get(&handle, 0, true, &mut buf) {
                        d.main_input_channels = info.channel_count;
                    }
                    let mut buf = AudioPortInfoBuffer::new();
                    if let Some(info) = ports.get(&handle, 0, false, &mut buf) {
                        d.main_output_channels = info.channel_count;
                    }
                }
                if let Some(notes) = handle.get_extension::<PluginNotePorts>() {
                    d.note_inputs = notes.count(&handle, true);
                }
                if let Some(params) = handle.get_extension::<PluginParams>() {
                    d.param_count = params.count(&handle);
                }
                d.has_gui = handle.get_extension::<PluginGui>().is_some();
                if !d.is_instrument && !d.is_effect {
                    // no declared role: infer from ports
                    d.is_instrument = d.note_inputs > 0 && d.audio_inputs == 0 && d.audio_outputs > 0;
                    d.is_effect = d.audio_inputs > 0 && d.audio_outputs > 0;
                }
                let _ = &mut handle;
            }
            Err(e) => d.notes.push(format!("instantiation failed during scan: {e}")),
        }
        plugins.push(d);
    }
    if plugins.is_empty() {
        return Err("module exposes no plugins".to_owned());
    }
    Ok(ScannedLibrary { path: path.to_path_buf(), fingerprint, plugins, scanned_at_unix: crate::catalog::now_unix(), api_version })
}

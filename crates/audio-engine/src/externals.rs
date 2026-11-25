//! Injection point for hosted third-party plugins (PLUGIN_API.md §3).
//!
//! The engine never loads plugin binaries itself; the app hands it a provider that
//! turns a non-native [`Instance`](rawdawg_project::Instance) into a prepared
//! processor whose realtime half is ready for the plan. Instances the provider
//! does not know (missing plugin, safe mode) become stand-ins with their state
//! preserved (`CompileOutput::missing_plugins`).

use rawdawg_dsp::processor::{PrepareSpec, Processor};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginRole {
    Instrument,
    Insert,
}

pub trait ExternalPlugins {
    /// A processor for a non-native instance, already activated for `spec`, or `Ok(None)`
    /// when the plugin is unavailable (stand-in). `Err` aborts the compile with a message.
    fn instantiate(&mut self, inst: &rawdawg_project::Instance, role: PluginRole, spec: &PrepareSpec) -> Result<Option<Box<dyn Processor>>, String>;
}

/// No hosting (offline tools, tests, safe mode): every external instance is a stand-in.
pub struct NoExternals;

impl ExternalPlugins for NoExternals {
    fn instantiate(&mut self, _inst: &rawdawg_project::Instance, _role: PluginRole, _spec: &PrepareSpec) -> Result<Option<Box<dyn Processor>>, String> {
        Ok(None)
    }
}

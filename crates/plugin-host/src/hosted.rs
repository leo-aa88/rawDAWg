//! Format-neutral view of a hosted plugin's main-thread instance, so the app and
//! the engine provider treat CLAP and VST3 instances alike.

use crate::clap::ClapInstance;
use crate::vst3::Vst3Instance;
use rawdawg_dsp::processor::{PrepareSpec, Processor};

pub trait HostedInstance: Processor {
    /// Produce the realtime half for a plan compiled with `spec` (activating if needed).
    fn activate_for_rt(&mut self, spec: &PrepareSpec) -> Result<Box<dyn Processor>, String>;
    /// Take back realtime halves returned by dropped plans and deactivate.
    fn collect_returns(&mut self);
    fn is_active(&self) -> bool;
    fn plugin_id(&self) -> &str;
    fn display_name(&self) -> &str;
    fn problem_count(&self) -> u32;
    fn refresh_values(&mut self);
    /// `clap:<id>` / `vst3:<cid>`.
    fn live_id(&self) -> String {
        Processor::type_id(self).to_owned()
    }
}

impl HostedInstance for ClapInstance {
    fn activate_for_rt(&mut self, spec: &PrepareSpec) -> Result<Box<dyn Processor>, String> {
        ClapInstance::activate_for_rt(self, spec).map(|p| Box::new(p) as Box<dyn Processor>)
    }
    fn collect_returns(&mut self) {
        ClapInstance::collect_returns(self)
    }
    fn is_active(&self) -> bool {
        ClapInstance::is_active(self)
    }
    fn plugin_id(&self) -> &str {
        ClapInstance::plugin_id(self)
    }
    fn display_name(&self) -> &str {
        &self.info.name
    }
    fn problem_count(&self) -> u32 {
        ClapInstance::problem_count(self)
    }
    fn refresh_values(&mut self) {
        ClapInstance::refresh_values(self)
    }
}

impl HostedInstance for Vst3Instance {
    fn activate_for_rt(&mut self, spec: &PrepareSpec) -> Result<Box<dyn Processor>, String> {
        Vst3Instance::activate_for_rt(self, spec).map(|p| Box::new(p) as Box<dyn Processor>)
    }
    fn collect_returns(&mut self) {
        Vst3Instance::collect_returns(self)
    }
    fn is_active(&self) -> bool {
        Vst3Instance::is_active(self)
    }
    fn plugin_id(&self) -> &str {
        Vst3Instance::plugin_id(self)
    }
    fn display_name(&self) -> &str {
        &self.info.name
    }
    fn problem_count(&self) -> u32 {
        Vst3Instance::problem_count(self)
    }
    fn refresh_values(&mut self) {
        Vst3Instance::refresh_values(self)
    }
}

/// Whether a mirror processor is a hosted plugin instance.
pub fn is_hosted(p: &dyn Processor) -> bool {
    let t = Processor::type_id(p);
    t.starts_with("clap:") || t.starts_with("vst3:")
}

/// Downcast a mirror to its hosted instance, for either format.
pub fn hosted_mut(p: &mut dyn Processor) -> Option<&mut dyn HostedInstance> {
    if !is_hosted(p) {
        return None;
    }
    let any = p.as_any_mut();
    if any.is::<ClapInstance>() {
        return any.downcast_mut::<ClapInstance>().map(|c| c as &mut dyn HostedInstance);
    }
    any.downcast_mut::<Vst3Instance>().map(|v| v as &mut dyn HostedInstance)
}

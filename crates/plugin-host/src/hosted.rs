//! Format-neutral view of a hosted plugin's main-thread instance, so the app and
//! the engine provider treat CLAP and VST3 instances alike.

use crate::clap::ClapInstance;
use crate::vst3::Vst3Instance;
use rawdawg_dsp::processor::{PrepareSpec, Processor};
use std::cell::RefCell;
use std::collections::HashMap;

/// Longest readout taken from a plugin's own value text (characters).
pub const MAX_PARAM_TEXT_CHARS: usize = 48;

/// Readouts already asked of the plugin, by (parameter id, value), so the UI asks again
/// only for values it has not shown yet (the generic panel draws every row each frame, and
/// a choice list asks for every step). Bounded: cleared when it reaches
/// [`MAX_CACHED_TEXTS`]. Main thread only.
#[derive(Default)]
pub(crate) struct ParamTextCache(RefCell<HashMap<(u32, u32), Option<String>>>);

/// Entries kept before the readout cache starts over.
pub const MAX_CACHED_TEXTS: usize = 8192;

impl ParamTextCache {
    pub(crate) fn get_or(&self, id: u32, normalized: f32, ask: impl FnOnce() -> Option<String>) -> Option<String> {
        let key = (id, normalized.to_bits());
        if let Some(text) = self.0.borrow().get(&key) {
            return text.clone();
        }
        let text = ask();
        let mut map = self.0.borrow_mut();
        if map.len() >= MAX_CACHED_TEXTS {
            map.clear();
        }
        map.insert(key, text.clone());
        text
    }

    /// Forget every readout (after a state load, which can change how a plugin formats).
    pub(crate) fn clear(&self) {
        self.0.borrow_mut().clear();
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.0.borrow().len()
    }
}

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
    /// Whether the plugin has an editor rawDAWg can show in a window on this platform.
    fn has_editor(&self) -> bool {
        false
    }
    fn editor_is_open(&self) -> bool {
        false
    }
    /// Open the plugin's own editor in a rawDAWg window titled `title` (main thread).
    fn open_editor(&mut self, _title: &str) -> Result<(), String> {
        Err("rawDAWg cannot show this plugin's editor yet; use the controls in the panel".into())
    }
    /// Service an open editor; call every UI frame while it is open.
    fn editor_idle(&mut self) -> crate::clap::EditorIdle {
        crate::clap::EditorIdle::default()
    }
    fn close_editor(&mut self) {}
    /// The native id of the window holding an open editor (X11 window id / HWND).
    fn editor_window_id(&self) -> Option<u64> {
        None
    }
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
    fn has_editor(&self) -> bool {
        ClapInstance::has_editor(self)
    }
    fn editor_is_open(&self) -> bool {
        ClapInstance::editor_is_open(self)
    }
    fn open_editor(&mut self, title: &str) -> Result<(), String> {
        ClapInstance::open_editor(self, title)
    }
    fn editor_idle(&mut self) -> crate::clap::EditorIdle {
        ClapInstance::editor_idle(self)
    }
    fn close_editor(&mut self) {
        ClapInstance::close_editor(self)
    }
    fn editor_window_id(&self) -> Option<u64> {
        ClapInstance::editor_window_id(self)
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
    fn has_editor(&self) -> bool {
        Vst3Instance::has_editor(self)
    }
    fn editor_is_open(&self) -> bool {
        Vst3Instance::editor_is_open(self)
    }
    fn open_editor(&mut self, title: &str) -> Result<(), String> {
        Vst3Instance::open_editor(self, title)
    }
    fn editor_idle(&mut self) -> crate::clap::EditorIdle {
        Vst3Instance::editor_idle(self)
    }
    fn close_editor(&mut self) {
        Vst3Instance::close_editor(self)
    }
    fn editor_window_id(&self) -> Option<u64> {
        Vst3Instance::editor_window_id(self)
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

#[cfg(test)]
mod tests {
    use super::{MAX_CACHED_TEXTS, ParamTextCache};
    use std::cell::Cell;

    #[test]
    fn readout_cache_asks_once_per_value_and_stays_bounded() {
        let cache = ParamTextCache::default();
        let asked = Cell::new(0);
        let ask = |t: &str| {
            asked.set(asked.get() + 1);
            Some(t.to_owned())
        };
        assert_eq!(cache.get_or(1, 0.5, || ask("a")).as_deref(), Some("a"));
        assert_eq!(cache.get_or(1, 0.5, || ask("b")).as_deref(), Some("a"), "cached");
        assert_eq!(cache.get_or(1, 0.25, || ask("c")).as_deref(), Some("c"));
        assert_eq!(cache.get_or(2, 0.5, || None), None);
        assert_eq!(cache.get_or(2, 0.5, || ask("d")), None, "a missing text is cached too");
        assert_eq!(asked.get(), 2);
        for i in 0..(MAX_CACHED_TEXTS as u32 * 2) {
            cache.get_or(3, i as f32, || Some(String::new()));
            assert!(cache.len() <= MAX_CACHED_TEXTS);
        }
        cache.clear();
        assert_eq!(cache.len(), 0);
    }
}

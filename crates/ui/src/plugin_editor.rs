//! The editor rawDAWg's exported plugins show in other hosts: the wordmark, the plugin's
//! name and its parameters drawn with the same widgets as the app's effect and instrument
//! panels. Format-neutral: CLAP and VST3 adapters implement [`ParamPort`].

use crate::editor::EditorUi;
use crate::params::{param_control, step_labels, wordmark};
use crate::theme;
use rawdawg_types::{ParamDescriptor, ParamId};

/// Logical size the exported plugin editors open with (points).
pub const DEFAULT_SIZE: (u32, u32) = (640, 480);
/// Smallest size they accept.
pub const MIN_SIZE: (u32, u32) = (420, 260);

/// How an editor reads and changes a plugin's parameters. All calls come from the editor's
/// (the host's UI) thread; implementations forward edits to the audio thread and the host.
pub trait ParamPort {
    /// The parameters to show, in order.
    fn descriptors(&self) -> Vec<ParamDescriptor>;
    /// Current normalized value.
    fn value(&self, id: ParamId) -> Option<f32>;
    /// Display text for a value, when the format layer has better text than the descriptor.
    fn text(&self, _id: ParamId, _normalized: f32) -> Option<String> {
        None
    }
    /// A user gesture on `id` starts (hosts group automation by gesture).
    fn begin_edit(&self, id: ParamId);
    fn edit(&self, id: ParamId, normalized: f32);
    fn end_edit(&self, id: ParamId);
}

/// One step of an edit a [`EditorVisual`] makes: gestures begin, set values and end, as the
/// host expects them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VisualEdit {
    Begin(ParamId),
    Set(ParamId, f32),
    End(ParamId),
}

/// A plugin-specific view drawn above the parameter list (an EQ's response curve, a synth's
/// envelope): it reads values through the port and returns its edits, which the editor sends
/// to the host (and ends if the window closes mid-gesture).
pub trait EditorVisual {
    fn ui(&mut self, ui: &mut egui::Ui, port: &dyn ParamPort) -> Vec<VisualEdit>;
}

pub struct PluginEditorUi<P: ParamPort> {
    name: String,
    port: P,
    gesture: Option<ParamId>,
    descriptors: Vec<ParamDescriptor>,
    visual: Option<Box<dyn EditorVisual>>,
    /// Gestures a visual began and has not ended yet.
    visual_gestures: Vec<ParamId>,
}

impl<P: ParamPort> PluginEditorUi<P> {
    pub fn new(name: impl Into<String>, port: P) -> Self {
        let descriptors = port.descriptors();
        Self { name: name.into(), port, gesture: None, descriptors, visual: None, visual_gestures: Vec::new() }
    }

    /// Show `visual` above the parameter list.
    pub fn with_visual(mut self, visual: Option<Box<dyn EditorVisual>>) -> Self {
        self.visual = visual;
        self
    }

    fn apply_visual(&mut self, edits: Vec<VisualEdit>) {
        for e in edits {
            match e {
                VisualEdit::Begin(id) => {
                    self.end_gesture();
                    if !self.visual_gestures.contains(&id) {
                        self.port.begin_edit(id);
                        self.visual_gestures.push(id);
                    }
                }
                VisualEdit::Set(id, v) => self.port.edit(id, v.clamp(0.0, 1.0)),
                VisualEdit::End(id) => {
                    if let Some(i) = self.visual_gestures.iter().position(|g| *g == id) {
                        self.visual_gestures.remove(i);
                        self.port.end_edit(id);
                    }
                }
            }
        }
    }

    pub fn port(&self) -> &P {
        &self.port
    }

    fn readout(&self, d: &ParamDescriptor, n: f32) -> String {
        self.port.text(d.id, n).unwrap_or_else(|| d.format(n))
    }
}

impl<P: ParamPort> EditorUi for PluginEditorUi<P> {
    fn ui(&mut self, root: &mut egui::Ui) {
        let frame = egui::Frame::new().fill(theme::PANEL).inner_margin(egui::Margin::same(12));
        frame.show(root, |ui| {
            ui.set_min_size(ui.available_size());
            ui.horizontal(|ui| {
                wordmark(ui, 20.0);
                ui.add_space(10.0);
                ui.label(egui::RichText::new(&self.name).size(16.0).color(theme::TEXT));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(rawdawg_types::TAGLINE).small().color(theme::TEXT_SECONDARY));
                });
            });
            ui.separator();
            if let Some(mut visual) = self.visual.take() {
                let edits = visual.ui(ui, &self.port);
                self.visual = Some(visual);
                self.apply_visual(edits);
                ui.separator();
            }
            let descriptors = self.descriptors.clone();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                egui::Grid::new("plugin-params").num_columns(3).spacing([12.0, 6.0]).striped(true).show(ui, |ui| {
                    for d in &descriptors {
                        let Some(current) = self.port.value(d.id) else { continue };
                        ui.label(d.name);
                        let labels = step_labels(d, |n| self.port.text(d.id, n));
                        let (resp, n) = param_control(ui, ("plugin-param", d.id.0), d, current, labels.as_deref());
                        ui.label(egui::RichText::new(self.readout(d, n)).monospace().color(theme::TEXT_SECONDARY));
                        ui.end_row();
                        if resp.drag_started() {
                            self.end_gesture();
                            self.port.begin_edit(d.id);
                            self.gesture = Some(d.id);
                        }
                        if resp.changed() && (n - current).abs() > 1e-7 {
                            if self.gesture == Some(d.id) {
                                self.port.edit(d.id, n);
                            } else {
                                // a click or a choice: a whole gesture at once
                                self.port.begin_edit(d.id);
                                self.port.edit(d.id, n);
                                self.port.end_edit(d.id);
                            }
                        }
                        if resp.drag_stopped() && self.gesture == Some(d.id) {
                            self.end_gesture();
                        }
                    }
                });
            });
        });
    }
}

impl<P: ParamPort> PluginEditorUi<P> {
    fn end_gesture(&mut self) {
        if let Some(id) = self.gesture.take() {
            self.port.end_edit(id);
        }
    }
}

impl<P: ParamPort> Drop for PluginEditorUi<P> {
    fn drop(&mut self) {
        // closing the editor mid-drag still ends the host's gesture
        self.end_gesture();
        for id in std::mem::take(&mut self.visual_gestures) {
            self.port.end_edit(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Pos2, Rect, pos2, vec2};
    use rawdawg_types::params::ParamCategory;
    use rawdawg_types::{ParamMapping, ParamUnit};
    use std::cell::RefCell;
    use std::rc::Rc;

    static DESCRIPTORS: [ParamDescriptor; 3] = [
        ParamDescriptor::continuous(1, "gain", "Gain", ParamUnit::Decibel, ParamMapping::Linear { min: -24.0, max: 24.0 }, 0.5, ParamCategory::Effects),
        ParamDescriptor::continuous(2, "on", "Enabled", ParamUnit::None, ParamMapping::Toggle, 1.0, ParamCategory::Effects),
        ParamDescriptor::continuous(3, "mode", "Mode", ParamUnit::None, ParamMapping::Discrete { steps: 3 }, 0.0, ParamCategory::Effects),
    ];

    /// Values live in the port, like a plugin's state.
    #[derive(Clone)]
    struct Port {
        log: Rc<RefCell<Vec<String>>>,
        values: Rc<RefCell<Vec<f32>>>,
    }

    impl ParamPort for Port {
        fn descriptors(&self) -> Vec<ParamDescriptor> {
            DESCRIPTORS.to_vec()
        }
        fn value(&self, id: ParamId) -> Option<f32> {
            DESCRIPTORS.iter().position(|d| d.id == id).map(|i| self.values.borrow()[i])
        }
        fn text(&self, id: ParamId, n: f32) -> Option<String> {
            (id.0 == 3).then(|| ["Soft", "Hard", "Fold"][(n * 2.0).round() as usize].to_owned())
        }
        fn begin_edit(&self, id: ParamId) {
            self.log.borrow_mut().push(format!("begin {}", id.0));
        }
        fn edit(&self, id: ParamId, n: f32) {
            let i = DESCRIPTORS.iter().position(|d| d.id == id).unwrap();
            self.values.borrow_mut()[i] = n;
            self.log.borrow_mut().push(format!("edit {}", id.0));
        }
        fn end_edit(&self, id: ParamId) {
            self.log.borrow_mut().push(format!("end {}", id.0));
        }
    }

    struct Driver {
        ctx: egui::Context,
        ui: PluginEditorUi<Port>,
    }

    impl Driver {
        /// One frame; returns (accessibility role, label/value text, bounds) per widget.
        fn frame(&mut self, events: Vec<egui::Event>) -> Vec<(egui::accesskit::Role, String, Rect)> {
            let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(640.0, 480.0))), events, ..Default::default() };
            let ui = &mut self.ui;
            let mut out = self.ctx.run_ui(input, |root| ui.ui(root));
            out.textures_delta.clear();
            out.platform_output
                .accesskit_update
                .map(|u| {
                    u.nodes
                        .iter()
                        .map(|(_, n)| {
                            let b = n.bounds().unwrap_or_default();
                            let text = n.label().or(n.value()).unwrap_or_default().to_owned();
                            (n.role(), text, Rect::from_min_max(pos2(b.x0 as f32, b.y0 as f32), pos2(b.x1 as f32, b.y1 as f32)))
                        })
                        .collect()
                })
                .unwrap_or_default()
        }
    }

    fn press(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
    }

    #[test]
    fn drags_clicks_and_choices_reach_the_port_as_complete_gestures() {
        let port = Port { log: Rc::default(), values: Rc::new(RefCell::new(DESCRIPTORS.iter().map(|d| d.default_normalized).collect())) };
        let mut d = Driver { ctx: egui::Context::default(), ui: PluginEditorUi::new("Test", port.clone()) };
        crate::theme::apply(&d.ctx);
        d.ctx.enable_accesskit();
        d.frame(vec![]);
        let nodes = d.frame(vec![]);
        let find = |role: egui::accesskit::Role| nodes.iter().find(|n| n.0 == role).map(|n| n.2).unwrap_or_else(|| panic!("{role:?} not drawn: {nodes:?}"));
        assert!(nodes.iter().any(|n| n.1.contains("Test")), "the plugin name is shown");
        assert!(nodes.iter().any(|n| n.1 == "0.0 dB"), "readouts use the descriptor's formatting");
        // drag the gain slider to the right: one gesture with several edits
        let slider = find(egui::accesskit::Role::Slider);
        let start = slider.center();
        d.frame(vec![egui::Event::PointerMoved(start), press(start, true)]);
        for k in 1..=5 {
            d.frame(vec![egui::Event::PointerMoved(start + vec2(12.0 * k as f32, 0.0))]);
        }
        d.frame(vec![press(start + vec2(60.0, 0.0), false)]);
        d.frame(vec![]);
        let log = port.log.borrow().clone();
        assert_eq!(log.first().map(String::as_str), Some("begin 1"), "{log:?}");
        assert_eq!(log.last().map(String::as_str), Some("end 1"), "{log:?}");
        assert!(log.iter().filter(|l| *l == "edit 1").count() >= 2, "{log:?}");
        assert!(port.values.borrow()[0] > 0.5, "the value moved right: {}", port.values.borrow()[0]);
        port.log.borrow_mut().clear();
        // a checkbox click: begin, edit, end at once
        let nodes = d.frame(vec![]);
        let check = nodes.iter().find(|n| n.0 == egui::accesskit::Role::CheckBox).map(|n| n.2).expect("checkbox");
        d.frame(vec![egui::Event::PointerMoved(check.center()), press(check.center(), true)]);
        d.frame(vec![press(check.center(), false)]);
        d.frame(vec![]);
        assert_eq!(*port.log.borrow(), vec!["begin 2", "edit 2", "end 2"]);
        assert_eq!(port.values.borrow()[1], 0.0);
        // the discrete parameter is a list named by the port's text
        let nodes = d.frame(vec![]);
        assert!(nodes.iter().any(|n| n.1 == "Soft"), "choice list shows the step name");
    }
}

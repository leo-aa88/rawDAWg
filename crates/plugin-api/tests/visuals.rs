//! PLUG-01: the exported EQ's and synth's editors show their curves and edit through the
//! parameter port with complete host gestures (pointer events driven headless).

use egui::{Pos2, Rect, pos2, vec2};
use rawdawg_dsp::BusLayout;
use rawdawg_dsp::processor::{PrepareSpec, Processor};
use rawdawg_plugin_api::visuals::{envelope_canvas_id, eq_canvas_id, filter_canvas_id, visual_for};
use rawdawg_types::{ParamDescriptor, ParamId};
use rawdawg_ui::editor::EditorUi;
use rawdawg_ui::plugin_editor::{ParamPort, PluginEditorUi};
use std::cell::RefCell;
use std::rc::Rc;

/// A port over a real native processor (the plugin's own parameters and values), logging the
/// host-facing gesture calls.
#[derive(Clone)]
struct Port(Rc<Inner>);

struct Inner {
    processor: RefCell<Box<dyn Processor>>,
    log: RefCell<Vec<(char, u32)>>,
}

impl Port {
    fn new(clap_id: &str) -> Self {
        let mut p = rawdawg_plugin_api::exported_plugins().into_iter().find(|p| p.clap_id == clap_id).unwrap().make().unwrap();
        p.prepare(&PrepareSpec::new(48_000.0, 256, BusLayout::Stereo)).unwrap();
        Port(Rc::new(Inner { processor: RefCell::new(p), log: RefCell::new(Vec::new()) }))
    }
    fn phys(&self, id: u32) -> f64 {
        let p = self.0.processor.borrow();
        let d = p.descriptors().iter().find(|d| d.id.0 == id).unwrap();
        d.to_physical(p.param(d.id).unwrap())
    }
    /// Every begun gesture was ended, and every edit happened inside one.
    fn gestures_balanced(&self) -> bool {
        let mut open: Vec<u32> = Vec::new();
        for (k, id) in self.0.log.borrow().iter() {
            match k {
                'b' => open.push(*id),
                'e' => {
                    if !open.contains(id) {
                        return false;
                    }
                }
                _ => match open.iter().position(|o| o == id) {
                    Some(i) => {
                        open.remove(i);
                    }
                    None => return false,
                },
            }
        }
        open.is_empty()
    }
}

impl ParamPort for Port {
    fn descriptors(&self) -> Vec<ParamDescriptor> {
        self.0.processor.borrow().descriptors().iter().filter(|d| !d.flags.hidden).copied().collect()
    }
    fn value(&self, id: ParamId) -> Option<f32> {
        self.0.processor.borrow().param(id)
    }
    fn begin_edit(&self, id: ParamId) {
        self.0.log.borrow_mut().push(('b', id.0));
    }
    fn edit(&self, id: ParamId, normalized: f32) {
        self.0.processor.borrow_mut().set_param(id, normalized);
        self.0.log.borrow_mut().push(('e', id.0));
    }
    fn end_edit(&self, id: ParamId) {
        self.0.log.borrow_mut().push(('x', id.0));
    }
}

struct Driver {
    ctx: egui::Context,
    ui: Option<PluginEditorUi<Port>>,
}

impl Driver {
    fn new(clap_id: &str, port: Port) -> Self {
        let ui = PluginEditorUi::new(clap_id, port).with_visual(visual_for(clap_id));
        let mut d = Self { ctx: egui::Context::default(), ui: Some(ui) };
        d.frame(vec![]);
        d.frame(vec![]);
        d
    }
    fn frame(&mut self, events: Vec<egui::Event>) {
        let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(640.0, 480.0))), events, ..Default::default() };
        let ui = self.ui.as_mut().unwrap();
        let mut out = self.ctx.run_ui(input, |root| ui.ui(root));
        out.textures_delta.clear();
    }
    fn rect(&self, id: egui::Id) -> Rect {
        self.ctx.read_response(id).expect("canvas drawn").rect
    }
    fn button(&mut self, pos: Pos2, button: egui::PointerButton, pressed: bool) {
        self.frame(vec![egui::Event::PointerButton { pos, button, pressed, modifiers: egui::Modifiers::NONE }]);
    }
    fn click(&mut self, pos: Pos2, button: egui::PointerButton) {
        self.frame(vec![egui::Event::PointerMoved(pos)]);
        self.button(pos, button, true);
        self.button(pos, button, false);
        self.frame(vec![]);
    }
    fn drag(&mut self, from: Pos2, to: Pos2) {
        self.frame(vec![egui::Event::PointerMoved(from)]);
        self.button(from, egui::PointerButton::Primary, true);
        for i in 1..=6 {
            self.frame(vec![egui::Event::PointerMoved(from + (to - from) * (i as f32 / 6.0))]);
        }
        self.button(to, egui::PointerButton::Primary, false);
        self.frame(vec![]);
    }
}

fn freq_x(r: Rect, f: f32) -> f32 {
    r.min.x + r.width() * ((f / 20.0).log10() / 1000f32.log10())
}
fn db_y(r: Rect, db: f32) -> f32 {
    r.center().y - db / 24.0 * r.height() * 0.5
}

#[test]
fn the_eq_window_adds_drags_and_removes_bands_on_its_response() {
    use rawdawg_effects::eq::{B_ENABLED, B_FREQ, B_GAIN, BANDS, band_param};
    let port = Port::new("rawdawg.fx.eq");
    let mut d = Driver::new("rawdawg.fx.eq", port.clone());
    let r = d.rect(eq_canvas_id());
    let free = (0..BANDS).find(|b| port.phys(band_param(*b, B_ENABLED)) < 0.5).expect("a free band");
    // click on empty space at 1 kHz, +6 dB: that band switches on there
    d.click(pos2(freq_x(r, 1000.0), db_y(r, 6.0)), egui::PointerButton::Primary);
    assert!(port.phys(band_param(free, B_ENABLED)) > 0.5, "band {free} on");
    assert!((port.phys(band_param(free, B_FREQ)) / 1000.0 - 1.0).abs() < 0.03, "{}", port.phys(band_param(free, B_FREQ)));
    assert!((port.phys(band_param(free, B_GAIN)) - 6.0).abs() < 0.3, "{}", port.phys(band_param(free, B_GAIN)));
    // drag its handle to 4 kHz, −3 dB: frequency and gain follow, in one gesture each
    port.0.log.borrow_mut().clear();
    d.drag(pos2(freq_x(r, 1000.0), db_y(r, 6.0)), pos2(freq_x(r, 4000.0), db_y(r, -3.0)));
    assert!((port.phys(band_param(free, B_FREQ)) / 4000.0 - 1.0).abs() < 0.03);
    assert!((port.phys(band_param(free, B_GAIN)) + 3.0).abs() < 0.3);
    let begins = port.0.log.borrow().iter().filter(|(k, _)| *k == 'b').count();
    assert_eq!(begins, 2, "one gesture for frequency, one for gain: {:?}", port.0.log.borrow());
    assert!(port.gestures_balanced(), "{:?}", port.0.log.borrow());
    // right-click it: off
    d.click(pos2(freq_x(r, 4000.0), db_y(r, -3.0)), egui::PointerButton::Secondary);
    assert!(port.phys(band_param(free, B_ENABLED)) < 0.5, "band switched off");
    assert!(port.gestures_balanced());
}

#[test]
fn the_synth_window_shapes_the_envelope_and_the_filter_and_ends_gestures_on_close() {
    use rawdawg_synthesizer::params as sp;
    let port = Port::new("rawdawg.synth");
    let mut d = Driver::new("rawdawg.synth", port.clone());
    // the same envelope editor as in rawDAWg: its points are where the shared widget puts them
    use rawdawg_ui::envelope::{Adsr, AdsrPoint, curve_rect, point_pos};
    let env = curve_rect(d.rect(envelope_canvas_id()));
    let adsr = |port: &Port| Adsr {
        attack_s: port.phys(sp::AMP_ATTACK) as f32,
        decay_s: port.phys(sp::AMP_DECAY) as f32,
        sustain: port.phys(sp::AMP_SUSTAIN) as f32,
        release_s: port.phys(sp::AMP_RELEASE) as f32,
    };
    let s = adsr(&port).sustain;
    // sustain up (the point at the end of the sustain)
    let sustain_handle = point_pos(env, &adsr(&port), AdsrPoint::Sustain);
    d.drag(sustain_handle, sustain_handle + vec2(0.0, -30.0));
    assert!(port.phys(sp::AMP_SUSTAIN) as f32 > s + 0.05, "sustain {s} → {}", port.phys(sp::AMP_SUSTAIN));
    assert!(port.gestures_balanced(), "{:?}", port.0.log.borrow());
    // release shorter: the envelope's end to the left
    let before = port.phys(sp::AMP_RELEASE);
    let release_handle = point_pos(env, &adsr(&port), AdsrPoint::Release);
    d.drag(release_handle, release_handle + vec2(-40.0, 0.0));
    assert!(port.phys(sp::AMP_RELEASE) < before, "dragging the end left shortens the release: {before} → {}", port.phys(sp::AMP_RELEASE));
    // attack longer: its peak to the right
    let before = port.phys(sp::AMP_ATTACK);
    let attack_handle = point_pos(env, &adsr(&port), AdsrPoint::Attack);
    d.drag(attack_handle, attack_handle + vec2(30.0, 0.0));
    assert!(port.phys(sp::AMP_ATTACK) > before, "attack {before} → {}", port.phys(sp::AMP_ATTACK));
    assert!(port.gestures_balanced(), "{:?}", port.0.log.borrow());
    // the filter handle: left lowers the cutoff
    let filt = d.rect(filter_canvas_id());
    let fc = port.phys(sp::FILTER_CUTOFF) as f32;
    let fx = freq_x(filt, fc.clamp(20.0, 20_000.0));
    let q = port.phys(sp::FILTER_RESONANCE) as f32;
    let peak_db = (20.0 * (q.max(0.1)).log10()).max(0.0);
    let handle = pos2(fx, db_y(filt, peak_db.min(24.0)));
    port.0.log.borrow_mut().clear();
    d.drag(handle, pos2(freq_x(filt, 500.0), handle.y));
    assert!((port.phys(sp::FILTER_CUTOFF) as f32 / 500.0 - 1.0).abs() < 0.05, "cutoff {fc} → {}", port.phys(sp::FILTER_CUTOFF));
    assert!(port.gestures_balanced(), "{:?}", port.0.log.borrow());
    // closing the window mid-drag still ends the host's gesture
    port.0.log.borrow_mut().clear();
    let start = pos2(freq_x(filt, 500.0), handle.y);
    d.frame(vec![egui::Event::PointerMoved(start)]);
    d.button(start, egui::PointerButton::Primary, true);
    d.frame(vec![egui::Event::PointerMoved(start + vec2(20.0, 0.0))]);
    d.frame(vec![egui::Event::PointerMoved(start + vec2(40.0, 0.0))]);
    assert!(port.0.log.borrow().iter().any(|(k, _)| *k == 'b'), "a gesture is open");
    d.ui = None;
    assert!(port.gestures_balanced(), "closing ended it: {:?}", port.0.log.borrow());
}

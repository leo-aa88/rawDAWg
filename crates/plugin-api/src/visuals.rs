//! Plugin-specific views for the exported plugins' editors (PLUG-01): the parametric EQ's
//! response with draggable bands, and the synth's amp envelope and filter curve. They read and
//! write only through the editor's [`ParamPort`], so hosts see ordinary parameter gestures.

use rawdawg_dsp::BusLayout;
use rawdawg_dsp::processor::{PrepareSpec, Processor};
use rawdawg_effects::eq::{B_ENABLED, B_FREQ, B_GAIN, B_KIND, B_Q, BANDS, KIND_LABELS, ParametricEq, band_param};
use rawdawg_synthesizer::params as sp;
use rawdawg_types::{ParamDescriptor, ParamId};
use rawdawg_ui::plugin_editor::{EditorVisual, ParamPort, VisualEdit};
use rawdawg_ui::theme;

/// The visual for an exported plugin, by its CLAP id.
pub fn visual_for(clap_id: &str) -> Option<Box<dyn EditorVisual>> {
    match clap_id {
        "rawdawg.fx.eq" => Some(Box::new(EqVisual::new())),
        "rawdawg.synth" => Some(Box::new(SynthVisual::default())),
        _ => None,
    }
}

/// Widget ids of the canvases (one editor per window, so they are unique there).
pub fn eq_canvas_id() -> egui::Id {
    egui::Id::unique("rawdawg-eq-response")
}
pub fn envelope_canvas_id() -> egui::Id {
    egui::Id::unique("rawdawg-synth-envelope")
}
pub fn filter_canvas_id() -> egui::Id {
    egui::Id::unique("rawdawg-synth-filter")
}

const F_MIN: f32 = 20.0;
const F_MAX: f32 = 20_000.0;
const DB_RANGE: f32 = 24.0;

fn freq_to_x(r: egui::Rect, f: f32) -> f32 {
    r.min.x + r.width() * ((f.clamp(F_MIN, F_MAX) / F_MIN).log10() / (F_MAX / F_MIN).log10())
}
fn x_to_freq(r: egui::Rect, x: f32) -> f32 {
    F_MIN * (F_MAX / F_MIN).powf(((x - r.min.x) / r.width()).clamp(0.0, 1.0))
}
fn db_to_y(r: egui::Rect, db: f32) -> f32 {
    r.center().y - (db.clamp(-DB_RANGE, DB_RANGE) / DB_RANGE) * r.height() * 0.5
}
fn y_to_db(r: egui::Rect, y: f32) -> f32 {
    ((r.center().y - y) / (r.height() * 0.5) * DB_RANGE).clamp(-DB_RANGE, DB_RANGE)
}

fn descriptor(port: &dyn ParamPort, id: u32) -> Option<ParamDescriptor> {
    port.descriptors().into_iter().find(|d| d.id.0 == id)
}

/// Physical value of a parameter as the port has it now.
fn phys(port: &dyn ParamPort, id: u32) -> Option<f64> {
    let d = descriptor(port, id)?;
    Some(d.to_physical(port.value(d.id)?))
}

/// A whole gesture that sets `id` to the physical value `v`.
fn set_once(port: &dyn ParamPort, id: u32, v: f64, out: &mut Vec<VisualEdit>) {
    if let Some(d) = descriptor(port, id) {
        out.extend([VisualEdit::Begin(d.id), VisualEdit::Set(d.id, d.to_normalized(v)), VisualEdit::End(d.id)]);
    }
}

fn set(port: &dyn ParamPort, id: u32, v: f64, out: &mut Vec<VisualEdit>) {
    if let Some(d) = descriptor(port, id) {
        out.push(VisualEdit::Set(d.id, d.to_normalized(v)));
    }
}

fn grid(painter: &egui::Painter, rect: egui::Rect) {
    painter.rect_filled(rect, 4.0, theme::BACKGROUND);
    for f in [50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10_000.0] {
        let x = freq_to_x(rect, f);
        painter.line_segment([egui::pos2(x, rect.min.y), egui::pos2(x, rect.max.y)], egui::Stroke::new(1.0, theme::BORDER.linear_multiply(0.5)));
        let label = if f >= 1000.0 { format!("{}k", f / 1000.0) } else { format!("{f}") };
        painter.text(egui::pos2(x + 2.0, rect.max.y - 2.0), egui::Align2::LEFT_BOTTOM, label, egui::FontId::proportional(9.0), theme::TEXT_SECONDARY);
    }
    for db in [-12.0, 0.0, 12.0] {
        let y = db_to_y(rect, db);
        painter.line_segment([egui::pos2(rect.min.x, y), egui::pos2(rect.max.x, y)], egui::Stroke::new(1.0, theme::BORDER.linear_multiply(if db == 0.0 { 1.0 } else { 0.5 })));
    }
}

/// The parametric EQ: its response, a handle per band; drag a handle for frequency and gain,
/// wheel for Q, click empty space to switch a free band on there, right-click to switch off.
pub struct EqVisual {
    eq: ParametricEq,
    drag: Option<usize>,
}

impl EqVisual {
    pub fn new() -> Self {
        let mut eq = ParametricEq::new();
        let _ = eq.prepare(&PrepareSpec::new(48_000.0, 64, BusLayout::Stereo));
        Self { eq, drag: None }
    }

    /// Handle position of each band: (band, enabled, position, kind).
    fn handles(&self, rect: egui::Rect) -> Vec<(usize, bool, egui::Pos2, usize)> {
        let p = self.eq.params();
        (0..BANDS)
            .map(|b| {
                let kind = p.as_index(band_param(b, B_KIND));
                let gain = if matches!(kind, 3..=5) { 0.0 } else { p.phys(band_param(b, B_GAIN)) };
                (b, p.as_bool(band_param(b, B_ENABLED)), egui::pos2(freq_to_x(rect, p.phys(band_param(b, B_FREQ))), db_to_y(rect, gain)), kind)
            })
            .collect()
    }
}

impl Default for EqVisual {
    fn default() -> Self {
        Self::new()
    }
}

impl EditorVisual for EqVisual {
    fn ui(&mut self, ui: &mut egui::Ui, port: &dyn ParamPort) -> Vec<VisualEdit> {
        let mut out = Vec::new();
        // mirror the plugin's values so the drawn response is the plugin's
        for d in self.eq.descriptors() {
            if let Some(v) = port.value(d.id) {
                self.eq.set_param(d.id, v);
            }
        }
        let (_, rect) = ui.allocate_space(egui::vec2(ui.available_width().max(300.0), 180.0));
        let resp = ui.interact(rect, eq_canvas_id(), egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        grid(&painter, rect);
        let curve: Vec<egui::Pos2> = (0..=160)
            .map(|i| {
                let f = F_MIN * (F_MAX / F_MIN).powf(i as f32 / 160.0);
                egui::pos2(freq_to_x(rect, f), db_to_y(rect, self.eq.response_db(f)))
            })
            .collect();
        painter.add(egui::Shape::line(curve, egui::Stroke::new(2.0, theme::ACCENT)));
        let handles = self.handles(rect);
        for (b, on, pos, _) in &handles {
            painter.circle(
                *pos,
                if *on { 6.0 } else { 4.0 },
                if *on { theme::WARNING } else { theme::TEXT_SECONDARY.linear_multiply(0.4) },
                egui::Stroke::new(1.0, theme::BACKGROUND),
            );
            painter.text(*pos + egui::vec2(0.0, -10.0), egui::Align2::CENTER_BOTTOM, format!("{}", b + 1), egui::FontId::proportional(9.0), theme::TEXT);
        }
        let near = |p: egui::Pos2| handles.iter().filter(|h| (h.2 - p).length() < 9.0).min_by(|a, b| (a.2 - p).length().total_cmp(&(b.2 - p).length())).map(|h| h.0);
        let id = |b: usize, o: u32| descriptor(port, band_param(b, o)).map(|d| d.id);
        // a drag starts where the button went down (egui reports it after a few pixels of movement)
        let origin = ui.input(|i| i.pointer.press_origin());
        if resp.drag_started()
            && let Some(p) = origin.or(resp.interact_pointer_pos())
        {
            let band = near(p).or_else(|| handles.iter().find(|h| !h.1).map(|h| h.0));
            if let Some(b) = band {
                if !handles[b].1 {
                    set_once(port, band_param(b, B_ENABLED), 1.0, &mut out);
                }
                self.drag = Some(b);
                out.extend([id(b, B_FREQ), id(b, B_GAIN)].into_iter().flatten().map(VisualEdit::Begin));
            }
        }
        if let Some(b) = self.drag
            && resp.dragged()
            && let Some(p) = resp.interact_pointer_pos()
        {
            set(port, band_param(b, B_FREQ), f64::from(x_to_freq(rect, p.x)), &mut out);
            if !matches!(handles[b].3, 3..=5) {
                set(port, band_param(b, B_GAIN), f64::from(y_to_db(rect, p.y)), &mut out);
            }
        }
        if resp.drag_stopped()
            && let Some(b) = self.drag.take()
        {
            out.extend([id(b, B_FREQ), id(b, B_GAIN)].into_iter().flatten().map(VisualEdit::End));
        }
        // a click on empty space switches a free band on there (a bell)
        if resp.clicked()
            && let Some(p) = resp.interact_pointer_pos()
            && near(p).is_none()
            && let Some(b) = handles.iter().find(|h| !h.1).map(|h| h.0)
        {
            set_once(port, band_param(b, B_ENABLED), 1.0, &mut out);
            set_once(port, band_param(b, B_KIND), 0.0, &mut out);
            set_once(port, band_param(b, B_FREQ), f64::from(x_to_freq(rect, p.x)), &mut out);
            set_once(port, band_param(b, B_GAIN), f64::from(y_to_db(rect, p.y)), &mut out);
        }
        if resp.secondary_clicked()
            && let Some(p) = resp.interact_pointer_pos()
            && let Some(b) = near(p)
        {
            set_once(port, band_param(b, B_ENABLED), 0.0, &mut out);
        }
        if let Some(p) = resp.hover_pos()
            && let Some(b) = near(p)
        {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0
                && let Some(q) = phys(port, band_param(b, B_Q))
            {
                set_once(port, band_param(b, B_Q), (q * (1.0 + f64::from(scroll.signum()) * 0.1)).clamp(0.1, 18.0), &mut out);
            }
            let p = self.eq.params();
            resp.clone().on_hover_text_at_pointer(format!(
                "Band {}: {} · {:.0} Hz · {:+.1} dB · Q {:.2}\ndrag: frequency/gain · wheel: Q · right-click: off",
                b + 1,
                KIND_LABELS[p.as_index(band_param(b, B_KIND)).min(5)],
                p.phys(band_param(b, B_FREQ)),
                p.phys(band_param(b, B_GAIN)),
                p.phys(band_param(b, B_Q))
            ));
        }
        out
    }
}

/// The synth: the amp envelope (the app's own editor: drag its attack, decay, sustain and
/// release points) and the filter's response with a handle for cutoff (left/right) and
/// resonance (up/down).
#[derive(Default)]
pub struct SynthVisual {
    /// Filter handle drag: the last pointer position.
    drag: Option<(f32, f32)>,
}

/// Magnitude (dB) of the synth's state-variable filter at `f` (mode 0 low-pass, 1 high-pass,
/// 2 band-pass, 3 notch).
fn svf_db(mode: usize, fc: f32, q: f32, f: f32) -> f32 {
    let x = f / fc.max(1.0);
    let q = q.max(0.1);
    let den = ((1.0 - x * x).powi(2) + (x / q).powi(2)).sqrt().max(1e-9);
    let mag = match mode {
        1 => x * x / den,
        2 => (x / q) / den,
        3 => (1.0 - x * x).abs() / den,
        _ => 1.0 / den,
    };
    20.0 * mag.max(1e-6).log10()
}

impl EditorVisual for SynthVisual {
    fn ui(&mut self, ui: &mut egui::Ui, port: &dyn ParamPort) -> Vec<VisualEdit> {
        let mut out = Vec::new();
        let v = |id: u32| phys(port, id).unwrap_or(0.0) as f32;
        let (a, d, s, r) = (v(sp::AMP_ATTACK), v(sp::AMP_DECAY), v(sp::AMP_SUSTAIN), v(sp::AMP_RELEASE));
        let (fc, q, mode, filter_on) = (v(sp::FILTER_CUTOFF), v(sp::FILTER_RESONANCE), v(sp::FILTER_MODE) as usize, v(sp::FILTER_ENABLED) >= 0.5);
        let width = ui.available_width().max(300.0);
        let half = width * 0.5 - 6.0;
        let (env_drag, filt_rect) = ui
            .horizontal(|ui| {
                let adsr = rawdawg_ui::envelope::Adsr { attack_s: a, decay_s: d, sustain: s, release_s: r };
                let drag = ui
                    .allocate_ui(egui::vec2(half, 150.0), |ui| {
                        ui.set_width(half);
                        rawdawg_ui::envelope::editor(ui, envelope_canvas_id(), &adsr, 150.0)
                    })
                    .inner;
                let (_, filt) = ui.allocate_space(egui::vec2(half, 150.0));
                (drag, filt)
            })
            .inner;
        // amp envelope: the dragged point's parameters follow the pointer (one gesture each)
        if let Some(drag) = env_drag {
            use rawdawg_ui::envelope::AdsrPoint;
            let moves: &[(u32, f32)] = &match drag.point {
                AdsrPoint::Attack => [(sp::AMP_ATTACK, drag.dx), (0, 0.0)],
                AdsrPoint::Decay => [(sp::AMP_DECAY, drag.dx), (sp::AMP_SUSTAIN, drag.dy)],
                AdsrPoint::Sustain => [(sp::AMP_SUSTAIN, drag.dy), (0, 0.0)],
                AdsrPoint::Release => [(sp::AMP_RELEASE, drag.dx), (0, 0.0)],
            };
            for &(pid, delta) in moves.iter().filter(|(pid, _)| *pid != 0) {
                let Some(dsc) = descriptor(port, pid) else { continue };
                if drag.started {
                    out.push(VisualEdit::Begin(dsc.id));
                }
                if delta != 0.0
                    && let Some(n) = port.value(dsc.id)
                {
                    out.push(VisualEdit::Set(dsc.id, (n + delta).clamp(0.0, 1.0)));
                }
                if drag.stopped {
                    out.push(VisualEdit::End(dsc.id));
                }
            }
        }
        // filter
        let filt_resp = ui.interact(filt_rect, filter_canvas_id(), egui::Sense::drag());
        let painter = ui.painter_at(filt_rect);
        grid(&painter, filt_rect);
        let curve: Vec<egui::Pos2> = (0..=120)
            .map(|i| {
                let f = F_MIN * (F_MAX / F_MIN).powf(i as f32 / 120.0);
                egui::pos2(freq_to_x(filt_rect, f), db_to_y(filt_rect, if filter_on { svf_db(mode, fc, q, f) } else { 0.0 }))
            })
            .collect();
        painter.add(egui::Shape::line(curve, egui::Stroke::new(2.0, if filter_on { theme::ACCENT } else { theme::TEXT_SECONDARY })));
        let handle = egui::pos2(freq_to_x(filt_rect, fc), db_to_y(filt_rect, svf_db(0, fc, q, fc).max(0.0)));
        painter.circle(handle, 5.0, theme::WARNING, egui::Stroke::new(1.0, theme::BACKGROUND));
        painter.text(
            filt_rect.left_top() + egui::vec2(6.0, 4.0),
            egui::Align2::LEFT_TOP,
            if filter_on { "Filter" } else { "Filter (off)" },
            egui::FontId::proportional(11.0),
            theme::TEXT_SECONDARY,
        );
        // the filter handle (within 14 px): cutoff follows the pointer, resonance moves with it
        let ids: Vec<ParamId> = [sp::FILTER_CUTOFF, sp::FILTER_RESONANCE].into_iter().filter_map(|p| descriptor(port, p).map(|d| d.id)).collect();
        if filt_resp.drag_started()
            && let Some(p) = ui.input(|i| i.pointer.press_origin()).or(filt_resp.interact_pointer_pos())
            && (handle - p).length() < 14.0
        {
            self.drag = Some((p.x, p.y));
            out.extend(ids.iter().copied().map(VisualEdit::Begin));
        }
        if let Some((_, y0)) = self.drag
            && filt_resp.dragged()
            && let Some(p) = filt_resp.interact_pointer_pos()
        {
            set(port, sp::FILTER_CUTOFF, f64::from(x_to_freq(filt_rect, p.x)), &mut out);
            if let Some(dsc) = descriptor(port, sp::FILTER_RESONANCE)
                && let Some(n) = port.value(dsc.id)
            {
                out.push(VisualEdit::Set(dsc.id, (n + (y0 - p.y) / filt_rect.height()).clamp(0.0, 1.0)));
            }
            self.drag = Some((p.x, p.y));
        }
        if filt_resp.drag_stopped() && self.drag.take().is_some() {
            out.extend(ids.into_iter().map(VisualEdit::End));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axes_round_trip_and_the_filter_curve_has_its_shape() {
        let r = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(400.0, 200.0));
        for f in [20.0f32, 100.0, 1000.0, 15_000.0] {
            assert!((x_to_freq(r, freq_to_x(r, f)) / f - 1.0).abs() < 1e-3);
        }
        assert!((y_to_db(r, db_to_y(r, -6.0)) + 6.0).abs() < 1e-4);
        // low-pass: flat below, −12 dB/octave above; resonance peaks at the cutoff
        assert!(svf_db(0, 1000.0, 0.707, 50.0).abs() < 0.1);
        assert!((svf_db(0, 1000.0, 0.707, 8000.0) + 36.0).abs() < 1.5);
        assert!(svf_db(0, 1000.0, 4.0, 1000.0) > 11.0);
        assert!(svf_db(1, 1000.0, 0.707, 50.0) < -40.0, "high-pass removes the lows");
        assert!(svf_db(3, 1000.0, 0.707, 1000.0) < -60.0, "notch at the cutoff");
    }
}

//! The ADSR envelope editor: the curve over a grid with its area filled, four points to drag
//! (attack peak, decay end, sustain end, release end) and the segment names underneath. The
//! caller owns the values; the widget reports how the dragged point moved, so the caller can
//! turn it into parameter gestures (live values, one undo step per drag).

use crate::theme;
use egui::{Pos2, Rect, Sense, Stroke, pos2, vec2};

/// The envelope as drawn: times in seconds, sustain level 0..1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Adsr {
    pub attack_s: f32,
    pub decay_s: f32,
    pub sustain: f32,
    pub release_s: f32,
}

/// The points that can be dragged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdsrPoint {
    /// Attack peak: left/right sets the attack time.
    Attack,
    /// End of the decay: left/right the decay time, up/down the sustain level.
    Decay,
    /// End of the sustain: up/down the sustain level.
    Sustain,
    /// End of the release: left/right the release time.
    Release,
}

impl AdsrPoint {
    pub const ALL: [AdsrPoint; 4] = [AdsrPoint::Attack, AdsrPoint::Decay, AdsrPoint::Sustain, AdsrPoint::Release];
}

/// One frame of a point drag: the pointer's movement as fractions of the curve area (x to the
/// right, y up), and whether the drag starts or ends in this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AdsrDrag {
    pub point: AdsrPoint,
    pub dx: f32,
    pub dy: f32,
    pub started: bool,
    pub stopped: bool,
}

/// Width given to the sustain segment (in the same √seconds units as the others).
const HOLD: f32 = 0.6;
/// Space under the curve for the segment names.
const LABEL_SPACE: f32 = 20.0;
/// How close to a point a press must be to pick it up.
const GRAB: f32 = 16.0;

/// The area the curve is drawn in, inside the widget's rect.
pub fn curve_rect(rect: Rect) -> Rect {
    Rect::from_min_max(rect.min + vec2(14.0, 12.0), rect.max - vec2(14.0, LABEL_SPACE + 6.0))
}

/// The curve's corners: start, attack peak, decay end, sustain end, release end. Segment
/// widths follow the square root of their times, so a 1 ms attack and a 5 s release both stay
/// visible (illustrative spacing, not a calibrated time axis).
pub fn curve_points(area: Rect, v: &Adsr) -> [Pos2; 5] {
    let w = [v.attack_s.max(0.0).sqrt() + 0.04, v.decay_s.max(0.0).sqrt() + 0.04, HOLD, v.release_s.max(0.0).sqrt() + 0.04];
    let total: f32 = w.iter().sum::<f32>().max(1e-6);
    let x = |k: usize| area.min.x + area.width() * w[..k].iter().sum::<f32>() / total;
    let y = |level: f32| area.max.y - area.height() * level.clamp(0.0, 1.0);
    [pos2(x(0), y(0.0)), pos2(x(1), y(1.0)), pos2(x(2), y(v.sustain)), pos2(x(3), y(v.sustain)), pos2(x(4), y(0.0))]
}

/// Where a point is drawn.
pub fn point_pos(area: Rect, v: &Adsr, point: AdsrPoint) -> Pos2 {
    let p = curve_points(area, v);
    match point {
        AdsrPoint::Attack => p[1],
        AdsrPoint::Decay => p[2],
        AdsrPoint::Sustain => p[3],
        AdsrPoint::Release => p[4],
    }
}

/// The point within reach of `at`, nearest first.
fn point_at(area: Rect, v: &Adsr, at: Pos2) -> Option<AdsrPoint> {
    AdsrPoint::ALL.into_iter().map(|p| (p, point_pos(area, v, p).distance(at))).filter(|(_, d)| *d <= GRAB).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(p, _)| p)
}

/// Draw the editor (`height` points tall, full width) and report the drag of a point, if any.
pub fn editor(ui: &mut egui::Ui, id: egui::Id, v: &Adsr, height: f32) -> Option<AdsrDrag> {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width().max(240.0), height), Sense::hover());
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    let area = curve_rect(rect);
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, theme::BACKGROUND);
    // grid
    let grid = Stroke::new(1.0, theme::BORDER.linear_multiply(0.45));
    for i in 0..=12 {
        let x = area.min.x + area.width() * i as f32 / 12.0;
        painter.line_segment([pos2(x, area.min.y), pos2(x, area.max.y)], grid);
    }
    for i in 0..=4 {
        let y = area.min.y + area.height() * i as f32 / 4.0;
        painter.line_segment([pos2(area.min.x, y), pos2(area.max.x, y)], grid);
    }
    // the area under the curve, one trapezoid per segment (the outline is not convex)
    let pts = curve_points(area, v);
    let fill = theme::KEY_HELD.linear_multiply(0.16);
    for w in pts.windows(2) {
        if w[1].x - w[0].x > 0.1 {
            painter.add(egui::Shape::convex_polygon(vec![w[0], w[1], pos2(w[1].x, area.max.y), pos2(w[0].x, area.max.y)], fill, Stroke::NONE));
        }
    }
    painter.add(egui::Shape::line(pts.to_vec(), Stroke::new(2.0, theme::KEY_HELD)));
    // which point is under the pointer or being dragged
    let drag_key = id.with("point");
    let dragging: Option<AdsrPoint> = ui.data(|d| d.get_temp::<Option<AdsrPoint>>(drag_key)).flatten();
    let hovered = resp.hover_pos().and_then(|p| point_at(area, v, p));
    for p in AdsrPoint::ALL {
        let at = point_pos(area, v, p);
        let hot = dragging == Some(p) || (dragging.is_none() && hovered == Some(p));
        painter.circle(at, if hot { 7.0 } else { 5.5 }, theme::KEY_HELD, Stroke::new(1.5, theme::BACKGROUND));
    }
    if hovered.is_some() || dragging.is_some() {
        ui.ctx().set_cursor_icon(if dragging.is_some() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab });
    }
    // segment names under the curve
    let label_y = area.max.y + 4.0;
    for (name, (a, b)) in ["Attack", "Decay", "Sustain", "Release"].into_iter().zip([(0, 1), (1, 2), (2, 3), (3, 4)]) {
        let x = (pts[a].x + pts[b].x) * 0.5;
        painter.text(pos2(x, label_y), egui::Align2::CENTER_TOP, name, egui::FontId::proportional(11.0), theme::TEXT_SECONDARY);
    }
    // the drag
    if resp.drag_started() {
        let origin = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos());
        let picked = origin.and_then(|p| point_at(area, v, p));
        ui.data_mut(|d| d.insert_temp(drag_key, picked));
        return picked.map(|point| AdsrDrag { point, dx: 0.0, dy: 0.0, started: true, stopped: false });
    }
    let point = dragging?;
    let delta = resp.drag_delta();
    let (dx, dy) = (delta.x / area.width().max(1.0), -delta.y / area.height().max(1.0));
    if resp.drag_stopped() || !resp.dragged() {
        ui.data_mut(|d| d.insert_temp::<Option<AdsrPoint>>(drag_key, None));
        return Some(AdsrDrag { point, dx, dy, started: false, stopped: true });
    }
    Some(AdsrDrag { point, dx, dy, started: false, stopped: false })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_curve_follows_the_values_and_keeps_tiny_segments_visible() {
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 100.0));
        let v = Adsr { attack_s: 0.001, decay_s: 0.2, sustain: 0.65, release_s: 0.4 };
        let p = curve_points(area, &v);
        assert_eq!(p[0], pos2(0.0, 100.0));
        assert_eq!(p[1].y, 0.0, "the attack reaches full level");
        assert!((p[2].y - 35.0).abs() < 1e-3 && (p[3].y - 35.0).abs() < 1e-3, "sustain at 65 %");
        assert_eq!(p[4], pos2(400.0, 100.0));
        assert!(p[1].x - p[0].x > 8.0, "a 1 ms attack is still wide enough to grab");
        assert!(p.windows(2).all(|w| w[1].x >= w[0].x));
        // a longer release takes a larger share
        let longer = curve_points(area, &Adsr { release_s: 4.0, ..v });
        assert!(longer[4].x - longer[3].x > p[4].x - p[3].x);
        assert_eq!(point_at(area, &v, p[2] + vec2(3.0, -2.0)), Some(AdsrPoint::Decay));
        assert_eq!(point_at(area, &v, pos2(200.0, 90.0)), None);
    }
}

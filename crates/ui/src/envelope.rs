//! The ADSR envelope editor: the curve over a grid with its area filled, four points to drag
//! (attack peak, decay end, sustain end, release end) and the segment names underneath. The
//! caller owns the values; the widget reports the envelope the dragged point asks for, so the
//! caller can turn it into parameter gestures (live values, one undo step per drag).
//!
//! Each time segment has its own share of the width on a fixed time scale (about linear below
//! 10 ms, logarithmic above, up to [`MAX_TIME_S`]): a point sits where its time puts it and
//! follows the pointer exactly while it is dragged.

use crate::theme;
use egui::{Pos2, Rect, Sense, Stroke, Vec2, pos2, vec2};

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

/// One frame of a point drag: the envelope with the dragged point where the pointer is, and
/// whether the drag starts or ends in this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AdsrDrag {
    pub point: AdsrPoint,
    pub value: Adsr,
    pub started: bool,
    pub stopped: bool,
}

/// Longest time a segment shows (the synth's envelope times go up to 30 s).
pub const MAX_TIME_S: f32 = 30.0;
/// The time scale's knee: about linear below it, logarithmic above.
const KNEE_S: f32 = 0.01;
/// Share of the curve's width of each time segment at [`MAX_TIME_S`].
const TIME_SHARE: f32 = 0.29;
/// Share of the sustain segment.
const SUSTAIN_SHARE: f32 = 1.0 - 3.0 * TIME_SHARE;
/// Space under the curve for the segment names.
const LABEL_SPACE: f32 = 20.0;
/// How close to a point a press must be to pick it up.
const GRAB: f32 = 16.0;

/// Position along a time segment (0..1) for a time in seconds.
pub fn time_to_frac(t: f32) -> f32 {
    let t = if t.is_finite() { t.clamp(0.0, MAX_TIME_S) } else { 0.0 };
    ((1.0 + t / KNEE_S).ln() / (1.0 + MAX_TIME_S / KNEE_S).ln()).clamp(0.0, 1.0)
}

/// The time at a position along a time segment.
pub fn frac_to_time(f: f32) -> f32 {
    let f = if f.is_finite() { f.clamp(0.0, 1.0) } else { 0.0 };
    if f >= 1.0 {
        return MAX_TIME_S;
    }
    (KNEE_S * ((f * (1.0 + MAX_TIME_S / KNEE_S).ln()).exp() - 1.0)).clamp(0.0, MAX_TIME_S)
}

/// The area the curve is drawn in, inside the widget's rect.
pub fn curve_rect(rect: Rect) -> Rect {
    Rect::from_min_max(rect.min + vec2(14.0, 12.0), rect.max - vec2(14.0, LABEL_SPACE + 6.0))
}

fn level_y(area: Rect, level: f32) -> f32 {
    let l = if level.is_finite() { level.clamp(0.0, 1.0) } else { 0.0 };
    area.max.y - area.height() * l
}

/// The curve's corners: start, attack peak, decay end, sustain end, release end.
pub fn curve_points(area: Rect, v: &Adsr) -> [Pos2; 5] {
    let seg = area.width() * TIME_SHARE;
    let x1 = area.min.x + seg * time_to_frac(v.attack_s);
    let x2 = x1 + seg * time_to_frac(v.decay_s);
    let x3 = x2 + area.width() * SUSTAIN_SHARE;
    let x4 = x3 + seg * time_to_frac(v.release_s);
    let s = level_y(area, v.sustain);
    [pos2(area.min.x, area.max.y), pos2(x1, area.min.y), pos2(x2, s), pos2(x3, s), pos2(x4, area.max.y)]
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

/// The envelope with `point` moved to `target` (as far as its segment allows).
pub fn dragged(area: Rect, v: &Adsr, point: AdsrPoint, target: Pos2) -> Adsr {
    let seg = (area.width() * TIME_SHARE).max(1.0);
    let p = curve_points(area, v);
    let level = ((area.max.y - target.y) / area.height().max(1.0)).clamp(0.0, 1.0);
    match point {
        AdsrPoint::Attack => Adsr { attack_s: frac_to_time((target.x - p[0].x) / seg), ..*v },
        AdsrPoint::Decay => Adsr { decay_s: frac_to_time((target.x - p[1].x) / seg), sustain: level, ..*v },
        AdsrPoint::Sustain => Adsr { sustain: level, ..*v },
        AdsrPoint::Release => Adsr { release_s: frac_to_time((target.x - p[3].x) / seg), ..*v },
    }
}

/// The point within reach of `at`, nearest first.
fn point_at(area: Rect, v: &Adsr, at: Pos2) -> Option<AdsrPoint> {
    AdsrPoint::ALL.into_iter().map(|p| (p, point_pos(area, v, p).distance(at))).filter(|(_, d)| *d <= GRAB).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(p, _)| p)
}

/// Draw the editor (`height` points tall, full width) and report the drag of a point, if any.
pub fn editor(ui: &mut egui::Ui, id: egui::Id, v: &Adsr, height: f32) -> Option<AdsrDrag> {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width().max(240.0), height), Sense::hover());
    // drag only: the drag starts at the press, so no movement is lost to a click threshold
    let resp = ui.interact(rect, id, Sense::drag());
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
    // which point is under the pointer or being dragged (with the grab offset)
    let drag_key = id.with("point");
    let dragging: Option<(AdsrPoint, Vec2)> = ui.data(|d| d.get_temp::<Option<(AdsrPoint, Vec2)>>(drag_key)).flatten();
    let hovered = resp.hover_pos().and_then(|p| point_at(area, v, p));
    for p in AdsrPoint::ALL {
        let at = point_pos(area, v, p);
        let hot = dragging.is_some_and(|d| d.0 == p) || (dragging.is_none() && hovered == Some(p));
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
    // the drag: the point keeps its offset from the pointer
    if resp.drag_started() {
        let origin = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos());
        let picked = origin.and_then(|o| point_at(area, v, o).map(|p| (p, point_pos(area, v, p) - o)));
        ui.data_mut(|d| d.insert_temp(drag_key, picked));
        let (point, offset) = picked?;
        let target = resp.interact_pointer_pos().map_or(point_pos(area, v, point), |p| p + offset);
        return Some(AdsrDrag { point, value: dragged(area, v, point, target), started: true, stopped: false });
    }
    let (point, offset) = dragging?;
    let target = resp.interact_pointer_pos().map(|p| p + offset);
    let value = target.map_or(*v, |t| dragged(area, v, point, t));
    if resp.drag_stopped() || !resp.dragged() {
        ui.data_mut(|d| d.insert_temp::<Option<(AdsrPoint, Vec2)>>(drag_key, None));
        return Some(AdsrDrag { point, value, started: false, stopped: true });
    }
    Some(AdsrDrag { point, value, started: false, stopped: false })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::from_min_size(pos2(10.0, 20.0), vec2(800.0, 100.0))
    }

    #[test]
    fn the_curve_follows_the_values_on_a_fixed_time_scale() {
        let v = Adsr { attack_s: 0.001, decay_s: 0.2, sustain: 0.65, release_s: 0.4 };
        let p = curve_points(area(), &v);
        assert_eq!(p[0], pos2(10.0, 120.0));
        assert_eq!(p[1].y, 20.0, "the attack reaches full level");
        assert!((p[2].y - 55.0).abs() < 1e-3 && (p[3].y - 55.0).abs() < 1e-3, "sustain at 65 %");
        assert!(p.windows(2).all(|w| w[1].x >= w[0].x));
        assert!(p[4].x < area().max.x - 40.0, "a 0.4 s release ends well before the edge");
        // a longer release ends further right; the other points stay
        let longer = curve_points(area(), &Adsr { release_s: 4.0, ..v });
        assert!(longer[4].x > p[4].x + 50.0);
        assert_eq!(&longer[..4], &p[..4]);
        // the longest times fill the width exactly
        let max = curve_points(area(), &Adsr { attack_s: MAX_TIME_S, decay_s: MAX_TIME_S, sustain: 0.5, release_s: MAX_TIME_S });
        assert!((max[4].x - area().max.x).abs() < 0.01);
        // the scale round-trips and refuses nonsense
        for t in [0.0, 0.001, 0.02, 0.5, 3.0, 30.0] {
            assert!((frac_to_time(time_to_frac(t)) - t).abs() <= t * 1e-3 + 1e-6, "{t}");
        }
        assert_eq!(time_to_frac(f32::NAN), 0.0);
        assert_eq!(time_to_frac(1e9), 1.0);
        assert_eq!(point_at(area(), &v, p[2] + vec2(3.0, -2.0)), Some(AdsrPoint::Decay));
        assert_eq!(point_at(area(), &v, pos2(400.0, 115.0)), None);
    }

    #[test]
    fn a_dragged_point_lands_where_the_pointer_is() {
        let v = Adsr { attack_s: 0.005, decay_s: 0.12, sustain: 0.75, release_s: 0.18 };
        for point in AdsrPoint::ALL {
            let from = point_pos(area(), &v, point);
            for delta in [vec2(60.0, 0.0), vec2(-8.0, 0.0), vec2(30.0, 20.0), vec2(5.0, -10.0)] {
                let target = from + delta;
                let moved = dragged(area(), &v, point, target);
                let at = point_pos(area(), &moved, point);
                match point {
                    // the sustain end only moves up and down
                    AdsrPoint::Sustain => assert!((at.y - target.y).abs() < 0.01 && at.x == from.x, "{point:?} {delta:?}: {at:?}"),
                    // the attack peak stays at full level, the release end at zero
                    AdsrPoint::Attack | AdsrPoint::Release => assert!((at.x - target.x).abs() < 0.05 && at.y == from.y, "{point:?} {delta:?}: {at:?} vs {target:?}"),
                    AdsrPoint::Decay => assert!(at.distance(target) < 0.05, "{point:?} {delta:?}: {at:?} vs {target:?}"),
                }
            }
        }
        // past the start of its segment a time is zero, past its end the longest
        let a = dragged(area(), &v, AdsrPoint::Attack, pos2(0.0, 0.0));
        assert_eq!(a.attack_s, 0.0);
        let r = dragged(area(), &v, AdsrPoint::Release, pos2(5000.0, 0.0));
        assert_eq!(r.release_s, MAX_TIME_S);
        assert_eq!(dragged(area(), &v, AdsrPoint::Sustain, pos2(0.0, -50.0)).sustain, 1.0);
    }
}

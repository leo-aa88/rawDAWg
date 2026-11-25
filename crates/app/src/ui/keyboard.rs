//! On-screen piano: click/drag to audition on the selected channel (IO-02).

use super::App;
use crate::theme;
use rawdawg_types::events::{EventSource, TimedEvent};

#[derive(Default)]
pub(crate) struct KeyboardState {
    /// Note currently held by the mouse: (note id, pitch).
    held: Option<(u32, u8)>,
    pub low_pitch: u8,
    pub octaves: u8,
}

fn is_black(p: u8) -> bool {
    matches!(p % 12, 1 | 3 | 6 | 8 | 10)
}

pub(crate) fn show(app: &mut App, ui: &mut egui::Ui) {
    if app.keyboard.octaves == 0 {
        app.keyboard.low_pitch = 36;
        app.keyboard.octaves = 4;
    }
    let low = app.keyboard.low_pitch;
    let count = u16::from(app.keyboard.octaves) * 12;
    let whites: Vec<u8> = (0..count).map(|i| (u16::from(low) + i).min(127) as u8).filter(|p| !is_black(*p)).collect();
    let height = 72.0;
    let width = ui.available_width().max(200.0);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let white_w = rect.width() / whites.len() as f32;
    let pitch_at = |pos: egui::Pos2| -> Option<u8> {
        if !rect.contains(pos) {
            return None;
        }
        let x = pos.x - rect.min.x;
        // black keys occupy the top 60% between whites
        if pos.y < rect.min.y + height * 0.6 {
            for (i, w) in whites.iter().enumerate() {
                let next = w + 1;
                if next <= 127 && is_black(next) && whites.get(i + 1).is_some() {
                    let bx = rect.min.x + (i as f32 + 1.0) * white_w;
                    if (x + rect.min.x - bx).abs() < white_w * 0.3 {
                        return Some(next);
                    }
                }
            }
        }
        let idx = ((x / white_w) as usize).min(whites.len() - 1);
        Some(whites[idx])
    };
    let held_pitch = app.keyboard.held.map(|h| h.1);
    for (i, w) in whites.iter().enumerate() {
        let x = rect.min.x + i as f32 * white_w;
        let r = egui::Rect::from_min_max(egui::pos2(x, rect.min.y), egui::pos2(x + white_w - 1.0, rect.max.y));
        let fill = if held_pitch == Some(*w) { theme::ACCENT } else { theme::TEXT };
        painter.rect_filled(r, 2.0, fill);
        if w % 12 == 0 {
            painter.text(
                egui::pos2(x + 3.0, rect.max.y - 3.0),
                egui::Align2::LEFT_BOTTOM,
                rawdawg_types::units::Pitch(*w).name(),
                egui::FontId::proportional(10.0),
                theme::BACKGROUND,
            );
        }
    }
    for (i, w) in whites.iter().enumerate() {
        let next = w + 1;
        if next <= 127 && is_black(next) && whites.get(i + 1).is_some() {
            let bx = rect.min.x + (i as f32 + 1.0) * white_w;
            let r = egui::Rect::from_center_size(egui::pos2(bx, rect.min.y + height * 0.3), egui::vec2(white_w * 0.6, height * 0.6));
            let fill = if held_pitch == Some(next) { theme::ACCENT } else { theme::BACKGROUND };
            painter.rect_filled(r, 2.0, fill);
        }
    }
    // interaction: press starts a note, dragging across keys glides by re-triggering, release stops
    let pointer = response.interact_pointer_pos();
    let wanted = if response.dragged() || response.is_pointer_button_down_on() { pointer.and_then(pitch_at) } else { None };
    let vel = app.prefs.typing_velocity;
    match (app.keyboard.held, wanted) {
        (None, Some(p)) => {
            let id = app.alloc_note_id();
            app.keyboard.held = Some((id, p));
            app.live(TimedEvent::note_on(0, EventSource::Mouse, 0, id, p, vel));
        }
        (Some((id, old)), Some(p)) if p != old => {
            app.live(TimedEvent::note_off(0, EventSource::Mouse, 0, id, old));
            let nid = app.alloc_note_id();
            app.keyboard.held = Some((nid, p));
            app.live(TimedEvent::note_on(0, EventSource::Mouse, 0, nid, p, vel));
        }
        (Some((id, old)), None) => {
            app.keyboard.held = None;
            app.live(TimedEvent::note_off(0, EventSource::Mouse, 0, id, old));
        }
        _ => {}
    }
    ui.horizontal(|ui| {
        if ui.small_button("<").clicked() {
            app.keyboard.low_pitch = app.keyboard.low_pitch.saturating_sub(12);
        }
        ui.label(
            egui::RichText::new(format!("{} … {}", rawdawg_types::units::Pitch(low).name(), rawdawg_types::units::Pitch((u16::from(low) + count - 1).min(127) as u8).name()))
                .color(theme::TEXT_SECONDARY),
        );
        if ui.small_button(">").clicked() {
            app.keyboard.low_pitch = (app.keyboard.low_pitch + 12).min(127 - 24);
        }
        ui.label(egui::RichText::new("Click or drag to play").color(theme::TEXT_SECONDARY));
    });
}

//! Parameter widgets shared by the app's generic effect/instrument panels and the plugin
//! editors (PLUGIN_API.md: "reuse actual synth/effect UI widgets in plugin editor").

use egui::Response;
use rawdawg_types::{ParamDescriptor, ParamMapping};

/// Most steps offered as a list of named choices (more get a step slider).
pub const MAX_LISTED_STEPS: u32 = 64;

/// Names of a discrete parameter's steps: the descriptor's own labels, otherwise `text` for
/// every step (hosted plugins name their choices). `None` when neither is available or the
/// steps are too many to list.
pub fn step_labels(d: &ParamDescriptor, text: impl Fn(f32) -> Option<String>) -> Option<Vec<String>> {
    let ParamMapping::Discrete { steps } = d.mapping else { return None };
    if let Some(labels) = d.labels.filter(|l| l.len() >= steps as usize) {
        return Some(labels.iter().take(steps as usize).map(|l| (*l).to_owned()).collect());
    }
    if steps > MAX_LISTED_STEPS {
        return None;
    }
    (0..steps).map(|i| text(d.to_normalized(f64::from(i)))).collect()
}

/// A choice list for a discrete parameter; returns the response and the (new) value.
pub fn step_combo(ui: &mut egui::Ui, salt: impl std::hash::Hash + std::fmt::Debug, d: &ParamDescriptor, labels: &[String], n: f32) -> (Response, f32) {
    let before = (d.to_physical(n).max(0.0) as usize).min(labels.len().saturating_sub(1));
    let mut idx = before;
    let mut r = egui::ComboBox::from_id_salt(salt)
        .selected_text(labels.get(idx).map(String::as_str).unwrap_or(""))
        .show_ui(ui, |ui| {
            for (i, l) in labels.iter().enumerate() {
                ui.selectable_value(&mut idx, i, l);
            }
        })
        .response;
    if idx != before {
        r.mark_changed();
        return (r, d.to_normalized(idx as f64));
    }
    (r, n)
}

/// The control for one parameter: a switch, a list of named choices, a step slider or a
/// continuous slider. Returns the response and the (possibly changed) normalized value.
pub fn param_control(ui: &mut egui::Ui, salt: impl std::hash::Hash + std::fmt::Debug, d: &ParamDescriptor, n: f32, labels: Option<&[String]>) -> (Response, f32) {
    match d.mapping {
        ParamMapping::Toggle => {
            let mut b = n >= 0.5;
            let r = ui.checkbox(&mut b, "");
            (r, if b { 1.0 } else { 0.0 })
        }
        ParamMapping::Discrete { .. } if labels.is_some() => step_combo(ui, salt, d, labels.unwrap_or_default(), n),
        ParamMapping::Discrete { steps } => {
            let mut idx = d.to_physical(n) as i64;
            let r = ui.add(slider(ui, &mut idx, 0..=(i64::from(steps) - 1)).show_value(true));
            (r, d.to_normalized(idx as f64))
        }
        _ => {
            let mut v = n;
            let r = ui.add(slider(ui, &mut v, 0.0..=1.0).show_value(false));
            (r, v)
        }
    }
}

/// Whether a button other than the primary one is pressed, held or released this frame.
pub fn non_primary_pointer(ui: &egui::Ui) -> bool {
    ui.input(|i| {
        [egui::PointerButton::Secondary, egui::PointerButton::Middle].into_iter().any(|b| i.pointer.button_pressed(b) || i.pointer.button_down(b) || i.pointer.button_released(b))
    })
}

/// `egui::Slider::new`, except that only the primary button moves it: a right-click opens the
/// control's menu (automation, MIDI learn) and leaves the value alone. egui's own slider follows
/// any button, so a right-click used to jump the value to the pointer.
pub fn slider<'a, N: egui::emath::Numeric>(ui: &egui::Ui, value: &'a mut N, range: std::ops::RangeInclusive<N>) -> egui::Slider<'a> {
    let frozen = non_primary_pointer(ui);
    let range_f64 = range.start().to_f64()..=range.end().to_f64();
    let s = egui::Slider::from_get_set(range_f64, move |v: Option<f64>| {
        if let Some(v) = v
            && !frozen
        {
            *value = N::from_f64(v);
        }
        value.to_f64()
    });
    if N::INTEGRAL { s.integer() } else { s }
}

/// `Response::context_menu` for parameter controls: also opens when the right button is released
/// over the control. egui's sliders sense only drags, so a right-click on one is a drag, never a
/// secondary click, and egui's own context menu never opened there. (Sensing clicks as well would
/// stop a left click from setting the slider.)
pub fn context_menu<R>(r: &Response, add: impl FnOnce(&mut egui::Ui) -> R) -> Option<egui::InnerResponse<R>> {
    let released_here = r.hovered() && r.ctx.input(|i| i.pointer.button_released(egui::PointerButton::Secondary));
    let open = if r.secondary_clicked() || released_here {
        Some(egui::SetOpenCommand::Bool(true))
    } else if r.clicked() {
        Some(egui::SetOpenCommand::Bool(false))
    } else {
        None
    };
    // its own popup id: egui's default (`id.with("popup")`) is the one a ComboBox keeps its list
    // under, so a menu on a dropdown opened the dropdown's list as well
    egui::Popup::menu(r).id(r.id.with("rawdawg-param-menu")).open_memory(open).at_pointer_fixed().show(add)
}

/// The product wordmark: `raw` `DAW` `g` with DAW in the accent colour (UI_UX.md §1).
pub fn wordmark(ui: &mut egui::Ui, size: f32) {
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("raw").strong().size(size));
            ui.label(egui::RichText::new("DAW").strong().size(size).color(crate::theme::ACCENT));
            ui.label(egui::RichText::new("g").strong().size(size));
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawdawg_types::{ParamId, ParamUnit, params::ParamCategory};

    #[test]
    fn step_names_come_from_labels_then_from_text() {
        static LABELS: [&str; 3] = ["Low", "Mid", "High"];
        let labelled = ParamDescriptor::continuous(1, "m", "Mode", ParamUnit::None, ParamMapping::Discrete { steps: 3 }, 0.0, ParamCategory::Effects).with_labels(&LABELS);
        assert_eq!(step_labels(&labelled, |_| None), Some(vec!["Low".to_owned(), "Mid".to_owned(), "High".to_owned()]));
        let plain = ParamDescriptor::continuous(2, "s", "Steps", ParamUnit::None, ParamMapping::Discrete { steps: 4 }, 0.0, ParamCategory::Effects);
        assert_eq!(step_labels(&plain, |n| Some(format!("{n:.2}"))).unwrap(), vec!["0.00", "0.33", "0.67", "1.00"]);
        assert_eq!(step_labels(&plain, |n| (n < 0.5).then(|| "x".to_owned())), None, "every step must have a name");
        let many = ParamDescriptor::continuous(3, "v", "Voices", ParamUnit::None, ParamMapping::Discrete { steps: MAX_LISTED_STEPS + 1 }, 0.0, ParamCategory::Effects);
        assert_eq!(step_labels(&many, |_| Some(String::new())), None);
        let cont = ParamDescriptor::continuous(4, "g", "Gain", ParamUnit::None, ParamMapping::Linear { min: 0.0, max: 1.0 }, 0.5, ParamCategory::Effects);
        assert_eq!(step_labels(&cont, |_| Some(String::new())), None);
        assert_eq!(cont.id, ParamId(4));
    }
}

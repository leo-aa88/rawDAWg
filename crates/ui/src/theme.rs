//! Dark-only visual tokens (UI_UX.md §1). There is no light theme.

use egui::{Color32, CornerRadius, Stroke, Visuals};

pub const BACKGROUND: Color32 = Color32::from_rgb(0x17, 0x19, 0x1E);
pub const PANEL: Color32 = Color32::from_rgb(0x20, 0x23, 0x2A);
pub const RAISED: Color32 = Color32::from_rgb(0x2B, 0x30, 0x39);
pub const BORDER: Color32 = Color32::from_rgb(0x3D, 0x44, 0x4F);
pub const TEXT: Color32 = Color32::from_rgb(0xE7, 0xEA, 0xF0);
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(0xAA, 0xB2, 0xC0);
pub const ACCENT: Color32 = Color32::from_rgb(0x87, 0xC8, 0xA2);
pub const WARNING: Color32 = Color32::from_rgb(0xE6, 0xB7, 0x6E);
pub const ERROR: Color32 = Color32::from_rgb(0xE6, 0x7D, 0x7D);
/// Keys being played, envelope and harmonic editors: a brighter, cooler accent that stands out
/// on the green note colours.
pub const KEY_HELD: Color32 = Color32::from_rgb(0x5C, 0xC8, 0xF0);

/// Build the permanent dark visuals.
pub fn visuals() -> Visuals {
    let mut v = Visuals::dark();
    v.dark_mode = true;
    v.override_text_color = Some(TEXT);
    v.panel_fill = PANEL;
    v.window_fill = PANEL;
    v.extreme_bg_color = BACKGROUND;
    v.faint_bg_color = RAISED;
    v.code_bg_color = BACKGROUND;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.selection.bg_fill = ACCENT.linear_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;
    v.warn_fg_color = WARNING;
    v.error_fg_color = ERROR;
    v.widgets.noninteractive.bg_fill = PANEL;
    v.widgets.noninteractive.weak_bg_fill = PANEL;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_SECONDARY);
    v.widgets.inactive.bg_fill = RAISED;
    v.widgets.inactive.weak_bg_fill = RAISED;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.hovered.bg_fill = RAISED.linear_multiply(1.25);
    v.widgets.hovered.weak_bg_fill = RAISED.linear_multiply(1.25);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    v.widgets.hovered.fg_stroke = Stroke::new(1.5, TEXT);
    v.widgets.active.bg_fill = ACCENT.linear_multiply(0.5);
    v.widgets.active.weak_bg_fill = ACCENT.linear_multiply(0.5);
    v.widgets.active.fg_stroke = Stroke::new(2.0, TEXT);
    v.widgets.open.bg_fill = RAISED;
    v.widgets.open.weak_bg_fill = RAISED;
    let r = CornerRadius::same(3);
    v.widgets.noninteractive.corner_radius = r;
    v.widgets.inactive.corner_radius = r;
    v.widgets.hovered.corner_radius = r;
    v.widgets.active.corner_radius = r;
    v.widgets.open.corner_radius = r;
    v.window_corner_radius = CornerRadius::same(4);
    v.menu_corner_radius = CornerRadius::same(4);
    v
}

/// egui's fonts with its emoji fonts (stars, play arrows, the lock) and, last in the
/// proportional fallback chain, the monospace font for the arrows and symbols the proportional
/// font lacks (→, ●, ⁻), so no label shows an empty box.
pub fn font_definitions() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    let mono = fonts.families.get(&egui::FontFamily::Monospace).and_then(|f| f.first().cloned());
    if let (Some(mono), Some(prop)) = (mono, fonts.families.get_mut(&egui::FontFamily::Proportional))
        && !prop.contains(&mono)
    {
        prop.push(mono);
    }
    fonts
}

/// Apply dark-only styling to a context (called every frame start to defeat any
/// system theme following). The fonts are installed once per context.
pub fn apply(ctx: &egui::Context) {
    let installed = egui::Id::unique("rawdawg-fonts-installed");
    if !ctx.data(|d| d.get_temp::<bool>(installed).unwrap_or(false)) {
        ctx.set_fonts(font_definitions());
        ctx.data_mut(|d| d.insert_temp(installed, true));
    }
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.set_visuals_of(egui::Theme::Dark, visuals());
    ctx.set_visuals_of(egui::Theme::Light, visuals());
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(6.0, 4.0);
        style.spacing.button_padding = egui::vec2(8.0, 4.0);
        style.spacing.interact_size = egui::vec2(24.0, 24.0);
    });
}

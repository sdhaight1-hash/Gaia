//! Gaia's restrained, high-contrast editor palette. Visual-only configuration.
use eframe::egui::{self, Color32, Frame, Stroke};

pub const BG: Color32 = Color32::from_rgb(14, 18, 23);
pub const PANEL: Color32 = Color32::from_rgb(20, 25, 32);
pub const PANEL_RAISED: Color32 = Color32::from_rgb(28, 35, 44);
pub const CANVAS: Color32 = Color32::from_rgb(15, 20, 26);
pub const BORDER: Color32 = Color32::from_rgb(43, 55, 63);
pub const TEXT: Color32 = Color32::from_rgb(228, 237, 239);
pub const MUTED: Color32 = Color32::from_rgb(136, 153, 160);
pub const ACCENT: Color32 = Color32::from_rgb(101, 218, 192);
pub const BLUE: Color32 = Color32::from_rgb(111, 164, 219);
pub const AMBER: Color32 = Color32::from_rgb(222, 171, 108);
pub const RED: Color32 = Color32::from_rgb(229, 116, 122);

pub fn apply(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = PANEL;
    visuals.window_fill = PANEL_RAISED;
    visuals.window_stroke = Stroke::new(1.0, BORDER);
    visuals.extreme_bg_color = BG;
    visuals.text_edit_bg_color = Some(CANVAS);
    visuals.faint_bg_color = PANEL_RAISED;
    visuals.code_bg_color = CANVAS;
    visuals.weak_text_color = Some(MUTED);
    visuals.hyperlink_color = ACCENT;
    visuals.selection.bg_fill = Color32::from_rgb(42, 95, 89);
    visuals.selection.stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.noninteractive.bg_fill = PANEL;
    visuals.widgets.noninteractive.weak_bg_fill = PANEL;
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.inactive.bg_fill = PANEL_RAISED;
    visuals.widgets.inactive.weak_bg_fill = PANEL_RAISED;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(39, 51, 60);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(39, 51, 60);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.active.bg_fill = Color32::from_rgb(43, 88, 83);
    visuals.widgets.active.weak_bg_fill = Color32::from_rgb(43, 88, 83);
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, TEXT);
    for widget in [
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
    ] {
        widget.corner_radius = 6.into();
    }
    visuals.window_corner_radius = 9.into();
    visuals.menu_corner_radius = 8.into();
    visuals.slider_trailing_fill = true;
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    style.spacing.item_spacing = egui::vec2(9.0, 10.0);
    style.spacing.button_padding = egui::vec2(13.0, 7.0);
    style.spacing.interact_size.y = 29.0;
    style.spacing.slider_width = 185.0;
    ctx.set_style_of(egui::Theme::Dark, style);
}

pub fn panel() -> Frame {
    Frame::new()
        .fill(PANEL)
        .inner_margin(14)
        .stroke(Stroke::new(1.0, BORDER))
}

pub fn toolbar() -> Frame {
    Frame::new()
        .fill(PANEL)
        .inner_margin(12)
        .stroke(Stroke::new(1.0, BORDER))
}

pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.add(egui::Label::new(
        egui::RichText::new(title).size(11.0).color(MUTED).strong()
    ));
    ui.add_space(5.0);
}

//! "Dark Control Booth" theme.

use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Visuals};
use livestage_core::state::{palette, CueState, Rgb};

pub const BG: Color32 = Color32::from_rgb(0x12, 0x12, 0x12);
pub const PANEL_RAISED: Color32 = Color32::from_rgb(0x1E, 0x1E, 0x1E);
pub const PANEL_RECESSED: Color32 = Color32::from_rgb(0x18, 0x18, 0x18);
pub const TEXT: Color32 = Color32::from_rgb(0xF2, 0xF2, 0xF2);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x9E, 0x9E, 0x9E);
pub const BORDER: Color32 = Color32::from_rgb(0x2C, 0x2C, 0x2C);
/// Dark emerald for selected rows / active tabs.
pub const ACCENT_DIM: Color32 = Color32::from_rgb(0x0E, 0x4A, 0x2C);

pub fn rgb(c: Rgb) -> Color32 {
    Color32::from_rgb(c.0, c.1, c.2)
}
pub fn standby() -> Color32 {
    rgb(palette::STANDBY)
}
pub fn active() -> Color32 {
    rgb(palette::ACTIVE)
}
pub fn go() -> Color32 {
    rgb(palette::GO)
}
pub fn stop() -> Color32 {
    rgb(palette::STOP)
}

pub fn state_color(s: CueState) -> Option<Color32> {
    s.color().map(rgb)
}

pub fn apply(ctx: &egui::Context) {
    let mut v = Visuals::dark();
    v.override_text_color = Some(TEXT);
    v.panel_fill = PANEL_RAISED;
    v.window_fill = PANEL_RAISED;
    v.extreme_bg_color = PANEL_RECESSED;
    v.faint_bg_color = Color32::from_rgb(0x22, 0x22, 0x22);
    v.code_bg_color = PANEL_RECESSED;
    v.window_stroke = Stroke::new(1.0_f32, BORDER);
    // Emerald accent for selection / active states.
    v.selection.bg_fill = ACCENT_DIM;
    v.selection.stroke = Stroke::new(1.0_f32, go());
    v.hyperlink_color = go();
    let r = CornerRadius::same(3);
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = r;
    }
    v.widgets.noninteractive.bg_fill = PANEL_RAISED;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT_DIM);
    v.widgets.inactive.bg_fill = Color32::from_rgb(0x2A, 0x2A, 0x2A);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(0x2A, 0x2A, 0x2A);
    v.widgets.hovered.bg_fill = Color32::from_rgb(0x36, 0x36, 0x36);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x36, 0x36, 0x36);
    v.widgets.active.bg_fill = Color32::from_rgb(0x44, 0x44, 0x44);
    ctx.set_visuals(v);

    ctx.style_mut(|s| {
        s.text_styles.insert(TextStyle::Body, FontId::new(14.0, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Button, FontId::new(14.0, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Heading, FontId::new(18.0, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Monospace, FontId::new(14.0, FontFamily::Monospace));
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 5.0);
        s.spacing.interact_size.y = 24.0;
    });
}

/// Large tabular figures for clocks and timers.
pub fn clock_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

/// Dock styling: recessed tab bar, raised bodies, emerald active-tab marker.
pub fn dock_style(base: &egui::Style) -> egui_dock::Style {
    let mut s = egui_dock::Style::from_egui(base);
    s.tab_bar.bg_fill = PANEL_RECESSED;
    s.tab_bar.height = 30.0;
    s.tab_bar.hline_color = BORDER;
    s.tab.hline_below_active_tab_name = true;
    s.tab.minimum_width = Some(90.0);
    s.tab.tab_body.bg_fill = PANEL_RAISED;
    s.tab.tab_body.inner_margin = egui::Margin::same(10);
    s.tab.tab_body.stroke = Stroke::new(1.0_f32, BORDER);
    for t in [&mut s.tab.active, &mut s.tab.focused, &mut s.tab.active_with_kb_focus, &mut s.tab.focused_with_kb_focus] {
        t.bg_fill = PANEL_RAISED;
        t.text_color = TEXT;
        t.outline_color = go();
    }
    for t in [&mut s.tab.inactive, &mut s.tab.inactive_with_kb_focus, &mut s.tab.hovered] {
        t.bg_fill = PANEL_RECESSED;
        t.text_color = TEXT_DIM;
        t.outline_color = BORDER;
    }
    s.tab.hovered.text_color = TEXT;
    s.separator.color_idle = BORDER;
    s.separator.color_hovered = go();
    s.separator.color_dragged = go();
    s.separator.width = 2.0;
    s.main_surface_border_stroke = Stroke::NONE;
    s
}

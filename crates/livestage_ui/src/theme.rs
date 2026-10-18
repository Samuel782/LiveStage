//! "Dark Control Booth" theme.

use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Visuals};
use livestage_core::state::{palette, CueState, Rgb};

pub const BG: Color32 = Color32::from_rgb(0x12, 0x12, 0x12);
pub const PANEL_RAISED: Color32 = Color32::from_rgb(0x1E, 0x1E, 0x1E);
pub const PANEL_RECESSED: Color32 = Color32::from_rgb(0x18, 0x18, 0x18);
pub const TEXT: Color32 = Color32::from_rgb(0xF2, 0xF2, 0xF2);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x9E, 0x9E, 0x9E);
pub const BORDER: Color32 = Color32::from_rgb(0x2C, 0x2C, 0x2C);

/// Operator-selectable UI accent (selection, focus, menu highlight). Cue
/// state colours (standby / active / GO / stop) never change: they carry meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Accent {
    #[default]
    Emerald,
    Blue,
    Violet,
    Amber,
    Rose,
}

impl Accent {
    pub const ALL: [Accent; 5] = [Accent::Emerald, Accent::Blue, Accent::Violet, Accent::Amber, Accent::Rose];
    pub fn label(self) -> &'static str {
        match self {
            Accent::Emerald => "Emerald",
            Accent::Blue => "Blue",
            Accent::Violet => "Violet",
            Accent::Amber => "Amber",
            Accent::Rose => "Rose",
        }
    }
    /// (bright, menu highlight, dim selection)
    fn colors(self) -> (Color32, Color32, Color32) {
        match self {
            Accent::Emerald => (Color32::from_rgb(0x00, 0xC8, 0x53), Color32::from_rgb(0x0B, 0x7A, 0x45), Color32::from_rgb(0x0E, 0x4A, 0x2C)),
            Accent::Blue => (Color32::from_rgb(0x2F, 0x8C, 0xFF), Color32::from_rgb(0x1E, 0x5F, 0xC7), Color32::from_rgb(0x15, 0x36, 0x66)),
            Accent::Violet => (Color32::from_rgb(0xA2, 0x6B, 0xFF), Color32::from_rgb(0x6A, 0x3F, 0xC4), Color32::from_rgb(0x3A, 0x26, 0x66)),
            Accent::Amber => (Color32::from_rgb(0xFF, 0xB3, 0x00), Color32::from_rgb(0xA8, 0x6E, 0x00), Color32::from_rgb(0x5A, 0x40, 0x08)),
            Accent::Rose => (Color32::from_rgb(0xFF, 0x5C, 0x8A), Color32::from_rgb(0xB8, 0x2E, 0x5A), Color32::from_rgb(0x5E, 0x1C, 0x33)),
        }
    }
}

static ACCENT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn set_accent(a: Accent) {
    let i = Accent::ALL.iter().position(|x| *x == a).unwrap_or(0) as u8;
    ACCENT.store(i, std::sync::atomic::Ordering::Relaxed);
}
pub fn current_accent() -> Accent {
    Accent::ALL[ACCENT.load(std::sync::atomic::Ordering::Relaxed) as usize % Accent::ALL.len()]
}
/// Bright accent (focus outlines, active tab marker, links).
pub fn accent() -> Color32 {
    current_accent().colors().0
}
/// Menu row highlight.
pub fn accent_menu() -> Color32 {
    current_accent().colors().1
}
/// Selected rows / text selection.
pub fn accent_dim() -> Color32 {
    current_accent().colors().2
}
/// Popup menus: slightly lighter than panels, hairline border.
pub const MENU_FILL: Color32 = Color32::from_rgb(0x26, 0x26, 0x28);
pub const MENU_STROKE: Color32 = Color32::from_rgb(0x3C, 0x3C, 0x40);
pub const MENU_SEPARATOR: Color32 = Color32::from_rgb(0x3A, 0x3A, 0x3E);

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

/// Adds the bundled monospace face (Hack) as a last fallback for
/// proportional text: it carries arrows and keyboard symbols (→ ← ⇧ ↵ ▸)
/// that the default UI face lacks. Installed once per context.
fn install_fonts(ctx: &egui::Context) {
    let flag = egui::Id::new("livestage-fonts");
    if ctx.data(|d| d.get_temp::<bool>(flag)).unwrap_or(false) {
        return;
    }
    let mut fonts = egui::FontDefinitions::default();
    if let Some(prop) = fonts.families.get_mut(&FontFamily::Proportional) {
        if !prop.iter().any(|f| f == "Hack") {
            prop.push("Hack".to_owned());
        }
    }
    ctx.set_fonts(fonts);
    ctx.data_mut(|d| d.insert_temp(flag, true));
}

pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    let mut v = Visuals::dark();
    v.override_text_color = Some(TEXT);
    v.panel_fill = PANEL_RAISED;
    v.window_fill = MENU_FILL;
    v.extreme_bg_color = PANEL_RECESSED;
    v.faint_bg_color = Color32::from_rgb(0x22, 0x22, 0x22);
    v.code_bg_color = PANEL_RECESSED;
    v.window_stroke = Stroke::new(1.0_f32, MENU_STROKE);
    // Accent for selection / active states.
    v.selection.bg_fill = accent_dim();
    v.selection.stroke = Stroke::new(1.0_f32, accent());
    v.hyperlink_color = accent();
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
    // macOS-like popups: rounded, hairline border, soft shadow.
    v.menu_corner_radius = CornerRadius::same(9);
    v.popup_shadow = egui::Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(140) };
    ctx.set_visuals(v);

    ctx.style_mut(|s| {
        s.text_styles.insert(TextStyle::Body, FontId::new(14.0, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Button, FontId::new(14.0, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Heading, FontId::new(18.0, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Monospace, FontId::new(14.0, FontFamily::Monospace));
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 5.0);
        s.spacing.interact_size.y = 24.0;
        s.spacing.menu_margin = egui::Margin::same(5);
        s.spacing.menu_spacing = 2.0;
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
        t.outline_color = accent();
    }
    for t in [&mut s.tab.inactive, &mut s.tab.inactive_with_kb_focus, &mut s.tab.hovered] {
        t.bg_fill = PANEL_RECESSED;
        t.text_color = TEXT_DIM;
        t.outline_color = BORDER;
    }
    s.tab.hovered.text_color = TEXT;
    s.separator.color_idle = BORDER;
    s.separator.color_hovered = accent();
    s.separator.color_dragged = accent();
    s.separator.width = 2.0;
    s.main_surface_border_stroke = Stroke::NONE;
    s
}

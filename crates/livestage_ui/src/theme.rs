//! "Obsidian Telemetry" theme: carbon / obsidian surfaces, hairline
//! structure, tabular readouts and three meaningful signal colours
//! (Cyberpunk Amber = standby, Laser Emerald = active, Deep Crimson =
//! panic / errors). Everything else stays neutral so those three read
//! instantly from across a dark booth.

use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Visuals};
use livestage_core::state::{palette, CueState, Rgb};

// ---------------------------------------------------------------- surfaces --

/// Deepest well: command deck, tab bar, text fields, meters.
pub const BG_DEEP: Color32 = Color32::from_rgb(0x0A, 0x0A, 0x0C);
/// Application background behind panels.
pub const BG: Color32 = Color32::from_rgb(0x0D, 0x0D, 0x10);
/// Recessed surfaces inside panels (grid header, tracks, wells).
pub const PANEL_RECESSED: Color32 = Color32::from_rgb(0x0F, 0x0F, 0x12);
/// Panel bodies.
pub const PANEL_RAISED: Color32 = Color32::from_rgb(0x14, 0x14, 0x18);
/// Alternate grid rows and raised controls.
pub const ROW_ALT: Color32 = Color32::from_rgb(0x18, 0x18, 0x1D);
/// Control faces (keys, buttons).
pub const CONTROL: Color32 = Color32::from_rgb(0x1C, 0x1C, 0x23);
pub const CONTROL_HOVER: Color32 = Color32::from_rgb(0x25, 0x25, 0x2E);
pub const CONTROL_ACTIVE: Color32 = Color32::from_rgb(0x2D, 0x2D, 0x38);

// ------------------------------------------------------------------- lines --

/// Hairline structure.
pub const BORDER: Color32 = Color32::from_rgb(0x26, 0x26, 0x2F);
/// Brighter hairline: module brackets, focused edges.
pub const BORDER_HI: Color32 = Color32::from_rgb(0x3A, 0x3A, 0x47);

// -------------------------------------------------------------------- text --

pub const TEXT: Color32 = Color32::from_rgb(0xEC, 0xEE, 0xF4);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8C, 0x8F, 0x9E);
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x58, 0x5B, 0x69);

/// Operator-selectable UI accent (selection, focus, menu highlight). Cue
/// state colours (standby / active / GO / stop) never change: they carry meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Accent {
    /// Telemetry cyan: distinct from every state colour.
    #[default]
    Ice,
    Emerald,
    Blue,
    Violet,
    Amber,
    Rose,
}

impl Accent {
    pub const ALL: [Accent; 6] = [Accent::Ice, Accent::Emerald, Accent::Blue, Accent::Violet, Accent::Amber, Accent::Rose];
    pub fn label(self) -> &'static str {
        match self {
            Accent::Ice => "Ice",
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
            Accent::Ice => (Color32::from_rgb(0x38, 0xC8, 0xFF), Color32::from_rgb(0x15, 0x6E, 0x96), Color32::from_rgb(0x10, 0x31, 0x44)),
            Accent::Emerald => (Color32::from_rgb(0x00, 0xC8, 0x80), Color32::from_rgb(0x0B, 0x6A, 0x48), Color32::from_rgb(0x0B, 0x36, 0x28)),
            Accent::Blue => (Color32::from_rgb(0x4C, 0x8D, 0xFF), Color32::from_rgb(0x1E, 0x4F, 0xB0), Color32::from_rgb(0x15, 0x29, 0x55)),
            Accent::Violet => (Color32::from_rgb(0xA2, 0x7B, 0xFF), Color32::from_rgb(0x5E, 0x3F, 0xB4), Color32::from_rgb(0x2F, 0x22, 0x55)),
            Accent::Amber => (Color32::from_rgb(0xFF, 0xB0, 0x20), Color32::from_rgb(0x9A, 0x66, 0x00), Color32::from_rgb(0x48, 0x33, 0x08)),
            Accent::Rose => (Color32::from_rgb(0xFF, 0x5C, 0x8A), Color32::from_rgb(0xA8, 0x2E, 0x55), Color32::from_rgb(0x4E, 0x1A, 0x2D)),
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
/// Popup menus: slightly lifted from panels, hairline border.
pub const MENU_FILL: Color32 = Color32::from_rgb(0x17, 0x17, 0x1D);
pub const MENU_SEPARATOR: Color32 = BORDER;

pub fn rgb(c: Rgb) -> Color32 {
    Color32::from_rgb(c.0, c.1, c.2)
}
/// Cyberpunk Amber.
pub fn standby() -> Color32 {
    rgb(palette::STANDBY)
}
/// Laser Emerald.
pub fn active() -> Color32 {
    rgb(palette::ACTIVE)
}
pub fn go() -> Color32 {
    rgb(palette::GO)
}
/// Deep Crimson.
pub fn stop() -> Color32 {
    rgb(palette::STOP)
}
/// Crimson lifted for text on dark surfaces (the fill colour is too deep to read small).
pub fn stop_text() -> Color32 {
    Color32::from_rgb(0xFF, 0x4D, 0x6A)
}

/// Cue-type colours: distinct from each other and from the three state
/// signals, so a type tag never reads as standby / active / panic.
pub fn kind_color(k: livestage_core::cue::CueKind) -> Color32 {
    use livestage_core::cue::CueKind;
    match k {
        CueKind::Audio => Color32::from_rgb(0x4C, 0x8D, 0xFF),
        CueKind::Video => Color32::from_rgb(0xA2, 0x7B, 0xFF),
        CueKind::Slides => Color32::from_rgb(0xE0, 0x7B, 0xD0),
        CueKind::Timer => Color32::from_rgb(0xC9, 0xD1, 0xDE),
        CueKind::Dmx => Color32::from_rgb(0x47, 0xC2, 0xB5),
        CueKind::Control => Color32::from_rgb(0x8A, 0x8F, 0xA3),
        CueKind::Timeline => Color32::from_rgb(0xD4, 0xA3, 0x73),
    }
}

pub fn state_color(s: CueState) -> Option<Color32> {
    s.color().map(rgb)
}

/// A signal colour as a translucent wash over a dark surface.
pub fn wash(c: Color32, strength: f32) -> Color32 {
    c.gamma_multiply(strength)
}

// ------------------------------------------------------------------- fonts --

/// Platform UI faces, tried in order (crisp system sans + monospace with
/// tabular figures). egui's bundled faces stay as fallbacks.
const SANS: &[&str] = &["/System/Library/Fonts/SFNS.ttf", "C:\\Windows\\Fonts\\segoeui.ttf", "/usr/share/fonts/truetype/inter/Inter-Regular.ttf"];
const MONO: &[&str] = &["/System/Library/Fonts/SFNSMono.ttf", "C:\\Windows\\Fonts\\consola.ttf", "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf"];

fn first_font(paths: &[&str]) -> Option<Vec<u8>> {
    paths.iter().find_map(|p| std::fs::read(p).ok())
}

/// Installs the system faces (when present) ahead of egui's bundled ones,
/// plus the bundled monospace face (Hack) as a last fallback for
/// proportional text: it carries arrows and symbols (→ ← ⇧ ↵ ▸) some UI
/// faces lack. Installed once per context.
fn install_fonts(ctx: &egui::Context) {
    let flag = egui::Id::new("livestage-fonts");
    if ctx.data(|d| d.get_temp::<bool>(flag)).unwrap_or(false) {
        return;
    }
    let mut fonts = egui::FontDefinitions::default();
    if let Some(bytes) = first_font(SANS) {
        fonts.font_data.insert("ui-sans".into(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
        fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "ui-sans".into());
    }
    if let Some(bytes) = first_font(MONO) {
        fonts.font_data.insert("ui-mono".into(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
        fonts.families.entry(FontFamily::Monospace).or_default().insert(0, "ui-mono".into());
    }
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
    v.extreme_bg_color = BG_DEEP;
    v.faint_bg_color = ROW_ALT;
    v.code_bg_color = BG_DEEP;
    v.window_stroke = Stroke::new(1.0_f32, BORDER_HI);
    v.window_corner_radius = CornerRadius::same(8);
    v.window_shadow = egui::Shadow { offset: [0, 10], blur: 28, spread: 0, color: Color32::from_black_alpha(170) };
    v.selection.bg_fill = accent_dim();
    v.selection.stroke = Stroke::new(1.0_f32, accent());
    v.hyperlink_color = accent();
    v.warn_fg_color = standby();
    v.error_fg_color = stop_text();
    v.striped = true;
    let r = CornerRadius::same(4);
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = r;
        w.expansion = 0.0;
    }
    v.widgets.noninteractive.bg_fill = PANEL_RAISED;
    v.widgets.noninteractive.weak_bg_fill = PANEL_RAISED;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT_DIM);
    // Controls: a defined face with a hairline edge, brightening on hover,
    // accent edge while pressed.
    v.widgets.inactive.bg_fill = CONTROL;
    v.widgets.inactive.weak_bg_fill = CONTROL;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    v.widgets.hovered.bg_fill = CONTROL_HOVER;
    v.widgets.hovered.weak_bg_fill = CONTROL_HOVER;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, BORDER_HI);
    v.widgets.hovered.fg_stroke = Stroke::new(1.5_f32, TEXT);
    v.widgets.active.bg_fill = CONTROL_ACTIVE;
    v.widgets.active.weak_bg_fill = CONTROL_ACTIVE;
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, accent());
    v.widgets.active.fg_stroke = Stroke::new(1.5_f32, TEXT);
    v.widgets.open.bg_fill = CONTROL_ACTIVE;
    v.widgets.open.weak_bg_fill = CONTROL_ACTIVE;
    v.widgets.open.bg_stroke = Stroke::new(1.0_f32, BORDER_HI);
    v.menu_corner_radius = CornerRadius::same(8);
    v.popup_shadow = egui::Shadow { offset: [0, 8], blur: 22, spread: 0, color: Color32::from_black_alpha(170) };
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Rect { aspect_ratio: 0.55 };
    ctx.set_visuals(v);

    ctx.style_mut(|s| {
        s.text_styles.insert(TextStyle::Small, FontId::new(11.0, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Body, FontId::new(13.5, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Button, FontId::new(13.0, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Heading, FontId::new(17.0, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace));
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(11.0, 5.0);
        s.spacing.interact_size.y = 26.0;
        s.spacing.menu_margin = egui::Margin::same(6);
        s.spacing.menu_spacing = 2.0;
        s.spacing.slider_rail_height = 4.0;
        s.spacing.window_margin = egui::Margin::same(14);
    });
}

/// Large tabular figures for clocks and timers.
pub fn clock_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

/// Dock styling: obsidian tab strip, panel bodies, accent bar over the active tab.
pub fn dock_style(base: &egui::Style) -> egui_dock::Style {
    let mut s = egui_dock::Style::from_egui(base);
    s.tab_bar.bg_fill = BG_DEEP;
    s.tab_bar.height = 30.0;
    s.tab_bar.hline_color = BORDER;
    s.tab_bar.corner_radius = CornerRadius::ZERO;
    s.tab.hline_below_active_tab_name = true;
    s.tab.minimum_width = Some(84.0);
    s.tab.tab_body.bg_fill = PANEL_RAISED;
    s.tab.tab_body.inner_margin = egui::Margin::same(10);
    s.tab.tab_body.stroke = Stroke::new(1.0_f32, BORDER);
    s.tab.tab_body.corner_radius = CornerRadius::ZERO;
    for t in [&mut s.tab.active, &mut s.tab.focused, &mut s.tab.active_with_kb_focus, &mut s.tab.focused_with_kb_focus] {
        t.bg_fill = PANEL_RAISED;
        t.text_color = TEXT;
        t.outline_color = BORDER;
        t.corner_radius = CornerRadius::ZERO;
    }
    for t in [&mut s.tab.inactive, &mut s.tab.inactive_with_kb_focus, &mut s.tab.hovered] {
        t.bg_fill = BG_DEEP;
        t.text_color = TEXT_FAINT;
        t.outline_color = BG_DEEP;
        t.corner_radius = CornerRadius::ZERO;
    }
    s.tab.hovered.text_color = TEXT_DIM;
    s.separator.color_idle = BG_DEEP;
    s.separator.color_hovered = accent();
    s.separator.color_dragged = accent();
    s.separator.width = 4.0;
    s.main_surface_border_stroke = Stroke::NONE;
    s
}

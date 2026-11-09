//! Design system: native-feeling dark and light appearances (macOS HIG /
//! Windows Fluent), one set of tokens for every panel.
//!
//! * **Appearance** follows the OS by default ([`Appearance::System`]) and
//!   switches live when the OS does; Dark and Light can be forced.
//! * **Colours** are read through functions ([`text`], [`border`], ...) so
//!   they follow the appearance. Nothing outside this module hard-codes a
//!   surface or text colour.
//! * **Signals** keep their meaning in both appearances: standby amber,
//!   active / GO green, stop red. Cue categories have fixed badge colours
//!   (audio blue, video purple, DMX amber, control green).
//! * **Geometry**: 4 px control radius, 1 px hairlines, an 8 px spacing
//!   grid, 2 px focus rings in the accent colour.
//! * **Type**: the platform UI face (SF Pro on macOS, Segoe UI on Windows)
//!   and its monospace companion for every clock and meter readout, so
//!   figures never jitter. Sizes follow the macOS HIG scale; tracking and
//!   leading are size-specific ([`tracking`], [`leading`]): large text is
//!   tightened, small text opened up, never one letter-spacing for all.
//! * **Depth**: surfaces get darker / heavier as they get more structural
//!   (header deck and tab strips deepest, panel bodies raised, controls
//!   lightest); bigger floating surfaces cast deeper shadows; floating
//!   chrome meets content with a soft edge shadow rather than a hard line.
//!   Modal tasks dim what is behind them. Accessibility display settings
//!   (increase contrast, reduce transparency, reduce motion) are honoured.
//! * **Motion**: springs from [`crate::motion`]; controls react on
//!   pointer-down.

use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Visuals};
use livestage_core::state::{palette, CueState, Rgb};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

// ------------------------------------------------------------------ tokens --

/// Control corner radius (buttons, fields, badges' boxes).
pub const RADIUS: u8 = 4;
/// Window and popup corner radius.
pub const WINDOW_RADIUS: u8 = 8;
/// Base spacing unit: layouts use multiples of it.
pub const GRID: f32 = 8.0;
/// Hairline width.
pub const HAIRLINE: f32 = 1.0;
/// Keyboard focus ring width.
pub const FOCUS_RING: f32 = 2.0;
/// Standard control height.
pub const CONTROL_HEIGHT: f32 = 28.0;

// -------------------------------------------------------------- appearance --

/// Light / dark choice (per machine, in the preferences).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Appearance {
    /// Follow the operating system.
    #[default]
    System,
    Dark,
    Light,
}

impl Appearance {
    pub const ALL: [Appearance; 3] = [Appearance::System, Appearance::Dark, Appearance::Light];
    pub fn label(self) -> &'static str {
        match self {
            Appearance::System => "Match system",
            Appearance::Dark => "Dark",
            Appearance::Light => "Light",
        }
    }
}

static APPEARANCE: AtomicU8 = AtomicU8::new(0);
/// The palette in force (resolved from the appearance and the OS).
static LIGHT: AtomicBool = AtomicBool::new(false);

pub fn set_appearance(a: Appearance) {
    let i = Appearance::ALL.iter().position(|x| *x == a).unwrap_or(0) as u8;
    APPEARANCE.store(i, Ordering::Relaxed);
}

pub fn appearance() -> Appearance {
    Appearance::ALL[APPEARANCE.load(Ordering::Relaxed) as usize % Appearance::ALL.len()]
}

/// The light palette is in force.
pub fn is_light() -> bool {
    LIGHT.load(Ordering::Relaxed)
}

fn wants_light(ctx: &egui::Context) -> bool {
    match appearance() {
        Appearance::Light => true,
        Appearance::Dark => false,
        Appearance::System => ctx.system_theme() == Some(egui::Theme::Light),
    }
}

/// Re-applies the theme when the OS appearance (or the preference) changed
/// since the last frame. Cheap; call once per frame.
pub fn sync(ctx: &egui::Context) {
    // OS accessibility settings, about once a second.
    let now = ctx.input(|i| i.time);
    let poll = egui::Id::new("livestage-a11y-poll");
    let last: f64 = ctx.data(|d| d.get_temp(poll)).unwrap_or(f64::NEG_INFINITY);
    let mut changed = false;
    if now - last >= 1.0 || now < last {
        ctx.data_mut(|d| d.insert_temp(poll, now));
        changed = crate::motion::refresh_os_settings();
    }
    if changed || wants_light(ctx) != is_light() {
        apply(ctx);
    }
}

// ----------------------------------------------------------------- palette --

struct Palette {
    /// Deepest well: header deck, tab strip, text fields, meters.
    bg_deep: Color32,
    /// Window background behind panels.
    bg: Color32,
    /// Recessed surfaces inside panels (table header, tracks, wells).
    recessed: Color32,
    /// Panel bodies.
    raised: Color32,
    /// Alternate table rows.
    row_alt: Color32,
    /// Control faces.
    control: Color32,
    control_hover: Color32,
    control_active: Color32,
    border: Color32,
    border_hi: Color32,
    text: Color32,
    text_dim: Color32,
    text_faint: Color32,
    menu_fill: Color32,
    shadow: Color32,
    standby: Color32,
    active: Color32,
    stop: Color32,
    stop_text: Color32,
}

const DARK: Palette = Palette {
    bg_deep: Color32::from_rgb(0x0B, 0x0B, 0x0E),
    bg: Color32::from_rgb(0x10, 0x10, 0x13),
    recessed: Color32::from_rgb(0x13, 0x13, 0x17),
    raised: Color32::from_rgb(0x18, 0x18, 0x1C),
    row_alt: Color32::from_rgb(0x1C, 0x1C, 0x21),
    control: Color32::from_rgb(0x24, 0x24, 0x2A),
    control_hover: Color32::from_rgb(0x2E, 0x2E, 0x36),
    control_active: Color32::from_rgb(0x38, 0x38, 0x42),
    border: Color32::from_rgb(0x2A, 0x2A, 0x32),
    border_hi: Color32::from_rgb(0x44, 0x44, 0x50),
    text: Color32::from_rgb(0xF2, 0xF2, 0xF7),
    text_dim: Color32::from_rgb(0x9A, 0x9A, 0xA4),
    text_faint: Color32::from_rgb(0x63, 0x63, 0x6E),
    menu_fill: Color32::from_rgb(0x1F, 0x1F, 0x25),
    shadow: Color32::from_black_alpha(160),
    standby: Color32::from_rgb(palette::STANDBY.0, palette::STANDBY.1, palette::STANDBY.2),
    active: Color32::from_rgb(palette::ACTIVE.0, palette::ACTIVE.1, palette::ACTIVE.2),
    stop: Color32::from_rgb(palette::STOP.0, palette::STOP.1, palette::STOP.2),
    stop_text: Color32::from_rgb(0xFF, 0x4D, 0x6A),
};

const LIGHT_PALETTE: Palette = Palette {
    bg_deep: Color32::from_rgb(0xE8, 0xE8, 0xEC),
    bg: Color32::from_rgb(0xEF, 0xEF, 0xF2),
    recessed: Color32::from_rgb(0xF4, 0xF4, 0xF6),
    raised: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    row_alt: Color32::from_rgb(0xF6, 0xF6, 0xF8),
    control: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    control_hover: Color32::from_rgb(0xF2, 0xF2, 0xF5),
    control_active: Color32::from_rgb(0xE4, 0xE4, 0xE9),
    border: Color32::from_rgb(0xD4, 0xD4, 0xDA),
    border_hi: Color32::from_rgb(0xB0, 0xB0, 0xBA),
    text: Color32::from_rgb(0x1C, 0x1C, 0x1E),
    text_dim: Color32::from_rgb(0x5E, 0x5E, 0x66),
    text_faint: Color32::from_rgb(0x96, 0x96, 0x9E),
    menu_fill: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    shadow: Color32::from_black_alpha(60),
    // Deeper signal shades: the dark-mode neons are unreadable on white.
    standby: Color32::from_rgb(0xB4, 0x5F, 0x06),
    active: Color32::from_rgb(0x04, 0x84, 0x5C),
    stop: Color32::from_rgb(0xC8, 0x10, 0x34),
    stop_text: Color32::from_rgb(0xB9, 0x1C, 0x1C),
};

fn p() -> &'static Palette {
    if is_light() {
        &LIGHT_PALETTE
    } else {
        &DARK
    }
}

pub fn bg_deep() -> Color32 {
    p().bg_deep
}
pub fn bg() -> Color32 {
    p().bg
}
pub fn recessed() -> Color32 {
    p().recessed
}
pub fn raised() -> Color32 {
    p().raised
}
pub fn row_alt() -> Color32 {
    p().row_alt
}
pub fn control() -> Color32 {
    p().control
}
pub fn control_hover() -> Color32 {
    p().control_hover
}
pub fn control_active() -> Color32 {
    p().control_active
}
pub fn border() -> Color32 {
    // Increase contrast: every hairline becomes a defined edge.
    if crate::motion::increase_contrast() {
        p().border_hi
    } else {
        p().border
    }
}
pub fn border_hi() -> Color32 {
    p().border_hi
}
pub fn text() -> Color32 {
    p().text
}
pub fn text_dim() -> Color32 {
    if crate::motion::increase_contrast() {
        p().text_dim.lerp_to_gamma(p().text, 0.4)
    } else {
        p().text_dim
    }
}
pub fn text_faint() -> Color32 {
    if crate::motion::increase_contrast() {
        p().text_dim
    } else {
        p().text_faint
    }
}
pub fn menu_fill() -> Color32 {
    p().menu_fill
}
/// Scrim over content (disabled meters, inactive channels).
pub fn scrim() -> Color32 {
    if is_light() {
        Color32::from_white_alpha(170)
    } else {
        Color32::from_black_alpha(160)
    }
}
/// Dims the window behind a modal task (alerts, About).
pub fn modal_scrim() -> Color32 {
    // Reduce transparency: an almost solid backdrop.
    let solid = crate::motion::reduce_transparency();
    match (is_light(), solid) {
        (true, false) => Color32::from_black_alpha(70),
        (false, false) => Color32::from_black_alpha(130),
        (true, true) => Color32::from_black_alpha(150),
        (false, true) => Color32::from_black_alpha(210),
    }
}
/// Hairline stroke.
pub fn hairline() -> Stroke {
    Stroke::new(HAIRLINE, border())
}

/// A soft shadow cast by floating chrome onto the content beside it (a
/// scroll-edge effect instead of a hard divider): `edge` is the chrome's
/// edge line, the shadow falls `depth` points away from it, towards `dir`
/// (+1 down / right of a horizontal / vertical edge, -1 up / left).
pub fn edge_shadow(p: &egui::Painter, edge: egui::Rect, horizontal: bool, dir: f32, depth: f32) {
    let strength: u8 = if is_light() { 26 } else { 90 };
    let dark = Color32::from_black_alpha(strength);
    let clear = Color32::TRANSPARENT;
    let mut mesh = egui::Mesh::default();
    let (a, b, c, d) = if horizontal {
        let y0 = if dir > 0.0 { edge.bottom() } else { edge.top() };
        let y1 = y0 + dir * depth;
        (egui::pos2(edge.left(), y0), egui::pos2(edge.right(), y0), egui::pos2(edge.right(), y1), egui::pos2(edge.left(), y1))
    } else {
        let x0 = if dir > 0.0 { edge.right() } else { edge.left() };
        let x1 = x0 + dir * depth;
        (egui::pos2(x0, edge.bottom()), egui::pos2(x0, edge.top()), egui::pos2(x1, edge.top()), egui::pos2(x1, edge.bottom()))
    };
    let base = mesh.vertices.len() as u32;
    for (pos, color) in [(a, dark), (b, dark), (c, clear), (d, clear)] {
        mesh.colored_vertex(pos, color);
    }
    mesh.add_triangle(base, base + 1, base + 2);
    mesh.add_triangle(base, base + 2, base + 3);
    p.add(mesh);
}

// ------------------------------------------------------------------ accent --

/// Operator-selectable UI accent (selection, focus, menu highlight). Cue
/// state colours (standby / active / GO / stop) never change: they carry meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Accent {
    /// Cyan: distinct from every state colour.
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
    /// (bright, deep) shades.
    fn shades(self) -> (Color32, Color32) {
        match self {
            Accent::Ice => (Color32::from_rgb(0x38, 0xC8, 0xFF), Color32::from_rgb(0x0A, 0x7A, 0xB0)),
            Accent::Emerald => (Color32::from_rgb(0x00, 0xC8, 0x80), Color32::from_rgb(0x05, 0x7A, 0x55)),
            Accent::Blue => (Color32::from_rgb(0x4C, 0x8D, 0xFF), Color32::from_rgb(0x1D, 0x5B, 0xD8)),
            Accent::Violet => (Color32::from_rgb(0xA2, 0x7B, 0xFF), Color32::from_rgb(0x6D, 0x3F, 0xD0)),
            Accent::Amber => (Color32::from_rgb(0xFF, 0xB0, 0x20), Color32::from_rgb(0xA3, 0x5E, 0x00)),
            Accent::Rose => (Color32::from_rgb(0xFF, 0x5C, 0x8A), Color32::from_rgb(0xBE, 0x2A, 0x58)),
        }
    }
    /// The swatch shown in Settings.
    pub fn swatch(self) -> Color32 {
        self.shades().0
    }
}

static ACCENT: AtomicU8 = AtomicU8::new(0);

pub fn set_accent(a: Accent) {
    let i = Accent::ALL.iter().position(|x| *x == a).unwrap_or(0) as u8;
    ACCENT.store(i, Ordering::Relaxed);
}
pub fn current_accent() -> Accent {
    Accent::ALL[ACCENT.load(Ordering::Relaxed) as usize % Accent::ALL.len()]
}
/// Focus rings, active tab marker, links.
pub fn accent() -> Color32 {
    let (bright, deep) = current_accent().shades();
    if is_light() {
        deep
    } else {
        bright
    }
}
/// Menu row highlight (white text on it).
pub fn accent_menu() -> Color32 {
    current_accent().shades().1
}
/// Selected rows / text selection.
pub fn accent_dim() -> Color32 {
    let (bright, deep) = current_accent().shades();
    if is_light() {
        Color32::WHITE.lerp_to_gamma(bright, 0.22)
    } else {
        Color32::BLACK.lerp_to_gamma(deep, 0.45)
    }
}

// ----------------------------------------------------------------- signals --

pub fn rgb(c: Rgb) -> Color32 {
    Color32::from_rgb(c.0, c.1, c.2)
}
/// Standby amber.
pub fn standby() -> Color32 {
    p().standby
}
/// Active green.
pub fn active() -> Color32 {
    p().active
}
/// The GO key's face (the same in both appearances: it is a lit key).
pub fn go() -> Color32 {
    rgb(palette::GO)
}
/// Stop / panic / errors.
pub fn stop() -> Color32 {
    p().stop
}
/// Stop red for small text (the fill shade is too deep to read small on dark).
pub fn stop_text() -> Color32 {
    p().stop_text
}

/// Cue-category badge colours.
pub mod category {
    use egui::Color32;
    /// Deep blue `#2563EB`.
    pub const AUDIO: Color32 = Color32::from_rgb(0x25, 0x63, 0xEB);
    /// Vivid purple `#9333EA`.
    pub const VIDEO: Color32 = Color32::from_rgb(0x93, 0x33, 0xEA);
    /// Amber `#D97706`.
    pub const DMX: Color32 = Color32::from_rgb(0xD9, 0x77, 0x06);
    /// Emerald `#059669`.
    pub const CONTROL: Color32 = Color32::from_rgb(0x05, 0x96, 0x69);
    /// Presentations: pink, next to video.
    pub const SLIDES: Color32 = Color32::from_rgb(0xDB, 0x27, 0x77);
    /// Timers: slate.
    pub const TIMER: Color32 = Color32::from_rgb(0x64, 0x74, 0x8B);
    /// Timelines: cyan.
    pub const TIMELINE: Color32 = Color32::from_rgb(0x08, 0x91, 0xB2);
}

/// Badge colour of a cue category.
pub fn kind_color(k: livestage_core::cue::CueKind) -> Color32 {
    use livestage_core::cue::CueKind;
    match k {
        CueKind::Audio => category::AUDIO,
        CueKind::Video => category::VIDEO,
        CueKind::Slides => category::SLIDES,
        CueKind::Timer => category::TIMER,
        CueKind::Dmx => category::DMX,
        CueKind::Control => category::CONTROL,
        CueKind::Timeline => category::TIMELINE,
    }
}

/// Text colour with the most contrast on `fill` (black or white).
pub fn on(fill: Color32) -> Color32 {
    let lin = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let l = 0.2126 * lin(fill.r()) + 0.7152 * lin(fill.g()) + 0.0722 * lin(fill.b());
    // Contrast against white vs black: (1.05) / (l + 0.05) vs (l + 0.05) / 0.05.
    if 1.05 / (l + 0.05) >= (l + 0.05) / 0.05 {
        Color32::WHITE
    } else {
        Color32::from_rgb(0x11, 0x11, 0x14)
    }
}

/// Colour of a cue state (none for idle / completed).
pub fn state_color(s: CueState) -> Option<Color32> {
    match s {
        CueState::Standby => Some(standby()),
        CueState::PreWait | CueState::Running | CueState::Paused => Some(active()),
        CueState::Stopped => Some(stop()),
        CueState::Idle | CueState::Completed => None,
    }
}

/// A signal colour as a translucent wash over the current surfaces.
pub fn wash(c: Color32, strength: f32) -> Color32 {
    c.gamma_multiply(strength)
}

// ------------------------------------------------------------------- fonts --

/// Platform UI faces, tried in order (Segoe UI Variable is Windows 11's).
/// egui's bundled faces stay as fallbacks.
const SANS: &[&str] = &[
    "/System/Library/Fonts/SFNS.ttf",
    "C:\\Windows\\Fonts\\SegUIVar.ttf",
    "C:\\Windows\\Fonts\\segoeui.ttf",
    "/usr/share/fonts/truetype/inter/Inter-Regular.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
];
/// Fixed-width companions for clocks, meters and tabular figures.
const MONO: &[&str] = &[
    "/System/Library/Fonts/SFNSMono.ttf",
    "C:\\Windows\\Fonts\\CascadiaMono.ttf",
    "C:\\Windows\\Fonts\\consola.ttf",
    "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
];

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

/// Type scale (macOS HIG text styles).
pub mod type_scale {
    /// Caption: column headers, legends.
    pub const CAPTION: f32 = 10.0;
    /// Subheadline / footnote: secondary lines, status bar.
    pub const SMALL: f32 = 11.0;
    /// Callout: hints under controls.
    pub const CALLOUT: f32 = 12.0;
    /// Body and controls.
    pub const BODY: f32 = 13.0;
    /// Title 3: panel titles, the show name.
    pub const SUBHEAD: f32 = 15.0;
    /// Title 2: window and section headings.
    pub const HEADING: f32 = 17.0;
    /// Title 1: dialog titles.
    pub const TITLE: f32 = 22.0;
}

/// Letter spacing for `size`-point text, in points: slightly open at
/// caption sizes, neutral at 12 pt, progressively tighter as text grows
/// (about -0.02 em for display sizes), after SF Pro's tracking table.
pub fn tracking(size: f32) -> f32 {
    // (size pt, tracking em)
    const TABLE: [(f32, f32); 7] = [(9.0, 0.012), (11.0, 0.006), (12.0, 0.0), (13.0, -0.006), (15.0, -0.014), (17.0, -0.022), (28.0, -0.02)];
    let em = if size <= TABLE[0].0 {
        TABLE[0].1
    } else if size >= TABLE[TABLE.len() - 1].0 {
        TABLE[TABLE.len() - 1].1
    } else {
        let i = TABLE.windows(2).position(|w| size < w[1].0).unwrap_or(0);
        let ((s0, t0), (s1, t1)) = (TABLE[i], TABLE[i + 1]);
        t0 + (t1 - t0) * (size - s0) / (s1 - s0)
    };
    em * size
}

/// Line height for `size`-point text: comfortable for body copy, tight
/// for large type (leading shrinks as size grows).
pub fn leading(size: f32) -> f32 {
    let ratio = if size <= 13.0 { 1.38 } else if size >= 28.0 { 1.12 } else { 1.38 - (size - 13.0) / 15.0 * 0.26 };
    (size * ratio).round()
}

/// Text at `size` with its size-specific tracking and leading.
pub fn styled(text: impl Into<String>, size: f32) -> egui::RichText {
    egui::RichText::new(text).size(size).extra_letter_spacing(tracking(size)).line_height(Some(leading(size)))
}

/// A single-line galley for painter text with the size-specific tracking
/// (`Painter::text` has none).
pub fn tracked_galley(p: &egui::Painter, text: impl Into<String>, font: FontId, color: Color32) -> std::sync::Arc<egui::Galley> {
    let size = font.size;
    let mut job = egui::text::LayoutJob::single_section(
        text.into(),
        egui::TextFormat { font_id: font, color, extra_letter_spacing: tracking(size), ..Default::default() },
    );
    job.wrap.max_width = f32::INFINITY;
    p.layout_job(job)
}

/// Painter text with size-specific tracking, anchored like `Painter::text`.
pub fn tracked_text(p: &egui::Painter, pos: egui::Pos2, anchor: egui::Align2, text: impl Into<String>, font: FontId, color: Color32) -> egui::Rect {
    let g = tracked_galley(p, text, font, color);
    let rect = anchor.anchor_size(pos, g.size());
    p.galley(rect.min, g, color);
    rect
}

// ------------------------------------------------------------------- apply --

/// Builds and installs the visuals for the current appearance.
pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    let light = wants_light(ctx);
    LIGHT.store(light, Ordering::Relaxed);
    let mut v = if light { Visuals::light() } else { Visuals::dark() };
    v.override_text_color = Some(text());
    v.panel_fill = raised();
    v.window_fill = menu_fill();
    v.extreme_bg_color = if light { Color32::WHITE } else { bg_deep() };
    v.text_edit_bg_color = Some(if light { Color32::WHITE } else { bg_deep() });
    v.faint_bg_color = row_alt();
    v.code_bg_color = recessed();
    v.window_stroke = Stroke::new(HAIRLINE, border_hi());
    v.window_corner_radius = CornerRadius::same(WINDOW_RADIUS);
    v.menu_corner_radius = CornerRadius::same(WINDOW_RADIUS);
    // Bigger surfaces read as thicker: windows cast a deep, soft shadow,
    // menus and popovers a short one.
    v.window_shadow = egui::Shadow { offset: [0, 16], blur: 44, spread: 0, color: p().shadow };
    v.popup_shadow = egui::Shadow { offset: [0, 5], blur: 16, spread: 0, color: p().shadow.gamma_multiply(0.75) };
    // Selection and keyboard focus: the accent, 2 px.
    v.selection.bg_fill = accent_dim();
    v.selection.stroke = Stroke::new(FOCUS_RING, accent());
    v.hyperlink_color = accent();
    v.warn_fg_color = standby();
    v.error_fg_color = stop_text();
    // Tables that want bands ask for them; forms and settings read cleaner without.
    v.striped = false;
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Circle;
    v.indent_has_left_vline = false;
    let r = CornerRadius::same(RADIUS);
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = r;
        w.expansion = 0.0;
    }
    v.widgets.noninteractive.bg_fill = raised();
    v.widgets.noninteractive.weak_bg_fill = raised();
    v.widgets.noninteractive.bg_stroke = hairline();
    v.widgets.noninteractive.fg_stroke = Stroke::new(HAIRLINE, text_dim());
    // Controls: a defined face with a hairline edge, a lighter face on
    // hover, and the accent edge while pressed.
    v.widgets.inactive.bg_fill = control();
    v.widgets.inactive.weak_bg_fill = control();
    v.widgets.inactive.bg_stroke = Stroke::new(HAIRLINE, if light { border_hi() } else { border() });
    v.widgets.inactive.fg_stroke = Stroke::new(HAIRLINE, text());
    v.widgets.hovered.bg_fill = control_hover();
    v.widgets.hovered.weak_bg_fill = control_hover();
    v.widgets.hovered.bg_stroke = Stroke::new(HAIRLINE, border_hi());
    v.widgets.hovered.fg_stroke = Stroke::new(1.5, text());
    // Pressed: the face sinks in by a point the instant the pointer goes
    // down (the web's `:active { scale(0.97) }`), before anything commits.
    v.widgets.active.expansion = -1.0;
    v.widgets.active.bg_fill = control_active();
    v.widgets.active.weak_bg_fill = control_active();
    v.widgets.active.bg_stroke = Stroke::new(HAIRLINE, accent());
    v.widgets.active.fg_stroke = Stroke::new(1.5, text());
    v.widgets.open.bg_fill = control_active();
    v.widgets.open.weak_bg_fill = control_active();
    v.widgets.open.bg_stroke = Stroke::new(HAIRLINE, border_hi());
    ctx.set_visuals_of(if light { egui::Theme::Light } else { egui::Theme::Dark }, v);
    // Crisp at any scale (Retina, 125 / 150 % Windows, mixed-DPI setups):
    // text, rectangles and straight lines land on whole physical pixels and
    // edges are anti-aliased over exactly one pixel. egui-winit feeds the
    // window's scale factor (and its changes when the window moves between
    // monitors), so everything is laid out in points and rasterised at the
    // display's native resolution.
    ctx.tessellation_options_mut(|t| {
        t.feathering = true;
        t.feathering_size_in_pixels = 1.0;
        t.round_text_to_pixels = true;
        t.round_rects_to_pixels = true;
        t.round_line_segments_to_pixels = true;
        t.prerasterized_discs = true;
    });
    ctx.set_theme(if light { egui::Theme::Light } else { egui::Theme::Dark });

    use type_scale::*;
    ctx.all_styles_mut(|s| {
        s.text_styles.insert(TextStyle::Small, FontId::new(SMALL, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Name("Caption".into()), FontId::new(CAPTION, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Name("Title".into()), FontId::new(TITLE, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Body, FontId::new(BODY, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Button, FontId::new(BODY, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Heading, FontId::new(HEADING, FontFamily::Proportional));
        s.text_styles.insert(TextStyle::Monospace, FontId::new(BODY, FontFamily::Monospace));
        s.spacing.item_spacing = egui::vec2(GRID, GRID * 0.75);
        s.spacing.button_padding = egui::vec2(GRID * 1.5, GRID * 0.5);
        s.spacing.interact_size.y = CONTROL_HEIGHT - 4.0;
        s.spacing.menu_margin = egui::Margin::same(GRID as i8 * 3 / 4);
        s.spacing.menu_spacing = 2.0;
        s.spacing.slider_rail_height = 4.0;
        s.spacing.window_margin = egui::Margin::same(GRID as i8 * 2);
        s.spacing.icon_width = 16.0;
        s.spacing.combo_width = GRID * 20.0;
        s.interaction.tooltip_delay = 0.5;
        // egui's own transitions (collapsing sections, window fades) are
        // short; anything a user touches springs (crate::motion) instead.
        s.animation_time = if crate::motion::reduce_motion() { 0.0 } else { 0.15 };
    });
}

/// Fixed-width figures for clocks and timers.
pub fn clock_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

/// Dock styling: tab strip on the deep surface, panel bodies raised, an
/// accent bar under the active tab.
pub fn dock_style(base: &egui::Style) -> egui_dock::Style {
    let mut s = egui_dock::Style::from_egui(base);
    // Each dock leaf reads as one rounded card: the tab strip is its
    // (heavier) top edge, the body its raised face.
    let top = CornerRadius { nw: WINDOW_RADIUS, ne: WINDOW_RADIUS, sw: 0, se: 0 };
    let bottom = CornerRadius { nw: 0, ne: 0, sw: WINDOW_RADIUS, se: WINDOW_RADIUS };
    s.tab_bar.bg_fill = bg_deep();
    s.tab_bar.height = 30.0;
    s.tab_bar.hline_color = border();
    s.tab_bar.corner_radius = top;
    s.tab.hline_below_active_tab_name = true;
    s.tab.minimum_width = Some(GRID * 10.0);
    // Breathing room between long titles ("Display Routing", "Audio Mixer").
    s.tab.spacing = GRID * 0.5;
    s.tab.tab_body.bg_fill = raised();
    s.tab.tab_body.inner_margin = egui::Margin::same(GRID as i8 + 2);
    s.tab.tab_body.stroke = hairline();
    s.tab.tab_body.corner_radius = bottom;
    for t in [&mut s.tab.active, &mut s.tab.focused, &mut s.tab.active_with_kb_focus, &mut s.tab.focused_with_kb_focus] {
        t.bg_fill = raised();
        t.text_color = text();
        t.outline_color = border();
        t.corner_radius = top;
    }
    for t in [&mut s.tab.inactive, &mut s.tab.inactive_with_kb_focus, &mut s.tab.hovered] {
        t.bg_fill = bg_deep();
        t.text_color = text_dim();
        t.outline_color = bg_deep();
        t.corner_radius = top;
    }
    s.tab.hovered.text_color = text();
    s.separator.color_idle = bg_deep();
    s.separator.color_hovered = accent();
    s.separator.color_dragged = accent();
    s.separator.width = 4.0;
    s.main_surface_border_stroke = Stroke::NONE;
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG contrast ratio.
    fn contrast(a: Color32, b: Color32) -> f32 {
        let l = |c: Color32| {
            let lin = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
        };
        let (x, y) = (l(a), l(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn both_palettes_keep_text_and_signals_legible() {
        for pal in [&DARK, &LIGHT_PALETTE] {
            for surface in [pal.raised, pal.bg_deep, pal.row_alt] {
                assert!(contrast(pal.text, surface) >= 12.0, "body text");
                assert!(contrast(pal.text_dim, surface) >= 4.5, "secondary text (WCAG AA)");
                for signal in [pal.standby, pal.active, pal.stop_text] {
                    assert!(contrast(signal, surface) >= 3.0, "signal colour {signal:?} on {surface:?}");
                }
            }
        }
    }

    #[test]
    fn tracking_tightens_and_leading_shrinks_as_type_grows() {
        assert!(tracking(10.0) > 0.0, "captions open up");
        assert_eq!(tracking(12.0), 0.0);
        assert!(tracking(13.0) < 0.0 && tracking(13.0) > -0.1, "body near zero");
        assert!(tracking(44.0) < tracking(17.0), "display text tighter in points");
        assert!((tracking(44.0) / 44.0 + 0.02).abs() < 1e-4, "-0.02 em at display sizes");
        let mut last = f32::INFINITY;
        for size in [9.0, 11.0, 12.0, 13.0, 15.0, 17.0, 22.0, 28.0, 44.0] {
            let em = tracking(size) / size;
            assert!(em <= last + 0.004, "monotonic-ish at {size}");
            last = em;
        }
        assert!(leading(13.0) / 13.0 > leading(28.0) / 28.0);
        assert_eq!(leading(13.0), 18.0);
    }

    #[test]
    fn badge_text_picks_the_readable_ink() {
        for c in [category::AUDIO, category::VIDEO, category::DMX, category::CONTROL, category::SLIDES, category::TIMER, category::TIMELINE] {
            assert!(contrast(on(c), c) >= 4.5, "badge {c:?}");
        }
        assert_eq!(on(Color32::WHITE), Color32::from_rgb(0x11, 0x11, 0x14));
        assert_eq!(on(Color32::BLACK), Color32::WHITE);
    }

    #[test]
    fn appearance_follows_the_system_until_forced() {
        let ctx = egui::Context::default();
        set_appearance(Appearance::Light);
        apply(&ctx);
        assert!(is_light());
        assert_eq!(ctx.style_of(egui::Theme::Light).visuals.panel_fill, Color32::WHITE);
        assert_eq!(ctx.theme(), egui::Theme::Light);
        set_appearance(Appearance::Dark);
        sync(&ctx);
        assert!(!is_light());
        assert_eq!(ctx.style_of(egui::Theme::Dark).visuals.panel_fill, DARK.raised);
        assert_eq!(ctx.theme(), egui::Theme::Dark);
        ctx.tessellation_options(|t| assert!(t.round_text_to_pixels && t.round_rects_to_pixels && t.round_line_segments_to_pixels && t.feathering_size_in_pixels == 1.0));
        set_appearance(Appearance::System);
    }
}

//! Reusable control library on the [`crate::theme`] tokens: keys and
//! switches, telemetry cards, badges and pills, menu rows, encoders and
//! small editors here; the GO button, SMPTE clock, VU meter and the
//! standard inputs in their own modules.

pub mod clock;
pub mod go_button;
pub mod inputs;
pub mod meter;

use egui::{DragValue, Ui};
use livestage_core::ShowTime;

/// Seconds editor for a [`ShowTime`]. Returns true if changed.
pub fn time_edit(ui: &mut Ui, value: &mut ShowTime) -> bool {
    let mut secs = value.as_secs_f64();
    let r = ui.add(DragValue::new(&mut secs).speed(0.05).range(0.0..=36_000.0).max_decimals(2).suffix(" s"));
    if r.changed() {
        *value = ShowTime::from_secs_f64(secs);
    }
    r.changed()
}

pub fn opt_time_edit(ui: &mut Ui, value: &mut Option<ShowTime>, none_label: &str) -> bool {
    ui.horizontal(|ui| opt_time_edit_inner(ui, value, none_label)).inner
}

fn opt_time_edit_inner(ui: &mut Ui, value: &mut Option<ShowTime>, none_label: &str) -> bool {
    let mut on = value.is_some();
    let mut changed = toggle(ui, &mut on, "").changed();
    if changed {
        *value = on.then_some(value.unwrap_or(ShowTime::from_secs(10)));
    }
    match value {
        Some(v) => changed |= time_edit(ui, v),
        None => {
            ui.weak(none_label);
        }
    }
    changed
}

pub fn fmt_dur(t: Option<ShowTime>) -> String {
    match t {
        Some(t) => t.to_clock_string()[3..].to_string(),
        None => "∞".into(),
    }
}

pub fn color_edit(ui: &mut Ui, rgba: &mut [f32; 4]) -> bool {
    let mut c = egui::Rgba::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3]);
    let changed = egui::color_picker::color_edit_button_rgba(ui, &mut c, egui::color_picker::Alpha::OnlyBlend).changed();
    if changed {
        *rgba = c.to_rgba_unmultiplied();
    }
    changed
}

/// Minimum interval between seeks emitted while dragging a scrub bar.
const SCRUB_INTERVAL: f64 = 0.04;

/// Playhead scrubber for a running cue: shows progress and returns a new
/// position while the operator clicks or drags (throttled to ~25 seeks/s,
/// plus the exact position on release).
pub fn scrub_bar(ui: &mut Ui, id_salt: impl egui::AsIdSalt, elapsed: ShowTime, duration: ShowTime, height: f32, accent: egui::Color32) -> Option<ShowTime> {
    use crate::theme;
    let width = ui.available_width().max(60.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click_and_drag());
    let id = ui.id().with(id_salt);
    let dur = duration.as_secs_f64().max(0.001);
    let to_time = |x: f32| ShowTime::from_secs_f64((((x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64) * dur);
    let dragging = resp.dragged() || resp.is_pointer_button_down_on();
    // While dragging, the bar follows the pointer rather than the (lagging) engine clock.
    let shown = match (dragging, resp.interact_pointer_pos()) {
        (true, Some(p)) => to_time(p.x),
        _ => elapsed,
    };
    let frac = (shown.as_secs_f64() / dur).clamp(0.0, 1.0) as f32;
    let p = ui.painter_at(rect.expand(2.0));
    let track = egui::Rect::from_center_size(rect.center(), egui::vec2(rect.width(), (height * 0.42).max(4.0)));
    p.rect_filled(track, 3.0, theme::recessed());
    p.rect_stroke(track, 3.0, egui::Stroke::new(1.0_f32, theme::border()), egui::StrokeKind::Inside);
    let filled = egui::Rect::from_min_max(track.min, egui::pos2(track.left() + track.width() * frac, track.bottom()));
    p.rect_filled(filled, 3.0, accent.gamma_multiply(if resp.hovered() || dragging { 0.85 } else { 0.6 }));
    let x = track.left() + track.width() * frac;
    let knob = if resp.hovered() || dragging { height * 0.42 } else { height * 0.32 };
    p.circle_filled(egui::pos2(x, rect.center().y), knob, theme::text());
    if resp.hovered() && !dragging {
        if let Some(h) = resp.hover_pos() {
            p.line_segment([egui::pos2(h.x, track.top() - 2.0), egui::pos2(h.x, track.bottom() + 2.0)], egui::Stroke::new(1.0_f32, theme::text_dim()));
        }
    }
    let hover_t = resp.hover_pos().map(|h| to_time(h.x));
    let resp = resp.on_hover_text_at_pointer(hover_t.map_or(String::new(), |t| format!("Seek to {}", t.to_clock_string())));
    let pos = resp.interact_pointer_pos().map(|p| to_time(p.x))?;
    let now = ui.input(|i| i.time);
    let last: f64 = ui.data(|d| d.get_temp(id)).unwrap_or(f64::NEG_INFINITY);
    let released = resp.drag_stopped() || resp.clicked();
    if released || now - last >= SCRUB_INTERVAL {
        ui.data_mut(|d| d.insert_temp(id, now));
        return Some(pos);
    }
    None
}

/// A macOS-style menu row: full-width highlight, label on the left and a
/// dimmed keyboard shortcut on the right. Closes the menu when clicked.
pub fn menu_item(ui: &mut Ui, label: &str, shortcut: Option<&str>) -> egui::Response {
    menu_row(ui, label, shortcut, true, false)
}

/// A destructive action (drawn in the stop colour).
pub fn menu_item_danger(ui: &mut Ui, label: &str, shortcut: Option<&str>) -> egui::Response {
    menu_row(ui, label, shortcut, true, true)
}

pub fn menu_item_enabled(ui: &mut Ui, enabled: bool, label: &str, shortcut: Option<&str>) -> egui::Response {
    menu_row(ui, label, shortcut, enabled, false)
}

fn menu_row(ui: &mut Ui, label: &str, shortcut: Option<&str>, enabled: bool, danger: bool) -> egui::Response {
    use crate::theme;
    let font = egui::FontId::proportional(13.5);
    let label_w = ui.fonts_mut(|f| f.layout_no_wrap(label.to_string(), font.clone(), theme::text()).size().x);
    let short_w = shortcut.map_or(0.0, |s| ui.fonts_mut(|f| f.layout_no_wrap(s.to_string(), font.clone(), theme::text()).size().x) + 28.0);
    let width = ui.available_width().max(label_w + short_w + 24.0).max(190.0);
    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 24.0), sense);
    let hot = enabled && (resp.hovered() || resp.has_focus());
    let p = ui.painter();
    if hot {
        p.rect_filled(rect, theme::RADIUS as f32, if danger { theme::stop().gamma_multiply(0.85) } else { theme::accent_menu() });
    }
    let color = match (enabled, hot, danger) {
        (false, _, _) => theme::text_dim().gamma_multiply(0.6),
        (true, true, _) => egui::Color32::WHITE,
        (true, false, true) => theme::stop(),
        (true, false, false) => theme::text(),
    };
    p.text(rect.left_center() + egui::vec2(10.0, 0.0), egui::Align2::LEFT_CENTER, label, font.clone(), color);
    if let Some(s) = shortcut {
        let c = if hot { egui::Color32::from_white_alpha(200) } else { theme::text_dim() };
        p.text(rect.right_center() - egui::vec2(10.0, 0.0), egui::Align2::RIGHT_CENTER, s, font, c);
    }
    if resp.clicked() {
        ui.close();
    }
    resp
}

/// Thin separator line between menu groups.
pub fn menu_separator(ui: &mut Ui) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 9.0), egui::Sense::hover());
    ui.painter().hline(rect.x_range().shrink(6.0), rect.center().y, egui::Stroke::new(1.0_f32, crate::theme::border()));
}

/// Small dimmed section heading inside a menu.
pub fn menu_heading(ui: &mut Ui, text: &str) {
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.add_space(10.0);
        ui.label(egui::RichText::new(text).size(11.5).color(crate::theme::text_dim()));
    });
}

/// Platform shortcut label: "⌘S" style on macOS, "Ctrl+S" elsewhere.
pub fn shortcut(keys: &str) -> String {
    if cfg!(target_os = "macos") {
        keys.replace("Cmd+Shift+", "⇧⌘").replace("Cmd+", "⌘").replace("Shift+", "⇧")
    } else {
        keys.replace("Cmd+", "Ctrl+")
    }
}

/// `HH:MM:SS:FF` editor. The text is committed (parsed at `rate`) when the
/// field loses focus or Enter is pressed; invalid input is outlined and
/// discarded. Returns true when `tc` changed.
pub fn timecode_field(ui: &mut Ui, id: impl egui::AsIdSalt, tc: &mut livestage_core::Timecode, rate: livestage_core::FrameRate) -> bool {
    use livestage_core::Timecode;
    let id = ui.make_persistent_id(id);
    let mut text = ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_else(|| tc.with_rate(rate).to_string());
    let parsed = Timecode::parse_with(&text, rate).map(|t| t.with_rate(rate));
    let mut edit = egui::TextEdit::singleline(&mut text).font(crate::theme::clock_font(14.0)).desired_width(104.0).hint_text("HH:MM:SS:FF");
    if parsed.is_err() {
        edit = edit.text_color(crate::theme::stop());
    }
    let r = ui.add(edit);
    let mut changed = false;
    if r.lost_focus() {
        if let Ok(t) = Timecode::parse_with(&text, rate).map(|t| t.with_rate(rate)) {
            changed = t != *tc;
            *tc = t;
        }
        ui.data_mut(|d| d.remove::<String>(id));
    } else if r.has_focus() {
        ui.data_mut(|d| d.insert_temp(id, text));
    }
    if parsed.is_err() && r.has_focus() {
        r.on_hover_text(format!("Timecode as HH:MM:SS:FF at {rate}"));
    }
    changed
}

// ================================================================ the kit ==

use crate::theme;
use egui::{Align2, Color32, FontId, Pos2, Rect, Response, RichText, Sense, Stroke, StrokeKind, Vec2, WidgetText};

/// Uppercase, letter-spaced telemetry caption text.
pub fn caption_text(text: &str) -> RichText {
    RichText::new(text.to_uppercase()).size(10.5).extra_letter_spacing(1.1).color(theme::text_dim())
}

/// Uppercase, letter-spaced telemetry caption.
pub fn caption(ui: &mut Ui, text: &str) -> Response {
    ui.label(caption_text(text))
}

/// Native-style switch (replaces checkboxes): accent track when on, white
/// knob, label part of the hit area, focus ring for keyboard use.
pub fn toggle(ui: &mut Ui, on: &mut bool, label: impl Into<WidgetText>) -> Response {
    let label: WidgetText = label.into();
    let track = Vec2::new(34.0, 20.0);
    let galley = (!label.is_empty()).then(|| label.into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Body));
    let gap = if galley.is_some() { theme::GRID } else { 0.0 };
    let width = track.x + gap + galley.as_ref().map_or(0.0, |g| g.size().x);
    let height = ui.spacing().interact_size.y.max(track.y);
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *on, galley.as_ref().map_or("", |g| g.text())));
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_with_time(resp.id, *on, 0.12);
        let tr = Rect::from_min_size(Pos2::new(rect.left(), rect.center().y - track.y / 2.0), track);
        let p = ui.painter();
        let enabled = ui.is_enabled();
        let off_fill = if theme::is_light() { theme::control_active() } else { theme::control() };
        let fill = off_fill.lerp_to_gamma(theme::accent(), t);
        let fill = if enabled { fill } else { fill.gamma_multiply(0.5) };
        p.rect(tr, track.y / 2.0, fill, Stroke::new(theme::HAIRLINE, theme::border().lerp_to_gamma(theme::accent(), t)), StrokeKind::Inside);
        let x = egui::lerp(tr.left() + track.y / 2.0..=tr.right() - track.y / 2.0, t);
        let knob = Pos2::new(x, tr.center().y);
        p.circle_filled(knob + Vec2::new(0.0, 0.5), 7.5, Color32::from_black_alpha(40));
        p.circle_filled(knob, 7.5, if enabled { Color32::WHITE } else { Color32::from_gray(200) });
        if resp.has_focus() {
            p.rect_stroke(tr.expand(2.0), track.y / 2.0 + 2.0, Stroke::new(theme::FOCUS_RING, theme::accent()), StrokeKind::Outside);
        }
        if let Some(g) = galley {
            let color = if enabled { theme::text() } else { theme::text_faint() };
            p.galley(Pos2::new(tr.right() + gap, rect.center().y - g.size().y / 2.0), g, color);
        }
    }
    resp
}

/// What a key looks like.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum KeyTone {
    /// Lit green (GO).
    Go,
    /// Lit red (PANIC, outputs live).
    Danger,
    /// Neutral face, red legend and edge (STOP ALL).
    Warn,
    /// Neutral face.
    Neutral,
}

/// A large tactile key: a flat face over a 2 px base edge that it sinks
/// into while pressed, an optional status bar (`lit`), and a small legend
/// (keyboard shortcut) in the corner. Drawn in `rect`.
pub fn key(ui: &mut Ui, rect: Rect, label: &str, size: f32, legend: Option<&str>, tone: KeyTone, lit: bool) -> Response {
    let resp = ui.allocate_rect(rect, Sense::click());
    let pressed = resp.is_pointer_button_down_on();
    let hovered = resp.hovered();
    let enabled = ui.is_enabled();
    let depth = 2.0;
    let face = if pressed { Rect::from_min_max(rect.min + Vec2::new(0.0, depth), rect.max) } else { Rect::from_min_max(rect.min, rect.max - Vec2::new(0.0, depth)) };
    let (fill, ink, edge) = match tone {
        KeyTone::Go => (theme::go(), theme::on(theme::go()), theme::go()),
        KeyTone::Danger => (theme::stop(), Color32::WHITE, theme::stop()),
        KeyTone::Warn => (theme::control(), theme::stop_text(), theme::stop().gamma_multiply(0.8)),
        KeyTone::Neutral => (theme::control(), theme::text(), theme::border_hi()),
    };
    let fill = if !enabled {
        fill.gamma_multiply(0.45)
    } else if hovered && !pressed {
        fill.lerp_to_gamma(if theme::is_light() { Color32::BLACK } else { Color32::WHITE }, 0.06)
    } else {
        fill
    };
    let radius = 6.0;
    let p = ui.painter();
    if !pressed {
        p.rect_filled(Rect::from_min_max(Pos2::new(rect.left(), rect.top() + depth), rect.max), radius, fill.lerp_to_gamma(Color32::BLACK, 0.35));
    }
    p.rect(face, radius, fill, Stroke::new(theme::HAIRLINE, edge.gamma_multiply(if hovered { 1.0 } else { 0.75 })), StrokeKind::Inside);
    if lit {
        let bar = Rect::from_min_size(face.min + Vec2::new(12.0, 7.0), Vec2::new(face.width() - 24.0, 3.0));
        p.rect_filled(bar, 1.5, if tone == KeyTone::Go { ink.gamma_multiply(0.55) } else { Color32::WHITE });
    }
    if resp.has_focus() {
        p.rect_stroke(rect.expand(2.0), radius + 2.0, Stroke::new(theme::FOCUS_RING, theme::accent()), StrokeKind::Outside);
    }
    let lines: Vec<&str> = label.split('\n').collect();
    let line_h = size * 1.05;
    let top = face.center().y - line_h * (lines.len() as f32 - 1.0) / 2.0 - if legend.is_some() { 4.0 } else { 0.0 };
    for (i, l) in lines.iter().enumerate() {
        p.text(Pos2::new(face.center().x, top + i as f32 * line_h), Align2::CENTER_CENTER, *l, FontId::proportional(size), ink);
    }
    if let Some(l) = legend {
        p.text(face.left_bottom() + Vec2::new(10.0, -8.0), Align2::LEFT_BOTTOM, l, FontId::monospace(9.5), ink.gamma_multiply(0.7));
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Uppercase with thin spaces between letters (for painter text, which has no letter spacing).
fn spaced(text: &str) -> String {
    let up = text.to_uppercase();
    let mut out = String::with_capacity(up.len() * 2);
    for (i, ch) in up.chars().enumerate() {
        if i > 0 && ch != ' ' {
            out.push('\u{200A}');
        }
        out.push(ch);
    }
    out
}

/// Status pill: a dot and an uppercase label on a tint of `color`,
/// vertically centred on `left_center`. Returns its rect.
pub fn status_pill(p: &egui::Painter, left_center: Pos2, text: &str, color: Color32) -> Rect {
    let g = p.layout_no_wrap(spaced(text), FontId::proportional(9.5), color);
    let rect = Rect::from_min_size(Pos2::new(left_center.x, left_center.y - 9.0), Vec2::new(g.size().x + 22.0, 18.0));
    p.rect(rect, 9.0, theme::wash(color, 0.14), Stroke::new(theme::HAIRLINE, theme::wash(color, 0.5)), StrokeKind::Inside);
    p.circle_filled(Pos2::new(rect.left() + 8.0, rect.center().y), 3.0, color);
    p.galley(Pos2::new(rect.left() + 14.0, rect.center().y - g.size().y / 2.0), g, color);
    rect
}

/// Status badge (cue state): outlined pill on a tint, at `center_left`.
pub fn pill(p: &egui::Painter, center_left: Pos2, text: &str, color: Color32) -> Rect {
    let g = p.layout_no_wrap(spaced(text), FontId::proportional(9.5), color);
    let rect = Rect::from_min_size(Pos2::new(center_left.x, center_left.y - 9.0), Vec2::new(g.size().x + 14.0, 18.0));
    p.rect(rect, 9.0, theme::wash(color, 0.12), Stroke::new(theme::HAIRLINE, color.gamma_multiply(0.75)), StrokeKind::Inside);
    p.galley(Pos2::new(rect.left() + 7.0, rect.center().y - g.size().y / 2.0), g, color);
    rect
}

/// Category badge: a solid 4 px-radius box in the category colour with
/// the most readable ink, at `left_center`. Returns its rect.
pub fn badge(p: &egui::Painter, left_center: Pos2, text: &str, color: Color32) -> Rect {
    let ink = theme::on(color);
    let g = p.layout_no_wrap(spaced(text), FontId::proportional(9.5), ink);
    let rect = Rect::from_min_size(Pos2::new(left_center.x, left_center.y - 9.0), Vec2::new(g.size().x + 12.0, 18.0));
    p.rect_filled(rect, theme::RADIUS as f32, color);
    p.galley(Pos2::new(rect.left() + 6.0, rect.center().y - g.size().y / 2.0), g, ink);
    rect
}

pub enum EncoderEvent {
    None,
    Set(f32),
    Release,
}

/// Rotary encoder with a graduated bezel: drag up / down or scroll, Shift
/// for fine, double-click releases. `active` lights the value arc.
pub fn encoder(ui: &mut Ui, label: &str, value: f32, active: bool) -> EncoderEvent {
    use std::f32::consts::PI;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(78.0, 100.0), Sense::click_and_drag());
    let p = ui.painter_at(rect);
    let c = Pos2::new(rect.center().x, rect.top() + 36.0);
    let r = 28.0;
    let start = 0.75 * PI;
    let sweep = 1.5 * PI;
    let v = value.clamp(0.0, 1.0);
    let at = |a: f32, rad: f32| c + Vec2::new(a.cos(), a.sin()) * rad;
    // Graduations: 11 ticks, majors at 0 / 50 / 100 %.
    for i in 0..=10 {
        let a = start + sweep * i as f32 / 10.0;
        let major = i % 5 == 0;
        let lit = active && (i as f32 / 10.0) <= v + 1e-4;
        let col = if lit { theme::accent() } else if major { theme::text_dim() } else { theme::text_faint() };
        p.line_segment([at(a, r + 2.0), at(a, r + if major { 7.0 } else { 5.0 })], Stroke::new(1.0_f32, col));
    }
    let arc = |from: f32, to: f32, rad: f32, stroke: Stroke| {
        let n = (((to - from) / sweep) * 56.0).ceil().max(2.0) as usize;
        let pts: Vec<Pos2> = (0..=n).map(|i| at(from + (to - from) * i as f32 / n as f32, rad)).collect();
        p.add(egui::Shape::line(pts, stroke));
    };
    arc(start, start + sweep, r - 2.0, Stroke::new(3.0_f32, theme::bg_deep()));
    let color = if active { theme::accent() } else { theme::text_dim() };
    if v > 0.0 {
        arc(start, start + sweep * v, r - 2.0, Stroke::new(3.0_f32, color));
    }
    // Knob body with an indicator line.
    let hovered = resp.hovered() || resp.dragged();
    p.circle(c, r - 8.0, if hovered { theme::control_hover() } else { theme::control() }, Stroke::new(1.0_f32, if hovered { theme::border_hi() } else { theme::border() }));
    let a = start + sweep * v;
    p.line_segment([at(a, 6.0), at(a, r - 11.0)], Stroke::new(2.0_f32, if active { theme::text() } else { theme::text_dim() }));
    p.circle_filled(c, 2.0, theme::bg_deep());
    p.text(Pos2::new(c.x, rect.top() + 74.0), Align2::CENTER_CENTER, format!("{:>3.0}", v * 100.0), FontId::monospace(13.0), if active { theme::text() } else { theme::text_dim() });
    p.text(Pos2::new(c.x, rect.bottom() - 4.0), Align2::CENTER_BOTTOM, spaced(label), FontId::proportional(9.5), theme::text_dim());
    let resp = resp.on_hover_text("Drag or scroll · Shift = fine · double-click releases");
    if resp.double_clicked() {
        return EncoderEvent::Release;
    }
    let scale = if ui.input(|i| i.modifiers.shift) { 0.1 } else { 1.0 };
    if resp.dragged() {
        let d = resp.drag_delta();
        let delta = (d.x - d.y) / 160.0 * scale;
        if delta != 0.0 {
            return EncoderEvent::Set((value + delta).clamp(0.0, 1.0));
        }
    }
    if resp.hovered() {
        let s = ui.input(|i| i.smooth_scroll_delta.y);
        if s != 0.0 {
            return EncoderEvent::Set((value + s / 500.0 * scale).clamp(0.0, 1.0));
        }
    }
    EncoderEvent::None
}

/// Heat colour of a DMX level: dark (0) through the active green to white
/// (full), so the busy channels of a universe read at a glance.
pub fn dmx_heat(v: u8) -> Color32 {
    let t = v as f32 / 255.0;
    let off = theme::wash(theme::text_faint(), 0.15);
    if v == 0 {
        off
    } else if t < 0.6 {
        off.lerp_to_gamma(theme::active(), 0.35 + t / 0.6 * 0.65)
    } else {
        theme::active().lerp_to_gamma(if theme::is_light() { Color32::from_rgb(0x00, 0x40, 0x2A) } else { Color32::WHITE }, (t - 0.6) / 0.4 * 0.8)
    }
}

/// A universe's 512 channels as a 32 × 16 heatmap, `width` points wide.
/// Hover shows the channel and value; values are printed when cells are
/// large enough.
pub fn dmx_heatmap(ui: &mut Ui, data: &[u8; 512], width: f32) -> Response {
    let cols = 32usize;
    let cell = (width / cols as f32).floor().max(4.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(cell * cols as f32, cell * 16.0), Sense::hover());
    let p = ui.painter_at(rect);
    let gap = if cell >= 8.0 { 1.0 } else { 0.0 };
    for (i, &v) in data.iter().enumerate() {
        let (x, y) = ((i % cols) as f32, (i / cols) as f32);
        let r = Rect::from_min_size(rect.min + Vec2::new(x * cell, y * cell), Vec2::new(cell - gap, cell - gap));
        let fill = dmx_heat(v);
        p.rect_filled(r, if cell >= 8.0 { 1.5 } else { 0.0 }, fill);
        if cell >= 20.0 && v > 0 {
            p.text(r.center(), Align2::CENTER_CENTER, v.to_string(), FontId::monospace(8.0), theme::on(fill));
        }
    }
    if let Some(h) = resp.hover_pos() {
        let (x, y) = (((h.x - rect.left()) / cell) as usize, ((h.y - rect.top()) / cell) as usize);
        let i = (y * cols + x).min(511);
        let r = Rect::from_min_size(rect.min + Vec2::new((i % cols) as f32 * cell, (i / cols) as f32 * cell), Vec2::splat(cell - gap));
        p.rect_stroke(r, 1.5, Stroke::new(1.5, theme::accent()), StrokeKind::Outside);
        return resp.on_hover_text_at_pointer(format!("Channel {} = {} ({:.0} %)", i + 1, data[i], data[i] as f32 / 2.55));
    }
    resp
}

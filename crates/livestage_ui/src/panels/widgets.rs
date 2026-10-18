//! Small shared widgets.

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
    let mut changed = ui.checkbox(&mut on, "").changed();
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
pub fn scrub_bar(ui: &mut Ui, id_salt: impl std::hash::Hash, elapsed: ShowTime, duration: ShowTime, height: f32, accent: egui::Color32) -> Option<ShowTime> {
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
    p.rect_filled(track, 3.0, theme::PANEL_RECESSED);
    p.rect_stroke(track, 3.0, egui::Stroke::new(1.0_f32, theme::BORDER), egui::StrokeKind::Inside);
    let filled = egui::Rect::from_min_max(track.min, egui::pos2(track.left() + track.width() * frac, track.bottom()));
    p.rect_filled(filled, 3.0, accent.gamma_multiply(if resp.hovered() || dragging { 0.85 } else { 0.6 }));
    let x = track.left() + track.width() * frac;
    let knob = if resp.hovered() || dragging { height * 0.42 } else { height * 0.32 };
    p.circle_filled(egui::pos2(x, rect.center().y), knob, theme::TEXT);
    if resp.hovered() && !dragging {
        if let Some(h) = resp.hover_pos() {
            p.line_segment([egui::pos2(h.x, track.top() - 2.0), egui::pos2(h.x, track.bottom() + 2.0)], egui::Stroke::new(1.0_f32, theme::TEXT_DIM));
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
    let label_w = ui.fonts(|f| f.layout_no_wrap(label.to_string(), font.clone(), theme::TEXT).size().x);
    let short_w = shortcut.map_or(0.0, |s| ui.fonts(|f| f.layout_no_wrap(s.to_string(), font.clone(), theme::TEXT).size().x) + 28.0);
    let width = ui.available_width().max(label_w + short_w + 24.0).max(190.0);
    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 24.0), sense);
    let hot = enabled && (resp.hovered() || resp.has_focus());
    let p = ui.painter();
    if hot {
        p.rect_filled(rect, 5.0, if danger { theme::stop().gamma_multiply(0.85) } else { theme::accent_menu() });
    }
    let color = match (enabled, hot, danger) {
        (false, _, _) => theme::TEXT_DIM.gamma_multiply(0.6),
        (true, true, _) => egui::Color32::WHITE,
        (true, false, true) => theme::stop(),
        (true, false, false) => theme::TEXT,
    };
    p.text(rect.left_center() + egui::vec2(10.0, 0.0), egui::Align2::LEFT_CENTER, label, font.clone(), color);
    if let Some(s) = shortcut {
        let c = if hot { egui::Color32::from_white_alpha(200) } else { theme::TEXT_DIM };
        p.text(rect.right_center() - egui::vec2(10.0, 0.0), egui::Align2::RIGHT_CENTER, s, font, c);
    }
    if resp.clicked() {
        ui.close_menu();
    }
    resp
}

/// Thin separator line between menu groups.
pub fn menu_separator(ui: &mut Ui) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 9.0), egui::Sense::hover());
    ui.painter().hline(rect.x_range().shrink(6.0), rect.center().y, egui::Stroke::new(1.0_f32, crate::theme::MENU_SEPARATOR));
}

/// Small dimmed section heading inside a menu.
pub fn menu_heading(ui: &mut Ui, text: &str) {
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.add_space(10.0);
        ui.label(egui::RichText::new(text).size(11.5).color(crate::theme::TEXT_DIM));
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

//! Inputs beyond egui's built-ins: the segmented control and the dB
//! fader. Numeric fields, sliders and dropdowns are egui's own, given one
//! size, radius, focus ring and colour set by [`crate::theme`].

use crate::theme;
use egui::{Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2};

/// Segmented control (macOS segmented control / Fluent pivot). Returns
/// true when the selection changed.
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, current: &mut T, options: &[(T, &str)]) -> bool {
    let font = egui::FontId::proportional(theme::type_scale::BODY);
    let widths: Vec<f32> = options.iter().map(|(_, l)| ui.fonts_mut(|f| f.layout_no_wrap(l.to_string(), font.clone(), theme::text()).size().x) + 2.0 * theme::GRID + 4.0).collect();
    let total: f32 = widths.iter().sum::<f32>() + 4.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(total, theme::CONTROL_HEIGHT - 2.0), Sense::hover());
    let thumb_id = ui.id().with(("seg-thumb", options.len()));
    let p = ui.painter().clone();
    p.rect(rect, theme::RADIUS as f32 + 2.0, theme::recessed(), theme::hairline(), StrokeKind::Inside);
    // Segment rects first: the thumb is drawn under the labels.
    let mut segs = Vec::with_capacity(options.len());
    let mut x = rect.left() + 2.0;
    for w in &widths {
        segs.push(Rect::from_min_size(Pos2::new(x, rect.top() + 2.0), Vec2::new(*w, rect.height() - 4.0)));
        x += w;
    }
    let mut changed = false;
    let mut labels = Vec::with_capacity(options.len());
    for (((value, label), r), i) in options.iter().zip(&segs).zip(0..) {
        let resp = ui.interact(*r, ui.id().with(("seg", label)), Sense::click());
        let on = *current == *value;
        // Pointer-down shows at once; the choice commits on release.
        if !on && resp.is_pointer_button_down_on() {
            p.rect_filled(*r, theme::RADIUS as f32, theme::control_active());
        } else if !on && resp.hovered() {
            p.rect_filled(*r, theme::RADIUS as f32, theme::control_hover());
        }
        if resp.clicked() && !on {
            *current = *value;
            changed = true;
        }
        labels.push((i, *r, *label, resp.has_focus()));
    }
    // The thumb slides (springs) to the selected segment, from wherever it is.
    let sel = options.iter().position(|(v, _)| *v == *current);
    if let Some(i) = sel {
        let ctx = ui.ctx();
        let target = segs[i];
        let left = crate::motion::value(ctx, thumb_id.with("x"), target.left(), crate::motion::Spring::SMOOTH);
        let width = crate::motion::value(ctx, thumb_id.with("w"), target.width(), crate::motion::Spring::SMOOTH);
        let thumb = Rect::from_min_size(Pos2::new(left, target.top()), Vec2::new(width, target.height()));
        p.rect(thumb, theme::RADIUS as f32, theme::control_active().lerp_to_gamma(theme::raised(), 0.2), Stroke::new(theme::HAIRLINE, theme::border_hi()), StrokeKind::Inside);
    }
    for (i, r, label, focus) in labels {
        if focus {
            p.rect_stroke(r, theme::RADIUS as f32, Stroke::new(theme::FOCUS_RING, theme::accent()), StrokeKind::Inside);
        }
        let on = sel == Some(i);
        p.text(r.center(), egui::Align2::CENTER_CENTER, label, font.clone(), if on { theme::text() } else { theme::text_dim() });
    }
    changed
}

/// Vertical fader in dB, with a 0 dB detent mark. Double-click resets to
/// 0 dB; Shift for fine. Returns the response (changed when moved).
pub fn fader(ui: &mut Ui, db: &mut f32, range: std::ops::RangeInclusive<f32>, size: Vec2) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let (lo, hi) = (*range.start(), *range.end());
    // Audio taper: the top 40 dB take most of the travel.
    let to_frac = |d: f32| {
        let d = d.clamp(lo, hi);
        let x = (d - lo) / (hi - lo);
        x.powf(0.6)
    };
    let from_frac = |f: f32| lo + (hi - lo) * f.clamp(0.0, 1.0).powf(1.0 / 0.6);
    let track = Rect::from_center_size(rect.center(), Vec2::new(4.0, rect.height() - 20.0));
    let y_of = |d: f32| track.bottom() - track.height() * to_frac(d);
    if resp.double_clicked() {
        *db = 0.0f32.clamp(lo, hi);
        resp.mark_changed();
    } else if resp.dragged() {
        let fine = if ui.input(|i| i.modifiers.shift) { 0.2 } else { 1.0 };
        let f = to_frac(*db) - resp.drag_delta().y / track.height() * fine;
        let new = (from_frac(f) * 10.0).round() / 10.0;
        if new != *db {
            *db = new;
            resp.mark_changed();
        }
    }
    let p = ui.painter();
    p.rect_filled(track, 2.0, theme::bg_deep());
    let zero = y_of(0.0);
    p.line_segment([Pos2::new(track.left() - 7.0, zero), Pos2::new(track.right() + 7.0, zero)], Stroke::new(1.0, theme::border_hi()));
    let cap = Rect::from_center_size(Pos2::new(rect.center().x, y_of(*db)), Vec2::new(rect.width().min(30.0), 14.0));
    let hot = resp.hovered() || resp.dragged();
    p.rect(cap, 3.0, if hot { theme::control_hover() } else { theme::control() }, Stroke::new(theme::HAIRLINE, if hot { theme::accent() } else { theme::border_hi() }), StrokeKind::Inside);
    p.hline(cap.x_range().shrink(4.0), cap.center().y, Stroke::new(1.0, theme::text()));
    resp.on_hover_text(format!("{:+.1} dB · drag, Shift = fine, double-click = 0 dB", *db))
}


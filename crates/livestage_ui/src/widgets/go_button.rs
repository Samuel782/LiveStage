//! The GO button: the largest, brightest control on screen. Lit green
//! with the standby cue under the legend while something is armed; a
//! dark, quiet face when the list has nothing on standby.

use crate::theme;
use egui::{Align2, Color32, FontId, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2};

/// The cue the next GO fires.
pub struct Standby<'a> {
    pub number: &'a str,
    pub name: &'a str,
}

/// Draws the GO button in `rect`. Click (or Space, handled by the caller's
/// shortcuts) fires the standby cue.
pub fn go_button(ui: &mut Ui, rect: Rect, standby: Option<Standby<'_>>) -> Response {
    let resp = ui.allocate_rect(rect, Sense::click());
    let pressed = resp.is_pointer_button_down_on();
    let armed = standby.is_some();
    let depth = 3.0;
    let face = if pressed { Rect::from_min_max(rect.min + Vec2::new(0.0, depth), rect.max) } else { Rect::from_min_max(rect.min, rect.max - Vec2::new(0.0, depth)) };
    let fill = if armed { theme::go() } else { theme::control() };
    let fill = if resp.hovered() && !pressed { fill.lerp_to_gamma(if armed { Color32::WHITE } else { theme::text() }, 0.07) } else { fill };
    let ink = if armed { theme::on(fill) } else { theme::text_faint() };
    let radius = 8.0;
    let p = ui.painter();
    if !pressed {
        p.rect_filled(Rect::from_min_max(Pos2::new(rect.left(), rect.top() + depth), rect.max), radius, fill.lerp_to_gamma(Color32::BLACK, 0.35));
    }
    let edge = if armed { fill.lerp_to_gamma(Color32::WHITE, 0.25) } else { theme::border_hi() };
    p.rect(face, radius, fill, Stroke::new(theme::HAIRLINE, edge), StrokeKind::Inside);
    if resp.has_focus() {
        p.rect_stroke(rect.expand(2.0), radius + 2.0, Stroke::new(theme::FOCUS_RING, theme::accent()), StrokeKind::Outside);
    }
    let legend_size = (face.height() * 0.38).clamp(22.0, 44.0);
    let has_line = face.height() > 70.0;
    let go_y = if has_line { face.top() + face.height() * 0.40 } else { face.center().y };
    p.text(Pos2::new(face.center().x, go_y), Align2::CENTER_CENTER, "GO", FontId::proportional(legend_size), ink);
    if has_line {
        let line = match &standby {
            Some(s) => format!("{}  {}", s.number, s.name),
            None => "Nothing on standby".into(),
        };
        let max_w = face.width() - 20.0;
        let font = FontId::proportional(12.0);
        let mut job = egui::text::LayoutJob::simple_singleline(line, font, ink.gamma_multiply(0.85));
        job.wrap = egui::text::TextWrapping::truncate_at_width(max_w);
        let g = ui.fonts_mut(|f| f.layout_job(job));
        p.galley(Pos2::new(face.center().x - g.size().x / 2.0, face.top() + face.height() * 0.70 - g.size().y / 2.0), g, ink);
    }
    p.text(face.right_top() + Vec2::new(-8.0, 7.0), Align2::RIGHT_TOP, "SPACE", FontId::monospace(9.0), ink.gamma_multiply(0.6));
    let hint = match &standby {
        Some(s) => format!("Fire cue {} — {} (Space)", s.number, s.name),
        None => "Nothing on standby".into(),
    };
    resp.on_hover_text(hint).on_hover_cursor(egui::CursorIcon::PointingHand)
}

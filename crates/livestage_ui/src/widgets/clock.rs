//! SMPTE digital clock: `HH:MM:SS:FF` in fixed-width figures, with pills
//! for the timecode source (`INTERNAL`, `MTC`, `LTC`, `ART-NET`) and the
//! lock / transport state.

use super::status_pill;
use crate::theme;
use egui::{Align2, Color32, Pos2, Response, Sense, Ui, Vec2};
use livestage_core::timecode::{LockState, TcTransport, TimecodeSource};

/// Source pill label.
pub fn source_label(s: TimecodeSource) -> &'static str {
    match s {
        TimecodeSource::Internal => "INTERNAL",
        TimecodeSource::Mtc => "MTC",
        TimecodeSource::Ltc => "LTC",
        TimecodeSource::ArtNet => "ART-NET",
    }
}

/// Transport / lock state as (label, colour).
pub fn state_of(t: &TcTransport, source: TimecodeSource) -> (&'static str, Color32) {
    if source == TimecodeSource::Internal {
        return if t.running { ("RUNNING", theme::active()) } else { ("STOPPED", theme::text_faint()) };
    }
    match t.state {
        LockState::Locked if t.running => ("LOCKED", theme::active()),
        LockState::Locked => ("READY", theme::active()),
        LockState::Freewheel => ("FREEWHEEL", theme::standby()),
        LockState::Locking => ("LOCKING", theme::standby()),
        LockState::Stopped => ("NO SIGNAL", theme::text_faint()),
    }
}

/// Draws the clock: digits `size` points tall, pills underneath. Returns
/// the digits' response (click to open timecode settings, for example).
pub fn smpte_clock(ui: &mut Ui, t: &TcTransport, source: TimecodeSource, size: f32) -> Response {
    let text = t.timecode().to_string();
    let (state, state_color) = state_of(t, source);
    let digits = if t.running { theme::text() } else { theme::text_dim() };
    let font = theme::clock_font(size);
    let g = ui.fonts_mut(|f| f.layout_no_wrap(text, font, digits));
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(g.size().x, g.size().y + 24.0), Sense::click());
    let p = ui.painter();
    p.galley(rect.min, g, digits);
    let y = rect.bottom() - 9.0;
    let src = status_pill(p, Pos2::new(rect.left(), y), source_label(source), theme::accent());
    status_pill(p, Pos2::new(src.right() + 6.0, y), state, state_color);
    let rate = t.rate.token();
    p.text(Pos2::new(rect.right(), y), Align2::RIGHT_CENTER, rate, egui::FontId::monospace(10.0), theme::text_faint());
    resp
}

//! Segmented audio meter with peak hold and a latching clip indicator.
//!
//! The audio engine reports per-channel sample peaks (read and reset each
//! UI frame). From those the meter keeps three levels: the instantaneous
//! peak, a VU-style average (300 ms integration of the peaks, the body of
//! the bar), and a peak hold that falls after 1.5 s. A peak at or above
//! −0.1 dBFS latches the clip light until it is clicked.

use crate::theme;
use egui::{Color32, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2};

/// Bottom of the scale.
pub const FLOOR_DB: f32 = -60.0;
/// Top of the scale (digital full scale).
pub const CEIL_DB: f32 = 0.0;
/// Above this the clip light latches.
pub const CLIP_DB: f32 = -0.1;
const HOLD_SECS: f64 = 1.5;
/// VU integration time.
const AVG_SECS: f32 = 0.3;
/// Release of the peak bar, dB per second.
const PEAK_FALL_DB_S: f32 = 24.0;

pub fn db(lin: f32) -> f32 {
    if lin <= 1e-6 {
        -120.0
    } else {
        20.0 * lin.log10()
    }
}

/// Ballistics of one channel.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MeterState {
    /// Peak bar level, dBFS.
    pub peak_db: f32,
    /// Averaged level, dBFS.
    pub avg_db: f32,
    pub hold_db: f32,
    hold_at: f64,
    /// Latched until reset.
    pub clipped: bool,
    started: bool,
}

impl MeterState {
    /// Feeds the peak (linear) measured since the last call, `dt` seconds ago.
    pub fn feed(&mut self, peak_lin: f32, now: f64, dt: f32) {
        let input = db(peak_lin).max(-120.0);
        if !self.started {
            *self = Self { peak_db: FLOOR_DB, avg_db: FLOOR_DB, hold_db: FLOOR_DB, started: true, ..*self };
        }
        let dt = dt.clamp(0.0, 0.5);
        // Instant attack, linear release.
        self.peak_db = input.max(self.peak_db - PEAK_FALL_DB_S * dt).max(FLOOR_DB);
        // One-pole average on the power of the peaks.
        let k = 1.0 - (-dt / AVG_SECS).exp();
        let p_avg = 10f32.powf(self.avg_db / 10.0);
        let p_in = 10f32.powf(input.max(FLOOR_DB) / 10.0);
        self.avg_db = (10.0 * (p_avg + (p_in - p_avg) * k).max(1e-12).log10()).max(FLOOR_DB);
        if input >= self.hold_db || now - self.hold_at > HOLD_SECS {
            self.hold_db = input.max(FLOOR_DB);
            self.hold_at = now;
        }
        if input >= CLIP_DB {
            self.clipped = true;
        }
    }

    pub fn reset_clip(&mut self) {
        self.clipped = false;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
    Vertical,
    Horizontal,
}

/// Segment colour for a level.
fn zone(db: f32) -> Color32 {
    if db > -6.0 {
        theme::stop()
    } else if db > -18.0 {
        theme::standby()
    } else {
        theme::active()
    }
}

/// Fraction of the scale for `db`.
pub fn frac(db: f32) -> f32 {
    ((db - FLOOR_DB) / (CEIL_DB - FLOOR_DB)).clamp(0.0, 1.0)
}

/// Draws a meter of `size` (thickness × length for vertical meters, length
/// × thickness for horizontal). `active = false` dims it (a channel the
/// device does not have). Clicking clears the clip light.
pub fn vu_meter(ui: &mut Ui, size: Vec2, state: &mut MeterState, orientation: Orientation, active: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        state.reset_clip();
    }
    let p = ui.painter_at(rect);
    p.rect(rect, 2.0, theme::bg_deep(), theme::hairline(), StrokeKind::Inside);
    let clip_len = 6.0;
    let inner = rect.shrink(2.0);
    // Track (the clip light takes the far end).
    let (track, clip) = match orientation {
        Orientation::Vertical => (
            Rect::from_min_max(Pos2::new(inner.left(), inner.top() + clip_len + 2.0), inner.max),
            Rect::from_min_size(inner.min, Vec2::new(inner.width(), clip_len)),
        ),
        Orientation::Horizontal => (
            Rect::from_min_max(inner.min, Pos2::new(inner.right() - clip_len - 2.0, inner.bottom())),
            Rect::from_min_max(Pos2::new(inner.right() - clip_len, inner.top()), inner.max),
        ),
    };
    let segments = 30;
    let len = match orientation {
        Orientation::Vertical => track.height(),
        Orientation::Horizontal => track.width(),
    };
    let seg = len / segments as f32;
    let off = theme::wash(theme::text_faint(), 0.18);
    for i in 0..segments {
        let lo = FLOOR_DB + (CEIL_DB - FLOOR_DB) * i as f32 / segments as f32;
        let hi = FLOOR_DB + (CEIL_DB - FLOOR_DB) * (i + 1) as f32 / segments as f32;
        let r = match orientation {
            Orientation::Vertical => Rect::from_min_max(
                Pos2::new(track.left(), track.bottom() - seg * (i + 1) as f32 + 0.5),
                Pos2::new(track.right(), track.bottom() - seg * i as f32 - 0.5),
            ),
            Orientation::Horizontal => Rect::from_min_max(
                Pos2::new(track.left() + seg * i as f32 + 0.5, track.top()),
                Pos2::new(track.left() + seg * (i + 1) as f32 - 0.5, track.bottom()),
            ),
        };
        let c = zone(hi);
        // Strictly above the segment's floor: silence lights nothing.
        let fill = if state.avg_db > lo {
            c
        } else if state.peak_db > lo {
            c.gamma_multiply(0.45)
        } else if state.hold_db > lo && state.hold_db <= hi {
            c
        } else {
            off
        };
        p.rect_filled(r, 1.0, if active { fill } else { fill.gamma_multiply(0.3) });
    }
    let clip_fill = if state.clipped { theme::stop() } else { off };
    p.rect_filled(clip, 1.0, clip_fill);
    if !active {
        p.rect_filled(rect, 2.0, theme::scrim());
    }
    let tip = if state.clipped {
        format!("Peak {:.1} dBFS · CLIPPED (click to clear)", state.hold_db)
    } else {
        format!("Peak {:.1} dBFS · average {:.1} dBFS", state.peak_db, state.avg_db)
    };
    resp.on_hover_text(tip)
}

/// dB scale (attenuation below full scale) beside a vertical meter of
/// `height`, in a 22-point column starting at `x`.
pub fn scale(p: &egui::Painter, top: f32, height: f32, x: f32) {
    for mark in [0.0, -6.0, -12.0, -18.0, -24.0, -36.0, -48.0, -60.0] {
        let y = top + 10.0 + (height - 12.0) * (1.0 - frac(mark));
        let label = if mark == 0.0 { "0".to_string() } else { format!("{}", mark.abs() as i32) };
        p.text(Pos2::new(x + 16.0, y), egui::Align2::RIGHT_CENTER, label, egui::FontId::monospace(9.0), theme::text_faint());
        p.line_segment([Pos2::new(x + 18.0, y), Pos2::new(x + 21.0, y)], Stroke::new(1.0, theme::border_hi()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ballistics_attack_hold_release_and_clip() {
        let mut m = MeterState::default();
        let dt = 1.0 / 60.0;
        let mut now = 0.0;
        // A -6 dBFS burst: the peak bar jumps, the average climbs.
        for _ in 0..30 {
            now += dt as f64;
            m.feed(0.5, now, dt);
        }
        assert!((m.peak_db - -6.02).abs() < 0.1);
        assert!(m.avg_db > -7.5 && m.avg_db <= -6.0, "average settles near the level: {}", m.avg_db);
        assert!(!m.clipped);
        // Silence: the peak bar falls at 24 dB/s, the hold stays for 1.5 s.
        for _ in 0..30 {
            now += dt as f64;
            m.feed(0.0, now, dt);
        }
        assert!((m.peak_db - (-6.02 - 12.0)).abs() < 0.6, "{}", m.peak_db);
        assert!((m.hold_db - -6.02).abs() < 0.1, "held");
        for _ in 0..90 {
            now += dt as f64;
            m.feed(0.0, now, dt);
        }
        assert_eq!(m.hold_db, FLOOR_DB, "hold released");
        // Full scale latches the clip light until cleared.
        m.feed(1.0, now, dt);
        m.feed(0.0, now + 1.0, dt);
        assert!(m.clipped);
        m.reset_clip();
        assert!(!m.clipped);
    }

    #[test]
    fn scale_maps_db_to_fraction() {
        assert_eq!(frac(0.0), 1.0);
        assert_eq!(frac(-60.0), 0.0);
        assert_eq!(frac(-30.0), 0.5);
        assert_eq!(frac(-90.0), 0.0);
    }
}

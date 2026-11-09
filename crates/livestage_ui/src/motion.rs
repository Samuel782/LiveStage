//! Motion: damped springs for every UI transition, after Apple's
//! *Designing Fluid Interfaces* (WWDC 2018).
//!
//! * **Springs, not durations.** A value moves towards its target under a
//!   spring described by Apple's two designer parameters: `damping` (1.0 =
//!   critically damped, no overshoot; below 1.0 overshoots) and `response`
//!   (seconds to get most of the way there; not a fixed duration).
//! * **Interruptible.** Changing the target mid-flight re-aims the spring
//!   from its current value *and velocity*, so a reversal never jumps or
//!   hits a "brick wall".
//! * **Instant response.** Feedback on pointer-down is never animated in:
//!   callers jump the value with [`snap`] and let the spring take it back.
//! * **Reduced motion.** With the OS (or Settings) asking for less motion,
//!   movement ([`value`]) lands at once; opacity changes ([`fade`]) stay as
//!   short cross-fades, because they help comprehension without motion.
//!
//! Spring state lives in egui's temporary memory, keyed by widget id.

use egui::{Context, Id};
use std::sync::atomic::{AtomicBool, Ordering};

/// A spring in Apple's terms.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    /// 1.0 = critically damped (no overshoot); lower = bouncier.
    pub damping: f32,
    /// Seconds to (mostly) reach the target; lower = snappier.
    pub response: f32,
}

impl Spring {
    /// House default: graceful, no overshoot (moves, highlights, sheets).
    pub const SMOOTH: Spring = Spring { damping: 1.0, response: 0.35 };
    /// Small controls answering a click (switch knobs, segment thumbs, hover).
    pub const SNAPPY: Spring = Spring { damping: 1.0, response: 0.22 };
    /// A key or pad coming back up after a press.
    pub const RELEASE: Spring = Spring { damping: 0.8, response: 0.25 };
    /// Cross-fades under reduced motion.
    const FADE: Spring = Spring { damping: 1.0, response: 0.15 };

    /// (stiffness, damping coefficient) for unit mass.
    fn coefficients(self) -> (f32, f32) {
        let omega = std::f32::consts::TAU / self.response.max(0.01);
        (omega * omega, 2.0 * self.damping * omega)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct State {
    x: f32,
    v: f32,
    time: f64,
}

/// Distance and speed (per second) under which a spring is at rest.
const REST: f32 = 1e-3;
/// Integration sub-step: stable for every response above 10 ms.
const SUBSTEP: f32 = 1.0 / 480.0;
/// Longest frame gap integrated; after a stall the spring resumes, not jumps.
const MAX_DT: f32 = 0.1;

/// Advances `x` / `v` towards `target` by `dt` seconds (semi-implicit Euler).
/// Returns true when the spring has come to rest (and snaps it there).
fn step(x: &mut f32, v: &mut f32, target: f32, spring: Spring, dt: f32) -> bool {
    let (k, c) = spring.coefficients();
    let mut left = dt.clamp(0.0, MAX_DT);
    while left > 0.0 {
        let h = left.min(SUBSTEP);
        *v += (-k * (*x - target) - c * *v) * h;
        *x += *v * h;
        left -= h;
    }
    // Rest is relative to the travel's scale, so pixels and 0..1 both settle.
    let scale = target.abs().max(1.0);
    if (*x - target).abs() < REST * scale && v.abs() < REST * scale * 10.0 {
        *x = target;
        *v = 0.0;
        true
    } else {
        false
    }
}

fn key(id: Id) -> Id {
    id.with("livestage-spring")
}

fn animate(ctx: &Context, id: Id, target: f32, spring: Spring) -> f32 {
    let now = ctx.input(|i| i.time);
    let id = key(id);
    let Some(mut s) = ctx.data(|d| d.get_temp::<State>(id)) else {
        // First sight: appear in place, no animation from nowhere.
        ctx.data_mut(|d| d.insert_temp(id, State { x: target, v: 0.0, time: now }));
        return target;
    };
    let dt = (now - s.time) as f32;
    s.time = now;
    let at_rest = s.x == target && s.v == 0.0 || step(&mut s.x, &mut s.v, target, spring, dt);
    if !at_rest {
        ctx.request_repaint();
    }
    ctx.data_mut(|d| d.insert_temp(id, s));
    s.x
}

/// A spatial value (position, size, scale) following `target`. Lands at
/// once when reduced motion is on.
pub fn value(ctx: &Context, id: Id, target: f32, spring: Spring) -> f32 {
    if reduce_motion() {
        snap(ctx, id, target);
        return target;
    }
    animate(ctx, id, target, spring)
}

/// `value` for an on / off state, as 0.0 ..= 1.0 (may overshoot slightly
/// with an under-damped spring; clamp where that matters).
pub fn toggle(ctx: &Context, id: Id, on: bool, spring: Spring) -> f32 {
    value(ctx, id, if on { 1.0 } else { 0.0 }, spring)
}

/// An opacity / colour mix following `target` (0.0 ..= 1.0). Under reduced
/// motion it still cross-fades, briefly.
pub fn fade(ctx: &Context, id: Id, target: f32, spring: Spring) -> f32 {
    let spring = if reduce_motion() { Spring::FADE } else { spring };
    animate(ctx, id, target, spring).clamp(0.0, 1.0)
}

/// Jumps a spring to `x` and stops it: instant feedback on pointer-down,
/// after which [`value`] springs it back to wherever it belongs (or a
/// starting point for something that should arrive with motion).
pub fn snap(ctx: &Context, id: Id, x: f32) {
    let now = ctx.input(|i| i.time);
    ctx.data_mut(|d| d.insert_temp(key(id), State { x, v: 0.0, time: now }));
}

// ---------------------------------------------------------- accessibility --

static REDUCE_MOTION_PREF: AtomicBool = AtomicBool::new(false);
static REDUCE_MOTION_OS: AtomicBool = AtomicBool::new(false);
static REDUCE_TRANSPARENCY_OS: AtomicBool = AtomicBool::new(false);
static INCREASE_CONTRAST_OS: AtomicBool = AtomicBool::new(false);

/// Settings → General → "Reduce motion" (in addition to the OS setting).
pub fn set_reduce_motion_pref(on: bool) {
    REDUCE_MOTION_PREF.store(on, Ordering::Relaxed);
}

/// Movement should be replaced by cross-fades.
pub fn reduce_motion() -> bool {
    REDUCE_MOTION_PREF.load(Ordering::Relaxed) || REDUCE_MOTION_OS.load(Ordering::Relaxed)
}

/// The OS asks for solid surfaces instead of translucent ones.
pub fn reduce_transparency() -> bool {
    REDUCE_TRANSPARENCY_OS.load(Ordering::Relaxed)
}

/// The OS asks for stronger borders and text contrast.
pub fn increase_contrast() -> bool {
    INCREASE_CONTRAST_OS.load(Ordering::Relaxed)
}

/// Reads the OS accessibility display settings (macOS: System Settings →
/// Accessibility → Display). Returns true when any of them changed. Cheap;
/// the theme polls it about once a second.
pub fn refresh_os_settings() -> bool {
    let (motion, transparency, contrast) = os_settings();
    let a = REDUCE_MOTION_OS.swap(motion, Ordering::Relaxed) != motion;
    let b = REDUCE_TRANSPARENCY_OS.swap(transparency, Ordering::Relaxed) != transparency;
    let c = INCREASE_CONTRAST_OS.swap(contrast, Ordering::Relaxed) != contrast;
    a || b || c
}

#[cfg(target_os = "macos")]
fn os_settings() -> (bool, bool, bool) {
    let ws = objc2_app_kit::NSWorkspace::sharedWorkspace();
    (
        ws.accessibilityDisplayShouldReduceMotion(),
        ws.accessibilityDisplayShouldReduceTransparency(),
        ws.accessibilityDisplayShouldIncreaseContrast(),
    )
}

#[cfg(not(target_os = "macos"))]
fn os_settings() -> (bool, bool, bool) {
    // Windows / Linux: the Settings toggle covers reduced motion.
    (false, false, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(spring: Spring, from: f32, to: f32, secs: f32) -> (f32, f32, bool, f32) {
        let (mut x, mut v) = (from, 0.0);
        let mut peak = from;
        let mut rest = false;
        let mut t = 0.0;
        while t < secs && !rest {
            rest = step(&mut x, &mut v, to, spring, 1.0 / 120.0);
            peak = if to > from { peak.max(x) } else { peak.min(x) };
            t += 1.0 / 120.0;
        }
        (x, v, rest, peak)
    }

    #[test]
    fn critically_damped_settles_without_overshoot() {
        let (x, _, rest, peak) = run(Spring::SMOOTH, 0.0, 100.0, 3.0);
        assert!(rest, "settles");
        assert_eq!(x, 100.0);
        assert!(peak <= 100.0 + 1e-3, "no overshoot, peak {peak}");
        // Most of the way there within about one `response`.
        let (x, ..) = run(Spring::SMOOTH, 0.0, 100.0, Spring::SMOOTH.response);
        assert!(x > 85.0, "{x}");
    }

    #[test]
    fn under_damped_overshoots_then_settles() {
        let (x, _, rest, peak) = run(Spring::RELEASE, 0.0, 1.0, 3.0);
        assert!(rest);
        assert_eq!(x, 1.0);
        assert!(peak > 1.005, "a little bounce, peak {peak}");
    }

    #[test]
    fn retargeting_keeps_velocity() {
        let (mut x, mut v) = (0.0_f32, 0.0_f32);
        for _ in 0..6 {
            step(&mut x, &mut v, 100.0, Spring::SMOOTH, 1.0 / 60.0);
        }
        let (x0, v0) = (x, v);
        assert!(v0 > 0.0);
        // Reverse mid-flight: the first step still moves forward (momentum),
        // with no jump in position.
        step(&mut x, &mut v, 0.0, Spring::SMOOTH, 1.0 / 60.0);
        assert!(x > x0 && x - x0 < v0 / 60.0 + 1.0, "continuous: {x0} → {x}");
        assert!(v < v0, "decelerating");
    }

    #[test]
    fn stalls_do_not_teleport() {
        let (mut x, mut v) = (0.0_f32, 0.0_f32);
        step(&mut x, &mut v, 100.0, Spring::SMOOTH, 5.0);
        assert!(x < 100.0, "a 5 s gap integrates at most {MAX_DT} s");
    }

    #[test]
    fn context_springs_appear_in_place_and_follow() {
        let ctx = Context::default();
        let id = Id::new("t");
        let frame = |t: f64, target: f32| {
            let mut out = 0.0;
            let input = egui::RawInput { time: Some(t), ..Default::default() };
            let mut output = ctx.run_ui(input, |ui| out = value(ui.ctx(), id, target, Spring::SNAPPY));
            // No renderer here: drop the font atlas uploads deliberately.
            output.textures_delta.clear();
            out
        };
        assert_eq!(frame(0.0, 10.0), 10.0, "first sight lands in place");
        let mid = frame(0.05, 20.0);
        assert!(mid > 10.0 && mid < 20.0, "{mid}");
        let mut t = 0.05;
        let mut last = mid;
        while t < 2.0 {
            t += 1.0 / 60.0;
            last = frame(t, 20.0);
        }
        assert_eq!(last, 20.0);
        set_reduce_motion_pref(true);
        assert_eq!(frame(t + 0.01, 0.0), 0.0, "reduced motion lands at once");
        set_reduce_motion_pref(false);
    }
}

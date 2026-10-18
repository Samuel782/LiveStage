//! Show time: a microsecond-resolution, serialisable duration / timestamp.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::{Add, AddAssign, Sub};
use std::time::Duration;

/// Microseconds since an arbitrary epoch (or a plain duration).
///
/// Microseconds give ample precision for DMX/video scheduling while keeping
/// JSON human-readable. Audio-accurate timing is expressed in sample frames
/// inside `livestage_audio` and converted at the boundary.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ShowTime(pub u64);

impl ShowTime {
    pub const ZERO: ShowTime = ShowTime(0);
    pub const MAX: ShowTime = ShowTime(u64::MAX);

    pub const fn from_micros(us: u64) -> Self {
        Self(us)
    }
    pub const fn from_millis(ms: u64) -> Self {
        Self(ms * 1_000)
    }
    pub const fn from_secs(s: u64) -> Self {
        Self(s * 1_000_000)
    }
    pub fn from_secs_f64(s: f64) -> Self {
        Self((s.max(0.0) * 1_000_000.0).round() as u64)
    }
    /// Converts a sample-frame position to show time.
    pub fn from_frames(frames: u64, sample_rate: u32) -> Self {
        if sample_rate == 0 {
            return Self::ZERO;
        }
        Self(((frames as u128 * 1_000_000) / sample_rate as u128) as u64)
    }
    pub const fn as_micros(self) -> u64 {
        self.0
    }
    pub fn as_secs_f64(self) -> f64 {
        self.0 as f64 / 1_000_000.0
    }
    pub fn as_secs_f32(self) -> f32 {
        self.as_secs_f64() as f32
    }
    pub fn to_frames(self, sample_rate: u32) -> u64 {
        ((self.0 as u128 * sample_rate as u128) / 1_000_000) as u64
    }
    pub fn saturating_sub(self, rhs: ShowTime) -> ShowTime {
        ShowTime(self.0.saturating_sub(rhs.0))
    }
    pub fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// `HH:MM:SS.cc` — the format used by the cue grid and show timers.
    pub fn to_clock_string(self) -> String {
        let total_cs = self.0 / 10_000;
        let cs = total_cs % 100;
        let s = (total_cs / 100) % 60;
        let m = (total_cs / 6_000) % 60;
        let h = total_cs / 360_000;
        format!("{h:02}:{m:02}:{s:02}.{cs:02}")
    }

    /// `MM:SS` (or `H:MM:SS` above one hour) for large countdown displays.
    pub fn to_timer_string(self) -> String {
        let total = self.0 / 1_000_000;
        let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
        if h > 0 {
            format!("{h}:{m:02}:{s:02}")
        } else {
            format!("{m:02}:{s:02}")
        }
    }
}

impl From<Duration> for ShowTime {
    fn from(d: Duration) -> Self {
        Self(d.as_micros().min(u64::MAX as u128) as u64)
    }
}
impl From<ShowTime> for Duration {
    fn from(t: ShowTime) -> Self {
        Duration::from_micros(t.0)
    }
}
impl Add for ShowTime {
    type Output = ShowTime;
    fn add(self, rhs: Self) -> Self {
        ShowTime(self.0.saturating_add(rhs.0))
    }
}
impl AddAssign for ShowTime {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}
impl Sub for ShowTime {
    type Output = ShowTime;
    fn sub(self, rhs: Self) -> Self {
        self.saturating_sub(rhs)
    }
}
impl fmt::Debug for ShowTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.6}s", self.as_secs_f64())
    }
}
impl fmt::Display for ShowTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_clock_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clock_format() {
        assert_eq!(ShowTime::from_millis(3_723_450).to_clock_string(), "01:02:03.45");
        assert_eq!(ShowTime::from_secs(65).to_timer_string(), "01:05");
    }
    #[test]
    fn frames_roundtrip() {
        let t = ShowTime::from_frames(48_000 * 3, 48_000);
        assert_eq!(t, ShowTime::from_secs(3));
        assert_eq!(t.to_frames(48_000), 144_000);
    }
}

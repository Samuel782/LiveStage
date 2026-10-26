//! SMPTE timecode: labels, frame rates (including 29.97 drop-frame), exact
//! conversion to and from [`ShowTime`], and the machinery that makes the
//! show follow a timecode stream.
//!
//! * [`Timecode`] / [`FrameRate`] — `HH:MM:SS:FF` labels and their math.
//! * [`lock`] — locks a jittery incoming stream (MTC, Art-Net) to the local
//!   clock with a phase/frequency loop: drift compensation, dropout
//!   freewheel, jump and stop detection.
//! * [`ltc`] — linear timecode (SMPTE 12M) audio encoder and decoder.
//! * [`clock`] — the internal generator (wall clock or audio sample clock).
//! * [`engine`] — picks the configured source and publishes a
//!   [`TcTransport`] every show tick.
//! * [`slave`] — timeline cues and cue lists that chase the transport.
//!
//! ## Time base
//!
//! A timecode *position* is the real time elapsed since `00:00:00:00` at the
//! stream's actual rate, as a [`ShowTime`]: frame `n` starts at
//! `n × den / num` seconds (`1001 / 30000` for 29.97). Drop-frame labels skip
//! frame numbers, not frames, so positions stay continuous across them.

pub mod clock;
pub mod engine;
pub mod lock;
pub mod ltc;
pub mod slave;

pub use clock::{ClockBase, InternalClock};
pub use engine::{TimecodeConfig, TimecodeEngine, TimecodeOutput, TimecodeSource};
pub use lock::{LockConfig, LockState, SampleKind, TcSample, TimecodeLock};
pub use slave::{ListTimecode, OnTimecodeStop, TimecodeSlave};

use crate::time::ShowTime;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Frames in ten minutes of 29.97 drop-frame (10 × 1800 − 9 × 2).
const DF_FRAMES_PER_10_MIN: u64 = 17_982;
/// Frames in a dropping minute of 29.97 drop-frame (1800 − 2).
const DF_FRAMES_PER_MIN: u64 = 1_798;

/// Standard SMPTE frame rates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameRate {
    /// 24 fps film.
    Fps24,
    /// 25 fps EBU / PAL.
    #[default]
    Fps25,
    /// 29.97 fps drop-frame (NTSC broadcast): labels stay in step with the wall clock.
    Fps2997Df,
    /// 29.97 fps non-drop: labels run 0.1 % slow against the wall clock.
    Fps2997,
    /// 30 fps.
    Fps30,
}

impl FrameRate {
    pub const ALL: [FrameRate; 5] = [FrameRate::Fps24, FrameRate::Fps25, FrameRate::Fps2997Df, FrameRate::Fps2997, FrameRate::Fps30];

    /// Frame numbers per second in the label (`FF` runs `0..nominal`).
    pub const fn nominal(self) -> u64 {
        match self {
            FrameRate::Fps24 => 24,
            FrameRate::Fps25 => 25,
            FrameRate::Fps2997Df | FrameRate::Fps2997 | FrameRate::Fps30 => 30,
        }
    }

    /// Exact rate as `num / den` frames per second.
    pub const fn ratio(self) -> (u64, u64) {
        match self {
            FrameRate::Fps24 => (24, 1),
            FrameRate::Fps25 => (25, 1),
            FrameRate::Fps2997Df | FrameRate::Fps2997 => (30_000, 1_001),
            FrameRate::Fps30 => (30, 1),
        }
    }

    pub fn fps(self) -> f64 {
        let (n, d) = self.ratio();
        n as f64 / d as f64
    }

    pub const fn is_drop_frame(self) -> bool {
        matches!(self, FrameRate::Fps2997Df)
    }

    /// Frames in 24 hours of labels (the wrap point).
    pub const fn frames_per_day(self) -> u64 {
        if self.is_drop_frame() {
            DF_FRAMES_PER_10_MIN * 6 * 24
        } else {
            self.nominal() * 86_400
        }
    }

    /// Real time from `00:00:00:00` to the wrap point.
    pub fn day_length(self) -> ShowTime {
        self.frame_start(self.frames_per_day())
    }

    /// Duration of one frame (rounded to the microsecond).
    pub fn frame_duration(self) -> ShowTime {
        let (n, d) = self.ratio();
        ShowTime::from_micros((d * 1_000_000 + n / 2) / n)
    }

    /// When frame `frame` starts. Rounded *up* to the microsecond, so
    /// [`FrameRate::frame_at`] of the result is `frame` again.
    pub fn frame_start(self, frame: u64) -> ShowTime {
        let (n, d) = self.ratio();
        ShowTime::from_micros((frame as u128 * d as u128 * 1_000_000).div_ceil(n as u128) as u64)
    }

    /// Index of the frame on screen at `t`.
    pub fn frame_at(self, t: ShowTime) -> u64 {
        let (n, d) = self.ratio();
        (t.as_micros() as u128 * n as u128 / (d as u128 * 1_000_000)) as u64
    }

    /// Fractional frames (sub-frame precision) to microseconds.
    pub fn frames_to_micros(self, frames: f64) -> f64 {
        let (n, d) = self.ratio();
        frames * d as f64 * 1_000_000.0 / n as f64
    }

    /// Microseconds to fractional frames.
    pub fn micros_to_frames(self, us: f64) -> f64 {
        let (n, d) = self.ratio();
        us * n as f64 / (d as f64 * 1_000_000.0)
    }

    /// Operator-facing name.
    pub fn label(self) -> &'static str {
        match self {
            FrameRate::Fps24 => "24 fps",
            FrameRate::Fps25 => "25 fps",
            FrameRate::Fps2997Df => "29.97 fps drop",
            FrameRate::Fps2997 => "29.97 fps",
            FrameRate::Fps30 => "30 fps",
        }
    }

    /// Short token used in serialised timecodes (`01:00:00:00@25`).
    pub fn token(self) -> &'static str {
        match self {
            FrameRate::Fps24 => "24",
            FrameRate::Fps25 => "25",
            FrameRate::Fps2997Df => "29.97df",
            FrameRate::Fps2997 => "29.97",
            FrameRate::Fps30 => "30",
        }
    }

    pub fn from_token(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "24" => FrameRate::Fps24,
            "25" => FrameRate::Fps25,
            "29.97df" | "29.97d" | "2997df" | "df" => FrameRate::Fps2997Df,
            "29.97" | "29.97ndf" | "2997" => FrameRate::Fps2997,
            "30" => FrameRate::Fps30,
            _ => return None,
        })
    }

    /// Rate code in MTC (hours bits 5–6) and Art-Net timecode `Type`. Both
    /// formats only know 24 / 25 / 30 drop / 30, so 29.97 non-drop is sent
    /// as 30 (the labels are identical; only the real-time rate differs).
    pub const fn smpte_code(self) -> u8 {
        match self {
            FrameRate::Fps24 => 0,
            FrameRate::Fps25 => 1,
            FrameRate::Fps2997Df => 2,
            FrameRate::Fps2997 | FrameRate::Fps30 => 3,
        }
    }

    pub const fn from_smpte_code(code: u8) -> Self {
        match code & 0x03 {
            0 => FrameRate::Fps24,
            1 => FrameRate::Fps25,
            2 => FrameRate::Fps2997Df,
            _ => FrameRate::Fps30,
        }
    }
}

impl fmt::Display for FrameRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TimecodeError {
    #[error("invalid timecode \"{0}\" (expected HH:MM:SS:FF)")]
    Format(String),
    #[error("timecode field out of range: {0}")]
    Range(String),
}

/// An SMPTE timecode label at a frame rate.
///
/// Serialised as a string, e.g. `"01:00:00:00@25"` or `"00:59:59;29@29.97df"`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Timecode {
    pub hours: u8,
    pub minutes: u8,
    pub seconds: u8,
    pub frames: u8,
    pub rate: FrameRate,
}

impl Timecode {
    pub fn new(hours: u8, minutes: u8, seconds: u8, frames: u8, rate: FrameRate) -> Result<Self, TimecodeError> {
        let tc = Self { hours, minutes, seconds, frames, rate };
        if hours > 23 || minutes > 59 || seconds > 59 || frames as u64 >= rate.nominal() {
            return Err(TimecodeError::Range(tc.to_string()));
        }
        if tc.is_dropped_label() {
            return Err(TimecodeError::Range(format!("{tc} is skipped in drop-frame")));
        }
        Ok(tc)
    }

    pub const fn zero(rate: FrameRate) -> Self {
        Self { hours: 0, minutes: 0, seconds: 0, frames: 0, rate }
    }

    /// Drop-frame skips frames 00 and 01 at the start of every minute except
    /// minutes 00, 10, 20, 30, 40 and 50.
    fn is_dropped_label(&self) -> bool {
        self.rate.is_drop_frame() && self.seconds == 0 && self.frames < 2 && !self.minutes.is_multiple_of(10)
    }

    /// Frame count since `00:00:00:00`.
    pub fn to_frames(&self) -> u64 {
        let (h, m, s, f) = (self.hours as u64, self.minutes as u64, self.seconds as u64, self.frames as u64);
        let nominal = self.rate.nominal();
        let label = ((h * 60 + m) * 60 + s) * nominal + f;
        if self.rate.is_drop_frame() {
            let total_minutes = h * 60 + m;
            label - 2 * (total_minutes - total_minutes / 10)
        } else {
            label
        }
    }

    /// The label of frame `frame` (wrapping at 24 hours).
    pub fn from_frames(frame: u64, rate: FrameRate) -> Self {
        let mut n = frame % rate.frames_per_day();
        if rate.is_drop_frame() {
            // Re-insert the skipped frame numbers, then count as 30 fps.
            let tens = n / DF_FRAMES_PER_10_MIN;
            let rem = n % DF_FRAMES_PER_10_MIN;
            let dropped = if rem < 2 { 18 * tens } else { 18 * tens + 2 * ((rem - 2) / DF_FRAMES_PER_MIN) };
            n += dropped;
        }
        let nominal = rate.nominal();
        Self {
            frames: (n % nominal) as u8,
            seconds: ((n / nominal) % 60) as u8,
            minutes: ((n / (nominal * 60)) % 60) as u8,
            hours: ((n / (nominal * 3600)) % 24) as u8,
            rate,
        }
    }

    /// Real time since `00:00:00:00` at which this frame starts.
    pub fn to_time(&self) -> ShowTime {
        self.rate.frame_start(self.to_frames())
    }

    /// The frame on screen at `t` (wrapping at 24 hours).
    pub fn from_time(t: ShowTime, rate: FrameRate) -> Self {
        Self::from_frames(rate.frame_at(t), rate)
    }

    /// The label at `t` plus how far into the frame `t` is (0..1).
    pub fn from_time_precise(t: ShowTime, rate: FrameRate) -> (Self, f32) {
        let frames = rate.micros_to_frames(t.as_micros() as f64);
        let tc = Self::from_time(t, rate);
        (tc, (frames - frames.floor()).clamp(0.0, 0.999_999) as f32)
    }

    /// The same label at another rate (frames clamped into range, dropped
    /// drop-frame labels moved to the next valid frame). This is how a cue
    /// set to start "at 01:00:00:00" follows a stream of any rate.
    pub fn with_rate(self, rate: FrameRate) -> Self {
        let mut tc = Self { rate, frames: (self.frames as u64).min(rate.nominal() - 1) as u8, ..self };
        if tc.is_dropped_label() {
            tc.frames = 2;
        }
        tc
    }

    /// Adds (or subtracts) frames, wrapping at 24 hours.
    pub fn add_frames(self, frames: i64) -> Self {
        let day = self.rate.frames_per_day() as i64;
        let n = (self.to_frames() as i64 + frames).rem_euclid(day);
        Self::from_frames(n as u64, self.rate)
    }

    /// Signed frame distance from `self` to `other` (`other` re-labelled at
    /// this rate), taking the shorter way around midnight.
    pub fn frames_until(self, other: Timecode) -> i64 {
        let day = self.rate.frames_per_day() as i64;
        let d = (other.with_rate(self.rate).to_frames() as i64 - self.to_frames() as i64).rem_euclid(day);
        if d > day / 2 {
            d - day
        } else {
            d
        }
    }

    /// Parses `HH:MM:SS:FF` (`;` or `.` before the frames marks drop-frame),
    /// with an optional `@rate` suffix. Without a suffix `default` is used
    /// (switched to drop-frame by a `;`).
    pub fn parse_with(s: &str, default: FrameRate) -> Result<Self, TimecodeError> {
        let err = || TimecodeError::Format(s.to_string());
        let (label, rate) = match s.split_once('@') {
            Some((l, r)) => (l.trim(), Some(FrameRate::from_token(r).ok_or_else(err)?)),
            None => (s.trim(), None),
        };
        let drop_sep = label.contains(';') || label.matches('.').count() == 1 && label.matches(':').count() == 2;
        let parts: Vec<&str> = label.split([':', ';', '.']).collect();
        if parts.len() != 4 || parts.iter().any(|p| p.is_empty() || p.len() > 2 || !p.bytes().all(|b| b.is_ascii_digit())) {
            return Err(err());
        }
        let n: Vec<u8> = parts.iter().map(|p| p.parse().unwrap_or(0)).collect();
        let rate = rate.unwrap_or(if drop_sep && !default.is_drop_frame() { FrameRate::Fps2997Df } else { default });
        Self::new(n[0], n[1], n[2], n[3], rate)
    }
}

impl Default for Timecode {
    fn default() -> Self {
        Self::zero(FrameRate::default())
    }
}

impl FromStr for Timecode {
    type Err = TimecodeError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse_with(s, FrameRate::default())
    }
}

impl fmt::Display for Timecode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sep = if self.rate.is_drop_frame() { ';' } else { ':' };
        write!(f, "{:02}:{:02}:{:02}{sep}{:02}", self.hours, self.minutes, self.seconds, self.frames)
    }
}

impl fmt::Debug for Timecode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}@{}", self.rate.token())
    }
}

impl Serialize for Timecode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(&format_args!("{self}@{}", self.rate.token()))
    }
}

impl<'de> Deserialize<'de> for Timecode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Where the show's timecode currently is — published every show tick by
/// the [`TimecodeEngine`] and consumed by slaved timelines and lists.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TcTransport {
    /// Real time since `00:00:00:00` (sub-frame precision).
    pub position: ShowTime,
    pub rate: FrameRate,
    /// Timecode is rolling (locked or freewheeling).
    pub running: bool,
    /// Bumped on every discontinuity (jump, locate, relock), so followers
    /// can tell a seek from continuous play.
    pub generation: u64,
    pub state: LockState,
    /// Measured source speed against the local clock (1.0 = no drift).
    pub speed: f64,
}

impl Default for TcTransport {
    fn default() -> Self {
        Self {
            position: ShowTime::ZERO,
            rate: FrameRate::default(),
            running: false,
            generation: 0,
            state: LockState::Stopped,
            speed: 1.0,
        }
    }
}

impl TcTransport {
    pub fn timecode(&self) -> Timecode {
        Timecode::from_time(self.position, self.rate)
    }
    /// Source drift against the local clock in parts per million.
    pub fn drift_ppm(&self) -> f64 {
        (self.speed - 1.0) * 1e6
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tc(s: &str) -> Timecode {
        s.parse().unwrap()
    }

    #[test]
    fn parse_and_display() {
        let t = tc("01:02:03:04");
        assert_eq!((t.hours, t.minutes, t.seconds, t.frames, t.rate), (1, 2, 3, 4, FrameRate::Fps25));
        assert_eq!(t.to_string(), "01:02:03:04");
        let df = tc("00:01:00;02");
        assert_eq!(df.rate, FrameRate::Fps2997Df);
        assert_eq!(df.to_string(), "00:01:00;02");
        assert_eq!(tc("10:00:00:23@24").rate, FrameRate::Fps24);
        assert_eq!(Timecode::parse_with("00:00:10:29", FrameRate::Fps30).unwrap().frames, 29);
        assert!("01:00:00:25".parse::<Timecode>().is_err(), "25 fps has frames 0..24");
        assert!("00:01:00;00".parse::<Timecode>().is_err(), "dropped label");
        assert!("24:00:00:00".parse::<Timecode>().is_err());
        assert!("1:2:3".parse::<Timecode>().is_err());
        assert!("aa:00:00:00".parse::<Timecode>().is_err());
    }

    #[test]
    fn serde_roundtrip_keeps_rate() {
        let t = Timecode::new(23, 59, 59, 29, FrameRate::Fps2997Df).unwrap();
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(json, "\"23:59:59;29@29.97df\"");
        assert_eq!(serde_json::from_str::<Timecode>(&json).unwrap(), t);
        assert_eq!(serde_json::from_str::<Timecode>("\"01:00:00:00\"").unwrap().rate, FrameRate::Fps25);
    }

    #[test]
    fn non_drop_frame_counts() {
        for rate in [FrameRate::Fps24, FrameRate::Fps25, FrameRate::Fps30, FrameRate::Fps2997] {
            let n = rate.nominal();
            assert_eq!(Timecode::new(1, 0, 0, 0, rate).unwrap().to_frames(), 3600 * n);
            assert_eq!(Timecode::new(0, 0, 1, 1, rate).unwrap().to_frames(), n + 1);
            for f in [0, 1, n - 1, n, 3600 * n - 1, rate.frames_per_day() - 1] {
                assert_eq!(Timecode::from_frames(f, rate).to_frames(), f, "{rate} frame {f}");
            }
        }
    }

    #[test]
    fn drop_frame_math() {
        let r = FrameRate::Fps2997Df;
        // The frame after 00:00:59;29 is 00:01:00;02 (00 and 01 are skipped).
        let a = Timecode::new(0, 0, 59, 29, r).unwrap();
        assert_eq!(a.to_frames(), 1799);
        assert_eq!(a.add_frames(1).to_string(), "00:01:00;02");
        // ...but not at the tenth minute.
        let b = Timecode::new(0, 9, 59, 29, r).unwrap();
        assert_eq!(b.add_frames(1).to_string(), "00:10:00;00");
        assert_eq!(Timecode::new(0, 10, 0, 0, r).unwrap().to_frames(), 17_982);
        // One hour of drop-frame labels is 107 892 frames, i.e. 3599.9964 s of real time.
        let hour = Timecode::new(1, 0, 0, 0, r).unwrap();
        assert_eq!(hour.to_frames(), 107_892);
        assert_eq!(hour.to_time(), ShowTime::from_micros(3_599_996_400));
        // Every frame of a day round-trips through its label.
        for f in (0..r.frames_per_day()).step_by(7) {
            let t = Timecode::from_frames(f, r);
            assert!(!t.is_dropped_label(), "{t:?}");
            assert_eq!(t.to_frames(), f);
        }
        assert_eq!(Timecode::from_frames(r.frames_per_day() - 1, r).to_string(), "23:59:59;29");
    }

    #[test]
    fn non_drop_2997_runs_slow_against_wall_clock() {
        // One hour of 29.97 non-drop labels lasts 3603.6 s.
        let hour = Timecode::new(1, 0, 0, 0, FrameRate::Fps2997).unwrap();
        assert_eq!(hour.to_time(), ShowTime::from_micros(3_603_600_000));
        assert_eq!(Timecode::new(1, 0, 0, 0, FrameRate::Fps25).unwrap().to_time(), ShowTime::from_secs(3600));
    }

    #[test]
    fn time_conversion_is_exact_per_frame() {
        for rate in FrameRate::ALL {
            for f in [0u64, 1, 2, 29, 30, 1798, 1799, 1800, 17_982, 107_891, rate.frames_per_day() - 1] {
                let start = rate.frame_start(f);
                assert_eq!(rate.frame_at(start), f, "{rate} frame {f} start");
                assert_eq!(rate.frame_at(rate.frame_start(f + 1) - ShowTime::from_micros(1)), f, "{rate} frame {f} end");
            }
        }
        assert_eq!(FrameRate::Fps25.frame_duration(), ShowTime::from_millis(40));
        assert_eq!(FrameRate::Fps2997Df.frame_duration(), ShowTime::from_micros(33_367));
        let (t, sub) = Timecode::from_time_precise(ShowTime::from_millis(1_020), FrameRate::Fps25);
        assert_eq!(t.to_string(), "00:00:01:00");
        assert!((sub - 0.5).abs() < 1e-3);
    }

    #[test]
    fn arithmetic_wraps_at_midnight() {
        let t = tc("23:59:59:24");
        assert_eq!(t.add_frames(1).to_string(), "00:00:00:00");
        assert_eq!(tc("00:00:00:00").add_frames(-1).to_string(), "23:59:59:24");
        assert_eq!(t.frames_until(tc("00:00:00:01")), 2, "shorter way across midnight");
        assert_eq!(tc("01:00:00:00").frames_until(tc("00:59:59:00")), -25);
    }

    #[test]
    fn relabel_between_rates() {
        let t = tc("01:00:00:24");
        assert_eq!(t.with_rate(FrameRate::Fps24).frames, 23);
        assert_eq!(tc("00:01:00:00").with_rate(FrameRate::Fps2997Df).to_string(), "00:01:00;02");
        assert_eq!(t.with_rate(FrameRate::Fps30).to_string(), "01:00:00:24");
    }

    #[test]
    fn smpte_codes() {
        for rate in [FrameRate::Fps24, FrameRate::Fps25, FrameRate::Fps2997Df, FrameRate::Fps30] {
            assert_eq!(FrameRate::from_smpte_code(rate.smpte_code()), rate);
        }
        assert_eq!(FrameRate::from_smpte_code(FrameRate::Fps2997.smpte_code()), FrameRate::Fps30);
    }
}

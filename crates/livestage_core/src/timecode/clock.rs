//! The internal timecode generator.
//!
//! A free-running clock advanced from a *time base*: either the show's wall
//! clock or the audio engine's sample clock. Following the sample clock
//! keeps timecode-slaved timelines in exact step with what the speakers
//! play (the audio device's crystal, not the CPU's, sets the pace). The
//! clock integrates base *deltas*, so the base can be switched (or the audio
//! device restarted) without a jump.

use super::{FrameRate, LockState, TcTransport};
use crate::time::ShowTime;
use serde::{Deserialize, Serialize};

/// What drives the internal generator.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockBase {
    /// The show's monotonic wall clock.
    Wall,
    /// The audio device sample clock (falls back to the wall clock without audio).
    #[default]
    Audio,
}

impl ClockBase {
    pub fn label(self) -> &'static str {
        match self {
            ClockBase::Wall => "System clock",
            ClockBase::Audio => "Audio sample clock",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct InternalClock {
    pub rate: FrameRate,
    running: bool,
    position: ShowTime,
    /// Last base reading and which base it came from.
    last_base: Option<(ClockBase, ShowTime)>,
    generation: u64,
}

impl InternalClock {
    pub fn new(rate: FrameRate) -> Self {
        Self { rate, ..Self::default() }
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    pub fn start(&mut self) {
        self.running = true;
    }

    pub fn stop(&mut self) {
        self.running = false;
    }

    /// Moves the clock (rolling or not) to `position`.
    pub fn locate(&mut self, position: ShowTime) {
        self.position = ShowTime::from_micros(position.as_micros() % self.rate.day_length().as_micros().max(1));
        self.generation += 1;
    }

    /// Advances by the time `base` moved since the previous reading. A
    /// change of base, or a base going backwards (audio device restarted),
    /// only re-references the clock.
    pub fn advance(&mut self, base: ClockBase, now: ShowTime) {
        if let Some((b, last)) = self.last_base {
            if self.running && b == base && now > last {
                let day = self.rate.day_length().as_micros().max(1);
                self.position = ShowTime::from_micros((self.position + (now - last)).as_micros() % day);
            }
        }
        self.last_base = Some((base, now));
    }

    pub fn position(&self) -> ShowTime {
        self.position
    }

    pub fn transport(&self) -> TcTransport {
        TcTransport {
            position: self.position,
            rate: self.rate,
            running: self.running,
            generation: self.generation,
            state: if self.running { LockState::Locked } else { LockState::Stopped },
            speed: 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timecode::Timecode;

    #[test]
    fn runs_from_its_base_only_while_started() {
        let mut c = InternalClock::new(FrameRate::Fps25);
        c.locate("01:00:00:00".parse::<Timecode>().unwrap().to_time());
        c.advance(ClockBase::Wall, ShowTime::from_secs(10));
        c.advance(ClockBase::Wall, ShowTime::from_secs(11));
        assert_eq!(c.transport().timecode().to_string(), "01:00:00:00", "stopped clocks hold");
        c.start();
        c.advance(ClockBase::Wall, ShowTime::from_millis(12_040));
        assert_eq!(c.transport().timecode().to_string(), "01:00:01:01");
        c.stop();
        c.advance(ClockBase::Wall, ShowTime::from_secs(20));
        assert_eq!(c.transport().timecode().to_string(), "01:00:01:01");
    }

    #[test]
    fn switching_base_does_not_jump() {
        let mut c = InternalClock::new(FrameRate::Fps30);
        c.start();
        c.advance(ClockBase::Wall, ShowTime::from_secs(100));
        c.advance(ClockBase::Wall, ShowTime::from_secs(101));
        // The audio clock reads a completely different absolute value.
        c.advance(ClockBase::Audio, ShowTime::from_secs(3));
        c.advance(ClockBase::Audio, ShowTime::from_millis(3_500));
        assert_eq!(c.position(), ShowTime::from_millis(1_500));
        // Audio device restarted: its clock went back to zero.
        c.advance(ClockBase::Audio, ShowTime::ZERO);
        c.advance(ClockBase::Audio, ShowTime::from_millis(500));
        assert_eq!(c.position(), ShowTime::from_secs(2));
    }

    #[test]
    fn audio_base_tracks_sample_clock_not_wall_clock() {
        // The generator counts rendered frames: 600 s worth of samples is ten
        // minutes of timecode however long the (drifting) device took.
        let mut c = InternalClock::new(FrameRate::Fps25);
        c.start();
        c.advance(ClockBase::Audio, ShowTime::ZERO);
        for k in 1..=600u64 {
            c.advance(ClockBase::Audio, ShowTime::from_frames(k * 48_000, 48_000));
        }
        assert_eq!(c.transport().timecode().to_string(), "00:10:00:00");
    }

    #[test]
    fn locate_bumps_generation_and_wraps() {
        let mut c = InternalClock::new(FrameRate::Fps24);
        let g = c.transport().generation;
        c.locate(FrameRate::Fps24.day_length() + ShowTime::from_secs(1));
        assert_eq!(c.transport().generation, g + 1);
        assert_eq!(c.transport().timecode().to_string(), "00:00:01:00");
    }
}

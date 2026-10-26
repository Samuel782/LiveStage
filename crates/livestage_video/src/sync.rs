//! Frame-locking visual layers to an external master clock.
//!
//! A layer normally free-runs on the show (wall) clock. The audio device runs
//! on its own crystal, so over a long song the two drift apart (tens of ppm,
//! i.e. a frame every few minutes). A layer bound to a master — a timeline's
//! audio cue — instead *follows* the audio sample clock every frame:
//!
//! * inside a 1 ms deadband it plays at exactly 1×;
//! * small errors are slewed out by nudging its speed (at most ±5 %, closing
//!   the gap over about a second), so the decoder only ever drops or repeats
//!   an occasional frame — invisible on stage;
//! * a layer late by more than [`SNAP_BEHIND`] jumps to the master (frames
//!   are skipped, or the decoder seeks when the jump is large);
//! * a layer early by up to [`HOLD_AHEAD`] holds its picture until the audio
//!   catches up, which avoids a backwards seek; beyond that it jumps back;
//! * while the master is held (count-in, pause, seek pending) the layer is
//!   pinned to it; when the master is gone (ended, no audio device) the layer
//!   free-runs from where it is.

use livestage_core::time::ShowTime;

/// Inside this error the layer plays at exactly 1×.
pub const DEADBAND: ShowTime = ShowTime::from_millis(1);
/// Video later than the master by more than this jumps forward.
pub const SNAP_BEHIND: ShowTime = ShowTime::from_millis(80);
/// Video earlier than the master by more than [`SNAP_BEHIND`] but less than
/// this holds its frame; beyond it the layer jumps back.
pub const HOLD_AHEAD: ShowTime = ShowTime::from_millis(500);
/// Largest speed change used to slew out small errors.
pub const MAX_SLEW: f64 = 0.05;
/// Time over which a slewed error is closed.
const SLEW_HORIZON_SECS: f64 = 1.0;
/// Weight of a new error sample in the smoothed error (rejects the jitter of
/// callback-interpolated audio positions so the speed doesn't hunt).
const ERROR_SMOOTHING: f64 = 0.2;

/// What the master clock is doing this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MasterClock {
    /// Advancing; the value is its media time.
    Running(ShowTime),
    /// Stopped at a position (count-in, paused, about to start).
    Held(ShowTime),
    /// No clock to follow (ended, stopped, no audio device).
    Gone,
}

/// What the corrector did on the last frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Correction {
    /// Not following a master (or not yet).
    #[default]
    FreeRun,
    /// Within the deadband at 1×.
    Locked,
    /// Speed nudged to close a small error.
    Slewing,
    /// Ahead of the master: holding the frame until it catches up.
    Holding,
    /// Jumped to the master.
    Snapped,
    /// Master held: pinned to its position.
    Pinned,
}

/// A layer's media clock: a position anchored at a show time, advancing at
/// `rate` (1× unless a master is being followed), freezable by pause.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayerClock {
    anchor_now: ShowTime,
    anchor_media: ShowTime,
    rate: f64,
    paused_at: Option<ShowTime>,
}

impl LayerClock {
    pub fn start(now: ShowTime, media: ShowTime) -> Self {
        Self { anchor_now: now, anchor_media: media, rate: 1.0, paused_at: None }
    }

    /// Media time at show time `now`.
    pub fn at(&self, now: ShowTime) -> ShowTime {
        let now = self.paused_at.unwrap_or(now);
        let run = (now - self.anchor_now).as_micros();
        let advanced = if self.rate == 1.0 { run } else { (run as f64 * self.rate).round() as u64 };
        self.anchor_media + ShowTime::from_micros(advanced)
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    pub fn paused_at(&self) -> Option<ShowTime> {
        self.paused_at
    }

    pub fn pause(&mut self, now: ShowTime) {
        self.paused_at.get_or_insert(now);
    }

    pub fn resume(&mut self, now: ShowTime) {
        if let Some(p) = self.paused_at.take() {
            self.anchor_now += now - p;
        }
    }

    /// Jumps to `media`; a paused clock stays paused there.
    pub fn seek(&mut self, now: ShowTime, media: ShowTime) {
        self.anchor_now = self.paused_at.unwrap_or(now);
        self.anchor_media = media;
    }

    /// Changes speed from `now` on without moving the current position.
    fn set_rate(&mut self, now: ShowTime, rate: f64) {
        if rate != self.rate {
            self.seek(now, self.at(now));
            self.rate = rate;
        }
    }
}

/// Binds a layer to a master: the layer's media time should be
/// `master - offset`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayerSync {
    pub master: livestage_core::id::CueId,
    pub offset: ShowTime,
    /// Smoothed error (master − layer) in seconds; positive = video late.
    error: f64,
    /// Error measured on the last frame (unsmoothed), seconds.
    pub last_error: f64,
    pub last: Correction,
    /// Hard re-times since the layer started.
    pub snaps: u32,
}

impl LayerSync {
    pub fn new(master: livestage_core::id::CueId, offset: ShowTime) -> Self {
        Self { master, offset, error: 0.0, last_error: 0.0, last: Correction::FreeRun, snaps: 0 }
    }

    /// Re-times `clock` towards the master for the frame shown at `now`.
    pub fn follow(&mut self, clock: &mut LayerClock, now: ShowTime, master: MasterClock) -> Correction {
        let correction = self.correct(clock, now, master);
        self.last = correction;
        if correction == Correction::Snapped {
            self.snaps += 1;
        }
        correction
    }

    fn correct(&mut self, clock: &mut LayerClock, now: ShowTime, master: MasterClock) -> Correction {
        if clock.paused_at.is_some() {
            // The cue paused the layer; its master is paused alongside.
            self.error = 0.0;
            return Correction::Pinned;
        }
        let (position, running) = match master {
            MasterClock::Running(p) => (p, true),
            MasterClock::Held(p) => (p, false),
            MasterClock::Gone => {
                clock.set_rate(now, 1.0);
                self.error = 0.0;
                self.last_error = 0.0;
                return Correction::FreeRun;
            }
        };
        let target = position - self.offset;
        let current = clock.at(now);
        let error = signed_secs(target, current);
        self.last_error = error;
        if !running || position < self.offset {
            // Count-in, paused master, or a master not yet at this layer's
            // anchor: show the frame the master points at and wait.
            clock.seek(now, target);
            clock.rate = 0.0;
            self.error = 0.0;
            return Correction::Pinned;
        }
        let behind = error > SNAP_BEHIND.as_secs_f64();
        let far_ahead = -error > HOLD_AHEAD.as_secs_f64();
        if behind || far_ahead {
            clock.seek(now, target);
            clock.rate = 1.0;
            self.error = 0.0;
            return Correction::Snapped;
        }
        if -error > SNAP_BEHIND.as_secs_f64() {
            // Well ahead: hold the picture rather than seeking backwards.
            clock.set_rate(now, 0.0);
            self.error = error;
            return Correction::Holding;
        }
        self.error += (error - self.error) * ERROR_SMOOTHING;
        if self.error.abs() <= DEADBAND.as_secs_f64() {
            clock.set_rate(now, 1.0);
            return Correction::Locked;
        }
        let slew = (self.error / SLEW_HORIZON_SECS).clamp(-MAX_SLEW, MAX_SLEW);
        clock.set_rate(now, 1.0 + slew);
        Correction::Slewing
    }
}

fn signed_secs(a: ShowTime, b: ShowTime) -> f64 {
    (a.as_micros() as i128 - b.as_micros() as i128) as f64 / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use livestage_core::id::CueId;

    const FRAME: ShowTime = ShowTime::from_micros(16_667);

    /// Plays a layer against an audio clock running `ppm` fast for `secs`,
    /// returning the worst error after the initial lock-in and the snaps.
    fn run(ppm: f64, secs: u64, jitter_us: u64) -> (f64, u32) {
        let mut clock = LayerClock::start(ShowTime::ZERO, ShowTime::ZERO);
        let mut sync = LayerSync::new(CueId::new(), ShowTime::ZERO);
        let mut worst = 0.0f64;
        let frames = secs * 60;
        for i in 1..=frames {
            let now = ShowTime::from_micros(i * FRAME.as_micros());
            let jitter = if i % 2 == 0 { jitter_us } else { 0 };
            let audio = ShowTime::from_secs_f64(now.as_secs_f64() * (1.0 + ppm * 1e-6)) + ShowTime::from_micros(jitter);
            sync.follow(&mut clock, now, MasterClock::Running(audio));
            if i > 120 {
                worst = worst.max(signed_secs(audio, clock.at(now)).abs());
            }
        }
        (worst, sync.snaps)
    }

    #[test]
    fn crystal_drift_is_slewed_out_without_snaps() {
        // 100 ppm is a bad crystal: 360 ms an hour if left free-running.
        let (worst, snaps) = run(100.0, 600, 0);
        assert_eq!(snaps, 0);
        assert!(worst < 0.003, "error {worst}");
        let (worst, snaps) = run(-100.0, 600, 0);
        assert_eq!(snaps, 0);
        assert!(worst < 0.003, "error {worst}");
    }

    #[test]
    fn interpolation_jitter_does_not_cause_hunting() {
        // ±1 ms jitter from callback-interpolated audio positions.
        let (worst, snaps) = run(20.0, 120, 1_000);
        assert_eq!(snaps, 0);
        assert!(worst < 0.004, "error {worst}");
    }

    #[test]
    fn late_video_jumps_forward() {
        let mut clock = LayerClock::start(ShowTime::ZERO, ShowTime::ZERO);
        let mut sync = LayerSync::new(CueId::new(), ShowTime::from_secs(10));
        let now = ShowTime::from_secs(1);
        // Master at 11.5 s, layer anchored at 10 s: should be 1.5 s in, is 1 s.
        assert_eq!(sync.follow(&mut clock, now, MasterClock::Running(ShowTime::from_millis(11_500))), Correction::Snapped);
        assert_eq!(clock.at(now), ShowTime::from_millis(1_500));
        assert_eq!(clock.rate(), 1.0);
        // Late by 0.2 s: snaps too.
        let mut clock = LayerClock::start(ShowTime::ZERO, ShowTime::ZERO);
        assert_eq!(sync.follow(&mut clock, now, MasterClock::Running(ShowTime::from_millis(11_200))), Correction::Snapped);
        assert_eq!(clock.at(now), ShowTime::from_millis(1_200));
    }

    #[test]
    fn early_video_holds_then_releases() {
        let mut clock = LayerClock::start(ShowTime::ZERO, ShowTime::ZERO);
        let mut sync = LayerSync::new(CueId::new(), ShowTime::ZERO);
        let now = ShowTime::from_secs(1);
        // Video 200 ms ahead: hold the frame instead of seeking back.
        assert_eq!(sync.follow(&mut clock, now, MasterClock::Running(ShowTime::from_millis(800))), Correction::Holding);
        let held = clock.at(now);
        assert_eq!(clock.at(now + ShowTime::from_millis(100)), held);
        // Audio catches up to within the slew band: playback resumes.
        let later = now + ShowTime::from_millis(150);
        let c = sync.follow(&mut clock, later, MasterClock::Running(ShowTime::from_millis(950)));
        assert!(matches!(c, Correction::Slewing | Correction::Locked));
        assert!(clock.at(later + FRAME) > held);
        // A full second ahead: jump back.
        assert_eq!(
            sync.follow(&mut clock, later, MasterClock::Running(ShowTime::ZERO + ShowTime::from_millis(1))),
            Correction::Snapped
        );
    }

    #[test]
    fn held_master_pins_and_gone_master_free_runs() {
        let mut clock = LayerClock::start(ShowTime::ZERO, ShowTime::ZERO);
        let mut sync = LayerSync::new(CueId::new(), ShowTime::ZERO);
        // Count-in: the audio clock sits at zero; the video waits on frame 0.
        for i in 1..30 {
            let now = ShowTime::from_micros(i * FRAME.as_micros());
            assert_eq!(sync.follow(&mut clock, now, MasterClock::Held(ShowTime::ZERO)), Correction::Pinned);
            assert_eq!(clock.at(now), ShowTime::ZERO);
        }
        // Audio ends first: the layer carries on at 1× from where it is.
        let now = ShowTime::from_secs(2);
        sync.follow(&mut clock, now, MasterClock::Running(ShowTime::from_secs(1)));
        assert_eq!(sync.follow(&mut clock, now, MasterClock::Gone), Correction::FreeRun);
        let at = clock.at(now);
        assert_eq!(clock.at(now + ShowTime::from_secs(1)), at + ShowTime::from_secs(1));
    }

    #[test]
    fn pause_resume_and_seek_keep_position() {
        let mut c = LayerClock::start(ShowTime::from_secs(5), ShowTime::from_secs(1));
        assert_eq!(c.at(ShowTime::from_secs(7)), ShowTime::from_secs(3));
        c.pause(ShowTime::from_secs(7));
        assert_eq!(c.at(ShowTime::from_secs(9)), ShowTime::from_secs(3));
        c.resume(ShowTime::from_secs(10));
        assert_eq!(c.at(ShowTime::from_secs(11)), ShowTime::from_secs(4));
        c.set_rate(ShowTime::from_secs(11), 1.5);
        assert_eq!(c.at(ShowTime::from_secs(13)), ShowTime::from_secs(7));
        c.pause(ShowTime::from_secs(13));
        c.seek(ShowTime::from_secs(20), ShowTime::from_secs(30));
        assert_eq!(c.at(ShowTime::from_secs(25)), ShowTime::from_secs(30));
    }
}

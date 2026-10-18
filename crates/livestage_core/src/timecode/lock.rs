//! Locking to an external timecode stream.
//!
//! Incoming timecode (MTC quarter frames, Art-Net packets) arrives with
//! jitter from the transport, the OS and the MIDI driver, and the sender's
//! clock drifts against ours. [`TimecodeLock`] turns those samples into a
//! smooth, continuous position on the local clock:
//!
//! * **Lock-in** — a few consistent samples are required before the stream
//!   is declared running, so a single stray packet does not start the show.
//! * **Drift compensation** — a second-order loop (phase + frequency)
//!   tracks the sender: phase errors are absorbed gradually and the
//!   measured speed converges on the sender's real rate, so extrapolation
//!   between samples stays accurate even with a sender running ±0.1 % off.
//! * **Jumps** — an error beyond `jump_frames` is a seek on the sender:
//!   the lock re-anchors immediately and bumps the transport generation.
//! * **Dropouts** — when samples stop the lock *freewheels* at the measured
//!   speed for `freewheel`, then reports the stream stopped.
//! * **Locate** — a stationary position (MTC full-frame message, repeated
//!   Art-Net frame) parks the transport there.

use super::{FrameRate, TcTransport};
use crate::time::ShowTime;
use serde::{Deserialize, Serialize};

/// Phase correction per sample (fraction of the error absorbed).
const KP: f64 = 0.03;
/// Frequency correction per sample. Tuned (with KP) for ±1.5 ms arrival
/// jitter: speed settles within ~100 ppm in a few seconds of MTC.
const KI: f64 = 0.0002;
/// Smoothing of the measured sample interval.
const INTERVAL_SMOOTHING: f64 = 0.05;
/// Largest speed error the loop will follow (±2 %); beyond that is not timecode.
const MAX_SPEED_DEVIATION: f64 = 0.02;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LockState {
    /// No stream (or a parked locate position).
    #[default]
    Stopped,
    /// Samples are arriving; waiting for them to agree.
    Locking,
    /// Following the stream.
    Locked,
    /// Samples stopped; extrapolating at the measured speed.
    Freewheel,
}

impl LockState {
    pub fn label(self) -> &'static str {
        match self {
            LockState::Stopped => "Stopped",
            LockState::Locking => "Locking",
            LockState::Locked => "Locked",
            LockState::Freewheel => "Freewheel",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleKind {
    /// Part of a rolling stream (quarter frame, per-frame packet).
    Running,
    /// A stationary position (MTC full-frame locate, repeated packet).
    Locate,
}

/// One decoded timecode observation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TcSample {
    /// Timecode position the sample represents at the moment it was received.
    pub position: ShowTime,
    pub rate: FrameRate,
    pub kind: SampleKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LockConfig {
    /// No samples for this long means the stream is interrupted.
    pub dropout: ShowTime,
    /// Keep running this long after a dropout before reporting a stop.
    pub freewheel: ShowTime,
    /// An error larger than this many frames is a jump, not jitter.
    pub jump_frames: u32,
    /// Consistent samples needed before declaring lock.
    pub lock_samples: u32,
}

impl Default for LockConfig {
    fn default() -> Self {
        Self { dropout: ShowTime::from_millis(100), freewheel: ShowTime::from_millis(200), jump_frames: 4, lock_samples: 3 }
    }
}

#[derive(Clone, Debug)]
pub struct TimecodeLock {
    pub config: LockConfig,
    state: LockState,
    rate: FrameRate,
    /// Local time (µs) and timecode position (µs) of the loop's reference point.
    anchor_local: f64,
    anchor_tc: f64,
    /// Timecode µs per local µs.
    speed: f64,
    /// Arrival time of the latest sample (µs).
    last_sample: f64,
    /// Smoothed time between samples (µs); 0 until measured.
    interval: f64,
    agreeing: u32,
    /// Position while stopped.
    parked: ShowTime,
    generation: u64,
    last_error_us: f64,
}

impl TimecodeLock {
    pub fn new(config: LockConfig) -> Self {
        Self {
            config,
            state: LockState::Stopped,
            rate: FrameRate::default(),
            anchor_local: 0.0,
            anchor_tc: 0.0,
            speed: 1.0,
            last_sample: 0.0,
            interval: 0.0,
            agreeing: 0,
            parked: ShowTime::ZERO,
            generation: 0,
            last_error_us: 0.0,
        }
    }

    pub fn state(&self) -> LockState {
        self.state
    }
    /// Measured sender speed against the local clock.
    pub fn speed(&self) -> f64 {
        self.speed
    }
    /// Error of the latest sample against the loop's prediction (µs).
    pub fn last_error_us(&self) -> f64 {
        self.last_error_us
    }

    fn predict(&self, local: f64) -> f64 {
        self.anchor_tc + (local - self.anchor_local) * self.speed
    }

    /// Signed error, taking the shorter way around midnight.
    fn wrapped_error(&self, sample: f64, predicted: f64) -> f64 {
        let day = self.rate.day_length().as_micros() as f64;
        let e = (sample - predicted).rem_euclid(day);
        if e > day / 2.0 {
            e - day
        } else {
            e
        }
    }

    fn jump_threshold_us(&self) -> f64 {
        self.rate.frames_to_micros(self.config.jump_frames.max(1) as f64)
    }

    fn anchor(&mut self, local: f64, position: f64) {
        self.anchor_local = local;
        self.anchor_tc = position;
    }

    /// Feeds a sample received at local time `local`.
    pub fn feed(&mut self, local: ShowTime, sample: TcSample) {
        let local_us = local.as_micros() as f64;
        let pos = sample.position.as_micros() as f64;
        if sample.rate != self.rate {
            // A rate change is a different stream: start over.
            self.rate = sample.rate;
            if self.state != LockState::Stopped {
                self.state = LockState::Stopped;
                self.generation += 1;
            }
        }
        if sample.kind == SampleKind::Locate {
            let was_running = self.is_running();
            let moved = (pos - self.parked.as_micros() as f64).abs() >= self.rate.frames_to_micros(0.5);
            self.state = LockState::Stopped;
            self.parked = sample.position;
            self.agreeing = 0;
            self.last_sample = local_us;
            if moved || was_running {
                self.generation += 1;
            }
            return;
        }

        match self.state {
            LockState::Stopped => {
                self.state = LockState::Locking;
                self.speed = 1.0;
                self.interval = 0.0;
                self.agreeing = 1;
                self.anchor(local_us, pos);
            }
            LockState::Locking => {
                let err = self.wrapped_error(pos, self.predict(local_us));
                self.last_error_us = err;
                if err.abs() <= self.jump_threshold_us() {
                    self.agreeing += 1;
                } else {
                    self.agreeing = 1;
                }
                // While locking, follow the samples exactly (no smoothing yet).
                self.anchor(local_us, pos);
                if self.agreeing >= self.config.lock_samples.max(1) {
                    self.state = LockState::Locked;
                    self.generation += 1;
                }
            }
            LockState::Locked | LockState::Freewheel => {
                let predicted = self.predict(local_us);
                let err = self.wrapped_error(pos, predicted);
                self.last_error_us = err;
                if err.abs() > self.jump_threshold_us() {
                    // The sender jumped: follow at once.
                    self.anchor(local_us, pos);
                    self.generation += 1;
                } else {
                    // Frequency term over the *smoothed* interval: dividing by each
                    // jittery interval would bias the speed estimate.
                    let dt = local_us - self.last_sample;
                    if dt > 0.0 {
                        self.interval = if self.interval > 0.0 { self.interval + INTERVAL_SMOOTHING * (dt - self.interval) } else { dt };
                        self.speed = (self.speed + KI * err / self.interval)
                            .clamp(1.0 - MAX_SPEED_DEVIATION, 1.0 + MAX_SPEED_DEVIATION);
                    }
                    self.anchor(local_us, predicted + KP * err);
                }
                self.state = LockState::Locked;
            }
        }
        self.last_sample = local_us;
    }

    /// Advances dropout / freewheel handling to local time `local`.
    pub fn update(&mut self, local: ShowTime) {
        let local_us = local.as_micros() as f64;
        let silent = local_us - self.last_sample;
        let dropout = self.config.dropout.as_micros() as f64;
        match self.state {
            LockState::Locking if silent > dropout => self.state = LockState::Stopped,
            LockState::Locked if silent > dropout => {
                self.state = if self.config.freewheel.is_zero() { self.stop_at(local_us) } else { LockState::Freewheel };
            }
            LockState::Freewheel if silent > dropout + self.config.freewheel.as_micros() as f64 => {
                self.state = self.stop_at(local_us);
            }
            _ => {}
        }
    }

    fn stop_at(&mut self, local_us: f64) -> LockState {
        self.parked = ShowTime::from_micros(self.predict(local_us).max(0.0) as u64);
        LockState::Stopped
    }

    pub fn is_running(&self) -> bool {
        matches!(self.state, LockState::Locked | LockState::Freewheel)
    }

    /// Position at local time `local`.
    pub fn position(&self, local: ShowTime) -> ShowTime {
        if self.is_running() {
            let day = self.rate.day_length().as_micros() as f64;
            ShowTime::from_micros(self.predict(local.as_micros() as f64).rem_euclid(day) as u64)
        } else {
            self.parked
        }
    }

    pub fn transport(&self, local: ShowTime) -> TcTransport {
        TcTransport {
            position: self.position(local),
            rate: self.rate,
            running: self.is_running(),
            generation: self.generation,
            state: self.state,
            speed: self.speed,
        }
    }

    /// Forgets the stream (source change).
    pub fn reset(&mut self) {
        let generation = self.generation + 1;
        *self = Self { generation, ..Self::new(self.config) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic jitter in ±`amp` µs.
    struct Lcg(u64);
    impl Lcg {
        fn jitter(&mut self, amp: f64) -> f64 {
            self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 33) as f64 / (1u64 << 31) as f64 * 2.0 - 1.0) * amp
        }
    }

    fn running(pos_us: f64, rate: FrameRate) -> TcSample {
        TcSample { position: ShowTime::from_micros(pos_us.max(0.0) as u64), rate, kind: SampleKind::Running }
    }

    /// Feeds an MTC-like stream (4 samples per frame) from a sender whose
    /// clock runs at `speed`, with arrival jitter, for `secs` seconds.
    /// Returns the lock and the local time reached.
    fn stream(lock: &mut TimecodeLock, start_tc: f64, speed: f64, jitter_us: f64, secs: f64, t0: f64) -> f64 {
        let rate = FrameRate::Fps25;
        let qf = rate.frames_to_micros(0.25);
        let mut rng = Lcg(7);
        let mut local = t0;
        let mut k = 0.0;
        while local - t0 < secs * 1e6 {
            // Sender time of quarter frame k, as seen on our clock.
            local = t0 + k * qf / speed;
            let arrival = local + jitter_us + rng.jitter(jitter_us);
            lock.feed(ShowTime::from_micros(arrival as u64), running(start_tc + k * qf, rate));
            lock.update(ShowTime::from_micros(arrival as u64));
            k += 1.0;
        }
        local
    }

    #[test]
    fn locks_after_consistent_samples() {
        let mut lock = TimecodeLock::new(LockConfig::default());
        let r = FrameRate::Fps25;
        lock.feed(ShowTime::from_millis(1000), running(3_600e6, r));
        assert_eq!(lock.state(), LockState::Locking);
        assert!(!lock.transport(ShowTime::from_millis(1000)).running);
        lock.feed(ShowTime::from_millis(1010), running(3_600e6 + 10_000.0, r));
        lock.feed(ShowTime::from_millis(1020), running(3_600e6 + 20_000.0, r));
        assert_eq!(lock.state(), LockState::Locked);
        let t = lock.transport(ShowTime::from_millis(1030));
        assert!(t.running);
        assert_eq!(t.position, ShowTime::from_micros(3_600_030_000));
        assert_eq!(t.timecode().to_string(), "01:00:00:00");
    }

    #[test]
    fn compensates_sender_drift() {
        // Sender clock 0.1 % fast (1000 ppm), ±1.5 ms arrival jitter.
        let mut lock = TimecodeLock::new(LockConfig::default());
        let start = 3_600e6;
        let local = stream(&mut lock, start, 1.001, 1_500.0, 30.0, 5e6);
        assert_eq!(lock.state(), LockState::Locked);
        let ppm = (lock.speed() - 1.0) * 1e6;
        assert!((ppm - 1000.0).abs() < 150.0, "speed converged on the sender: {ppm} ppm");
        // Position tracks the sender's true time to well within a frame.
        let truth = start + (local - 5e6) * 1.001;
        let est = lock.position(ShowTime::from_micros((local + 1_500.0) as u64)).as_micros() as f64;
        assert!((est - truth).abs() < 4_000.0, "error {} µs", est - truth);
    }

    #[test]
    fn extrapolation_between_samples_is_smooth() {
        let mut lock = TimecodeLock::new(LockConfig::default());
        let local = stream(&mut lock, 0.0, 0.9995, 800.0, 20.0, 0.0);
        // Positions across the next 8 ms advance monotonically at ~sender speed.
        let a = lock.position(ShowTime::from_micros(local as u64)).as_micros() as f64;
        let b = lock.position(ShowTime::from_micros(local as u64 + 8_000)).as_micros() as f64;
        assert!(((b - a) / 8_000.0 - 0.9995).abs() < 2e-4);
    }

    #[test]
    fn jump_is_followed_immediately() {
        let mut lock = TimecodeLock::new(LockConfig::default());
        let local = stream(&mut lock, 0.0, 1.0, 500.0, 2.0, 0.0);
        let gen = lock.transport(ShowTime::from_micros(local as u64)).generation;
        // Sender locates to 10:00:00:00 and keeps rolling.
        let at = local + 10_000.0;
        lock.feed(ShowTime::from_micros(at as u64), running(36_000e6, FrameRate::Fps25));
        let t = lock.transport(ShowTime::from_micros(at as u64));
        assert!(t.running);
        assert_eq!(t.generation, gen + 1);
        assert_eq!(t.timecode().to_string(), "10:00:00:00");
    }

    #[test]
    fn small_errors_are_smoothed_not_jumped() {
        let mut lock = TimecodeLock::new(LockConfig::default());
        let local = stream(&mut lock, 0.0, 1.0, 0.0, 2.0, 0.0);
        let before = lock.transport(ShowTime::from_micros(local as u64));
        // One sample 1 frame late: inside the jump threshold.
        let at = local + 10_000.0;
        let expected = before.position.as_micros() as f64 + 10_000.0;
        lock.feed(ShowTime::from_micros(at as u64), running(expected - 40_000.0, FrameRate::Fps25));
        let after = lock.transport(ShowTime::from_micros(at as u64));
        assert_eq!(after.generation, before.generation);
        let moved = expected - after.position.as_micros() as f64;
        assert!((moved - KP * 40_000.0).abs() < 1_000.0, "absorbed KP of the error: {moved}");
    }

    #[test]
    fn dropout_freewheels_then_stops() {
        let cfg = LockConfig::default();
        let mut lock = TimecodeLock::new(cfg);
        let local = stream(&mut lock, 0.0, 1.0, 0.0, 1.0, 0.0);
        let last = lock.position(ShowTime::from_micros(local as u64));
        // 150 ms of silence: past the dropout, inside the freewheel.
        let t1 = ShowTime::from_micros(local as u64 + 150_000);
        lock.update(t1);
        assert_eq!(lock.state(), LockState::Freewheel);
        let fw = lock.transport(t1);
        assert!(fw.running);
        assert!(fw.position > last + ShowTime::from_millis(140), "keeps rolling while freewheeling");
        // Past dropout + freewheel: stopped where the extrapolation ended.
        let t2 = ShowTime::from_micros(local as u64 + 400_000);
        lock.update(t2);
        let st = lock.transport(t2);
        assert_eq!(st.state, LockState::Stopped);
        assert!(!st.running);
        assert_eq!(lock.transport(t2 + ShowTime::from_secs(5)).position, st.position, "parked");
        // Samples resuming relock.
        stream(&mut lock, 2e6, 1.0, 0.0, 0.2, local + 500_000.0);
        assert_eq!(lock.state(), LockState::Locked);
    }

    #[test]
    fn locate_parks_and_bumps_generation() {
        let mut lock = TimecodeLock::new(LockConfig::default());
        let r = FrameRate::Fps30;
        let locate = |us: u64| TcSample { position: ShowTime::from_micros(us), rate: r, kind: SampleKind::Locate };
        lock.feed(ShowTime::from_secs(1), locate(7_200_000_000));
        let t = lock.transport(ShowTime::from_secs(3));
        assert!(!t.running);
        assert_eq!(t.timecode().to_string(), "02:00:00:00");
        let g = t.generation;
        lock.feed(ShowTime::from_secs(4), locate(7_200_000_000));
        assert_eq!(lock.transport(ShowTime::from_secs(4)).generation, g, "same position: no event");
        lock.feed(ShowTime::from_secs(5), locate(7_260_000_000));
        assert_eq!(lock.transport(ShowTime::from_secs(5)).generation, g + 1);
    }

    #[test]
    fn midnight_wrap_is_not_a_jump() {
        let mut lock = TimecodeLock::new(LockConfig::default());
        let day = FrameRate::Fps25.day_length().as_micros() as f64;
        let local = stream(&mut lock, day - 1e6, 1.0, 0.0, 0.5, 0.0);
        let g = lock.transport(ShowTime::from_micros(local as u64)).generation;
        // Continue across 00:00:00:00 (positions restart from zero).
        let mut l = local;
        for k in 1..200 {
            l = local + k as f64 * 10_000.0;
            let truth = (day - 1e6 + l).rem_euclid(day);
            lock.feed(ShowTime::from_micros(l as u64), running(truth, FrameRate::Fps25));
        }
        let t = lock.transport(ShowTime::from_micros(l as u64));
        assert_eq!(t.generation, g);
        assert!(t.position < ShowTime::from_secs(2), "wrapped to the new day: {:?}", t.position);
    }
}

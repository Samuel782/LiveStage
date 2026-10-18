//! Source selection: one place that turns the configured timecode source
//! (internal generator, MTC, Art-Net) into the show's [`TcTransport`].

use super::{ClockBase, FrameRate, InternalClock, LockConfig, TcSample, TcTransport, Timecode, TimecodeLock};
use crate::time::ShowTime;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimecodeSource {
    /// LiveStage generates timecode itself.
    #[default]
    Internal,
    /// MIDI Time Code (quarter frames + full-frame locates).
    Mtc,
    /// Art-Net `OpTimeCode` packets.
    ArtNet,
}

impl TimecodeSource {
    pub const ALL: [TimecodeSource; 3] = [TimecodeSource::Internal, TimecodeSource::Mtc, TimecodeSource::ArtNet];
    pub fn label(self) -> &'static str {
        match self {
            TimecodeSource::Internal => "Internal",
            TimecodeSource::Mtc => "MIDI Time Code",
            TimecodeSource::ArtNet => "Art-Net Timecode",
        }
    }
    pub fn short(self) -> &'static str {
        match self {
            TimecodeSource::Internal => "INT",
            TimecodeSource::Mtc => "MTC",
            TimecodeSource::ArtNet => "ART",
        }
    }
}

/// Show-level timecode settings (saved in the show file).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TimecodeConfig {
    pub source: TimecodeSource,
    /// Rate of the internal generator. External streams carry their own.
    pub rate: FrameRate,
    pub internal_base: ClockBase,
    /// Where the internal generator is parked on load and on Reset.
    pub internal_start: Timecode,
    /// MIDI input carrying MTC (None = first available).
    pub mtc_input: Option<String>,
    /// UDP port for Art-Net timecode.
    pub artnet_port: u16,
    pub lock: LockConfig,
}

impl Default for TimecodeConfig {
    fn default() -> Self {
        Self {
            source: TimecodeSource::Internal,
            rate: FrameRate::Fps25,
            internal_base: ClockBase::Audio,
            internal_start: Timecode::new(1, 0, 0, 0, FrameRate::Fps25).unwrap_or_default(),
            mtc_input: None,
            artnet_port: 6454,
            lock: LockConfig::default(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct TimecodeEngine {
    config: TimecodeConfig,
    lock: TimecodeLock,
    internal: InternalClock,
    /// Bumped on source changes so followers always see a discontinuity.
    switches: u64,
    transport: TcTransport,
}

impl TimecodeEngine {
    pub fn new(config: TimecodeConfig) -> Self {
        let mut internal = InternalClock::new(config.rate);
        internal.locate(config.internal_start.with_rate(config.rate).to_time());
        let lock = TimecodeLock::new(config.lock);
        let mut e = Self { config, lock, internal, switches: 0, transport: TcTransport::default() };
        e.transport = e.current(ShowTime::ZERO);
        e
    }

    pub fn config(&self) -> &TimecodeConfig {
        &self.config
    }

    /// Applies edited settings; a new source restarts locking.
    pub fn set_config(&mut self, config: TimecodeConfig) {
        if config.source != self.config.source {
            self.lock.reset();
            self.switches += 1;
        }
        if config.rate != self.internal.rate {
            // Keep the label on screen, at the new rate.
            let label = Timecode::from_time(self.internal.position(), self.internal.rate).with_rate(config.rate);
            self.internal.rate = config.rate;
            self.internal.locate(label.to_time());
        }
        self.lock.config = config.lock;
        self.config = config;
    }

    /// Feeds a sample from `origin`, received at local time `local`.
    /// Samples from sources other than the configured one are ignored.
    pub fn feed(&mut self, local: ShowTime, origin: super::TimecodeSource, sample: TcSample) {
        if origin == self.config.source && origin != TimecodeSource::Internal {
            self.lock.feed(local, sample);
        }
    }

    /// Advances the engine; `audio_clock` is the audio device sample clock
    /// (None when no audio device is running).
    pub fn update(&mut self, local: ShowTime, audio_clock: Option<ShowTime>) -> TcTransport {
        match self.config.source {
            TimecodeSource::Internal => match (self.config.internal_base, audio_clock) {
                (ClockBase::Audio, Some(a)) => self.internal.advance(ClockBase::Audio, a),
                _ => self.internal.advance(ClockBase::Wall, local),
            },
            _ => self.lock.update(local),
        }
        self.transport = self.current(local);
        self.transport
    }

    fn current(&self, local: ShowTime) -> TcTransport {
        let mut t = match self.config.source {
            TimecodeSource::Internal => self.internal.transport(),
            _ => self.lock.transport(local),
        };
        t.generation = t.generation.wrapping_add(self.switches << 32);
        t
    }

    /// The transport as of the last [`TimecodeEngine::update`].
    pub fn transport(&self) -> TcTransport {
        self.transport
    }

    /// Error of the latest external sample against the lock's prediction (µs).
    pub fn lock_error_us(&self) -> f64 {
        self.lock.last_error_us()
    }

    /// Internal generator transport controls. Return false for external
    /// sources, which only the sender controls.
    pub fn start(&mut self) -> bool {
        self.internal_only(|c| c.start())
    }
    pub fn stop(&mut self) -> bool {
        self.internal_only(|c| c.stop())
    }
    pub fn locate(&mut self, tc: Timecode) -> bool {
        let rate = self.internal.rate;
        self.internal_only(|c| c.locate(tc.with_rate(rate).to_time()))
    }
    /// Parks the generator at the configured start.
    pub fn reset(&mut self) -> bool {
        let start = self.config.internal_start;
        self.internal_only(|c| {
            c.stop();
            c.locate(start.with_rate(c.rate).to_time());
        })
    }

    fn internal_only(&mut self, f: impl FnOnce(&mut InternalClock)) -> bool {
        if self.config.source != TimecodeSource::Internal {
            return false;
        }
        f(&mut self.internal);
        self.transport = self.current(ShowTime::ZERO);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timecode::SampleKind;

    #[test]
    fn internal_generator_follows_audio_clock_when_available() {
        let mut e = TimecodeEngine::new(TimecodeConfig::default());
        assert_eq!(e.transport().timecode().to_string(), "01:00:00:00");
        assert!(e.start());
        e.update(ShowTime::from_secs(50), Some(ShowTime::from_secs(2)));
        // Wall clock jumps 10 s, the audio clock only 1 s: audio wins.
        let t = e.update(ShowTime::from_secs(60), Some(ShowTime::from_secs(3)));
        assert_eq!(t.timecode().to_string(), "01:00:01:00");
        // Audio device gone: falls back to the wall clock without a jump.
        e.update(ShowTime::from_secs(61), None);
        let t = e.update(ShowTime::from_secs(62), None);
        assert_eq!(t.timecode().to_string(), "01:00:02:00");
    }

    #[test]
    fn external_source_ignores_other_origins_and_internal_controls() {
        let cfg = TimecodeConfig { source: TimecodeSource::ArtNet, ..TimecodeConfig::default() };
        let mut e = TimecodeEngine::new(cfg);
        assert!(!e.start(), "internal controls are refused for external sources");
        let s = |us: u64| TcSample { position: ShowTime::from_micros(us), rate: FrameRate::Fps25, kind: SampleKind::Running };
        for k in 0..5u64 {
            e.feed(ShowTime::from_millis(40 * k), TimecodeSource::Mtc, s(40_000 * k));
        }
        assert!(!e.update(ShowTime::from_millis(200), None).running, "MTC samples ignored");
        for k in 0..5u64 {
            e.feed(ShowTime::from_millis(1000 + 40 * k), TimecodeSource::ArtNet, s(3_600_000_000 + 40_000 * k));
        }
        let t = e.update(ShowTime::from_millis(1160), None);
        assert!(t.running);
        assert_eq!(t.timecode().to_string(), "01:00:00:04");
    }

    #[test]
    fn source_change_is_a_discontinuity() {
        let mut e = TimecodeEngine::new(TimecodeConfig::default());
        let g = e.update(ShowTime::ZERO, None).generation;
        e.set_config(TimecodeConfig { source: TimecodeSource::Mtc, ..TimecodeConfig::default() });
        assert_ne!(e.update(ShowTime::ZERO, None).generation, g);
    }

    #[test]
    fn internal_rate_change_keeps_label() {
        let mut e = TimecodeEngine::new(TimecodeConfig::default());
        e.locate("02:00:00:12".parse().unwrap());
        e.set_config(TimecodeConfig { rate: FrameRate::Fps2997Df, ..TimecodeConfig::default() });
        let t = e.update(ShowTime::ZERO, None);
        assert_eq!(t.rate, FrameRate::Fps2997Df);
        assert_eq!(t.timecode().to_string(), "02:00:00;12");
    }
}

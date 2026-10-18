//! Sample-accurate click-track generator (allocation-free).

use livestage_core::action::ClickTrack;

#[derive(Debug, Clone)]
pub struct Metronome {
    pub bpm: f64,
    pub beats_per_bar: u32,
    pub gain: f32,
    sample_rate: f64,
    /// Fractional frame counter since start.
    frame: u64,
    env: f32,
    phase: f32,
    freq: f32,
    decay: f32,
}

impl Metronome {
    pub fn new(cfg: &ClickTrack, sample_rate: u32) -> Self {
        let sr = sample_rate.max(1) as f64;
        Self {
            bpm: cfg.bpm.clamp(20.0, 400.0),
            beats_per_bar: cfg.beats_per_bar.max(1),
            gain: crate::db_to_gain(cfg.gain_db),
            sample_rate: sr,
            frame: 0,
            env: 0.0,
            phase: 0.0,
            freq: 1000.0,
            // ~25 ms exponential decay to -60 dB.
            decay: (-6.9 / (0.025 * sr)).exp() as f32,
        }
    }

    pub fn frames_per_beat(&self) -> f64 {
        self.sample_rate * 60.0 / self.bpm
    }

    /// Frames of count-in for `bars` bars.
    pub fn count_in_frames(&self, bars: u32) -> u64 {
        (self.frames_per_beat() * (bars * self.beats_per_bar) as f64).round() as u64
    }

    pub fn seek(&mut self, frame: u64) {
        self.frame = frame;
        self.env = 0.0;
    }

    /// Produces the next mono click sample.
    #[inline]
    pub fn next_sample(&mut self) -> f32 {
        let fpb = self.frames_per_beat();
        // A beat starts on the first frame whose position crosses a beat boundary.
        let beat_now = (self.frame as f64 / fpb).floor() as u64;
        let beat_prev = if self.frame == 0 { u64::MAX } else { ((self.frame - 1) as f64 / fpb).floor() as u64 };
        if beat_now != beat_prev {
            let accent = beat_now.is_multiple_of(self.beats_per_bar as u64);
            self.freq = if accent { 1_600.0 } else { 1_000.0 };
            self.env = if accent { 1.0 } else { 0.7 };
            self.phase = 0.0;
        }
        self.frame += 1;
        if self.env < 1e-4 {
            return 0.0;
        }
        let s = (self.phase * std::f32::consts::TAU).sin() * self.env * self.gain;
        self.phase = (self.phase + self.freq / self.sample_rate as f32).fract();
        self.env *= self.decay;
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use livestage_core::id::StemId;

    #[test]
    fn clicks_land_on_beats() {
        let cfg = ClickTrack { bpm: 120.0, beats_per_bar: 4, stem: StemId::new(), gain_db: 0.0, count_in_bars: 1 };
        let mut m = Metronome::new(&cfg, 48_000);
        assert_eq!(m.frames_per_beat(), 24_000.0);
        assert_eq!(m.count_in_frames(1), 96_000);
        let samples: Vec<f32> = (0..48_000).map(|_| m.next_sample()).collect();
        // Energy right after each beat, silence just before the next one.
        assert!(samples[1..200].iter().any(|s| s.abs() > 0.1));
        assert!(samples[23_000..24_000].iter().all(|s| s.abs() < 1e-3));
        assert!(samples[24_001..24_200].iter().any(|s| s.abs() > 0.1));
    }
}

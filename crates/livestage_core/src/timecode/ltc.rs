//! Linear (longitudinal) timecode, SMPTE 12M: one 80-bit frame per video
//! frame, biphase-mark coded on an audio signal.
//!
//! Every bit starts with a level change, and a `1` has a second change in
//! its middle, so the signal carries its own clock and is independent of
//! polarity. Bits 64–79 are the sync word `0011 1111 1111 1101`, which
//! marks the end of a frame and its direction.
//!
//! Both halves are allocation-free so they can run on audio threads:
//! [`LtcEncoder`] in the output callback, [`LtcDecoder`] in an input
//! callback.

use super::{FrameRate, Timecode};

/// Bits 64..=79, transmission order read as a little-endian `u16`.
const SYNC_WORD: u16 = 0xBFFC;
const FRAME_BITS: u32 = 80;

/// The 80 bits of a frame, bit 0 (sent first) in the lowest position.
pub fn frame_bits(tc: &Timecode) -> u128 {
    let mut b: u128 = 0;
    let mut put = |value: u8, at: u32, width: u32| b |= ((value as u128) & ((1 << width) - 1)) << at;
    put(tc.frames % 10, 0, 4);
    put(tc.frames / 10, 8, 2);
    put(tc.rate.is_drop_frame() as u8, 10, 1);
    put(tc.seconds % 10, 16, 4);
    put(tc.seconds / 10, 24, 3);
    put(tc.minutes % 10, 32, 4);
    put(tc.minutes / 10, 40, 3);
    put(tc.hours % 10, 48, 4);
    put(tc.hours / 10, 56, 2);
    b |= (SYNC_WORD as u128) << 64;
    // Polarity correction: an even number of zeros per frame, so every
    // frame starts on the same edge (bit 59 at 25 fps, bit 27 otherwise).
    let zeros = FRAME_BITS - b.count_ones();
    if zeros % 2 == 1 {
        b |= 1 << if tc.rate == FrameRate::Fps25 { 59 } else { 27 };
    }
    b
}

/// The label in a frame, at `rate` (the drop-frame flag picks 29.97 DF).
pub fn parse_bits(b: u128, rate: FrameRate) -> Option<Timecode> {
    if ((b >> 64) & 0xFFFF) as u16 != SYNC_WORD {
        return None;
    }
    let get = |at: u32, width: u32| ((b >> at) as u8) & ((1u8 << width) - 1);
    let rate = if get(10, 1) == 1 { FrameRate::Fps2997Df } else if rate == FrameRate::Fps2997Df { FrameRate::Fps2997 } else { rate };
    let (f, s, m, h) = (get(0, 4) + 10 * get(8, 2), get(16, 4) + 10 * get(24, 3), get(32, 4) + 10 * get(40, 3), get(48, 4) + 10 * get(56, 2));
    Timecode::new(h, m, s, f, rate).ok()
}

/// The lock sample for a frame read when its last bit arrived: that moment
/// is the start of the next frame.
pub fn frame_end_sample(tc: &Timecode) -> super::TcSample {
    let rate = tc.rate;
    let next = (tc.to_frames() + 1) % rate.frames_per_day();
    super::TcSample { position: rate.frame_start(next), rate, kind: super::SampleKind::Running }
}

// ------------------------------------------------------------- encoder --

/// Biphase-mark encoder producing ±1 samples, one frame at a time.
#[derive(Clone, Debug)]
pub struct LtcEncoder {
    sample_rate: f64,
    bits: u128,
    /// Bit being sent; 80 when the frame is complete.
    bit: u32,
    /// Position inside the bit, 0..1 (carried across frames).
    phase: f64,
    /// Bits per output sample.
    step: f64,
    level: f32,
}

impl LtcEncoder {
    pub fn new(sample_rate: u32) -> Self {
        Self { sample_rate: sample_rate.max(1) as f64, bits: 0, bit: FRAME_BITS, phase: 0.0, step: 0.0, level: 1.0 }
    }

    /// Starts sending `tc` at `speed` × its frame rate. The bit phase carries
    /// over from the previous frame, so consecutive frames keep exact time.
    pub fn begin_frame(&mut self, tc: &Timecode, speed: f64) {
        self.bits = frame_bits(tc);
        self.bit = 0;
        self.step = FRAME_BITS as f64 * tc.rate.fps() * speed.clamp(0.5, 2.0) / self.sample_rate;
        self.level = -self.level; // the edge that starts bit 0
    }

    /// Forgets the phase (after a stop): the next frame starts afresh.
    pub fn reset(&mut self) {
        self.bit = FRAME_BITS;
        self.phase = 0.0;
    }

    pub fn frame_done(&self) -> bool {
        self.bit >= FRAME_BITS
    }

    /// The next sample of the current frame; `None` once it is complete.
    pub fn next_sample(&mut self) -> Option<f32> {
        if self.frame_done() {
            return None;
        }
        let out = self.level;
        let before = self.phase;
        self.phase += self.step;
        if (self.bits >> self.bit) & 1 == 1 && before < 0.5 && self.phase >= 0.5 {
            self.level = -self.level;
        }
        if self.phase >= 1.0 {
            self.phase -= 1.0;
            self.bit += 1;
            if self.bit < FRAME_BITS {
                self.level = -self.level;
                if (self.bits >> self.bit) & 1 == 1 && self.phase >= 0.5 {
                    self.level = -self.level;
                }
            }
        }
        Some(out)
    }
}

// ------------------------------------------------------------- decoder --

/// A decoded frame: its label and the sample index where its last bit ended
/// (the position there is the label's start plus one frame).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LtcFrame {
    pub timecode: Timecode,
    pub end_sample: u64,
}

/// Biphase-mark decoder for a mono sample stream (24–30 fps, forward play).
#[derive(Clone, Debug)]
pub struct LtcDecoder {
    sample_rate: f64,
    /// Samples since the stream started.
    sample: u64,
    /// Signal envelope, for a level-independent crossing threshold.
    envelope: f32,
    high: bool,
    /// Sample (fractional) of the last level change.
    last_edge: f64,
    /// Samples per bit, tracked.
    bit_len: f64,
    /// A half-bit seen, waiting for its partner (a `1`).
    half: Option<f64>,
    bits: u128,
    count: u32,
    /// End sample of the previous frame, and the averaged frame length.
    last_frame_end: Option<u64>,
    frame_len: f64,
    /// Frame lengths averaged into `frame_len`.
    measured: u32,
    /// The 30 fps family is running 0.1 % slow (29.97 non-drop).
    ntsc: bool,
}

impl LtcDecoder {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate.max(1) as f64;
        Self {
            sample_rate: sr,
            sample: 0,
            envelope: 0.0,
            high: false,
            last_edge: 0.0,
            bit_len: sr / (FRAME_BITS as f64 * 25.0),
            half: None,
            bits: 0,
            count: 0,
            last_frame_end: None,
            frame_len: 0.0,
            measured: 0,
            ntsc: false,
        }
    }

    /// A decoder that assumes 29.97 rather than 30 fps until it has measured
    /// enough frames to tell, when the show is set up for 29.97.
    pub fn with_rate_hint(sample_rate: u32, hint: FrameRate) -> Self {
        Self { ntsc: hint == FrameRate::Fps2997, ..Self::new(sample_rate) }
    }

    /// Feeds one sample; returns a frame when its sync word completes.
    pub fn push(&mut self, x: f32) -> Option<LtcFrame> {
        let n = self.sample;
        self.sample += 1;
        self.envelope = (self.envelope * 0.9995).max(x.abs());
        let threshold = self.envelope * 0.25;
        let flipped = if self.high { x < -threshold } else { x > threshold };
        if !flipped || self.envelope < 1e-3 {
            return None;
        }
        self.high = !self.high;
        let edge = n as f64;
        let interval = edge - self.last_edge;
        self.last_edge = edge;
        self.edge(interval, n)
    }

    fn edge(&mut self, interval: f64, n: u64) -> Option<LtcFrame> {
        // Bit lengths of 24–30 fps, with margin for varispeed.
        let (min_bit, max_bit) = (self.sample_rate / (FRAME_BITS as f64 * 36.0), self.sample_rate / (FRAME_BITS as f64 * 20.0));
        if interval < min_bit * 0.4 || interval > max_bit * 1.3 {
            self.resync();
            return None;
        }
        let bit = if interval > self.bit_len * 0.75 {
            self.half = None; // a pending half-bit here was a misread
            self.bit_len += (interval - self.bit_len) * 0.05;
            false
        } else if let Some(first) = self.half.take() {
            self.bit_len += (first + interval - self.bit_len) * 0.05;
            true
        } else {
            self.half = Some(interval);
            return None;
        };
        self.bit_len = self.bit_len.clamp(min_bit, max_bit);
        self.bits = (self.bits >> 1) | ((bit as u128) << (FRAME_BITS - 1));
        self.count = self.count.saturating_add(1);
        if self.count < FRAME_BITS || ((self.bits >> 64) & 0xFFFF) as u16 != SYNC_WORD {
            return None;
        }
        self.count = 0;
        let rate = self.rate(n);
        parse_bits(self.bits, rate).map(|timecode| LtcFrame { timecode, end_sample: n })
    }

    fn resync(&mut self) {
        self.half = None;
        self.count = 0;
    }

    /// Frame rate from the averaged frame length; the drop-frame flag in the
    /// frame itself tells 29.97 DF apart. 29.97 and 30 differ by 0.1 %, so
    /// that choice waits for a second of frames and has hysteresis: the
    /// reported rate never flickers mid-stream.
    fn rate(&mut self, end: u64) -> FrameRate {
        if let Some(prev) = self.last_frame_end {
            let len = (end - prev) as f64;
            let expected = self.bit_len * FRAME_BITS as f64;
            if (len - expected).abs() < expected * 0.2 {
                self.measured = self.measured.saturating_add(1);
                let alpha = (1.0 / self.measured as f64).max(0.02);
                self.frame_len += (len - self.frame_len) * alpha;
            } else {
                self.measured = 0; // a jump or a gap: measure afresh
                self.frame_len = 0.0;
            }
        }
        self.last_frame_end = Some(end);
        let fps = if self.measured > 0 { self.sample_rate / self.frame_len } else { self.sample_rate / (self.bit_len * FRAME_BITS as f64) };
        if fps < 24.5 {
            FrameRate::Fps24
        } else if fps < 27.5 {
            FrameRate::Fps25
        } else {
            if self.measured >= 30 {
                if fps < 29.982 {
                    self.ntsc = true;
                } else if fps > 29.988 {
                    self.ntsc = false;
                }
            }
            if self.ntsc {
                FrameRate::Fps2997
            } else {
                FrameRate::Fps30
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tc(s: &str, rate: FrameRate) -> Timecode {
        Timecode::parse_with(s, rate).unwrap().with_rate(rate)
    }

    /// Encodes `frames` consecutive frames from `start` at `sr`.
    fn encode(start: Timecode, frames: u64, sr: u32) -> Vec<f32> {
        let mut e = LtcEncoder::new(sr);
        let mut out = Vec::new();
        for k in 0..frames {
            e.begin_frame(&start.add_frames(k as i64), 1.0);
            while let Some(s) = e.next_sample() {
                out.push(s);
            }
        }
        out
    }

    #[test]
    fn bit_layout_and_polarity() {
        let t = tc("12:34:56:23", FrameRate::Fps25);
        let b = frame_bits(&t);
        assert_eq!(parse_bits(b, FrameRate::Fps25), Some(t));
        assert_eq!((FRAME_BITS - b.count_ones()) % 2, 0, "even number of zeros");
        assert_eq!(b & 0xF, 3);
        assert_eq!((b >> 8) & 0x3, 2);
        let df = tc("01:00:00;02", FrameRate::Fps2997Df);
        assert_eq!(parse_bits(frame_bits(&df), FrameRate::Fps30), Some(df), "the DF flag selects 29.97 DF");
    }

    #[test]
    fn decodes_its_own_encoding_at_every_rate() {
        for rate in FrameRate::ALL {
            for sr in [44_100, 48_000, 96_000] {
                let start = tc("09:59:58:00", rate);
                let signal = encode(start, 150, sr);
                let mut d = LtcDecoder::new(sr);
                let frames: Vec<LtcFrame> = signal.iter().filter_map(|&x| d.push(x * 0.3)).collect();
                // The first frame or two pass while the decoder locks on.
                // A bit is read at the edge that ends it, which for a frame's
                // last bit is the next frame's first edge: the final frame of
                // a finite signal is never completed.
                assert!(frames.len() >= 146, "{rate:?} @ {sr}: {} frames", frames.len());
                let last = frames.last().unwrap();
                assert_eq!(last.timecode, start.add_frames(148), "{rate:?} @ {sr}");
                for w in frames.windows(2) {
                    assert_eq!(w[0].timecode.to_frames() + 1, w[1].timecode.to_frames(), "{rate:?} @ {sr}: consecutive labels");
                }
                assert!(frames[40..].iter().all(|f| f.timecode.rate == rate), "{rate:?} @ {sr}: rate settled");
                // Frames end exactly one frame of samples apart.
                let per_frame = sr as f64 / rate.fps();
                let span = (last.end_sample - frames[0].end_sample) as f64 / (frames.len() - 1) as f64;
                assert!((span - per_frame).abs() < 1.0, "{rate:?} @ {sr}: {span} vs {per_frame}");
            }
        }
    }

    #[test]
    fn survives_inversion_noise_dc_and_dropouts() {
        let sr = 48_000;
        let start = tc("00:10:00:00", FrameRate::Fps25);
        let mut signal = encode(start, 100, sr);
        // Inverted, offset, noisy, low level; then a gap of silence.
        let mut seed = 1u32;
        for s in &mut signal {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 9) as f32 / (1 << 23) as f32 - 0.5;
            *s = -*s * 0.05 + 0.01 + noise * 0.01;
        }
        let gap = vec![0.0; sr as usize / 2];
        let resumed = encode(start.add_frames(200), 50, sr);
        let mut d = LtcDecoder::new(sr);
        let frames: Vec<_> = signal.iter().chain(&gap).chain(&resumed).filter_map(|&x| d.push(x)).collect();
        assert!(frames.iter().filter(|f| f.timecode.to_frames() < start.add_frames(100).to_frames()).count() >= 97);
        assert_eq!(frames.last().unwrap().timecode, start.add_frames(248), "picks up again after the gap");
    }

    #[test]
    fn tells_2997_from_30_by_frame_length() {
        let sr = 48_000;
        for rate in [FrameRate::Fps2997, FrameRate::Fps30] {
            let start = tc("01:00:00:00", rate);
            let mut d = LtcDecoder::new(sr);
            let frames: Vec<_> = encode(start, 300, sr).iter().filter_map(|&x| d.push(x)).collect();
            assert_eq!(frames.last().unwrap().timecode.rate, rate);
        }
    }
}

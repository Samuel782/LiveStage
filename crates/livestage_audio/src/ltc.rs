//! Linear timecode on the audio device: an LTC generator mixed into one
//! output channel by the renderer, and an LTC reader on an audio input.
//!
//! ## Generator
//!
//! The show thread sends an [`LtcAnchor`]: "at device frame `at_frame` the
//! show timecode was `position`, moving at `speed`". The renderer
//! extrapolates the position for every sample, starts each LTC frame on the
//! timecode frame grid, labels it with the frame it starts, and re-aligns
//! after a locate. Frames follow each other with exact bit timing, so
//! anchors that agree with each other (the internal generator runs on this
//! same sample clock) never disturb the signal. It is added after the
//! master fader: panics and master fades do not silence timecode.
//!
//! ## Reader
//!
//! [`LtcInput`] opens an input device and decodes one channel in the input
//! callback (allocation-free); decoded frames wait in a lock-free queue,
//! stamped with the time their last bit arrived, for the show thread.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use livestage_core::timecode::ltc::{LtcDecoder, LtcEncoder};
use livestage_core::timecode::{FrameRate, Timecode};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::{HeapCons, HeapRb};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

// ------------------------------------------------------------- generator --

/// Where the show timecode is, in device frames, for the LTC generator.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LtcAnchor {
    /// Output channel (0-based); `None` switches the generator off.
    pub channel: Option<usize>,
    /// Linear gain.
    pub gain: f32,
    pub rate: FrameRate,
    pub running: bool,
    /// Device frame at which the timecode position was `position_us`.
    pub at_frame: u64,
    /// Real time since `00:00:00:00`, µs.
    pub position_us: f64,
    /// Timecode speed against the device clock (1.0 for the internal generator).
    pub speed: f64,
}

impl LtcAnchor {
    pub const OFF: LtcAnchor =
        LtcAnchor { channel: None, gain: 0.0, rate: FrameRate::Fps25, running: false, at_frame: 0, position_us: 0.0, speed: 1.0 };
}

/// LTC generator state on the audio thread.
pub(crate) struct LtcGenerator {
    enc: LtcEncoder,
    anchor: LtcAnchor,
    sample_rate: f64,
    /// Frames are being sent on the frame grid.
    aligned: bool,
    /// One-pole low-pass state: a rise time of some 25 µs, as SMPTE 12M asks.
    smooth: f32,
    alpha: f32,
}

impl LtcGenerator {
    pub(crate) fn new(sample_rate: u32) -> Self {
        let sr = sample_rate.max(1) as f64;
        let alpha = 1.0 - (-1.0 / (12e-6 * sr)).exp();
        Self { enc: LtcEncoder::new(sample_rate), anchor: LtcAnchor::OFF, sample_rate: sr, aligned: false, smooth: 0.0, alpha: alpha as f32 }
    }

    pub(crate) fn set(&mut self, a: LtcAnchor) {
        let old = self.anchor;
        if !a.running || a.channel != old.channel || a.rate != old.rate {
            self.enc.reset();
            self.aligned = false;
        }
        self.anchor = a;
    }

    /// Timecode position (in frames) at device frame `n`.
    fn frames_at(&self, n: u64) -> f64 {
        let a = &self.anchor;
        let us = a.position_us + (n as f64 - a.at_frame as f64) / self.sample_rate * 1e6 * a.speed;
        us * a.rate.fps() / 1e6
    }

    /// Adds the signal to interleaved `out` (`ch` channels), whose first
    /// frame is device frame `first`.
    pub(crate) fn render(&mut self, out: &mut [f32], ch: usize, first: u64) {
        let a = self.anchor;
        let Some(c) = a.channel.filter(|c| *c < ch) else { return };
        if !a.running {
            return;
        }
        let per_sample = a.rate.fps() * a.speed / self.sample_rate; // frames per sample
        let day = a.rate.frames_per_day() as f64;
        for (i, frame) in out.chunks_exact_mut(ch).enumerate() {
            if self.enc.frame_done() {
                let pos = self.frames_at(first + i as u64);
                let nearest = pos.round();
                let off = pos - nearest;
                if !self.aligned {
                    // Wait for the next frame boundary (within one sample).
                    if !(0.0..per_sample).contains(&off) {
                        continue;
                    }
                    self.aligned = true;
                } else if off.abs() > 0.25 {
                    // The anchor moved (a locate or a new speed): re-align.
                    self.aligned = false;
                    self.enc.reset();
                    continue;
                }
                let label = nearest.rem_euclid(day) as u64;
                self.enc.begin_frame(&Timecode::from_frames(label, a.rate), a.speed);
            }
            if let Some(s) = self.enc.next_sample() {
                self.smooth += (s - self.smooth) * self.alpha;
                frame[c] = (frame[c] + self.smooth * a.gain).clamp(-1.0, 1.0);
            }
        }
    }
}

// ---------------------------------------------------------------- reader --

/// A decoded LTC frame and when its last bit arrived.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LtcReading {
    pub timecode: Timecode,
    pub at: Instant,
}

const READINGS: usize = 256;

/// Audio input devices (name, input channels).
pub fn list_input_devices() -> Vec<(String, u16)> {
    let host = cpal::default_host();
    let Ok(devices) = host.input_devices() else { return Vec::new() };
    devices
        .filter_map(|d| {
            let name = d.name().ok()?;
            let ch = d.supported_input_configs().ok()?.map(|c| c.channels()).max().unwrap_or(0);
            Some((name, ch))
        })
        .collect()
}

/// An open LTC input. Dropping it closes the device.
pub struct LtcInput {
    readings: HeapCons<LtcReading>,
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    decoded: Arc<AtomicU64>,
    pub device_name: String,
    pub channel: u16,
}

impl LtcInput {
    /// Opens `device` (None = default input) and decodes channel `channel`
    /// (1-based). `hint` is the expected rate (29.97 vs 30 needs time to tell).
    pub fn open(device: Option<&str>, channel: u16, hint: FrameRate) -> crate::AudioResult<Self> {
        let (tx, readings) = HeapRb::<LtcReading>::new(READINGS).split();
        let decoded = Arc::new(AtomicU64::new(0));
        let (info_tx, info_rx) = mpsc::channel::<crate::AudioResult<(String, u16)>>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let want = device.map(str::to_string);
        let count = decoded.clone();
        // cpal streams are !Send: the stream lives on its own thread.
        let thread = std::thread::Builder::new().name("livestage-ltc-in".into()).spawn(move || {
            let stream = match open_stream(want.as_deref(), channel, hint, tx, count) {
                Ok((stream, name, ch)) => {
                    let _ = info_tx.send(Ok((name, ch)));
                    stream
                }
                Err(e) => return drop(info_tx.send(Err(e))),
            };
            let _ = stop_rx.recv();
            drop(stream);
        })?;
        let (device_name, channel) = info_rx.recv().map_err(|_| crate::AudioError::Device("LTC input thread exited".into()))??;
        tracing::info!(device = %device_name, channel, "LTC input open");
        Ok(Self { readings, stop: Some(stop_tx), thread: Some(thread), decoded, device_name, channel })
    }

    /// Frames decoded since the last call, oldest first.
    pub fn drain(&mut self) -> impl Iterator<Item = LtcReading> + '_ {
        self.readings.pop_iter()
    }

    /// Frames decoded since the input opened.
    pub fn frames_decoded(&self) -> u64 {
        self.decoded.load(Ordering::Relaxed)
    }
}

impl Drop for LtcInput {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

type Opened = (cpal::Stream, String, u16);

fn open_stream(
    want: Option<&str>,
    channel: u16,
    hint: FrameRate,
    tx: ringbuf::HeapProd<LtcReading>,
    count: Arc<AtomicU64>,
) -> crate::AudioResult<Opened> {
    let host = cpal::default_host();
    let device = match want {
        Some(w) => host
            .input_devices()
            .map_err(|e| crate::AudioError::Device(e.to_string()))?
            .find(|d| d.name().is_ok_and(|n| n == w))
            .ok_or_else(|| crate::AudioError::Device(format!("no audio input named {w}")))?,
        None => host.default_input_device().ok_or_else(|| crate::AudioError::Device("no audio input available".into()))?,
    };
    let name = device.name().unwrap_or_else(|_| "Unknown input".into());
    let config = device.default_input_config().map_err(|e| crate::AudioError::Device(e.to_string()))?;
    let channels = config.channels().max(1);
    let channel = channel.clamp(1, channels);
    let stream = match config.sample_format() {
        SampleFormat::F32 => build::<f32>(&device, &config.config(), channel, hint, tx, count),
        SampleFormat::I16 => build::<i16>(&device, &config.config(), channel, hint, tx, count),
        SampleFormat::I32 => build::<i32>(&device, &config.config(), channel, hint, tx, count),
        SampleFormat::U16 => build::<u16>(&device, &config.config(), channel, hint, tx, count),
        other => return Err(crate::AudioError::Device(format!("unsupported input sample format {other:?}"))),
    }?;
    stream.play().map_err(|e| crate::AudioError::Device(e.to_string()))?;
    Ok((stream, name, channel))
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channel: u16,
    hint: FrameRate,
    mut tx: ringbuf::HeapProd<LtcReading>,
    count: Arc<AtomicU64>,
) -> crate::AudioResult<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let ch = config.channels.max(1) as usize;
    let pick = channel as usize - 1;
    let rate = config.sample_rate.0;
    let mut decoder = LtcDecoder::with_rate_hint(rate, hint);
    let callback = move |data: &[T], info: &cpal::InputCallbackInfo| {
        let now = Instant::now();
        // How long ago the end of this buffer was captured.
        let ts = info.timestamp();
        let latency = ts.callback.duration_since(&ts.capture).unwrap_or_default();
        let frames = data.len() / ch;
        for (i, frame) in data.chunks_exact(ch).enumerate() {
            let Some(f) = decoder.push(<f32 as FromSample<T>>::from_sample_(frame[pick])) else { continue };
            let back = latency + Duration::from_secs_f64((frames - 1 - i) as f64 / rate as f64);
            let _ = tx.try_push(LtcReading { timecode: f.timecode, at: now.checked_sub(back).unwrap_or(now) });
            count.fetch_add(1, Ordering::Relaxed);
        }
    };
    device
        .build_input_stream(config, callback, |e| tracing::error!("LTC input error: {e}"), None)
        .map_err(|e| crate::AudioError::Device(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use livestage_core::timecode::ltc::LtcDecoder;

    fn anchor(position: &str, rate: FrameRate, at_frame: u64) -> LtcAnchor {
        let tc = Timecode::parse_with(position, rate).unwrap().with_rate(rate);
        LtcAnchor { channel: Some(1), gain: 0.5, rate, running: true, at_frame, position_us: tc.to_time().as_micros() as f64, speed: 1.0 }
    }

    /// Renders `secs` of generator output on channel 1 of a stereo device
    /// in 256-frame callbacks, decodes it, and returns (frame, label).
    fn run(g: &mut LtcGenerator, sr: u32, start: u64, secs: f64) -> Vec<(u64, Timecode)> {
        let mut dec = LtcDecoder::new(sr);
        let mut out = Vec::new();
        let mut buf = vec![0.0f32; 512];
        let mut n = start;
        while ((n - start) as f64) < secs * sr as f64 {
            buf.fill(0.0);
            g.render(&mut buf, 2, n);
            for (i, f) in buf.as_chunks::<2>().0.iter().enumerate() {
                assert_eq!(f[0], 0.0, "only the LTC channel is written");
                if let Some(fr) = dec.push(f[1]) {
                    out.push((n + i as u64, fr.timecode));
                }
            }
            n += 256;
        }
        out
    }

    #[test]
    fn frames_carry_the_anchored_timecode_on_the_frame_grid() {
        for rate in [FrameRate::Fps25, FrameRate::Fps2997Df, FrameRate::Fps24] {
            let sr = 48_000;
            let mut g = LtcGenerator::new(sr);
            // Half a frame into 01:00:00:00 at device frame 1000.
            let mut a = anchor("01:00:00:00", rate, 1000);
            a.position_us += 0.5e6 / rate.fps();
            g.set(a);
            let frames = run(&mut g, sr, 1000, 2.0);
            assert!(frames.len() as f64 >= rate.fps() * 2.0 - 3.0, "{rate:?}: {}", frames.len());
            let per_frame = sr as f64 / rate.fps();
            for (end, label) in &frames {
                // A frame ends where the next label's frame starts.
                let us = a.position_us + (*end as f64 - 1000.0) / sr as f64 * 1e6;
                let expected = label.to_frames() as f64 + 1.0;
                let at = us * rate.fps() / 1e6;
                assert!((at - expected).abs() * per_frame < 2.0, "{rate:?}: {label} ends {:.2} samples off", (at - expected) * per_frame);
            }
        }
    }

    #[test]
    fn locate_and_stop() {
        let sr = 48_000;
        let mut g = LtcGenerator::new(sr);
        g.set(anchor("10:00:00:00", FrameRate::Fps25, 0));
        let before = run(&mut g, sr, 0, 1.0);
        assert_eq!(before.last().unwrap().1.to_string(), "10:00:00:24", "frame 24 ends at 1 s");
        // Locate: from device frame 48_000 the timecode is 02:00:00:00.
        g.set(anchor("02:00:00:00", FrameRate::Fps25, 48_000));
        let after = run(&mut g, sr, 48_000, 1.0);
        assert_eq!(after.first().unwrap().1.hours, 2, "{after:?}");
        let mut stopped = anchor("02:00:00:00", FrameRate::Fps25, 96_000);
        stopped.running = false;
        g.set(stopped);
        assert!(run(&mut g, sr, 96_000, 0.5).is_empty(), "silence while stopped");
    }
}

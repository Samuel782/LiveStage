//! Disk streaming for long audio files.
//!
//! Short files (cart pads, stingers, clicks) are decoded whole into RAM as
//! [`AudioClip`]s. Files longer than [`STREAM_MIN_SECS`] are *streamed*: only
//! their first [`PREROLL_SECS`] stay resident ([`StreamedClip::preroll`]); the
//! rest is decoded during playback by the streamer thread into one lock-free
//! ring per playing track, which the audio thread drains ([`StreamReader`]).
//!
//! ```text
//!  loader thread ── probe + decode pre-roll ──▶ StreamedClip (resident)
//!  control thread ─ open_stream(start) ──┬──▶ StreamJob ──▶ streamer thread
//!                                         │      decode → resample → ring
//!                                         └──▶ StreamReader ──▶ audio thread
//!                                                pre-roll, then ring
//! ```
//!
//! * **Start.** A cue starting inside the pre-roll plays it from RAM at once
//!   while the streamer fills the ring from the pre-roll's end. A cue
//!   starting past it waits (off the show thread) until its ring is primed.
//! * **Seek.** New readers are opened at the target and primed while the old
//!   ones keep playing; the audio thread swaps them at the de-click splice.
//! * **Loops** are produced by the streamer in play order (it wraps itself),
//!   so the reader only ever reads sequentially.
//! * **Underruns** (a disk slower than real time) play silence but keep the
//!   track sample-aligned: the missed frames are skipped when data arrives.
//!
//! The resampler is the same 4-point Catmull-Rom as
//! [`AudioClip::resampled`], evaluated at the same positions, so a streamed
//! file plays sample-for-sample what the resident path would.

use crate::renderer::EngineShared;
use crate::{AudioClip, AudioError, AudioResult};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::TimeBase;

/// Files at least this long are streamed from disk; shorter ones (cart
/// pads, stingers) are decoded whole into RAM.
pub const STREAM_MIN_SECS: f64 = 15.0;
/// Resident head of a streamed file: starting a cue inside it is instant.
pub const PREROLL_SECS: f64 = 3.0;
/// Ring buffer per playing track.
const RING_SECS: f64 = 4.0;
/// Decoded audio a ring must hold before a start or seek past the pre-roll
/// is released to the audio thread.
const PRIME_SECS: f64 = 0.3;
/// Frames decoded per streamer step.
const CHUNK_FRAMES: usize = 4_096;
/// Frames the audio thread pulls from a ring at a time.
const READ_FRAMES: usize = 256;
/// Frames decoded (and discarded) before a seek target, so lapped codecs
/// (MP3's bit reservoir and overlap, AAC, Vorbis) are warmed up and the
/// target frame matches a decode from the start of the file.
const SEEK_WARMUP_FRAMES: u64 = 8_192;
/// Streamer sleep when every ring is full.
const IDLE: Duration = Duration::from_millis(4);

// ------------------------------------------------------------- decoding --

/// Sequential decoder for one file at its native rate, with sample-accurate
/// seeking.
struct FileDecoder {
    path: PathBuf,
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    time_base: Option<TimeBase>,
    rate: u32,
    sbuf: Option<SampleBuffer<f32>>,
    /// Input frame the next delivered sample must start at (after a seek).
    skip_to: Option<u64>,
}

fn decode_err(path: &Path, e: impl std::fmt::Display) -> AudioError {
    AudioError::Decode { path: path.display().to_string(), msg: e.to_string() }
}

impl FileDecoder {
    fn open(path: &Path) -> AudioResult<Self> {
        let file = File::open(path)?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let probed = symphonia::default::get_probe()
            .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
            .map_err(|e| decode_err(path, e))?;
        let format = probed.format;
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or_else(|| AudioError::NoTrack(path.display().to_string()))?;
        let params = track.codec_params.clone();
        let decoder = symphonia::default::get_codecs().make(&params, &DecoderOptions::default()).map_err(|e| decode_err(path, e))?;
        Ok(Self {
            path: path.to_path_buf(),
            track_id: track.id,
            time_base: params.time_base,
            rate: params.sample_rate.unwrap_or(48_000),
            format,
            decoder,
            sbuf: None,
            skip_to: None,
        })
    }

    fn frame_to_ts(&self, frame: u64) -> u64 {
        match self.time_base {
            Some(tb) if tb.numer > 0 => ((frame as u128 * tb.denom as u128) / (tb.numer as u128 * self.rate.max(1) as u128)) as u64,
            _ => frame,
        }
    }

    fn ts_to_frame(&self, ts: u64) -> u64 {
        match self.time_base {
            Some(tb) if tb.denom > 0 => ((ts as u128 * tb.numer as u128 * self.rate as u128) / tb.denom as u128) as u64,
            _ => ts,
        }
    }

    /// The next [`read`](Self::read) starts exactly at input frame `frame`.
    fn seek(&mut self, frame: u64) -> AudioResult<()> {
        let warm = frame.saturating_sub(SEEK_WARMUP_FRAMES);
        let to = SeekTo::TimeStamp { ts: self.frame_to_ts(warm), track_id: self.track_id };
        if self.format.seek(SeekMode::Accurate, to).is_err() {
            // Unseekable stream: start over and decode up to the target.
            *self = Self::open(&self.path.clone())?;
        }
        self.decoder.reset();
        self.skip_to = Some(frame);
        Ok(())
    }

    /// Appends the next decoded packet (interleaved, native rate) to `out`.
    /// Returns false at the end of the file.
    fn read(&mut self, out: &mut Vec<f32>) -> AudioResult<bool> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(false),
                Err(SymError::ResetRequired) => return Ok(false),
                Err(e) => return Err(decode_err(&self.path, e)),
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            let start = self.ts_to_frame(packet.ts());
            let buf = match self.decoder.decode(&packet) {
                Ok(b) => b,
                Err(SymError::DecodeError(e)) => {
                    tracing::warn!(file = %self.path.display(), "skipping bad frame: {e}");
                    continue;
                }
                Err(e) => return Err(decode_err(&self.path, e)),
            };
            let spec = *buf.spec();
            let ch = spec.channels.count();
            let needed = buf.capacity() as u64;
            let sb = match &mut self.sbuf {
                Some(sb) if sb.capacity() as u64 >= needed * ch as u64 => sb,
                _ => self.sbuf.insert(SampleBuffer::<f32>::new(needed, spec)),
            };
            sb.copy_interleaved_ref(buf);
            let samples = sb.samples();
            let skip = match self.skip_to {
                Some(target) => {
                    let n = samples.len() / ch.max(1);
                    if start + n as u64 <= target {
                        continue; // entirely before the seek target
                    }
                    self.skip_to = None;
                    target.saturating_sub(start) as usize * ch
                }
                None => 0,
            };
            out.extend_from_slice(&samples[skip.min(samples.len())..]);
            return Ok(true);
        }
    }
}

// ------------------------------------------------------------ generator --

/// Produces a file's frames at the engine rate, in play order, from any
/// start frame: decode → resample → optional loop.
struct Generator {
    dec: FileDecoder,
    channels: usize,
    /// Input frames per output frame.
    ratio: f64,
    in_frames: u64,
    /// Decoded input, `base` = file frame of its first frame.
    input: Vec<f32>,
    base: u64,
    eof: bool,
    /// Next output (engine-rate) frame.
    next: u64,
    region: (u64, u64),
    looped: bool,
    /// Nothing decoded since the file was opened (no seek needed for frame 0).
    fresh: bool,
}

impl Generator {
    fn new(clip: &StreamedClip, start: u64, region: (u64, u64), looped: bool) -> AudioResult<Self> {
        let mut g = Self {
            dec: FileDecoder::open(&clip.path)?,
            channels: clip.channels as usize,
            ratio: clip.file_rate as f64 / clip.sample_rate as f64,
            in_frames: clip.in_frames,
            input: Vec::new(),
            base: 0,
            eof: false,
            next: 0,
            region,
            looped,
            fresh: true,
        };
        g.reposition(start)?;
        Ok(g)
    }

    fn passthrough(&self) -> bool {
        self.ratio == 1.0
    }

    /// First input frame output frame `o` reads.
    fn first_input(&self, o: u64) -> u64 {
        if self.passthrough() {
            o
        } else {
            ((o as f64 * self.ratio).floor() as u64).saturating_sub(1)
        }
    }

    fn reposition(&mut self, o: u64) -> AudioResult<()> {
        self.next = o;
        self.base = self.first_input(o);
        self.input.clear();
        self.eof = false;
        if self.base > 0 || !self.fresh {
            self.dec.seek(self.base)?;
        }
        self.fresh = false;
        Ok(())
    }

    fn buffered(&self) -> u64 {
        (self.input.len() / self.channels.max(1)) as u64
    }

    /// Decodes until input frame `last` is buffered (or the file ends).
    fn fill_to(&mut self, last: u64) -> AudioResult<()> {
        while !self.eof && self.base + self.buffered() <= last {
            if !self.dec.read(&mut self.input)? {
                self.eof = true;
            }
        }
        Ok(())
    }

    /// Input sample, clamped to the file like the resident resampler.
    #[inline]
    fn input_at(&self, f: i64, c: usize) -> f32 {
        let f = f.clamp(0, self.in_frames.saturating_sub(1) as i64) as u64;
        let have = self.buffered();
        if have == 0 {
            return 0.0;
        }
        // Past what the file delivered (length over-reported): hold the last frame.
        let idx = f.saturating_sub(self.base).min(have - 1) as usize;
        self.input[idx * self.channels + c]
    }

    /// Appends up to `max` frames to `out`. Returns false once a non-looped
    /// region is exhausted.
    fn produce(&mut self, max: usize, out: &mut Vec<f32>) -> AudioResult<bool> {
        let (start, end) = self.region;
        if self.next >= end {
            if !self.looped || end <= start {
                return Ok(false);
            }
            self.reposition(start)?;
        }
        let n = (max as u64).min(end - self.next);
        let last_out = self.next + n - 1;
        let last_in = if self.passthrough() { last_out } else { (last_out as f64 * self.ratio).floor() as u64 + 2 };
        self.fill_to(last_in)?;
        let ch = self.channels;
        for o in self.next..self.next + n {
            if self.passthrough() {
                for c in 0..ch {
                    out.push(self.input_at(o as i64, c));
                }
            } else {
                let pos = o as f64 * self.ratio;
                let i = pos.floor() as i64;
                let t = (pos - i as f64) as f32;
                for c in 0..ch {
                    let (p0, p1, p2, p3) = (self.input_at(i - 1, c), self.input_at(i, c), self.input_at(i + 1, c), self.input_at(i + 2, c));
                    let a = -0.5 * p0 + 1.5 * p1 - 1.5 * p2 + 0.5 * p3;
                    let b = p0 - 2.5 * p1 + 2.0 * p2 - 0.5 * p3;
                    let cc = -0.5 * p0 + 0.5 * p2;
                    out.push(((a * t + b) * t + cc) * t + p1);
                }
            }
        }
        self.next += n;
        // Drop input no later output frame needs.
        let keep_from = self.first_input(self.next);
        if keep_from > self.base {
            let drop = ((keep_from - self.base).min(self.buffered()) as usize) * ch;
            self.input.drain(..drop);
            self.base += (drop / ch.max(1)) as u64;
        }
        Ok(true)
    }
}

// ---------------------------------------------------------- clip & load --

/// A long file played from disk. Only its pre-roll is resident.
#[derive(Debug)]
pub struct StreamedClip {
    pub path: PathBuf,
    /// Engine rate.
    pub sample_rate: u32,
    pub channels: u16,
    /// Length at the engine rate.
    pub frames: u64,
    file_rate: u32,
    in_frames: u64,
    /// The file's first [`PREROLL_SECS`] at the engine rate.
    pub preroll: Arc<AudioClip>,
}

impl StreamedClip {
    /// Probes `path` and decodes its pre-roll; `None` when the file is short
    /// enough (or of unknown length) to be decoded whole instead.
    pub fn open(path: &Path, sample_rate: u32) -> AudioResult<Option<Self>> {
        let info = crate::decode::probe(path)?;
        let Some(in_frames) = info.frames else { return Ok(None) };
        let channels = info.channels as usize;
        if in_frames as f64 / info.sample_rate.max(1) as f64 <= STREAM_MIN_SECS || channels == 0 || channels > crate::renderer::MAX_CLIP_CHANNELS {
            return Ok(None);
        }
        let frames = if info.sample_rate == sample_rate {
            in_frames
        } else {
            (in_frames as f64 / (info.sample_rate as f64 / sample_rate as f64)).floor() as u64
        };
        let mut clip = Self {
            path: path.to_path_buf(),
            sample_rate,
            channels: info.channels,
            frames,
            file_rate: info.sample_rate,
            in_frames,
            preroll: Arc::new(AudioClip { sample_rate, channels: info.channels, data: Vec::new() }),
        };
        let n = ((PREROLL_SECS * sample_rate as f64) as u64).min(frames);
        let mut g = Generator::new(&clip, 0, (0, n), false)?;
        let mut data = Vec::with_capacity(n as usize * info.channels as usize);
        while g.produce(CHUNK_FRAMES, &mut data)? {}
        clip.preroll = Arc::new(AudioClip { sample_rate, channels: info.channels, data });
        Ok(Some(clip))
    }

    pub fn duration_secs(&self) -> f64 {
        self.frames as f64 / self.sample_rate.max(1) as f64
    }

    /// Resident bytes (the pre-roll).
    pub fn resident_bytes(&self) -> usize {
        self.preroll.data.len() * std::mem::size_of::<f32>()
    }
}

// ------------------------------------------------------- reader & jobs --

/// State shared by a reader and its streamer job.
#[derive(Debug, Default)]
struct StreamShared {
    /// Cleared when the reader is dropped: the job stops.
    alive: AtomicBool,
    /// The ring holds enough to start (or the region is fully decoded).
    primed: AtomicBool,
}

/// Audio-thread side of one streamed track: the pre-roll, then the ring.
pub struct StreamReader {
    clip: Arc<StreamedClip>,
    channels: usize,
    /// Next pre-roll frame, while below `preroll_until`.
    preroll_pos: u64,
    preroll_until: u64,
    ring: HeapCons<f32>,
    local: Box<[f32]>,
    local_pos: usize,
    local_len: usize,
    /// Frames played as silence during an underrun, skipped once data arrives.
    deficit: u64,
    starving: bool,
    shared: Arc<StreamShared>,
    engine: Arc<EngineShared>,
}

impl std::fmt::Debug for StreamReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamReader").field("file", &self.clip.path).field("deficit", &self.deficit).finish()
    }
}

impl Drop for StreamReader {
    fn drop(&mut self) {
        self.shared.alive.store(false, Ordering::Release);
    }
}

impl StreamReader {
    pub fn clip(&self) -> &Arc<StreamedClip> {
        &self.clip
    }

    /// Can play without waiting: inside the pre-roll, or the ring is primed.
    pub fn primed(&self) -> bool {
        self.preroll_pos < self.preroll_until || self.shared.primed.load(Ordering::Acquire)
    }

    /// Writes the next frame's samples into `out[..channels]`. Real-time
    /// safe: no locks, no allocation.
    #[inline]
    pub fn next_frame(&mut self, out: &mut [f32]) {
        let ch = self.channels;
        if self.preroll_pos < self.preroll_until {
            let i = self.preroll_pos as usize * ch;
            out[..ch].copy_from_slice(&self.clip.preroll.data[i..i + ch]);
            self.preroll_pos += 1;
            return;
        }
        if self.local_pos >= self.local_len && !self.refill() {
            out[..ch].fill(0.0);
            self.deficit += 1;
            if !self.starving {
                self.starving = true;
                self.engine.underruns.fetch_add(1, Ordering::Relaxed);
            }
            return;
        }
        out[..ch].copy_from_slice(&self.local[self.local_pos..self.local_pos + ch]);
        self.local_pos += ch;
    }

    fn refill(&mut self) -> bool {
        let ch = self.channels;
        if self.deficit > 0 {
            let frames = self.deficit.min((self.ring.occupied_len() / ch) as u64);
            self.ring.skip(frames as usize * ch);
            self.deficit -= frames;
            if self.deficit > 0 {
                return false;
            }
        }
        let n = self.ring.pop_slice(&mut self.local);
        self.local_pos = 0;
        self.local_len = n - n % ch;
        if self.local_len > 0 {
            self.starving = false;
        }
        self.local_len > 0
    }
}

/// Streamer-thread side: decodes into the ring in play order.
pub struct StreamJob {
    gen: Option<Generator>,
    ring: HeapProd<f32>,
    channels: usize,
    pending: Vec<f32>,
    pending_pos: usize,
    shared: Arc<StreamShared>,
    prime_frames: usize,
    path: PathBuf,
}

impl StreamJob {
    /// Pushes decoded audio while the ring has room. Returns whether it did
    /// any work.
    fn service(&mut self) -> bool {
        let ch = self.channels.max(1);
        let mut worked = false;
        loop {
            if self.pending_pos < self.pending.len() {
                let room = self.ring.vacant_len() / ch * ch;
                let n = room.min(self.pending.len() - self.pending_pos);
                if n == 0 {
                    break;
                }
                self.ring.push_slice(&self.pending[self.pending_pos..self.pending_pos + n]);
                self.pending_pos += n;
                worked = true;
                if self.ring.occupied_len() >= self.prime_frames * ch {
                    self.shared.primed.store(true, Ordering::Release);
                }
                continue;
            }
            let Some(gen) = self.gen.as_mut() else { break };
            if self.ring.vacant_len() < CHUNK_FRAMES * ch {
                break;
            }
            self.pending.clear();
            self.pending_pos = 0;
            // A decoder that panics on a damaged file ends this stream only
            // (it plays out what is buffered), not the streamer thread.
            let pending = &mut self.pending;
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| gen.produce(CHUNK_FRAMES, pending))) {
                Ok(Ok(true)) => worked = true,
                Ok(Ok(false)) => self.gen = None,
                Ok(Err(e)) => {
                    tracing::error!(file = %self.path.display(), "stream decode failed: {e}");
                    self.gen = None;
                }
                Err(_) => {
                    tracing::error!(file = %self.path.display(), "stream decoder crashed; the rest of the file is skipped");
                    self.pending.clear();
                    self.gen = None;
                }
            }
        }
        if self.gen.is_none() && self.pending_pos >= self.pending.len() {
            // Fully decoded: whatever is in the ring is all there is.
            self.shared.primed.store(true, Ordering::Release);
        }
        worked
    }
}

/// Opens a reader for frames `start..` of `clip`'s play region
/// `region.0..region.1` (looping back to `region.0` when `looped`), plus the
/// job that feeds it. Called on the control thread.
pub fn open_stream(
    clip: &Arc<StreamedClip>,
    start: u64,
    region: (u64, u64),
    looped: bool,
    engine: &Arc<EngineShared>,
) -> AudioResult<(StreamReader, StreamJob)> {
    let ch = clip.channels as usize;
    let end = region.1.min(clip.frames);
    let preroll_end = clip.preroll.frames().min(end);
    let (preroll_pos, preroll_until, mut job_start) = if start < preroll_end { (start, preroll_end, preroll_end) } else { (0, 0, start) };
    let mut gen = None;
    if job_start >= end && looped && end > region.0 {
        job_start = region.0;
    }
    if job_start < end {
        gen = Some(Generator::new(clip, job_start, (region.0, end), looped)?);
    }
    let rate = clip.sample_rate as f64;
    let (prod, cons) = HeapRb::<f32>::new(((RING_SECS * rate) as usize).max(CHUNK_FRAMES * 2) * ch).split();
    let shared = Arc::new(StreamShared { alive: AtomicBool::new(true), primed: AtomicBool::new(gen.is_none()) });
    let reader = StreamReader {
        clip: clip.clone(),
        channels: ch,
        preroll_pos,
        preroll_until,
        ring: cons,
        local: vec![0.0; READ_FRAMES * ch].into_boxed_slice(),
        local_pos: 0,
        local_len: 0,
        deficit: 0,
        starving: false,
        shared: shared.clone(),
        engine: engine.clone(),
    };
    let job = StreamJob {
        gen,
        ring: prod,
        channels: ch,
        pending: Vec::with_capacity(CHUNK_FRAMES * ch),
        pending_pos: 0,
        shared,
        prime_frames: (PRIME_SECS * rate) as usize,
        path: clip.path.clone(),
    };
    Ok((reader, job))
}

/// The thread that keeps every playing stream's ring topped up.
#[derive(Clone)]
pub struct Streamer {
    tx: mpsc::Sender<StreamJob>,
}

impl Streamer {
    pub fn spawn() -> std::io::Result<Self> {
        let (tx, rx) = mpsc::channel::<StreamJob>();
        std::thread::Builder::new()
            .name("livestage-audio-stream".into())
            .spawn(move || {
                let mut jobs: Vec<StreamJob> = Vec::new();
                loop {
                    if jobs.is_empty() {
                        match rx.recv() {
                            Ok(j) => jobs.push(j),
                            Err(_) => return,
                        }
                    }
                    jobs.extend(rx.try_iter());
                    jobs.retain(|j| j.shared.alive.load(Ordering::Acquire));
                    // Unprimed rings first: they gate a start or a seek.
                    jobs.sort_by_key(|j| j.shared.primed.load(Ordering::Relaxed));
                    let mut worked = false;
                    for j in &mut jobs {
                        worked |= j.service();
                    }
                    if !worked {
                        // Rings full (finished jobs stay until their reader is
                        // dropped, since the ring may still hold audio).
                        std::thread::sleep(IDLE);
                    }
                }
            })?;
        Ok(Self { tx })
    }

    pub fn submit(&self, job: StreamJob) {
        let _ = self.tx.send(job);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::tests::write_wav;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ls-stream-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A stereo ramp long enough to be streamed: every sample encodes its frame.
    fn long_wav(dir: &Path, rate: u32, secs: u32) -> PathBuf {
        let n = (rate * secs) as usize;
        let samples: Vec<i16> = (0..n).flat_map(|i| [((i % 30_000) as i16), -((i % 20_000) as i16)]).collect();
        let p = dir.join(format!("long-{rate}.wav"));
        write_wav(&p, rate, 2, &samples);
        p
    }

    fn drain(reader: &mut StreamReader, job: &mut StreamJob, frames: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames * 2);
        let mut f = [0.0f32; 2];
        for _ in 0..frames {
            if reader.local_pos >= reader.local_len && reader.preroll_pos >= reader.preroll_until {
                job.service();
            }
            reader.next_frame(&mut f);
            out.extend_from_slice(&f);
        }
        out
    }

    fn engine() -> Arc<EngineShared> {
        Arc::new(EngineShared::default())
    }

    #[test]
    fn short_files_are_not_streamed() {
        let dir = temp("short");
        let p = long_wav(&dir, 48_000, 2);
        assert!(StreamedClip::open(&p, 48_000).unwrap().is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn streamed_matches_resident_at_any_start_and_rate() {
        let dir = temp("match");
        for rate in [48_000, 44_100] {
            let p = long_wav(&dir, rate, 20);
            let resident = crate::decode::decode_file(&p).unwrap().resampled(48_000);
            let clip = Arc::new(StreamedClip::open(&p, 48_000).unwrap().expect("streamed"));
            assert_eq!(clip.frames, resident.frames(), "length at {rate} Hz");
            assert!(clip.resident_bytes() < resident.data.len() * 4 / 5);
            // Inside the pre-roll (crosses into the ring), and well past it.
            for start in [0u64, 100_000, 500_000, clip.frames - 1_000] {
                let (mut r, mut j) = open_stream(&clip, start, (0, clip.frames), false, &engine()).unwrap();
                let n = 160_000.min((clip.frames - start) as usize);
                let got = drain(&mut r, &mut j, n);
                let want = &resident.data[start as usize * 2..(start as usize + n) * 2];
                let bad = got.iter().zip(want).position(|(a, b)| (a - b).abs() > 1e-6);
                assert_eq!(bad, None, "{rate} Hz from {start}: first mismatch at sample {bad:?}");
            }
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn loops_wrap_in_play_order() {
        let dir = temp("loop");
        let p = long_wav(&dir, 48_000, 20);
        let resident = crate::decode::decode_file(&p).unwrap();
        let clip = Arc::new(StreamedClip::open(&p, 48_000).unwrap().unwrap());
        // Region inside the pre-roll and region past it.
        for region in [(1_000u64, 50_000u64), (200_000, 300_000)] {
            let len = region.1 - region.0;
            let (mut r, mut j) = open_stream(&clip, region.0, region, true, &engine()).unwrap();
            let got = drain(&mut r, &mut j, (len * 3) as usize);
            for k in [0u64, len - 1, len, len + 7, 2 * len + 3] {
                let src = region.0 + k % len;
                assert_eq!(got[k as usize * 2], resident.data[src as usize * 2], "region {region:?} frame {k}");
            }
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn underrun_plays_silence_then_realigns() {
        let dir = temp("underrun");
        let p = long_wav(&dir, 48_000, 20);
        let resident = crate::decode::decode_file(&p).unwrap();
        let clip = Arc::new(StreamedClip::open(&p, 48_000).unwrap().unwrap());
        let start = 400_000u64;
        let shared = engine();
        let (mut r, mut j) = open_stream(&clip, start, (0, clip.frames), false, &shared).unwrap();
        let mut f = [0.0f32; 2];
        // Nothing decoded yet: 100 frames of silence, one underrun.
        for _ in 0..100 {
            r.next_frame(&mut f);
            assert_eq!(f, [0.0, 0.0]);
        }
        assert_eq!(shared.underruns.load(Ordering::Relaxed), 1);
        // Once data arrives the track is still on the sample clock.
        j.service();
        r.next_frame(&mut f);
        assert_eq!(f[0], resident.data[(start as usize + 100) * 2]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn readers_past_the_preroll_wait_for_priming() {
        let dir = temp("prime");
        let p = long_wav(&dir, 48_000, 20);
        let clip = Arc::new(StreamedClip::open(&p, 48_000).unwrap().unwrap());
        let (r, _j) = open_stream(&clip, 0, (0, clip.frames), false, &engine()).unwrap();
        assert!(r.primed(), "pre-roll starts are instant");
        let (r, mut j) = open_stream(&clip, 600_000, (0, clip.frames), false, &engine()).unwrap();
        assert!(!r.primed());
        j.service();
        assert!(r.primed());
        std::fs::remove_dir_all(dir).ok();
    }

    /// Diagnostic for real files (MP3, AAC, FLAC...): streamed output vs a
    /// full decode, from several starts. Compressed formats may differ by a
    /// codec's seek preroll right at a seek point; the rest must match.
    /// `LIVESTAGE_STREAM_FILE=/path/song.mp3 cargo test -p livestage_audio real_file -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_file_matches_full_decode() {
        let Ok(path) = std::env::var("LIVESTAGE_STREAM_FILE") else { return };
        let path = PathBuf::from(path);
        let resident = crate::decode::decode_file(&path).unwrap().resampled(48_000);
        let clip = Arc::new(StreamedClip::open(&path, 48_000).unwrap().expect("long enough to stream"));
        println!("{}: {} frames streamed, {} decoded whole", path.display(), clip.frames, resident.frames());
        let ch = clip.channels as usize;
        for start in [0u64, 300_000, 2_000_000, clip.frames / 2] {
            let (mut r, mut j) = open_stream(&clip, start, (0, clip.frames), false, &engine()).unwrap();
            let n = 96_000usize.min((clip.frames - start) as usize);
            let mut f = vec![0.0f32; ch];
            let mut worst = (0.0f32, 0usize);
            for k in 0..n {
                if r.local_pos >= r.local_len && r.preroll_pos >= r.preroll_until {
                    j.service();
                }
                r.next_frame(&mut f);
                for (c, v) in f.iter().enumerate() {
                    let d = (v - resident.data[(start as usize + k) * ch + c]).abs();
                    if d > worst.0 {
                        worst = (d, k);
                    }
                }
            }
            println!("start {start}: worst |diff| {:.6} at frame +{}", worst.0, worst.1);
        }
    }
}

//! Movie-file backend built on FFmpeg (`ffmpeg-next`), enabled by the
//! `ffmpeg` cargo feature.
//!
//! Each source owns a worker thread that demuxes, decodes and converts to
//! RGBA8 ahead of the playhead. `frame_at` only drains a bounded channel, so
//! the render thread never waits on the codec. Seeks follow the same
//! generation-counter scheme as [`ImageSequenceSource`](crate::source::ImageSequenceSource).
//!
//! **Hardware decode.** When the platform has a hardware decoder
//! (VideoToolbox on macOS, D3D11VA on Windows, VAAPI on Linux) the codec is
//! given a hardware device; decoded surfaces are transferred as NV12 and
//! uploaded as two GPU planes, with YUV→RGB done in the layer shader. That
//! removes both the CPU decode and the CPU colour conversion. Streams the
//! hardware can't decode fall back to software inside FFmpeg; set
//! `LIVESTAGE_HWDEC=0` to force software decode.
//!
//! Frame positions are on an *unwrapped* timeline: when a looped clip reaches
//! EOF the worker rewinds and keeps counting up from the clip length, so the
//! loop point is just more look-ahead rather than a seek.

use crate::source::{next_serial, VideoBackend, VideoFrame, VideoSource, YuvMatrix};
use crate::{VideoError, VideoResult};
use ffmpeg_next as ff;
use livestage_core::time::ShowTime;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use std::time::Duration;

/// Frames decoded ahead of the playhead.
const LOOKAHEAD: usize = 8;

/// File extensions routed to this backend.
const EXTENSIONS: &[&str] =
    &["mov", "mp4", "m4v", "mkv", "webm", "avi", "mxf", "mpg", "mpeg", "ts", "m2ts", "mts", "wmv", "flv", "ogv", "dv", "nut"];

/// FFmpeg decoder backend for movie files.
pub struct FfmpegBackend {
    _private: (),
}

impl FfmpegBackend {
    /// Initialises the FFmpeg libraries (once per process).
    pub fn new() -> VideoResult<Self> {
        static INIT: OnceLock<Result<(), String>> = OnceLock::new();
        INIT.get_or_init(|| {
            ff::init().map_err(|e| e.to_string())?;
            ff::util::log::set_level(ff::util::log::Level::Error);
            Ok(())
        })
        .clone()
        .map_err(|msg| VideoError::Decode { path: "<ffmpeg init>".into(), msg })?;
        Ok(Self { _private: () })
    }
}

impl VideoBackend for FfmpegBackend {
    fn name(&self) -> &str {
        "ffmpeg (movie files)"
    }
    fn can_open(&self, path: &Path) -> bool {
        path.is_file()
            && path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
    }
    fn open(&self, path: &Path, looped: bool) -> VideoResult<Box<dyn VideoSource>> {
        Ok(Box::new(FfmpegSource::open(path, looped)?))
    }
}

/// Stream facts probed by the worker before it starts decoding.
struct StreamInfo {
    size: (u32, u32),
    duration_us: Option<i64>,
    frame_us: i64,
}

enum Command {
    /// Restart decoding so the first frame sent covers `target_us`
    /// (unwrapped timeline).
    Seek { generation: u64, target_us: i64 },
}

struct Decoded {
    generation: u64,
    pos_us: i64,
    frame: VideoFrame,
}

/// A movie file decoded on a worker thread.
pub struct FfmpegSource {
    size: (u32, u32),
    looped: bool,
    duration_us: Option<i64>,
    frame_us: i64,
    rx: mpsc::Receiver<Decoded>,
    commands: mpsc::Sender<Command>,
    /// Shared with the worker so it can abandon stale work without waiting
    /// for the command channel.
    latest_generation: Arc<AtomicU64>,
    /// Bumped on every seek so stale look-ahead frames are discarded.
    generation: u64,
    /// Seek target not yet answered by a frame of the current generation.
    pending_seek: Option<i64>,
    current: Option<(u64, i64, VideoFrame)>,
    next: Option<(i64, VideoFrame)>,
}

impl FfmpegSource {
    pub fn open(path: &Path, looped: bool) -> VideoResult<Self> {
        let (tx, rx) = mpsc::sync_channel::<Decoded>(LOOKAHEAD);
        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();
        let (info_tx, info_rx) = mpsc::channel::<Result<StreamInfo, String>>();
        let latest_generation = Arc::new(AtomicU64::new(0));
        let worker = Worker { path: path.to_path_buf(), looped, tx, commands: cmd_rx, latest_generation: latest_generation.clone() };
        std::thread::Builder::new().name("livestage-ffmpeg".into()).spawn(move || worker.run(info_tx))?;

        let decode_err = |msg: String| VideoError::Decode { path: path.display().to_string(), msg };
        let info = info_rx.recv().map_err(|_| decode_err("decoder thread exited".into()))?.map_err(decode_err)?;
        Ok(Self {
            size: info.size,
            looped,
            duration_us: info.duration_us,
            frame_us: info.frame_us,
            rx,
            commands: cmd_tx,
            latest_generation,
            generation: 0,
            pending_seek: None,
            current: None,
            next: None,
        })
    }

    /// Length of one pass through the clip, if the container reports it
    /// (unlike [`VideoSource::duration`], also for looped sources).
    pub fn clip_duration(&self) -> Option<ShowTime> {
        self.duration_us.map(|d| ShowTime::from_micros(d.max(0) as u64))
    }

    fn seek(&mut self, target_us: i64) {
        self.generation += 1;
        self.latest_generation.store(self.generation, Ordering::Release);
        let _ = self.commands.send(Command::Seek { generation: self.generation, target_us });
        self.pending_seek = Some(target_us);
        self.next = None;
    }
}

impl VideoSource for FfmpegSource {
    fn frame_at(&mut self, t: ShowTime) -> Option<VideoFrame> {
        let mut want = t.0.min(i64::MAX as u64) as i64;
        if !self.looped {
            // Past the end: hold the last frame rather than seeking off the end.
            if let Some(d) = self.duration_us {
                want = want.min(d.max(0));
            }
        }
        // Where the worker's stream is (or will be once the pending seek lands).
        let head = self.pending_seek.unwrap_or(match &self.current {
            Some((g, pos, _)) if *g == self.generation => *pos,
            _ => 0,
        });
        let jump = (self.frame_us * LOOKAHEAD as i64 * 2).max(250_000);
        if want < head || want > head + jump {
            self.seek(want);
        }
        loop {
            if let Some((pos, frame)) = self.next.take() {
                if pos > want {
                    self.next = Some((pos, frame));
                    break;
                }
                self.current = Some((self.generation, pos, frame));
                continue;
            }
            match self.rx.try_recv() {
                Ok(d) if d.generation == self.generation => {
                    if self.current.as_ref().is_none_or(|(g, _, _)| *g != self.generation) {
                        // First frame after open/seek: show it straight away
                        // rather than holding the pre-seek picture.
                        self.current = Some((d.generation, d.pos_us, d.frame));
                        self.pending_seek = None;
                    } else {
                        self.next = Some((d.pos_us, d.frame));
                    }
                }
                Ok(_) => continue, // stale look-ahead from before a seek
                Err(_) => break,
            }
        }
        self.current.as_ref().map(|(_, _, f)| f.clone())
    }
    fn duration(&self) -> Option<ShowTime> {
        if self.looped {
            None
        } else {
            self.clip_duration()
        }
    }
    fn size(&self) -> (u32, u32) {
        self.size
    }
}

// ----------------------------------------------------------------- worker --

struct Worker {
    path: PathBuf,
    looped: bool,
    tx: mpsc::SyncSender<Decoded>,
    commands: mpsc::Receiver<Command>,
    latest_generation: Arc<AtomicU64>,
}

/// Per-stream decode state owned by the worker thread (FFmpeg contexts are
/// not `Send`, so everything is created there).
struct Decoder {
    input: ff::format::context::Input,
    stream: usize,
    time_base: ff::Rational,
    start_pts: i64,
    decoder: ff::decoder::Video,
    scaler: Option<(ff::format::Pixel, u32, u32, ff::software::scaling::Context)>,
    /// Keeps the hardware device alive (the codec holds its own reference too).
    _hw: Option<HwDevice>,
    /// Logged once: which path the first frame took.
    reported_path: bool,
    duration_us: Option<i64>,
    frame_us: i64,
    /// Measured length of one pass, known after the first EOF. Can exceed
    /// `duration_us` when the container under-reports by a frame.
    loop_us: Option<i64>,
}

/// Outcome of trying to hand a frame to the render side.
enum Sent {
    Ok,
    Stale,
    Closed,
}

impl Worker {
    fn run(self, info: mpsc::Sender<Result<StreamInfo, String>>) {
        let mut dec = match Decoder::open(&self.path) {
            Ok(d) => d,
            Err(e) => {
                let _ = info.send(Err(e));
                return;
            }
        };
        let size = (dec.decoder.width(), dec.decoder.height());
        if info.send(Ok(StreamInfo { size, duration_us: dec.duration_us, frame_us: dec.frame_us })).is_err() {
            return;
        }
        drop(info);

        let mut generation = 0u64;
        // Unwrapped position of clip time zero for the current pass.
        let mut base_us = 0i64;
        // Frames ending at or before this clip time are decoded but not sent.
        let mut skip_until_us = 0i64;
        // End of the last decoded frame in this pass (loop length fallback).
        let mut pass_end_us = 0i64;
        let mut frames_this_pass = 0usize;
        let mut packet = ff::Packet::empty();
        let mut decoded = ff::frame::Video::empty();

        loop {
            let mut seek = None;
            while let Ok(cmd) = self.commands.try_recv() {
                seek = Some(cmd);
            }
            if let Some(Command::Seek { generation: g, target_us }) = seek {
                generation = g;
                let (base, clip) = self.split(&dec, target_us);
                base_us = base;
                skip_until_us = clip;
                frames_this_pass = 0;
                dec.seek(clip);
            }

            let at_eof = match packet.read(&mut dec.input) {
                Ok(()) => {
                    if packet.stream() == dec.stream {
                        if let Err(e) = dec.decoder.send_packet(&packet) {
                            tracing::debug!("{}: {e}", self.path.display());
                        }
                    }
                    false
                }
                Err(ff::Error::InvalidData) => continue,
                Err(ff::Error::Eof) => {
                    let _ = dec.decoder.send_eof();
                    true
                }
                Err(e) => {
                    tracing::warn!("{}: {e}", self.path.display());
                    let _ = dec.decoder.send_eof();
                    true
                }
            };

            // Drain whatever the decoder has ready.
            let mut closed = false;
            while dec.decoder.receive_frame(&mut decoded).is_ok() {
                if self.latest_generation.load(Ordering::Acquire) != generation {
                    break; // a seek is waiting; stop converting stale frames
                }
                let Some(pos) = dec.position_us(&decoded) else { continue };
                pass_end_us = pass_end_us.max(pos + dec.frame_us);
                frames_this_pass += 1;
                if pos + dec.frame_us <= skip_until_us {
                    continue;
                }
                let frame = match dec.convert(&decoded) {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::warn!("{}: {e}", self.path.display());
                        continue;
                    }
                };
                match self.send(Decoded { generation, pos_us: base_us + pos, frame }) {
                    Sent::Ok => {}
                    Sent::Stale => break,
                    Sent::Closed => {
                        closed = true;
                        break;
                    }
                }
            }
            if closed {
                return;
            }

            if at_eof {
                if self.looped && frames_this_pass > 0 {
                    // A pass that started mid-clip after a seek still ends at the
                    // clip's true end, so the max over passes is the loop length.
                    let pass = dec.duration_us.unwrap_or(0).max(pass_end_us).max(dec.frame_us);
                    let pass = dec.loop_us.map_or(pass, |l| l.max(pass));
                    dec.loop_us = Some(pass);
                    base_us += pass;
                    skip_until_us = 0;
                    pass_end_us = 0;
                    frames_this_pass = 0;
                    dec.seek(0);
                } else {
                    // Hold the last frame until the playhead moves back.
                    match self.commands.recv() {
                        Ok(cmd) => {
                            let Command::Seek { generation: g, target_us } = cmd;
                            generation = g;
                            let (base, clip) = self.split(&dec, target_us);
                            base_us = base;
                            skip_until_us = clip;
                            pass_end_us = 0;
                            frames_this_pass = 0;
                            dec.seek(clip);
                        }
                        Err(_) => return,
                    }
                }
            }
        }
    }

    /// Splits an unwrapped target into (pass base, position within the clip).
    fn split(&self, dec: &Decoder, target_us: i64) -> (i64, i64) {
        match dec.loop_us.or(dec.duration_us) {
            Some(d) if self.looped && d > 0 => {
                let clip = target_us.rem_euclid(d);
                (target_us - clip, clip)
            }
            _ => (0, target_us.max(0)),
        }
    }

    /// Non-blocking send that gives up on frames a seek has made stale.
    fn send(&self, mut item: Decoded) -> Sent {
        loop {
            if self.latest_generation.load(Ordering::Acquire) != item.generation {
                return Sent::Stale;
            }
            match self.tx.try_send(item) {
                Ok(()) => return Sent::Ok,
                Err(mpsc::TrySendError::Full(back)) => {
                    item = back;
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(mpsc::TrySendError::Disconnected(_)) => return Sent::Closed,
            }
        }
    }
}

impl Decoder {
    fn open(path: &Path) -> Result<Self, String> {
        let input = ff::format::input(path).map_err(|e| e.to_string())?;
        let stream = input.streams().best(ff::media::Type::Video).ok_or("no video stream")?;
        let index = stream.index();
        let time_base = stream.time_base();
        let start_pts = if stream.start_time() == ff::ffi::AV_NOPTS_VALUE { 0 } else { stream.start_time() };

        let rate = [stream.avg_frame_rate(), stream.rate()]
            .into_iter()
            .map(f64::from)
            .find(|r| r.is_finite() && *r > 0.0)
            .unwrap_or(25.0);
        let frame_us = ((1_000_000.0 / rate).round() as i64).max(1);
        // Some muxers report the last frame's start rather than its end, so
        // take the larger of the stream and container figures.
        let stream_us = (stream.duration() > 0).then(|| rescale_us(stream.duration(), time_base));
        let container_us = (input.duration() > 0).then(|| input.duration()); // AV_TIME_BASE is µs
        let duration_us = stream_us.max(container_us);

        let mut ctx = ff::codec::context::Context::from_parameters(stream.parameters()).map_err(|e| e.to_string())?;
        // count 0 = one thread per core.
        ctx.set_threading(ff::codec::threading::Config { kind: ff::codec::threading::Type::Frame, count: 0 });
        let hw = if hwdec_enabled() { HwDevice::create().ok() } else { None };
        if let Some(hw) = &hw {
            // Must be set before the codec is opened. FFmpeg's default
            // get_format then picks the hardware pixel format when the codec
            // supports it, and silently stays in software otherwise.
            unsafe { (*ctx.as_mut_ptr()).hw_device_ctx = ff::ffi::av_buffer_ref(hw.0) };
        }
        let decoder = ctx.decoder().video().map_err(|e| e.to_string())?;
        if decoder.width() == 0 || decoder.height() == 0 {
            return Err("video stream has no dimensions".into());
        }
        Ok(Self {
            input,
            stream: index,
            time_base,
            start_pts,
            decoder,
            scaler: None,
            _hw: hw,
            reported_path: false,
            duration_us,
            frame_us,
            loop_us: None,
        })
    }

    /// Seeks to the key frame at or before `clip_us` and resets the codec.
    fn seek(&mut self, clip_us: i64) {
        let start_us = rescale_us(self.start_pts, self.time_base);
        let ts = clip_us + start_us;
        if let Err(e) = self.input.seek(ts, ..ts + 1) {
            tracing::debug!("seek to {ts}us failed ({e}); rewinding");
            let _ = self.input.seek(start_us, ..start_us + 1);
        }
        self.decoder.flush();
    }

    /// Frame position relative to the stream start, in microseconds.
    fn position_us(&self, frame: &ff::frame::Video) -> Option<i64> {
        let pts = frame.timestamp().or(frame.pts())?;
        Some(rescale_us(pts - self.start_pts, self.time_base))
    }

    /// Hardware surface → NV12 transfer, NV12 passthrough, else RGBA via swscale.
    fn convert(&mut self, src: &ff::frame::Video) -> Result<VideoFrame, ff::Error> {
        let is_hw = unsafe { !(*src.as_ptr()).hw_frames_ctx.is_null() };
        let mut transferred;
        let frame = if is_hw {
            transferred = ff::frame::Video::empty();
            let ret = unsafe { ff::ffi::av_hwframe_transfer_data(transferred.as_mut_ptr(), src.as_ptr(), 0) };
            if ret < 0 {
                return Err(ff::Error::from(ret));
            }
            // The transfer does not carry timing/colour tags; colour comes from `src`.
            &transferred
        } else {
            src
        };
        if !self.reported_path {
            self.reported_path = true;
            tracing::info!(
                hardware = is_hw,
                format = ?frame.format(),
                size = %format!("{}x{}", frame.width(), frame.height()),
                "video decode path"
            );
        }
        if frame.format() == ff::format::Pixel::NV12 {
            Ok(nv12_frame(frame, src))
        } else {
            self.convert_rgba(frame)
        }
    }

    fn convert_rgba(&mut self, src: &ff::frame::Video) -> Result<VideoFrame, ff::Error> {
        let (fmt, w, h) = (src.format(), src.width(), src.height());
        let scaler = match &mut self.scaler {
            Some((f, sw, sh, ctx)) if (*f, *sw, *sh) == (fmt, w, h) => ctx,
            entry => {
                let ctx = ff::software::scaling::Context::get(
                fmt,
                w,
                h,
                ff::format::Pixel::RGBA,
                w,
                h,
                ff::software::scaling::Flags::BILINEAR,
                )?;
                &mut entry.insert((fmt, w, h, ctx)).3
            }
        };
        let mut rgba = ff::frame::Video::empty();
        scaler.run(src, &mut rgba)?;

        let row = w as usize * 4;
        let stride = rgba.stride(0);
        let plane = rgba.data(0);
        let mut data = Vec::with_capacity(row * h as usize);
        for y in 0..h as usize {
            data.extend_from_slice(&plane[y * stride..y * stride + row]);
        }
        Ok(VideoFrame::new(w, h, data, next_serial()))
    }
}

/// Copies an NV12 frame's two planes (dropping row padding). `tags` supplies
/// colour metadata, which hardware transfers don't copy.
fn nv12_frame(frame: &ff::frame::Video, tags: &ff::frame::Video) -> VideoFrame {
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut data = Vec::with_capacity(w * h + cw * ch * 2);
    let (ys, y) = (frame.stride(0), frame.data(0));
    for row in 0..h {
        data.extend_from_slice(&y[row * ys..row * ys + w]);
    }
    let (uvs, uv) = (frame.stride(1), frame.data(1));
    for row in 0..ch {
        data.extend_from_slice(&uv[row * uvs..row * uvs + cw * 2]);
    }
    use ff::util::color::{Range, Space};
    let matrix = match tags.color_space() {
        Space::BT709 => YuvMatrix::Bt709,
        Space::BT470BG | Space::SMPTE170M => YuvMatrix::Bt601,
        // Untagged: the usual convention is HD = 709, SD = 601.
        _ if h >= 720 => YuvMatrix::Bt709,
        _ => YuvMatrix::Bt601,
    };
    let full_range = tags.color_range() == Range::JPEG;
    VideoFrame::nv12(w as u32, h as u32, data, matrix, full_range, next_serial())
}

fn hwdec_enabled() -> bool {
    !matches!(std::env::var("LIVESTAGE_HWDEC").as_deref(), Ok("0" | "off" | "false"))
}

/// An FFmpeg hardware device context (`AVBufferRef`).
struct HwDevice(*mut ff::ffi::AVBufferRef);

impl HwDevice {
    fn device_type() -> Option<ff::ffi::AVHWDeviceType> {
        use ff::ffi::AVHWDeviceType::*;
        if cfg!(target_os = "macos") {
            Some(AV_HWDEVICE_TYPE_VIDEOTOOLBOX)
        } else if cfg!(target_os = "windows") {
            Some(AV_HWDEVICE_TYPE_D3D11VA)
        } else if cfg!(target_os = "linux") {
            Some(AV_HWDEVICE_TYPE_VAAPI)
        } else {
            None
        }
    }

    fn create() -> Result<Self, String> {
        let kind = Self::device_type().ok_or("no hardware decode API on this platform")?;
        let mut ctx: *mut ff::ffi::AVBufferRef = std::ptr::null_mut();
        let ret = unsafe { ff::ffi::av_hwdevice_ctx_create(&mut ctx, kind, std::ptr::null(), std::ptr::null_mut(), 0) };
        if ret < 0 || ctx.is_null() {
            let error = ff::Error::from(ret).to_string();
            tracing::info!(?kind, %error, "hardware video decode unavailable; using software");
            return Err(error);
        }
        Ok(Self(ctx))
    }
}

/// Display name of the platform hardware decode API.
fn hw_api_name() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some("VideoToolbox")
    } else if cfg!(target_os = "windows") {
        Some("D3D11VA")
    } else if cfg!(target_os = "linux") {
        Some("VAAPI")
    } else {
        None
    }
}

/// Startup capability probe behind [`crate::capabilities::movie_support`].
pub(crate) fn probe() -> crate::capabilities::MovieSupport {
    use crate::capabilities::{HwDecode, MovieSupport, ESSENTIAL_DECODERS, OPTIONAL_DECODERS};
    if let Err(e) = FfmpegBackend::new() {
        return MovieSupport::InitFailed(e.to_string());
    }
    let version = unsafe { std::ffi::CStr::from_ptr(ff::ffi::av_version_info()) }.to_string_lossy().into_owned();
    let hardware = match hw_api_name() {
        None => HwDecode::Unsupported,
        Some(_) if !hwdec_enabled() => HwDecode::Disabled,
        // Opening (and dropping) a device proves the driver path works.
        Some(api) => match HwDevice::create() {
            Ok(_) => HwDecode::Available { api },
            Err(reason) => HwDecode::Unavailable { api, reason },
        },
    };
    let missing_from = |list: &[(&str, &'static str)]| -> Vec<&'static str> {
        list.iter().filter(|(name, _)| ff::codec::decoder::find_by_name(name).is_none()).map(|(_, label)| *label).collect()
    };
    MovieSupport::Ready { version, hardware, missing: missing_from(ESSENTIAL_DECODERS), missing_optional: missing_from(OPTIONAL_DECODERS) }
}

impl Drop for HwDevice {
    fn drop(&mut self) {
        unsafe { ff::ffi::av_buffer_unref(&mut self.0) };
    }
}

fn rescale_us(ts: i64, time_base: ff::Rational) -> i64 {
    (ts as f64 * f64::from(time_base) * 1_000_000.0).round() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::BackendRegistry;
    use std::time::Instant;

    const W: u32 = 64;
    const H: u32 = 48;
    const FPS: i32 = 10;
    const FRAMES: i64 = 10;

    fn luma(i: i64) -> u8 {
        (16 + i * 20) as u8
    }

    /// Writes a 1 s, 10 fps clip whose frame `i` is flat grey `luma(i)`.
    /// Returns `None` when this FFmpeg build has no MPEG-4 encoder.
    fn generate_clip(path: &Path) -> Option<()> {
        FfmpegBackend::new().ok()?;
        generate_with(path, ff::encoder::find(ff::codec::Id::MPEG4)?, W, H)
    }

    /// Same clip pattern with any encoder / size.
    fn generate_with(path: &Path, codec: ff::Codec, width: u32, height: u32) -> Option<()> {
        let mut out = ff::format::output(path).ok()?;
        let global_header = out.format().flags().contains(ff::format::Flags::GLOBAL_HEADER);
        let mut enc = ff::codec::context::Context::new_with_codec(codec).encoder().video().ok()?;
        enc.set_width(width);
        enc.set_height(height);
        enc.set_format(ff::format::Pixel::YUV420P);
        enc.set_time_base((1, FPS));
        enc.set_frame_rate(Some((FPS, 1)));
        enc.set_gop(4);
        enc.set_bit_rate(4_000_000);
        if global_header {
            enc.set_flags(ff::codec::Flags::GLOBAL_HEADER);
        }
        let mut enc = enc.open_as(codec).ok()?;
        let stream_tb = {
            let mut st = out.add_stream(codec).ok()?;
            st.set_parameters(&enc);
            st.set_time_base((1, FPS));
            st.index()
        };
        out.write_header().ok()?;
        let ost_tb = out.stream(stream_tb).unwrap().time_base();

        let mut packet = ff::Packet::empty();
        let mut drain = |enc: &mut ff::encoder::Video, out: &mut ff::format::context::Output| {
            while enc.receive_packet(&mut packet).is_ok() {
                packet.set_stream(stream_tb);
                packet.rescale_ts((1, FPS), ost_tb);
                packet.write_interleaved(out).unwrap();
            }
        };
        for i in 0..FRAMES {
            let mut frame = ff::frame::Video::new(ff::format::Pixel::YUV420P, width, height);
            frame.data_mut(0).fill(luma(i));
            frame.data_mut(1).fill(128);
            frame.data_mut(2).fill(128);
            frame.set_pts(Some(i));
            enc.send_frame(&frame).ok()?;
            drain(&mut enc, &mut out);
        }
        enc.send_eof().ok()?;
        drain(&mut enc, &mut out);
        out.write_trailer().ok()?;
        Some(())
    }

    /// Expected frame index for a decoded grey level.
    fn index_of(frame: &VideoFrame) -> i64 {
        let (w, h) = (frame.width, frame.height);
        let y = match frame.format {
            // Hardware path: read luma straight from the Y plane.
            crate::source::PixelFormat::Nv12 { .. } => frame.data[(h / 2 * w + w / 2) as usize] as f64,
            crate::source::PixelFormat::Rgba8 => {
                let rgb = frame.data[((h / 2 * w + w / 2) * 4) as usize] as f64;
                // Limited-range BT.601: Y 16..235 -> RGB 0..255.
                rgb * 219.0 / 255.0 + 16.0
            }
        };
        ((y - 16.0) / 20.0).round() as i64
    }

    /// Polls a non-blocking source until it shows frame `expected`.
    fn wait_for(src: &mut dyn VideoSource, t: ShowTime, expected: i64) -> VideoFrame {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let call = Instant::now();
            let frame = src.frame_at(t);
            assert!(call.elapsed() < Duration::from_millis(50), "frame_at blocked for {:?}", call.elapsed());
            if let Some(f) = frame.filter(|f| index_of(f) == expected) {
                return f;
            }
            assert!(Instant::now() < deadline, "timed out waiting for frame {expected} at {t:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn decodes_seeks_and_loops_generated_clip() {
        let dir = std::env::temp_dir().join(format!("livestage-ffmpeg-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("clip.mkv");
        if generate_clip(&clip).is_none() {
            eprintln!("skipping: FFmpeg MPEG-4 encoder unavailable");
            return;
        }

        let registry = BackendRegistry::default();
        assert!(registry.names()[0].starts_with("ffmpeg"));

        let mut src = registry.open(&clip, false).unwrap();
        assert_eq!(src.size(), (W, H));
        let d = src.duration().expect("duration").as_secs_f64();
        assert!((d - 1.0).abs() < 0.11, "duration {d}");

        let f0 = wait_for(&mut *src, ShowTime::ZERO, 0);
        assert_eq!((f0.width, f0.height), (W, H));
        if f0.format == crate::source::PixelFormat::Rgba8 {
            assert_eq!(f0.data.len(), (W * H * 4) as usize);
            assert_eq!(f0.data[3], 255);
        }
        wait_for(&mut *src, ShowTime::from_millis(350), 3);
        // Forward through look-ahead, then backwards (a seek).
        wait_for(&mut *src, ShowTime::from_millis(850), 8);
        wait_for(&mut *src, ShowTime::from_millis(150), 1);
        // Past the end of a one-shot clip holds the last frame.
        wait_for(&mut *src, ShowTime::from_secs(5), FRAMES - 1);

        let mut looped = registry.open(&clip, true).unwrap();
        assert!(looped.duration().is_none());
        wait_for(&mut *looped, ShowTime::from_millis(50), 0);
        wait_for(&mut *looped, ShowTime::from_millis(950), 9);
        // Across the loop point with no seek, then far into a later pass.
        wait_for(&mut *looped, ShowTime::from_millis(1_250), 2);
        wait_for(&mut *looped, ShowTime::from_millis(7_450), 4);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// H.264 from the VideoToolbox encoder must come back from the hardware
    /// decoder as NV12 (no CPU colour conversion), with the right content.
    #[cfg(target_os = "macos")]
    #[test]
    fn hardware_decode_yields_nv12() {
        if !hwdec_enabled() || FfmpegBackend::new().is_err() {
            return;
        }
        let Some(codec) = ff::encoder::find_by_name("h264_videotoolbox") else {
            eprintln!("skipping: no h264_videotoolbox encoder");
            return;
        };
        let dir = std::env::temp_dir().join(format!("livestage-hwdec-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("clip.mp4");
        if generate_with(&clip, codec, 320, 240).is_none() {
            eprintln!("skipping: VideoToolbox encoder could not be opened");
            return;
        }
        let mut src = FfmpegSource::open(&clip, false).unwrap();
        let f = wait_for(&mut src, ShowTime::from_millis(550), 5);
        assert!(matches!(f.format, crate::source::PixelFormat::Nv12 { .. }), "expected hardware NV12, got {:?}", f.format);
        assert_eq!(f.data.len(), 320 * 240 + 160 * 120 * 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_non_movies() {
        let Ok(backend) = FfmpegBackend::new() else { return };
        assert!(!backend.can_open(Path::new("still.png")));
        assert!(!backend.can_open(Path::new("/definitely/missing.mov")));
        assert!(FfmpegSource::open(Path::new("/definitely/missing.mov"), false).is_err());
    }
}

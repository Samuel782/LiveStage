//! Video sources and the pluggable decoder backend trait.

use crate::{VideoError, VideoResult};
use livestage_core::time::ShowTime;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;

/// A decoded RGBA8 frame (straight alpha, row-major, tightly packed).
#[derive(Clone)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    /// Pixel bytes; layout given by `format`.
    pub data: Arc<Vec<u8>>,
    pub format: PixelFormat,
    /// Monotonic id; renderers re-upload only when it changes.
    pub serial: u64,
}

/// YUV → RGB matrix coefficients.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum YuvMatrix {
    /// SD video.
    Bt601,
    /// HD video (the default for anything ≥ 720p).
    Bt709,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// Tightly packed RGBA8, straight alpha.
    Rgba8,
    /// Bi-planar 4:2:0 as produced by hardware decoders: a full-resolution Y
    /// plane (`width * height` bytes) followed by an interleaved CbCr plane
    /// (`chroma_width() * chroma_height() * 2` bytes). Converted to RGB on the GPU.
    Nv12 { matrix: YuvMatrix, full_range: bool },
}

impl std::fmt::Debug for VideoFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "VideoFrame({}x{} {:?} #{})", self.width, self.height, self.format, self.serial)
    }
}

impl VideoFrame {
    pub fn new(width: u32, height: u32, data: Vec<u8>, serial: u64) -> Self {
        debug_assert_eq!(data.len(), (width * height * 4) as usize);
        Self { width, height, data: Arc::new(data), format: PixelFormat::Rgba8, serial }
    }

    /// An NV12 frame; `data` holds the Y plane then the CbCr plane, both tightly packed.
    pub fn nv12(width: u32, height: u32, data: Vec<u8>, matrix: YuvMatrix, full_range: bool, serial: u64) -> Self {
        let f = Self { width, height, data: Arc::new(data), format: PixelFormat::Nv12 { matrix, full_range }, serial };
        debug_assert_eq!(f.data.len(), f.y_len() + f.chroma_width() as usize * f.chroma_height() as usize * 2);
        f
    }

    pub fn chroma_width(&self) -> u32 {
        self.width.div_ceil(2)
    }
    pub fn chroma_height(&self) -> u32 {
        self.height.div_ceil(2)
    }
    /// Byte length of the Y plane of an NV12 frame.
    pub fn y_len(&self) -> usize {
        (self.width * self.height) as usize
    }

    pub fn solid(rgba: [u8; 4]) -> Self {
        Self::new(1, 1, rgba.to_vec(), 0)
    }

    /// Projector calibration grid: 1/8 grid, centre cross, circle, corner markers.
    pub fn test_pattern(width: u32, height: u32) -> Self {
        let mut d = vec![0u8; (width * height * 4) as usize];
        let (cx, cy) = (width as f32 / 2.0, height as f32 / 2.0);
        let radius = width.min(height) as f32 * 0.45;
        for y in 0..height {
            for x in 0..width {
                let i = ((y * width + x) * 4) as usize;
                let gx = (x * 8) % width < 8.max(width / 400);
                let gy = (y * 8) % height < 8.max(height / 400);
                let dist = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
                let circle = (dist - radius).abs() < 2.0;
                let cross = (x as f32 - cx).abs() < 2.0 || (y as f32 - cy).abs() < 2.0;
                let border = x < 4 || y < 4 || x >= width - 4 || y >= height - 4;
                let (r, g, b) = if border {
                    (255, 0, 0)
                } else if circle || cross {
                    (255, 255, 255)
                } else if gx || gy {
                    (0, 200, 255)
                } else {
                    // Grey ramp to judge blend gamma.
                    let v = (x * 64 / width.max(1)) as u8;
                    (v, v, v)
                };
                d[i..i + 4].copy_from_slice(&[r, g, b, 255]);
            }
        }
        Self::new(width, height, d, 1)
    }
}

/// A time-addressable visual source.
pub trait VideoSource: Send {
    /// Frame to display at `t` into the clip. May return the same frame
    /// (same `serial`) repeatedly.
    fn frame_at(&mut self, t: ShowTime) -> Option<VideoFrame>;
    fn duration(&self) -> Option<ShowTime>;
    fn size(&self) -> (u32, u32);
}

/// A decoder backend (FFmpeg, AVFoundation, GStreamer, HAP...).
pub trait VideoBackend: Send + Sync {
    fn name(&self) -> &str;
    fn can_open(&self, path: &Path) -> bool;
    fn open(&self, path: &Path, looped: bool) -> VideoResult<Box<dyn VideoSource>>;
}

// ---------------------------------------------------------------- stills --

pub struct StillSource {
    frame: VideoFrame,
}

impl StillSource {
    pub fn new(frame: VideoFrame) -> Self {
        Self { frame }
    }
}

impl VideoSource for StillSource {
    fn frame_at(&mut self, _t: ShowTime) -> Option<VideoFrame> {
        Some(self.frame.clone())
    }
    fn duration(&self) -> Option<ShowTime> {
        None
    }
    fn size(&self) -> (u32, u32) {
        (self.frame.width, self.frame.height)
    }
}

static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1_000);

pub fn next_serial() -> u64 {
    SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Decodes a PNG/JPEG still to RGBA8.
pub fn load_image(path: &Path) -> VideoResult<VideoFrame> {
    let img = image::open(path).map_err(|e| VideoError::Image { path: path.display().to_string(), msg: e.to_string() })?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Ok(VideoFrame::new(w, h, rgba.into_raw(), next_serial()))
}

// -------------------------------------------------------- image sequence --

/// Plays a folder of numbered stills (`frame_0001.png`, ...) — the classic
/// media-server interchange format. Frames are decoded ahead on a worker
/// thread so the render thread never touches the disk.
pub struct ImageSequenceSource {
    files: Arc<Vec<PathBuf>>,
    fps: f64,
    looped: bool,
    size: (u32, u32),
    rx: mpsc::Receiver<(u64, usize, VideoFrame)>,
    restart: mpsc::Sender<(u64, usize)>,
    /// Bumped on every seek so stale look-ahead frames are discarded.
    generation: u64,
    current: Option<(usize, VideoFrame)>,
    next: Option<(usize, VideoFrame)>,
}

const LOOKAHEAD: usize = 8;

impl ImageSequenceSource {
    pub fn open(dir: &Path, fps: f64, looped: bool) -> VideoResult<Self> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg"))
            })
            .collect();
        files.sort();
        let first = files.first().ok_or_else(|| VideoError::Unsupported(format!("{} has no frames", dir.display())))?;
        let probe = load_image(first)?;
        let size = (probe.width, probe.height);
        let files = Arc::new(files);

        let (tx, rx) = mpsc::sync_channel::<(u64, usize, VideoFrame)>(LOOKAHEAD);
        let (restart_tx, restart_rx) = mpsc::channel::<(u64, usize)>();
        let worker_files = files.clone();
        std::thread::Builder::new().name("livestage-imgseq".into()).spawn(move || {
            let (mut generation, mut index) = (0u64, 0usize);
            loop {
                // Seek requests take priority.
                while let Ok((g, i)) = restart_rx.try_recv() {
                    (generation, index) = (g, i);
                }
                if index >= worker_files.len() {
                    if looped {
                        index = 0;
                    } else {
                        match restart_rx.recv() {
                            Ok((g, i)) => (generation, index) = (g, i),
                            Err(_) => return,
                        }
                        continue;
                    }
                }
                let frame = match load_image(&worker_files[index]) {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::warn!("{e}");
                        index += 1;
                        continue;
                    }
                };
                if tx.send((generation, index, frame)).is_err() {
                    return;
                }
                index += 1;
            }
        })?;
        Ok(Self { files, fps: fps.max(1.0), looped, size, rx, restart: restart_tx, generation: 0, current: None, next: None })
    }
}

impl VideoSource for ImageSequenceSource {
    fn frame_at(&mut self, t: ShowTime) -> Option<VideoFrame> {
        let n = self.files.len();
        let mut want = (t.as_secs_f64() * self.fps) as usize;
        if self.looped {
            want %= n;
        } else {
            want = want.min(n - 1);
        }
        // Large jump (seek / loop point mismatch): restart the worker there.
        let have = self.current.as_ref().map(|(i, _)| *i);
        if have.is_none_or(|h| want < h || want > h + LOOKAHEAD * 2) && have != Some(want) {
            self.generation += 1;
            let _ = self.restart.send((self.generation, want));
            self.next = None;
        }
        loop {
            if let Some((i, _)) = &self.next {
                if *i <= want {
                    self.current = self.next.take();
                    if self.current.as_ref().is_some_and(|(i, _)| *i == want) {
                        break;
                    }
                    continue;
                }
                break;
            }
            match self.rx.try_recv() {
                Ok((g, i, f)) if g == self.generation => self.next = Some((i, f)),
                Ok(_) => continue, // stale look-ahead from before a seek
                Err(_) => break,
            }
        }
        self.current.as_ref().map(|(_, f)| f.clone())
    }
    fn duration(&self) -> Option<ShowTime> {
        (!self.looped).then(|| ShowTime::from_secs_f64(self.files.len() as f64 / self.fps))
    }
    fn size(&self) -> (u32, u32) {
        self.size
    }
}

/// Built-in backend: stills and image-sequence folders.
pub struct BuiltinBackend {
    pub sequence_fps: f64,
}

impl VideoBackend for BuiltinBackend {
    fn name(&self) -> &str {
        "builtin (stills, image sequences)"
    }
    fn can_open(&self, path: &Path) -> bool {
        path.is_dir()
            || path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg"))
    }
    fn open(&self, path: &Path, looped: bool) -> VideoResult<Box<dyn VideoSource>> {
        if path.is_dir() {
            Ok(Box::new(ImageSequenceSource::open(path, self.sequence_fps, looped)?))
        } else {
            Ok(Box::new(StillSource::new(load_image(path)?)))
        }
    }
}

/// Ordered list of backends; the first that can open a file wins.
pub struct BackendRegistry {
    backends: Vec<Box<dyn VideoBackend>>,
}

impl Default for BackendRegistry {
    fn default() -> Self {
        #[allow(unused_mut)]
        let mut registry = Self { backends: vec![Box::new(BuiltinBackend { sequence_fps: 30.0 })] };
        #[cfg(feature = "ffmpeg")]
        match crate::ffmpeg_backend::FfmpegBackend::new() {
            Ok(b) => registry.register(Box::new(b)),
            Err(e) => tracing::error!("FFmpeg backend disabled: {e}"),
        }
        registry
    }
}

impl BackendRegistry {
    /// Registers a backend with priority over existing ones.
    pub fn register(&mut self, backend: Box<dyn VideoBackend>) {
        self.backends.insert(0, backend);
    }
    pub fn open(&self, path: &Path, looped: bool) -> VideoResult<Box<dyn VideoSource>> {
        match self.backends.iter().find(|b| b.can_open(path)) {
            Some(b) => b.open(path, looped),
            None => Err(VideoError::Unsupported(format!(
                "no video backend for {} (movie files need the `ffmpeg` feature)",
                path.display()
            ))),
        }
    }
    pub fn names(&self) -> Vec<&str> {
        self.backends.iter().map(|b| b.name()).collect()
    }
}

/// Affine YUV → RGB transform as three rows applied to `(Y, Cb, Cr, 1)`,
/// with inputs normalised to 0..1 as sampled from `R8Unorm` / `Rg8Unorm`.
pub fn yuv_to_rgb_rows(matrix: YuvMatrix, full_range: bool) -> [[f32; 4]; 3] {
    let (kr, kb) = match matrix {
        YuvMatrix::Bt601 => (0.299f32, 0.114f32),
        YuvMatrix::Bt709 => (0.2126, 0.0722),
    };
    let kg = 1.0 - kr - kb;
    // Y' = ys * Y + yo ; C' = cs * (C - 0.5)
    let (ys, yo, cs) = if full_range { (1.0, 0.0, 1.0) } else { (255.0 / 219.0, -16.0 / 219.0, 255.0 / 224.0) };
    let rv = 2.0 * (1.0 - kr);
    let bu = 2.0 * (1.0 - kb);
    let gu = -bu * kb / kg;
    let gv = -rv * kr / kg;
    // Each row: r = ys*Y + a*cs*Cb + b*cs*Cr + (yo - 0.5*cs*(a + b))
    let row = |a: f32, b: f32| [ys, a * cs, b * cs, yo - 0.5 * cs * (a + b)];
    [row(0.0, rv), row(gu, gv), row(bu, 0.0)]
}

#[cfg(test)]
mod yuv_tests {
    use super::*;

    fn apply(m: &[[f32; 4]; 3], y: u8, cb: u8, cr: u8) -> [f32; 3] {
        let v = [y as f32 / 255.0, cb as f32 / 255.0, cr as f32 / 255.0, 1.0];
        m.map(|r| r[0] * v[0] + r[1] * v[1] + r[2] * v[2] + r[3])
    }

    #[test]
    fn limited_range_black_white_and_primaries() {
        let m = yuv_to_rgb_rows(YuvMatrix::Bt709, false);
        let close = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.01);
        assert!(close(apply(&m, 16, 128, 128), [0.0, 0.0, 0.0]));
        assert!(close(apply(&m, 235, 128, 128), [1.0, 1.0, 1.0]));
        // BT.709 limited-range pure red is (63, 102, 240).
        assert!(close(apply(&m, 63, 102, 240), [1.0, 0.0, 0.0]), "{:?}", apply(&m, 63, 102, 240));
        let full = yuv_to_rgb_rows(YuvMatrix::Bt601, true);
        assert!(close(apply(&full, 255, 128, 128), [1.0, 1.0, 1.0]));
    }
}

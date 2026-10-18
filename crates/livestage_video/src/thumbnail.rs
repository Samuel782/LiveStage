//! Small RGBA previews of media files for the operator UI (next-cue
//! previews, multiview): stills are downscaled, movies yield a poster frame.
//! Runs on UI worker threads, never on the render or show thread.

use crate::source::{next_serial, BackendRegistry, PixelFormat, VideoFrame};
use crate::{VideoError, VideoResult};
use livestage_core::time::ShowTime;
use std::path::Path;
use std::time::{Duration, Instant};

/// How long to wait for an asynchronous decoder to deliver a poster frame.
const POSTER_TIMEOUT: Duration = Duration::from_secs(4);

/// Converts any frame to tightly packed RGBA8 on the CPU.
pub fn to_rgba(frame: &VideoFrame) -> VideoFrame {
    let PixelFormat::Nv12 { matrix, full_range } = frame.format else { return frame.clone() };
    let rows = crate::source::yuv_to_rgb_rows(matrix, full_range);
    let (w, h) = (frame.width as usize, frame.height as usize);
    let cw = frame.chroma_width() as usize;
    let (y_plane, uv) = frame.data.split_at(frame.y_len());
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let l = y_plane[y * w + x] as f32 / 255.0;
            let ci = ((y / 2) * cw + x / 2) * 2;
            let (cb, cr) = (uv.get(ci).copied().unwrap_or(128) as f32 / 255.0, uv.get(ci + 1).copied().unwrap_or(128) as f32 / 255.0);
            for r in &rows {
                out.push(((r[0] * l + r[1] * cb + r[2] * cr + r[3]).clamp(0.0, 1.0) * 255.0).round() as u8);
            }
            out.push(255);
        }
    }
    VideoFrame::new(frame.width, frame.height, out, next_serial())
}

/// Downscales an RGBA frame to fit within `max` (keeping aspect).
pub fn fit(frame: &VideoFrame, max: (u32, u32)) -> VideoFrame {
    let frame = to_rgba(frame);
    let scale = (max.0 as f32 / frame.width.max(1) as f32).min(max.1 as f32 / frame.height.max(1) as f32);
    if scale >= 1.0 {
        return frame;
    }
    let (w, h) = (((frame.width as f32 * scale).round() as u32).max(1), ((frame.height as f32 * scale).round() as u32).max(1));
    let Some(img) = image::RgbaImage::from_raw(frame.width, frame.height, frame.data.to_vec()) else { return frame };
    let small = image::imageops::resize(&img, w, h, image::imageops::FilterType::Triangle);
    VideoFrame::new(w, h, small.into_raw(), next_serial())
}

/// A preview of `path` at `at` (movies), fitted into `max`.
pub fn thumbnail(path: &Path, at: ShowTime, max: (u32, u32), backends: &BackendRegistry) -> VideoResult<VideoFrame> {
    let mut src = backends.open(path, false)?;
    let deadline = Instant::now() + POSTER_TIMEOUT;
    loop {
        if let Some(f) = src.frame_at(at) {
            return Ok(fit(&f, max));
        }
        if Instant::now() > deadline {
            return Err(VideoError::Decode { path: path.display().to_string(), msg: "no frame for preview".into() });
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::YuvMatrix;

    #[test]
    fn nv12_to_rgba_and_fit() {
        // 4x2 mid-grey NV12 (full range): Y = 128, Cb = Cr = 128.
        let f = VideoFrame::nv12(4, 2, vec![128; 4 * 2 + 2 * 2], YuvMatrix::Bt709, true, 1);
        let rgba = to_rgba(&f);
        assert_eq!(rgba.format, PixelFormat::Rgba8);
        assert!(rgba.data.chunks(4).all(|p| (p[0] as i32 - 128).abs() <= 1 && p[3] == 255));
        let big = VideoFrame::new(400, 200, vec![255; 400 * 200 * 4], 2);
        let small = fit(&big, (100, 100));
        assert_eq!((small.width, small.height), (100, 50));
    }

    #[test]
    fn still_thumbnail() {
        let dir = std::env::temp_dir().join(format!("livestage-thumb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.png");
        image::RgbaImage::from_pixel(640, 360, image::Rgba([10, 20, 30, 255])).save(&p).unwrap();
        let t = thumbnail(&p, ShowTime::ZERO, (320, 320), &BackendRegistry::default()).unwrap();
        assert_eq!((t.width, t.height), (320, 180));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

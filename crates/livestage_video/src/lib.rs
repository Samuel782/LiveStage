//! # livestage_video
//!
//! GPU render engine built on `wgpu`.
//!
//! ```text
//!  VideoAction ─▶ Compositor (per logical output: z-ordered layers, fades)
//!                     │
//!   per output window ▼
//!   ┌───────────────── OutputRenderer ─────────────────┐
//!   │ pass A: layers ─▶ canvas texture (canvas res)     │  (+ UI draws text /
//!   │                                                  │   timer overlays here)
//!   │ pass B: canvas ─▶ Bezier warp mesh ─▶ surface     │
//!   │         with edge blend, polygon mask, brightness │
//!   │ optional: canvas ─▶ low-res readback ─▶ pixel map │
//!   └──────────────────────────────────────────────────┘
//! ```
//!
//! Video decoding is pluggable through [`source::VideoBackend`]. The
//! built-in backends are stills (PNG/JPEG), image sequences, solid colours
//! and a calibration pattern. Movie files are decoded by
//! [`ffmpeg_backend::FfmpegBackend`] when the `ffmpeg` feature is enabled;
//! other codec backends (AVFoundation, Media Foundation, GStreamer) would
//! implement the same trait.

pub mod compositor;
#[cfg(feature = "ffmpeg")]
pub mod ffmpeg_backend;
pub mod gpu;
pub mod ndi;
pub mod output;
pub mod pipelines;
pub mod slides;
pub mod source;
pub mod thumbnail;

pub use compositor::{Compositor, Overlay, OverlayKind};
pub use gpu::GpuContext;
pub use output::OutputRenderer;
pub use pipelines::Pipelines;

/// Texture format used for canvases and window surfaces. Non-sRGB so that
/// egui overlays and video (already gamma-encoded) blend identically.
pub const CANVAS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

#[derive(Debug, thiserror::Error)]
pub enum VideoError {
    #[error("no suitable GPU adapter found")]
    NoAdapter,
    #[error("GPU device request failed: {0}")]
    Device(#[from] wgpu::RequestDeviceError),
    #[error("image decode failed for {path}: {msg}")]
    Image { path: String, msg: String },
    #[error("video decode failed for {path}: {msg}")]
    Decode { path: String, msg: String },
    #[error("unsupported media: {0}")]
    Unsupported(String),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
}

pub type VideoResult<T> = Result<T, VideoError>;

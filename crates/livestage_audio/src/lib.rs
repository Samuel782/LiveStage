//! # livestage_audio
//!
//! Multi-channel, low-latency audio playback for live shows.
//!
//! ```text
//!  control thread (UI / show)                      cpal real-time thread
//!  ──────────────────────────                      ─────────────────────
//!  AudioEngine ── RtCommand (SPSC ringbuf) ──────▶ Renderer::render()
//!      ▲                                              │  mixes VoiceGroups
//!      └──── Garbage (SPSC ringbuf) ◀─────────────────┘  through RoutingTable
//!      └──── atomics: positions, meters, xruns ◀──────┘
//! ```
//!
//! **Real-time guarantees in `Renderer::render`:** no allocation, no
//! deallocation, no locks, no syscalls. Everything the audio thread needs is
//! allocated on the control thread and *moved* in through a lock-free SPSC
//! ring buffer; everything it is done with is moved back out through a
//! second ring buffer and dropped on the control thread.

pub mod clip;
pub mod decode;
pub mod device;
pub mod engine;
pub mod ltc;
pub mod metronome;
pub mod renderer;
pub mod routing;
pub mod stream;

pub use clip::AudioClip;
pub use device::{list_output_devices, OutputDeviceInfo};
pub use engine::{AudioEngine, ClockReading, EngineConfig, VoiceClock};
pub use renderer::Renderer;

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("decode error in {path}: {msg}")]
    Decode { path: String, msg: String },
    #[error("no audio track in {0}")]
    NoTrack(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("audio device error: {0}")]
    Device(String),
    #[error("asset could not be resolved: {0}")]
    Unresolved(String),
}

pub type AudioResult<T> = Result<T, AudioError>;

/// dB → linear gain. Values ≤ -96 dB are treated as silence.
#[inline]
pub fn db_to_gain(db: f32) -> f32 {
    if db <= -96.0 {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

/// Linear gain → dB (for meters).
#[inline]
pub fn gain_to_db(g: f32) -> f32 {
    if g <= 1e-6 {
        -120.0
    } else {
        20.0 * g.log10()
    }
}

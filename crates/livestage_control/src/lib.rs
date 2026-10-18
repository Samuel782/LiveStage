//! # livestage_control
//!
//! External control surfaces. Every input — OSC, MIDI notes/CC/PC, MIDI
//! Show Control, WebSocket JSON, the keyboard panic key — is normalised to a
//! core [`ShowCommand`] and delivered to the show thread through a single
//! [`ControlHub`] channel, so the show thread has exactly one input queue.
//! Timecode (MTC, Art-Net) is a stream rather than commands and arrives on
//! its own channel of time-stamped samples ([`timecode`]).

pub mod hub;
pub mod midi;
pub mod osc;
pub mod panic;
pub mod timecode;
pub mod websocket;

pub use hub::{ControlEvent, ControlHub, ControlSource};
pub use livestage_core::ShowCommand;
pub use panic::PanicController;

#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("MIDI: {0}")]
    Midi(String),
    #[error("OSC: {0}")]
    Osc(String),
}

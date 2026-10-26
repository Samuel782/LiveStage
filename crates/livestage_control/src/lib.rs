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

/// Control input arrives from the network and MIDI ports; no packet, however
/// malformed, may panic a parser (it would kill the listener thread).
#[cfg(test)]
mod robustness {
    /// Deterministic pseudo-random packets: pure noise, and valid headers
    /// followed by noise of every length.
    fn packets() -> impl Iterator<Item = Vec<u8>> {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let headers: [&[u8]; 6] = [b"", b"/livestage/go\0\0\0,", b"#bundle\0", b"Art-Net\0\x00\x97", &[0xF0, 0x7F], &[0xF1]];
        (0..20_000).map(move |i| {
            let mut p = headers[i % headers.len()].to_vec();
            let len = (next() % 64) as usize;
            p.extend((0..len).map(|_| next() as u8));
            p
        })
    }

    #[test]
    fn malformed_input_never_panics() {
        let mut mtc = crate::timecode::MtcDecoder::default();
        for p in packets() {
            if let Ok((_, packet)) = rosc::decoder::decode_udp(&p) {
                let mut cmds = Vec::new();
                crate::osc::walk(packet, &mut cmds);
            }
            let _ = crate::midi::parse(&p, 0x7F);
            let _ = crate::timecode::parse_art_timecode(&p);
            let _ = mtc.message(&p);
        }
    }
}

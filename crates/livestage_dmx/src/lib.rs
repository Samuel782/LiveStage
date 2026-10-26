//! # livestage_dmx
//!
//! * [`artnet`] / [`sacn`] — bit-exact packet encoders (Art-Net 4 ArtDmx,
//!   ANSI E1.31-2018 data packets).
//! * [`output`] — a paced UDP transmitter thread.
//! * [`sacn_in`] — sACN reception, E1.31 source merging and input patching.
//! * [`fixture`] — fixture definitions, built-in library and the patch.
//! * [`import`] — GDTF and Open Fixture Library profile import.
//! * [`playback`] — LTP crossfade engine driven by DMX cues.
//! * [`console`] — programmer, palettes, effects, submasters and masters
//!   layered on top of playback.
//! * [`pixelmap`] — samples video canvases onto LED matrices.
//! * [`engine`] — glue implementing core `DmxAction`s.

pub mod artnet;
pub mod console;
pub mod engine;
pub mod fixture;
pub mod import;
pub mod output;
pub mod pixelmap;
pub mod playback;
pub mod sacn;
pub mod sacn_in;
pub mod universe;

pub use engine::DmxEngine;
pub use universe::Universe;

#[derive(Debug, thiserror::Error)]
pub enum DmxError {
    #[error("network error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid destination address `{0}`")]
    BadAddress(String),
    #[error("patch error: {0}")]
    Patch(String),
}

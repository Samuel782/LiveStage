//! Execution states and the colour language shared by every LiveStage UI.

use serde::{Deserialize, Serialize};

/// sRGB colour, 8 bits per channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub const fn hex(v: u32) -> Self {
        Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
    }
    pub fn to_hex_string(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.0, self.1, self.2)
    }
}

/// Canonical state palette.
pub mod palette {
    use super::Rgb;
    /// Standby — Cyberpunk Amber `#FFB020`.
    pub const STANDBY: Rgb = Rgb::hex(0xFFB020);
    /// Active / playing — Laser Emerald `#00F5A0`.
    pub const ACTIVE: Rgb = Rgb::hex(0x00F5A0);
    /// GO key — Laser Emerald, one step deeper for a large lit surface `#00E091`.
    pub const GO: Rgb = Rgb::hex(0x00E091);
    /// Stop / Panic / errors — Deep Crimson `#D3123A`.
    pub const STOP: Rgb = Rgb::hex(0xD3123A);
}

/// Lifecycle state of a single cue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CueState {
    /// Not loaded, not armed for the next GO.
    #[default]
    Idle,
    /// Under the playhead: assets prepared, the next GO fires it.
    Standby,
    /// GO received, counting down its pre-wait.
    PreWait,
    /// Running.
    Running,
    /// Paused mid-run (resumable).
    Paused,
    /// Finished naturally (its effect may persist, e.g. DMX tracking).
    Completed,
    /// Stopped by the operator or by panic.
    Stopped,
}

impl CueState {
    pub fn is_active(self) -> bool {
        matches!(self, CueState::PreWait | CueState::Running | CueState::Paused)
    }
    /// Colour used to render this state in the cue grid.
    pub fn color(self) -> Option<Rgb> {
        match self {
            CueState::Standby => Some(palette::STANDBY),
            CueState::PreWait | CueState::Running | CueState::Paused => Some(palette::ACTIVE),
            CueState::Stopped => Some(palette::STOP),
            CueState::Idle | CueState::Completed => None,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            CueState::Idle => "",
            CueState::Standby => "STANDBY",
            CueState::PreWait => "WAIT",
            CueState::Running => "PLAY",
            CueState::Paused => "PAUSE",
            CueState::Completed => "DONE",
            CueState::Stopped => "STOP",
        }
    }
}

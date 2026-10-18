//! Operator-level commands. Every control surface (keyboard, MIDI, OSC,
//! WebSocket, UI buttons) is translated into a [`ShowCommand`].

use crate::id::CueListId;
use crate::time::ShowTime;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum ShowCommand {
    /// Fire the standby cue of the given (or active) cue list.
    Go { list: Option<CueListId> },
    /// Fire a cue by its number, e.g. `"12.5"`.
    GoCue { number: String },
    /// Move the standby pointer.
    StandbyNext,
    StandbyPrevious,
    SetStandby { number: String },
    /// Scrub a cue (by number, in the active list) to `position`, even while
    /// it is playing.
    SeekCue { number: String, position: ShowTime },
    /// Advance the most recently started running slide cue.
    NextSlide,
    PrevSlide,
    /// Jump the most recently started running slide cue to slide `slide` (1-based).
    GotoSlide { slide: usize },
    /// Pause / resume everything that is running.
    PauseAll,
    ResumeAll,
    /// Stop everything, with an optional fade.
    StopAll { fade: Option<ShowTime> },
    /// Global panic. `hard = false` fades out quickly; `hard = true` cuts
    /// everything immediately (audio to silence, video to black, DMX blackout).
    Panic { hard: bool },
    /// Master output level in dB.
    SetMasterGain { db: f32 },
    /// Switch all program outputs in/out of kiosk fullscreen.
    SetOutputsFullscreen { enabled: bool },
    /// Press (`down`) or release a cart pad. `cart` and `pad` are 1-based
    /// (pads row by row).
    CartPad { cart: usize, pad: usize, down: bool },
    /// Stop every playing pad of every cart.
    CartStopAll,
    /// Lighting: submaster fader level (1-based, 0..=1).
    SetSubmaster { sub: usize, level: f32 },
    /// Lighting: hold (`down`) / release a submaster's flash button.
    FlashSubmaster { sub: usize, down: bool },
    SetGrandMaster { level: f32 },
    /// Lighting: desk blackout (intensities to zero; positions and colours hold).
    SetBlackout { enabled: bool },
    /// Lighting: start / stop an effect (1-based) from the console.
    SetEffect { effect: usize, running: bool },
    /// Lighting: clear the programmer.
    ClearProgrammer,
    /// Internal timecode generator transport (ignored for external sources).
    TimecodeStart,
    TimecodeStop,
    /// Move the internal generator to a label (its rate is kept).
    TimecodeLocate { timecode: crate::timecode::Timecode },
    /// Request a state snapshot (remote API).
    RequestStatus,
}

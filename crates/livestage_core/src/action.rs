//! Engine actions: the only way cues affect the outside world.
//!
//! Cues never call an engine directly. They emit [`EngineAction`]s through
//! the [`CueContext`](crate::cue::CueContext); the application routes them to
//! the audio, video, DMX and control engines. This keeps `livestage_core`
//! hardware-free, deterministic and unit-testable.

use crate::id::{CueId, FixtureId, OutputId, StemId};
use crate::project::AssetRef;
use crate::time::ShowTime;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq)]
pub enum EngineAction {
    Audio(AudioAction),
    Video(VideoAction),
    Dmx(DmxAction),
    Control(ControlAction),
}

/// One file played through one stem. Several tracks started by the same
/// [`AudioAction::Play`] are started on the same sample frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioTrack {
    pub asset: AssetRef,
    pub stem: StemId,
    #[serde(default)]
    pub gain_db: f32,
    #[serde(default)]
    pub mute: bool,
}

/// Generated click track that runs in sync with an audio cue.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClickTrack {
    pub bpm: f64,
    pub beats_per_bar: u32,
    pub stem: StemId,
    #[serde(default = "default_click_gain")]
    pub gain_db: f32,
    /// Bars of count-in before the cue's audio starts.
    #[serde(default)]
    pub count_in_bars: u32,
}
fn default_click_gain() -> f32 {
    -6.0
}

#[derive(Clone, Debug, PartialEq)]
pub enum AudioAction {
    Play {
        cue: CueId,
        tracks: Vec<AudioTrack>,
        click: Option<ClickTrack>,
        gain_db: f32,
        fade_in: ShowTime,
        /// Start position inside the files.
        start_at: ShowTime,
        /// Optional end position (exclusive).
        end_at: Option<ShowTime>,
        looped: bool,
    },
    Stop { cue: CueId, fade: ShowTime },
    Pause { cue: CueId },
    Resume { cue: CueId },
    Seek { cue: CueId, position: ShowTime },
    SetGain { cue: CueId, gain_db: f32, fade: ShowTime },
    /// Preload / decode assets so that GO is instant.
    Preload { assets: Vec<AssetRef> },
    StopAll { fade: ShowTime },
    SetMasterGain { db: f32 },
}

/// What a visual layer shows. Any content can be routed to any output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VisualContent {
    Video { asset: AssetRef, looped: bool },
    Image { asset: AssetRef },
    Text(TextContent),
    Timer(TimerContent),
    Ndi { source_name: String },
    /// Solid colour fill (useful for blackouts, test fills, wash looks).
    Color { rgba: [f32; 4] },
    /// Calibration grid for projection alignment.
    TestPattern,
    /// A rendered presentation (PDF / PowerPoint): one image per slide,
    /// showing slide `index` (0-based).
    Slides { slides: Vec<AssetRef>, index: usize },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextContent {
    pub text: String,
    pub font_size: f32,
    pub color: [f32; 4],
    /// Teleprompter scroll speed in points per second (0 = static).
    #[serde(default)]
    pub scroll_speed: f32,
    #[serde(default)]
    pub mirror_horizontal: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum TimerMode {
    CountUp,
    CountDown { from: ShowTime },
    TimeOfDay,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimerContent {
    pub mode: TimerMode,
    pub label: String,
    pub font_size: f32,
    pub color: [f32; 4],
    /// Colour switched to in the final `warn_below` of a countdown.
    pub warn_color: [f32; 4],
    pub warn_below: ShowTime,
    /// Wall-clock show time at which the timer started (filled at GO).
    #[serde(skip)]
    pub started_at: ShowTime,
    /// Time already elapsed at `started_at` (start mid-way, seek).
    #[serde(skip)]
    pub elapsed_offset: ShowTime,
}

#[derive(Clone, Debug, PartialEq)]
pub enum VideoAction {
    Show {
        cue: CueId,
        output: OutputId,
        content: VisualContent,
        layer: i32,
        opacity: f32,
        fade_in: ShowTime,
        start_at: ShowTime,
    },
    Hide { cue: CueId, fade: ShowTime },
    Pause { cue: CueId },
    Resume { cue: CueId },
    Seek { cue: CueId, position: ShowTime },
    SetOpacity { cue: CueId, opacity: f32, fade: ShowTime },
    /// Change the slide of a [`VisualContent::Slides`] layer, cross-fading over `fade`.
    GotoSlide { cue: CueId, index: usize, fade: ShowTime },
    Preload { assets: Vec<AssetRef> },
    /// Fade every output to black.
    BlackoutAll { fade: ShowTime },
}

/// A raw channel level: universe (internal index), channel 1..=512.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmxChannelValue {
    pub universe: u16,
    pub channel: u16,
    pub value: u8,
}

/// A fixture attribute level, resolved through the patch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FixtureAttributeValue {
    pub fixture: FixtureId,
    /// Attribute name in the fixture definition, e.g. `"intensity"`, `"red"`, `"pan"`.
    pub attribute: String,
    /// Normalised 0.0..=1.0 (16-bit attributes use the full precision).
    pub value: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DmxAction {
    ApplyScene {
        cue: CueId,
        channels: Vec<DmxChannelValue>,
        attributes: Vec<FixtureAttributeValue>,
        /// Resolved against the current palettes when applied.
        palettes: Vec<crate::lighting::PaletteRef>,
        effects: Vec<crate::id::EffectId>,
        fade: ShowTime,
    },
    Release { cue: CueId, fade: ShowTime },
    Blackout { fade: ShowTime },
    /// Enable / disable the pixel mapper for a given map index.
    SetPixelMap { index: usize, enabled: bool },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OscArg {
    Int(i32),
    Float(f32),
    String(String),
    Bool(bool),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlAction {
    SendOsc { host: String, port: u16, address: String, args: Vec<OscArg> },
    SendMidi { port: Option<String>, bytes: Vec<u8> },
    /// Run another show command (e.g. GO another cue list).
    Command(crate::command::ShowCommand),
    /// Log a marker (useful for show reports and rehearsals).
    Marker { text: String },
}

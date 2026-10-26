//! Serialisable DMX configuration stored in the project manifest.

use crate::id::{FixtureId, OutputId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "protocol", rename_all = "snake_case")]
pub enum DmxTransport {
    /// Art-Net 4. `port_address` is the 15-bit Net:SubNet:Universe.
    ArtNet { port_address: u16, destination: String },
    /// E1.31 sACN. `destination = None` uses the standard multicast group.
    Sacn { universe: u16, priority: u8, destination: Option<String> },
    /// Not transmitted (for pixel-map previews / offline programming).
    Disabled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UniverseConfig {
    /// Internal universe index used by cues and the patch.
    pub index: u16,
    pub name: String,
    pub transport: DmxTransport,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PatchedFixture {
    pub id: FixtureId,
    pub name: String,
    /// Key of the fixture definition in the library, e.g. `"generic/rgbw-par"`.
    pub definition: String,
    pub mode: String,
    pub universe: u16,
    /// 1-based start address.
    pub address: u16,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixelOrder {
    #[default]
    Rgb,
    Grb,
    Brg,
    Rgbw,
}

impl PixelOrder {
    pub fn channels(self) -> u16 {
        if self == PixelOrder::Rgbw {
            4
        } else {
            3
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixelWiring {
    /// Every row starts on the left.
    #[default]
    RowsLeftToRight,
    /// Snake: even rows left->right, odd rows right->left.
    Serpentine,
    /// Column-major snake (common for vertical LED strips).
    ColumnSerpentine,
}

/// Samples a region of a logical display's canvas and drives an LED matrix.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PixelMapConfig {
    pub name: String,
    pub source: OutputId,
    /// Region of the source canvas, normalised `[x, y, w, h]`.
    pub region: [f32; 4],
    pub width: u32,
    pub height: u32,
    pub order: PixelOrder,
    pub wiring: PixelWiring,
    pub start_universe: u16,
    /// 1-based.
    pub start_address: u16,
    pub enabled: bool,
}

/// Where an imported fixture profile came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileSource {
    /// GDTF (`.gdtf`, DIN SPEC 15800).
    Gdtf,
    /// Open Fixture Library JSON.
    Ofl,
}

impl ProfileSource {
    pub fn label(self) -> &'static str {
        match self {
            ProfileSource::Gdtf => "GDTF",
            ProfileSource::Ofl => "OFL",
        }
    }
}

/// One DMX attribute of an imported mode.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProfileAttribute {
    /// Attribute name used by cues and palettes (`intensity`, `red`, `pan`, ...).
    pub name: String,
    /// Attribute kind (`intensity`, `red`, `pan`, `gobo`, `generic`, ...).
    pub kind: String,
    /// 0-based offset of the coarse byte inside the footprint.
    pub coarse: u16,
    /// 0-based offset of the fine byte of a 16-bit attribute.
    #[serde(default)]
    pub fine: Option<u16>,
    /// Home value, normalised 0..=1.
    #[serde(default)]
    pub default: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProfileMode {
    pub name: String,
    pub footprint: u16,
    pub attributes: Vec<ProfileAttribute>,
}

/// A fixture type imported from GDTF or the Open Fixture Library. Stored in
/// the show, so a show file opens with its fixture types on any machine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FixtureProfile {
    /// Library key the patch refers to, e.g. `ofl/cameo/auro-spot-300`.
    pub key: String,
    pub manufacturer: String,
    pub model: String,
    pub source: ProfileSource,
    pub modes: Vec<ProfileMode>,
}

/// How an input universe combines with LiveStage's own output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputMerge {
    /// Highest takes precedence, per channel.
    #[default]
    Htp,
    /// The input replaces the show's values while a source is sending.
    Override,
}

impl InputMerge {
    pub const ALL: [InputMerge; 2] = [InputMerge::Htp, InputMerge::Override];
    pub fn label(self) -> &'static str {
        match self {
            InputMerge::Htp => "HTP merge",
            InputMerge::Override => "Override",
        }
    }
}

/// Patches channels of a received sACN universe onto an output universe.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InputPatch {
    /// sACN universe to receive (1..=63999).
    pub sacn_universe: u16,
    /// First received channel (1-based).
    pub from: u16,
    /// Number of channels.
    pub count: u16,
    /// Internal output universe index.
    pub universe: u16,
    /// First output channel (1-based).
    pub to: u16,
    pub merge: InputMerge,
    pub enabled: bool,
}

impl InputPatch {
    /// A whole universe, merged HTP onto the same-numbered output.
    pub fn whole(sacn_universe: u16, universe: u16) -> Self {
        Self { sacn_universe, from: 1, count: 512, universe, to: 1, merge: InputMerge::Htp, enabled: true }
    }
}

/// sACN reception: merged per the E1.31 rules (priority, then HTP between
/// sources of equal priority) and patched onto output universes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SacnInputConfig {
    pub enabled: bool,
    pub patches: Vec<InputPatch>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DmxConfig {
    pub universes: Vec<UniverseConfig>,
    pub patch: Vec<PatchedFixture>,
    pub pixel_maps: Vec<PixelMapConfig>,
    /// Fixture types imported from GDTF / OFL files.
    #[serde(default)]
    pub profiles: Vec<FixtureProfile>,
    #[serde(default)]
    pub sacn_input: SacnInputConfig,
    /// Transmit rate in Hz (Art-Net / sACN typically 30–44).
    pub refresh_hz: f32,
    /// How often the lighting layers are rendered on the show thread, in Hz.
    /// Clamped to `refresh_hz..=200`; cues still run at the show-thread rate.
    #[serde(default = "default_render_hz")]
    pub render_hz: f32,
}

fn default_render_hz() -> f32 {
    100.0
}

impl Default for DmxConfig {
    fn default() -> Self {
        Self {
            universes: vec![UniverseConfig {
                index: 0,
                name: "Universe 1".into(),
                transport: DmxTransport::Sacn { universe: 1, priority: 100, destination: None },
            }],
            patch: vec![],
            pixel_maps: vec![],
            profiles: vec![],
            sacn_input: SacnInputConfig::default(),
            refresh_hz: 40.0,
            render_hz: default_render_hz(),
        }
    }
}

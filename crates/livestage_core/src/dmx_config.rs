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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DmxConfig {
    pub universes: Vec<UniverseConfig>,
    pub patch: Vec<PatchedFixture>,
    pub pixel_maps: Vec<PixelMapConfig>,
    /// Transmit rate in Hz (Art-Net / sACN typically 30–44).
    pub refresh_hz: f32,
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
            refresh_hz: 40.0,
        }
    }
}

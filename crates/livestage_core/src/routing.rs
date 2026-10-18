//! Routing abstractions: logical displays and the audio patch bay.
//!
//! LiveStage separates *logical* targets (what the show designer names:
//! "Main LED Wall", "FOH Main", "Click") from *physical* targets (a monitor
//! port, a sound-card channel). Shows are authored against logical targets,
//! so a show moves between venues by re-patching, never by editing cues.

use crate::id::{BusId, OutputId, StemId};
use crate::projection::ProjectionSetup;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------- displays --

/// Information about a monitor detected by the OS at runtime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalDisplayInfo {
    pub name: String,
    pub position: [i32; 2],
    pub size: [u32; 2],
    pub refresh_millihertz: Option<u32>,
    pub scale_factor_milli: u32,
    pub is_primary: bool,
}

/// A persisted reference to a physical display. Monitors have no stable OS
/// id across reboots, so we match on name + position + size with fallbacks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalDisplayKey {
    pub name: String,
    pub position: [i32; 2],
    pub size: [u32; 2],
}

impl From<&PhysicalDisplayInfo> for PhysicalDisplayKey {
    fn from(i: &PhysicalDisplayInfo) -> Self {
        Self { name: i.name.clone(), position: i.position, size: i.size }
    }
}

impl PhysicalDisplayKey {
    /// Finds the best matching display: exact match, then same name + size,
    /// then same name, then same position + size.
    pub fn best_match(&self, available: &[PhysicalDisplayInfo]) -> Option<usize> {
        let score = |d: &PhysicalDisplayInfo| -> u8 {
            let name = d.name == self.name;
            let pos = d.position == self.position;
            let size = d.size == self.size;
            match (name, pos, size) {
                (true, true, true) => 4,
                (true, _, true) => 3,
                (true, _, _) => 2,
                (false, true, true) => 1,
                _ => 0,
            }
        };
        available
            .iter()
            .enumerate()
            .map(|(i, d)| (i, score(d)))
            .filter(|(_, s)| *s > 0)
            .max_by_key(|(_, s)| *s)
            .map(|(i, _)| i)
    }
}

/// A named output channel defined by the operator. Any visual content can be
/// routed to any logical display.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogicalDisplay {
    pub id: OutputId,
    pub name: String,
    /// Physical monitor this output is mapped to (None = preview only).
    pub physical: Option<PhysicalDisplayKey>,
    /// Canvas resolution content is composed at. None = native resolution.
    pub canvas_size: Option<[u32; 2]>,
    /// Kiosk fullscreen when the show is live.
    pub fullscreen: bool,
    pub enabled: bool,
    #[serde(default)]
    pub projection: ProjectionSetup,
    /// Publish this output's program canvas as an NDI source with this name.
    #[serde(default)]
    pub ndi_output: Option<String>,
    /// NDI stream resolution (None = same as the canvas). Scaled on the GPU,
    /// letterboxed if the aspect ratio differs.
    #[serde(default)]
    pub ndi_resolution: Option<[u32; 2]>,
}

impl LogicalDisplay {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: OutputId::new(),
            name: name.into(),
            physical: None,
            canvas_size: Some([1920, 1080]),
            fullscreen: true,
            enabled: true,
            projection: ProjectionSetup::default(),
            ndi_output: None,
            ndi_resolution: None,
        }
    }
}

// ------------------------------------------------------------------- audio --

/// A logical audio stem. Audio cue tracks play *into* stems; the patch bay
/// routes stems to physical outputs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stem {
    pub id: StemId,
    pub name: String,
    /// Number of source channels a stem carries (1 = mono, 2 = stereo ...).
    pub channels: u16,
    pub gain_db: f32,
    pub mute: bool,
}

/// A named group of physical outputs, used as a patching shortcut.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutputBus {
    pub id: BusId,
    pub name: String,
    /// 0-based physical output channels.
    pub channels: Vec<u16>,
}

/// One point in the patch matrix: stem source channel -> physical output.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Crosspoint {
    pub stem: StemId,
    pub source_channel: u16,
    pub output: u16,
    pub gain_db: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioRouting {
    /// Preferred device (by name). None = system default output.
    pub device_name: Option<String>,
    pub sample_rate: Option<u32>,
    pub buffer_frames: Option<u32>,
    /// Number of physical outputs the show expects.
    pub output_channels: u16,
    /// Human labels for physical outputs ("1 FOH L", ...).
    pub output_labels: Vec<String>,
    pub stems: Vec<Stem>,
    pub buses: Vec<OutputBus>,
    pub crosspoints: Vec<Crosspoint>,
    pub master_gain_db: f32,
}

impl Default for AudioRouting {
    /// FOH Main on 1-2, Click on 3-4, In-Ears on 5-8.
    fn default() -> Self {
        let stem = |name: &str, channels| Stem {
            id: StemId::new(),
            name: name.into(),
            channels,
            gain_db: 0.0,
            mute: false,
        };
        let bus = |name: &str, channels: Vec<u16>| OutputBus { id: BusId::new(), name: name.into(), channels };
        let main = stem("Backing Tracks", 2);
        let click = stem("Click", 1);
        let iem = stem("In-Ear Mix", 2);
        let foh = bus("FOH Main", vec![0, 1]);
        let click_bus = bus("Click", vec![2, 3]);
        let iem_bus = bus("In-Ears", vec![4, 5, 6, 7]);
        let mut r = Self {
            device_name: None,
            sample_rate: None,
            buffer_frames: Some(256),
            output_channels: 8,
            output_labels: (1..=8).map(|i| format!("Out {i}")).collect(),
            stems: vec![],
            buses: vec![],
            crosspoints: vec![],
            master_gain_db: 0.0,
        };
        r.patch_stem_to_bus(&main, &foh);
        r.patch_stem_to_bus(&click, &click_bus);
        r.patch_stem_to_bus(&iem, &iem_bus);
        r.stems = vec![main, click, iem];
        r.buses = vec![foh, click_bus, iem_bus];
        r
    }
}

impl AudioRouting {
    pub fn stem(&self, id: StemId) -> Option<&Stem> {
        self.stems.iter().find(|s| s.id == id)
    }
    pub fn stem_by_name(&self, name: &str) -> Option<&Stem> {
        self.stems.iter().find(|s| s.name.eq_ignore_ascii_case(name))
    }

    /// Replaces the stem's crosspoints so it feeds every channel of `bus`.
    /// A mono stem feeds all bus channels; a stereo stem alternates L/R.
    pub fn patch_stem_to_bus(&mut self, stem: &Stem, bus: &OutputBus) {
        self.crosspoints.retain(|c| c.stem != stem.id);
        for (i, &out) in bus.channels.iter().enumerate() {
            let src = (i as u16) % stem.channels.max(1);
            self.crosspoints.push(Crosspoint { stem: stem.id, source_channel: src, output: out, gain_db: 0.0 });
        }
    }

    pub fn is_patched(&self, stem: StemId, source_channel: u16, output: u16) -> bool {
        self.crosspoints
            .iter()
            .any(|c| c.stem == stem && c.source_channel == source_channel && c.output == output)
    }

    /// Toggles a crosspoint; returns whether it is now patched.
    pub fn toggle(&mut self, stem: StemId, source_channel: u16, output: u16) -> bool {
        if self.is_patched(stem, source_channel, output) {
            self.crosspoints
                .retain(|c| !(c.stem == stem && c.source_channel == source_channel && c.output == output));
            false
        } else {
            self.crosspoints.push(Crosspoint { stem, source_channel, output, gain_db: 0.0 });
            true
        }
    }

    pub fn output_label(&self, output: u16) -> String {
        self.output_labels
            .get(output as usize)
            .cloned()
            .unwrap_or_else(|| format!("Out {}", output + 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_patch_matches_spec() {
        let r = AudioRouting::default();
        let main = r.stem_by_name("Backing Tracks").unwrap();
        assert!(r.is_patched(main.id, 0, 0));
        assert!(r.is_patched(main.id, 1, 1));
        let click = r.stem_by_name("Click").unwrap();
        assert!(r.is_patched(click.id, 0, 2) && r.is_patched(click.id, 0, 3));
        let iem = r.stem_by_name("In-Ear Mix").unwrap();
        assert!(r.is_patched(iem.id, 0, 4) && r.is_patched(iem.id, 1, 7));
    }

    #[test]
    fn display_matching_prefers_exact() {
        let a = PhysicalDisplayInfo {
            name: "DELL".into(),
            position: [0, 0],
            size: [1920, 1080],
            refresh_millihertz: None,
            scale_factor_milli: 1000,
            is_primary: true,
        };
        let mut b = a.clone();
        b.position = [1920, 0];
        let key = PhysicalDisplayKey::from(&b);
        assert_eq!(key.best_match(&[a, b]), Some(1));
    }
}

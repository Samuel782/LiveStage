//! The project manifest (`project.json`) — the full show tree.

use crate::cuelist::CueList;
use crate::dmx_config::DmxConfig;
use crate::id::{AssetId, CueListId, ProjectId};
use crate::routing::{AudioRouting, LogicalDisplay};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path};

/// Current manifest schema. Bump on breaking changes and add a migration in
/// [`Project::migrate`].
pub const SCHEMA_VERSION: u32 = 2;

/// Bundle-relative path to an asset, always with `/` separators, e.g.
/// `assets/audio/intro.wav`. Validated to never escape the bundle.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AssetRef(String);

impl AssetRef {
    pub fn new(path: impl Into<String>) -> Self {
        Self(path.into().replace('\\', "/"))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// True if the path is relative and contains no `..`, root or prefix.
    pub fn is_safe(&self) -> bool {
        !self.0.is_empty()
            && Path::new(&self.0).components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
    }
    pub fn file_name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }
}

impl std::fmt::Display for AssetRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    Audio,
    Video,
    Image,
    /// Presentations (PDF, PowerPoint) — rendered to slides at import.
    Document,
    Data,
}

impl AssetKind {
    pub fn from_extension(ext: &str) -> Self {
        match ext.to_ascii_lowercase().as_str() {
            "wav" | "wave" | "aif" | "aiff" | "flac" | "mp3" | "ogg" | "oga" | "opus" | "m4a" | "aac" | "caf" | "alac" => {
                AssetKind::Audio
            }
            "mp4" | "mov" | "m4v" | "mkv" | "webm" | "avi" | "mxf" | "hap" => AssetKind::Video,
            "png" | "jpg" | "jpeg" | "bmp" | "gif" | "tif" | "tiff" | "webp" | "exr" | "tga" => AssetKind::Image,
            "pdf" | "pptx" | "ppsx" | "potx" | "ppt" | "odp" | "key" => AssetKind::Document,
            _ => AssetKind::Data,
        }
    }
    pub fn folder(self) -> &'static str {
        match self {
            AssetKind::Audio => "audio",
            AssetKind::Video => "video",
            AssetKind::Image => "images",
            AssetKind::Document => "documents",
            AssetKind::Data => "data",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AssetRecord {
    pub id: AssetId,
    pub path: AssetRef,
    pub kind: AssetKind,
    /// File name at import time.
    pub original_name: String,
    pub size_bytes: u64,
    /// Seconds since the UNIX epoch.
    pub imported_at: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ControlConfig {
    pub osc_enabled: bool,
    pub osc_listen_port: u16,
    pub websocket_enabled: bool,
    pub websocket_port: u16,
    /// MIDI input port name to open (None = all / none, see engine).
    pub midi_input: Option<String>,
    /// Accept MIDI Show Control messages for this device id (0x7F = all).
    pub msc_device_id: u8,
}

impl Default for ControlConfig {
    fn default() -> Self {
        Self {
            osc_enabled: true,
            osc_listen_port: 53_100,
            websocket_enabled: true,
            websocket_port: 8_765,
            midi_input: None,
            msc_device_id: 0x7F,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub schema_version: u32,
    pub id: ProjectId,
    pub name: String,
    pub created_at: u64,
    pub modified_at: u64,
    pub cue_lists: Vec<CueList>,
    /// Cue list driven by the main GO button.
    pub active_cue_list: Option<CueListId>,
    pub displays: Vec<LogicalDisplay>,
    pub audio: AudioRouting,
    pub dmx: DmxConfig,
    pub control: ControlConfig,
    pub assets: Vec<AssetRecord>,
    /// Operator multiview layout (None = automatic).
    #[serde(default)]
    pub multiview: Option<crate::multiview::MultiviewLayout>,
    /// Timecode source and lock settings.
    #[serde(default)]
    pub timecode: crate::timecode::TimecodeConfig,
}

pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

impl Project {
    pub fn new(name: impl Into<String>) -> Self {
        let main = CueList::new("Main Cue List");
        let now = unix_now();
        Self {
            schema_version: SCHEMA_VERSION,
            id: ProjectId::new(),
            name: name.into(),
            created_at: now,
            modified_at: now,
            active_cue_list: Some(main.id),
            cue_lists: vec![main],
            displays: vec![LogicalDisplay::new("Main LED Wall"), LogicalDisplay::new("Confidence Monitor")],
            audio: AudioRouting::default(),
            dmx: DmxConfig::default(),
            control: ControlConfig::default(),
            assets: Vec::new(),
            multiview: None,
            timecode: Default::default(),
        }
    }

    pub fn active_list(&self) -> Option<&CueList> {
        let id = self.active_cue_list?;
        self.cue_lists.iter().find(|l| l.id == id)
    }
    pub fn active_list_mut(&mut self) -> Option<&mut CueList> {
        let id = self.active_cue_list?;
        self.cue_lists.iter_mut().find(|l| l.id == id)
    }
    pub fn display(&self, id: crate::id::OutputId) -> Option<&LogicalDisplay> {
        self.displays.iter().find(|d| d.id == id)
    }

    /// Upgrades older manifests in place.
    pub(crate) fn migrate(&mut self) -> crate::CoreResult<()> {
        if self.schema_version > SCHEMA_VERSION {
            return Err(crate::CoreError::UnsupportedSchema { found: self.schema_version, supported: SCHEMA_VERSION });
        }
        if self.schema_version < 2 {
            // v1 files were written with a defaulted brightness of 0.0, which
            // rendered every program output black. No operator-facing control
            // could have produced a meaningful 0.0, so restore full brightness.
            for d in &mut self.displays {
                if d.projection.brightness == 0.0 {
                    d.projection.brightness = 1.0;
                }
            }
        }
        self.schema_version = SCHEMA_VERSION;
        Ok(())
    }

    /// Carries live cue state over from `old` (see [`CueList::adopt_runtime_from`]).
    pub fn adopt_runtime_from(&mut self, old: &Project) {
        for list in &mut self.cue_lists {
            if let Some(o) = old.cue_lists.iter().find(|l| l.id == list.id) {
                list.adopt_runtime_from(o);
            }
        }
    }

    /// Configuration problems across the whole show.
    pub fn validate(&self) -> Vec<String> {
        use crate::cue::Cue;
        let mut issues = Vec::new();
        for list in &self.cue_lists {
            for cue in &list.cues {
                for e in cue.validate() {
                    issues.push(format!("[{}] cue {}: {e}", list.name, cue.header().number));
                }
                for a in cue.assets() {
                    if !a.is_safe() {
                        issues.push(format!("[{}] cue {}: unsafe asset path {a}", list.name, cue.header().number));
                    }
                }
            }
        }
        issues
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn asset_ref_safety() {
        assert!(AssetRef::new("assets/audio/a.wav").is_safe());
        assert!(!AssetRef::new("../secret").is_safe());
        assert!(!AssetRef::new("/etc/passwd").is_safe());
        assert!(!AssetRef::new("assets/../../x").is_safe());
    }
}

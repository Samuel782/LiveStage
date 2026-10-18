//! Application preferences (per machine, not per show), stored as JSON in
//! the platform config folder:
//!
//! * macOS: `~/Library/Application Support/LiveStage/preferences.json`
//! * Linux: `$XDG_CONFIG_HOME/livestage/preferences.json` (or `~/.config/...`)
//! * Windows: `%APPDATA%\LiveStage\preferences.json`
//!
//! Show-specific settings (audio device and buffer, control ports, MIDI)
//! live in the show file so they travel with it; the Settings window edits
//! both and says which is which.

use crate::theme::Accent;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    // Appearance
    pub accent: Accent,
    /// UI zoom (egui points per pixel multiplier).
    pub ui_scale: f32,
    // Windows / outputs
    /// Program outputs open as soon as the show is loaded.
    pub outputs_on_launch: bool,
    /// Kiosk outputs stay above every other window.
    pub outputs_always_on_top: bool,
    /// Hide the mouse pointer over kiosk outputs.
    pub hide_cursor_on_outputs: bool,
    /// Ask before quitting while program outputs are live.
    pub confirm_quit_when_live: bool,
    // Files
    /// Show opened when LiveStage starts without arguments (None = demo show).
    pub startup_show: Option<PathBuf>,
    /// Folder pre-filled in the import dialog.
    pub media_folder: Option<PathBuf>,
    /// Crash-recovery autosave interval (seconds, 0 = off).
    pub autosave_secs: u64,
    // Engines / external tools
    /// LibreOffice `soffice` for PowerPoint / Keynote import (None = auto-detect).
    pub office_path: Option<PathBuf>,
    /// Folder containing the NDI runtime library (None = SDK defaults).
    pub ndi_runtime_dir: Option<PathBuf>,
    /// Hardware video decoding (VideoToolbox / D3D11VA / VAAPI).
    pub hardware_decode: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            accent: Accent::Ice,
            ui_scale: 1.0,
            outputs_on_launch: false,
            outputs_always_on_top: true,
            hide_cursor_on_outputs: true,
            confirm_quit_when_live: true,
            startup_show: None,
            media_folder: None,
            autosave_secs: 30,
            office_path: None,
            ndi_runtime_dir: None,
            hardware_decode: true,
        }
    }
}

/// Platform configuration folder for LiveStage.
pub fn config_dir() -> Option<PathBuf> {
    let home = || std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        home().map(|h| h.join("Library/Application Support/LiveStage"))
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("LiveStage"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| home().map(|h| h.join(".config"))).map(|c| c.join("livestage"))
    }
}

impl Preferences {
    pub fn path() -> Option<PathBuf> {
        config_dir().map(|d| d.join("preferences.json"))
    }

    /// Loads preferences; missing or unreadable files give the defaults.
    pub fn load() -> Self {
        let Some(path) = Self::path() else { return Self::default() };
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!("ignoring unreadable {}: {e}", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path().ok_or_else(|| std::io::Error::other("no config folder"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(tmp, path)
    }

    /// Environment switches read by the engines. Called once at startup,
    /// before any engine thread exists.
    pub fn export_env(&self) {
        // SAFETY (edition 2021: not `unsafe` yet, but the same rule applies):
        // only called from `main` before other threads are spawned.
        if let Some(dir) = &self.ndi_runtime_dir {
            std::env::set_var("NDI_RUNTIME_DIR_V6", dir);
        }
        if !self.hardware_decode && std::env::var_os("LIVESTAGE_HWDEC").is_none() {
            std::env::set_var("LIVESTAGE_HWDEC", "0");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_files_fill_defaults() {
        let p: Preferences = serde_json::from_str(r#"{ "accent": "Blue", "ui_scale": 1.25 }"#).unwrap();
        assert_eq!(p.accent, Accent::Blue);
        assert_eq!(p.autosave_secs, 30);
        assert!(p.hide_cursor_on_outputs);
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Preferences>(&json).unwrap(), p);
    }
}

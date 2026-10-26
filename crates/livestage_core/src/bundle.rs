//! The `.livestage` folder bundle.
//!
//! ```text
//! MyShow.livestage/
//! ├── project.json        master manifest (show tree, routing, DMX, control)
//! ├── project.json.bak    previous manifest, rotated on every save
//! ├── project.autosave.json  crash-recovery copy of unsaved edits
//! ├── project.session     present while LiveStage has the show open
//! └── assets/
//!     ├── audio/          imported media is *copied* here for portability
//!     ├── video/
//!     ├── images/
//!     ├── documents/      imported PDF / PowerPoint originals
//!     ├── slides/<deck>/  slides rendered from them (001.png, 002.png, ...)
//!     └── data/
//! ```
//!
//! A folder (rather than a zip) lets engines stream / memory-map individual
//! assets lazily and lets show files live in version control. Saves are
//! atomic: the manifest is written to a temp file, fsynced and renamed. A
//! manifest that cannot be read falls back to the `.bak` copy.

use crate::error::{CoreError, CoreResult};
use crate::project::{unix_now, AssetKind, AssetRecord, AssetRef, Project};
use crate::id::AssetId;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const BUNDLE_EXTENSION: &str = "livestage";
pub const MANIFEST_FILE: &str = "project.json";
pub const ASSETS_DIR: &str = "assets";
/// Rendered presentation slides, one folder per imported document.
pub const SLIDES_DIR: &str = "slides";
/// Periodic crash-recovery copy of the manifest; removed on every real save.
pub const AUTOSAVE_FILE: &str = "project.autosave.json";
/// Session marker: written when a show is opened, removed on a clean quit.
/// Finding one at startup means the last session crashed or was killed.
pub const SESSION_FILE: &str = "project.session";

#[derive(Debug)]
pub struct ProjectBundle {
    root: PathBuf,
    pub project: Project,
    /// Why the manifest was unreadable, when the show was opened from `.bak`.
    backup_reason: Option<String>,
}

/// The process that has (or had) a show open.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionMarker {
    pub pid: u32,
    /// Unix seconds.
    pub started_at: u64,
}

impl ProjectBundle {
    /// Creates `<parent>/<name>.livestage/` with an empty show.
    pub fn create(parent: impl AsRef<Path>, name: &str) -> CoreResult<Self> {
        let root = parent.as_ref().join(format!("{}.{BUNDLE_EXTENSION}", sanitize_file_name(name)));
        if root.exists() {
            return Err(CoreError::BundleExists(root));
        }
        fs::create_dir_all(&root).map_err(|e| CoreError::io(&root, e))?;
        let mut bundle = Self { root, project: Project::new(name), backup_reason: None };
        bundle.ensure_layout()?;
        bundle.save()?;
        Ok(bundle)
    }

    /// Opens an existing bundle. Accepts the bundle folder or its manifest.
    pub fn open(path: impl AsRef<Path>) -> CoreResult<Self> {
        let mut root = path.as_ref().to_path_buf();
        if root.file_name().is_some_and(|n| n == MANIFEST_FILE) {
            root.pop();
        }
        let manifest = root.join(MANIFEST_FILE);
        if !manifest.is_file() {
            return Err(CoreError::NotABundle(root));
        }
        let (project, backup_reason) = match read_manifest(&manifest) {
            Ok(p) => (p, None),
            // A newer schema is not damage: the backup would be just as new.
            Err(e @ CoreError::UnsupportedSchema { .. }) => return Err(e),
            Err(e) => {
                let bak = root.join(format!("{MANIFEST_FILE}.bak"));
                match read_manifest(&bak) {
                    Ok(p) => {
                        tracing::error!("{e}; opened the backup manifest instead");
                        (p, Some(e.to_string()))
                    }
                    Err(_) => return Err(e),
                }
            }
        };
        let bundle = Self { root, project, backup_reason };
        bundle.ensure_layout()?;
        let missing = bundle.missing_assets();
        if !missing.is_empty() {
            tracing::warn!(count = missing.len(), "project references missing assets");
        }
        Ok(bundle)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    /// Set when `project.json` was unreadable and the show was opened from
    /// `project.json.bak`: the reason the manifest was rejected.
    pub fn opened_from_backup(&self) -> Option<&str> {
        self.backup_reason.as_deref()
    }
    pub fn manifest_path(&self) -> PathBuf {
        self.root.join(MANIFEST_FILE)
    }
    pub fn assets_dir(&self) -> PathBuf {
        self.root.join(ASSETS_DIR)
    }

    fn ensure_layout(&self) -> CoreResult<()> {
        for kind in [AssetKind::Audio, AssetKind::Video, AssetKind::Image, AssetKind::Document, AssetKind::Data] {
            let dir = self.assets_dir().join(kind.folder());
            fs::create_dir_all(&dir).map_err(|e| CoreError::io(&dir, e))?;
        }
        Ok(())
    }

    /// Atomically writes the manifest, keeping the previous one as `.bak`.
    pub fn save(&mut self) -> CoreResult<()> {
        self.project.modified_at = unix_now();
        let manifest = self.manifest_path();
        let tmp = self.root.join(format!("{MANIFEST_FILE}.tmp"));
        let json = serde_json::to_vec_pretty(&self.project)
            .map_err(|source| CoreError::Manifest { path: manifest.clone(), source })?;
        {
            let mut f = fs::File::create(&tmp).map_err(|e| CoreError::io(&tmp, e))?;
            f.write_all(&json).map_err(|e| CoreError::io(&tmp, e))?;
            f.sync_all().map_err(|e| CoreError::io(&tmp, e))?;
        }
        if manifest.exists() {
            let bak = self.root.join(format!("{MANIFEST_FILE}.bak"));
            fs::copy(&manifest, &bak).map_err(|e| CoreError::io(&bak, e))?;
        }
        fs::rename(&tmp, &manifest).map_err(|e| CoreError::io(&manifest, e))?;
        self.discard_autosave();
        self.backup_reason = None;
        Ok(())
    }

    pub fn session_path(&self) -> PathBuf {
        self.root.join(SESSION_FILE)
    }

    /// Marks the show as open by this process. Returns the marker a previous
    /// session left behind: that session crashed or was killed, or another
    /// LiveStage still has the show open.
    pub fn begin_session(&self) -> Option<SessionMarker> {
        let path = self.session_path();
        let previous = fs::read(&path).ok().and_then(|b| serde_json::from_slice::<SessionMarker>(&b).ok());
        let marker = SessionMarker { pid: std::process::id(), started_at: unix_now() };
        let written = serde_json::to_vec(&marker).map_err(std::io::Error::other).and_then(|json| fs::write(&path, json));
        if let Err(e) = written {
            tracing::warn!("cannot write session marker {}: {e}", path.display());
        }
        previous.filter(|p| p.pid != marker.pid || p.started_at != marker.started_at)
    }

    /// Clears this process's session marker (a clean quit).
    pub fn end_session(&self) {
        let path = self.session_path();
        let ours = fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<SessionMarker>(&b).ok())
            .is_some_and(|m| m.pid == std::process::id());
        if ours {
            let _ = fs::remove_file(path);
        }
    }

    pub fn autosave_path(&self) -> PathBuf {
        self.root.join(AUTOSAVE_FILE)
    }

    /// If an autosave newer than the manifest exists (i.e. the app quit or
    /// crashed with unsaved changes), returns its modification time.
    pub fn recovery_available(&self) -> Option<std::time::SystemTime> {
        let auto = fs::metadata(self.autosave_path()).and_then(|m| m.modified()).ok()?;
        let manifest = fs::metadata(self.manifest_path()).and_then(|m| m.modified()).ok();
        manifest.is_none_or(|m| auto > m).then_some(auto)
    }

    /// Reads the autosaved project (does not apply it).
    pub fn load_autosave(&self) -> CoreResult<Project> {
        let path = self.autosave_path();
        let bytes = fs::read(&path).map_err(|e| CoreError::io(&path, e))?;
        let mut p: Project = serde_json::from_slice(&bytes).map_err(|source| CoreError::Manifest { path, source })?;
        p.migrate()?;
        Ok(p)
    }

    pub fn discard_autosave(&self) {
        let _ = fs::remove_file(self.autosave_path());
    }

    /// Copies an external file into `assets/<kind>/` (never moves the
    /// original), de-duplicating the file name, and registers it.
    pub fn import_asset(&mut self, source: impl AsRef<Path>) -> CoreResult<AssetRecord> {
        let source = source.as_ref();
        let meta = fs::metadata(source).map_err(|e| CoreError::io(source, e))?;
        let original_name = source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| CoreError::AssetMissing(source.display().to_string()))?;
        let ext = source.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
        let kind = AssetKind::from_extension(&ext);

        let dir = self.assets_dir().join(kind.folder());
        let safe = sanitize_file_name(&original_name);
        let (stem, dot_ext) = match safe.rsplit_once('.') {
            Some((s, e)) => (s.to_string(), format!(".{e}")),
            None => (safe.clone(), String::new()),
        };
        let mut file_name = safe.clone();
        let mut n = 1;
        while dir.join(&file_name).exists() {
            n += 1;
            file_name = format!("{stem} ({n}){dot_ext}");
        }
        let dest = dir.join(&file_name);
        fs::copy(source, &dest).map_err(|e| CoreError::io(&dest, e))?;

        let record = AssetRecord {
            id: AssetId::new(),
            path: AssetRef::new(format!("{ASSETS_DIR}/{}/{file_name}", kind.folder())),
            kind,
            original_name,
            size_bytes: meta.len(),
            imported_at: unix_now(),
        };
        self.project.assets.push(record.clone());
        Ok(record)
    }

    /// A fresh directory under `assets/slides/` for a rendered deck named
    /// after `stem`. Returns `(absolute dir, bundle-relative prefix)`.
    pub fn new_slides_dir(&self, stem: &str) -> CoreResult<(PathBuf, String)> {
        let base = self.assets_dir().join(SLIDES_DIR);
        let safe = sanitize_file_name(stem);
        let mut name = safe.clone();
        let mut n = 1;
        while base.join(&name).exists() {
            n += 1;
            name = format!("{safe} ({n})");
        }
        let dir = base.join(&name);
        fs::create_dir_all(&dir).map_err(|e| CoreError::io(&dir, e))?;
        Ok((dir, format!("{ASSETS_DIR}/{SLIDES_DIR}/{name}")))
    }

    /// Absolute path for an asset, refusing paths that escape the bundle.
    pub fn resolve(&self, asset: &AssetRef) -> CoreResult<PathBuf> {
        resolve_asset(&self.root, asset)
    }

    /// Assets referenced by cues or the registry that are not on disk.
    pub fn missing_assets(&self) -> Vec<AssetRef> {
        use crate::cue::AnyCue;
        let mut refs: Vec<AssetRef> = self.project.assets.iter().map(|a| a.path.clone()).collect();
        for list in &self.project.cue_lists {
            refs.extend(list.cues.iter().flat_map(AnyCue::assets).cloned());
        }
        for cart in &self.project.carts {
            refs.extend(cart.pads.iter().flatten().flat_map(|p| p.cue.assets()).cloned());
        }
        refs.sort();
        refs.dedup();
        refs.into_iter().filter(|a| self.resolve(a).map(|p| !p.is_file()).unwrap_or(true)).collect()
    }
}

/// Reads and migrates a manifest file.
fn read_manifest(path: &Path) -> CoreResult<Project> {
    let bytes = fs::read(path).map_err(|e| CoreError::io(path, e))?;
    let mut project: Project =
        serde_json::from_slice(&bytes).map_err(|source| CoreError::Manifest { path: path.to_path_buf(), source })?;
    project.migrate()?;
    Ok(project)
}

/// Atomically writes a crash-recovery copy of `project` into the bundle at
/// `root` (fsynced, so a power cut leaves the old copy or the new one, never
/// a torn file). Takes no `&ProjectBundle` so it can run on a background
/// thread from a snapshot.
pub fn write_autosave(root: &Path, project: &Project) -> CoreResult<()> {
    let path = root.join(AUTOSAVE_FILE);
    let tmp = root.join(format!("{AUTOSAVE_FILE}.tmp"));
    let json = serde_json::to_vec(project).map_err(|source| CoreError::Manifest { path: path.clone(), source })?;
    {
        let mut f = fs::File::create(&tmp).map_err(|e| CoreError::io(&tmp, e))?;
        f.write_all(&json).map_err(|e| CoreError::io(&tmp, e))?;
        f.sync_all().map_err(|e| CoreError::io(&tmp, e))?;
    }
    fs::rename(&tmp, &path).map_err(|e| CoreError::io(&path, e))
}

/// Resolves `asset` against a bundle root without needing the bundle itself
/// (used by threads that only hold the root path, e.g. the render thread).
pub fn resolve_asset(root: &Path, asset: &AssetRef) -> CoreResult<PathBuf> {
    if !asset.is_safe() {
        return Err(CoreError::UnsafeAssetPath(asset.to_string()));
    }
    Ok(root.join(asset.as_str()))
}

/// Removes characters that are invalid on any of macOS / Windows / Linux.
pub fn sanitize_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '_' } else { c })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.');
    if trimmed.is_empty() {
        "Untitled".into()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::AudioTrack;
    use crate::cue::{AnyCue, AudioCue, Cue, CueHeader};
    use crate::time::ShowTime;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("livestage-test-{tag}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn create_import_save_open_roundtrip() {
        let dir = temp_dir("bundle");
        let src = dir.join("Intro Song.wav");
        fs::write(&src, b"RIFF....WAVE").unwrap();

        let mut b = ProjectBundle::create(&dir, "My Show").unwrap();
        assert!(b.root().ends_with("My Show.livestage"));
        let rec = b.import_asset(&src).unwrap();
        assert_eq!(rec.path.as_str(), "assets/audio/Intro Song.wav");
        let rec2 = b.import_asset(&src).unwrap();
        assert_eq!(rec2.path.as_str(), "assets/audio/Intro Song (2).wav");

        let stem = b.project.audio.stems[0].id;
        let cue = AnyCue::Audio(AudioCue {
            header: CueHeader::new("1", "Intro"),
            tracks: vec![AudioTrack { asset: rec.path.clone(), stem, gain_db: -3.0, mute: false }],
            click: None,
            gain_db: 0.0,
            fade_in: ShowTime::ZERO,
            fade_out: ShowTime::from_millis(500),
            start_at: ShowTime::ZERO,
            end_at: None,
            looped: false,
            media_length: None,
        });
        b.project.active_list_mut().unwrap().cues.push(cue);
        b.save().unwrap();
        b.save().unwrap();
        assert!(b.root().join("project.json.bak").is_file());

        let reopened = ProjectBundle::open(b.root()).unwrap();
        assert_eq!(reopened.project.name, "My Show");
        assert_eq!(reopened.project.assets.len(), 2);
        let list = reopened.project.active_list().unwrap();
        assert_eq!(list.cues[0].header().name, "Intro");
        assert!(reopened.missing_assets().is_empty());
        assert!(reopened.resolve(&AssetRef::new("../../etc/passwd")).is_err());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_assets_reported() {
        let dir = temp_dir("missing");
        let mut b = ProjectBundle::create(&dir, "Show").unwrap();
        let rec = b.import_asset({
            let p = dir.join("clip.mov");
            fs::write(&p, b"x").unwrap();
            p
        })
        .unwrap();
        fs::remove_file(b.resolve(&rec.path).unwrap()).unwrap();
        assert_eq!(b.missing_assets(), vec![rec.path]);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn v1_black_outputs_are_repaired() {
        let dir = temp_dir("migrate");
        let b = ProjectionBundleFixture::write_v1(&dir);
        let opened = ProjectBundle::open(&b).unwrap();
        assert_eq!(opened.project.schema_version, crate::project::SCHEMA_VERSION);
        assert!(opened.project.displays.iter().all(|d| d.projection.brightness == 1.0));
        fs::remove_dir_all(&dir).ok();
    }

    struct ProjectionBundleFixture;
    impl ProjectionBundleFixture {
        /// A schema-1 bundle whose displays carry the buggy brightness 0.0.
        fn write_v1(dir: &Path) -> PathBuf {
            let mut b = ProjectBundle::create(dir, "Old").unwrap();
            for d in &mut b.project.displays {
                d.projection.brightness = 0.0;
            }
            b.project.schema_version = 1;
            b.save().unwrap();
            b.root().to_path_buf()
        }
    }

    #[test]
    fn autosave_recovery_cycle() {
        let dir = temp_dir("autosave");
        let mut b = ProjectBundle::create(&dir, "Show").unwrap();
        assert!(b.recovery_available().is_none());
        std::thread::sleep(std::time::Duration::from_millis(20));
        let mut edited = b.project.clone();
        edited.name = "Show (edited)".into();
        write_autosave(b.root(), &edited).unwrap();
        assert!(b.recovery_available().is_some());
        assert_eq!(b.load_autosave().unwrap().name, "Show (edited)");
        b.save().unwrap();
        assert!(b.recovery_available().is_none(), "a real save clears the autosave");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_manifest_opens_the_backup() {
        let dir = temp_dir("corrupt");
        let mut b = ProjectBundle::create(&dir, "Show").unwrap();
        b.project.name = "Second save".into();
        b.save().unwrap(); // the first save is now project.json.bak
        let root = b.root().to_path_buf();
        // A torn write (disk full, yanked drive) leaves half a manifest.
        let json = fs::read(root.join(MANIFEST_FILE)).unwrap();
        fs::write(root.join(MANIFEST_FILE), &json[..json.len() / 2]).unwrap();
        let opened = ProjectBundle::open(&root).unwrap();
        assert_eq!(opened.project.name, "Show");
        assert!(opened.opened_from_backup().is_some_and(|r| r.contains("invalid project manifest")));
        // Without a usable backup the original error comes back.
        fs::write(root.join(format!("{MANIFEST_FILE}.bak")), b"{").unwrap();
        assert!(matches!(ProjectBundle::open(&root), Err(CoreError::Manifest { .. })));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn session_marker_reports_an_unclean_exit() {
        let dir = temp_dir("session");
        let b = ProjectBundle::create(&dir, "Show").unwrap();
        assert_eq!(b.begin_session(), None, "first open: nothing left behind");
        b.end_session();
        assert!(!b.session_path().exists(), "a clean quit removes the marker");
        assert_eq!(b.begin_session(), None);
        // Crash: the marker stays. A dead process left it behind.
        let dead = SessionMarker { pid: u32::MAX - 1, started_at: 1_700_000_000 };
        fs::write(b.session_path(), serde_json::to_vec(&dead).unwrap()).unwrap();
        assert_eq!(b.begin_session(), Some(dead));
        assert_eq!(b.begin_session(), None, "the new session replaced it");
        // Only our own marker is removed on quit.
        let other = SessionMarker { pid: std::process::id().wrapping_add(1), started_at: 1 };
        fs::write(b.session_path(), serde_json::to_vec(&other).unwrap()).unwrap();
        b.end_session();
        assert!(b.session_path().exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sanitizes_names() {
        assert_eq!(sanitize_file_name("a/b:c?.wav"), "a_b_c_.wav");
        assert_eq!(sanitize_file_name("..."), "Untitled");
    }
}

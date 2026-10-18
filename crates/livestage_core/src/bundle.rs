//! The `.livestage` folder bundle.
//!
//! ```text
//! MyShow.livestage/
//! ├── project.json        master manifest (show tree, routing, DMX, control)
//! ├── project.json.bak    previous manifest, rotated on every save
//! └── assets/
//!     ├── audio/          imported media is *copied* here for portability
//!     ├── video/
//!     ├── images/
//!     └── data/
//! ```
//!
//! A folder (rather than a zip) lets engines stream / memory-map individual
//! assets lazily and lets show files live in version control. Saves are
//! atomic: the manifest is written to a temp file, fsynced and renamed.

use crate::error::{CoreError, CoreResult};
use crate::project::{unix_now, AssetKind, AssetRecord, AssetRef, Project};
use crate::id::AssetId;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const BUNDLE_EXTENSION: &str = "livestage";
pub const MANIFEST_FILE: &str = "project.json";
pub const ASSETS_DIR: &str = "assets";
/// Periodic crash-recovery copy of the manifest; removed on every real save.
pub const AUTOSAVE_FILE: &str = "project.autosave.json";

#[derive(Debug)]
pub struct ProjectBundle {
    root: PathBuf,
    pub project: Project,
}

impl ProjectBundle {
    /// Creates `<parent>/<name>.livestage/` with an empty show.
    pub fn create(parent: impl AsRef<Path>, name: &str) -> CoreResult<Self> {
        let root = parent.as_ref().join(format!("{}.{BUNDLE_EXTENSION}", sanitize_file_name(name)));
        if root.exists() {
            return Err(CoreError::BundleExists(root));
        }
        fs::create_dir_all(&root).map_err(|e| CoreError::io(&root, e))?;
        let mut bundle = Self { root, project: Project::new(name) };
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
        let bytes = fs::read(&manifest).map_err(|e| CoreError::io(&manifest, e))?;
        let mut project: Project =
            serde_json::from_slice(&bytes).map_err(|source| CoreError::Manifest { path: manifest.clone(), source })?;
        project.migrate()?;
        let bundle = Self { root, project };
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
    pub fn manifest_path(&self) -> PathBuf {
        self.root.join(MANIFEST_FILE)
    }
    pub fn assets_dir(&self) -> PathBuf {
        self.root.join(ASSETS_DIR)
    }

    fn ensure_layout(&self) -> CoreResult<()> {
        for kind in [AssetKind::Audio, AssetKind::Video, AssetKind::Image, AssetKind::Data] {
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
        Ok(())
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
        refs.sort();
        refs.dedup();
        refs.into_iter().filter(|a| self.resolve(a).map(|p| !p.is_file()).unwrap_or(true)).collect()
    }
}

/// Atomically writes a crash-recovery copy of `project` into the bundle at
/// `root`. Takes no `&ProjectBundle` so it can run on a background thread
/// from a snapshot.
pub fn write_autosave(root: &Path, project: &Project) -> CoreResult<()> {
    let path = root.join(AUTOSAVE_FILE);
    let tmp = root.join(format!("{AUTOSAVE_FILE}.tmp"));
    let json = serde_json::to_vec(project).map_err(|source| CoreError::Manifest { path: path.clone(), source })?;
    fs::write(&tmp, json).map_err(|e| CoreError::io(&tmp, e))?;
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
    fn sanitizes_names() {
        assert_eq!(sanitize_file_name("a/b:c?.wav"), "a_b_c_.wav");
        assert_eq!(sanitize_file_name("..."), "Untitled");
    }
}

//! Where provided builds live, a cross-process lock so parallel builds
//! (several profiles, `cargo check` next to rust-analyzer) provide each
//! one once, and the marker that says a build is complete.

use crate::Result;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

/// Written last into a finished build; its absence means "redo".
pub const MARKER: &str = ".livestage-ffmpeg";

/// The cache root: `LIVESTAGE_FFMPEG_CACHE`, else `<target-dir>/ffmpeg-build`
/// (outside any profile, so debug, release and `cargo check` share it).
pub fn root() -> PathBuf {
    if let Some(dir) = env::var_os("LIVESTAGE_FFMPEG_CACHE").filter(|d| !d.is_empty()) {
        return dir.into();
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set for build scripts"));
    target_dir(&out).unwrap_or(out).join("ffmpeg-build")
}

/// Cargo's target directory above a build script's `OUT_DIR`
/// (`<target>/[<triple>/]<profile>/build/<pkg>-<hash>/out`): the outermost
/// ancestor holding a `CACHEDIR.TAG`. Cargo writes one in the target
/// directory and another in `<triple>/` when cross-compiling.
pub fn target_dir(out_dir: &Path) -> Option<PathBuf> {
    out_dir.ancestors().skip(1).filter(|d| d.join("CACHEDIR.TAG").is_file()).last().map(Path::to_path_buf)
}

/// The profile directory (`target/debug`, `target/<triple>/release`, …)
/// that binaries, tests (`deps/`) and examples are written to.
pub fn profile_dir(out_dir: &Path) -> Option<PathBuf> {
    let dir = out_dir.ancestors().nth(3)?;
    (out_dir.ancestors().nth(2)?.file_name()? == "build").then(|| dir.to_path_buf())
}

/// Holds an exclusive lock on `<root>/<key>.lock` until dropped. The OS
/// releases it if the build is interrupted, so a killed build never
/// leaves a stale lock behind.
pub struct Lock(#[allow(dead_code)] File);

pub fn lock(root: &Path, key: &str) -> Result<Lock> {
    fs::create_dir_all(root)?;
    let file = OpenOptions::new().create(true).truncate(false).write(true).open(root.join(format!("{key}.lock")))?;
    file.lock()?;
    Ok(Lock(file))
}

pub fn is_complete(dir: &Path) -> bool {
    dir.join(MARKER).is_file()
}

pub fn mark_complete(dir: &Path, details: &str) -> Result<()> {
    fs::write(dir.join(MARKER), details)?;
    Ok(())
}

/// Removes `path` if it exists (file or directory).
pub fn remove(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(path)?,
        Ok(_) => fs::remove_file(path)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("livestage-ffmpeg-cache-{name}-{}", std::process::id()));
        remove(&dir).unwrap();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn finds_target_and_profile_dirs_from_out_dir() {
        let root = scratch("layout");
        let target = root.join("target");
        let out = target.join("aarch64-apple-darwin/release/build/ffmpeg-sys-next-0123/out");
        fs::create_dir_all(&out).unwrap();
        assert_eq!(target_dir(&out), None);
        fs::write(target.join("CACHEDIR.TAG"), "Signature: 8a477f597d28d172789f06886806bc55").unwrap();
        assert_eq!(target_dir(&out), Some(target.clone()));
        fs::write(target.join("aarch64-apple-darwin/CACHEDIR.TAG"), "").unwrap();
        assert_eq!(target_dir(&out), Some(target.clone()));
        assert_eq!(profile_dir(&out), Some(target.join("aarch64-apple-darwin/release")));
        assert_eq!(profile_dir(&root.join("elsewhere/out")), None);
        remove(&root).unwrap();
    }

    #[test]
    fn lock_is_exclusive_and_released_on_drop() {
        let root = scratch("lock");
        let held = lock(&root, "k").unwrap();
        let other = OpenOptions::new().write(true).open(root.join("k.lock")).unwrap();
        assert!(other.try_lock().is_err());
        drop(held);
        other.try_lock().unwrap();
        remove(&root).unwrap();
    }

    #[test]
    fn marker_round_trip() {
        let root = scratch("marker");
        assert!(!is_complete(&root));
        mark_complete(&root, "url=x\n").unwrap();
        assert!(is_complete(&root));
        remove(&root).unwrap();
        assert!(!root.exists());
        remove(&root).unwrap();
    }
}

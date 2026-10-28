//! Archive extraction (`.zip`, `.tar.xz`, `.tar.gz`). Release archives
//! wrap everything in one top-level folder; that folder becomes the
//! destination, so `include/` and `lib/` sit directly inside it.
//! Entries that would land outside the destination are rejected by the
//! `tar` and `zip` crates.

use crate::cache::remove;
use crate::{bail, Result};
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Zip,
    TarXz,
    TarGz,
}

impl Kind {
    pub fn of(name: &str) -> Option<Kind> {
        let name = name.to_ascii_lowercase();
        if name.ends_with(".zip") {
            Some(Kind::Zip)
        } else if name.ends_with(".tar.xz") || name.ends_with(".txz") {
            Some(Kind::TarXz)
        } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
            Some(Kind::TarGz)
        } else {
            None
        }
    }
}

/// Extracts `archive` (named `name`, for its type) so that its contents
/// end up in `dest`, replacing whatever was there.
pub fn unpack(archive: &Path, name: &str, dest: &Path) -> Result<()> {
    let Some(kind) = Kind::of(name) else { bail!("{name}: unsupported archive type (use .zip, .tar.xz or .tar.gz)") };
    let staging = PathBuf::from(format!("{}.unpack", dest.display()));
    remove(&staging)?;
    fs::create_dir_all(&staging)?;
    let file = BufReader::new(File::open(archive)?);
    let extracted = match kind {
        Kind::Zip => zip::ZipArchive::new(file).and_then(|mut z| z.extract(&staging)).map_err(|e| e.to_string()),
        Kind::TarXz => untar(liblzma::read::XzDecoder::new_multi_decoder(file), &staging),
        Kind::TarGz => untar(flate2::read::GzDecoder::new(file), &staging),
    };
    if let Err(e) = extracted {
        remove(&staging)?;
        bail!("unpacking {name}: {e}");
    }
    let root = single_root(&staging)?;
    remove(dest)?;
    fs::rename(&root, dest)?;
    remove(&staging)?;
    Ok(())
}

fn untar(reader: impl Read, into: &Path) -> std::result::Result<(), String> {
    let mut tar = tar::Archive::new(reader);
    // Keep executable bits (`configure` and its helper scripts).
    tar.set_preserve_permissions(true);
    tar.unpack(into).map_err(|e| e.to_string())
}

/// The lone top-level folder of an extracted archive, or the extraction
/// directory itself when there are several entries.
fn single_root(dir: &Path) -> Result<PathBuf> {
    let entries: Vec<_> = fs::read_dir(dir)?.collect::<std::io::Result<_>>()?;
    match entries.as_slice() {
        [only] if only.file_type()?.is_dir() => Ok(only.path()),
        [] => bail!("the archive was empty"),
        _ => Ok(dir.to_path_buf()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("livestage-ffmpeg-unpack-{name}-{}", std::process::id()));
        remove(&dir).unwrap();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn tar_bytes(top: Option<&str>) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (path, body) in [("include/libavcodec/avcodec.h", "/* h */"), ("lib/libavcodec.so", "so")] {
            let path = top.map_or(path.to_string(), |t| format!("{t}/{path}"));
            let mut h = tar::Header::new_gnu();
            h.set_size(body.len() as u64);
            h.set_mode(0o755);
            h.set_cksum();
            b.append_data(&mut h, path, body.as_bytes()).unwrap();
        }
        b.into_inner().unwrap()
    }

    fn check_layout(dest: &Path) {
        assert_eq!(fs::read_to_string(dest.join("include/libavcodec/avcodec.h")).unwrap(), "/* h */");
        assert_eq!(fs::read_to_string(dest.join("lib/libavcodec.so")).unwrap(), "so");
        assert!(!PathBuf::from(format!("{}.unpack", dest.display())).exists());
    }

    #[test]
    fn tar_xz_with_a_top_folder_is_flattened() {
        let dir = scratch("txz");
        let archive = dir.join("a.tar.xz");
        let mut enc = liblzma::write::XzEncoder::new(File::create(&archive).unwrap(), 6);
        enc.write_all(&tar_bytes(Some("ffmpeg-n9.0-latest-linux64-lgpl-shared-9.0"))).unwrap();
        enc.finish().unwrap();
        let dest = dir.join("ffmpeg-9.0.2-x");
        fs::create_dir_all(dest.join("stale")).unwrap();
        unpack(&archive, "a.tar.xz", &dest).unwrap();
        check_layout(&dest);
        assert!(!dest.join("stale").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(dest.join("lib/libavcodec.so")).unwrap().permissions().mode() & 0o111, 0o111);
        }
        remove(&dir).unwrap();
    }

    #[test]
    fn tar_gz_without_a_top_folder_is_kept() {
        let dir = scratch("tgz");
        let archive = dir.join("a.tgz");
        let mut enc = flate2::write::GzEncoder::new(File::create(&archive).unwrap(), flate2::Compression::fast());
        enc.write_all(&tar_bytes(None)).unwrap();
        enc.finish().unwrap();
        let dest = dir.join("out");
        unpack(&archive, "a.tgz", &dest).unwrap();
        check_layout(&dest);
        remove(&dir).unwrap();
    }

    #[test]
    fn zip_is_flattened() {
        let dir = scratch("zip");
        let archive = dir.join("a.zip");
        let mut z = zip::ZipWriter::new(File::create(&archive).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        for (path, body) in [("ff/include/libavcodec/avcodec.h", "/* h */"), ("ff/lib/libavcodec.so", "so")] {
            z.start_file(path, opts).unwrap();
            z.write_all(body.as_bytes()).unwrap();
        }
        z.finish().unwrap();
        let dest = dir.join("out");
        unpack(&archive, "a.zip", &dest).unwrap();
        check_layout(&dest);
        remove(&dir).unwrap();
    }

    #[test]
    fn corrupt_and_unknown_archives_fail_cleanly() {
        let dir = scratch("bad");
        let archive = dir.join("a.tar.xz");
        fs::write(&archive, b"not xz").unwrap();
        let dest = dir.join("out");
        assert!(unpack(&archive, "a.tar.xz", &dest).unwrap_err().0.contains("a.tar.xz"));
        assert!(!dest.exists() && !dir.join("out.unpack").exists());
        assert!(unpack(&archive, "a.rar", &dest).is_err());
        remove(&dir).unwrap();
    }
}

//! HTTPS downloads (rustls, proxies from `HTTPS_PROXY` / `ALL_PROXY`),
//! streamed to disk and hashed on the way, with retries.

use crate::{bail, Error, Result};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const ATTEMPTS: u32 = 3;

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(concat!("livestage-ffmpeg-provision/", env!("CARGO_PKG_VERSION")))
        .timeout_connect(Some(Duration::from_secs(30)))
        // A stalled transfer fails instead of hanging the build.
        .timeout_per_read(Some(Duration::from_secs(60)))
        .build()
        .new_agent()
}

/// Retries `f` on failure, with a short backoff.
fn retry<T>(what: &str, mut f: impl FnMut() -> Result<T>) -> Result<T> {
    let mut attempt = 1;
    loop {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) if attempt >= ATTEMPTS => return Err(Error(format!("{what}: {e}"))),
            Err(_) => {
                std::thread::sleep(Duration::from_secs(2 * u64::from(attempt)));
                attempt += 1;
            }
        }
    }
}

/// A small text resource (a checksum list).
pub fn text(url: &str) -> Result<String> {
    retry(url, || {
        let resp = agent().get(url).call().map_err(|e| Error(e.to_string()))?;
        resp.into_body().with_config().limit(1 << 20).read_to_string().map_err(|e| Error(e.to_string()))
    })
}

/// Downloads `url` to `dest` and checks it against `sha256` (hex). An
/// existing `dest` with the right digest is kept (a resumed provision).
/// The file only appears at `dest` once verified.
pub fn download_verified(url: &str, dest: &Path, sha256: &str) -> Result<()> {
    let want = sha256.to_ascii_lowercase();
    if dest.is_file() && hash_file(dest)? == want {
        return Ok(());
    }
    let part = PathBuf::from(format!("{}.part", dest.display()));
    let got = retry(url, || {
        let resp = agent().get(url).call().map_err(|e| Error(e.to_string()))?;
        stream_to(resp.into_body().into_reader(), &part)
    })?;
    if got != want {
        let _ = fs::remove_file(&part);
        bail!("{url}: SHA-256 mismatch (expected {want}, got {got}); the download was discarded");
    }
    fs::rename(&part, dest)?;
    Ok(())
}

/// Copies `reader` into `path`, returning the SHA-256 of what was written.
fn stream_to(mut reader: impl Read, path: &Path) -> Result<String> {
    let mut out = BufWriter::new(File::create(path)?);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n])?;
    }
    out.flush()?;
    Ok(hex(&hasher.finalize()))
}

pub fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_hashes_what_it_writes() {
        let path = std::env::temp_dir().join(format!("livestage-ffmpeg-fetch-{}", std::process::id()));
        let sha = stream_to(&b"abc"[..], &path).unwrap();
        // FIPS 180-2 test vector for "abc".
        assert_eq!(sha, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(hash_file(&path).unwrap(), sha);
        assert_eq!(fs::read(&path).unwrap(), b"abc");
        // Already present with the right digest: no network needed.
        download_verified("https://invalid.invalid/x", &path, &sha.to_uppercase()).unwrap();
        fs::remove_file(&path).unwrap();
    }
}

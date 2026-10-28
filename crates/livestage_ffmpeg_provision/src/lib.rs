//! Zero-configuration FFmpeg for LiveStage's `ffmpeg` feature.
//!
//! `ffmpeg-sys-next` is vendored in `vendor/rust-ffmpeg-sys` with one
//! change: its build script calls [`build_script::run`] before the
//! upstream discovery (`FFMPEG_DIR`, then vcpkg, then pkg-config). When an
//! FFmpeg 9 is already configured that call does nothing. Otherwise it
//! provides one under `<target-dir>/ffmpeg-build/` and sets `FFMPEG_DIR`
//! (and `PKG_CONFIG_PATH`) for the rest of that build script, so the
//! upstream code emits the `rustc-link-search` / `rustc-link-lib` lines
//! and generates bindings against the provided headers and libraries.
//!
//! | Target | What is provided |
//! |---|---|
//! | Linux x86_64 / aarch64 (glibc) | BtbN/FFmpeg-Builds `n9.0` LGPL shared build (`.tar.xz`), checked against that release's `checksums.sha256` |
//! | Windows x86_64 / aarch64 | the same build as a `.zip`: headers, import `.lib`s and DLLs |
//! | macOS, other Unix | the FFmpeg [`plan::SOURCE_VERSION`] source release (pinned SHA-256), built once with `configure && make` |
//!
//! Prebuilt LGPL shared libraries keep LiveStage's licensing as it is
//! with a system FFmpeg (dynamic linking, no GPL components).
//!
//! # Environment
//!
//! - `LIVESTAGE_FFMPEG`: `auto` (default; use a configured FFmpeg 9,
//!   fetch otherwise), `fetch` (always use the provided one, even if a
//!   system FFmpeg exists) or `system` (never fetch).
//! - `LIVESTAGE_FFMPEG_CACHE`: where provided builds are kept (default
//!   `<target-dir>/ffmpeg-build`, shared by every profile).
//! - `LIVESTAGE_FFMPEG_URL` with `LIVESTAGE_FFMPEG_SHA256`: a prebuilt
//!   archive of your own (`.zip`, `.tar.xz` or `.tar.gz`) holding
//!   `include/` and `lib/` (plus `bin/` with the DLLs on Windows).
//!
//! # Running the result
//!
//! Cargo puts link-search directories inside the target directory on the
//! dynamic-library path for `cargo run` and `cargo test`. For launching
//! the binary directly, the Windows DLLs are copied next to it, the app's
//! build script adds an rpath on Linux from the `links` metadata
//! (`DEP_FFMPEG_LIVESTAGE_RUNTIME_DIR`), and macOS libraries carry their
//! absolute install name. `scripts/build-release.sh` bundles them for
//! distribution.

pub mod build_script;
pub mod cache;
pub mod fetch;
pub mod plan;
pub mod source;
pub mod unpack;

use std::fmt;

/// A provisioning failure, with enough context to act on.
#[derive(Debug)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// `Err(Error(..))` from a format string.
macro_rules! bail {
    ($($arg:tt)*) => { return Err($crate::Error(format!($($arg)*))) };
}
pub(crate) use bail;

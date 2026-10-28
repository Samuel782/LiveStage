//! What to provide for a target: which archive, how to verify it, and
//! where it is cached.

use crate::{bail, Result};

/// FFmpeg release built from source where no prebuilt libraries exist.
pub const SOURCE_VERSION: &str = "9.0.2";
pub const SOURCE_URL: &str = "https://ffmpeg.org/releases/ffmpeg-9.0.2.tar.xz";
/// SHA-256 of [`SOURCE_URL`]. Pinned after checking the release signature
/// (`ffmpeg-9.0.2.tar.xz.asc`, FFmpeg release key
/// FCF9 86EA 15E6 E293 A564 4F10 B432 2F04 D676 58D8).
pub const SOURCE_SHA256: &str = "8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e";

/// BtbN's rolling release. Its `n9.0` assets follow the 9.0 maintenance
/// branch (same ABI as [`SOURCE_VERSION`]); the checksum list is published
/// with each upload.
const BTBN_RELEASE: &str = "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest";

/// The oldest `libavcodec` major version `ffmpeg-sys-next` 9 works with
/// (FFmpeg 9.0).
pub const MIN_AVCODEC_MAJOR: u32 = 63;

/// The target being compiled for, from the build-script environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub triple: String,
    pub os: String,
    pub arch: String,
    pub env: String,
    /// Whether the build runs on the target itself (source builds need it).
    pub native: bool,
}

impl Target {
    pub fn from_build_env() -> Self {
        let var = |k: &str| std::env::var(k).unwrap_or_default();
        let triple = var("TARGET");
        Target { native: var("HOST") == triple, triple, os: var("CARGO_CFG_TARGET_OS"), arch: var("CARGO_CFG_TARGET_ARCH"), env: var("CARGO_CFG_TARGET_ENV") }
    }

    pub fn is_windows(&self) -> bool {
        self.os == "windows"
    }
}

/// How the expected SHA-256 of a download is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Digest {
    /// Pinned in the source or given by the user.
    Known(String),
    /// Listed next to the archive (`<hex>  <name>` lines).
    Listed { list_url: String, name: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// An archive with `include/` and `lib/` (and `bin/`), used as is.
    Prebuilt { key: String, url: String, digest: Digest },
    /// A source release, configured and built into the cache.
    Source { key: String, url: String, sha256: String, libs: Vec<&'static str> },
}

impl Plan {
    /// Directory name in the cache. Distinct per target, origin and (for
    /// source builds) library set, so nothing stale is ever reused.
    pub fn key(&self) -> &str {
        match self {
            Plan::Prebuilt { key, .. } | Plan::Source { key, .. } => key,
        }
    }

    pub fn url(&self) -> &str {
        match self {
            Plan::Prebuilt { url, .. } | Plan::Source { url, .. } => url,
        }
    }
}

/// FFmpeg libraries a source build compiles, from the `ffmpeg-sys-next`
/// features (`CARGO_FEATURE_*` of the calling build script). `avutil` is
/// always built.
pub fn wanted_libs(feature: impl Fn(&str) -> bool) -> Vec<&'static str> {
    ["avcodec", "avdevice", "avfilter", "avformat", "swresample", "swscale"].into_iter().filter(|l| feature(l)).collect()
}

/// Chooses what to provide for `t`. `custom` is a user-supplied
/// `(url, sha256)`.
pub fn plan(t: &Target, custom: Option<(String, String)>, libs: Vec<&'static str>) -> Result<Plan> {
    if let Some((url, sha256)) = custom {
        let sha256 = sha256.trim().to_ascii_lowercase();
        if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("LIVESTAGE_FFMPEG_SHA256 must be the 64-digit SHA-256 of {url}");
        }
        return Ok(Plan::Prebuilt { key: format!("ffmpeg-custom-{}-{}", t.triple, &sha256[..16]), url, digest: Digest::Known(sha256) });
    }
    let btbn = match (t.os.as_str(), t.arch.as_str(), t.env.as_str()) {
        ("linux", "x86_64", "gnu") => Some(("linux64", "tar.xz")),
        ("linux", "aarch64", "gnu") => Some(("linuxarm64", "tar.xz")),
        ("windows", "x86_64", _) => Some(("win64", "zip")),
        ("windows", "aarch64", _) => Some(("winarm64", "zip")),
        _ => None,
    };
    if let Some((platform, ext)) = btbn {
        let name = format!("ffmpeg-n9.0-latest-{platform}-lgpl-shared-9.0.{ext}");
        return Ok(Plan::Prebuilt {
            key: format!("ffmpeg-n9.0-btbn-{platform}-lgpl-shared"),
            url: format!("{BTBN_RELEASE}/{name}"),
            digest: Digest::Listed { list_url: format!("{BTBN_RELEASE}/checksums.sha256"), name },
        });
    }
    if t.is_windows() {
        bail!("no prebuilt FFmpeg for {} — set FFMPEG_DIR to a shared FFmpeg 9 build", t.triple);
    }
    if !t.native {
        bail!("FFmpeg for {} has to be built from source, which needs a native build — set FFMPEG_DIR to an FFmpeg 9 build for that target", t.triple);
    }
    Ok(Plan::Source { key: format!("ffmpeg-{SOURCE_VERSION}-{}-{}", t.triple, libs.join("-")), url: SOURCE_URL.into(), sha256: SOURCE_SHA256.into(), libs })
}

/// The digest for `name` in a `sha256sum`-style list (`<hex>  <name>` or
/// `<hex> *<name>`).
pub fn listed_digest(list: &str, name: &str) -> Option<String> {
    list.lines().find_map(|line| {
        let (hex, file) = line.trim().split_once(char::is_whitespace)?;
        let file = file.trim_start().trim_start_matches('*');
        (file == name && hex.len() == 64).then(|| hex.to_ascii_lowercase())
    })
}

/// The major version from a pkg-config module version such as `63.1.102`.
pub fn major_version(version: &str) -> Option<u32> {
    version.split('.').next()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(triple: &str, os: &str, arch: &str, env: &str) -> Target {
        Target { triple: triple.into(), os: os.into(), arch: arch.into(), env: env.into(), native: true }
    }

    const LIBS: &[&str] = &["avcodec", "avformat", "swscale"];

    #[test]
    fn linux_and_windows_use_the_lgpl_shared_build() {
        let cases = [
            (target("x86_64-unknown-linux-gnu", "linux", "x86_64", "gnu"), "linux64-lgpl-shared-9.0.tar.xz"),
            (target("aarch64-unknown-linux-gnu", "linux", "aarch64", "gnu"), "linuxarm64-lgpl-shared-9.0.tar.xz"),
            (target("x86_64-pc-windows-msvc", "windows", "x86_64", "msvc"), "win64-lgpl-shared-9.0.zip"),
            (target("aarch64-pc-windows-msvc", "windows", "aarch64", "msvc"), "winarm64-lgpl-shared-9.0.zip"),
            (target("x86_64-pc-windows-gnu", "windows", "x86_64", "gnu"), "win64-lgpl-shared-9.0.zip"),
        ];
        for (t, suffix) in cases {
            let p = plan(&t, None, LIBS.to_vec()).unwrap();
            let Plan::Prebuilt { url, digest: Digest::Listed { list_url, name }, .. } = &p else { panic!("{p:?}") };
            assert!(url.ends_with(suffix), "{url}");
            assert!(url.ends_with(name.as_str()));
            assert!(list_url.ends_with("/checksums.sha256"));
            assert!(url.starts_with("https://"));
        }
    }

    #[test]
    fn macos_and_musl_build_the_pinned_source() {
        for t in [target("aarch64-apple-darwin", "macos", "aarch64", ""), target("x86_64-apple-darwin", "macos", "x86_64", ""), target("x86_64-unknown-linux-musl", "linux", "x86_64", "musl")] {
            let p = plan(&t, None, LIBS.to_vec()).unwrap();
            let Plan::Source { key, url, sha256, libs } = &p else { panic!("{p:?}") };
            assert_eq!(url, SOURCE_URL);
            assert_eq!(sha256, SOURCE_SHA256);
            assert_eq!(libs, LIBS);
            assert!(key.contains(&t.triple) && key.ends_with("avcodec-avformat-swscale"), "{key}");
        }
    }

    #[test]
    fn source_builds_need_a_native_build_and_unix() {
        let mut t = target("aarch64-apple-darwin", "macos", "aarch64", "");
        t.native = false;
        assert!(plan(&t, None, vec![]).unwrap_err().0.contains("FFMPEG_DIR"));
        let t = target("i686-pc-windows-msvc", "windows", "x86", "msvc");
        assert!(plan(&t, None, vec![]).unwrap_err().0.contains("FFMPEG_DIR"));
    }

    #[test]
    fn custom_archives_need_a_full_digest() {
        let t = target("aarch64-apple-darwin", "macos", "aarch64", "");
        assert!(plan(&t, Some(("https://x/ff.zip".into(), "abc".into())), vec![]).is_err());
        let hex = "AB".repeat(32);
        let p = plan(&t, Some(("https://x/ff.zip".into(), hex)), vec![]).unwrap();
        assert_eq!(p, Plan::Prebuilt { key: "ffmpeg-custom-aarch64-apple-darwin-abababababababab".into(), url: "https://x/ff.zip".into(), digest: Digest::Known("ab".repeat(32)) });
    }

    #[test]
    fn digests_are_found_in_sha256sum_lists() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let list = format!("{a}  ffmpeg-a.zip\n{b} *ffmpeg-b.tar.xz\nnot a line\n");
        assert_eq!(listed_digest(&list, "ffmpeg-a.zip"), Some(a));
        assert_eq!(listed_digest(&list, "ffmpeg-b.tar.xz"), Some("b".repeat(64)));
        assert_eq!(listed_digest(&list, "ffmpeg-a"), None);
    }

    #[test]
    fn library_set_follows_features() {
        let on = ["avcodec", "avformat", "swscale"];
        assert_eq!(wanted_libs(|l| on.contains(&l)), on);
        assert_eq!(major_version("63.1.102"), Some(63));
        assert_eq!(major_version("x"), None);
    }
}

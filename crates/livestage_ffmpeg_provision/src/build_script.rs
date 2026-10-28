//! The build-script entry point: decide whether FFmpeg needs providing,
//! provide it, and hand it to the upstream `ffmpeg-sys-next` discovery.

use crate::cache::{self, remove};
use crate::plan::{self, Digest, Plan, Target};
use crate::{bail, fetch, source, unpack, Error, Result};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Inputs that change the decision or the result.
const WATCHED_ENV: &[&str] = &["LIVESTAGE_FFMPEG", "LIVESTAGE_FFMPEG_CACHE", "LIVESTAGE_FFMPEG_URL", "LIVESTAGE_FFMPEG_SHA256", "PKG_CONFIG_PATH", "VCPKG_ROOT", "MACOSX_DEPLOYMENT_TARGET"];

/// A provided FFmpeg: `root` holds `include/` and `lib/` (and `bin/`).
#[derive(Debug)]
pub struct Provided {
    pub root: PathBuf,
    /// Downloaded or built by this run (rather than found in the cache).
    pub fresh: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum Decision {
    /// FFmpeg is configured (or building it is upstream's job): change nothing.
    Configured,
    Provide,
}

/// Called first thing in the `ffmpeg-sys-next` build script. Either
/// returns without touching anything, or leaves `FFMPEG_DIR` pointing at
/// a provided FFmpeg. Exits the build script with an explanation when
/// FFmpeg is needed but cannot be provided.
pub fn run() {
    for var in WATCHED_ENV {
        println!("cargo:rerun-if-env-changed={var}");
    }
    let t = Target::from_build_env();
    if decide(&t) == Decision::Configured {
        return;
    }
    match provide(&t) {
        Ok(p) => apply(&t, &p),
        Err(e) => {
            eprintln!("error: LiveStage could not provide FFmpeg for {}: {e}", t.triple);
            eprintln!();
            eprintln!("Alternatives:");
            eprintln!("  - point FFMPEG_DIR (or PKG_CONFIG_PATH) at an FFmpeg 9.x build with headers and shared libraries");
            eprintln!("  - LIVESTAGE_FFMPEG_URL + LIVESTAGE_FFMPEG_SHA256: an archive of such a build");
            eprintln!("  - build without `--features ffmpeg` (no movie playback)");
            std::process::exit(1);
        }
    }
}

fn var(name: &str) -> Option<String> {
    env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn decide(t: &Target) -> Decision {
    if env::var_os("CARGO_FEATURE_BUILD").is_some() {
        return Decision::Configured;
    }
    match var("LIVESTAGE_FFMPEG").map(|m| m.to_ascii_lowercase()).as_deref() {
        None | Some("auto") => {}
        Some("system") => return Decision::Configured,
        Some("fetch") => return Decision::Provide,
        Some(other) => println!("cargo:warning=LIVESTAGE_FFMPEG={other} is not one of auto, fetch, system; using auto"),
    }
    if var("LIVESTAGE_FFMPEG_URL").is_some() {
        return Decision::Provide;
    }
    if var("FFMPEG_DIR").is_some() || (t.env == "msvc" && vcpkg_has_ffmpeg()) {
        return Decision::Configured;
    }
    match pkg_config::Config::new().cargo_metadata(false).env_metadata(false).probe("libavcodec") {
        Ok(lib) if plan::major_version(&lib.version).is_some_and(|m| m >= plan::MIN_AVCODEC_MAJOR) => Decision::Configured,
        Ok(lib) => {
            println!("cargo:warning=system FFmpeg (libavcodec {}) predates FFmpeg 9; using a provided FFmpeg 9 instead", lib.version);
            Decision::Provide
        }
        Err(_) => Decision::Provide,
    }
}

/// Whether `VCPKG_ROOT` has an FFmpeg installed for some triplet; the
/// upstream discovery then takes it from vcpkg.
fn vcpkg_has_ffmpeg() -> bool {
    let Some(root) = var("VCPKG_ROOT") else { return false };
    let Ok(triplets) = fs::read_dir(Path::new(&root).join("installed")) else { return false };
    triplets.flatten().any(|d| d.path().join("include/libavcodec/avcodec.h").is_file())
}

/// Finds the provided FFmpeg in the cache, or downloads / builds it.
pub fn provide(t: &Target) -> Result<Provided> {
    let libs = plan::wanted_libs(|l| env::var_os(format!("CARGO_FEATURE_{}", l.to_ascii_uppercase())).is_some());
    let custom = match (var("LIVESTAGE_FFMPEG_URL"), var("LIVESTAGE_FFMPEG_SHA256")) {
        (Some(url), Some(sha)) => Some((url, sha)),
        (Some(url), None) => bail!("LIVESTAGE_FFMPEG_URL={url} needs LIVESTAGE_FFMPEG_SHA256"),
        _ => None,
    };
    let plan = plan::plan(t, custom, libs)?;
    let cache = cache::root();
    let dir = cache.join(plan.key());
    if cache::is_complete(&dir) {
        return Ok(Provided { root: dir, fresh: false });
    }
    let _lock = cache::lock(&cache, plan.key())?;
    // Another build may have finished it while we waited for the lock.
    if cache::is_complete(&dir) {
        return Ok(Provided { root: dir, fresh: false });
    }
    let downloads = cache.join("downloads");
    fs::create_dir_all(&downloads)?;
    let name = file_name(plan.url())?;
    let archive = downloads.join(&name);
    match &plan {
        Plan::Prebuilt { url, digest, .. } => {
            let sha = match digest {
                Digest::Known(sha) => {
                    fetch::download_verified(url, &archive, sha)?;
                    sha.clone()
                }
                Digest::Listed { list_url, name } => download_listed(url, list_url, name, &archive)?,
            };
            unpack::unpack(&archive, &name, &dir)?;
            check_layout(&dir, t)?;
            cache::mark_complete(&dir, &format!("url={url}\nsha256={sha}\n"))?;
        }
        Plan::Source { url, sha256, libs, key } => {
            fetch::download_verified(url, &archive, sha256)?;
            let src = cache.join(format!("{key}.src"));
            unpack::unpack(&archive, &name, &src)?;
            remove(&dir)?;
            let log = cache.join(format!("{key}.build.log"));
            remove(&log)?;
            if let Err(e) = source::build(&src, &dir, t, libs, &log) {
                // Keep the log for diagnosis, drop the half-built trees.
                remove(&dir)?;
                remove(&src)?;
                return Err(e);
            }
            // The object files are several hundred MB; the installed tree is all that's needed.
            remove(&src)?;
            check_layout(&dir, t)?;
            cache::mark_complete(&dir, &format!("url={url}\nsha256={sha256}\nbuilt-from-source\nlog={}\n", log.display()))?;
        }
    }
    remove(&archive)?;
    Ok(Provided { root: dir, fresh: true })
}

/// Downloads an archive whose digest is published next to it. A rolling
/// release can be replaced between reading the list and the download, so
/// one mismatch re-reads the list and tries again.
fn download_listed(url: &str, list_url: &str, name: &str, archive: &Path) -> Result<String> {
    let mut last = None;
    for _ in 0..2 {
        let list = fetch::text(list_url)?;
        let Some(sha) = plan::listed_digest(&list, name) else { bail!("{name} is not listed in {list_url}") };
        match fetch::download_verified(url, archive, &sha) {
            Ok(()) => return Ok(sha),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| Error("download failed".into())))
}

fn file_name(url: &str) -> Result<String> {
    let name = url.split(['?', '#']).next().unwrap_or(url).rsplit('/').next().unwrap_or_default();
    if name.is_empty() || name.contains("..") {
        bail!("{url} does not end in an archive file name");
    }
    Ok(name.to_string())
}

/// The headers and link libraries upstream will look for.
fn check_layout(dir: &Path, t: &Target) -> Result<()> {
    if !dir.join("include/libavcodec/avcodec.h").is_file() {
        bail!("{} has no include/libavcodec/avcodec.h", dir.display());
    }
    let lib = dir.join("lib");
    let found = fs::read_dir(&lib).map(|d| d.flatten().any(|e| e.file_name().to_string_lossy().contains("avcodec"))).unwrap_or(false);
    if !found {
        bail!("{} has no avcodec library", lib.display());
    }
    if t.is_windows() && !dir.join("bin").is_dir() {
        bail!("{} has no bin/ with the FFmpeg DLLs", dir.display());
    }
    Ok(())
}

/// Points the upstream discovery at `p`, and makes the libraries
/// loadable by the binaries this build produces.
fn apply(t: &Target, p: &Provided) {
    let root = &p.root;
    // The rest of this build script (the upstream code) reads these.
    env::set_var("FFMPEG_DIR", root);
    let pkgconfig = root.join("lib").join("pkgconfig");
    let existing = env::var_os("PKG_CONFIG_PATH").map(|v| env::split_paths(&v).collect::<Vec<_>>()).unwrap_or_default();
    if let Ok(joined) = env::join_paths(std::iter::once(pkgconfig).chain(existing)) {
        env::set_var("PKG_CONFIG_PATH", joined);
    }
    let runtime = root.join(if t.is_windows() { "bin" } else { "lib" });
    // Rebuild if the cache is deleted.
    println!("cargo:rerun-if-changed={}", root.join(cache::MARKER).display());
    // `links = "ffmpeg"` metadata: DEP_FFMPEG_LIVESTAGE_ROOT / _RUNTIME_DIR for
    // direct dependents (the app's build script adds an rpath from it).
    println!("cargo:livestage_root={}", root.display());
    println!("cargo:livestage_runtime_dir={}", runtime.display());
    if t.is_windows() {
        if let Err(e) = copy_dlls(&runtime) {
            println!("cargo:warning=FFmpeg DLLs were not copied next to the binaries ({e}); add {} to PATH", runtime.display());
        }
    }
    if p.fresh {
        println!("cargo:warning=FFmpeg provided for {} in {} (once; later builds reuse it)", t.triple, root.display());
    }
}

/// Windows finds DLLs next to the executable: copy them into the profile
/// directory and its `deps/` (tests) and `examples/`.
fn copy_dlls(from: &Path) -> Result<()> {
    let out = PathBuf::from(env::var_os("OUT_DIR").ok_or_else(|| Error("OUT_DIR not set".into()))?);
    let Some(profile) = cache::profile_dir(&out) else { bail!("unexpected OUT_DIR layout {}", out.display()) };
    let dlls: Vec<PathBuf> = fs::read_dir(from)?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("dll"))).collect();
    for dir in [profile.clone(), profile.join("deps"), profile.join("examples")] {
        fs::create_dir_all(&dir)?;
        for dll in &dlls {
            let dest = dir.join(dll.file_name().unwrap_or_default());
            let same = fs::metadata(&dest).ok().zip(fs::metadata(dll).ok()).is_some_and(|(a, b)| a.len() == b.len());
            if !same {
                fs::copy(dll, &dest)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_names_come_from_the_url() {
        assert_eq!(file_name("https://h/r/ffmpeg-9.0.2.tar.xz").unwrap(), "ffmpeg-9.0.2.tar.xz");
        assert_eq!(file_name("https://h/a.zip?raw=1").unwrap(), "a.zip");
        assert!(file_name("https://h/").is_err());
    }

    /// Downloads, verifies and unpacks the real Linux and Windows archives
    /// (~140 MB). `cargo test -p livestage_ffmpeg_provision -- --ignored`
    #[test]
    #[ignore = "network: downloads the BtbN FFmpeg 9.0 builds"]
    fn prebuilt_archives_download_verify_and_unpack() {
        let cache = env::temp_dir().join(format!("livestage-ffmpeg-net-{}", std::process::id()));
        for (triple, os) in [("x86_64-unknown-linux-gnu", "linux"), ("x86_64-pc-windows-msvc", "windows")] {
            let env_ = if os == "linux" { "gnu" } else { "msvc" };
            let t = Target { triple: triple.into(), os: os.into(), arch: "x86_64".into(), env: env_.into(), native: false };
            let Plan::Prebuilt { url, digest: Digest::Listed { list_url, name }, key } = plan::plan(&t, None, vec![]).unwrap() else { panic!() };
            let archive = cache.join(&name);
            fs::create_dir_all(&cache).unwrap();
            let sha = download_listed(&url, &list_url, &name, &archive).unwrap();
            assert_eq!(fetch::hash_file(&archive).unwrap(), sha);
            let dir = cache.join(&key);
            unpack::unpack(&archive, &name, &dir).unwrap();
            check_layout(&dir, &t).unwrap();
            assert!(dir.join("include/libavcodec/version_major.h").is_file());
            remove(&archive).unwrap();
        }
        remove(&cache).unwrap();
    }

    #[test]
    fn layout_check_wants_headers_and_libraries() {
        let dir = env::temp_dir().join(format!("livestage-ffmpeg-layout-{}", std::process::id()));
        remove(&dir).unwrap();
        let t = Target { triple: "x".into(), os: "linux".into(), arch: "x86_64".into(), env: "gnu".into(), native: true };
        fs::create_dir_all(dir.join("include/libavcodec")).unwrap();
        assert!(check_layout(&dir, &t).is_err());
        fs::write(dir.join("include/libavcodec/avcodec.h"), "").unwrap();
        fs::create_dir_all(dir.join("lib")).unwrap();
        fs::write(dir.join("lib/libavcodec.so.63"), "").unwrap();
        check_layout(&dir, &t).unwrap();
        let win = Target { os: "windows".into(), ..t };
        assert!(check_layout(&dir, &win).unwrap_err().0.contains("bin/"));
        remove(&dir).unwrap();
    }
}

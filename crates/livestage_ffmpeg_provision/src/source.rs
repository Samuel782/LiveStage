//! Builds the pinned FFmpeg source release into the cache (macOS and other
//! Unix targets without a prebuilt download).
//!
//! The build is a shared, LGPL, decode-and-encode FFmpeg with only the
//! libraries `ffmpeg-sys-next` was asked for. On macOS autodetection is
//! off and the system frameworks are enabled explicitly, so the result
//! never picks up Homebrew or MacPorts libraries that other machines
//! would lack.

use crate::plan::Target;
use crate::{bail, Result};
use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};

/// Every library FFmpeg's configure can switch off (`avutil` is always built).
const OPTIONAL_LIBS: &[&str] = &["avcodec", "avdevice", "avfilter", "avformat", "swresample", "swscale"];

/// `configure` arguments for installing into `prefix`.
pub fn configure_args(prefix: &Path, t: &Target, libs: &[&str], have_nasm: bool, macos_min: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = vec![
        format!("--prefix={}", prefix.display()),
        "--enable-shared".into(),
        "--disable-static".into(),
        "--disable-programs".into(),
        "--disable-doc".into(),
        "--disable-debug".into(),
    ];
    args.extend(OPTIONAL_LIBS.iter().filter(|l| !libs.contains(l)).map(|l| format!("--disable-{l}")));
    if t.os == "macos" {
        // Install names stay absolute (`<prefix>/lib/…`), so binaries linked
        // against the cache run without DYLD_* variables. No iconv: forced on
        // without autodetection it isn't linked (`-liconv`), and it only
        // serves subtitle charset conversion.
        args.extend(["--disable-autodetect", "--enable-videotoolbox", "--enable-audiotoolbox", "--enable-zlib", "--enable-bzlib"].map(String::from));
        if let Some(min) = macos_min {
            args.push(format!("--extra-cflags=-mmacosx-version-min={min}"));
            args.push(format!("--extra-ldflags=-mmacosx-version-min={min}"));
        }
    }
    if matches!(t.arch.as_str(), "x86_64" | "x86") && !have_nasm {
        // Plain C fallbacks: slower decoding, but it builds.
        args.push("--disable-x86asm".into());
    }
    args
}

/// Configures, builds and installs the source tree `src` into `prefix`,
/// logging to `log`.
pub fn build(src: &Path, prefix: &Path, t: &Target, libs: &[&str], log: &Path) -> Result<()> {
    let have_nasm = Command::new("nasm").arg("-v").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    if matches!(t.arch.as_str(), "x86_64" | "x86") && !have_nasm {
        println!("cargo:warning=nasm not found: building FFmpeg without x86 assembly (slower decoding). Install nasm and delete {} to rebuild.", prefix.display());
    }
    let macos_min = std::env::var("MACOSX_DEPLOYMENT_TARGET").ok().filter(|v| !v.is_empty());
    let args = configure_args(prefix, t, libs, have_nasm, macos_min.as_deref());
    let jobs = std::thread::available_parallelism().map_or(4, |n| n.get());
    let make = std::env::var("MAKE").unwrap_or_else(|_| "make".into());

    run(Command::new("sh").arg("./configure").args(&args).current_dir(src), "configure", log)?;
    run(Command::new(&make).arg(format!("-j{jobs}")).current_dir(src), "make", log)?;
    run(Command::new(&make).arg("install").current_dir(src), "make install", log)?;
    if !prefix.join("include/libavutil/avutil.h").is_file() {
        bail!("FFmpeg installed without headers into {} (see {})", prefix.display(), log.display());
    }
    Ok(())
}

/// Runs one build step with its output appended to `log`. Cargo's own
/// variables are removed: `make` would import `TARGET`, `PROFILE`, `DEBUG`
/// and the jobserver flags as make variables.
fn run(cmd: &mut Command, step: &str, log: &Path) -> Result<()> {
    let out = File::options().create(true).append(true).open(log)?;
    for (k, _) in std::env::vars_os() {
        let k = k.to_string_lossy();
        if k.starts_with("CARGO") || k.starts_with("RUSTC") || matches!(&*k, "TARGET" | "HOST" | "PROFILE" | "DEBUG" | "OPT_LEVEL" | "NUM_JOBS" | "OUT_DIR" | "MAKEFLAGS" | "MFLAGS" | "MAKELEVEL") {
            cmd.env_remove(&*k);
        }
    }
    let status = cmd.stdin(Stdio::null()).stdout(out.try_clone()?).stderr(out).status().map_err(|e| crate::Error(format!("could not run {step} ({e}); building FFmpeg needs a C compiler, make and sh")))?;
    if !status.success() {
        let text = std::fs::read_to_string(log).unwrap_or_default();
        let tail: Vec<&str> = text.lines().rev().take(25).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        bail!("FFmpeg {step} failed ({status}). Last lines of {}:\n{}", log.display(), tail.join("\n"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(os: &str, arch: &str) -> Target {
        Target { triple: format!("{arch}-{os}"), os: os.into(), arch: arch.into(), env: String::new(), native: true }
    }

    #[test]
    fn macos_build_uses_only_system_frameworks() {
        let a = configure_args(Path::new("/c/ff"), &target("macos", "aarch64"), &["avcodec", "avformat", "swscale"], false, Some("12.0"));
        for want in ["--prefix=/c/ff", "--enable-shared", "--disable-static", "--disable-programs", "--disable-autodetect", "--enable-videotoolbox", "--disable-avdevice", "--disable-avfilter", "--disable-swresample", "--extra-cflags=-mmacosx-version-min=12.0"] {
            assert!(a.iter().any(|x| x == want), "{want} missing from {a:?}");
        }
        assert!(!a.iter().any(|x| x == "--disable-avcodec" || x == "--disable-swscale" || x == "--disable-x86asm"));
        assert!(!a.iter().any(|x| x.contains("gpl") || x.contains("nonfree")));
    }

    #[test]
    fn x86_without_nasm_drops_assembly() {
        let t = target("linux", "x86_64");
        let a = configure_args(Path::new("/p"), &t, OPTIONAL_LIBS, false, None);
        assert!(a.iter().any(|x| x == "--disable-x86asm"));
        assert!(!a.iter().any(|x| x.starts_with("--disable-av") || x == "--disable-autodetect"));
        let a = configure_args(Path::new("/p"), &t, OPTIONAL_LIBS, true, None);
        assert!(!a.iter().any(|x| x == "--disable-x86asm"));
    }
}

//! Embeds the application icon in the Windows executable: the PNGs in
//! `icon/` are packed into an `.ico` (see `src/ico.rs`) and compiled into
//! the resource section with `winresource`, so Explorer, the taskbar and
//! shortcuts show it. Other platforms set the icon at run time.
//!
//! With `--features ffmpeg` and an FFmpeg provided by the build (see
//! `livestage_ffmpeg_provision`), it also adds an rpath on Linux so the
//! binary finds those libraries when launched directly.

#[allow(dead_code)]
#[path = "src/ico.rs"]
mod ico;

fn main() {
    println!("cargo:rerun-if-changed=../../icon");
    println!("cargo:rerun-if-changed=src/ico.rs");
    ffmpeg_rpath();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let sizes = ["16", "32", "64", "128", "256"];
    let pngs: Vec<Vec<u8>> = sizes
        .iter()
        .map(|s| std::fs::read(format!("../../icon/{s}.png")).unwrap_or_else(|e| panic!("icon/{s}.png: {e}")))
        .collect();
    let refs: Vec<&[u8]> = pngs.iter().map(Vec::as_slice).collect();
    let ico = ico::encode(&refs).unwrap_or_else(|e| panic!("building the .ico: {e}"));
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("livestage.ico");
    std::fs::write(&out, ico).unwrap_or_else(|e| panic!("{}: {e}", out.display()));
    embed(&out);
}

/// `cargo run` / `cargo test` put the provided FFmpeg on the library path
/// themselves; a directly launched Linux binary needs an rpath. macOS
/// libraries carry absolute install names and Windows DLLs are copied next
/// to the binary, so neither needs one. `scripts/build-release.sh`
/// replaces it with `$ORIGIN/../lib` when bundling.
fn ffmpeg_rpath() {
    println!("cargo:rerun-if-env-changed=DEP_FFMPEG_LIVESTAGE_RUNTIME_DIR");
    let Ok(dir) = std::env::var("DEP_FFMPEG_LIVESTAGE_RUNTIME_DIR") else { return };
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if !matches!(os.as_str(), "macos" | "ios" | "windows") {
        println!("cargo:rustc-link-arg-bins=-Wl,-rpath,{dir}");
    }
}

#[cfg(windows)]
fn embed(ico: &std::path::Path) {
    let mut res = winresource::WindowsResource::new();
    res.set_icon(&ico.to_string_lossy());
    res.set("ProductName", "LiveStage");
    res.set("FileDescription", "LiveStage show control");
    if let Err(e) = res.compile() {
        println!("cargo:warning=the Windows icon resource was not embedded: {e}");
    }
}

#[cfg(not(windows))]
fn embed(_ico: &std::path::Path) {
    // Cross-compiling: no resource compiler here. The window still gets
    // its icon at run time.
    println!("cargo:warning=cross-compiled for Windows without the icon resource (build on Windows to embed it)");
}

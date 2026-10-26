//! Embeds the application icon in the Windows executable: the PNGs in
//! `icon/` are packed into an `.ico` (see `src/ico.rs`) and compiled into
//! the resource section with `winresource`, so Explorer, the taskbar and
//! shortcuts show it. Other platforms set the icon at run time.

#[allow(dead_code)]
#[path = "src/ico.rs"]
mod ico;

fn main() {
    println!("cargo:rerun-if-changed=../../icon");
    println!("cargo:rerun-if-changed=src/ico.rs");
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

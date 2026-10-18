//! Presentations (PDF, PowerPoint) as slide cues.
//!
//! ```text
//!  import (background thread)                        show time
//!  ──────────────────────────                        ─────────
//!  .pdf  ── hayro (pure Rust) ──┐                     SlideSource: decodes the
//!  .pptx ── LibreOffice → PDF ──┼─▶ 001.png 002.png …  current slide and its
//!        └─ built-in renderer ──┘   + text / notes     neighbours on a worker
//!  .ppt/.key/.odp ─ LibreOffice ┘   (SlideDeck)        thread → wgpu texture
//! ```
//!
//! Rendering happens once, at import, at the resolution of the largest
//! output, so slide changes during the show only decode a PNG that has
//! usually been prefetched already.

pub mod pdf;
pub mod pptx;
mod source;

pub use source::SlideSource;

use crate::{VideoError, VideoResult};
use livestage_core::cue::{Slide, SlideDeck};
use livestage_core::project::AssetRef;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// One rendered page: straight-alpha RGBA plus its text.
pub struct RenderedSlide {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub text: String,
    pub notes: String,
}

#[derive(Clone, Debug)]
pub struct ImportOptions {
    /// Slides are contain-fitted into this size (pixels).
    pub target: (u32, u32),
    /// LibreOffice `soffice` binary used for PowerPoint fidelity and for
    /// `.ppt` / `.key` / `.odp`. `None` = built-in PowerPoint renderer only.
    pub office: Option<PathBuf>,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self { target: (1920, 1080), office: find_office() }
    }
}

/// Locates LibreOffice: `$LIVESTAGE_SOFFICE`, then `PATH`, then the
/// standard install locations.
pub fn find_office() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("LIVESTAGE_SOFFICE").map(PathBuf::from) {
        return p.is_file().then_some(p);
    }
    let names: &[&str] = if cfg!(windows) { &["soffice.exe", "soffice.com"] } else { &["soffice", "libreoffice"] };
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for n in names {
                let p = dir.join(n);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    let known: &[&str] = &[
        "/Applications/LibreOffice.app/Contents/MacOS/soffice",
        "/usr/bin/soffice",
        "/usr/lib/libreoffice/program/soffice",
        "/opt/libreoffice/program/soffice",
        r"C:\Program Files\LibreOffice\program\soffice.exe",
    ];
    known.iter().map(PathBuf::from).find(|p| p.is_file())
}

fn extension(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

/// Whether `path` is a document this module can turn into slides.
pub fn is_document(path: &Path) -> bool {
    matches!(extension(path).as_str(), "pdf" | "pptx" | "ppsx" | "potx" | "ppt" | "odp" | "key")
}

/// Converts an office document to PDF with LibreOffice (headless).
fn office_to_pdf(office: &Path, src: &Path) -> VideoResult<(PathBuf, PathBuf)> {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let out = std::env::temp_dir().join(format!("livestage-convert-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&out)?;
    // A private profile avoids clashing with a running LibreOffice instance.
    let profile = format!("-env:UserInstallation=file://{}", out.join("profile").display());
    let mut child = Command::new(office)
        .arg(profile)
        .args(["--headless", "--norestore", "--convert-to", "pdf", "--outdir"])
        .arg(&out)
        .arg(src)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                return Err(VideoError::Decode { path: src.display().to_string(), msg: format!("LibreOffice conversion failed ({status})") });
            }
            break;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            return Err(VideoError::Decode { path: src.display().to_string(), msg: "LibreOffice conversion timed out".into() });
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let stem = src.file_stem().unwrap_or_default();
    let pdf = out.join(stem).with_extension("pdf");
    if !pdf.is_file() {
        return Err(VideoError::Decode { path: src.display().to_string(), msg: "LibreOffice produced no PDF".into() });
    }
    Ok((pdf, out))
}

fn write_png(path: &Path, s: &RenderedSlide) -> VideoResult<()> {
    use image::ImageEncoder;
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let enc = image::codecs::png::PngEncoder::new_with_quality(file, image::codecs::png::CompressionType::Fast, image::codecs::png::FilterType::Adaptive);
    enc.write_image(&s.rgba, s.width, s.height, image::ExtendedColorType::Rgba8)
        .map_err(|e| VideoError::Image { path: path.display().to_string(), msg: e.to_string() })
}

/// Renders a document into `out_dir` as `001.png`, `002.png`, ... and
/// returns the deck. `rel_prefix` is the bundle-relative path of `out_dir`
/// (e.g. `assets/slides/Keynote`). `progress(done, total)` is called per slide.
pub fn import_document(
    src: &Path,
    source_ref: Option<AssetRef>,
    out_dir: &Path,
    rel_prefix: &str,
    opts: &ImportOptions,
    progress: &mut dyn FnMut(usize, usize),
) -> VideoResult<SlideDeck> {
    let ext = extension(src);
    let mut deck = SlideDeck { source: source_ref, size: [0, 0], slides: Vec::new() };
    let mut sink = |i: usize, total: usize, s: RenderedSlide| -> VideoResult<()> {
        let name = format!("{:03}.png", i + 1);
        write_png(&out_dir.join(&name), &s)?;
        if deck.size == [0, 0] {
            deck.size = [s.width, s.height];
        }
        deck.slides.push(Slide { image: AssetRef::new(format!("{rel_prefix}/{name}")), text: s.text, notes: s.notes });
        progress(i + 1, total);
        Ok(())
    };
    match (ext.as_str(), opts.office.as_deref()) {
        ("pdf", _) => pdf::render_pages(src, opts.target, &mut sink)?,
        ("pptx" | "ppsx" | "potx", None) => pptx::render_slides(src, opts.target, &mut sink)?,
        ("pptx" | "ppsx" | "potx", Some(office)) => match office_to_pdf(office, src) {
            Ok((pdf_path, tmp)) => {
                let result = pdf::render_pages(&pdf_path, opts.target, &mut sink);
                let _ = std::fs::remove_dir_all(tmp);
                result?;
                // PowerPoint's own text and speaker notes beat text scraped from the PDF.
                if let Ok(texts) = pptx::extract_text(src) {
                    if texts.len() == deck.slides.len() {
                        for (slide, (text, notes)) in deck.slides.iter_mut().zip(texts) {
                            slide.text = text;
                            slide.notes = notes;
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!("{e}; using the built-in PowerPoint renderer");
                pptx::render_slides(src, opts.target, &mut sink)?;
            }
        },
        ("ppt" | "odp" | "key", Some(office)) => {
            let (pdf_path, tmp) = office_to_pdf(office, src)?;
            let result = pdf::render_pages(&pdf_path, opts.target, &mut sink);
            let _ = std::fs::remove_dir_all(tmp);
            result?;
        }
        ("ppt" | "odp" | "key", None) => {
            return Err(VideoError::Unsupported(format!(
                ".{ext} needs LibreOffice to import (or export the file as PDF / .pptx)"
            )))
        }
        _ => return Err(VideoError::Unsupported(format!("not a presentation: {}", src.display()))),
    }
    Ok(deck)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A two-page PDF with text, written by hand (no external tools).
    pub(crate) fn sample_pdf(path: &Path) {
        let pages = ["Opening Night", "Second Act"];
        let mut objs: Vec<String> = Vec::new();
        objs.push("<< /Type /Catalog /Pages 2 0 R >>".into());
        let kids: Vec<String> = (0..pages.len()).map(|i| format!("{} 0 R", 3 + i * 2)).collect();
        objs.push(format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), pages.len()));
        let font_obj = 3 + pages.len() * 2;
        for (i, text) in pages.iter().enumerate() {
            let content = format!("0 0.5 0 rg 0 0 400 225 re f BT /F1 24 Tf 1 1 1 rg 40 120 Td ({text}) Tj ET");
            objs.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 225] /Resources << /Font << /F1 {font_obj} 0 R >> >> /Contents {} 0 R >>",
                4 + i * 2
            ));
            objs.push(format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()));
        }
        objs.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).bytes());
        for off in offsets {
            out.extend(format!("{off:010} 00000 n \n").bytes());
        }
        out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).bytes());
        std::fs::write(path, out).unwrap();
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("livestage-slides-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn imports_pdf_pages_as_slides() {
        let dir = scratch("pdf");
        let src = dir.join("talk.pdf");
        sample_pdf(&src);
        let out = dir.join("slides");
        std::fs::create_dir_all(&out).unwrap();
        let mut seen = Vec::new();
        let opts = ImportOptions { target: (1280, 720), office: None };
        let deck = import_document(&src, None, &out, "assets/slides/talk", &opts, &mut |d, t| seen.push((d, t))).unwrap();
        assert_eq!(seen, vec![(1, 2), (2, 2)]);
        assert_eq!(deck.size, [1280, 720]);
        assert_eq!(deck.slides[0].image.as_str(), "assets/slides/talk/001.png");
        assert_eq!(deck.slides[0].text, "Opening Night");
        assert_eq!(deck.slides[1].text, "Second Act");
        let img = image::open(out.join("002.png")).unwrap().to_rgba8();
        assert_eq!(img.dimensions(), (1280, 720));
        let p = img.get_pixel(10, 10).0;
        assert!(p[1] > 100 && p[0] < 30, "page background drawn: {p:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn imports_pptx_without_libreoffice() {
        let dir = scratch("pptx");
        let src = dir.join("deck.pptx");
        pptx::tests::sample_pptx(&src);
        let out = dir.join("slides");
        std::fs::create_dir_all(&out).unwrap();
        let opts = ImportOptions { target: (1920, 1080), office: None };
        let deck = import_document(&src, Some(AssetRef::new("assets/documents/deck.pptx")), &out, "assets/slides/deck", &opts, &mut |_, _| {}).unwrap();
        assert_eq!(deck.slides.len(), 2);
        assert_eq!(deck.slides[1].notes, "Cue the band after this slide.");
        assert!(out.join("001.png").is_file() && out.join("002.png").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_formats_need_libreoffice() {
        let dir = scratch("key");
        let src = dir.join("old.ppt");
        std::fs::write(&src, b"not really").unwrap();
        let opts = ImportOptions { target: (640, 360), office: None };
        let err = import_document(&src, None, &dir, "x", &opts, &mut |_, _| {}).unwrap_err();
        assert!(err.to_string().contains("LibreOffice"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

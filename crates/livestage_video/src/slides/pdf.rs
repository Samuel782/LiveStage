//! PDF pages → RGBA bitmaps + text, with the pure-Rust `hayro` rasteriser.

use super::RenderedSlide;
use crate::{VideoError, VideoResult};
use hayro::hayro_interpret::font::Glyph;
use hayro::hayro_interpret::hayro_cmap::BfString;
use hayro::hayro_interpret::{
    interpret_page, BlendMode, ClipPath, Context, Device, GlyphDrawMode, Image, InterpreterCache, InterpreterSettings, Paint,
    PathDrawMode, SoftMask,
};
use hayro::hayro_interpret::TransformExt;
use hayro::hayro_syntax::Pdf;
use hayro::{render, RenderCache, RenderSettings};
use std::path::Path;
use std::sync::Arc;

/// Number of pages in a PDF.
pub fn page_count(path: &Path) -> VideoResult<usize> {
    Ok(open(path)?.pages().len())
}

fn open(path: &Path) -> VideoResult<Pdf> {
    let data = std::fs::read(path)?;
    Pdf::new(Arc::new(data)).map_err(|e| VideoError::Decode { path: path.display().to_string(), msg: format!("{e:?}") })
}

/// Renders every page contain-fitted into `target` (white paper), calling
/// `each(index, total, slide)` as pages complete so callers can stream them
/// to disk instead of holding a whole deck in memory.
pub fn render_pages(path: &Path, target: (u32, u32), each: &mut dyn FnMut(usize, usize, RenderedSlide) -> VideoResult<()>) -> VideoResult<()> {
    let pdf = open(path)?;
    let pages = pdf.pages();
    let total = pages.len();
    if total == 0 {
        return Err(VideoError::Decode { path: path.display().to_string(), msg: "document has no pages".into() });
    }
    let cache = RenderCache::new();
    let settings = InterpreterSettings::default();
    for (i, page) in pages.iter().enumerate() {
        let (w, h) = page.render_dimensions();
        let scale = (target.0 as f32 / w.max(1.0)).min(target.1 as f32 / h.max(1.0));
        let pixmap = render(
            page,
            &cache,
            &settings,
            &RenderSettings {
                x_scale: scale,
                y_scale: scale,
                bg_color: hayro::vello_cpu::color::palette::css::WHITE,
                ..Default::default()
            },
        );
        let (pw, ph) = (pixmap.width() as u32, pixmap.height() as u32);
        let rgba = pixmap.take_unpremultiplied().into_iter().flat_map(|p| [p.r, p.g, p.b, p.a]).collect();
        let text = page_text(page, &settings);
        each(i, total, RenderedSlide { width: pw, height: ph, rgba, text, notes: String::new() })?;
    }
    Ok(())
}

fn page_text(page: &hayro::hayro_syntax::page::Page<'_>, settings: &InterpreterSettings) -> String {
    let (w, h) = page.render_dimensions();
    let mut dev = TextCollector::default();
    let cache = InterpreterCache::new();
    let mut ctx = Context::new(
        page.initial_transform(true).to_kurbo(),
        kurbo::Rect::new(0.0, 0.0, w as f64, h as f64),
        &cache,
        page.xref(),
        settings.clone(),
    );
    interpret_page(page, &mut ctx, &mut dev);
    dev.finish()
}

/// Collects glyph text in drawing order, breaking lines when the baseline moves.
#[derive(Default)]
struct TextCollector {
    lines: Vec<String>,
    line: String,
    last: Option<(f64, f64)>,
    /// Running average horizontal advance on the current line (word gaps).
    advance: f64,
}

impl TextCollector {
    fn push(&mut self, x: f64, y: f64, text: &str) {
        if let Some((lx, ly)) = self.last {
            if (ly - y).abs() > 2.0 {
                self.lines.push(std::mem::take(&mut self.line));
                self.advance = 0.0;
            } else {
                let dx = x - lx;
                if self.advance > 0.0 && dx > self.advance * 1.8 && !self.line.ends_with(' ') {
                    self.line.push(' ');
                }
                if dx > 0.0 {
                    self.advance = if self.advance == 0.0 { dx } else { self.advance * 0.8 + dx * 0.2 };
                }
            }
        }
        self.last = Some((x, y));
        self.line.push_str(text);
    }

    fn finish(mut self) -> String {
        self.lines.push(self.line);
        clean_text(&self.lines.join("\n"))
    }
}

/// Collapses runs of whitespace and drops empty lines.
pub(crate) fn clean_text(s: &str) -> String {
    s.lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

impl<'a> Device<'a> for TextCollector {
    fn set_soft_mask(&mut self, _: Option<SoftMask<'a>>) {}
    fn set_blend_mode(&mut self, _: BlendMode) {}
    fn draw_path(&mut self, _: &kurbo::BezPath, _: kurbo::Affine, _: &Paint<'a>, _: &PathDrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_glyph(
        &mut self,
        glyph: &Glyph<'a>,
        transform: kurbo::Affine,
        glyph_transform: kurbo::Affine,
        _: &Paint<'a>,
        _: &GlyphDrawMode,
    ) {
        let at = (transform * glyph_transform).translation();
        match glyph.as_unicode() {
            Some(BfString::Char(c)) => self.push(at.x, at.y, c.encode_utf8(&mut [0; 4])),
            Some(BfString::String(s)) => self.push(at.x, at.y, &s),
            None => {}
        }
    }
    fn draw_image(&mut self, _: Image<'a, '_>, _: kurbo::Affine) {}
    fn pop_clip_path(&mut self) {}
    fn pop_transparency_group(&mut self) {}
}

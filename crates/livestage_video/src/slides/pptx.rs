//! PowerPoint (`.pptx`) reader and built-in slide renderer.
//!
//! A `.pptx` is a zip of DrawingML XML parts. This module reads the slide
//! order, slide size, theme colours, backgrounds, filled shapes, pictures,
//! text (with wrapping, alignment, bullets, autofit scale and inherited
//! placeholder positions), groups and speaker notes, and rasterises each
//! slide on the CPU.
//!
//! It covers typical show decks (titles, bullet text, photos, colour
//! blocks). Charts, SmartArt, tables, gradients, effects and embedded
//! fonts are not drawn; when LibreOffice is installed the importer renders
//! through it instead for full fidelity and only uses this module for the
//! slide text and notes.

use super::RenderedSlide;
use crate::{VideoError, VideoResult};
use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use image::{Rgba, RgbaImage};
use roxmltree::{Document, Node};
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

/// English Metric Units per point.
const EMU_PER_PT: f64 = 12_700.0;
/// Default slide size (16:9, 13.333 × 7.5 in).
const DEFAULT_SIZE: (i64, i64) = (12_192_000, 6_858_000);

struct Package {
    zip: zip::ZipArchive<std::io::Cursor<Vec<u8>>>,
}

impl Package {
    fn open(path: &Path) -> VideoResult<Self> {
        let data = std::fs::read(path)?;
        let zip = zip::ZipArchive::new(std::io::Cursor::new(data)).map_err(|e| bad(path, e))?;
        Ok(Self { zip })
    }

    fn bytes(&mut self, name: &str) -> Option<Vec<u8>> {
        let mut f = self.zip.by_name(name).ok()?;
        let mut out = Vec::with_capacity(f.size() as usize);
        f.read_to_end(&mut out).ok()?;
        Some(out)
    }

    fn text(&mut self, name: &str) -> Option<String> {
        self.bytes(name).and_then(|b| String::from_utf8(b).ok())
    }

    /// Relationships of a part: id → (type suffix, absolute part name).
    fn rels(&mut self, part: &str) -> HashMap<String, (String, String)> {
        let (dir, file) = part.rsplit_once('/').unwrap_or(("", part));
        let rels_name = format!("{dir}/_rels/{file}.rels");
        let Some(xml) = self.text(&rels_name) else { return HashMap::new() };
        let Ok(doc) = Document::parse(&xml) else { return HashMap::new() };
        doc.descendants()
            .filter(|n| n.tag_name().name() == "Relationship")
            .filter_map(|n| {
                let id = n.attribute("Id")?.to_string();
                let ty = n.attribute("Type")?.rsplit('/').next()?.to_string();
                let target = n.attribute("Target")?;
                if n.attribute("TargetMode") == Some("External") {
                    return None;
                }
                Some((id, (ty, resolve_part(dir, target))))
            })
            .collect()
    }
}

fn bad(path: &Path, e: impl std::fmt::Display) -> VideoError {
    VideoError::Decode { path: path.display().to_string(), msg: e.to_string() }
}

/// Resolves a relationship target relative to the source part's folder.
fn resolve_part(dir: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_string();
    }
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for seg in target.split('/') {
        match seg {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            s => parts.push(s),
        }
    }
    parts.join("/")
}

// ------------------------------------------------------------------ model --

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Anchor {
    Top,
    Middle,
    Bottom,
}

#[derive(Clone, Debug)]
struct Run {
    text: String,
    size_pt: Option<f64>,
    bold: bool,
    color: Option<[u8; 3]>,
}

#[derive(Clone, Debug)]
struct Para {
    runs: Vec<Run>,
    align: Option<Align>,
    level: u32,
    bullet: Option<bool>,
}

#[derive(Clone, Debug)]
struct TextBody {
    paras: Vec<Para>,
    /// Shape-level list style (`a:lstStyle`).
    lst: ListStyle,
    anchor: Option<Anchor>,
    /// `normAutofit fontScale` (1.0 = 100 %).
    font_scale: f64,
    insets: [f64; 4],
}

/// Paragraph defaults for one outline level (from `lvlNpPr`).
#[derive(Clone, Debug, Default)]
struct LevelStyle {
    align: Option<Align>,
    size_pt: Option<f64>,
    bullet: Option<bool>,
    color: Option<[u8; 3]>,
}

impl LevelStyle {
    /// Fills unset fields from `fallback`.
    fn or(mut self, fallback: &LevelStyle) -> LevelStyle {
        self.align = self.align.or(fallback.align);
        self.size_pt = self.size_pt.or(fallback.size_pt);
        self.bullet = self.bullet.or(fallback.bullet);
        self.color = self.color.or(fallback.color);
        self
    }
}

/// Level styles indexed by outline level (0-based).
type ListStyle = Vec<LevelStyle>;

fn level(lst: &ListStyle, lvl: u32) -> LevelStyle {
    lst.get(lvl as usize).cloned().unwrap_or_default()
}

fn parse_align(a: &str) -> Align {
    match a {
        "ctr" | "just" | "dist" => Align::Center,
        "r" => Align::Right,
        _ => Align::Left,
    }
}

fn bullet_of(ppr: Node) -> Option<bool> {
    if child(ppr, "buNone").is_some() {
        Some(false)
    } else if child(ppr, "buChar").is_some() || child(ppr, "buAutoNum").is_some() || child(ppr, "buBlip").is_some() {
        Some(true)
    } else {
        None
    }
}

/// Parses `a:lstStyle`, `p:titleStyle`, `p:bodyStyle` or `p:otherStyle`.
fn list_style(n: Option<Node>, theme: &Theme) -> ListStyle {
    let mut out = vec![LevelStyle::default(); 9];
    let Some(n) = n else { return out };
    for l in n.children().filter(|c| c.is_element()) {
        let name = l.tag_name().name();
        let Some(i) = name.strip_prefix("lvl").and_then(|r| r.strip_suffix("pPr")).and_then(|d| d.parse::<usize>().ok()) else { continue };
        if !(1..=9).contains(&i) {
            continue;
        }
        let def = child(l, "defRPr");
        out[i - 1] = LevelStyle {
            align: l.attribute("algn").map(parse_align),
            size_pt: def.and_then(|d| num(d, "sz")).map(|v| v / 100.0),
            bullet: bullet_of(l),
            color: def.and_then(|d| child(d, "solidFill")).and_then(|f| color_of(f, theme)).map(|c| [c[0], c[1], c[2]]),
        };
    }
    out
}

/// The master's `p:txStyles`.
#[derive(Clone, Debug, Default)]
struct MasterStyles {
    title: ListStyle,
    body: ListStyle,
    other: ListStyle,
}

/// A placeholder declared on a layout (depth 0) or master (depth 1).
#[derive(Clone, Debug)]
struct InheritedPh {
    depth: u8,
    ph: Placeholder,
    rect: Option<Rect>,
    anchor: Option<Anchor>,
    lst: ListStyle,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Placeholder {
    kind: String,
    idx: Option<String>,
}

#[derive(Clone, Debug)]
enum Element {
    Shape { rect: Option<Rect>, fill: Option<[u8; 4]>, text: Option<TextBody>, ph: Option<Placeholder> },
    Picture { rect: Option<Rect>, media: String },
}

/// Per-axis mapping from a group's child space to slide space.
#[derive(Clone, Copy)]
struct Space {
    sx: f64,
    sy: f64,
    ox: f64,
    oy: f64,
}

impl Space {
    const IDENTITY: Space = Space { sx: 1.0, sy: 1.0, ox: 0.0, oy: 0.0 };
    fn map(&self, r: Rect) -> Rect {
        Rect { x: self.ox + r.x * self.sx, y: self.oy + r.y * self.sy, w: r.w * self.sx, h: r.h * self.sy }
    }
}

fn child<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.tag_name().name() == name)
}

fn path<'a, 'i>(n: Node<'a, 'i>, names: &[&str]) -> Option<Node<'a, 'i>> {
    names.iter().try_fold(n, |n, name| child(n, name))
}

fn num(n: Node, attr: &str) -> Option<f64> {
    n.attribute(attr).and_then(|v| v.parse().ok())
}

fn attr_local<'a>(n: Node<'a, '_>, local: &str) -> Option<&'a str> {
    n.attributes().find(|a| a.name() == local).map(|a| a.value())
}

fn xfrm(sp_pr: Option<Node>) -> Option<Rect> {
    let x = child(sp_pr?, "xfrm")?;
    let off = child(x, "off")?;
    let ext = child(x, "ext")?;
    Some(Rect { x: num(off, "x")?, y: num(off, "y")?, w: num(ext, "cx")?, h: num(ext, "cy")? })
}

/// Theme colour scheme: name (dk1, lt1, accent1, ...) → RGB.
type Theme = HashMap<String, [u8; 3]>;

fn hex(v: &str) -> Option<[u8; 3]> {
    if v.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(v, 16).ok()?;
    Some([(n >> 16) as u8, (n >> 8) as u8, n as u8])
}

/// Resolves a colour element (`srgbClr`, `schemeClr`, `sysClr`, `prstClr`) and
/// its alpha / lumMod / lumOff modifiers.
fn color_of(n: Node, theme: &Theme) -> Option<[u8; 4]> {
    let c = n.children().find(|c| c.is_element())?;
    let mut rgb = match c.tag_name().name() {
        "srgbClr" => hex(c.attribute("val")?)?,
        "sysClr" => hex(c.attribute("lastClr").unwrap_or("000000"))?,
        "schemeClr" => {
            let key = match c.attribute("val")? {
                "tx1" => "dk1",
                "bg1" => "lt1",
                "tx2" => "dk2",
                "bg2" => "lt2",
                other => other,
            };
            *theme.get(key)?
        }
        "prstClr" => match c.attribute("val")? {
            "white" => [255, 255, 255],
            "red" => [255, 0, 0],
            "green" => [0, 128, 0],
            "blue" => [0, 0, 255],
            "yellow" => [255, 255, 0],
            _ => [0, 0, 0],
        },
        _ => return None,
    };
    let mut alpha = 255u8;
    let (mut lum_mod, mut lum_off) = (1.0, 0.0);
    for m in c.children().filter(|m| m.is_element()) {
        let v = num(m, "val").unwrap_or(100_000.0) / 100_000.0;
        match m.tag_name().name() {
            "alpha" => alpha = (v.clamp(0.0, 1.0) * 255.0) as u8,
            "lumMod" => lum_mod = v,
            "lumOff" => lum_off = v,
            _ => {}
        }
    }
    if lum_mod != 1.0 || lum_off != 0.0 {
        // Approximation of HSL luminance modulation in RGB space.
        rgb = rgb.map(|ch| ((ch as f64 / 255.0 * lum_mod + lum_off).clamp(0.0, 1.0) * 255.0) as u8);
    }
    Some([rgb[0], rgb[1], rgb[2], alpha])
}

fn solid_fill(sp_pr: Option<Node>, theme: &Theme) -> Option<[u8; 4]> {
    let sp_pr = sp_pr?;
    if child(sp_pr, "noFill").is_some() {
        return None;
    }
    color_of(child(sp_pr, "solidFill")?, theme)
}

fn placeholder(nv: Option<Node>) -> Option<Placeholder> {
    let ph = path(nv?, &["nvPr", "ph"])?;
    Some(Placeholder {
        kind: ph.attribute("type").unwrap_or(if ph.attribute("idx").is_some() { "body" } else { "obj" }).to_string(),
        idx: ph.attribute("idx").map(str::to_string),
    })
}

fn text_body(tx: Node, theme: &Theme) -> TextBody {
    let body_pr = child(tx, "bodyPr");
    let anchor = body_pr.and_then(|b| b.attribute("anchor")).map(|a| match a {
        "ctr" => Anchor::Middle,
        "b" => Anchor::Bottom,
        _ => Anchor::Top,
    });
    let font_scale = body_pr
        .and_then(|b| child(b, "normAutofit"))
        .and_then(|a| num(a, "fontScale"))
        .map_or(1.0, |v| v / 100_000.0);
    let inset = |name: &str, default: f64| body_pr.and_then(|b| num(b, name)).unwrap_or(default);
    let insets = [inset("lIns", 91_440.0), inset("tIns", 45_720.0), inset("rIns", 91_440.0), inset("bIns", 45_720.0)];
    let paras = tx
        .children()
        .filter(|p| p.tag_name().name() == "p")
        .map(|p| {
            let ppr = child(p, "pPr");
            let align = ppr.and_then(|a| a.attribute("algn")).map(parse_align);
            let level = ppr.and_then(|a| num(a, "lvl")).unwrap_or(0.0) as u32;
            let bullet = ppr.and_then(bullet_of);
            let mut runs = Vec::new();
            for r in p.children() {
                match r.tag_name().name() {
                    "r" | "fld" => {
                        let rpr = child(r, "rPr");
                        runs.push(Run {
                            text: child(r, "t").and_then(|t| t.text()).unwrap_or_default().to_string(),
                            size_pt: rpr.and_then(|a| num(a, "sz")).map(|s| s / 100.0),
                            bold: rpr.and_then(|a| a.attribute("b")).is_some_and(|b| b == "1" || b == "true"),
                            color: rpr.and_then(|a| child(a, "solidFill")).and_then(|f| color_of(f, theme)).map(|c| [c[0], c[1], c[2]]),
                        });
                    }
                    "br" => runs.push(Run { text: "\n".into(), size_pt: None, bold: false, color: None }),
                    _ => {}
                }
            }
            Para { runs, align, level, bullet }
        })
        .collect();
    let lst = list_style(child(tx, "lstStyle"), theme);
    TextBody { paras, lst, anchor, font_scale, insets }
}

fn elements(tree: Node, space: Space, theme: &Theme, rels: &HashMap<String, (String, String)>, out: &mut Vec<Element>) {
    for n in tree.children().filter(|n| n.is_element()) {
        match n.tag_name().name() {
            "sp" => {
                let sp_pr = child(n, "spPr");
                out.push(Element::Shape {
                    rect: xfrm(sp_pr).map(|r| space.map(r)),
                    fill: solid_fill(sp_pr, theme),
                    text: child(n, "txBody").map(|t| text_body(t, theme)),
                    ph: placeholder(child(n, "nvSpPr")),
                });
            }
            "pic" => {
                let embed = path(n, &["blipFill", "blip"]).and_then(|b| attr_local(b, "embed"));
                if let Some((_, media)) = embed.and_then(|id| rels.get(id)) {
                    out.push(Element::Picture { rect: xfrm(child(n, "spPr")).map(|r| space.map(r)), media: media.clone() });
                }
            }
            "grpSp" => {
                let gx = child(n, "grpSpPr").and_then(|g| child(g, "xfrm"));
                let inner = gx
                    .and_then(|x| {
                        let off = child(x, "off")?;
                        let ext = child(x, "ext")?;
                        let ch_off = child(x, "chOff")?;
                        let ch_ext = child(x, "chExt")?;
                        let sx = num(ext, "cx")? / num(ch_ext, "cx")?.max(1.0);
                        let sy = num(ext, "cy")? / num(ch_ext, "cy")?.max(1.0);
                        let local = Space { sx, sy, ox: num(off, "x")? - num(ch_off, "x")? * sx, oy: num(off, "y")? - num(ch_off, "y")? * sy };
                        Some(Space { sx: space.sx * local.sx, sy: space.sy * local.sy, ox: space.ox + local.ox * space.sx, oy: space.oy + local.oy * space.sy })
                    })
                    .unwrap_or(space);
                elements(n, inner, theme, rels, out);
            }
            _ => {}
        }
    }
}

/// A slide, layout or master part, parsed.
struct Part {
    elements: Vec<Element>,
    background: Option<Background>,
    rels: HashMap<String, (String, String)>,
    /// Masters only.
    styles: Option<MasterStyles>,
}

#[derive(Clone)]
enum Background {
    Color([u8; 4]),
    Picture(String),
}

fn parse_part(pkg: &mut Package, name: &str, theme: &Theme) -> Option<Part> {
    let xml = pkg.text(name)?;
    let doc = Document::parse(&xml).ok()?;
    let rels = pkg.rels(name);
    let c_sld = doc.descendants().find(|n| n.tag_name().name() == "cSld")?;
    let background = child(c_sld, "bg").and_then(|bg| {
        if let Some(pr) = child(bg, "bgPr") {
            if let Some(f) = child(pr, "solidFill") {
                return color_of(f, theme).map(Background::Color);
            }
            if let Some(id) = path(pr, &["blipFill", "blip"]).and_then(|b| attr_local(b, "embed")) {
                return rels.get(id).map(|(_, m)| Background::Picture(m.clone()));
            }
            return None;
        }
        // Theme background reference: use the colour it carries.
        child(bg, "bgRef").and_then(|r| color_of(r, theme)).map(Background::Color)
    });
    let mut elements_out = Vec::new();
    if let Some(tree) = child(c_sld, "spTree") {
        elements(tree, Space::IDENTITY, theme, &rels, &mut elements_out);
    }
    let styles = doc.descendants().find(|n| n.tag_name().name() == "txStyles").map(|t| MasterStyles {
        title: list_style(child(t, "titleStyle"), theme),
        body: list_style(child(t, "bodyStyle"), theme),
        other: list_style(child(t, "otherStyle"), theme),
    });
    Some(Part { elements: elements_out, background, rels, styles })
}

fn parse_theme(pkg: &mut Package, name: &str) -> Theme {
    let mut theme = Theme::new();
    let Some(xml) = pkg.text(name) else { return theme };
    let Ok(doc) = Document::parse(&xml) else { return theme };
    if let Some(scheme) = doc.descendants().find(|n| n.tag_name().name() == "clrScheme") {
        for entry in scheme.children().filter(|n| n.is_element()) {
            if let Some(c) = color_of(entry, &Theme::new()) {
                theme.insert(entry.tag_name().name().to_string(), [c[0], c[1], c[2]]);
            }
        }
    }
    theme
}

/// A slide with its inheritance chain resolved enough to draw.
struct SlideModel {
    elements: Vec<Element>,
    background: Background,
    /// Layout and master placeholders, for shapes that inherit position and style.
    inherited: Vec<InheritedPh>,
    styles: MasterStyles,
    notes: String,
}

struct Deck {
    size: (i64, i64),
    theme: Theme,
    slides: Vec<SlideModel>,
}

fn load(path: &Path) -> VideoResult<(Package, Deck)> {
    let mut pkg = Package::open(path)?;
    let pres_xml = pkg.text("ppt/presentation.xml").ok_or_else(|| bad(path, "not a PowerPoint file (no ppt/presentation.xml)"))?;
    let pres = Document::parse(&pres_xml).map_err(|e| bad(path, e))?;
    let size = pres
        .descendants()
        .find(|n| n.tag_name().name() == "sldSz")
        .and_then(|n| Some((num(n, "cx")? as i64, num(n, "cy")? as i64)))
        .unwrap_or(DEFAULT_SIZE);
    let pres_rels = pkg.rels("ppt/presentation.xml");
    let order: Vec<String> = pres
        .descendants()
        .filter(|n| n.tag_name().name() == "sldId")
        // `r:id` (namespaced), not the numeric `id`.
        .filter_map(|n| n.attributes().find(|a| a.name() == "id" && a.namespace().is_some()).map(|a| a.value()))
        .filter_map(|rid| pres_rels.get(rid).map(|(_, p)| p.clone()))
        .collect();
    let theme_part = pres_rels.values().find(|(t, _)| t == "theme").map(|(_, p)| p.clone()).unwrap_or_else(|| "ppt/theme/theme1.xml".into());
    let theme = parse_theme(&mut pkg, &theme_part);

    type Cached = (Vec<InheritedPh>, Option<Background>, Option<String>, Option<MasterStyles>);
    let mut parts_cache: HashMap<String, Option<Cached>> = HashMap::new();
    let mut slides = Vec::new();
    for slide_part in order {
        let Some(slide) = parse_part(&mut pkg, &slide_part, &theme) else { continue };
        let layout = slide.rels.values().find(|(t, _)| t == "slideLayout").map(|(_, p)| p.clone());
        let notes_part = slide.rels.values().find(|(t, _)| t == "notesSlide").map(|(_, p)| p.clone());
        // Layout then master: placeholder positions and the background.
        let mut inherited = Vec::new();
        let mut styles = MasterStyles::default();
        let mut background = slide.background.clone();
        let mut next = layout;
        for depth in 0..2u8 {
            let Some(name) = next.take() else { break };
            let entry = parts_cache
                .entry(name.clone())
                .or_insert_with(|| {
                    let part = parse_part(&mut pkg, &name, &theme)?;
                    let phs = part
                        .elements
                        .iter()
                        .filter_map(|e| match e {
                            Element::Shape { rect, ph: Some(p), text, .. } => Some(InheritedPh {
                                depth: 0,
                                ph: p.clone(),
                                rect: *rect,
                                anchor: text.as_ref().and_then(|t| t.anchor),
                                lst: text.as_ref().map(|t| t.lst.clone()).unwrap_or_default(),
                            }),
                            _ => None,
                        })
                        .collect();
                    let parent = part.rels.values().find(|(t, _)| t == "slideMaster").map(|(_, p)| p.clone());
                    Some((phs, part.background.clone(), parent, part.styles.clone()))
                })
                .clone();
            if let Some((phs, bg, parent, part_styles)) = entry {
                inherited.extend(phs.into_iter().map(|p| InheritedPh { depth, ..p }));
                if let Some(st) = part_styles {
                    styles = st;
                }
                if background.is_none() {
                    background = bg;
                }
                next = parent;
            }
        }
        let notes = notes_part.map(|n| notes_text(&mut pkg, &n)).unwrap_or_default();
        slides.push(SlideModel {
            elements: slide.elements,
            background: background.unwrap_or(Background::Color(theme.get("lt1").map_or([255; 4], |c| [c[0], c[1], c[2], 255]))),
            inherited,
            styles,
            notes,
        });
    }
    if slides.is_empty() {
        return Err(bad(path, "presentation has no slides"));
    }
    Ok((pkg, Deck { size, theme, slides }))
}

fn notes_text(pkg: &mut Package, part: &str) -> String {
    let Some(xml) = pkg.text(part) else { return String::new() };
    let Ok(doc) = Document::parse(&xml) else { return String::new() };
    // The notes body is the "body" placeholder (the other one is the slide image).
    doc.descendants()
        .filter(|n| n.tag_name().name() == "sp")
        .filter(|sp| placeholder(child(*sp, "nvSpPr")).is_some_and(|p| p.kind == "body"))
        .filter_map(|sp| child(sp, "txBody"))
        .map(|tx| paragraphs_text(&text_body(tx, &Theme::new())))
        .collect::<Vec<_>>()
        .join("\n")
}

fn paragraphs_text(body: &TextBody) -> String {
    body.paras
        .iter()
        .map(|p| p.runs.iter().map(|r| r.text.as_str()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn slide_text(s: &SlideModel) -> String {
    // Titles first, then everything else in drawing order.
    let mut titled: Vec<(bool, String)> = s
        .elements
        .iter()
        .filter_map(|e| match e {
            Element::Shape { text: Some(t), ph, .. } => {
                let is_title = ph.as_ref().is_some_and(|p| p.kind == "title" || p.kind == "ctrTitle");
                Some((!is_title, paragraphs_text(t)))
            }
            _ => None,
        })
        .collect();
    titled.sort_by_key(|(rest, _)| *rest);
    super::pdf::clean_text(&titled.into_iter().map(|(_, t)| t).collect::<Vec<_>>().join("\n"))
}

/// Slide text and speaker notes only (used when another renderer draws the pictures).
pub fn extract_text(path: &Path) -> VideoResult<Vec<(String, String)>> {
    let (_, deck) = load(path)?;
    Ok(deck.slides.iter().map(|s| (slide_text(s), super::pdf::clean_text(&s.notes))).collect())
}

// -------------------------------------------------------------- rendering --

fn default_size_pt(ph: Option<&Placeholder>) -> f64 {
    match ph.map(|p| p.kind.as_str()) {
        Some("ctrTitle") => 54.0,
        Some("title") => 40.0,
        Some("subTitle") => 28.0,
        Some("body" | "obj") => 24.0,
        Some("ftr" | "dt" | "sldNum") => 12.0,
        _ => 18.0,
    }
}

fn norm_kind(k: &str) -> &str {
    match k {
        "ctrTitle" => "title",
        "obj" | "subTitle" => "body",
        other => other,
    }
}

fn kind_matches(slide: &str, inherited: &str) -> bool {
    slide == inherited || norm_kind(slide) == norm_kind(inherited)
}

/// The layout and master placeholders a slide placeholder inherits from,
/// nearest first: per level, by `idx` when possible, else by type.
fn inheritance<'a>(ph: &Placeholder, inherited: &'a [InheritedPh]) -> Vec<&'a InheritedPh> {
    (0..2u8)
        .filter_map(|depth| {
            let at: Vec<&InheritedPh> = inherited.iter().filter(|i| i.depth == depth).collect();
            let by_idx = ph.idx.as_ref().and_then(|idx| at.iter().find(|i| i.ph.idx.as_ref() == Some(idx) && kind_matches(&ph.kind, &i.ph.kind)));
            let by_exact = || at.iter().find(|i| i.ph.kind == ph.kind);
            let by_kind = || at.iter().find(|i| kind_matches(&ph.kind, &i.ph.kind));
            by_idx.or_else(by_exact).or_else(by_kind).copied()
        })
        .collect()
}

/// Conventional placement when nothing in the chain gives geometry.
fn fallback_rect(ph: &Placeholder, size: (i64, i64)) -> Rect {
    let (w, h) = (size.0 as f64, size.1 as f64);
    match ph.kind.as_str() {
        "title" | "ctrTitle" => Rect { x: w * 0.06, y: h * 0.05, w: w * 0.88, h: h * 0.18 },
        "subTitle" => Rect { x: w * 0.12, y: h * 0.55, w: w * 0.76, h: h * 0.25 },
        _ => Rect { x: w * 0.06, y: h * 0.26, w: w * 0.88, h: h * 0.66 },
    }
}

fn luminance(c: [u8; 4]) -> f64 {
    (0.2126 * c[0] as f64 + 0.7152 * c[1] as f64 + 0.0722 * c[2] as f64) / 255.0
}

fn blend(img: &mut RgbaImage, x: i64, y: i64, c: [u8; 4], coverage: f32) {
    if x < 0 || y < 0 || x >= img.width() as i64 || y >= img.height() as i64 {
        return;
    }
    let a = (c[3] as f32 / 255.0) * coverage.clamp(0.0, 1.0);
    if a <= 0.0 {
        return;
    }
    let p = img.get_pixel_mut(x as u32, y as u32);
    for (dst, src) in p.0.iter_mut().zip(c).take(3) {
        *dst = (*dst as f32 * (1.0 - a) + src as f32 * a).round() as u8;
    }
    p.0[3] = 255;
}

fn fill_rect(img: &mut RgbaImage, r: Rect, c: [u8; 4]) {
    let (x0, y0) = (r.x.round().max(0.0) as i64, r.y.round().max(0.0) as i64);
    let (x1, y1) = ((r.x + r.w).round().min(img.width() as f64) as i64, (r.y + r.h).round().min(img.height() as f64) as i64);
    for y in y0..y1 {
        for x in x0..x1 {
            blend(img, x, y, c, 1.0);
        }
    }
}

fn draw_picture(img: &mut RgbaImage, pkg: &mut Package, media: &str, r: Rect) {
    let Some(bytes) = pkg.bytes(media) else { return };
    let Ok(pic) = image::load_from_memory(&bytes) else {
        tracing::debug!(media, "pptx picture format not supported; skipped");
        return;
    };
    let (w, h) = (r.w.round().max(1.0) as u32, r.h.round().max(1.0) as u32);
    if w > 16_384 || h > 16_384 {
        return;
    }
    let scaled = image::imageops::resize(&pic.to_rgba8(), w, h, image::imageops::FilterType::Triangle);
    let (ox, oy) = (r.x.round() as i64, r.y.round() as i64);
    for (x, y, p) in scaled.enumerate_pixels() {
        blend(img, ox + x as i64, oy + y as i64, p.0, 1.0);
    }
}

struct Fonts<'a> {
    regular: FontRef<'a>,
}

/// One laid-out glyph run segment.
struct Word {
    text: String,
    px: f32,
    bold: bool,
    color: [u8; 4],
    width: f32,
}

fn measure(font: &FontRef, text: &str, px: f32) -> f32 {
    let f = font.as_scaled(PxScale::from(px));
    let mut w = 0.0;
    let mut prev = None;
    for ch in text.chars() {
        let id = f.glyph_id(ch);
        if let Some(p) = prev {
            w += f.kern(p, id);
        }
        w += f.h_advance(id);
        prev = Some(id);
    }
    w
}

fn draw_run(img: &mut RgbaImage, font: &FontRef, word: &Word, x: f32, baseline: f32) {
    let f = font.as_scaled(PxScale::from(word.px));
    let mut pen = x;
    let mut prev = None;
    // Faux bold: the bundled UI font has one weight, so smear it horizontally.
    let passes: &[f32] = if word.bold { &[0.0, (word.px / 28.0).max(0.6)] } else { &[0.0] };
    for ch in word.text.chars() {
        let id = f.glyph_id(ch);
        if let Some(p) = prev {
            pen += f.kern(p, id);
        }
        for dx in passes {
            let glyph = id.with_scale_and_position(PxScale::from(word.px), ab_glyph::point(pen + dx, baseline));
            if let Some(outline) = font.outline_glyph(glyph) {
                let b = outline.px_bounds();
                outline.draw(|gx, gy, cov| blend(img, b.min.x as i64 + gx as i64, b.min.y as i64 + gy as i64, word.color, cov));
            }
        }
        pen += f.h_advance(id);
        prev = Some(id);
    }
}

/// Lays out and draws a text body into `r` (pixels). `px_per_pt` converts sizes.
/// How paragraphs are styled when the run itself does not say.
struct TextDefaults<'a> {
    /// Inherited level style (layout → master → master text styles).
    style: &'a dyn Fn(u32) -> LevelStyle,
    size_pt: f64,
    color: [u8; 4],
    anchor: Option<Anchor>,
    /// Background luminance: inherited colours that would vanish against it are replaced.
    bg_luminance: f64,
}

fn draw_text(img: &mut RgbaImage, fonts: &Fonts, body: &TextBody, r: Rect, px_per_emu: f64, d: &TextDefaults) {
    let px_per_pt = px_per_emu * EMU_PER_PT;
    let inner = Rect {
        x: r.x + body.insets[0] * px_per_emu,
        y: r.y + body.insets[1] * px_per_emu,
        w: (r.w - (body.insets[0] + body.insets[2]) * px_per_emu).max(1.0),
        h: (r.h - (body.insets[1] + body.insets[3]) * px_per_emu).max(1.0),
    };
    let font = &fonts.regular;
    // Lines: (words, line height, alignment, indent).
    let mut lines: Vec<(Vec<Word>, f32, Align, f32)> = Vec::new();
    for p in &body.paras {
        let ls = level(&body.lst, p.level).or(&(d.style)(p.level));
        let default_pt = ls.size_pt.unwrap_or(d.size_pt);
        let default_color = match ls.color {
            Some(c) if (luminance([c[0], c[1], c[2], 255]) - d.bg_luminance).abs() >= 0.2 => [c[0], c[1], c[2], 255],
            _ => d.color,
        };
        let align = p.align.or(ls.align).unwrap_or(Align::Left);
        let indent = (p.level as f64 * 0.4 * 72.0 * px_per_pt) as f32;
        let first_size = p.runs.iter().find_map(|r| r.size_pt).unwrap_or(default_pt);
        let mut words: Vec<Word> = Vec::new();
        let bullet = p.bullet.or(ls.bullet).unwrap_or(false) && p.runs.iter().any(|r| !r.text.trim().is_empty());
        if bullet {
            let px = (first_size * body.font_scale * px_per_pt) as f32;
            words.push(Word { text: "• ".into(), px, bold: false, color: default_color, width: measure(font, "• ", px) });
        }
        for run in &p.runs {
            let px = (run.size_pt.unwrap_or(default_pt) * body.font_scale * px_per_pt) as f32;
            let color = run.color.map_or(default_color, |c| [c[0], c[1], c[2], 255]);
            if run.text == "\n" {
                words.push(Word { text: "\n".into(), px, bold: false, color, width: 0.0 });
                continue;
            }
            // Split into words, keeping the trailing space with each word.
            let mut cur = String::new();
            for ch in run.text.chars() {
                cur.push(ch);
                if ch == ' ' {
                    let w = measure(font, &cur, px);
                    words.push(Word { text: std::mem::take(&mut cur), px, bold: run.bold, color, width: w });
                }
            }
            if !cur.is_empty() {
                let w = measure(font, &cur, px);
                words.push(Word { text: cur, px, bold: run.bold, color, width: w });
            }
        }
        let empty_px = (first_size * body.font_scale * px_per_pt) as f32;
        if words.is_empty() {
            lines.push((Vec::new(), empty_px * 1.2, align, indent));
            continue;
        }
        let max_w = inner.w as f32 - indent;
        let mut line: Vec<Word> = Vec::new();
        let mut line_w = 0.0;
        for w in words {
            let forced = w.text == "\n";
            if forced || (!line.is_empty() && line_w + w.width.min(measure(font, w.text.trim_end(), w.px)) > max_w) {
                let h = line.iter().map(|w| w.px).fold(empty_px, f32::max) * 1.2;
                lines.push((std::mem::take(&mut line), h, align, indent));
                line_w = 0.0;
                if forced {
                    continue;
                }
            }
            line_w += w.width;
            line.push(w);
        }
        let h = line.iter().map(|w| w.px).fold(empty_px, f32::max) * 1.2;
        lines.push((line, h, align, indent));
    }
    let total: f32 = lines.iter().map(|l| l.1).sum();
    let mut y = match body.anchor.or(d.anchor).unwrap_or(Anchor::Top) {
        Anchor::Top => inner.y as f32,
        Anchor::Middle => inner.y as f32 + (inner.h as f32 - total) / 2.0,
        Anchor::Bottom => inner.y as f32 + inner.h as f32 - total,
    };
    for (words, h, align, indent) in lines {
        let width: f32 = words.iter().map(|w| w.width).sum::<f32>()
            - words.last().map_or(0.0, |w| w.width - measure(font, w.text.trim_end(), w.px));
        let x0 = match align {
            Align::Left => inner.x as f32 + indent,
            Align::Center => inner.x as f32 + indent + (inner.w as f32 - indent - width) / 2.0,
            Align::Right => (inner.x + inner.w) as f32 - width,
        };
        let ascent = words.iter().map(|w| font.as_scaled(PxScale::from(w.px)).ascent()).fold(0.0, f32::max);
        let baseline = y + (h - h / 1.2) / 2.0 + ascent;
        let mut x = x0;
        for w in &words {
            draw_run(img, font, w, x, baseline);
            x += w.width;
        }
        y += h;
    }
}

fn render_slide(pkg: &mut Package, deck: &Deck, slide: &SlideModel, target: (u32, u32), fonts: &Fonts) -> RgbaImage {
    let (cx, cy) = (deck.size.0 as f64, deck.size.1 as f64);
    let s = (target.0 as f64 / cx).min(target.1 as f64 / cy);
    let (w, h) = ((cx * s).round().max(1.0) as u32, (cy * s).round().max(1.0) as u32);
    let to_px = |r: Rect| Rect { x: r.x * s, y: r.y * s, w: r.w * s, h: r.h * s };
    let mut img = RgbaImage::from_pixel(w, h, Rgba([255, 255, 255, 255]));
    let bg = match &slide.background {
        Background::Color(c) => {
            fill_rect(&mut img, Rect { x: 0.0, y: 0.0, w: w as f64, h: h as f64 }, [c[0], c[1], c[2], 255]);
            *c
        }
        Background::Picture(media) => {
            draw_picture(&mut img, pkg, media, Rect { x: 0.0, y: 0.0, w: w as f64, h: h as f64 });
            [0, 0, 0, 255]
        }
    };
    // Dark backgrounds get light default text (as the theme's own pairing would).
    let dk = deck.theme.get("dk1").copied().unwrap_or([0, 0, 0]);
    let lt = deck.theme.get("lt1").copied().unwrap_or([255, 255, 255]);
    let default_color = if luminance(bg) < 0.45 { [lt[0], lt[1], lt[2], 255] } else { [dk[0], dk[1], dk[2], 255] };
    for e in &slide.elements {
        match e {
            Element::Shape { rect, fill, text, ph } => {
                let chain = ph.as_ref().map(|p| inheritance(p, &slide.inherited)).unwrap_or_default();
                let r = match (rect, ph) {
                    (Some(r), _) => *r,
                    (None, Some(p)) => chain.iter().find_map(|i| i.rect).unwrap_or_else(|| fallback_rect(p, deck.size)),
                    (None, None) => continue,
                };
                let r = to_px(r);
                if let Some(c) = fill {
                    fill_rect(&mut img, r, *c);
                }
                if let Some(t) = text {
                    let color = match fill {
                        Some(c) if luminance(*c) < 0.45 => [lt[0], lt[1], lt[2], 255],
                        Some(_) => [dk[0], dk[1], dk[2], 255],
                        None => default_color,
                    };
                    let master = match ph.as_ref().map(|p| p.kind.as_str()) {
                        Some("title" | "ctrTitle") => Some(&slide.styles.title),
                        Some("body" | "obj" | "subTitle") => Some(&slide.styles.body),
                        Some(_) => Some(&slide.styles.other),
                        None => None,
                    };
                    let is_list = ph.as_ref().is_some_and(|p| p.kind == "body" || p.kind == "obj");
                    let style = |lvl: u32| {
                        let mut ls = LevelStyle::default();
                        for i in &chain {
                            ls = ls.or(&level(&i.lst, lvl));
                        }
                        if let Some(m) = master {
                            ls = ls.or(&level(m, lvl));
                        }
                        // Content placeholders are bulleted unless a style says otherwise.
                        if ls.bullet.is_none() && is_list {
                            ls.bullet = Some(true);
                        }
                        ls
                    };
                    let defaults = TextDefaults {
                        style: &style,
                        size_pt: default_size_pt(ph.as_ref()),
                        color,
                        anchor: chain.iter().find_map(|i| i.anchor),
                        bg_luminance: fill.map_or(luminance(bg), luminance),
                    };
                    draw_text(&mut img, fonts, t, r, s, &defaults);
                }
            }
            Element::Picture { rect: Some(r), media } => draw_picture(&mut img, pkg, media, to_px(*r)),
            Element::Picture { rect: None, .. } => {}
        }
    }
    img
}

/// Renders every slide contain-fitted into `target`, streaming them to `each`.
pub fn render_slides(path: &Path, target: (u32, u32), each: &mut dyn FnMut(usize, usize, RenderedSlide) -> VideoResult<()>) -> VideoResult<()> {
    let (mut pkg, deck) = load(path)?;
    let fonts = Fonts { regular: FontRef::try_from_slice(epaint_default_fonts::UBUNTU_LIGHT).map_err(|e| bad(path, e))? };
    let total = deck.slides.len();
    for (i, slide) in deck.slides.iter().enumerate() {
        let img = render_slide(&mut pkg, &deck, slide, target, &fonts);
        let (width, height) = img.dimensions();
        each(i, total, RenderedSlide { width, height, rgba: img.into_raw(), text: slide_text(slide), notes: super::pdf::clean_text(&slide.notes) })?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    /// Builds a small two-slide deck: a red block with a title, and a
    /// picture on a dark background with bullet text and speaker notes.
    pub(crate) fn sample_pptx(path: &Path) {
        let f = std::fs::File::create(path).unwrap();
        let mut z = zip::ZipWriter::new(f);
        let opts: zip::write::SimpleFileOptions = Default::default();
        let mut put = |name: &str, body: &[u8]| {
            z.start_file(name, opts).unwrap();
            z.write_all(body).unwrap();
        };
        const NS: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;
        put(
            "ppt/presentation.xml",
            format!(r#"<p:presentation {NS}><p:sldIdLst><p:sldId id="256" r:id="rId2"/><p:sldId id="257" r:id="rId3"/></p:sldIdLst><p:sldSz cx="12192000" cy="6858000"/></p:presentation>"#).as_bytes(),
        );
        put(
            "ppt/_rels/presentation.xml.rels",
            br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="theme/theme1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide2.xml"/></Relationships>"#,
        );
        put(
            "ppt/theme/theme1.xml",
            br#"<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:themeElements><a:clrScheme name="t"><a:dk1><a:srgbClr val="111111"/></a:dk1><a:lt1><a:srgbClr val="FFFFFF"/></a:lt1><a:accent1><a:srgbClr val="00C853"/></a:accent1></a:clrScheme></a:themeElements></a:theme>"#,
        );
        put(
            "ppt/slides/slide1.xml",
            format!(
                r#"<p:sld {NS}><p:cSld><p:spTree>
                <p:sp><p:nvSpPr><p:cNvPr id="2" name="Block"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>
                  <p:spPr><a:xfrm><a:off x="0" y="3429000"/><a:ext cx="6096000" cy="3429000"/></a:xfrm><a:solidFill><a:srgbClr val="FF0000"/></a:solidFill></p:spPr></p:sp>
                <p:sp><p:nvSpPr><p:cNvPr id="3" name="Title"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr>
                  <p:spPr><a:xfrm><a:off x="609600" y="304800"/><a:ext cx="10972800" cy="1143000"/></a:xfrm></p:spPr>
                  <p:txBody><a:bodyPr anchor="ctr"/><a:p><a:pPr algn="ctr"/><a:r><a:rPr sz="4400" b="1"/><a:t>Welcome to the Show</a:t></a:r></a:p></p:txBody></p:sp>
                <p:sp><p:nvSpPr><p:cNvPr id="4" name="Accent"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>
                  <p:spPr><a:xfrm><a:off x="6096000" y="3429000"/><a:ext cx="6096000" cy="3429000"/></a:xfrm><a:solidFill><a:schemeClr val="accent1"/></a:solidFill></p:spPr></p:sp>
                </p:spTree></p:cSld></p:sld>"#
            )
            .as_bytes(),
        );
        put(
            "ppt/slides/slide2.xml",
            format!(
                r#"<p:sld {NS}><p:cSld><p:bg><p:bgPr><a:solidFill><a:srgbClr val="000000"/></a:solidFill></p:bgPr></p:bg><p:spTree>
                <p:pic><p:nvPicPr><p:cNvPr id="5" name="Photo"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr>
                  <p:blipFill><a:blip r:embed="rId9"/></p:blipFill>
                  <p:spPr><a:xfrm><a:off x="6096000" y="0"/><a:ext cx="6096000" cy="6858000"/></a:xfrm></p:spPr></p:pic>
                <p:sp><p:nvSpPr><p:cNvPr id="6" name="Body"/><p:cNvSpPr/><p:nvPr><p:ph idx="1"/></p:nvPr></p:nvSpPr>
                  <p:spPr><a:xfrm><a:off x="304800" y="304800"/><a:ext cx="5486400" cy="6096000"/></a:xfrm></p:spPr>
                  <p:txBody><a:bodyPr/><a:p><a:r><a:t>First point</a:t></a:r></a:p><a:p><a:r><a:t>Second point</a:t></a:r></a:p></p:txBody></p:sp>
                </p:spTree></p:cSld></p:sld>"#
            )
            .as_bytes(),
        );
        put(
            "ppt/slides/_rels/slide2.xml.rels",
            br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/><Relationship Id="rId10" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesSlide" Target="../notesSlides/notesSlide2.xml"/></Relationships>"#,
        );
        put(
            "ppt/notesSlides/notesSlide2.xml",
            format!(r#"<p:notes {NS}><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="Notes"/><p:cNvSpPr/><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:txBody><a:bodyPr/><a:p><a:r><a:t>Cue the band after this slide.</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:notes>"#).as_bytes(),
        );
        let mut png = Vec::new();
        RgbaImage::from_pixel(8, 8, Rgba([0, 0, 255, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        put("ppt/media/image1.png", &png);
        z.finish().unwrap();
    }

    #[test]
    fn renders_shapes_text_pictures_and_notes() {
        let dir = std::env::temp_dir().join(format!("livestage-pptx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("deck.pptx");
        sample_pptx(&file);
        let mut slides = Vec::new();
        render_slides(&file, (960, 540), &mut |_, total, s| {
            assert_eq!(total, 2);
            slides.push(s);
            Ok(())
        })
        .unwrap();
        let px = |s: &RenderedSlide, x: u32, y: u32| {
            let i = ((y * s.width + x) * 4) as usize;
            [s.rgba[i], s.rgba[i + 1], s.rgba[i + 2]]
        };
        let one = &slides[0];
        assert_eq!((one.width, one.height), (960, 540));
        assert_eq!(px(one, 100, 400), [255, 0, 0], "solid red block");
        assert_eq!(px(one, 800, 400), [0, 0xC8, 0x53], "theme accent colour");
        assert_eq!(px(one, 20, 200), [255, 255, 255], "white background");
        // Title text drawn in the title box (dark ink on white).
        let title_ink = (40..120).flat_map(|y| (100..860).map(move |x| (x, y))).filter(|&(x, y)| px(one, x, y)[0] < 128).count();
        assert!(title_ink > 500, "title text missing ({title_ink} dark pixels)");
        assert_eq!(one.text, "Welcome to the Show");

        let two = &slides[1];
        assert_eq!(px(two, 700, 270), [0, 0, 255], "picture placed");
        // Light text on the dark background.
        let ink = (20..200).flat_map(|y| (20..470).map(move |x| (x, y))).filter(|&(x, y)| px(two, x, y)[0] > 128).count();
        assert!(ink > 200, "body text missing ({ink} light pixels)");
        assert_eq!(two.text, "First point\nSecond point");
        assert_eq!(two.notes, "Cue the band after this slide.");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

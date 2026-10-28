//! Builds `Feature Tour.livestage`: a demo show that touches every LiveStage
//! feature, with generated, licence-free media (tones, a pad loop, a title
//! card, a lower third, a PDF deck, an image sequence and — with the
//! `ffmpeg` feature — a countdown movie).
//!
//! ```text
//! cargo run --release -p livestage_video --features ffmpeg --example feature_tour -- [parent dir] [--force]
//! ```
//!
//! The show is written to `<parent dir>/Feature Tour.livestage` (default
//! `shows/`). `--force` replaces an existing one. See `docs/FEATURE_TOUR.md`
//! for what to try.

use ab_glyph::{point, Font, FontRef, PxScale, ScaleFont};
use image::{Rgba, RgbaImage};
use livestage_core::action::{ClickTrack, ControlAction, DmxChannelValue, FixtureAttributeValue, OscArg, TextContent, TimerContent, TimerMode, VisualContent};
use livestage_core::cart::{CartPad, CueCart, PadMode, PadNote};
use livestage_core::cue::{AudioCue, ControlCue, DmxCue, EndBehavior, SlideCue, TimerCue, VideoCue};
use livestage_core::cuelist::CueList;
use livestage_core::dmx_config::{DmxTransport, PatchedFixture, PixelMapConfig, PixelOrder, PixelWiring, UniverseConfig};
use livestage_core::id::{AssetId, EffectId, FixtureId, OutputId, PaletteId, StemId};
use livestage_core::lighting::{Effect, EffectKind, LightValue, Palette, PaletteKind, PaletteRef, Submaster, SubmasterKind};
use livestage_core::multiview::{MultiviewLayout, MultiviewSource};
use livestage_core::project::{AssetKind, AssetRecord, AssetRef};
use livestage_core::projection::PolygonMask;
use livestage_core::timecode::slave::ListTimecode;
use livestage_core::timecode::TimecodeSource;
use livestage_core::{AnyCue, CueHeader, FrameRate, ProjectBundle, Rgb, ShowCommand, ShowTime, Timecode, TimelineCue, TimelineItem};
use std::f32::consts::TAU;
use std::fs;
use std::path::{Path, PathBuf};

const RATE: u32 = 48_000;
const BPM: f64 = 120.0;
/// Song length: 16 bars of 4/4 at 120 BPM.
const SONG_SECS: f32 = 32.0;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let force = args.iter().any(|a| a == "--force");
    let parent = PathBuf::from(args.iter().find(|a| !a.starts_with("--")).map_or("shows", String::as_str));
    let target = parent.join("Feature Tour.livestage");
    if target.exists() {
        if !force {
            eprintln!("{} exists; pass --force to replace it", target.display());
            std::process::exit(1);
        }
        fs::remove_dir_all(&target).expect("removing the old show");
    }
    fs::create_dir_all(&parent).expect("parent folder");
    let staging = std::env::temp_dir().join(format!("livestage-feature-tour-{}", std::process::id()));
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging).expect("staging folder");

    println!("Generating media in {}", staging.display());
    let media = Media::generate(&staging);
    let mut bundle = ProjectBundle::create(&parent, "Feature Tour").expect("creating the bundle");
    let assets = Assets::import(&mut bundle, &media);
    build_show(&mut bundle, &assets);
    let issues = bundle.project.validate();
    bundle.save().expect("saving the show");
    let _ = fs::remove_dir_all(&staging);

    // Round trip: the show must open the way the app opens it.
    let reopened = ProjectBundle::open(&target).expect("reopening the show");
    let missing = reopened.missing_assets();
    let cues: usize = reopened.project.cue_lists.iter().map(|l| l.cues.len()).sum();
    println!("Wrote {}", target.display());
    println!(
        "  {} cue lists, {cues} cues, {} cart pads, {} fixtures, {} assets{}",
        reopened.project.cue_lists.len(),
        reopened.project.carts.iter().map(|c| c.pads.iter().flatten().count()).sum::<usize>(),
        reopened.project.dmx.patch.len(),
        reopened.project.assets.len(),
        if media.movie.is_some() { "" } else { " (no movie: build with --features ffmpeg)" }
    );
    for issue in &issues {
        println!("  validation: {issue}");
    }
    if !missing.is_empty() {
        eprintln!("  missing assets: {missing:?}");
        std::process::exit(1);
    }
}

// ------------------------------------------------------------------ media --

/// Files generated into the staging folder.
struct Media {
    backing: PathBuf,
    ambience: PathBuf,
    sting: PathBuf,
    applause: PathBuf,
    tone: PathBuf,
    title: PathBuf,
    lower_third: PathBuf,
    deck: PathBuf,
    sequence: PathBuf,
    movie: Option<PathBuf>,
}

impl Media {
    fn generate(dir: &Path) -> Self {
        let font = FontRef::try_from_slice(epaint_default_fonts::UBUNTU_LIGHT).expect("bundled font");
        let media = Media {
            backing: write_wav(&dir.join("Song 1 backing (120 BPM).wav"), &backing_track()),
            ambience: write_wav(&dir.join("Ambience pad (12 s loop).wav"), &ambience_pad()),
            sting: write_wav(&dir.join("Sting.wav"), &sting()),
            applause: write_wav(&dir.join("Applause.wav"), &applause()),
            tone: write_wav(&dir.join("Line check 1 kHz -20 dBFS.wav"), &tone_1k()),
            title: save_png(&dir.join("Title card.png"), &title_card(&font)),
            lower_third: save_png(&dir.join("Lower third.png"), &lower_third(&font)),
            deck: write_pdf(&dir.join("Feature Tour deck.pdf")),
            sequence: image_sequence(&dir.join("Spinner sequence"), &font),
            movie: movie::write(&dir.join("Countdown clip.mp4"), &font),
        };
        if media.movie.is_none() {
            println!("  (movie skipped: needs --features ffmpeg and an H.264 or MPEG-4 encoder)");
        }
        media
    }
}

/// Interleaved stereo f32 → 16-bit PCM WAV.
fn write_wav(path: &Path, stereo: &[f32]) -> PathBuf {
    let data: Vec<u8> = stereo.iter().flat_map(|s| ((s.clamp(-1.0, 1.0) * 32_767.0) as i16).to_le_bytes()).collect();
    let mut b = Vec::with_capacity(44 + data.len());
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&RATE.to_le_bytes());
    b.extend_from_slice(&(RATE * 4).to_le_bytes());
    b.extend_from_slice(&4u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(data.len() as u32).to_le_bytes());
    b.extend_from_slice(&data);
    fs::write(path, b).expect("writing wav");
    path.to_path_buf()
}

fn frames(secs: f32) -> usize {
    (secs * RATE as f32) as usize
}

fn midi_hz(note: f32) -> f32 {
    440.0 * 2f32.powf((note - 69.0) / 12.0)
}

/// Soft synth voice: sine plus two gentle harmonics.
fn voice(phase: f32) -> f32 {
    (phase * TAU).sin() + 0.3 * (phase * 2.0 * TAU).sin() + 0.12 * (phase * 3.0 * TAU).sin()
}

/// 32 s, 120 BPM: C – Am – F – G pad chords (two bars each), bass on the
/// beat and a soft kick, so the click and the lights have something to
/// line up with.
fn backing_track() -> Vec<f32> {
    let n = frames(SONG_SECS);
    let beat = 60.0 / BPM as f32;
    let chords: [[f32; 4]; 4] = [[48.0, 60.0, 64.0, 67.0], [45.0, 57.0, 60.0, 64.0], [41.0, 57.0, 60.0, 65.0], [43.0, 55.0, 59.0, 62.0]];
    let mut out = vec![0.0; n * 2];
    for i in 0..n {
        let t = i as f32 / RATE as f32;
        let bar = (t / (beat * 4.0)) as usize;
        let chord = chords[(bar / 2) % 4];
        let in_beat = t % beat;
        let mut s = 0.0;
        for (k, &note) in chord.iter().enumerate() {
            let hz = midi_hz(note);
            let level = if k == 0 { 0.0 } else { 0.07 };
            s += level * voice(hz * t);
        }
        // Bass: root, plucked on every beat.
        s += 0.16 * (-in_beat * 6.0).exp() * voice(midi_hz(chord[0]) * t);
        // Kick: falling sine on every beat.
        let kick_hz = 50.0 + 90.0 * (-in_beat * 30.0).exp();
        s += 0.35 * (-in_beat * 18.0).exp() * (kick_hz * in_beat * TAU).sin();
        // 10 ms fade at both ends: no clicks.
        let edge = (t / 0.01).min((SONG_SECS - t) / 0.01).clamp(0.0, 1.0);
        let pan = 0.15 * (t * 0.25 * TAU).sin();
        out[i * 2] = s * edge * (1.0 - pan);
        out[i * 2 + 1] = s * edge * (1.0 + pan);
    }
    out
}

/// 12 s A-minor pad. Every partial completes whole cycles in 12 s, so the
/// loop point is seamless.
fn ambience_pad() -> Vec<f32> {
    let n = frames(12.0);
    let partials = [110.0, 165.0, 220.0, 261.75, 330.0];
    let mut out = vec![0.0; n * 2];
    for i in 0..n {
        let t = i as f32 / RATE as f32;
        let swell = 0.75 + 0.25 * (t / 6.0 * TAU).sin();
        let l: f32 = partials.iter().enumerate().map(|(k, hz)| (hz * t * TAU + k as f32).sin() / (k + 2) as f32).sum();
        let r: f32 = partials.iter().enumerate().map(|(k, hz)| (hz * t * TAU + 1.3 * k as f32).sin() / (k + 2) as f32).sum();
        out[i * 2] = 0.16 * swell * l;
        out[i * 2 + 1] = 0.16 * swell * r;
    }
    out
}

/// A rising four-note chime.
fn sting() -> Vec<f32> {
    let n = frames(2.5);
    let notes = [72.0, 76.0, 79.0, 84.0];
    let mut out = vec![0.0; n * 2];
    for i in 0..n {
        let t = i as f32 / RATE as f32;
        let mut s = 0.0;
        for (k, &note) in notes.iter().enumerate() {
            let start = k as f32 * 0.12;
            if t >= start {
                let dt = t - start;
                s += 0.2 * (-dt * 2.5).exp() * voice(midi_hz(note) * dt);
            }
        }
        let fade = ((2.5 - t) / 0.05).clamp(0.0, 1.0);
        out[i * 2] = s * fade;
        out[i * 2 + 1] = s * fade;
    }
    out
}

/// Crowd-like noise: filtered random claps with a swell and a fade.
fn applause() -> Vec<f32> {
    let n = frames(4.0);
    let mut seed = 0x2545_f491_u32;
    let mut rand = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as f32 / u32::MAX as f32
    };
    let mut out = vec![0.0; n * 2];
    let (mut lp_l, mut lp_r) = (0.0, 0.0);
    let mut claps = vec![0.0f32; n];
    for _ in 0..1600 {
        let at = (rand() * (n - 2000) as f32) as usize;
        let level = 0.4 + 0.6 * rand();
        for (j, c) in claps[at..at + 2000].iter_mut().enumerate() {
            *c += level * (-(j as f32) / 300.0).exp();
        }
    }
    for i in 0..n {
        let t = i as f32 / RATE as f32;
        let env = (t / 0.4).min(1.0) * ((4.0 - t) / 1.5).clamp(0.0, 1.0);
        lp_l += 0.35 * ((rand() - 0.5) * claps[i] - lp_l);
        lp_r += 0.35 * ((rand() - 0.5) * claps[i] - lp_r);
        out[i * 2] = 0.6 * env * lp_l;
        out[i * 2 + 1] = 0.6 * env * lp_r;
    }
    out
}

/// 5 s of 1 kHz at −20 dBFS.
fn tone_1k() -> Vec<f32> {
    let n = frames(5.0);
    let amp = 10f32.powf(-20.0 / 20.0);
    (0..n)
        .flat_map(|i| {
            let t = i as f32 / RATE as f32;
            let edge = (t / 0.005).min((5.0 - t) / 0.005).clamp(0.0, 1.0);
            let s = amp * edge * (1000.0 * t * TAU).sin();
            [s, s]
        })
        .collect()
}

// ----------------------------------------------------------------- images --

fn save_png(path: &Path, img: &RgbaImage) -> PathBuf {
    img.save(path).expect("writing png");
    path.to_path_buf()
}

fn text_width(font: &FontRef, text: &str, px: f32) -> f32 {
    let f = font.as_scaled(PxScale::from(px));
    text.chars().map(|c| f.h_advance(f.glyph_id(c))).sum()
}

/// Draws `text` with its baseline at `y`, alpha-blended.
fn draw_text(img: &mut RgbaImage, font: &FontRef, text: &str, px: f32, x: f32, y: f32, color: [u8; 4]) {
    let scaled = font.as_scaled(PxScale::from(px));
    let mut caret = x;
    let mut prev = None;
    for c in text.chars() {
        let id = scaled.glyph_id(c);
        if let Some(p) = prev {
            caret += scaled.kern(p, id);
        }
        let glyph = id.with_scale_and_position(px, point(caret, y));
        caret += scaled.h_advance(id);
        prev = Some(id);
        let Some(outline) = font.outline_glyph(glyph) else { continue };
        let b = outline.px_bounds();
        outline.draw(|gx, gy, cov| {
            let (px_, py) = (b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32);
            if px_ >= 0 && py >= 0 && (px_ as u32) < img.width() && (py as u32) < img.height() {
                blend(img.get_pixel_mut(px_ as u32, py as u32), color, cov);
            }
        });
    }
}

fn draw_centered(img: &mut RgbaImage, font: &FontRef, text: &str, px: f32, y: f32, color: [u8; 4]) {
    let x = (img.width() as f32 - text_width(font, text, px)) / 2.0;
    draw_text(img, font, text, px, x, y, color);
}

/// Source-over blend of `color` at `coverage` onto `dst`.
fn blend(dst: &mut Rgba<u8>, color: [u8; 4], coverage: f32) {
    let a = coverage.clamp(0.0, 1.0) * color[3] as f32 / 255.0;
    let da = dst[3] as f32 / 255.0;
    let out_a = a + da * (1.0 - a);
    if out_a <= 0.0 {
        return;
    }
    for k in 0..3 {
        let c = (color[k] as f32 * a + dst[k] as f32 * da * (1.0 - a)) / out_a;
        dst[k] = c.round() as u8;
    }
    dst[3] = (out_a * 255.0).round() as u8;
}

fn fill_rect(img: &mut RgbaImage, x0: u32, y0: u32, w: u32, h: u32, color: [u8; 4]) {
    for y in y0..(y0 + h).min(img.height()) {
        for x in x0..(x0 + w).min(img.width()) {
            blend(img.get_pixel_mut(x, y), color, 1.0);
        }
    }
}

fn gradient(w: u32, h: u32, top: [u8; 3], bottom: [u8; 3]) -> RgbaImage {
    RgbaImage::from_fn(w, h, |_, y| {
        let t = y as f32 / h as f32;
        let c = |k: usize| (top[k] as f32 + (bottom[k] as f32 - top[k] as f32) * t) as u8;
        Rgba([c(0), c(1), c(2), 255])
    })
}

const WHITE: [u8; 4] = [255, 255, 255, 255];
const DIM: [u8; 4] = [170, 180, 200, 255];

fn title_card(font: &FontRef) -> RgbaImage {
    let mut img = gradient(1920, 1080, [12, 16, 32], [36, 18, 60]);
    // Category colour stripes: audio, video, DMX, control.
    for (k, c) in [[37, 99, 235], [147, 51, 234], [217, 119, 6], [5, 150, 105]].iter().enumerate() {
        fill_rect(&mut img, 660 + k as u32 * 150, 640, 140, 10, [c[0], c[1], c[2], 255]);
    }
    draw_centered(&mut img, font, "LiveStage", 180.0, 470.0, WHITE);
    draw_centered(&mut img, font, "Feature Tour", 96.0, 590.0, DIM);
    draw_centered(&mut img, font, "Image cue · Main Screen · layer 0", 36.0, 760.0, [120, 130, 150, 255]);
    img
}

/// Transparent PNG: only the bar is opaque, so it sits on whatever plays below.
fn lower_third(font: &FontRef) -> RgbaImage {
    let mut img = RgbaImage::from_pixel(1920, 1080, Rgba([0, 0, 0, 0]));
    fill_rect(&mut img, 120, 820, 900, 150, [10, 14, 24, 215]);
    fill_rect(&mut img, 120, 820, 12, 150, [147, 51, 234, 255]);
    draw_text(&mut img, font, "Guest Speaker", 64.0, 170.0, 893.0, WHITE);
    draw_text(&mut img, font, "Lower third · PNG with alpha · layer 20", 34.0, 172.0, 945.0, DIM);
    img
}

/// 3 s at 25 fps: a rotating spinner with the frame number.
fn image_sequence(dir: &Path, font: &FontRef) -> PathBuf {
    fs::create_dir_all(dir).expect("sequence folder");
    let (w, h, n) = (640u32, 360u32, 75u32);
    for f in 0..n {
        let mut img = gradient(w, h, [8, 30, 40], [4, 12, 20]);
        let angle = f as f32 / n as f32 * TAU;
        let (cx, cy, r0, r1) = (w as f32 / 2.0, h as f32 / 2.0 - 20.0, 70.0, 110.0);
        for k in 0..12 {
            let a = angle + k as f32 / 12.0 * TAU;
            let level = (255.0 * (1.0 - k as f32 / 12.0)) as u8;
            for s in 0..40 {
                let rr = r0 + (r1 - r0) * s as f32 / 40.0;
                for d in -3..=3 {
                    let (x, y) = (cx + rr * a.cos() - d as f32 * a.sin(), cy + rr * a.sin() + d as f32 * a.cos());
                    if x >= 0.0 && y >= 0.0 && (x as u32) < w && (y as u32) < h {
                        blend(img.get_pixel_mut(x as u32, y as u32), [5, 150, 105, level], 1.0);
                    }
                }
            }
        }
        draw_centered(&mut img, font, &format!("Image sequence · frame {:02}/{n}", f + 1), 28.0, h as f32 - 30.0, DIM);
        img.save(dir.join(format!("frame_{:04}.png", f + 1))).expect("writing sequence frame");
    }
    dir.to_path_buf()
}

// -------------------------------------------------------------------- pdf --

/// A five-page 16:9 deck with Helvetica text and vector shapes.
fn write_pdf(path: &Path) -> PathBuf {
    let pages: [(&str, [f32; 3], &[&str]); 5] = [
        ("Feature Tour", [0.15, 0.39, 0.92], &["A PDF imported as a slide deck", "Next slide: right arrow, Page Down or OSC /livestage/slide/next"]),
        ("Audio", [0.15, 0.39, 0.92], &["Stems, buses and a crosspoint matrix", "Timeline: backing track + click with count-in", "Cart pads: sting, applause, loop, 1 kHz"]),
        ("Video", [0.58, 0.20, 0.92], &["Layers, fades, opacity and alpha PNGs", "Movies via FFmpeg (VideoToolbox / D3D11)", "Warp, edge blend and masks per output"]),
        ("Lighting", [0.85, 0.47, 0.02], &["sACN + Art-Net, frame sync, pixel mapping", "Palettes, submasters, effects, stage plot"]),
        ("Control", [0.02, 0.59, 0.41], &["OSC, WebSocket, MIDI / MSC, timecode", "Thanks for testing!"]),
    ];
    let esc = |s: &str| s.replace('\\', "\\\\").replace('(', "\\(").replace(')', "\\)");
    let mut objects: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        String::new(), // pages, filled in below
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".into(),
    ];
    let mut kids = Vec::new();
    for (i, (title, accent, lines)) in pages.iter().enumerate() {
        let mut c = String::new();
        c += "0.06 0.08 0.13 rg 0 0 960 540 re f\n";
        c += &format!("{} {} {} rg 0 0 960 14 re f 60 380 8 70 re f\n", accent[0], accent[1], accent[2]);
        c += &format!("BT /F1 54 Tf 1 1 1 rg 84 392 Td ({}) Tj ET\n", esc(title));
        for (k, line) in lines.iter().enumerate() {
            c += &format!("BT /F1 26 Tf 0.8 0.84 0.9 rg 84 {} Td ({}) Tj ET\n", 300 - k as i32 * 50, esc(line));
        }
        c += &format!("BT /F1 16 Tf 0.5 0.55 0.65 rg 820 40 Td (Slide {} / {}) Tj ET\n", i + 1, pages.len());
        let content = objects.len() + 2;
        objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 960 540] /Resources << /Font << /F1 3 0 R >> >> /Contents {content} 0 R >>"));
        kids.push(format!("{} 0 R", objects.len()));
        objects.push(format!("<< /Length {} >>\nstream\n{c}endstream", c.len()));
    }
    objects[1] = format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), kids.len());
    let mut pdf = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, o) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf += &format!("{} 0 obj\n{o}\nendobj\n", i + 1);
    }
    let xref = pdf.len();
    pdf += &format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
    for off in offsets {
        pdf += &format!("{off:010} 00000 n \n");
    }
    pdf += &format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1);
    fs::write(path, pdf).expect("writing pdf");
    path.to_path_buf()
}

// ------------------------------------------------------------------ movie --

#[cfg(feature = "ffmpeg")]
mod movie {
    use super::*;
    use ffmpeg_next as ff;

    const W: u32 = 1280;
    const H: u32 = 720;
    const FPS: i32 = 25;
    const SECS: i32 = 10;

    /// A 10 s 720p25 countdown: big seconds digit, frame counter and a bar
    /// that sweeps once per second (easy to judge smoothness and sync).
    pub fn write(path: &Path, font: &FontRef) -> Option<PathBuf> {
        ff::init().ok()?;
        let codec = ff::encoder::find_by_name("h264_videotoolbox").or_else(|| ff::encoder::find(ff::codec::Id::MPEG4))?;
        let mut out = ff::format::output(path).ok()?;
        let global_header = out.format().flags().contains(ff::format::Flags::GLOBAL_HEADER);
        let mut enc = ff::codec::context::Context::new_with_codec(codec).encoder().video().ok()?;
        enc.set_width(W);
        enc.set_height(H);
        enc.set_format(ff::format::Pixel::YUV420P);
        enc.set_time_base((1, FPS));
        enc.set_frame_rate(Some((FPS, 1)));
        enc.set_gop(FPS as u32);
        enc.set_bit_rate(8_000_000);
        enc.set_colorspace(ff::color::Space::BT709);
        enc.set_color_range(ff::color::Range::MPEG);
        if global_header {
            enc.set_flags(ff::codec::Flags::GLOBAL_HEADER);
        }
        let mut enc = enc.open_as(codec).ok()?;
        let index = {
            let mut st = out.add_stream(codec).ok()?;
            st.set_parameters(&enc);
            st.set_time_base((1, FPS));
            st.index()
        };
        out.write_header().ok()?;
        let stream_tb = out.stream(index)?.time_base();
        let mut packet = ff::Packet::empty();
        let mut drain = |enc: &mut ff::encoder::Video, out: &mut ff::format::context::Output| {
            while enc.receive_packet(&mut packet).is_ok() {
                packet.set_stream(index);
                packet.rescale_ts((1, FPS), stream_tb);
                let _ = packet.write_interleaved(out);
            }
        };
        for i in 0..(FPS * SECS) {
            let rgba = movie_frame(font, i);
            let mut frame = ff::frame::Video::new(ff::format::Pixel::YUV420P, W, H);
            to_yuv420(&rgba, &mut frame);
            frame.set_pts(Some(i as i64));
            enc.send_frame(&frame).ok()?;
            drain(&mut enc, &mut out);
        }
        enc.send_eof().ok()?;
        drain(&mut enc, &mut out);
        out.write_trailer().ok()?;
        Some(path.to_path_buf())
    }

    fn movie_frame(font: &FontRef, i: i32) -> RgbaImage {
        let sec = i / FPS;
        let within = i % FPS;
        let hue = sec as f32 / SECS as f32;
        let top = [(30.0 + 60.0 * hue) as u8, 20, (90.0 - 50.0 * hue) as u8];
        let mut img = gradient(W, H, top, [8, 8, 14]);
        draw_centered(&mut img, font, &format!("{}", SECS - sec), 360.0, 470.0, WHITE);
        let tc = format!("00:00:{:02}:{:02}", sec, within);
        draw_text(&mut img, font, &format!("Movie · frame {:03} · {tc}", i), 30.0, 40.0, 60.0, DIM);
        // Sweep bar: crosses the screen once per second.
        let x = (within as f32 / FPS as f32 * (W - 80) as f32) as u32;
        fill_rect(&mut img, 0, H - 40, W, 40, [20, 20, 30, 255]);
        fill_rect(&mut img, x, H - 36, 80, 32, [217, 119, 6, 255]);
        // A white flash on every whole second.
        if within == 0 {
            fill_rect(&mut img, W - 120, 20, 100, 100, WHITE);
        }
        img
    }

    /// RGBA → limited-range BT.709 YUV 4:2:0.
    fn to_yuv420(img: &RgbaImage, frame: &mut ff::frame::Video) {
        let (w, h) = (img.width() as usize, img.height() as usize);
        let px = |x: usize, y: usize| {
            let p = img.get_pixel(x as u32, y as u32);
            (p[0] as f32, p[1] as f32, p[2] as f32)
        };
        let ys = frame.stride(0);
        let data = frame.data_mut(0);
        for y in 0..h {
            for x in 0..w {
                let (r, g, b) = px(x, y);
                data[y * ys + x] = (16.0 + (0.1826 * r + 0.6142 * g + 0.0620 * b)).round() as u8;
            }
        }
        for plane in 1..3 {
            let stride = frame.stride(plane);
            let data = frame.data_mut(plane);
            for y in 0..h / 2 {
                for x in 0..w / 2 {
                    let mut acc = (0.0, 0.0, 0.0);
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let (r, g, b) = px(x * 2 + dx, y * 2 + dy);
                        acc = (acc.0 + r / 4.0, acc.1 + g / 4.0, acc.2 + b / 4.0);
                    }
                    let (r, g, b) = acc;
                    let v = if plane == 1 { 128.0 - 0.1006 * r - 0.3386 * g + 0.4392 * b } else { 128.0 + 0.4392 * r - 0.3989 * g - 0.0403 * b };
                    data[y * stride + x] = v.round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
}

#[cfg(not(feature = "ffmpeg"))]
mod movie {
    use super::*;

    pub fn write(_: &Path, _: &FontRef) -> Option<PathBuf> {
        None
    }
}

// ----------------------------------------------------------------- import --

/// Bundle-relative references to the imported media.
struct Assets {
    backing: AssetRef,
    ambience: AssetRef,
    sting: AssetRef,
    applause: AssetRef,
    tone: AssetRef,
    title: AssetRef,
    lower_third: AssetRef,
    sequence: AssetRef,
    movie: Option<AssetRef>,
    deck: livestage_core::cue::SlideDeck,
}

impl Assets {
    fn import(bundle: &mut ProjectBundle, m: &Media) -> Self {
        let mut add = |p: &Path| bundle.import_asset(p).expect("importing asset").path;
        let (backing, ambience, sting, applause, tone) = (add(&m.backing), add(&m.ambience), add(&m.sting), add(&m.applause), add(&m.tone));
        let (title, lower_third) = (add(&m.title), add(&m.lower_third));
        let movie = m.movie.as_deref().map(&mut add);
        let deck_src = add(&m.deck);

        // Image sequences are folders of frames under assets/video/.
        let seq_name = "Spinner sequence";
        let seq_dir = bundle.assets_dir().join(AssetKind::Video.folder()).join(seq_name);
        fs::create_dir_all(&seq_dir).expect("sequence folder");
        let mut size = 0;
        for e in fs::read_dir(&m.sequence).expect("sequence frames").flatten() {
            size += fs::copy(e.path(), seq_dir.join(e.file_name())).expect("copying frame");
        }
        let sequence = AssetRef::new(format!("assets/{}/{seq_name}", AssetKind::Video.folder()));
        bundle.project.assets.push(AssetRecord {
            id: AssetId::new(),
            path: sequence.clone(),
            kind: AssetKind::Video,
            original_name: seq_name.into(),
            size_bytes: size,
            imported_at: bundle.project.created_at,
        });

        // The deck goes through the same importer as File → Import.
        let src = bundle.resolve(&deck_src).expect("deck path");
        let (out, prefix) = bundle.new_slides_dir("Feature Tour deck").expect("slides folder");
        let opts = livestage_video::slides::ImportOptions { target: (1920, 1080), office: None };
        let deck = livestage_video::slides::import_document(&src, Some(deck_src), &out, &prefix, &opts, &mut |_, _| {}).expect("rendering the PDF deck");
        Assets { backing, ambience, sting, applause, tone, title, lower_third, sequence, movie, deck }
    }
}

// ------------------------------------------------------------------- show --

fn secs(s: f64) -> ShowTime {
    ShowTime::from_secs_f64(s)
}

fn header(number: &str, name: &str, notes: &str) -> CueHeader {
    let mut h = CueHeader::new(number, name);
    h.notes = notes.into();
    h
}

fn audio(h: CueHeader, asset: &AssetRef, stem: StemId, gain_db: f32, length: f64) -> AudioCue {
    AudioCue {
        header: h,
        tracks: vec![livestage_core::action::AudioTrack { asset: asset.clone(), stem, gain_db: 0.0, mute: false }],
        click: None,
        gain_db,
        fade_in: ShowTime::ZERO,
        fade_out: ShowTime::ZERO,
        start_at: ShowTime::ZERO,
        end_at: None,
        looped: false,
        media_length: Some(secs(length)),
    }
}

fn visual(h: CueHeader, output: OutputId, content: VisualContent, layer: i32) -> VideoCue {
    VideoCue {
        header: h,
        output,
        content,
        layer,
        opacity: 1.0,
        fade_in: secs(0.5),
        fade_out: secs(0.5),
        hold: None,
        media_length: None,
        end_behavior: EndBehavior::Hide,
    }
}

fn text(text: &str, size: f32, scroll: f32) -> VisualContent {
    VisualContent::Text(TextContent { text: text.into(), font_size: size, color: [1.0, 1.0, 1.0, 1.0], scroll_speed: scroll, mirror_horizontal: false })
}

fn dmx(h: CueHeader, attributes: Vec<FixtureAttributeValue>, palettes: Vec<PaletteRef>, effects: Vec<EffectId>, fade: f64) -> DmxCue {
    DmxCue { header: h, channels: vec![], attributes, fade: secs(fade), release_on_stop: false, palettes, effects }
}

fn control(h: CueHeader, action: ControlAction) -> ControlCue {
    ControlCue { header: h, action }
}

fn level(fixtures: &[FixtureId], attribute: &str, value: f32) -> Vec<FixtureAttributeValue> {
    fixtures.iter().map(|&fixture| FixtureAttributeValue { fixture, attribute: attribute.into(), value }).collect()
}

fn pal(palette: PaletteId, fixtures: &[FixtureId]) -> PaletteRef {
    PaletteRef { palette, fixtures: fixtures.to_vec() }
}

fn tc(h: u8, m: u8, s: u8, f: u8) -> Timecode {
    Timecode::new(h, m, s, f, FrameRate::Fps25).expect("valid timecode")
}

fn colour(rgb: Rgb, h: CueHeader) -> CueHeader {
    CueHeader { color: Some(rgb), ..h }
}

/// Everything the show needs beyond media: outputs, routing, DMX, control,
/// lighting console, carts and cue lists.
fn build_show(bundle: &mut ProjectBundle, a: &Assets) {
    let p = &mut bundle.project;

    // --- outputs: windowed, so nothing takes over a screen on first open.
    p.displays[0].name = "Main Screen".into();
    p.displays[1].name = "Confidence Monitor".into();
    let mut projector = livestage_core::routing::LogicalDisplay::new("Projector (warp + blend + mask)");
    {
        let warp = &mut projector.projection.warp;
        // A gentle keystone: top edge pulled in.
        *warp.point_mut(0, 0) = [0.06, 0.02];
        *warp.point_mut(3, 0) = [0.94, 0.02];
        projector.projection.blend.right.width = 0.12;
        projector.projection.masks.push(PolygonMask {
            name: "Truss leg".into(),
            points: vec![[0.78, 0.0], [0.86, 0.0], [0.86, 1.0], [0.78, 1.0]],
            invert: false,
            enabled: true,
        });
    }
    p.displays.push(projector);
    for d in &mut p.displays {
        d.fullscreen = false;
        d.canvas_size = Some([1920, 1080]);
    }
    let (main, conf, proj) = (p.displays[0].id, p.displays[1].id, p.displays[2].id);
    p.multiview = Some(MultiviewLayout {
        rows: 2,
        cols: 3,
        feature_first: true,
        cells: vec![
            MultiviewSource::Program,
            MultiviewSource::Next,
            MultiviewSource::NextSlide,
            MultiviewSource::Output { id: conf },
            MultiviewSource::Output { id: proj },
            MultiviewSource::Clock,
        ],
    });

    // --- audio: default routing (Backing → FOH 1-2, Click → 3-4, In-Ear → 5-8).
    let stem = |name: &str| p.audio.stems.iter().find(|s| s.name == name).map(|s| s.id).expect("default stem");
    let (backing_stem, click_stem, iem_stem) = (stem("Backing Tracks"), stem("Click"), stem("In-Ear Mix"));

    // --- DMX: everything unicast to this computer until you point it at a node.
    p.dmx.universes = vec![
        UniverseConfig { index: 0, name: "Stage (sACN 1)".into(), transport: DmxTransport::Sacn { universe: 1, priority: 100, destination: Some("127.0.0.1".into()) } },
        UniverseConfig { index: 1, name: "Pixels (Art-Net 0:0:1)".into(), transport: DmxTransport::ArtNet { port_address: 1, destination: "127.0.0.1".into() } },
    ];
    p.dmx.sync.sacn_sync_universe = Some(7_000);
    p.dmx.sync.artsync = true;
    let mut fixture = |name: &str, def: &str, mode: &str, address: u16| {
        let f = PatchedFixture { id: FixtureId::new(), name: name.into(), definition: def.into(), mode: mode.into(), universe: 0, address };
        let id = f.id;
        p.dmx.patch.push(f);
        id
    };
    let pars: Vec<FixtureId> = (0..4).map(|i| fixture(&format!("Par {}", i + 1), "generic/rgbw-par", "5ch", 1 + i * 5)).collect();
    let movers: Vec<FixtureId> = (0..2).map(|i| fixture(&format!("Spot {}", i + 1), "generic/moving-head", "12ch", 21 + i * 12)).collect();
    let house = fixture("House lights", "generic/dimmer", "8bit", 50);
    let all: Vec<FixtureId> = pars.iter().chain(&movers).copied().collect();
    p.dmx.pixel_maps.push(PixelMapConfig {
        name: "LED strip wall 32×8 (from Main Screen)".into(),
        source: main,
        region: [0.0, 0.0, 1.0, 1.0],
        width: 32,
        height: 8,
        order: PixelOrder::Rgb,
        wiring: PixelWiring::Serpentine,
        start_universe: 1,
        start_address: 1,
        enabled: true,
    });

    // --- control: OSC and WebSocket on (this computer only).
    p.control.osc_enabled = true;
    p.control.websocket_enabled = true;
    p.control.external_access = false;

    // --- timecode: internal clock starting at 01:00:00:00 (drives the second list).
    p.timecode.source = TimecodeSource::Internal;
    p.timecode.rate = FrameRate::Fps25;
    p.timecode.internal_start = tc(1, 0, 0, 0);

    // --- lighting console.
    let lv = LightValue::any;
    let palette = |name: &str, kind: PaletteKind, swatch: Option<Rgb>, values: Vec<LightValue>| {
        let mut pl = Palette::new(name, kind, values);
        pl.swatch = swatch;
        pl
    };
    let warm = palette("Warm white", PaletteKind::Color, Some(Rgb::hex(0xffd8a0)), vec![lv("red", 1.0), lv("green", 0.8), lv("blue", 0.5), lv("white", 1.0)]);
    let blue = palette("Deep blue", PaletteKind::Color, Some(Rgb::hex(0x1e3cff)), vec![lv("red", 0.0), lv("green", 0.15), lv("blue", 1.0), lv("white", 0.0)]);
    let red = palette("Red", PaletteKind::Color, Some(Rgb::hex(0xff1010)), vec![lv("red", 1.0), lv("green", 0.0), lv("blue", 0.0), lv("white", 0.0)]);
    let magenta = palette("Magenta", PaletteKind::Color, Some(Rgb::hex(0xe020e0)), vec![lv("red", 0.9), lv("green", 0.0), lv("blue", 0.9), lv("white", 0.0)]);
    let centre = palette("Centre stage", PaletteKind::Position, None, vec![lv("pan", 0.5), lv("tilt", 0.5)]);
    let audience = palette("Audience", PaletteKind::Position, None, vec![lv("pan", 0.5), lv("tilt", 0.82)]);
    let full = palette("Full", PaletteKind::Intensity, None, vec![lv("intensity", 1.0)]);
    let tight = palette("Tight beam", PaletteKind::Beam, None, vec![lv("zoom", 0.1), lv("focus", 0.6)]);
    let ids = (warm.id, blue.id, red.id, magenta.id, centre.id, audience.id, full.id);
    p.lighting.palettes = vec![warm, blue, red, magenta, centre, audience, full, tight];
    let mut pars_sub = Submaster::new("Pars (additive)", SubmasterKind::Additive { values: vec![lv("intensity", 1.0), lv("white", 1.0)] }, pars.clone());
    pars_sub.color = Some(Rgb::hex(0xd97706));
    let strobe = Submaster::new("Strobe 1–15 Hz", SubmasterKind::Strobe { min_hz: 1.0, max_hz: 15.0 }, pars.clone());
    let mut inhibit = Submaster::new("Movers (inhibit)", SubmasterKind::Inhibit, movers.clone());
    inhibit.level = 1.0;
    let mut house_sub = Submaster::new("House lights", SubmasterKind::Additive { values: vec![lv("intensity", 1.0)] }, vec![house]);
    house_sub.level = 0.6;
    p.lighting.submasters = vec![pars_sub, strobe, inhibit, house_sub];
    let mut rainbow = Effect::new(
        "Rainbow chase",
        EffectKind::ColorChase { colors: vec![Rgb::hex(0xff0000), Rgb::hex(0xff8000), Rgb::hex(0x00ff40), Rgb::hex(0x0040ff), Rgb::hex(0xc000ff)] },
        pars.clone(),
    );
    (rainbow.rate_hz, rainbow.size, rainbow.spread) = (0.5, 1.0, 1.0);
    let mut circle = Effect::new("Mover circle", EffectKind::Circle, movers.clone());
    (circle.rate_hz, circle.size, circle.spread) = (0.25, 0.3, 0.5);
    let mut chase = Effect::new("Dimmer chase", EffectKind::DimmerChase, pars.clone());
    (chase.rate_hz, chase.size, chase.spread) = (1.0, 1.0, 1.0);
    let fx = (rainbow.id, circle.id, chase.id);
    p.lighting.effects = vec![rainbow, circle, chase];
    for (i, f) in pars.iter().enumerate() {
        p.lighting.stage.insert(*f, [0.2 + 0.2 * i as f32, 0.72]);
    }
    p.lighting.stage.insert(movers[0], [0.3, 0.3]);
    p.lighting.stage.insert(movers[1], [0.7, 0.3]);
    p.lighting.stage.insert(house, [0.5, 0.95]);
    let (warm, blue, red, magenta, centre, audience, full) = ids;

    // --- main cue list.
    let mut list = CueList::new("Feature Tour");
    let mut add = |c: AnyCue| list.cues.push(c);

    let mut h = colour(Rgb::hex(0x9333ea), header("1", "Title card (image, 1 s fade)", "Image cue on Main Screen. Auto-continues to cue 2 after 2 s."));
    h.follow = livestage_core::cue::FollowMode::AutoContinue;
    h.post_wait = secs(2.0);
    let mut c = visual(h, main, VisualContent::Image { asset: a.title.clone() }, 0);
    c.fade_in = secs(1.0);
    add(AnyCue::Video(c));

    add(AnyCue::Timer(TimerCue {
        header: header("2", "Doors countdown (30 s)", "Timer cue: turns red under 10 s and holds at 00:00."),
        output: main,
        timer: TimerContent {
            mode: TimerMode::CountDown { from: secs(30.0) },
            label: "Doors in".into(),
            font_size: 160.0,
            color: [1.0, 1.0, 1.0, 1.0],
            warn_color: [1.0, 0.25, 0.2, 1.0],
            warn_below: secs(10.0),
            started_at: ShowTime::ZERO,
            elapsed_offset: ShowTime::ZERO,
        },
        layer: 10,
        hold_at_zero: true,
    }));

    let mut attrs = level(&pars, "intensity", 0.8);
    attrs.extend(level(&movers, "intensity", 0.6));
    add(AnyCue::Dmx(dmx(
        colour(Rgb::hex(0xd97706), header("3", "House look (palettes, 3 s fade)", "Warm-white pars, movers to centre stage. Watch the DMX and Lighting panels.")),
        attrs,
        vec![pal(warm, &pars), pal(centre, &movers)],
        vec![],
        3.0,
    )));

    let mut c = audio(colour(Rgb::hex(0x2563eb), header("4", "Ambience pad (loop, 2 s fade in)", "Looped 12 s pad. Stops with cue 18 (Stop all).")), &a.ambience, backing_stem, -6.0, 12.0);
    c.looped = true;
    c.fade_in = secs(2.0);
    c.fade_out = secs(3.0);
    add(AnyCue::Audio(c));

    let mut h = header("5", "Line check 1 kHz −20 dBFS (disarmed)", "Disarmed: GO skips it. Fire it on purpose to check routing (In-Ear stem → outs 5-8).");
    h.armed = false;
    add(AnyCue::Audio(audio(h, &a.tone, iem_stem, 0.0, 5.0)));

    add(AnyCue::Video(visual(header("6", "Test pattern → Confidence Monitor", "Check output assignment and scaling."), conf, VisualContent::TestPattern, 0)));

    let mut h = header("7", "Lower third (PNG alpha, layer 20)", "1 s pre-wait, then fades over whatever plays below.");
    h.pre_wait = secs(1.0);
    add(AnyCue::Video(visual(h, main, VisualContent::Image { asset: a.lower_third.clone() }, 20)));

    let mut c = visual(header("8", "Ticker (scrolling text, layer 21)", "Text cue with scroll speed."), main, text("LiveStage Feature Tour  ·  scrolling text on layer 21  ·  ", 48.0, 140.0), 21);
    c.hold = Some(secs(20.0));
    add(AnyCue::Video(c));

    add(AnyCue::Slides(SlideCue {
        header: colour(Rgb::hex(0x0891b2), header("9", "Keynote deck (PDF → slides)", "5 slides. Next/previous with arrow keys, the transport, or OSC /livestage/slide/next.")),
        output: main,
        deck: a.deck.clone(),
        start_slide: 0,
        layer: 5,
        opacity: 1.0,
        fade_in: secs(0.5),
        fade_out: secs(0.5),
        transition: secs(0.4),
        current: 0,
    }));

    if let Some(movie) = &a.movie {
        let mut c = visual(colour(Rgb::hex(0x9333ea), header("10", "Movie: countdown clip (FFmpeg)", "10 s H.264 720p25; holds its last frame. Hardware decode on macOS/Windows.")), main, VisualContent::Video { asset: movie.clone(), looped: false }, 2);
        c.end_behavior = EndBehavior::HoldLastFrame;
        c.media_length = Some(secs(10.0));
        add(AnyCue::Video(c));
    }

    add(AnyCue::Video(visual(header("11", "Image sequence spinner → Projector (loop)", "PNG frame folder; see the projector's warp, blend and mask."), proj, VisualContent::Video { asset: a.sequence.clone(), looped: true }, 0)));

    // Song: backing + click (1 bar count-in) with lights and confidence text on the timeline.
    let mut master = audio(header("12.0", "Backing + click", ""), &a.backing, backing_stem, -3.0, SONG_SECS as f64);
    master.click = Some(ClickTrack { bpm: BPM, beats_per_bar: 4, stem: click_stem, gain_db: -6.0, count_in_bars: 1 });
    let lyric = |n: &str, words: &str| {
        let mut c = visual(header(n, words, ""), conf, text(words, 72.0, 0.0), 30);
        c.hold = Some(secs(7.5));
        AnyCue::Video(c)
    };
    let item = |offset: f64, cue: AnyCue| TimelineItem { offset: secs(offset), cue, fired: false };
    let song = TimelineCue {
        header: colour(Rgb::hex(0x0d9488), header("12", "Song 1 — timeline (backing, click, lights, lyrics)", "Click on the Click stem (outs 3-4). Drag items on the timeline to retime them.")),
        master: Some(Box::new(AnyCue::Audio(master))),
        items: vec![
            item(0.0, AnyCue::Dmx(dmx(header("12.1", "Verse look", ""), level(&pars, "intensity", 0.7), vec![pal(blue, &pars)], vec![], 1.0))),
            item(0.0, lyric("12.2", "Verse — bars 1–4")),
            item(8.0, AnyCue::Dmx(dmx(header("12.3", "Chorus look", ""), level(&all, "intensity", 1.0), vec![pal(red, &pars), pal(audience, &movers)], vec![], 0.3))),
            item(8.0, lyric("12.4", "Chorus — bars 5–8")),
            item(16.0, AnyCue::Dmx(dmx(header("12.5", "Bridge: rainbow + circle", ""), vec![], vec![], vec![fx.0, fx.1], 0.0))),
            item(16.0, lyric("12.6", "Bridge — bars 9–12")),
            item(24.0, AnyCue::Dmx(dmx(header("12.7", "Outro: magenta, slow fade", ""), level(&pars, "intensity", 0.5), vec![pal(magenta, &pars)], vec![], 4.0))),
            item(24.0, lyric("12.8", "Outro — bars 13–16")),
        ],
        length: Some(secs(SONG_SECS as f64)),
        timecode: None,
    };
    add(AnyCue::Timeline(song));

    add(AnyCue::Dmx(dmx(header("13", "Effects: rainbow chase + dimmer chase", "Lighting → Effects shows them running."), level(&pars, "intensity", 1.0), vec![pal(full, &pars)], vec![fx.0, fx.2], 0.5)));
    add(AnyCue::Dmx(dmx(header("14", "Effect: mover circle", ""), level(&movers, "intensity", 0.9), vec![], vec![fx.1], 0.5)));

    add(AnyCue::Control(control(
        colour(Rgb::hex(0x059669), header("15", "OSC out → 127.0.0.1:9000 /livestage/hello", "Watch it with any OSC monitor on port 9000.")),
        ControlAction::SendOsc { host: "127.0.0.1".into(), port: 9000, address: "/livestage/hello".into(), args: vec![OscArg::String("feature tour".into()), OscArg::Int(15), OscArg::Float(0.5)] },
    )));
    let mut h = header("16", "MIDI out: note on C4 (disarmed)", "Disarmed: needs a MIDI output port. Uses the first port when none is named.");
    h.armed = false;
    add(AnyCue::Control(control(h, ControlAction::SendMidi { port: None, bytes: vec![0x90, 60, 100] })));
    let mut h = header("17", "NDI input (disarmed)", "Disarmed: set the source name to an NDI source on your network (needs the NDI runtime).");
    h.armed = false;
    add(AnyCue::Video(visual(h, proj, VisualContent::Ndi { source_name: "MACHINE (Source name)".into() }, 1)));
    add(AnyCue::Video(visual(header("18", "Red wash → Projector (colour, layer 1)", ""), proj, VisualContent::Color { rgba: [0.8, 0.05, 0.05, 1.0] }, 1)));
    add(AnyCue::Control(control(colour(Rgb::hex(0xdc2626), header("19", "Stop all (3 s fade)", "Fades every audio and video cue.")), ControlAction::Command(ShowCommand::StopAll { fade: Some(secs(3.0)) }))));
    add(AnyCue::Dmx(dmx(header("20", "Lights out (2 s)", "Intensity to zero; effects stop with their cue."), level(&all, "intensity", 0.0), vec![], vec![], 2.0)));
    add(AnyCue::Control(control(header("21", "Roll internal timecode", "Starts the internal clock at 01:00:00:00: the 'Timecode Show' list fires on its own."), ControlAction::Command(ShowCommand::TimecodeStart))));
    add(AnyCue::Control(control(header("22", "Marker — end of tour", "Markers only log a line."), ControlAction::Marker { text: "Feature tour complete".into() })));
    // A raw channel cue: house lights via channel 50 rather than a fixture attribute.
    add(AnyCue::Dmx(DmxCue {
        header: header("23", "House lights up (raw channel 50 @ 70 %)", "Raw DMX channel value, 1 s fade."),
        channels: vec![DmxChannelValue { universe: 0, channel: 50, value: 178 }],
        attributes: vec![],
        fade: secs(1.0),
        release_on_stop: true,
        palettes: vec![],
        effects: vec![],
    }));
    let main_id = list.id;

    // --- second list: fired by timecode.
    let mut tcl = CueList::new("Timecode Show (01:00:00:00)");
    tcl.timecode = Some(ListTimecode::new());
    let at = |h: CueHeader, t: Timecode| CueHeader { timecode: Some(t), ..h };
    tcl.cues.push(AnyCue::Dmx(dmx(at(header("T1", "Blue look @ 01:00:00:00", ""), tc(1, 0, 0, 0)), level(&pars, "intensity", 0.8), vec![pal(blue, &pars)], vec![], 1.0)));
    let mut c = visual(at(header("T2", "Text @ 01:00:04:00", ""), tc(1, 0, 4, 0)), conf, text("Timecode 01:00:04:00", 96.0, 0.0), 0);
    c.hold = Some(secs(3.5));
    tcl.cues.push(AnyCue::Video(c));
    tcl.cues.push(AnyCue::Dmx(dmx(at(header("T3", "Red look @ 01:00:08:00", ""), tc(1, 0, 8, 0)), level(&pars, "intensity", 1.0), vec![pal(red, &pars)], vec![], 0.5)));
    tcl.cues.push(AnyCue::Control(control(at(header("T4", "Stop timecode @ 01:00:12:00", ""), tc(1, 0, 12, 0)), ControlAction::Command(ShowCommand::TimecodeStop))));
    p.cue_lists = vec![list, tcl];
    p.active_cue_list = Some(main_id);

    // --- cart: keys 1–8 and MIDI notes 36–43 (channel 1).
    let mut cart = CueCart::new("Sound board", 4, 4);
    let pad = |cue: AnyCue, mode: PadMode, rgb: u32, key: &str, note: u8| CartPad {
        mode,
        color: Some(Rgb::hex(rgb)),
        key: Some(key.into()),
        midi: Some(PadNote { channel: 0, note }),
        ..CartPad::new(cue)
    };
    let mut loop_pad = audio(header("P3", "Ambience loop", ""), &a.ambience, backing_stem, -6.0, 12.0);
    loop_pad.looped = true;
    let mut flash = DmxCue { release_on_stop: true, ..dmx(header("P5", "Strobe flash (hold)", ""), level(&pars, "intensity", 1.0), vec![pal(warm, &pars)], vec![], 0.0) };
    flash.attributes.extend(level(&pars, "white", 1.0));
    cart.pads[0] = Some(pad(AnyCue::Audio(audio(header("P1", "Sting", ""), &a.sting, backing_stem, 0.0, 2.5)), PadMode::Retrigger, 0x2563eb, "Num1", 36));
    cart.pads[1] = Some(pad(AnyCue::Audio(audio(header("P2", "Applause", ""), &a.applause, backing_stem, 0.0, 4.0)), PadMode::Retrigger, 0x2563eb, "Num2", 37));
    cart.pads[2] = Some(pad(AnyCue::Audio(loop_pad), PadMode::Toggle, 0x1d4ed8, "Num3", 38));
    cart.pads[3] = Some(pad(AnyCue::Audio(audio(header("P4", "1 kHz tone", ""), &a.tone, iem_stem, 0.0, 5.0)), PadMode::Toggle, 0x64748b, "Num4", 39));
    cart.pads[4] = Some(pad(AnyCue::Dmx(flash), PadMode::Hold, 0xd97706, "Num5", 40));
    cart.pads[5] = Some(pad(AnyCue::Video(visual(header("P6", "Red wash", ""), proj, VisualContent::Color { rgba: [0.8, 0.05, 0.05, 1.0] }, 3)), PadMode::Toggle, 0xdc2626, "Num6", 41));
    cart.pads[6] = Some(pad(AnyCue::Video(visual(header("P7", "Test pattern", ""), main, VisualContent::TestPattern, 40)), PadMode::Toggle, 0x9333ea, "Num7", 42));
    cart.pads[7] = Some(pad(AnyCue::Control(control(header("P8", "Stop all", ""), ControlAction::Command(ShowCommand::StopAll { fade: Some(secs(1.0)) }))), PadMode::Retrigger, 0x059669, "Num0", 43));
    p.carts = vec![cart];
}

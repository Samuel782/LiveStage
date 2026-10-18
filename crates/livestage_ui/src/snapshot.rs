//! Headless UI snapshots: renders the real control UI offscreen to a PNG.
//!
//! ```text
//! livestage --ui-snapshot out.png [--show <bundle>] [--size 1600x1000] [--scale 2]
//!           [--go <cue number>]... [--next-slide <n>] [--select <cue number>] [--run-ms 1500]
//!           [--tab <name>] [--import <file>]... [--settings <section>]
//!           [--pad <cart>/<pad>]... [--edit-pad <cart>/<pad>]
//!           [--demo-rig] [--select-all-fixtures] [--apply-palette <name>]...
//!           [--fx <n>]... [--sub <n>=<level>]...
//! ```
//!
//! Used for visual review of layout changes and in CI-style checks. It
//! drives the same `panels::draw` as the app, with live canvases for the
//! multiview / display previews.

use crate::display::OutputEngine;
use crate::panels::{self, NdiStatus, Previews, UiState};
use crate::runtime::ShowRuntime;
use anyhow::Context as _;
use livestage_control::{ControlSource, ShowCommand};
use livestage_core::cue::Cue;
use livestage_core::ProjectBundle;
use livestage_video::{GpuContext, CANVAS_FORMAT};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub struct SnapshotArgs {
    pub out: PathBuf,
    pub show: Option<PathBuf>,
    pub size: [u32; 2],
    pub scale: f32,
    /// Cue numbers fired in order (`--go` may repeat).
    pub go: Vec<String>,
    /// Presses of Next Slide after the GOs.
    pub next_slides: u32,
    pub select: Option<String>,
    pub run_ms: u64,
    pub tab: Option<String>,
    /// Opens the Settings window on this section (general, audio, control, outputs, files).
    pub settings: Option<String>,
    /// Files imported (and, for presentations, rendered) before the snapshot.
    pub import: Vec<PathBuf>,
    /// Cart pads pressed after the GOs (1-based `cart/pad`).
    pub pads: Vec<(usize, usize)>,
    /// Opens the Soundboard in edit mode on this pad.
    pub edit_pad: Option<(usize, usize)>,
    /// Lighting: patch the demo rig, select every fixture, apply palettes
    /// to the programmer, start effects (1-based), set submasters.
    pub demo_rig: bool,
    pub select_all_fixtures: bool,
    pub palettes: Vec<String>,
    pub fx: Vec<usize>,
    pub subs: Vec<(usize, f32)>,
}

impl SnapshotArgs {
    pub fn parse(args: &[String]) -> Option<Self> {
        let i = args.iter().position(|a| a == "--ui-snapshot")?;
        let mut s = Self {
            out: args.get(i + 1)?.into(),
            show: None,
            size: [1600, 1000],
            scale: 1.0,
            go: Vec::new(),
            next_slides: 0,
            select: None,
            run_ms: 1500,
            tab: None,
            import: Vec::new(),
            settings: None,
            pads: Vec::new(),
            edit_pad: None,
            demo_rig: args.iter().any(|a| a == "--demo-rig"),
            select_all_fixtures: args.iter().any(|a| a == "--select-all-fixtures"),
            palettes: args.windows(2).filter(|w| w[0] == "--apply-palette").map(|w| w[1].clone()).collect(),
            fx: args.windows(2).filter(|w| w[0] == "--fx").filter_map(|w| w[1].parse().ok()).collect(),
            subs: args
                .windows(2)
                .filter(|w| w[0] == "--sub")
                .filter_map(|w| w[1].split_once('=').and_then(|(n, l)| Some((n.parse().ok()?, l.parse().ok()?))))
                .collect(),
        };
        let cart_pad = |v: &str| v.split_once('/').and_then(|(c, p)| Some((c.parse().ok()?, p.parse().ok()?)));
        let val = |flag: &str| args.iter().position(|a| a == flag).and_then(|j| args.get(j + 1)).cloned();
        s.show = val("--show").map(PathBuf::from);
        if let Some((Some(w), Some(h))) = val("--size").and_then(|v| v.split_once('x').map(|(w, h)| (w.parse().ok(), h.parse().ok()))) {
            s.size = [w, h];
        }
        s.scale = val("--scale").and_then(|v| v.parse().ok()).unwrap_or(1.0);
        s.go = args.windows(2).filter(|w| w[0] == "--go").map(|w| w[1].clone()).collect();
        s.next_slides = val("--next-slide").and_then(|v| v.parse().ok()).unwrap_or(0);
        s.select = val("--select");
        s.tab = val("--tab");
        s.settings = val("--settings");
        s.pads = args.windows(2).filter(|w| w[0] == "--pad").filter_map(|w| cart_pad(&w[1])).collect();
        s.edit_pad = val("--edit-pad").and_then(|v| cart_pad(&v));
        s.import = args.windows(2).filter(|w| w[0] == "--import").map(|w| PathBuf::from(&w[1])).collect();
        s.run_ms = val("--run-ms").and_then(|v| v.parse().ok()).unwrap_or(1500);
        Some(s)
    }
}

pub fn run(a: SnapshotArgs) -> anyhow::Result<()> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let gpu = pollster::block_on(GpuContext::new(instance, None))?;
    let bundle = match &a.show {
        Some(p) => ProjectBundle::open(p)?,
        None => ProjectBundle::open(std::env::current_dir()?.join("shows/Demo Show.livestage")).context("demo show")?,
    };
    let mut rt = ShowRuntime::new(bundle);
    if a.demo_rig {
        crate::demo::lighting_rig(&mut rt.bundle.project);
        rt.rebuild_dmx();
    }
    if a.select_all_fixtures {
        rt.dmx.console.programmer.selection = rt.bundle.project.dmx.patch.iter().map(|f| f.id).collect();
    }
    for name in &a.palettes {
        if let Some(p) = rt.bundle.project.lighting.palettes.iter().find(|p| p.name.eq_ignore_ascii_case(name)).cloned() {
            rt.dmx.programmer_apply_palette(&p);
        }
    }
    for &effect in &a.fx {
        rt.command(ControlSource::Ui, ShowCommand::SetEffect { effect, running: true });
    }
    for &(sub, level) in &a.subs {
        rt.command(ControlSource::Ui, ShowCommand::SetSubmaster { sub, level });
    }
    for f in &a.import {
        rt.import_and_create_cue(f);
    }
    // Wait for background presentation renders to land as cues.
    let deadline = Instant::now() + Duration::from_secs(60);
    while !rt.imports.is_empty() && Instant::now() < deadline {
        rt.tick();
        std::thread::sleep(Duration::from_millis(20));
    }
    for n in &a.go {
        rt.command(ControlSource::Ui, ShowCommand::GoCue { number: n.clone() });
        rt.tick();
    }
    for &(cart, pad) in &a.pads {
        rt.command(ControlSource::Ui, ShowCommand::CartPad { cart, pad, down: true });
        rt.tick();
    }
    for _ in 0..a.next_slides {
        rt.command(ControlSource::Ui, ShowCommand::NextSlide);
        rt.tick();
    }
    // Let the show run (cues, fades, timelines) as the show thread would.
    let until = Instant::now() + Duration::from_millis(a.run_ms);
    while Instant::now() < until {
        rt.tick();
        std::thread::sleep(Duration::from_millis(1));
    }

    let mut ui = UiState::default();
    if let Some(t) = &a.tab {
        if let Some(tab) = panels::Tab::ALL.into_iter().find(|x| x.title().to_lowercase().starts_with(&t.to_lowercase())) {
            ui.focus_tab(tab);
        }
    }
    if let Some((cart, pad)) = a.edit_pad {
        ui.focus_tab(panels::Tab::Carts);
        ui.cart_page = cart.saturating_sub(1);
        ui.cart_edit = true;
        ui.cart_slot = Some(pad.saturating_sub(1));
    }
    if let Some(sec) = &a.settings {
        ui.settings_open = true;
        ui.settings_section = crate::panels::settings::Section::from_name(sec);
    }
    ui.selected = a
        .select
        .as_deref()
        .and_then(|n| rt.bundle.project.active_list().and_then(|l| l.cues.iter().find(|c| c.header().number == n)))
        .map(|c| c.header().id);
    if let (None, Some((_, pad))) = (ui.selected, a.edit_pad) {
        ui.selected = rt.bundle.project.carts.get(ui.cart_page).and_then(|k| k.pad(pad.saturating_sub(1))).map(|p| p.cue.header().id);
    }

    // Program canvases for previews / multiview, rendered by the same engine
    // the output thread uses (driven synchronously here).
    let mut engine = OutputEngine::new(gpu.clone(), CANVAS_FORMAT);
    let root = rt.bundle.root().to_path_buf();
    let resolve = |x: &livestage_core::project::AssetRef| livestage_core::bundle::resolve_asset(&root, x).ok();
    let project_displays = rt.bundle.project.displays.clone();
    engine.sync_slots(&project_displays);
    for (at, action) in rt.video_receiver().try_iter() {
        engine.compositor.handle(at, &action, &resolve);
    }
    // A few frames so asynchronous sources (slides, video) deliver a picture.
    for _ in 0..10 {
        engine.compositor.update(rt.now());
        engine.render(rt.now(), &project_displays, &[]);
        std::thread::sleep(Duration::from_millis(20));
    }

    let ctx = egui::Context::default();
    crate::theme::apply(&ctx);
    let mut renderer = egui_wgpu::Renderer::new(&gpu.device, CANVAS_FORMAT, None, 1, false);
    let mut previews = Previews::default();
    for (id, c) in engine.previews() {
        let tex = renderer.register_native_texture(&gpu.device, &c.view, wgpu::FilterMode::Linear);
        previews.insert(id, (tex, c.size));
    }
    let ndi = NdiStatus::default();

    let [w, h] = a.size;
    let px = [(w as f32 * a.scale) as u32, (h as f32 * a.scale) as u32];
    let input = |time: f64| {
        let mut raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w as f32, h as f32))),
            time: Some(time),
            ..Default::default()
        };
        raw.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point = Some(a.scale);
        raw
    };
    // A few passes so egui settles sizes (first frames measure text, dock splits).
    // Texture uploads (font atlas) from earlier passes must be kept.
    let mut textures = egui::TexturesDelta::default();
    // Extra passes while thumbnails decode on worker threads (bounded), then
    // one more so freshly uploaded thumbnails are drawn.
    let mut i = 0;
    let mut output = loop {
        let mut out = ctx.run(input(i as f64 * 0.016), |ctx| panels::draw(ctx, &mut rt, &mut ui, &[], &previews, &ndi));
        textures.append(std::mem::take(&mut out.textures_delta));
        let settling = i < 4 || (ui.thumbs.pending() > 0 && i < 200);
        if i >= 3 && ui.thumbs.pending() > 0 {
            std::thread::sleep(Duration::from_millis(20));
        }
        i += 1;
        if !settling && i > 4 {
            break out;
        }
    };
    output.textures_delta = textures;

    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("snapshot"),
        size: wgpu::Extent3d { width: px[0], height: px[1], depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: CANVAS_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    let bg = crate::theme::BG;
    let clear = wgpu::Color { r: bg.r() as f64 / 255.0, g: bg.g() as f64 / 255.0, b: bg.b() as f64 / 255.0, a: 1.0 };
    let mut cmds = crate::gfx::paint_egui(&gpu, &mut renderer, &ctx, &mut enc, &view, px, output, Some(clear));
    let row = (px[0] * 4).div_ceil(256) * 256;
    let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (row * px[1]) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo { texture: &target, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(px[1]) },
        },
        wgpu::Extent3d { width: px[0], height: px[1], depth_or_array_layers: 1 },
    );
    cmds.push(enc.finish());
    gpu.queue.submit(cmds);
    buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    gpu.device.poll(wgpu::Maintain::Wait);
    let mapped = buf.slice(..).get_mapped_range();
    let mut rgba = Vec::with_capacity((px[0] * px[1] * 4) as usize);
    for y in 0..px[1] as usize {
        rgba.extend_from_slice(&mapped[y * row as usize..y * row as usize + px[0] as usize * 4]);
    }
    image::save_buffer(&a.out, &rgba, px[0], px[1], image::ExtendedColorType::Rgba8)?;
    println!("wrote {}", a.out.display());
    Ok(())
}

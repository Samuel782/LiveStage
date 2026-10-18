//! Headless UI snapshots: renders the real control UI offscreen to a PNG.
//!
//! ```text
//! livestage --ui-snapshot out.png [--show <bundle>] [--size 1600x1000] [--scale 2]
//!           [--go <cue number>] [--select <cue number>] [--run-ms 1500]
//! ```
//!
//! Used for visual review of layout changes and in CI-style checks. It
//! drives the same `panels::draw` as the app, with live canvases for the
//! multiview / display previews.

use crate::display::DisplayManager;
use crate::panels::{self, NdiStatus, Previews, UiState};
use crate::runtime::ShowRuntime;
use anyhow::Context as _;
use livestage_control::{ControlSource, ShowCommand};
use livestage_core::cue::Cue;
use livestage_core::ProjectBundle;
use livestage_video::{GpuContext, Pipelines, CANVAS_FORMAT};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub struct SnapshotArgs {
    pub out: PathBuf,
    pub show: Option<PathBuf>,
    pub size: [u32; 2],
    pub scale: f32,
    pub go: Option<String>,
    pub select: Option<String>,
    pub run_ms: u64,
    pub tab: Option<String>,
}

impl SnapshotArgs {
    pub fn parse(args: &[String]) -> Option<Self> {
        let i = args.iter().position(|a| a == "--ui-snapshot")?;
        let mut s = Self {
            out: args.get(i + 1)?.into(),
            show: None,
            size: [1600, 1000],
            scale: 1.0,
            go: None,
            select: None,
            run_ms: 1500,
            tab: None,
        };
        let val = |flag: &str| args.iter().position(|a| a == flag).and_then(|j| args.get(j + 1)).cloned();
        s.show = val("--show").map(PathBuf::from);
        if let Some((Some(w), Some(h))) = val("--size").and_then(|v| v.split_once('x').map(|(w, h)| (w.parse().ok(), h.parse().ok()))) {
            s.size = [w, h];
        }
        s.scale = val("--scale").and_then(|v| v.parse().ok()).unwrap_or(1.0);
        s.go = val("--go");
        s.select = val("--select");
        s.tab = val("--tab");
        s.run_ms = val("--run-ms").and_then(|v| v.parse().ok()).unwrap_or(1500);
        Some(s)
    }
}

pub fn run(a: SnapshotArgs) -> anyhow::Result<()> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let gpu = pollster::block_on(GpuContext::new(instance, None))?;
    let pipes = Pipelines::new(&gpu.device);
    let bundle = match &a.show {
        Some(p) => ProjectBundle::open(p)?,
        None => ProjectBundle::open(std::env::current_dir()?.join("shows/Demo Show.livestage")).context("demo show")?,
    };
    let mut rt = ShowRuntime::new(bundle);
    if let Some(n) = &a.go {
        rt.command(ControlSource::Ui, ShowCommand::GoCue { number: n.clone() });
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
    ui.selected = a
        .select
        .as_deref()
        .and_then(|n| rt.bundle.project.active_list().and_then(|l| l.cues.iter().find(|c| c.header().number == n)))
        .map(|c| c.header().id);

    // Program canvases for previews / multiview.
    let mut displays = DisplayManager::default();
    let mut compositor = livestage_video::Compositor::default();
    let root = rt.bundle.root().to_path_buf();
    let resolve = |x: &livestage_core::project::AssetRef| livestage_core::bundle::resolve_asset(&root, x).ok();
    for (at, action) in rt.video_receiver().try_iter() {
        compositor.handle(at, &action, &resolve);
    }
    let project_displays = rt.bundle.project.displays.clone();
    displays.sync_slots(&gpu, &pipes, &project_displays, CANVAS_FORMAT);
    displays.render(&gpu, &pipes, &mut compositor, rt.now(), &project_displays, &[]);

    let ctx = egui::Context::default();
    crate::theme::apply(&ctx);
    let mut renderer = egui_wgpu::Renderer::new(&gpu.device, CANVAS_FORMAT, None, 1, false);
    let mut previews = Previews::default();
    for slot in displays.slots.values() {
        let tex = renderer.register_native_texture(&gpu.device, slot.renderer.canvas_view(), wgpu::FilterMode::Linear);
        previews.insert(slot.id, (tex, slot.renderer.canvas_size()));
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
    let mut output = None;
    for i in 0..4 {
        let mut out = ctx.run(input(i as f64 * 0.016), |ctx| panels::draw(ctx, &mut rt, &mut ui, &displays.physical, &previews, &ndi));
        textures.append(std::mem::take(&mut out.textures_delta));
        output = Some(out);
    }
    let mut output = output.expect("ran");
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

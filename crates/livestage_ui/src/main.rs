//! LiveStage — operator application.
//!
//! ```text
//! livestage                      open ./shows/Demo Show.livestage (created on first run)
//! livestage <Show.livestage>     open a bundle
//! livestage --new <dir> <name>   create an empty bundle and open it
//! ```

mod demo;
mod display;
mod gfx;
mod panels;
mod priority;
mod runtime;
mod show_thread;
mod snapshot;
mod undo;
mod theme;

use anyhow::Context as _;
use display::DisplayManager;
use gfx::ControlWindow;
use livestage_control::{ControlSource, ShowCommand};
use livestage_core::ProjectBundle;
use livestage_video::{GpuContext, Pipelines};
use panels::{Previews, UiState};
use livestage_video::Compositor;
use runtime::{LogLevel, ShowRuntime, VideoFinished, VideoMsg};
use show_thread::{SharedRuntime, ShowThread};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

enum Launch {
    Default,
    Open(PathBuf),
    New { dir: PathBuf, name: String },
}

fn parse_args() -> Launch {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag, dir, name] if flag == "--new" => Launch::New { dir: dir.into(), name: name.clone() },
        [path] => Launch::Open(path.into()),
        _ => Launch::Default,
    }
}

fn load_bundle(launch: &Launch) -> anyhow::Result<ProjectBundle> {
    Ok(match launch {
        Launch::Open(p) => ProjectBundle::open(p).with_context(|| format!("opening {}", p.display()))?,
        Launch::New { dir, name } => ProjectBundle::create(dir, name)?,
        Launch::Default => {
            let shows = std::env::current_dir()?.join("shows");
            let demo = shows.join("Demo Show.livestage");
            if demo.exists() {
                ProjectBundle::open(&demo)?
            } else {
                std::fs::create_dir_all(&shows)?;
                tracing::info!("creating demo show in {}", shows.display());
                demo::create_demo(&shows)?
            }
        }
    })
}

const AUTOSAVE_EVERY: Duration = Duration::from_secs(30);

struct Gfx {
    gpu: Arc<GpuContext>,
    pipes: Pipelines,
    control: ControlWindow,
}

struct App {
    launch: Launch,
    gfx: Option<Gfx>,
    /// Shared with the show thread; the UI locks it only while laying out panels.
    rt: Option<SharedRuntime>,
    show_thread: Option<ShowThread>,
    /// Render-thread state: the compositor and its inputs from the show thread.
    compositor: Compositor,
    video_rx: Option<crossbeam_channel::Receiver<VideoMsg>>,
    video_finished: VideoFinished,
    bundle_root: PathBuf,
    epoch: Instant,
    displays: DisplayManager,
    ui: UiState,
    previews: Previews,
    last_scan: Instant,
    last_autosave: Instant,
    autosaved_generation: u64,
}

impl App {
    fn init(&mut self, el: &ActiveEventLoop) -> anyhow::Result<()> {
        let attrs = Window::default_attributes()
            .with_title("LiveStage")
            .with_inner_size(winit::dpi::LogicalSize::new(1600.0, 1000.0))
            .with_min_inner_size(winit::dpi::LogicalSize::new(1024.0, 640.0));
        let window = Arc::new(el.create_window(attrs)?);
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance.create_surface(window.clone())?;
        let gpu = pollster::block_on(GpuContext::new(instance, Some(&surface)))?;
        let pipes = Pipelines::new(&gpu.device);
        let control = ControlWindow::new(&gpu, window, surface);
        self.displays.refresh_monitors(el);
        let bundle = load_bundle(&self.launch)?;
        control.window.set_title(&format!("LiveStage — {}", bundle.project.name));
        if let Some(when) = bundle.recovery_available() {
            let when: chrono::DateTime<chrono::Local> = when.into();
            self.ui.recovery = Some(when.format("%H:%M:%S on %d %b %Y").to_string());
        }
        let rt = ShowRuntime::new(bundle);
        self.bundle_root = rt.bundle.root().to_path_buf();
        self.epoch = rt.epoch();
        self.video_rx = Some(rt.video_receiver());
        self.video_finished = rt.video_finished.clone();
        let shared: SharedRuntime = Arc::new(parking_lot::Mutex::new(rt));
        self.show_thread = Some(ShowThread::spawn(shared.clone(), show_thread::SHOW_RATE_HZ)?);
        self.rt = Some(shared);
        self.gfx = Some(Gfx { gpu, pipes, control });
        Ok(())
    }

    fn now(&self) -> livestage_core::ShowTime {
        self.epoch.elapsed().into()
    }

    fn frame(&mut self, el: &ActiveEventLoop) {
        let now = self.now();
        if let Some(shared) = self.rt.clone() {
            self.autosave(&shared);
        }
        let (Some(gfx), Some(shared)) = (self.gfx.as_mut(), self.rt.clone()) else { return };

        // 1. Apply video actions from the show thread, at the time they were dispatched.
        if let Some(rx) = &self.video_rx {
            let root = &self.bundle_root;
            let resolve = |a: &livestage_core::project::AssetRef| livestage_core::bundle::resolve_asset(root, a).ok();
            for (at, action) in rx.try_iter() {
                self.compositor.handle(at, &action, &resolve);
            }
        }
        self.compositor.update(now);
        {
            let mut finished = self.video_finished.lock();
            finished.clone_from(self.compositor.finished_cues());
        }

        // 2. Short critical section: read what the render pass needs.
        let (displays, pixel_sources, live_request) = {
            let mut rt = shared.lock();
            let live = rt.outputs_live_request.take();
            let pixel: Vec<_> = rt.bundle.project.dmx.pixel_maps.iter().filter(|m| m.enabled).map(|m| m.source).collect();
            (rt.bundle.project.displays.clone(), pixel, live)
        };
        self.displays.sync_slots(&gfx.gpu, &gfx.pipes, &displays, gfx.control.format);
        if let Some(live) = live_request {
            self.displays.set_live(el, &gfx.gpu, &displays, live);
            let mut rt = shared.lock();
            rt.outputs_live = live;
            rt.log(LogLevel::Info, if live { "program outputs LIVE" } else { "program outputs closed" });
        }

        // 3. GPU work and presentation happen without holding the runtime lock.
        let readbacks = self.displays.render(&gfx.gpu, &gfx.pipes, &mut self.compositor, now, &displays, &pixel_sources);
        if !readbacks.is_empty() {
            let mut rt = shared.lock();
            for (id, w, h, px) in readbacks {
                rt.dmx.feed_canvas(id, w, h, &px);
            }
        }

        // Keep live canvas previews registered with the control renderer.
        let renderer = &mut gfx.control.renderer;
        self.previews.retain(|id, (tex, _)| {
            let keep = self.displays.slots.contains_key(id);
            if !keep {
                renderer.free_texture(tex);
            }
            keep
        });
        for slot in self.displays.slots.values_mut() {
            let generation = slot.renderer.canvas_generation();
            let tex = match slot.preview {
                Some((tex, g)) if g == generation => tex,
                Some((tex, _)) => {
                    renderer.update_egui_texture_from_wgpu_texture(&gfx.gpu.device, slot.renderer.canvas_view(), wgpu::FilterMode::Linear, tex);
                    tex
                }
                None => renderer.register_native_texture(&gfx.gpu.device, slot.renderer.canvas_view(), wgpu::FilterMode::Linear),
            };
            slot.preview = Some((tex, generation));
            self.previews.insert(slot.id, (tex, slot.renderer.canvas_size()));
        }

        // 4. Panels: the lock is held only for egui layout; tessellation and
        //    presentation run after it is released.
        let ndi: panels::NdiStatus =
            self.displays.slots.values().filter_map(|s| s.ndi.as_ref().map(|n| (s.id, n.connections()))).collect();
        let (ui, physical, previews) = (&mut self.ui, &self.displays.physical, &self.previews);
        gfx.control.frame(&gfx.gpu, |ctx| {
            let mut rt = shared.lock();
            panels::draw(ctx, &mut rt, ui, physical, previews, &ndi);
        });
    }

    /// Every 30 s, snapshots a dirty project and writes the recovery copy on a
    /// background thread (disk I/O never runs on the show or UI thread).
    fn autosave(&mut self, shared: &SharedRuntime) {
        if self.last_autosave.elapsed() < AUTOSAVE_EVERY {
            return;
        }
        self.last_autosave = Instant::now();
        let snapshot = {
            let rt = shared.lock();
            (rt.dirty && rt.edit_generation != self.autosaved_generation).then(|| (rt.bundle.project.clone(), rt.edit_generation))
        };
        if let Some((project, generation)) = snapshot {
            self.autosaved_generation = generation;
            let root = self.bundle_root.clone();
            std::thread::spawn(move || match livestage_core::bundle::write_autosave(&root, &project) {
                Ok(()) => tracing::debug!("autosaved"),
                Err(e) => tracing::warn!("autosave failed: {e}"),
            });
        }
    }

    fn output_window_event(&mut self, id: WindowId, event: WindowEvent) {
        let (Some(gfx), Some(shared)) = (self.gfx.as_ref(), self.rt.clone()) else { return };
        let Some(slot) = self.displays.slot_for_window(id) else { return };
        let Some(w) = slot.window.as_mut() else { return };
        match event {
            WindowEvent::Resized(s) => w.resize(&gfx.gpu, s.width, s.height),
            WindowEvent::CursorEntered { .. } | WindowEvent::Focused(true) | WindowEvent::Occluded(false) => w.reassert(),
            // Kiosk outputs cannot be closed by the OS (Cmd+W / Alt+F4); use "Outputs off".
            WindowEvent::CloseRequested if w.kiosk => shared.lock().log(LogLevel::Warn, "close request on a kiosk output ignored"),
            WindowEvent::CloseRequested => slot.window = None,
            // An output that grabbed focus must still obey the show keys.
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed && !event.repeat => {
                let rt = shared.lock();
                match event.logical_key {
                    Key::Named(NamedKey::Space) => rt.command(ControlSource::Keyboard, ShowCommand::Go { list: None }),
                    Key::Named(NamedKey::Escape) => rt.command(ControlSource::Keyboard, ShowCommand::Panic { hard: false }),
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        if let Err(e) = self.init(el) {
            tracing::error!("startup failed: {e:#}");
            eprintln!("LiveStage failed to start: {e:#}");
            el.exit();
        }
        el.set_control_flow(ControlFlow::Poll);
    }

    fn window_event(&mut self, el: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let is_control = self.gfx.as_ref().is_some_and(|g| g.control.window.id() == id);
        if !is_control {
            return self.output_window_event(id, event);
        }
        if let Some(g) = self.gfx.as_mut() {
            let resp = g.control.state.on_window_event(&g.control.window, &event);
            if resp.repaint {
                g.control.window.request_redraw();
            }
        }
        match event {
            WindowEvent::CloseRequested => {
                if let Some(shared) = self.rt.clone() {
                    let mut rt = shared.lock();
                    if rt.dirty {
                        rt.save();
                    }
                    rt.command(ControlSource::Ui, ShowCommand::Panic { hard: true });
                    rt.tick();
                }
                // Joins the show thread (engines stop when the runtime drops).
                self.show_thread = None;
                el.exit();
            }
            WindowEvent::Resized(s) => {
                if let Some(g) = self.gfx.as_mut() {
                    g.control.resize(&g.gpu, s.width, s.height);
                }
            }
            WindowEvent::RedrawRequested => self.frame(el),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if self.last_scan.elapsed() > Duration::from_secs(2) {
            self.displays.refresh_monitors(el);
            self.last_scan = Instant::now();
        }
        if let Some(g) = &self.gfx {
            g.control.window.request_redraw();
        }
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,wgpu_core=warn,wgpu_hal=warn,naga=warn".into()),
        )
        .init();
    let args: Vec<String> = std::env::args().collect();
    if let Some(snap) = snapshot::SnapshotArgs::parse(&args) {
        return snapshot::run(snap);
    }
    let event_loop = EventLoop::new()?;
    let mut app = App {
        launch: parse_args(),
        gfx: None,
        rt: None,
        show_thread: None,
        compositor: Compositor::default(),
        video_rx: None,
        video_finished: VideoFinished::default(),
        bundle_root: PathBuf::new(),
        epoch: Instant::now(),
        displays: DisplayManager::default(),
        ui: UiState::default(),
        previews: Previews::default(),
        last_scan: Instant::now(),
        last_autosave: Instant::now(),
        autosaved_generation: 0,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}

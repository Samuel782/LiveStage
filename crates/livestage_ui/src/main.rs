//! LiveStage — operator application.
//!
//! ```text
//! livestage                      open ./shows/Demo Show.livestage (created on first run)
//! livestage <Show.livestage>     open a bundle
//! livestage --new <dir> <name>   create an empty bundle and open it
//! livestage --check-media        report movie playback support and exit
//! ```

mod demo;
mod display;
mod gfx;
mod health;
mod logging;
mod panels;
mod prefs;
mod priority;
mod runtime;
mod show_thread;
mod snapshot;
mod undo;
mod view;
mod theme;
mod thumbs;

use anyhow::Context as _;
use display::{CanvasPreview, Monitors, OutputCmd, OutputThread, ProgramWindow};
use gfx::ControlWindow;
use health::{Heartbeat, Watchdog, Watched};
use livestage_control::{ControlSource, ShowCommand};
use livestage_core::ProjectBundle;
use livestage_core::OutputId;
use livestage_video::GpuContext;
use panels::{Previews, UiState};
use runtime::{LogLevel, ShowRuntime};
use std::collections::HashMap;
use show_thread::{SharedRuntime, ShowThread};
use view::UiRuntime;
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

fn load_bundle(launch: &Launch, startup_show: Option<&std::path::Path>) -> anyhow::Result<ProjectBundle> {
    Ok(match launch {
        Launch::Open(p) => ProjectBundle::open(p).with_context(|| format!("opening {}", p.display()))?,
        Launch::New { dir, name } => ProjectBundle::create(dir, name)?,
        Launch::Default => match startup_show.filter(|p| p.join(livestage_core::bundle::MANIFEST_FILE).is_file()) {
            Some(p) => ProjectBundle::open(p).with_context(|| format!("opening {}", p.display()))?,
            None => {
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
        },
    })
}


struct Gfx {
    gpu: Arc<GpuContext>,
    control: ControlWindow,
}

struct App {
    launch: Launch,
    gfx: Option<Gfx>,
    /// Shared with the show thread; the UI locks it only while laying out panels.
    rt: Option<SharedRuntime>,
    /// The UI's frame copy of the runtime (kept to reuse its allocations).
    view: Option<UiRuntime>,
    show_thread: Option<ShowThread>,
    /// Renders and presents every program output (see [`display`]).
    output_thread: Option<OutputThread>,
    bundle_root: PathBuf,
    monitors: Monitors,
    /// Program windows currently open (main-thread handles; surfaces live on the output thread).
    program_windows: HashMap<OutputId, ProgramWindow>,
    ui: UiState,
    previews: Previews,
    /// Canvas generation each preview texture was registered for.
    registered: HashMap<OutputId, u64>,
    last_scan: Instant,
    /// Watches the show, output and UI loops and the audio device; autosaves.
    watchdog: Option<Watchdog>,
    /// Beaten by the event loop (UI freezes go to the log only).
    ui_heartbeat: Arc<Heartbeat>,
    _ui_running: Option<health::Running>,
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
        let control = ControlWindow::new(&gpu, window, surface);
        panels::settings::apply_prefs(&control.ctx, &self.ui.prefs);
        self.monitors.refresh(el);
        let bundle = load_bundle(&self.launch, self.ui.prefs.startup_show.as_deref())?;
        control.window.set_title(&format!("LiveStage — {}", bundle.project.name));
        let previous = bundle.begin_session();
        let other_instance = previous.as_ref().filter(|p| health::process_alive(p.pid));
        if previous.is_some() && other_instance.is_none() {
            let started = previous.as_ref().and_then(|p| chrono::DateTime::from_timestamp(p.started_at as i64, 0));
            let started = started.map(|t| t.with_timezone(&chrono::Local).format(" (opened %H:%M on %d %b)").to_string());
            self.ui.unclean_exit = Some(format!("The last session{} did not shut down cleanly.", started.unwrap_or_default()));
        }
        if let Some(when) = bundle.recovery_available() {
            let when: chrono::DateTime<chrono::Local> = when.into();
            self.ui.recovery = Some(when.format("%H:%M:%S on %d %b %Y").to_string());
        }
        let backup = bundle.opened_from_backup().map(str::to_string);
        let mut rt = ShowRuntime::new(bundle);
        if let Some(p) = other_instance {
            rt.log(LogLevel::Warn, format!("this show is also open in another LiveStage (pid {}): saves will overwrite each other", p.pid));
        }
        if let Some(exit) = &self.ui.unclean_exit {
            rt.log(LogLevel::Warn, exit.clone());
        }
        if let Some(reason) = backup {
            rt.log(LogLevel::Error, format!("project.json could not be read ({reason}); opened the backup from the save before. Save to repair it."));
        }
        rt.office_path = self.ui.prefs.office_path.clone();
        let media = livestage_video::capabilities::movie_support();
        match media.warning() {
            Some(w) => rt.log(runtime::LogLevel::Warn, w),
            None => rt.log(runtime::LogLevel::Info, media.summary()),
        }
        if self.ui.prefs.outputs_on_launch {
            rt.outputs_live_request = Some(true);
        }
        self.bundle_root = rt.bundle.root().to_path_buf();
        let (epoch, video_rx, video_finished) = (rt.epoch(), rt.video_receiver(), rt.video_finished.clone());
        let shared: SharedRuntime = Arc::new(parking_lot::Mutex::new(rt));
        self.show_thread = Some(ShowThread::spawn(shared.clone(), show_thread::SHOW_RATE_HZ)?);
        self.output_thread = Some(OutputThread::spawn(
            gpu.clone(),
            shared.clone(),
            video_rx,
            video_finished,
            epoch,
            self.bundle_root.clone(),
            control.format,
        )?);
        let watched = vec![
            Watched::new(self.show_thread.as_ref().map(|s| s.heartbeat.clone()).unwrap_or_else(|| Heartbeat::new("show")), Duration::from_millis(100), true),
            Watched::new(self.output_thread.as_ref().map(|o| o.heartbeat.clone()).unwrap_or_else(|| Heartbeat::new("output")), Duration::from_millis(500), true),
            Watched::new(self.ui_heartbeat.clone(), Duration::from_secs(2), false),
        ];
        self._ui_running = Some(self.ui_heartbeat.enter());
        self.watchdog = Some(Watchdog::spawn(shared.clone(), watched, Default::default())?);
        self.rt = Some(shared);
        self.gfx = Some(Gfx { gpu, control });
        Ok(())
    }

    /// Opens or closes program windows. When `live`, mapped displays go kiosk
    /// fullscreen; unmapped ones open as floating preview windows.
    fn set_live(&mut self, el: &ActiveEventLoop, displays: &[livestage_core::routing::LogicalDisplay], live: bool) {
        let (Some(gfx), Some(out)) = (self.gfx.as_ref(), self.output_thread.as_ref()) else { return };
        if !live {
            for id in self.program_windows.keys() {
                out.send(OutputCmd::Close(*id));
            }
            // Main-thread handles go now; the output thread returns its clones
            // once the surfaces are dropped.
            self.program_windows.clear();
            return;
        }
        for d in displays.iter().filter(|d| d.enabled) {
            if self.program_windows.contains_key(&d.id) {
                continue;
            }
            let monitor = d.physical.as_ref().and_then(|k| self.monitors.monitor_for(k)).cloned();
            let opts = display::WindowOpts {
                always_on_top: self.ui.prefs.outputs_always_on_top,
                hide_cursor: self.ui.prefs.hide_cursor_on_outputs,
            };
            match display::open_program_window(el, &gfx.gpu, d, monitor.as_ref(), opts) {
                Ok((pw, surface, config)) => {
                    out.send(OutputCmd::Open { output: d.id, window: pw.window.clone(), surface, config });
                    self.program_windows.insert(d.id, pw);
                }
                Err(e) => tracing::error!(output = %d.name, "cannot open program window: {e}"),
            }
        }
    }

    fn frame(&mut self, el: &ActiveEventLoop) {
        let Some(shared) = self.rt.clone() else { return };
        if let Some(w) = &self.watchdog {
            use std::sync::atomic::Ordering;
            let st = &w.state;
            st.autosave_secs.store(self.ui.prefs.autosave_secs, Ordering::Relaxed);
            let (show, out) = (self.show_thread.as_ref().map(|s| &s.heartbeat), self.output_thread.as_ref().map(|o| &o.heartbeat));
            self.ui.watch = panels::WatchView {
                show_stalled: show.is_some_and(|h| h.is_stalled()),
                output_stalled: out.is_some_and(|h| h.is_stalled()),
                audio_stalled: st.audio_stalled.load(Ordering::Relaxed),
                stalls: show.map_or(0, |h| h.stalls()) + out.map_or(0, |h| h.stalls()),
                faults: show.map_or(0, |h| h.faults()) + out.map_or(0, |h| h.faults()),
                autosave_failures: st.autosave_failures.load(Ordering::Relaxed),
            };
        }

        // 1. Program windows requested by commands (UI, OSC, MIDI, cues).
        let live_request = {
            let mut rt = shared.lock();
            rt.outputs_live_request.take().map(|live| (live, rt.bundle.project.displays.clone()))
        };
        if let Some((live, displays)) = live_request {
            self.set_live(el, &displays, live);
            let mut rt = shared.lock();
            rt.outputs_live = live;
            rt.log(LogLevel::Info, if live { "program outputs LIVE" } else { "program outputs closed" });
        }
        if let Some(out) = &self.output_thread {
            // Windows released by the output thread are dropped here, on the main thread.
            for w in out.returned.try_iter() {
                drop(w);
            }
        }
        let Some(gfx) = self.gfx.as_mut() else { return };

        // 2. Keep live canvas previews registered with the control renderer.
        let (canvases, ndi): (HashMap<OutputId, CanvasPreview>, panels::NdiStatus) = match &self.output_thread {
            Some(out) => {
                use std::sync::atomic::Ordering;
                self.ui.output_fps = out.shared.fps_milli.load(Ordering::Relaxed) as f32 / 1000.0;
                self.ui.av_sync = panels::AvSync {
                    layers: out.shared.sync_layers.load(Ordering::Relaxed),
                    worst_ms: out.shared.sync_error_us.load(Ordering::Relaxed) as f32 / 1000.0,
                    snaps: out.shared.sync_snaps.load(Ordering::Relaxed),
                };
                (out.shared.previews.lock().clone(), out.shared.ndi.lock().clone())
            }
            None => Default::default(),
        };
        let renderer = &mut gfx.control.renderer;
        let registered = &mut self.registered;
        self.previews.retain(|id, (tex, _)| {
            let keep = canvases.contains_key(id);
            if !keep {
                renderer.free_texture(tex);
                registered.remove(id);
            }
            keep
        });
        for (id, c) in &canvases {
            match self.previews.get_mut(id) {
                Some((tex, size)) => {
                    if registered.get(id) != Some(&c.generation) {
                        renderer.update_egui_texture_from_wgpu_texture(&gfx.gpu.device, &c.view, wgpu::FilterMode::Linear, *tex);
                        registered.insert(*id, c.generation);
                    }
                    *size = c.size;
                }
                None => {
                    let tex = renderer.register_native_texture(&gfx.gpu.device, &c.view, wgpu::FilterMode::Linear);
                    registered.insert(*id, c.generation);
                    self.previews.insert(*id, (tex, c.size));
                }
            }
        }

        // 3. Panels lay out against a copy of the runtime (crate::view): the
        //    lock is held only to copy state in and to fold edits back, never
        //    during egui layout, tessellation or presentation.
        let view = self.view.get_or_insert_with(|| UiRuntime::new(&shared.lock()));
        view.sync(&shared.lock());
        view.begin_frame();
        let (ui, physical, previews) = (&mut self.ui, &self.monitors.physical, &self.previews);
        gfx.control.frame(&gfx.gpu, |ctx| panels::draw(ctx, view, ui, physical, previews, &ndi));
        let edits = view.take_edits();
        view.apply(&mut shared.lock(), edits);

        // 4. Deferred work that must not run under the runtime lock.
        let restart = {
            let mut rt = shared.lock();
            std::mem::take(&mut rt.restart_control_requested).then(|| rt.take_control_servers())
        };
        if let Some(old) = restart {
            drop(old); // joins the old server threads and frees their ports
            shared.lock().start_control_services();
        }
        if std::mem::take(&mut self.ui.quit_requested) {
            self.quit(el);
        }
    }

    /// Saves, panics everything and shuts the threads down in order.
    fn quit(&mut self, el: &ActiveEventLoop) {
        // First, so the shutdown itself is not reported as stalls (it logs a
        // final health summary and finishes an autosave in progress).
        self.watchdog = None;
        if let Some(shared) = self.rt.clone() {
            let mut rt = shared.lock();
            if rt.dirty {
                rt.save();
            }
            rt.command(ControlSource::Ui, ShowCommand::Panic { hard: true });
            rt.tick();
            rt.bundle.end_session();
            tracing::info!("clean shutdown");
        }
        // Joins the output thread (it hands its windows back), then the
        // show thread (engines stop when the runtime drops).
        self.output_thread = None;
        self.program_windows.clear();
        self.show_thread = None;
        el.exit();
    }

    fn output_window_event(&mut self, id: WindowId, event: WindowEvent) {
        let Some(shared) = self.rt.clone() else { return };
        let Some((&output, w)) = self.program_windows.iter().find(|(_, w)| w.window.id() == id) else { return };
        match event {
            WindowEvent::Resized(s) => {
                if let Some(out) = &self.output_thread {
                    out.send(OutputCmd::Resized { window: id, size: [s.width, s.height] });
                }
            }
            WindowEvent::CursorEntered { .. } | WindowEvent::Focused(true) | WindowEvent::Occluded(false) => w.reassert(),
            // Kiosk outputs cannot be closed by the OS (Cmd+W / Alt+F4); use "Outputs off".
            WindowEvent::CloseRequested if w.kiosk => shared.lock().log(LogLevel::Warn, "close request on a kiosk output ignored"),
            WindowEvent::CloseRequested => {
                if let Some(out) = &self.output_thread {
                    out.send(OutputCmd::Close(output));
                }
                self.program_windows.remove(&output);
            }
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
                let live = self.rt.as_ref().is_some_and(|r| r.lock().outputs_live);
                if live && self.ui.prefs.confirm_quit_when_live {
                    self.ui.confirm_quit = true;
                } else {
                    self.quit(el);
                }
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
        self.ui_heartbeat.beat();
        if health::stop_requested() {
            tracing::info!("stop signal received");
            return self.quit(el);
        }
        if self.last_scan.elapsed() > Duration::from_secs(2) {
            self.monitors.refresh(el);
            self.last_scan = Instant::now();
        }
        if let Some(g) = &self.gfx {
            g.control.window.request_redraw();
        }
    }
}

/// `--check-media`: prints what movie playback can do and exits non-zero
/// when movies cannot play or an essential codec is missing (for release
/// smoke tests and support).
fn check_media() -> i32 {
    use livestage_video::capabilities::{movie_support, MovieSupport};
    let s = movie_support();
    println!("{}", s.summary());
    if let MovieSupport::Ready { missing_optional, .. } = s {
        if !missing_optional.is_empty() {
            println!("optional codecs not available: {}", missing_optional.join(", "));
        }
    }
    if let Some(w) = s.warning() {
        println!("warning: {w}");
    }
    match s {
        MovieSupport::Ready { missing, .. } if missing.is_empty() => 0,
        _ => 1,
    }
}

fn main() -> anyhow::Result<()> {
    let log_file = logging::init();
    health::install_panic_hook(logging::log_dir());
    health::install_stop_signals();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        pid = std::process::id(),
        log = %log_file.as_deref().map_or("(stdout only)".into(), |p| p.display().to_string()),
        "LiveStage starting"
    );
    let args: Vec<String> = std::env::args().collect();
    if let Some(snap) = snapshot::SnapshotArgs::parse(&args) {
        return snapshot::run(snap);
    }
    let prefs = prefs::Preferences::load();
    prefs.export_env();
    if args.iter().any(|a| a == "--check-media") {
        std::process::exit(check_media());
    }
    let event_loop = EventLoop::new()?;
    let mut app = App {
        launch: parse_args(),
        gfx: None,
        rt: None,
        view: None,
        show_thread: None,
        output_thread: None,
        bundle_root: PathBuf::new(),
        monitors: Monitors::default(),
        program_windows: HashMap::new(),
        ui: UiState { prefs, ..UiState::default() },
        previews: Previews::default(),
        registered: HashMap::new(),
        last_scan: Instant::now(),
        watchdog: None,
        ui_heartbeat: Heartbeat::new("ui"),
        _ui_running: None,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}

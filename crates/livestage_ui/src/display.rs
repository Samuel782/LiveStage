//! Display routing manager: physical monitor discovery, per-output canvases,
//! hardened kiosk fullscreen program windows and the output render thread.
//!
//! ```text
//!  main thread (winit)                    livestage-output thread
//!  ───────────────────                    ───────────────────────
//!  monitor discovery                      Compositor (video actions)
//!  create window + surface ── Open ──▶    OutputEngine: per output
//!  Resized / Close         ── cmds ──▶      layers → canvas → warp → surface
//!  drop returned windows   ◀─ Arc<Window>   NDI / pixel-map readbacks
//!  egui previews           ◀─ canvases ─    (paced by vsync of the outputs)
//! ```
//!
//! Program windows are rendered and presented on their own thread, so an
//! operator window that is occluded (a kiosk output covering it), minimised,
//! being dragged or blocked on its own vsync never freezes or blacks out the
//! outputs.

use crate::gfx::OverlayPainter;
use crate::show_thread::SharedRuntime;
use livestage_core::routing::{LogicalDisplay, PhysicalDisplayInfo, PhysicalDisplayKey};
use livestage_audio::VoiceClock;
use livestage_core::{CueId, OutputId, ShowTime};
use livestage_video::{Compositor, GpuContext, MasterClock, OutputRenderer, Pipelines};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use winit::dpi::{LogicalPosition, LogicalSize, PhysicalSize};
use winit::event_loop::ActiveEventLoop;
use winit::monitor::MonitorHandle;
#[cfg(not(target_os = "macos"))]
use winit::window::Fullscreen;
use winit::window::{Window, WindowId, WindowLevel};

pub fn describe_monitors(monitors: &[MonitorHandle], primary: Option<&MonitorHandle>) -> Vec<PhysicalDisplayInfo> {
    monitors
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let pos = m.position();
            let size = m.size();
            PhysicalDisplayInfo {
                name: m.name().unwrap_or_else(|| format!("Display {}", i + 1)),
                position: [pos.x, pos.y],
                size: [size.width, size.height],
                refresh_millihertz: m.refresh_rate_millihertz(),
                scale_factor_milli: (m.scale_factor() * 1000.0) as u32,
                is_primary: primary.is_some_and(|p| p == m),
            }
        })
        .collect()
}

// ============================================================ main thread ==

/// Connected monitors (main thread only: `MonitorHandle` queries go through winit).
#[derive(Default)]
pub struct Monitors {
    pub handles: Vec<MonitorHandle>,
    pub physical: Vec<PhysicalDisplayInfo>,
}

impl Monitors {
    pub fn refresh(&mut self, el: &ActiveEventLoop) {
        let handles: Vec<MonitorHandle> = el.available_monitors().collect();
        let primary = el.primary_monitor();
        let physical = describe_monitors(&handles, primary.as_ref());
        if physical != self.physical {
            tracing::info!(count = physical.len(), "displays changed");
            self.physical = physical;
        }
        self.handles = handles;
    }

    pub fn monitor_for(&self, key: &PhysicalDisplayKey) -> Option<&MonitorHandle> {
        key.best_match(&self.physical).and_then(|i| self.handles.get(i))
    }
}

/// Kiosk behaviour (from Preferences).
#[derive(Clone, Copy, Debug)]
pub struct WindowOpts {
    pub always_on_top: bool,
    pub hide_cursor: bool,
}

/// A program window as seen by the main thread (events, kiosk invariants).
pub struct ProgramWindow {
    pub window: Arc<Window>,
    pub kiosk: bool,
    opts: WindowOpts,
}

impl ProgramWindow {
    /// Re-applies kiosk invariants (called on focus / cursor-enter events,
    /// because some window managers restore the cursor or chrome).
    pub fn reassert(&self) {
        if self.kiosk {
            self.window.set_cursor_visible(!self.opts.hide_cursor);
            if self.opts.always_on_top {
                self.window.set_window_level(WindowLevel::AlwaysOnTop);
            }
        }
    }
}

/// Where a kiosk window must be placed, in the monitor's own logical
/// coordinates. winit converts a *physical* position with the scale factor
/// of the display the window is created on (usually the main one), which
/// puts windows for a monitor with a different scale on the wrong screen;
/// logical coordinates are unambiguous.
fn kiosk_geometry(m: &MonitorHandle) -> (LogicalPosition<f64>, LogicalSize<f64>) {
    let scale = m.scale_factor();
    (m.position().to_logical(scale), m.size().to_logical(scale))
}

/// Creates a program window (and its surface) for `display`. Must run on the
/// main thread; the surface is then handed to the output thread.
pub fn open_program_window(
    el: &ActiveEventLoop,
    gpu: &GpuContext,
    display: &LogicalDisplay,
    monitor: Option<&MonitorHandle>,
    opts: WindowOpts,
) -> anyhow::Result<(ProgramWindow, wgpu::Surface<'static>, wgpu::SurfaceConfiguration)> {
    let kiosk = monitor.is_some() && display.fullscreen;
    let mut attrs = Window::default_attributes().with_title(format!("LiveStage — {}", display.name));
    match monitor.filter(|_| kiosk) {
        Some(m) => {
            let (pos, size) = kiosk_geometry(m);
            attrs = attrs
                .with_decorations(false)
                .with_resizable(false)
                .with_window_level(if opts.always_on_top { WindowLevel::AlwaysOnTop } else { WindowLevel::Normal })
                .with_position(pos)
                .with_inner_size(size);
            #[cfg(not(target_os = "macos"))]
            {
                attrs = attrs.with_fullscreen(Some(Fullscreen::Borderless(Some(m.clone()))));
            }
        }
        None => {
            let [w, h] = display.canvas_size.unwrap_or([1920, 1080]);
            attrs = attrs.with_inner_size(PhysicalSize::new(w / 2, h / 2));
            if let Some(m) = monitor {
                let (pos, _) = kiosk_geometry(m);
                attrs = attrs.with_position(LogicalPosition::new(pos.x + 40.0, pos.y + 40.0));
            }
        }
    }
    let window = Arc::new(el.create_window(attrs)?);
    if kiosk {
        harden(&window, monitor, opts);
    }
    let surface = gpu.instance.create_surface(window.clone())?;
    let s = window.inner_size();
    let size = [s.width.max(1), s.height.max(1)];
    // Capabilities are queried and the swapchain configured here, on the main
    // thread (Metal reads the view's geometry, which is main-thread only);
    // the output thread then only re-applies this configuration.
    let format = gpu.surface_format(&surface);
    // Two frames in flight: the output thread never stalls a whole vsync on
    // one window while another is waiting.
    let config = gpu.surface_config(&surface, format, size, true, 2);
    surface.configure(&gpu.device, &config);
    let on = window.current_monitor().and_then(|m| m.name()).unwrap_or_default();
    let name = &display.name;
    tracing::info!(output = %name, kiosk, ?size, screen = %on, "program window open");
    Ok((ProgramWindow { window, kiosk, opts }, surface, config))
}

/// Kiosk mode: no decorations, no cursor, always on top, true fullscreen.
fn harden(window: &Window, monitor: Option<&MonitorHandle>, opts: WindowOpts) {
    window.set_cursor_visible(!opts.hide_cursor);
    window.set_decorations(false);
    if opts.always_on_top {
        window.set_window_level(WindowLevel::AlwaysOnTop);
    }
    #[cfg(target_os = "macos")]
    {
        // Make sure the window really sits on the target screen before going
        // fullscreen: "simple" fullscreen fills whichever screen the window is on.
        if let Some(m) = monitor {
            if window.current_monitor().as_ref() != Some(m) {
                let (pos, size) = kiosk_geometry(m);
                window.set_outer_position(pos);
                let _ = window.request_inner_size(size);
            }
        }
        // "Simple" fullscreen stays on the window's own display without a
        // Space transition and hides the menu bar and Dock.
        use winit::platform::macos::WindowExtMacOS;
        window.set_simple_fullscreen(true);
    }
    #[cfg(not(target_os = "macos"))]
    {
        window.set_fullscreen(Some(Fullscreen::Borderless(monitor.cloned())));
    }
}

// ========================================================== output engine ==

/// A program window surface owned by the output thread.
struct OutputSurface {
    window_id: WindowId,
    /// Kept alive until the surface is dropped, then returned to the main thread.
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    /// Frames presented (the first present is logged: proves the output is live).
    presented: u64,
}

/// One logical output: its always-running canvas plus an optional window.
pub struct OutputSlot {
    pub id: OutputId,
    pub renderer: OutputRenderer,
    overlay: OverlayPainter,
    surface: Option<OutputSurface>,
    /// NDI publication of this output's canvas.
    pub ndi: Option<livestage_video::ndi::NdiOutput>,
}

/// A finished pixel-map readback: `(output, width, height, rgba)`.
pub type Readback = (OutputId, u32, u32, Vec<u8>);

/// A canvas the UI can show as a live preview.
#[derive(Clone)]
pub struct CanvasPreview {
    pub view: wgpu::TextureView,
    pub generation: u64,
    pub size: (u32, u32),
}

/// Canvases, windows and the compositor. Driven by the output thread (or
/// synchronously by the UI snapshot harness).
pub struct OutputEngine {
    gpu: Arc<GpuContext>,
    pipes: Pipelines,
    pub compositor: Compositor,
    pub slots: HashMap<OutputId, OutputSlot>,
    canvas_format: wgpu::TextureFormat,
}

impl OutputEngine {
    pub fn new(gpu: Arc<GpuContext>, canvas_format: wgpu::TextureFormat) -> Self {
        let pipes = Pipelines::new(&gpu.device);
        Self { gpu, pipes, compositor: Compositor::default(), slots: HashMap::new(), canvas_format }
    }

    /// Creates / removes canvases to match the project's logical displays.
    /// Returns windows whose output was removed (to be dropped on the main thread).
    pub fn sync_slots(&mut self, displays: &[LogicalDisplay]) -> Vec<Arc<Window>> {
        let mut orphaned = Vec::new();
        self.slots.retain(|id, slot| {
            let keep = displays.iter().any(|d| d.id == *id && d.enabled);
            if !keep {
                if let Some(s) = slot.surface.take() {
                    orphaned.push(release(s));
                }
            }
            keep
        });
        let (gpu, pipes) = (&self.gpu, &self.pipes);
        for d in displays.iter().filter(|d| d.enabled) {
            let canvas = d.canvas_size.map(|[w, h]| (w, h)).unwrap_or((1920, 1080));
            let format = self.canvas_format;
            let slot = self.slots.entry(d.id).or_insert_with(|| OutputSlot {
                id: d.id,
                renderer: OutputRenderer::new(gpu, pipes, d.id, canvas, format, &d.projection),
                overlay: OverlayPainter::new(gpu),
                surface: None,
                ndi: None,
            });
            slot.renderer.set_canvas_size(gpu, pipes, canvas);
            slot.renderer.set_projection(gpu, &d.projection);
            let want = d.ndi_output.as_deref().map(str::trim).filter(|n| !n.is_empty());
            if slot.ndi.as_ref().map(|n| n.name()) != want {
                slot.ndi = want.and_then(|name| livestage_video::ndi::NdiOutput::new(name, (60, 1)));
            }
        }
        orphaned
    }

    fn attach(&mut self, output: OutputId, window: Arc<Window>, surface: wgpu::Surface<'static>, config: wgpu::SurfaceConfiguration) -> Option<Arc<Window>> {
        let Some(slot) = self.slots.get_mut(&output) else { return Some(window) };
        let old = slot.surface.replace(OutputSurface { window_id: window.id(), window, surface, config, presented: 0 });
        old.map(release)
    }

    fn detach(&mut self, output: OutputId) -> Option<Arc<Window>> {
        self.slots.get_mut(&output).and_then(|s| s.surface.take()).map(release)
    }

    fn resize(&mut self, window: WindowId, size: [u32; 2]) {
        if size[0] == 0 || size[1] == 0 {
            return;
        }
        for s in self.slots.values_mut().filter_map(|s| s.surface.as_mut()) {
            if s.window_id == window && [s.config.width, s.config.height] != size {
                s.config.width = size[0];
                s.config.height = size[1];
                s.surface.configure(&self.gpu.device, &s.config);
            }
        }
    }

    pub fn previews(&self) -> HashMap<OutputId, CanvasPreview> {
        self.slots
            .values()
            .map(|s| {
                let preview = CanvasPreview {
                    view: s.renderer.canvas_view().clone(),
                    generation: s.renderer.canvas_generation(),
                    size: s.renderer.canvas_size(),
                };
                (s.id, preview)
            })
            .collect()
    }

    /// Renders every canvas and presents every open program window.
    /// Returns finished pixel-map readbacks as `(output, width, height, rgba)`
    /// and whether any window was presented.
    pub fn render(&mut self, now: ShowTime, displays: &[LogicalDisplay], pixel_sources: &[OutputId]) -> (Vec<Readback>, bool) {
        let (gpu, pipes) = (&self.gpu, &self.pipes);
        let mut readbacks = Vec::new();
        let mut presented_any = false;
        for slot in self.slots.values_mut() {
            let mut enc = gpu.device.create_command_encoder(&Default::default());
            slot.renderer.render_layers(gpu, pipes, &mut enc, self.compositor.layers_mut(slot.id), now);
            let overlays = self.compositor.overlays(slot.id, now);
            let mut cmds = if overlays.is_empty() {
                Vec::new()
            } else {
                let size = slot.renderer.canvas_size();
                slot.overlay.paint(gpu, &mut enc, slot.renderer.canvas_view(), size, &overlays)
            };
            let wants_readback = pixel_sources.contains(&slot.id);
            if wants_readback {
                slot.renderer.encode_readback(gpu, pipes, &mut enc);
            }
            if slot.ndi.is_some() {
                let size = displays
                    .iter()
                    .find(|d| d.id == slot.id)
                    .and_then(|d| d.ndi_resolution)
                    .map_or(slot.renderer.canvas_size(), |[w, h]| (w, h));
                slot.renderer.encode_stream_readback(gpu, pipes, &mut enc, size);
            }
            let mut frame = None;
            if let Some(s) = &mut slot.surface {
                if s.config.format != slot.renderer.surface_format() {
                    // The warp pass renders straight into the window's swapchain,
                    // so its pipeline must match the surface format (Metal and
                    // DXGI swapchains are BGRA while canvases are RGBA).
                    let canvas = slot.renderer.canvas_size();
                    let setup = displays.iter().find(|d| d.id == slot.id).map(|d| d.projection.clone()).unwrap_or_default();
                    slot.renderer = OutputRenderer::new(gpu, pipes, slot.id, canvas, s.config.format, &setup);
                    // The new canvas is empty until the next frame: draw the layers again.
                    slot.renderer.render_layers(gpu, pipes, &mut enc, self.compositor.layers_mut(slot.id), now);
                    if !overlays.is_empty() {
                        let size = slot.renderer.canvas_size();
                        cmds = slot.overlay.paint(gpu, &mut enc, slot.renderer.canvas_view(), size, &overlays);
                    }
                }
                match s.surface.get_current_texture() {
                    Ok(f) => {
                        if f.suboptimal {
                            tracing::debug!("program surface suboptimal; will reconfigure");
                        }
                        let view = f.texture.create_view(&Default::default());
                        slot.renderer.render_warp(&mut enc, &view);
                        frame = Some(f);
                    }
                    Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => s.surface.configure(&gpu.device, &s.config),
                    Err(wgpu::SurfaceError::Timeout) => {}
                    Err(e) => tracing::warn!("program surface: {e}"),
                }
            }
            cmds.push(enc.finish());
            gpu.queue.submit(cmds);
            if let (Some(f), Some(s)) = (frame, &mut slot.surface) {
                let suboptimal = f.suboptimal;
                f.present();
                presented_any = true;
                if s.presented == 0 {
                    let size = [s.config.width, s.config.height];
                    tracing::info!(output = %slot.id, ?size, format = ?s.config.format, "program output presenting");
                }
                s.presented += 1;
                if suboptimal {
                    s.surface.configure(&gpu.device, &s.config);
                }
            }
            slot.renderer.after_submit();
            if wants_readback {
                if let Some((w, h, px)) = slot.renderer.poll_readback(gpu) {
                    readbacks.push((slot.id, w, h, px));
                }
            }
            if let Some(ndi) = &slot.ndi {
                if let Some((w, h, px)) = slot.renderer.poll_stream_readback(gpu) {
                    ndi.send(livestage_video::source::VideoFrame::new(w, h, px, livestage_video::source::next_serial()));
                }
            }
        }
        (readbacks, presented_any)
    }

    fn release_all(&mut self) -> Vec<Arc<Window>> {
        self.slots.values_mut().filter_map(|s| s.surface.take()).map(release).collect()
    }
}

/// Drops the surface first, then hands the window back (windows are only
/// ever dropped on the main thread).
fn release(s: OutputSurface) -> Arc<Window> {
    let OutputSurface { window, surface, .. } = s;
    drop(surface);
    window
}

// ========================================================== output thread ==

pub enum OutputCmd {
    Open { output: OutputId, window: Arc<Window>, surface: wgpu::Surface<'static>, config: wgpu::SurfaceConfiguration },
    Close(OutputId),
    Resized { window: WindowId, size: [u32; 2] },
}

/// State the output thread publishes for the UI.
#[derive(Default)]
pub struct OutputShared {
    pub previews: Mutex<HashMap<OutputId, CanvasPreview>>,
    /// NDI receivers connected, per published output.
    pub ndi: Mutex<HashMap<OutputId, i32>>,
    /// Output frame rate ×1000 and worst frame time (µs) since last read.
    pub fps_milli: AtomicU64,
    pub max_frame_us: AtomicU64,
    /// Layers frame-locked to an audio master, on the last frame.
    pub sync_layers: AtomicU64,
    /// Worst |audio − video| of locked layers over the last second (µs).
    pub sync_error_us: AtomicU64,
    /// Hard re-times since the outputs started.
    pub sync_snaps: AtomicU64,
}

/// Audio clocks of the compositor's sync masters, looked up under the runtime
/// lock and read lock-free at the instant each frame is rendered.
pub(crate) fn master_clocks(rt: &crate::runtime::ShowRuntime, masters: &[CueId]) -> Vec<(CueId, VoiceClock)> {
    let Some(audio) = rt.audio.as_ref() else { return Vec::new() };
    masters.iter().filter_map(|m| audio.voice_clock(*m).map(|c| (*m, c))).collect()
}

/// The master clock as the compositor sees it.
pub fn master_reading(clocks: &[(CueId, VoiceClock)], cue: CueId) -> MasterClock {
    let Some((_, clock)) = clocks.iter().find(|(c, _)| *c == cue) else { return MasterClock::Gone };
    let r = clock.read();
    if r.ended {
        MasterClock::Gone
    } else if r.running {
        MasterClock::Running(r.position)
    } else {
        MasterClock::Held(r.position)
    }
}

pub struct OutputThread {
    tx: crossbeam_channel::Sender<OutputCmd>,
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    pub shared: Arc<OutputShared>,
    /// Windows released by the output thread, to be dropped on the main thread.
    pub returned: crossbeam_channel::Receiver<Arc<Window>>,
}

/// Frame period when no program window paces the loop with its vsync.
const IDLE_PERIOD: Duration = Duration::from_micros(16_667);

impl OutputThread {
    pub fn spawn(
        gpu: Arc<GpuContext>,
        rt: SharedRuntime,
        video_rx: crossbeam_channel::Receiver<crate::runtime::VideoMsg>,
        video_finished: crate::runtime::VideoFinished,
        epoch: Instant,
        bundle_root: std::path::PathBuf,
        canvas_format: wgpu::TextureFormat,
    ) -> std::io::Result<Self> {
        let (tx, rx) = crossbeam_channel::unbounded::<OutputCmd>();
        let (ret_tx, returned) = crossbeam_channel::unbounded();
        let running = Arc::new(AtomicBool::new(true));
        let shared = Arc::new(OutputShared::default());
        let (r, sh) = (running.clone(), shared.clone());
        let handle = std::thread::Builder::new().name("livestage-output".into()).spawn(move || {
            let mut engine = OutputEngine::new(gpu, canvas_format);
            let mut published: HashMap<OutputId, u64> = HashMap::new();
            let (mut frames, mut window_start) = (0u64, Instant::now());
            let mut sync_worst = 0.0f64;
            // Copy of the project's displays, refreshed only when they change.
            let (mut displays, mut displays_rev) = (Vec::new(), 0u64);
            let mut next = Instant::now();
            while r.load(Ordering::Relaxed) {
                let frame_start = Instant::now();
                // Video actions, applied at the show time they were dispatched.
                let resolve = |a: &livestage_core::project::AssetRef| livestage_core::bundle::resolve_asset(&bundle_root, a).ok();
                for (at, action) in video_rx.try_iter() {
                    engine.compositor.handle(at, &action, &resolve);
                }
                // Snapshot what the render pass needs (short lock).
                let masters = engine.compositor.sync_masters();
                let (pixel_sources, clocks) = {
                    let rt = rt.lock();
                    if rt.displays_rev != displays_rev {
                        displays_rev = rt.displays_rev;
                        displays.clone_from(&rt.bundle.project.displays);
                    }
                    let pixel: Vec<_> = rt.bundle.project.dmx.pixel_maps.iter().filter(|m| m.enabled).map(|m| m.source).collect();
                    (pixel, master_clocks(&rt, &masters))
                };
                for w in engine.sync_slots(&displays) {
                    let _ = ret_tx.send(w);
                }
                for cmd in rx.try_iter() {
                    let back = match cmd {
                        OutputCmd::Open { output, window, surface, config } => engine.attach(output, window, surface, config),
                        OutputCmd::Close(output) => engine.detach(output),
                        OutputCmd::Resized { window, size } => {
                            engine.resize(window, size);
                            None
                        }
                    };
                    if let Some(w) = back {
                        let _ = ret_tx.send(w);
                    }
                }
                let now: ShowTime = epoch.elapsed().into();
                // Timeline layers follow their audio master's sample clock.
                let report = engine.compositor.follow_masters(now, |m| master_reading(&clocks, m));
                sync_worst = sync_worst.max(report.worst_error);
                sh.sync_layers.store(report.layers as u64, Ordering::Relaxed);
                sh.sync_snaps.fetch_add(report.snaps as u64, Ordering::Relaxed);
                engine.compositor.update(now);
                video_finished.lock().clone_from(engine.compositor.finished_cues());

                let (readbacks, presented) = engine.render(now, &displays, &pixel_sources);
                if !readbacks.is_empty() {
                    let mut rt = rt.lock();
                    for (id, w, h, px) in readbacks {
                        rt.dmx.feed_canvas(id, w, h, &px);
                    }
                }
                // Publish canvases whose texture changed, and NDI state.
                let generations: HashMap<OutputId, u64> =
                    engine.slots.values().map(|s| (s.id, s.renderer.canvas_generation())).collect();
                if generations != published {
                    *sh.previews.lock() = engine.previews();
                    published = generations;
                }
                *sh.ndi.lock() = engine.slots.values().filter_map(|s| s.ndi.as_ref().map(|n| (s.id, n.connections()))).collect();

                frames += 1;
                sh.max_frame_us.fetch_max(frame_start.elapsed().as_micros() as u64, Ordering::Relaxed);
                if window_start.elapsed() >= Duration::from_secs(1) {
                    let fps = frames as f64 / window_start.elapsed().as_secs_f64();
                    sh.fps_milli.store((fps * 1000.0) as u64, Ordering::Relaxed);
                    sh.sync_error_us.store((sync_worst * 1e6) as u64, Ordering::Relaxed);
                    let layers = sh.sync_layers.load(Ordering::Relaxed);
                    if layers > 0 {
                        let snaps = sh.sync_snaps.load(Ordering::Relaxed);
                        tracing::debug!(layers, worst_ms = sync_worst * 1e3, snaps, "a/v lock");
                    }
                    sync_worst = 0.0;
                    (frames, window_start) = (0, Instant::now());
                }
                // Presenting windows are paced by their vsync; otherwise run at 60 Hz.
                next += IDLE_PERIOD;
                let t = Instant::now();
                if presented {
                    next = t;
                } else if next > t {
                    std::thread::sleep(next - t);
                } else if t - next > IDLE_PERIOD * 10 {
                    next = t;
                }
            }
            for w in engine.release_all() {
                let _ = ret_tx.send(w);
            }
        })?;
        Ok(Self { tx, running, handle: Some(handle), shared, returned })
    }

    pub fn send(&self, cmd: OutputCmd) {
        let _ = self.tx.send(cmd);
    }
}

impl Drop for OutputThread {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod av_sync_tests {
    use super::master_reading;
    use livestage_audio::{AudioClip, AudioEngine};
    use livestage_core::action::{AudioAction, AudioTrack, MediaSync, VideoAction, VisualContent};
    use livestage_core::project::AssetRef;
    use livestage_core::routing::AudioRouting;
    use livestage_core::{CueId, OutputId, ShowTime};
    use livestage_video::Compositor;
    use std::path::PathBuf;

    /// A timeline video layer stays on a drifting audio device clock: the real
    /// engine is rendered offline as fast as a device 2000 ppm fast would pull
    /// it, and the compositor follows its voice clock frame by frame.
    #[test]
    fn video_layer_follows_a_drifting_audio_device() {
        const RATE: u32 = 48_000;
        const PPM: f64 = 2_000.0;
        let routing = AudioRouting::default();
        let (mut engine, mut device) = AudioEngine::offline(RATE, 2, &routing);
        engine.insert_clip("/song.wav", AudioClip { sample_rate: RATE, channels: 1, data: vec![0.0; RATE as usize * 40] });
        let master = CueId::new();
        let resolve = |a: &AssetRef| Some(PathBuf::from("/").join(a.as_str()));
        engine.handle(
            &AudioAction::Play {
                cue: master,
                tracks: vec![AudioTrack { asset: AssetRef::new("song.wav"), stem: routing.stems[0].id, gain_db: 0.0, mute: false }],
                click: None,
                gain_db: 0.0,
                fade_in: ShowTime::ZERO,
                start_at: ShowTime::ZERO,
                end_at: None,
                looped: false,
            },
            &resolve,
        );
        let mut comp = Compositor::default();
        let (out, layer) = (OutputId::new(), CueId::new());
        comp.handle(
            ShowTime::ZERO,
            &VideoAction::Show {
                cue: layer,
                output: out,
                content: VisualContent::TestPattern,
                layer: 0,
                opacity: 1.0,
                fade_in: ShowTime::ZERO,
                start_at: ShowTime::ZERO,
                sync: Some(MediaSync { master, offset: ShowTime::ZERO }),
            },
            &|_| None,
        );
        assert_eq!(comp.sync_masters(), vec![master]);

        let mut buf = vec![0.0f32; 256 * 2];
        let mut rendered = 0u64;
        let (mut worst_locked, mut free_run_error) = (0.0f64, 0.0f64);
        for frame in 1..=30 * 60u64 {
            let now = ShowTime::from_micros(frame * 16_667);
            // The device pulls buffers on its own (fast) crystal.
            while (rendered as f64) < now.as_secs_f64() * RATE as f64 * (1.0 + PPM * 1e-6) {
                device.render(&mut buf);
                rendered += 256;
            }
            let clocks = vec![(master, engine.voice_clock(master).unwrap())];
            let report = comp.follow_masters(now, |m| master_reading(&clocks, m));
            assert_eq!(report.locked, 1);
            let audio = engine.media_position(master).unwrap();
            let video = comp.media_position(layer, now).unwrap();
            let err = (audio.as_secs_f64() - video.as_secs_f64()).abs();
            if frame > 120 {
                worst_locked = worst_locked.max(err);
            }
            free_run_error = (audio.as_secs_f64() - now.as_secs_f64()).abs();
            assert_eq!(report.snaps, 0, "hard re-time at frame {frame} (error {err})");
        }
        // Free-running, the picture would be 60 ms (1.5 frames) off by now.
        assert!(free_run_error > 0.055, "free-run error {free_run_error}");
        // Locked, it stays within one audio buffer (5.3 ms) of the sample clock.
        assert!(worst_locked < 0.008, "locked error {worst_locked}");
    }
}

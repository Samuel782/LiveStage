//! Display routing manager: physical monitor discovery, per-output canvases,
//! and hardened kiosk fullscreen program windows.

use crate::gfx::OverlayPainter;
use livestage_core::routing::{LogicalDisplay, PhysicalDisplayInfo, PhysicalDisplayKey};
use livestage_core::OutputId;
use livestage_video::{GpuContext, OutputRenderer, Pipelines};
use std::collections::HashMap;
use std::sync::Arc;
use winit::dpi::{PhysicalPosition, PhysicalSize};
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

/// A program output window on a physical display.
pub struct OutputWindow {
    pub window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    format: wgpu::TextureFormat,
    size: [u32; 2],
    pub kiosk: bool,
}

impl OutputWindow {
    fn open(
        el: &ActiveEventLoop,
        gpu: &GpuContext,
        display: &LogicalDisplay,
        monitor: Option<&MonitorHandle>,
    ) -> anyhow::Result<Self> {
        let kiosk = monitor.is_some() && display.fullscreen;
        let mut attrs = Window::default_attributes().with_title(format!("LiveStage — {}", display.name));
        match monitor.filter(|_| kiosk) {
            Some(m) => {
                attrs = attrs
                    .with_decorations(false)
                    .with_resizable(false)
                    .with_window_level(WindowLevel::AlwaysOnTop)
                    .with_position(m.position())
                    .with_inner_size(m.size());
                #[cfg(not(target_os = "macos"))]
                {
                    attrs = attrs.with_fullscreen(Some(Fullscreen::Borderless(Some(m.clone()))));
                }
            }
            None => {
                let [w, h] = display.canvas_size.unwrap_or([1920, 1080]);
                attrs = attrs.with_inner_size(PhysicalSize::new(w / 2, h / 2));
                if let Some(m) = monitor {
                    attrs = attrs.with_position(PhysicalPosition::new(m.position().x + 40, m.position().y + 40));
                }
            }
        }
        let window = Arc::new(el.create_window(attrs)?);
        if kiosk {
            harden(&window, monitor);
        }
        let surface = gpu.instance.create_surface(window.clone())?;
        let format = gpu.surface_format(&surface);
        let s = window.inner_size();
        let size = [s.width.max(1), s.height.max(1)];
        gpu.configure_surface(&surface, format, size, true);
        let name = &display.name;
        tracing::info!(output = %name, kiosk, ?size, "program window open");
        Ok(Self { window, surface, format, size, kiosk })
    }

    pub fn resize(&mut self, gpu: &GpuContext, w: u32, h: u32) {
        if w > 0 && h > 0 {
            self.size = [w, h];
            gpu.configure_surface(&self.surface, self.format, self.size, true);
        }
    }

    /// Re-applies kiosk invariants (called on focus / cursor-enter events,
    /// because some window managers restore the cursor or chrome).
    pub fn reassert(&self) {
        if self.kiosk {
            self.window.set_cursor_visible(false);
            self.window.set_window_level(WindowLevel::AlwaysOnTop);
        }
    }
}

/// Kiosk mode: no decorations, no cursor, always on top, true fullscreen.
fn harden(window: &Window, monitor: Option<&MonitorHandle>) {
    window.set_cursor_visible(false);
    window.set_decorations(false);
    window.set_window_level(WindowLevel::AlwaysOnTop);
    #[cfg(target_os = "macos")]
    {
        // "Simple" fullscreen stays on the window's own display without a
        // Space transition and hides the menu bar and Dock.
        use winit::platform::macos::WindowExtMacOS;
        let _ = monitor;
        window.set_simple_fullscreen(true);
    }
    #[cfg(not(target_os = "macos"))]
    {
        window.set_fullscreen(Some(Fullscreen::Borderless(monitor.cloned())));
    }
}

/// One logical output: its always-running canvas plus an optional window.
pub struct OutputSlot {
    pub id: OutputId,
    pub renderer: OutputRenderer,
    pub overlay: OverlayPainter,
    pub window: Option<OutputWindow>,
    /// egui preview texture (id, canvas generation it was registered for).
    pub preview: Option<(egui::TextureId, u64)>,
    /// NDI publication of this output's canvas.
    pub ndi: Option<livestage_video::ndi::NdiOutput>,
}

#[derive(Default)]
pub struct DisplayManager {
    pub monitors: Vec<MonitorHandle>,
    pub physical: Vec<PhysicalDisplayInfo>,
    pub slots: HashMap<OutputId, OutputSlot>,
}

impl DisplayManager {
    pub fn refresh_monitors(&mut self, el: &ActiveEventLoop) {
        let monitors: Vec<MonitorHandle> = el.available_monitors().collect();
        let primary = el.primary_monitor();
        let physical = describe_monitors(&monitors, primary.as_ref());
        if physical != self.physical {
            tracing::info!(count = physical.len(), "displays changed");
            self.physical = physical;
        }
        self.monitors = monitors;
    }

    pub fn monitor_for(&self, key: &PhysicalDisplayKey) -> Option<&MonitorHandle> {
        key.best_match(&self.physical).and_then(|i| self.monitors.get(i))
    }

    /// Creates / removes canvases to match the project's logical displays.
    pub fn sync_slots(&mut self, gpu: &GpuContext, pipes: &Pipelines, displays: &[LogicalDisplay], surface_format: wgpu::TextureFormat) {
        self.slots.retain(|id, _| displays.iter().any(|d| d.id == *id && d.enabled));
        for d in displays.iter().filter(|d| d.enabled) {
            let canvas = d.canvas_size.map(|[w, h]| (w, h)).unwrap_or((1920, 1080));
            let slot = self.slots.entry(d.id).or_insert_with(|| OutputSlot {
                id: d.id,
                renderer: OutputRenderer::new(gpu, pipes, d.id, canvas, surface_format, &d.projection),
                overlay: OverlayPainter::new(gpu),
                window: None,
                preview: None,
                ndi: None,
            });
            slot.renderer.set_canvas_size(gpu, pipes, canvas);
            slot.renderer.set_projection(gpu, &d.projection);
            let want = d.ndi_output.as_deref().map(str::trim).filter(|n| !n.is_empty());
            if slot.ndi.as_ref().map(|n| n.name()) != want {
                slot.ndi = want.and_then(|name| livestage_video::ndi::NdiOutput::new(name, (60, 1)));
            }
        }
    }

    /// Opens or closes program windows. When `live`, mapped displays go kiosk
    /// fullscreen; unmapped ones open as floating preview windows.
    pub fn set_live(&mut self, el: &ActiveEventLoop, gpu: &GpuContext, displays: &[LogicalDisplay], live: bool) {
        for d in displays.iter().filter(|d| d.enabled) {
            let monitor = d.physical.as_ref().and_then(|k| self.monitor_for(k)).cloned();
            let Some(slot) = self.slots.get_mut(&d.id) else { continue };
            if !live {
                slot.window = None;
                continue;
            }
            if slot.window.is_none() {
                match OutputWindow::open(el, gpu, d, monitor.as_ref()) {
                    Ok(w) => slot.window = Some(w),
                    Err(e) => tracing::error!(output = %d.name, "cannot open program window: {e}"),
                }
            }
        }
    }

    pub fn slot_for_window(&mut self, id: WindowId) -> Option<&mut OutputSlot> {
        self.slots.values_mut().find(|s| s.window.as_ref().is_some_and(|w| w.window.id() == id))
    }

    /// Renders every canvas and presents every open program window.
    /// Returns finished pixel-map readbacks as `(output, width, height, rgba)`.
    pub fn render(
        &mut self,
        gpu: &GpuContext,
        pipes: &Pipelines,
        compositor: &mut livestage_video::Compositor,
        now: livestage_core::ShowTime,
        displays: &[LogicalDisplay],
        pixel_sources: &[OutputId],
    ) -> Vec<(OutputId, u32, u32, Vec<u8>)> {
        let mut readbacks = Vec::new();
        for slot in self.slots.values_mut() {
            let mut enc = gpu.device.create_command_encoder(&Default::default());
            slot.renderer.render_layers(gpu, pipes, &mut enc, compositor.layers_mut(slot.id), now);
            let overlays = compositor.overlays(slot.id, now);
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
            if let Some(w) = &mut slot.window {
                if w.format != slot.renderer.surface_format() {
                    // Rebuild for this surface format (rare: heterogeneous GPUs/displays).
                    let canvas = slot.renderer.canvas_size();
                    let setup = displays.iter().find(|d| d.id == slot.id).map(|d| d.projection.clone()).unwrap_or_default();
                    slot.renderer = OutputRenderer::new(gpu, pipes, slot.id, canvas, w.format, &setup);
                    slot.preview = None;
                }
                match w.surface.get_current_texture() {
                    Ok(f) => {
                        let view = f.texture.create_view(&Default::default());
                        slot.renderer.render_warp(&mut enc, &view);
                        frame = Some(f);
                    }
                    Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                        gpu.configure_surface(&w.surface, w.format, w.size, true)
                    }
                    Err(e) => tracing::warn!("program surface: {e}"),
                }
            }
            cmds.push(enc.finish());
            gpu.queue.submit(cmds);
            if let (Some(f), Some(w)) = (frame, &slot.window) {
                w.window.pre_present_notify();
                f.present();
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
        readbacks
    }
}

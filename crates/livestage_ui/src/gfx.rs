//! egui ⇄ wgpu plumbing for the control window and canvas overlays.

use egui::{Align2, Color32, FontId, LayerId, Pos2, Rect};
use livestage_video::{GpuContext, Overlay, CANVAS_FORMAT};
use std::sync::Arc;
use winit::window::Window;

/// Tessellates and paints an egui frame into `target`.
#[allow(clippy::too_many_arguments)]
pub fn paint_egui(
    gpu: &GpuContext,
    renderer: &mut egui_wgpu::Renderer,
    ctx: &egui::Context,
    encoder: &mut wgpu::CommandEncoder,
    target: &wgpu::TextureView,
    size: [u32; 2],
    output: egui::FullOutput,
    clear: Option<wgpu::Color>,
) -> Vec<wgpu::CommandBuffer> {
    let ppp = output.pixels_per_point;
    let prims = ctx.tessellate(output.shapes, ppp);
    for (id, delta) in &output.textures_delta.set {
        renderer.update_texture(&gpu.device, &gpu.queue, *id, delta);
    }
    let sd = egui_wgpu::ScreenDescriptor { size_in_pixels: size, pixels_per_point: ppp };
    let cmds = renderer.update_buffers(&gpu.device, &gpu.queue, encoder, &prims, &sd);
    {
        let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("egui"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: clear.map_or(wgpu::LoadOp::Load, wgpu::LoadOp::Clear),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        let mut pass = pass.forget_lifetime();
        renderer.render(&mut pass, &prims, &sd);
    }
    for id in &output.textures_delta.free {
        renderer.free_texture(id);
    }
    cmds
}

/// The operator window.
pub struct ControlWindow {
    pub window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    pub format: wgpu::TextureFormat,
    size: [u32; 2],
    pub ctx: egui::Context,
    pub state: egui_winit::State,
    pub renderer: egui_wgpu::Renderer,
}

impl ControlWindow {
    pub fn new(gpu: &GpuContext, window: Arc<Window>, surface: wgpu::Surface<'static>) -> Self {
        let format = gpu.surface_format(&surface);
        let s = window.inner_size();
        let size = [s.width.max(1), s.height.max(1)];
        gpu.configure_surface(&surface, format, size, true);
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx);
        let state = egui_winit::State::new(
            ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            Some(gpu.device.limits().max_texture_dimension_2d as usize),
        );
        let renderer = egui_wgpu::Renderer::new(&gpu.device, format, None, 1, false);
        Self { window, surface, format, size, ctx, state, renderer }
    }

    pub fn resize(&mut self, gpu: &GpuContext, w: u32, h: u32) {
        if w > 0 && h > 0 {
            self.size = [w, h];
            gpu.configure_surface(&self.surface, self.format, self.size, true);
        }
    }

    /// Runs the UI closure and presents.
    pub fn frame(&mut self, gpu: &GpuContext, ui: impl FnMut(&egui::Context)) {
        let raw = self.state.take_egui_input(&self.window);
        let output = self.ctx.run(raw, ui);
        self.state.handle_platform_output(&self.window, output.platform_output.clone());
        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                gpu.configure_surface(&self.surface, self.format, self.size, true);
                return;
            }
            Err(e) => return tracing::warn!("control surface: {e}"),
        };
        let view = frame.texture.create_view(&Default::default());
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        let bg = crate::theme::BG;
        let clear = wgpu::Color { r: bg.r() as f64 / 255.0, g: bg.g() as f64 / 255.0, b: bg.b() as f64 / 255.0, a: 1.0 };
        let ctx = self.ctx.clone();
        let mut cmds = paint_egui(gpu, &mut self.renderer, &ctx, &mut enc, &view, self.size, output, Some(clear));
        cmds.push(enc.finish());
        gpu.queue.submit(cmds);
        self.window.pre_present_notify();
        frame.present();
    }
}

/// Draws text / timer overlays into a program canvas with egui's font engine.
pub struct OverlayPainter {
    ctx: egui::Context,
    renderer: egui_wgpu::Renderer,
}

impl OverlayPainter {
    pub fn new(gpu: &GpuContext) -> Self {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        Self { ctx, renderer: egui_wgpu::Renderer::new(&gpu.device, CANVAS_FORMAT, None, 1, false) }
    }

    pub fn paint(
        &mut self,
        gpu: &GpuContext,
        encoder: &mut wgpu::CommandEncoder,
        canvas: &wgpu::TextureView,
        canvas_size: (u32, u32),
        overlays: &[Overlay],
    ) -> Vec<wgpu::CommandBuffer> {
        let screen = Rect::from_min_size(Pos2::ZERO, egui::vec2(canvas_size.0 as f32, canvas_size.1 as f32));
        let raw = egui::RawInput { screen_rect: Some(screen), ..Default::default() };
        let mut sorted: Vec<&Overlay> = overlays.iter().collect();
        sorted.sort_by_key(|o| o.z);
        let output = self.ctx.run(raw, |ctx| {
            let painter = ctx.layer_painter(LayerId::background());
            let rect = ctx.screen_rect();
            for o in &sorted {
                let [r, g, b, a] = o.color;
                let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0) as u8;
                let color = Color32::from_rgba_unmultiplied(to8(r), to8(g), to8(b), to8(a * o.opacity));
                if o.monospace {
                    let center = rect.center();
                    painter.text(center, Align2::CENTER_CENTER, &o.text, FontId::monospace(o.font_size), color);
                    if let Some(label) = &o.label {
                        let dim = color.gamma_multiply(0.6);
                        let y = center.y - o.font_size * 0.65;
                        painter.text(Pos2::new(center.x, y), Align2::CENTER_BOTTOM, label, FontId::proportional(o.font_size * 0.22), dim);
                    }
                } else {
                    let galley = painter.layout(o.text.clone(), FontId::proportional(o.font_size), color, rect.width() * 0.9);
                    let x = rect.center().x - galley.size().x / 2.0;
                    let y = if o.scroll > 0.0 {
                        // Teleprompter: enters from the bottom and scrolls up.
                        rect.max.y - o.scroll
                    } else {
                        rect.center().y - galley.size().y / 2.0
                    };
                    let origin = Pos2::new(x, y);
                    if o.mirror {
                        // Teleprompter glass: flip the glyph mesh horizontally.
                        let w = galley.size().x;
                        let mut mesh = egui::Mesh::default();
                        for row in &galley.rows {
                            let mut m = row.visuals.mesh.clone();
                            for v in &mut m.vertices {
                                v.pos = Pos2::new(origin.x + w - v.pos.x, origin.y + v.pos.y);
                            }
                            mesh.append(m);
                        }
                        painter.add(egui::Shape::mesh(mesh));
                    } else {
                        painter.galley(origin, galley, color);
                    }
                }
            }
        });
        let ctx = self.ctx.clone();
        paint_egui(gpu, &mut self.renderer, &ctx, encoder, canvas, [canvas_size.0, canvas_size.1], output, None)
    }
}

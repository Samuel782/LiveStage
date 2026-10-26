//! Per-output renderer: layers → canvas → warp/blend/mask → window surface.

use crate::compositor::Layer;
use crate::gpu::GpuContext;
use crate::pipelines::*;
use crate::CANVAS_FORMAT;
use livestage_core::id::OutputId;
use livestage_core::projection::{rasterize_masks, BlendEdge, ProjectionSetup};
use livestage_core::time::ShowTime;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use wgpu::util::DeviceExt;

const MASK_RES: u32 = 512;
/// Resolution of the low-res canvas copy fed to the pixel mapper.
pub const READBACK_SIZE: (u32, u32) = (256, 144);

/// Number of staging buffers per readback stream. With three, frame N can
/// be encoded while N-1 is mapping and N-2 is being consumed, so a slow
/// map never stalls the render loop (frames are skipped instead).
const RING: usize = 3;

struct Staging {
    buffer: wgpu::Buffer,
    ready: Arc<AtomicBool>,
    state: ReadbackState,
    /// Encode order, to hand out the oldest finished frame first.
    seq: u64,
}

/// A downscale target plus a ring of mappable staging buffers.
struct Readback {
    size: (u32, u32),
    target: wgpu::Texture,
    target_view: wgpu::TextureView,
    canvas_bind: wgpu::BindGroup,
    padded_row: u32,
    ring: Vec<Staging>,
    next_seq: u64,
}

#[derive(Clone, Copy)]
enum ReadbackKind {
    PixelMap = 0,
    /// Full-quality stream (NDI) at its own resolution.
    Stream = 1,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum ReadbackState {
    Idle,
    Encoded,
    Mapping,
}

pub struct OutputRenderer {
    pub output: OutputId,
    canvas: wgpu::Texture,
    canvas_view: wgpu::TextureView,
    canvas_size: (u32, u32),
    surface_format: wgpu::TextureFormat,
    warp_pipeline: wgpu::RenderPipeline,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    warp_uniform: wgpu::Buffer,
    mask: wgpu::Texture,
    warp_bind: wgpu::BindGroup,
    setup: ProjectionSetup,
    /// Indexed by [`ReadbackKind`].
    readbacks: [Option<Readback>; 2],
    /// Incremented whenever the canvas texture is recreated (UI previews re-register).
    canvas_generation: u64,
}

fn make_canvas(device: &wgpu::Device, size: (u32, u32)) -> (wgpu::Texture, wgpu::TextureView) {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("canvas"),
        size: wgpu::Extent3d { width: size.0.max(1), height: size.1.max(1), depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: CANVAS_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = tex.create_view(&Default::default());
    (tex, view)
}

fn edge(e: &BlendEdge) -> [f32; 4] {
    [e.width, e.power, e.gamma, 0.0]
}

fn warp_uniform(setup: &ProjectionSetup) -> WarpUniform {
    let b = &setup.blend;
    let masked = setup.masks.iter().any(|m| m.enabled && m.points.len() >= 3);
    WarpUniform {
        edges: [edge(&b.left), edge(&b.right), edge(&b.top), edge(&b.bottom)],
        params: [setup.brightness, if masked { 1.0 } else { 0.0 }, 0.0, 0.0],
    }
}

fn mesh_buffers(device: &wgpu::Device, setup: &ProjectionSetup) -> (wgpu::Buffer, wgpu::Buffer, u32) {
    let grid = if setup.warp.is_valid() { setup.warp.clone() } else { Default::default() };
    let mesh = grid.tessellate();
    let verts: Vec<WarpVertexGpu> = mesh.vertices.iter().map(|v| WarpVertexGpu { position: v.position, uv: v.uv }).collect();
    let vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("warp-vertices"),
        contents: bytemuck::cast_slice(&verts),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let ib = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("warp-indices"),
        contents: bytemuck::cast_slice(&mesh.indices),
        usage: wgpu::BufferUsages::INDEX,
    });
    (vb, ib, mesh.indices.len() as u32)
}

impl OutputRenderer {
    pub fn new(
        gpu: &GpuContext,
        pipes: &Pipelines,
        output: OutputId,
        canvas_size: (u32, u32),
        surface_format: wgpu::TextureFormat,
        setup: &ProjectionSetup,
    ) -> Self {
        let device = &gpu.device;
        let (canvas, canvas_view) = make_canvas(device, canvas_size);
        let warp_pipeline = pipes.warp_pipeline(device, surface_format);
        let (vertices, indices, index_count) = mesh_buffers(device, setup);
        let warp_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("warp-uniform"),
            contents: bytemuck::bytes_of(&warp_uniform(setup)),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let mask = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("mask"),
            size: wgpu::Extent3d { width: MASK_RES, height: MASK_RES, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let warp_bind = Self::warp_bind(device, pipes, &warp_uniform, &canvas_view, &mask);
        let r = Self {
            output,
            canvas,
            canvas_view,
            canvas_size,
            surface_format,
            warp_pipeline,
            vertices,
            indices,
            index_count,
            warp_uniform,
            mask,
            warp_bind,
            setup: setup.clone(),
            readbacks: [None, None],
            canvas_generation: 0,
        };
        r.upload_mask(&gpu.queue);
        r
    }

    fn warp_bind(
        device: &wgpu::Device,
        pipes: &Pipelines,
        uniform: &wgpu::Buffer,
        canvas_view: &wgpu::TextureView,
        mask: &wgpu::Texture,
    ) -> wgpu::BindGroup {
        let mask_view = mask.create_view(&Default::default());
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("warp-bg"),
            layout: &pipes.warp_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(canvas_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&pipes.sampler) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&mask_view) },
            ],
        })
    }

    fn upload_mask(&self, queue: &wgpu::Queue) {
        let data = rasterize_masks(&self.setup.masks, MASK_RES, MASK_RES);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &self.mask, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            &data,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(MASK_RES), rows_per_image: Some(MASK_RES) },
            wgpu::Extent3d { width: MASK_RES, height: MASK_RES, depth_or_array_layers: 1 },
        );
    }

    pub fn canvas_view(&self) -> &wgpu::TextureView {
        &self.canvas_view
    }
    pub fn canvas_size(&self) -> (u32, u32) {
        self.canvas_size
    }
    pub fn canvas_generation(&self) -> u64 {
        self.canvas_generation
    }
    pub fn surface_format(&self) -> wgpu::TextureFormat {
        self.surface_format
    }

    /// Applies projection edits (cheap no-op when unchanged).
    pub fn set_projection(&mut self, gpu: &GpuContext, setup: &ProjectionSetup) {
        if *setup == self.setup {
            return;
        }
        if setup.warp != self.setup.warp {
            let (v, i, n) = mesh_buffers(&gpu.device, setup);
            (self.vertices, self.indices, self.index_count) = (v, i, n);
        }
        let masks_changed = setup.masks != self.setup.masks;
        self.setup = setup.clone();
        gpu.queue.write_buffer(&self.warp_uniform, 0, bytemuck::bytes_of(&warp_uniform(setup)));
        if masks_changed {
            self.upload_mask(&gpu.queue);
        }
    }

    pub fn set_canvas_size(&mut self, gpu: &GpuContext, pipes: &Pipelines, size: (u32, u32)) {
        if size == self.canvas_size || size.0 == 0 || size.1 == 0 {
            return;
        }
        let (c, v) = make_canvas(&gpu.device, size);
        (self.canvas, self.canvas_view, self.canvas_size) = (c, v, size);
        self.warp_bind = Self::warp_bind(&gpu.device, pipes, &self.warp_uniform, &self.canvas_view, &self.mask);
        self.readbacks = [None, None];
        self.canvas_generation += 1;
    }

    /// Pass A: composite texture layers into the canvas (cleared to black).
    pub fn render_layers(&mut self, gpu: &GpuContext, pipes: &Pipelines, encoder: &mut wgpu::CommandEncoder, layers: &mut [Layer], now: ShowTime) {
        for layer in layers.iter_mut().filter(|l| l.is_texture()) {
            let Some(frame) = layer.frame(now) else { continue };
            let size = (frame.width, frame.height);
            if layer.gpu.as_ref().is_none_or(|g| !g.fits(&frame)) {
                layer.gpu = Some(GpuLayer::new(&gpu.device, pipes, &frame));
            }
            let opacity = layer.opacity.value(now);
            let g = layer.gpu.as_mut().expect("created above");
            g.upload(&gpu.queue, &frame);
            let u = LayerUniform { rect: fit_rect(size, self.canvas_size), tint: [1.0, 1.0, 1.0, opacity], yuv: g.yuv_rows() };
            gpu.queue.write_buffer(&g.uniform, 0, bytemuck::bytes_of(&u));
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("canvas-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.canvas_view,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        for layer in layers.iter() {
            if let Some(g) = &layer.gpu {
                if layer.is_texture() && layer.opacity.value(now) > 0.0 {
                    pass.set_pipeline(g.pipeline(pipes));
                    pass.set_bind_group(0, &g.bind_group, &[]);
                    pass.draw(0..4, 0..1);
                }
            }
        }
    }

    /// Pass B: warp + blend + mask the canvas onto the window surface.
    pub fn render_warp(&self, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("warp-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&self.warp_pipeline);
        pass.set_bind_group(0, &self.warp_bind, &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.set_index_buffer(self.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..self.index_count, 0, 0..1);
    }

    // ------------------------------------------------------- readback --

    fn ensure_readback(&mut self, gpu: &GpuContext, pipes: &Pipelines, kind: ReadbackKind, size: (u32, u32)) -> &mut Readback {
        let slot = kind as usize;
        let size = (size.0.max(1), size.1.max(1));
        if self.readbacks[slot].as_ref().is_none_or(|r| r.size != size) {
            let device = &gpu.device;
            let (w, h) = size;
            let target = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("readback-target"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: CANVAS_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let target_view = target.create_view(&Default::default());
            // Letterbox into the target when aspect ratios differ.
            let rect = match kind {
                ReadbackKind::PixelMap => [0.0, 0.0, 1.0, 1.0],
                ReadbackKind::Stream => fit_rect(self.canvas_size, size),
            };
            let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("readback-uniform"),
                contents: bytemuck::bytes_of(&LayerUniform::new(rect, [1.0; 4])),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let canvas_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("readback-bg"),
                layout: &pipes.layer_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&self.canvas_view) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&pipes.sampler) },
                ],
            });
            let padded_row = (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
            let ring = (0..RING)
                .map(|_| Staging {
                    buffer: device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("readback-staging"),
                        size: (padded_row * h) as u64,
                        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                        mapped_at_creation: false,
                    }),
                    ready: Arc::new(AtomicBool::new(false)),
                    state: ReadbackState::Idle,
                    seq: 0,
                })
                .collect();
            self.readbacks[slot] = Some(Readback { size, target, target_view, canvas_bind, padded_row, ring, next_seq: 0 });
        }
        self.readbacks[slot].as_mut().expect("initialised")
    }

    /// Encodes a downscaled canvas copy for the pixel mapper.
    pub fn encode_readback(&mut self, gpu: &GpuContext, pipes: &Pipelines, encoder: &mut wgpu::CommandEncoder) {
        self.encode(gpu, pipes, encoder, ReadbackKind::PixelMap, READBACK_SIZE);
    }

    /// Encodes a canvas copy at `size` (e.g. 1920×1080 for NDI from a 4K
    /// canvas). The GPU does the scaling; only `size` pixels cross the bus.
    pub fn encode_stream_readback(&mut self, gpu: &GpuContext, pipes: &Pipelines, encoder: &mut wgpu::CommandEncoder, size: (u32, u32)) {
        self.encode(gpu, pipes, encoder, ReadbackKind::Stream, size);
    }

    fn encode(&mut self, gpu: &GpuContext, pipes: &Pipelines, encoder: &mut wgpu::CommandEncoder, kind: ReadbackKind, size: (u32, u32)) {
        let rb = self.ensure_readback(gpu, pipes, kind, size);
        // Every staging buffer busy: drop this frame rather than wait.
        let Some(idx) = rb.ring.iter().position(|s| s.state == ReadbackState::Idle) else { return };
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("readback-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &rb.target_view,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&pipes.layer_pipeline);
            pass.set_bind_group(0, &rb.canvas_bind, &[]);
            pass.draw(0..4, 0..1);
        }
        let (w, h) = rb.size;
        let staging = &mut rb.ring[idx];
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &rb.target, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging.buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(rb.padded_row), rows_per_image: Some(h) },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        staging.state = ReadbackState::Encoded;
        staging.seq = rb.next_seq;
        rb.next_seq += 1;
    }

    /// Call after `queue.submit` to start mapping freshly encoded buffers.
    pub fn after_submit(&mut self) {
        for rb in self.readbacks.iter_mut().flatten() {
            for s in rb.ring.iter_mut().filter(|s| s.state == ReadbackState::Encoded) {
                let ready = s.ready.clone();
                s.buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
                    if r.is_ok() {
                        ready.store(true, Ordering::Release);
                    }
                });
                s.state = ReadbackState::Mapping;
            }
        }
    }

    /// Returns a finished pixel-map readback as tightly packed RGBA8, if available.
    pub fn poll_readback(&mut self, gpu: &GpuContext) -> Option<(u32, u32, Vec<u8>)> {
        self.poll(gpu, ReadbackKind::PixelMap)
    }

    /// Returns the newest finished stream readback (older finished ones are recycled).
    pub fn poll_stream_readback(&mut self, gpu: &GpuContext) -> Option<(u32, u32, Vec<u8>)> {
        self.poll(gpu, ReadbackKind::Stream)
    }

    fn poll(&mut self, gpu: &GpuContext, kind: ReadbackKind) -> Option<(u32, u32, Vec<u8>)> {
        let rb = self.readbacks[kind as usize].as_mut()?;
        if rb.ring.iter().all(|s| s.state != ReadbackState::Mapping) {
            return None;
        }
        gpu.device.poll(wgpu::Maintain::Poll);
        // Newest ready frame wins; any older ready ones are released unread.
        let ready: Vec<usize> = (0..rb.ring.len())
            .filter(|&i| rb.ring[i].state == ReadbackState::Mapping && rb.ring[i].ready.load(Ordering::Acquire))
            .collect();
        let newest = *ready.iter().max_by_key(|&&i| rb.ring[i].seq)?;
        let (w, h) = rb.size;
        let row = (w * 4) as usize;
        let mut out = Vec::with_capacity(row * h as usize);
        {
            let mapped = rb.ring[newest].buffer.slice(..).get_mapped_range();
            if rb.padded_row as usize == row {
                // Common case (e.g. 1920 px wide): no row padding, one copy.
                out.extend_from_slice(&mapped[..row * h as usize]);
            } else {
                for y in 0..h as usize {
                    let start = y * rb.padded_row as usize;
                    out.extend_from_slice(&mapped[start..start + row]);
                }
            }
        }
        for &i in &ready {
            let s = &mut rb.ring[i];
            s.buffer.unmap();
            s.ready.store(false, Ordering::Release);
            s.state = ReadbackState::Idle;
        }
        Some((w, h, out))
    }
}

#[cfg(test)]
mod gpu_tests {
    use super::*;
    use crate::compositor::Compositor;
    use livestage_core::action::{VideoAction, VisualContent};
    use livestage_core::id::CueId;
    use livestage_core::projection::{BlendEdge, PolygonMask};

    fn gpu() -> Option<Arc<GpuContext>> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        pollster::block_on(GpuContext::new(instance, None)).ok()
    }

    /// Full pipeline on a real adapter: layer pass, warp pass with blend +
    /// mask into an offscreen target, and pixel-map readback.
    #[test]
    fn renders_layers_warp_and_readback() {
        let Some(gpu) = gpu() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let pipes = Pipelines::new(&gpu.device);
        let out = OutputId::new();
        let mut setup = ProjectionSetup { brightness: 1.0, ..Default::default() };
        setup.blend.left = BlendEdge { width: 0.25, power: 2.0, gamma: 1.0 };
        setup.masks.push(PolygonMask {
            name: "bottom".into(),
            points: vec![[0.0, 0.5], [1.0, 0.5], [1.0, 1.0], [0.0, 1.0]],
            invert: false,
            enabled: true,
        });
        let mut r = OutputRenderer::new(&gpu, &pipes, out, (64, 64), CANVAS_FORMAT, &setup);

        let mut comp = Compositor::default();
        comp.handle(
            ShowTime::ZERO,
            &VideoAction::Show {
                cue: CueId::new(),
                output: out,
                content: VisualContent::Color { rgba: [1.0, 0.0, 0.0, 1.0] },
                layer: 0,
                opacity: 1.0,
                fade_in: ShowTime::ZERO,
                start_at: ShowTime::ZERO,
                sync: None,
            },
            &|_| None,
        );

        let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: 64, height: 64, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: CANVAS_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());
        let row = 256u32;
        let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (row * 64) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut enc = gpu.device.create_command_encoder(&Default::default());
        r.render_layers(&gpu, &pipes, &mut enc, comp.layers_mut(out), ShowTime::ZERO);
        r.render_warp(&mut enc, &view);
        r.encode_readback(&gpu, &pipes, &mut enc);
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &target, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(64) },
            },
            wgpu::Extent3d { width: 64, height: 64, depth_or_array_layers: 1 },
        );
        gpu.queue.submit([enc.finish()]);
        r.after_submit();
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        gpu.device.poll(wgpu::Maintain::Wait);

        let px = |x: u32, y: u32| -> [u8; 4] {
            let m = buf.slice(..).get_mapped_range();
            let i = (y * row + x * 4) as usize;
            [m[i], m[i + 1], m[i + 2], m[i + 3]]
        };
        let right_top = px(60, 10);
        let left_edge = px(1, 10);
        let masked = px(60, 50);
        assert!(right_top[0] > 250 && right_top[1] < 5, "full red outside blend: {right_top:?}");
        assert!(left_edge[0] < 40, "edge blend attenuates the left edge: {left_edge:?}");
        assert!(masked[0] < 5, "polygon mask hides the bottom half: {masked:?}");

        let (w, h, pixels) = r.poll_readback(&gpu).expect("readback ready after Wait");
        assert_eq!((w, h), READBACK_SIZE);
        assert!(pixels[0] > 250 && pixels[1] < 5, "readback sees the unwarped red canvas");
    }

    /// NV12 frames are converted to RGB on the GPU with the right matrix and range.
    #[test]
    fn nv12_layer_converts_on_gpu() {
        use crate::source::{VideoFrame, YuvMatrix};
        let Some(gpu) = gpu() else { return };
        let pipes = Pipelines::new(&gpu.device);
        let (w, h) = (8u32, 8u32);
        // BT.709 limited-range pure red: Y=63, Cb=102, Cr=240.
        let mut data = vec![63u8; (w * h) as usize];
        for _ in 0..(w / 2) * (h / 2) {
            data.extend_from_slice(&[102, 240]);
        }
        let frame = VideoFrame::nv12(w, h, data, YuvMatrix::Bt709, false, 7);
        let mut layer = GpuLayer::new(&gpu.device, &pipes, &frame);
        layer.upload(&gpu.queue, &frame);
        let u = LayerUniform { rect: [0.0, 0.0, 1.0, 1.0], tint: [1.0; 4], yuv: layer.yuv_rows() };
        gpu.queue.write_buffer(&layer.uniform, 0, bytemuck::bytes_of(&u));

        let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: CANVAS_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());
        let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256 * h as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(layer.pipeline(&pipes));
            pass.set_bind_group(0, &layer.bind_group, &[]);
            pass.draw(0..4, 0..1);
        }
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &target, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(256), rows_per_image: Some(h) },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        gpu.queue.submit([enc.finish()]);
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        gpu.device.poll(wgpu::Maintain::Wait);
        let m = buf.slice(..).get_mapped_range();
        let px = [m[4 * 4 + 4 * 256], m[4 * 4 + 4 * 256 + 1], m[4 * 4 + 4 * 256 + 2]];
        assert!(px[0] > 245 && px[1] < 10 && px[2] < 10, "expected red, got {px:?}");
    }

    /// Regression: a display with the *default* projection setup must show
    /// its content on the window surface (it used to be brightness 0 = black).
    #[test]
    fn default_projection_is_not_black() {
        let Some(gpu) = gpu() else { return };
        let pipes = Pipelines::new(&gpu.device);
        let out = OutputId::new();
        let mut r = OutputRenderer::new(&gpu, &pipes, out, (32, 32), CANVAS_FORMAT, &ProjectionSetup::default());
        let mut comp = Compositor::default();
        comp.handle(
            ShowTime::ZERO,
            &VideoAction::Show {
                cue: CueId::new(),
                output: out,
                content: VisualContent::Color { rgba: [0.0, 1.0, 0.0, 1.0] },
                layer: 0,
                opacity: 1.0,
                fade_in: ShowTime::ZERO,
                start_at: ShowTime::ZERO,
                sync: None,
            },
            &|_| None,
        );
        let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: 32, height: 32, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: CANVAS_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());
        let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256 * 32,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        r.render_layers(&gpu, &pipes, &mut enc, comp.layers_mut(out), ShowTime::ZERO);
        r.render_warp(&mut enc, &view);
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &target, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(256), rows_per_image: Some(32) },
            },
            wgpu::Extent3d { width: 32, height: 32, depth_or_array_layers: 1 },
        );
        gpu.queue.submit([enc.finish()]);
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        gpu.device.poll(wgpu::Maintain::Wait);
        let m = buf.slice(..).get_mapped_range();
        let i = (16 * 256 + 16 * 4) as usize;
        assert!(m[i + 1] > 245, "window surface must show the green layer, got {:?}", &m[i..i + 4]);
    }

    /// Stream readback scales on the GPU, letterboxes, and never blocks when
    /// more frames are encoded than the ring can hold.
    #[test]
    fn stream_readback_scales_letterboxes_and_drops() {
        let Some(gpu) = gpu() else { return };
        let pipes = Pipelines::new(&gpu.device);
        let out = OutputId::new();
        let mut r = OutputRenderer::new(&gpu, &pipes, out, (64, 32), CANVAS_FORMAT, &ProjectionSetup::default());
        let mut comp = Compositor::default();
        comp.handle(
            ShowTime::ZERO,
            &VideoAction::Show {
                cue: CueId::new(),
                output: out,
                content: VisualContent::Color { rgba: [1.0, 0.0, 0.0, 1.0] },
                layer: 0,
                opacity: 1.0,
                fade_in: ShowTime::ZERO,
                start_at: ShowTime::ZERO,
                sync: None,
            },
            &|_| None,
        );
        // Encode five frames without polling: only RING fit, the rest are dropped.
        for _ in 0..5 {
            let mut enc = gpu.device.create_command_encoder(&Default::default());
            r.render_layers(&gpu, &pipes, &mut enc, comp.layers_mut(out), ShowTime::ZERO);
            r.encode_stream_readback(&gpu, &pipes, &mut enc, (32, 32));
            gpu.queue.submit([enc.finish()]);
            r.after_submit();
        }
        gpu.device.poll(wgpu::Maintain::Wait);
        let (w, h, px) = r.poll_stream_readback(&gpu).expect("a finished frame");
        assert_eq!((w, h, px.len()), (32, 32, 32 * 32 * 4));
        let at = |x: u32, y: u32| &px[((y * w + x) * 4) as usize..][..3];
        assert!(at(16, 2)[0] < 5, "top letterbox bar is black");
        assert!(at(16, 16)[0] > 245, "picture is red in the middle");
        // All buffers were released: encoding works again immediately.
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        r.encode_stream_readback(&gpu, &pipes, &mut enc, (32, 32));
        gpu.queue.submit([enc.finish()]);
        r.after_submit();
        gpu.device.poll(wgpu::Maintain::Wait);
        assert!(r.poll_stream_readback(&gpu).is_some());
    }
}


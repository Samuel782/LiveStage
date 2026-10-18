//! Render pipelines shared by every output on a device.

use crate::CANVAS_FORMAT;
use crate::source::{yuv_to_rgb_rows, PixelFormat, VideoFrame};
use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

const LAYER_WGSL: &str = r#"
struct LayerUniform {
    // Destination rect in normalised canvas space (x0, y0, x1, y1), y down.
    rect: vec4<f32>,
    tint: vec4<f32>,
    // YUV -> RGB rows applied to (Y, Cb, Cr, 1); unused for RGBA layers.
    yuv_r: vec4<f32>,
    yuv_g: vec4<f32>,
    yuv_b: vec4<f32>,
};
@group(0) @binding(0) var<uniform> u: LayerUniform;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
    var corners = array<vec2<f32>, 4>(vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0), vec2(1.0, 1.0));
    let c = corners[i];
    let p = vec2(mix(u.rect.x, u.rect.z, c.x), mix(u.rect.y, u.rect.w, c.y));
    var o: VsOut;
    o.pos = vec4(p.x * 2.0 - 1.0, 1.0 - p.y * 2.0, 0.0, 1.0);
    o.uv = c;
    return o;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(tex, samp, in.uv) * u.tint;
}
"#;

/// NV12 layers: Y and interleaved CbCr planes are sampled separately and
/// converted to RGB here, so decoded video never goes through a CPU colour
/// conversion. Shares the vertex stage with `LAYER_WGSL`.
const NV12_WGSL: &str = r#"
struct LayerUniform {
    rect: vec4<f32>,
    tint: vec4<f32>,
    yuv_r: vec4<f32>,
    yuv_g: vec4<f32>,
    yuv_b: vec4<f32>,
};
@group(0) @binding(0) var<uniform> u: LayerUniform;
@group(0) @binding(1) var tex_y: texture_2d<f32>;
@group(0) @binding(2) var tex_uv: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
    var corners = array<vec2<f32>, 4>(vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0), vec2(1.0, 1.0));
    let c = corners[i];
    let p = vec2(mix(u.rect.x, u.rect.z, c.x), mix(u.rect.y, u.rect.w, c.y));
    var o: VsOut;
    o.pos = vec4(p.x * 2.0 - 1.0, 1.0 - p.y * 2.0, 0.0, 1.0);
    o.uv = c;
    return o;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let y = textureSample(tex_y, samp, in.uv).r;
    let c = textureSample(tex_uv, samp, in.uv).rg;
    let v = vec4(y, c.x, c.y, 1.0);
    let rgb = clamp(vec3(dot(u.yuv_r, v), dot(u.yuv_g, v), dot(u.yuv_b, v)), vec3(0.0), vec3(1.0));
    return vec4(rgb, 1.0) * u.tint;
}
"#;

const WARP_WGSL: &str = r#"
struct WarpUniform {
    // Per edge (left, right, top, bottom): width, power, gamma, unused.
    edges: array<vec4<f32>, 4>,
    // brightness, mask_enabled, unused, unused
    params: vec4<f32>,
};
@group(0) @binding(0) var<uniform> u: WarpUniform;
@group(0) @binding(1) var canvas: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var mask: texture_2d<f32>;

struct VsIn {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
};
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) out_pos: vec2<f32>,
};

@vertex
fn vs_main(v: VsIn) -> VsOut {
    var o: VsOut;
    o.pos = vec4(v.position.x * 2.0 - 1.0, 1.0 - v.position.y * 2.0, 0.0, 1.0);
    o.uv = v.uv;
    o.out_pos = v.position;
    return o;
}

fn ramp(d: f32, e: vec4<f32>) -> f32 {
    if (e.x <= 0.0) { return 1.0; }
    let x = clamp(d / e.x, 0.0, 1.0);
    var c: f32;
    if (x < 0.5) {
        c = 0.5 * pow(2.0 * x, e.y);
    } else {
        c = 1.0 - 0.5 * pow(2.0 * (1.0 - x), e.y);
    }
    return pow(c, 1.0 / max(e.z, 0.01));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let src = textureSample(canvas, samp, in.uv);
    let blend = ramp(in.uv.x, u.edges[0]) * ramp(1.0 - in.uv.x, u.edges[1])
              * ramp(in.uv.y, u.edges[2]) * ramp(1.0 - in.uv.y, u.edges[3]);
    var m = 1.0;
    if (u.params.y > 0.5) {
        m = textureSample(mask, samp, in.out_pos).r;
    }
    let k = blend * m * u.params.x;
    return vec4(src.rgb * k, 1.0);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct LayerUniform {
    pub rect: [f32; 4],
    pub tint: [f32; 4],
    pub yuv: [[f32; 4]; 3],
}

impl LayerUniform {
    pub fn new(rect: [f32; 4], tint: [f32; 4]) -> Self {
        Self { rect, tint, yuv: [[0.0; 4]; 3] }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct WarpUniform {
    pub edges: [[f32; 4]; 4],
    pub params: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct WarpVertexGpu {
    pub position: [f32; 2],
    pub uv: [f32; 2],
}

/// Device-wide pipelines and layouts.
pub struct Pipelines {
    pub layer_layout: wgpu::BindGroupLayout,
    pub layer_pipeline: wgpu::RenderPipeline,
    pub nv12_layout: wgpu::BindGroupLayout,
    pub nv12_pipeline: wgpu::RenderPipeline,
    pub warp_layout: wgpu::BindGroupLayout,
    pub sampler: wgpu::Sampler,
    warp_module: wgpu::ShaderModule,
    warp_pipeline_layout: wgpu::PipelineLayout,
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}
fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}
fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

impl Pipelines {
    pub fn new(device: &wgpu::Device) -> Self {
        let layer_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("layer.wgsl"),
            source: wgpu::ShaderSource::Wgsl(LAYER_WGSL.into()),
        });
        let warp_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("warp.wgsl"),
            source: wgpu::ShaderSource::Wgsl(WARP_WGSL.into()),
        });
        let layer_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("layer-bgl"),
            entries: &[uniform_entry(0), texture_entry(1), sampler_entry(2)],
        });
        let warp_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("warp-bgl"),
            entries: &[uniform_entry(0), texture_entry(1), sampler_entry(2), texture_entry(3)],
        });
        let layer_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("layer-pl"),
            bind_group_layouts: &[&layer_layout],
            push_constant_ranges: &[],
        });
        let warp_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("warp-pl"),
            bind_group_layouts: &[&warp_layout],
            push_constant_ranges: &[],
        });
        let nv12_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("nv12.wgsl"),
            source: wgpu::ShaderSource::Wgsl(NV12_WGSL.into()),
        });
        let nv12_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("nv12-bgl"),
            entries: &[uniform_entry(0), texture_entry(1), texture_entry(2), sampler_entry(3)],
        });
        let nv12_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("nv12-pl"),
            bind_group_layouts: &[&nv12_layout],
            push_constant_ranges: &[],
        });
        let nv12_pipeline = quad_pipeline(device, "nv12-pipeline", &nv12_pl, &nv12_module);
        let layer_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("layer-pipeline"),
            layout: Some(&layer_pl),
            vertex: wgpu::VertexState {
                module: &layer_module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &layer_module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: CANVAS_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("linear-clamp"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self { layer_layout, layer_pipeline, nv12_layout, nv12_pipeline, warp_layout, sampler, warp_module, warp_pipeline_layout }
    }

    /// Warp pipelines depend on the window surface format.
    pub fn warp_pipeline(&self, device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("warp-pipeline"),
            layout: Some(&self.warp_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &self.warp_module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<WarpVertexGpu>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
                }],
            },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, ..Default::default() },
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &self.warp_module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        })
    }
}

/// Alpha-blended full-quad pipeline into the canvas.
fn quad_pipeline(device: &wgpu::Device, label: &str, layout: &wgpu::PipelineLayout, module: &wgpu::ShaderModule) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState { module, entry_point: Some("vs_main"), compilation_options: Default::default(), buffers: &[] },
        primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: CANVAS_FORMAT,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview: None,
        cache: None,
    })
}

/// GPU texture + bind group for one compositor layer.
pub struct GpuLayer {
    /// One texture for RGBA, two (Y, CbCr) for NV12.
    textures: Vec<wgpu::Texture>,
    pub size: (u32, u32),
    pub format: PixelFormat,
    pub uniform: wgpu::Buffer,
    pub bind_group: wgpu::BindGroup,
    pub serial: u64,
}

fn plane_texture(device: &wgpu::Device, w: u32, h: u32, format: wgpu::TextureFormat) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("layer-plane"),
        size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn write_plane(queue: &wgpu::Queue, tex: &wgpu::Texture, data: &[u8], w: u32, h: u32, bytes_per_px: u32) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo { texture: tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        data,
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * bytes_per_px), rows_per_image: Some(h) },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
}

impl GpuLayer {
    pub fn new(device: &wgpu::Device, pipes: &Pipelines, frame: &VideoFrame) -> Self {
        let size = (frame.width, frame.height);
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("layer-uniform"),
            contents: bytemuck::bytes_of(&LayerUniform::new([0.0, 0.0, 1.0, 1.0], [1.0; 4])),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let sampler = wgpu::BindingResource::Sampler(&pipes.sampler);
        let (textures, bind_group) = match frame.format {
            PixelFormat::Rgba8 => {
                let t = plane_texture(device, size.0, size.1, wgpu::TextureFormat::Rgba8Unorm);
                let v = t.create_view(&Default::default());
                let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("layer-bg"),
                    layout: &pipes.layer_layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&v) },
                        wgpu::BindGroupEntry { binding: 2, resource: sampler },
                    ],
                });
                (vec![t], bg)
            }
            PixelFormat::Nv12 { .. } => {
                let y = plane_texture(device, size.0, size.1, wgpu::TextureFormat::R8Unorm);
                let uv = plane_texture(device, frame.chroma_width(), frame.chroma_height(), wgpu::TextureFormat::Rg8Unorm);
                let (yv, uvv) = (y.create_view(&Default::default()), uv.create_view(&Default::default()));
                let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("nv12-bg"),
                    layout: &pipes.nv12_layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&yv) },
                        wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&uvv) },
                        wgpu::BindGroupEntry { binding: 3, resource: sampler },
                    ],
                });
                (vec![y, uv], bg)
            }
        };
        Self { textures, size, format: frame.format, uniform, bind_group, serial: u64::MAX }
    }

    /// Whether this layer's textures can take `frame` (same size and plane layout).
    pub fn fits(&self, frame: &VideoFrame) -> bool {
        self.size == (frame.width, frame.height)
            && matches!(
                (self.format, frame.format),
                (PixelFormat::Rgba8, PixelFormat::Rgba8) | (PixelFormat::Nv12 { .. }, PixelFormat::Nv12 { .. })
            )
    }

    pub fn pipeline<'a>(&self, pipes: &'a Pipelines) -> &'a wgpu::RenderPipeline {
        match self.format {
            PixelFormat::Rgba8 => &pipes.layer_pipeline,
            PixelFormat::Nv12 { .. } => &pipes.nv12_pipeline,
        }
    }

    /// YUV rows for the uniform (zero for RGBA layers).
    pub fn yuv_rows(&self) -> [[f32; 4]; 3] {
        match self.format {
            PixelFormat::Rgba8 => [[0.0; 4]; 3],
            PixelFormat::Nv12 { matrix, full_range } => yuv_to_rgb_rows(matrix, full_range),
        }
    }

    pub fn upload(&mut self, queue: &wgpu::Queue, frame: &VideoFrame) {
        if frame.serial == self.serial {
            return;
        }
        match frame.format {
            PixelFormat::Rgba8 => write_plane(queue, &self.textures[0], &frame.data, frame.width, frame.height, 4),
            PixelFormat::Nv12 { .. } => {
                let (y, uv) = frame.data.split_at(frame.y_len());
                write_plane(queue, &self.textures[0], y, frame.width, frame.height, 1);
                write_plane(queue, &self.textures[1], uv, frame.chroma_width(), frame.chroma_height(), 2);
            }
        }
        // Colour tags can change mid-stream without a size change.
        self.format = frame.format;
        self.serial = frame.serial;
    }
}

/// Aspect-preserving "contain" rectangle in normalised canvas space.
pub fn fit_rect(content: (u32, u32), canvas: (u32, u32)) -> [f32; 4] {
    let (cw, ch) = (content.0.max(1) as f32, content.1.max(1) as f32);
    let (vw, vh) = (canvas.0.max(1) as f32, canvas.1.max(1) as f32);
    // 1x1 content (solid colours) always fills.
    if content == (1, 1) {
        return [0.0, 0.0, 1.0, 1.0];
    }
    let scale = (vw / cw).min(vh / ch);
    let (w, h) = (cw * scale / vw, ch * scale / vh);
    let (x, y) = ((1.0 - w) / 2.0, (1.0 - h) / 2.0);
    [x, y, x + w, y + h]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn letterbox() {
        let r = fit_rect((1920, 1080), (1920, 1200));
        assert!((r[0]).abs() < 1e-6 && (r[2] - 1.0).abs() < 1e-6);
        assert!((r[1] - 0.05).abs() < 1e-6 && (r[3] - 0.95).abs() < 1e-6);
    }
}

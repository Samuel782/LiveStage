//! Shared wgpu instance / adapter / device.

use crate::{VideoError, VideoResult};
use std::sync::Arc;

/// One device shared by the control UI and every program output so that
/// textures (video frames, canvases) are shared without copies.
pub struct GpuContext {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl GpuContext {
    /// Creates the context. Pass a surface so the chosen adapter can present to it.
    pub async fn new(instance: wgpu::Instance, compatible_surface: Option<&wgpu::Surface<'_>>) -> VideoResult<Arc<Self>> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface,
            })
            .await
            .ok_or(VideoError::NoAdapter)?;
        let info = adapter.get_info();
        tracing::info!(adapter = %info.name, backend = ?info.backend, "GPU adapter selected");
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("livestage-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
                    memory_hints: wgpu::MemoryHints::Performance,
                },
                None,
            )
            .await?;
        device.on_uncaptured_error(Box::new(|e| tracing::error!("wgpu: {e}")));
        Ok(Arc::new(Self { instance, adapter, device, queue }))
    }

    /// Picks a non-sRGB surface format (see [`crate::CANVAS_FORMAT`]).
    pub fn surface_format(&self, surface: &wgpu::Surface<'_>) -> wgpu::TextureFormat {
        let caps = surface.get_capabilities(&self.adapter);
        caps.formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb() && matches!(f, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm))
            .or_else(|| caps.formats.iter().copied().find(|f| !f.is_srgb()))
            .unwrap_or(caps.formats[0])
    }

    /// Configures a surface for low-latency presentation.
    pub fn configure_surface(&self, surface: &wgpu::Surface<'_>, format: wgpu::TextureFormat, size: [u32; 2], vsync: bool) {
        self.configure_surface_latency(surface, format, size, vsync, 1);
    }

    /// Builds a surface configuration (queries the surface's capabilities:
    /// on macOS do this on the main thread).
    pub fn surface_config(&self, surface: &wgpu::Surface<'_>, format: wgpu::TextureFormat, size: [u32; 2], vsync: bool, frames_in_flight: u32) -> wgpu::SurfaceConfiguration {
        let caps = surface.get_capabilities(&self.adapter);
        let present_mode = if vsync {
            wgpu::PresentMode::AutoVsync
        } else if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else {
            wgpu::PresentMode::AutoNoVsync
        };
        wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size[0].max(1),
            height: size[1].max(1),
            present_mode,
            desired_maximum_frame_latency: frames_in_flight.max(1),
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        }
    }

    /// Like [`Self::configure_surface`] with an explicit number of frames in flight.
    pub fn configure_surface_latency(
        &self,
        surface: &wgpu::Surface<'_>,
        format: wgpu::TextureFormat,
        size: [u32; 2],
        vsync: bool,
        frames_in_flight: u32,
    ) {
        let caps = surface.get_capabilities(&self.adapter);
        let present_mode = if vsync {
            wgpu::PresentMode::AutoVsync
        } else if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else {
            wgpu::PresentMode::AutoNoVsync
        };
        surface.configure(
            &self.device,
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format,
                width: size[0].max(1),
                height: size[1].max(1),
                present_mode,
                desired_maximum_frame_latency: frames_in_flight.max(1),
                alpha_mode: caps.alpha_modes[0],
                view_formats: vec![],
            },
        );
    }
}

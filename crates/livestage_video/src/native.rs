//! Zero-copy hand-off of hardware-decoded frames to the renderer.
//!
//! A hardware decoder leaves each frame in GPU memory. Instead of copying it
//! to the CPU (`av_hwframe_transfer_data`) and uploading it again
//! (`write_texture`), the decoder wraps the platform surface in a
//! [`NativeSurface`] and the renderer imports it into `wgpu`:
//!
//! - **macOS**: the VideoToolbox `CVPixelBuffer` is backed by an IOSurface,
//!   whose luma and chroma planes become two Metal textures that the NV12
//!   pipeline samples directly.
//! - **Windows**: the D3D11VA decoder texture is copied GPU → GPU into a
//!   shared NV12 texture, which wgpu's Direct3D 12 device opens through an NT
//!   handle; its planes are then copied into the layer's own textures.
//!
//! Anything that goes wrong (another wgpu backend, a missing device feature,
//! the driver refusing the import) turns zero-copy off for the rest of the
//! process with [`disable`]: decoders go back to the CPU transfer path, and
//! the frame that failed is read back on the CPU with
//! [`NativeSurface::read_nv12`], so nothing is dropped and nothing panics.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

static DISABLED: AtomicBool = AtomicBool::new(false);

/// Whether decoders should hand over native surfaces. Off when
/// `LIVESTAGE_ZEROCOPY=0`, or after an import failed.
pub fn enabled() -> bool {
    static ENV: OnceLock<bool> = OnceLock::new();
    let env = *ENV.get_or_init(|| !matches!(std::env::var("LIVESTAGE_ZEROCOPY").as_deref(), Ok("0" | "off" | "false")));
    env && cfg!(any(target_os = "macos", target_os = "windows")) && !DISABLED.load(Ordering::Acquire)
}

/// Turns zero-copy off for the rest of the process (logged once).
pub fn disable(reason: &str) {
    #[cfg(test)]
    if test_hooks::FAIL_IMPORT.get() {
        // A simulated failure must not switch off the other tests' decoders.
        test_hooks::DISABLED_HERE.set(true);
        return;
    }
    if !DISABLED.swap(true, Ordering::AcqRel) {
        tracing::warn!(%reason, "zero-copy video import unavailable; using the CPU upload path");
    }
}

/// Device features that let the renderer import native frames, to request
/// when the adapter has them.
pub fn wanted_features() -> wgpu::Features {
    if cfg!(target_os = "windows") {
        wgpu::Features::TEXTURE_FORMAT_NV12
    } else {
        wgpu::Features::empty()
    }
}

/// A hardware-decoded NV12 frame still in GPU memory.
pub struct NativeSurface {
    inner: platform::Surface,
}

impl std::fmt::Debug for NativeSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeSurface(..)")
    }
}

/// One plane to copy into a layer texture.
pub(crate) struct PlaneSource {
    pub texture: wgpu::Texture,
    pub aspect: wgpu::TextureAspect,
}

/// The imported planes of a frame.
pub(crate) enum Planes {
    /// Exactly frame-sized `R8Unorm` / `Rg8Unorm` textures to sample directly.
    #[cfg_attr(target_os = "windows", allow(dead_code))]
    Bind { y: wgpu::Texture, uv: wgpu::Texture },
    /// Planes to copy (GPU → GPU) into the layer's own textures: the import
    /// is padded, or is copy-only.
    Copy { y: PlaneSource, uv: PlaneSource },
}

impl NativeSurface {
    /// Imports the frame's planes on `device`. `size` is the visible frame size.
    pub(crate) fn import(&self, device: &wgpu::Device, size: (u32, u32)) -> Result<Planes, String> {
        #[cfg(test)]
        if test_hooks::FAIL_IMPORT.get() {
            return Err("simulated import failure".into());
        }
        // The interop crate panics on misuse; the preconditions are checked
        // first, and this catches whatever the driver still refuses.
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.inner.import(device, size)))
            .unwrap_or_else(|_| Err("the GPU driver refused the native frame".into()))
    }

    /// The frame as tightly packed NV12 bytes (Y plane then CbCr), read back
    /// on the CPU. The fallback when the import fails.
    pub fn read_nv12(&self, size: (u32, u32)) -> Option<Vec<u8>> {
        self.inner.read_nv12(size)
    }
}

/// Copies an NV12 frame's planes, given as row slices with strides, into a
/// tightly packed buffer.
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
fn pack_nv12(size: (u32, u32), y: (&[u8], usize), uv: (&[u8], usize)) -> Option<Vec<u8>> {
    let (w, h) = (size.0 as usize, size.1 as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut out = Vec::with_capacity(w * h + cw * ch * 2);
    for row in 0..h {
        out.extend_from_slice(y.0.get(row * y.1..row * y.1 + w)?);
    }
    for row in 0..ch {
        out.extend_from_slice(uv.0.get(row * uv.1..row * uv.1 + cw * 2)?);
    }
    Some(out)
}

// ------------------------------------------------------------------ macOS --

#[cfg(target_os = "macos")]
mod platform {
    use super::{Planes, PlaneSource};
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::sync::Arc;
    use wgpu_external_frame::io_surface::{Ycbcr420IoSurfaceFrame, Ycbcr420Plane};

    #[allow(non_snake_case)]
    #[link(name = "CoreVideo", kind = "framework")]
    unsafe extern "C" {
        fn CVPixelBufferRetain(buffer: *mut c_void) -> *mut c_void;
        fn CVPixelBufferRelease(buffer: *mut c_void);
        fn CVPixelBufferGetIOSurface(buffer: *mut c_void) -> *mut c_void;
        fn CVPixelBufferGetPixelFormatType(buffer: *mut c_void) -> u32;
        fn CVPixelBufferLockBaseAddress(buffer: *mut c_void, flags: u64) -> i32;
        fn CVPixelBufferUnlockBaseAddress(buffer: *mut c_void, flags: u64) -> i32;
        fn CVPixelBufferGetBaseAddressOfPlane(buffer: *mut c_void, plane: usize) -> *mut c_void;
        fn CVPixelBufferGetBytesPerRowOfPlane(buffer: *mut c_void, plane: usize) -> usize;
        fn CVPixelBufferGetHeightOfPlane(buffer: *mut c_void, plane: usize) -> usize;
    }

    const READ_ONLY: u64 = 1; // kCVPixelBufferLock_ReadOnly
    /// 8-bit bi-planar 4:2:0, video and full range (`420v`, `420f`).
    const NV12_FORMATS: [u32; 2] = [u32::from_be_bytes(*b"420v"), u32::from_be_bytes(*b"420f")];

    /// A retained `CVPixelBuffer`, released on drop.
    struct PixelBuffer(NonNull<c_void>);

    // SAFETY: CVPixelBuffer is a thread-safe, reference-counted CoreFoundation
    // object; this wrapper only retains, reads and releases it.
    unsafe impl Send for PixelBuffer {}
    // SAFETY: as above; the shared operations (plane reads under a read-only
    // lock) are thread-safe.
    unsafe impl Sync for PixelBuffer {}

    impl Drop for PixelBuffer {
        fn drop(&mut self) {
            // SAFETY: balances the retain in `Surface::retain`.
            unsafe { CVPixelBufferRelease(self.0.as_ptr()) };
        }
    }

    pub(super) struct Surface(Arc<PixelBuffer>);

    impl Surface {
        /// Retains a VideoToolbox frame's `CVPixelBuffer` if it is 8-bit NV12.
        ///
        /// # Safety
        /// `buffer` must be a live `CVPixelBufferRef` (FFmpeg's
        /// `AVFrame.data[3]` for `AV_PIX_FMT_VIDEOTOOLBOX`).
        pub(super) unsafe fn retain(buffer: *mut c_void) -> Option<Self> {
            let buffer = NonNull::new(buffer)?;
            // SAFETY: live per the contract.
            let format = unsafe { CVPixelBufferGetPixelFormatType(buffer.as_ptr()) };
            // SAFETY: as above.
            let io_surface = unsafe { CVPixelBufferGetIOSurface(buffer.as_ptr()) };
            if !NV12_FORMATS.contains(&format) || io_surface.is_null() {
                return None;
            }
            // SAFETY: as above; released by `PixelBuffer::drop`.
            let retained = unsafe { CVPixelBufferRetain(buffer.as_ptr()) };
            Some(Self(Arc::new(PixelBuffer(NonNull::new(retained)?))))
        }

        pub(super) fn import(&self, device: &wgpu::Device, size: (u32, u32)) -> Result<Planes, String> {
            // SAFETY: only checks which backend the device runs on.
            if unsafe { device.as_hal::<wgpu::hal::api::Metal>() }.is_none() {
                return Err("the GPU device is not Metal".into());
            }
            // SAFETY: the pixel buffer is retained by `self.0`.
            let io_surface = NonNull::new(unsafe { CVPixelBufferGetIOSurface(self.0 .0.as_ptr()) }).ok_or("pixel buffer has no IOSurface")?;
            // SAFETY: the IOSurface lives as long as its pixel buffer, which
            // the frame keeps as its owner until the GPU is done with it.
            let frame = unsafe { Ycbcr420IoSurfaceFrame::retain(io_surface) }.with_owner(self.0.clone());
            let needs = frame.format().required_features();
            if !device.features().contains(needs) {
                return Err(format!("the GPU device lacks {needs:?}"));
            }
            let y = frame.import(device, Ycbcr420Plane::Luma);
            let uv = frame.import(device, Ycbcr420Plane::Chroma);
            let chroma = (size.0.div_ceil(2), size.1.div_ceil(2));
            #[cfg(test)]
            let exact = !super::test_hooks::FORCE_COPY.get();
            #[cfg(not(test))]
            let exact = true;
            if exact && (y.width(), y.height()) == size && (uv.width(), uv.height()) == chroma {
                Ok(Planes::Bind { y, uv })
            } else {
                // Padded surface: copy the visible region.
                let all = wgpu::TextureAspect::All;
                Ok(Planes::Copy { y: PlaneSource { texture: y, aspect: all }, uv: PlaneSource { texture: uv, aspect: all } })
            }
        }

        pub(super) fn read_nv12(&self, size: (u32, u32)) -> Option<Vec<u8>> {
            let buffer = self.0 .0.as_ptr();
            // SAFETY: the buffer is retained; the planes are read only between
            // a successful lock and the matching unlock, within the lengths
            // CoreVideo reports.
            unsafe {
                if CVPixelBufferLockBaseAddress(buffer, READ_ONLY) != 0 {
                    return None;
                }
                let plane = |i: usize| {
                    let base = CVPixelBufferGetBaseAddressOfPlane(buffer, i) as *const u8;
                    let stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, i);
                    let rows = CVPixelBufferGetHeightOfPlane(buffer, i);
                    (!base.is_null()).then(|| (std::slice::from_raw_parts(base, stride * rows), stride))
                };
                let packed = match (plane(0), plane(1)) {
                    (Some(y), Some(uv)) => super::pack_nv12(size, y, uv),
                    _ => None,
                };
                CVPixelBufferUnlockBaseAddress(buffer, READ_ONLY);
                packed
            }
        }
    }

    impl super::NativeSurface {
        /// Wraps a VideoToolbox frame, or `None` if it is not 8-bit NV12
        /// backed by an IOSurface (the caller then transfers it).
        ///
        /// # Safety
        /// `pixel_buffer` must be a live `CVPixelBufferRef`.
        pub unsafe fn from_cv_pixel_buffer(pixel_buffer: *mut c_void) -> Option<Self> {
            // SAFETY: forwarded contract.
            unsafe { Surface::retain(pixel_buffer) }.map(|inner| Self { inner })
        }
    }
}

// ---------------------------------------------------------------- Windows --

#[cfg(target_os = "windows")]
mod platform {
    use super::{PlaneSource, Planes};
    use parking_lot::Mutex;
    use std::ffi::c_void;
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
    use std::ptr::NonNull;
    use std::sync::Arc;
    use windows::core::Interface as _;
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
    use windows::Win32::Graphics::Dxgi::{IDXGIResource1, DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE};
    use wgpu_external_frame::shared_handle::SharedHandleFrame;

    /// Most shared textures a decoder keeps in flight; past this, frames take
    /// the CPU transfer path until one comes back.
    const MAX_SLOTS: usize = 16;

    /// D3D11 objects used from the decoder and render threads.
    struct D3d {
        device: ID3D11Device,
        context: ID3D11DeviceContext,
    }

    // SAFETY: FFmpeg creates its D3D11VA device multithread-protected
    // (`ID3D10Multithread::SetMultithreadProtected`), so the device and its
    // immediate context serialise calls from any thread.
    unsafe impl Send for D3d {}
    // SAFETY: as above.
    unsafe impl Sync for D3d {}

    /// One shared NV12 texture of a decoder's ring.
    struct Slot {
        d3d: Arc<D3d>,
        texture: ID3D11Texture2D,
        handle: OwnedHandle,
        /// Allocated (even) extent.
        extent: (u32, u32),
        /// The texture opened on the wgpu device, kept across reuses.
        imported: Mutex<Option<wgpu::Texture>>,
    }

    // SAFETY: the texture is a free-threaded D3D11 resource; see `D3d`.
    unsafe impl Send for Slot {}
    // SAFETY: as above.
    unsafe impl Sync for Slot {}

    pub(super) struct Surface(Arc<Slot>);

    /// A decoder's shared textures. Lives on the decoder thread.
    pub struct SharedRing {
        slots: Vec<Arc<Slot>>,
    }

    impl Default for SharedRing {
        fn default() -> Self {
            Self::new()
        }
    }

    impl SharedRing {
        pub fn new() -> Self {
            Self { slots: Vec::new() }
        }

        /// Copies array slice `index` of the D3D11VA decoder texture into a
        /// free shared texture, waiting for the copy to finish on the GPU.
        ///
        /// # Safety
        /// `texture` must be a live `ID3D11Texture2D` in NV12 format (FFmpeg's
        /// `AVFrame.data[0]` for `AV_PIX_FMT_D3D11`, `index` from `data[1]`).
        pub unsafe fn copy_frame(&mut self, texture: *mut c_void, index: u32, size: (u32, u32)) -> Result<super::NativeSurface, String> {
            let ptr = NonNull::new(texture).ok_or("null decoder texture")?;
            // SAFETY: a live ID3D11Texture2D per the contract; borrowed, not released.
            let raw = ptr.as_ptr();
            let src = unsafe { ID3D11Texture2D::from_raw_borrowed(&raw) }.ok_or("null decoder texture")?;
            let extent = (size.0.next_multiple_of(2), size.1.next_multiple_of(2));
            // A slot nobody else holds: no queued frame and no pending GPU read.
            let slot = match self.slots.iter().find(|s| Arc::strong_count(s) == 1 && s.extent == extent) {
                Some(s) => s.clone(),
                None => {
                    self.slots.retain(|s| s.extent == extent);
                    if self.slots.len() >= MAX_SLOTS {
                        return Err("all shared textures are in use".into());
                    }
                    let slot = Arc::new(new_slot(src, extent)?);
                    self.slots.push(slot.clone());
                    slot
                }
            };
            let (w, h) = extent;
            let region = D3D11_BOX { left: 0, top: 0, front: 0, right: w, bottom: h, back: 1 };
            let ctx = &slot.d3d.context;
            // SAFETY: both textures are NV12 on the same device; the box is
            // within the visible (≤ allocated) extent of each.
            unsafe {
                ctx.CopySubresourceRegion(&slot.texture, 0, 0, 0, 0, src, index, Some(&region));
            }
            wait_idle(&slot.d3d)?;
            Ok(super::NativeSurface { inner: Surface(slot) })
        }
    }

    fn new_slot(src: &ID3D11Texture2D, extent: (u32, u32)) -> Result<Slot, String> {
        fn e(what: &'static str) -> impl Fn(windows::core::Error) -> String {
            move |err| format!("{what}: {err}")
        }
        // SAFETY: plain COM calls on a live texture and its device.
        unsafe {
            let device = src.GetDevice().map_err(e("D3D11 device"))?;
            let context = device.GetImmediateContext().map_err(e("D3D11 context"))?;
            let desc = D3D11_TEXTURE2D_DESC {
                Width: extent.0,
                Height: extent.1,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0) as u32,
            };
            let mut texture = None;
            device.CreateTexture2D(&desc, None, Some(&mut texture)).map_err(e("shared NV12 texture"))?;
            let texture = texture.ok_or("shared NV12 texture")?;
            let resource: IDXGIResource1 = texture.cast().map_err(e("IDXGIResource1"))?;
            let handle = resource
                .CreateSharedHandle(None, (DXGI_SHARED_RESOURCE_READ | DXGI_SHARED_RESOURCE_WRITE).0, None)
                .map_err(e("shared handle"))?;
            Ok(Slot {
                d3d: Arc::new(D3d { device, context }),
                texture,
                handle: OwnedHandle::from_raw_handle(handle.0),
                extent,
                imported: Mutex::new(None),
            })
        }
    }

    /// Blocks until the GPU has run everything submitted on the context, so a
    /// copy is complete before another device reads its destination.
    fn wait_idle(d3d: &D3d) -> Result<(), String> {
        let desc = D3D11_QUERY_DESC { Query: D3D11_QUERY_EVENT, MiscFlags: 0 };
        // SAFETY: plain COM calls; `done` is a BOOL-sized out value.
        unsafe {
            let mut query = None;
            d3d.device.CreateQuery(&desc, Some(&mut query)).map_err(|err| format!("D3D11 query: {err}"))?;
            let query = query.ok_or("D3D11 query")?;
            d3d.context.End(&query);
            d3d.context.Flush();
            let started = std::time::Instant::now();
            loop {
                let mut done = 0i32;
                // S_FALSE (not yet) is also `Ok`; `done` tells them apart.
                let polled = d3d.context.GetData(&query, Some((&mut done as *mut i32).cast()), 4, 0);
                if polled.is_ok() && done != 0 {
                    return Ok(());
                }
                if polled.is_err() || started.elapsed() > std::time::Duration::from_millis(250) {
                    return Err("the D3D11 copy did not finish".into());
                }
                std::thread::yield_now();
            }
        }
    }

    impl Surface {
        pub(super) fn import(&self, device: &wgpu::Device, _size: (u32, u32)) -> Result<Planes, String> {
            // SAFETY: only checks which backend the device runs on.
            if unsafe { device.as_hal::<wgpu::hal::api::Dx12>() }.is_none() {
                return Err("the GPU device is not Direct3D 12".into());
            }
            if !device.features().contains(wgpu::Features::TEXTURE_FORMAT_NV12) {
                return Err("the GPU device lacks NV12 textures".into());
            }
            let slot = &self.0;
            let mut cached = slot.imported.lock();
            let texture = match &*cached {
                Some(t) => t.clone(),
                None => {
                    let handle = NonNull::new(slot.handle.as_raw_handle()).ok_or("null shared handle")?;
                    // SAFETY: the handle is a live NT handle to an NV12
                    // texture of `extent`, owned by the slot.
                    let frame = unsafe { SharedHandleFrame::duplicate(handle, slot.extent.0, slot.extent.1, wgpu::TextureFormat::NV12) };
                    cached.insert(frame.import(device)).clone()
                }
            };
            Ok(Planes::Copy {
                y: PlaneSource { texture: texture.clone(), aspect: wgpu::TextureAspect::Plane0 },
                uv: PlaneSource { texture, aspect: wgpu::TextureAspect::Plane1 },
            })
        }

        pub(super) fn read_nv12(&self, size: (u32, u32)) -> Option<Vec<u8>> {
            let slot = &self.0;
            let (w, h) = slot.extent;
            let desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            // SAFETY: plain COM calls; the mapped rows are read within the
            // mapped length (`RowPitch` × rows of both planes) before Unmap.
            unsafe {
                let mut staging = None;
                slot.d3d.device.CreateTexture2D(&desc, None, Some(&mut staging)).ok()?;
                let staging = staging?;
                let ctx = &slot.d3d.context;
                ctx.CopyResource(&staging, &slot.texture);
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).ok()?;
                let pitch = mapped.RowPitch as usize;
                // NV12 staging layout: Y rows, then h/2 interleaved CbCr rows.
                let base = mapped.pData as *const u8;
                let y = std::slice::from_raw_parts(base, pitch * h as usize);
                let uv = std::slice::from_raw_parts(base.add(pitch * h as usize), pitch * (h as usize / 2));
                let packed = super::pack_nv12(size, (y, pitch), (uv, pitch));
                ctx.Unmap(&staging, 0);
                packed
            }
        }
    }
}

// ---------------------------------------------------------- other systems --

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod platform {
    use super::Planes;

    /// No native frames are produced on this platform.
    pub(super) enum Surface {}

    impl Surface {
        pub(super) fn import(&self, _: &wgpu::Device, _: (u32, u32)) -> Result<Planes, String> {
            match *self {}
        }
        pub(super) fn read_nv12(&self, _: (u32, u32)) -> Option<Vec<u8>> {
            match *self {}
        }
    }
}

#[cfg(target_os = "windows")]
pub use platform::SharedRing;

/// Per-thread switches for tests (the renderer imports on the calling thread).
#[cfg(test)]
pub(crate) mod test_hooks {
    use std::cell::Cell;
    thread_local! {
        /// Make imports fail, as an unsupported driver would.
        pub(crate) static FAIL_IMPORT: Cell<bool> = const { Cell::new(false) };
        /// Set when a simulated failure called `disable`.
        pub(crate) static DISABLED_HERE: Cell<bool> = const { Cell::new(false) };
        /// Copy the planes even when they could be bound directly.
        pub(crate) static FORCE_COPY: Cell<bool> = const { Cell::new(false) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_strided_planes() {
        // 3x3 frame: Y stride 4, chroma 2x2 pairs with stride 6.
        let y = [1, 2, 3, 0, 4, 5, 6, 0, 7, 8, 9, 0];
        let uv = [10, 11, 12, 13, 0, 0, 14, 15, 16, 17, 0, 0];
        let packed = pack_nv12((3, 3), (&y, 4), (&uv, 6)).unwrap();
        assert_eq!(packed, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17]);
        assert!(pack_nv12((3, 3), (&y[..8], 4), (&uv, 6)).is_none(), "short plane is refused, not read past");
    }
}

//! NDI® send / receive through the runtime-loaded NDI library.
//!
//! The NDI SDK is proprietary and its runtime ships separately (NDI Tools,
//! the SDK, or bundled with apps), so LiveStage never links it. The library
//! is opened with `libloading` on first use; when it isn't installed every
//! entry point degrades to "unavailable" and the compositor shows a notice.
//!
//! * [`NdiVideoSource`] — receives an NDI source as an ordinary
//!   [`VideoSource`] layer (RGBA, decoded on the receiver's own thread).
//! * [`NdiOutput`] — publishes a program canvas as an NDI source from a
//!   dedicated sender thread (NDI compression never runs on the render thread).
//! * [`discovered_sources`] — background discovery for the UI.
//!
//! The `#[repr(C)]` structs mirror `Processing.NDI.*.h` (v5/v6 ABI, 64-bit);
//! their layout is pinned by tests below.

use crate::source::{next_serial, VideoFrame, VideoSource};
use livestage_core::time::ShowTime;
use parking_lot::Mutex;
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

/// A discovered NDI source on the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NdiSourceInfo {
    pub name: String,
    pub url: Option<String>,
}

// ------------------------------------------------------------------ FFI --

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RawSource {
    pub p_ndi_name: *const c_char,
    /// Union of `p_url_address` / `p_ip_address`.
    pub p_url_address: *const c_char,
}

#[repr(C)]
pub struct RawFindCreate {
    pub show_local_sources: bool,
    pub p_groups: *const c_char,
    pub p_extra_ips: *const c_char,
}

#[repr(C)]
pub struct RawRecvCreateV3 {
    pub source_to_connect_to: RawSource,
    pub color_format: i32,
    pub bandwidth: i32,
    pub allow_video_fields: bool,
    pub p_ndi_recv_name: *const c_char,
}

#[repr(C)]
pub struct RawSendCreate {
    pub p_ndi_name: *const c_char,
    pub p_groups: *const c_char,
    pub clock_video: bool,
    pub clock_audio: bool,
}

#[repr(C)]
pub struct RawVideoFrameV2 {
    pub xres: i32,
    pub yres: i32,
    pub fourcc: u32,
    pub frame_rate_n: i32,
    pub frame_rate_d: i32,
    pub picture_aspect_ratio: f32,
    pub frame_format_type: i32,
    pub timecode: i64,
    pub p_data: *mut u8,
    /// Union of `line_stride_in_bytes` / `data_size_in_bytes`.
    pub line_stride_in_bytes: i32,
    pub p_metadata: *const c_char,
    pub timestamp: i64,
}

impl RawVideoFrameV2 {
    fn zeroed() -> Self {
        // SAFETY: plain-old-data; all-zero is the SDK's documented "empty" frame.
        unsafe { std::mem::zeroed() }
    }
}

const fn fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
    a as u32 | (b as u32) << 8 | (c as u32) << 16 | (d as u32) << 24
}
pub const FOURCC_RGBA: u32 = fourcc(b'R', b'G', b'B', b'A');
pub const FOURCC_RGBX: u32 = fourcc(b'R', b'G', b'B', b'X');
const RECV_COLOR_RGBX_RGBA: i32 = 2;
const RECV_BANDWIDTH_HIGHEST: i32 = 100;
const FRAME_FORMAT_PROGRESSIVE: i32 = 1;
const FRAME_TYPE_VIDEO: i32 = 1;
const FRAME_TYPE_ERROR: i32 = 4;
/// `NDIlib_send_timecode_synthesize`.
const TIMECODE_SYNTHESIZE: i64 = i64::MAX;

type Instance = *mut c_void;

struct Fns {
    destroy: unsafe extern "C" fn(),
    find_create_v2: unsafe extern "C" fn(*const RawFindCreate) -> Instance,
    find_destroy: unsafe extern "C" fn(Instance),
    find_wait_for_sources: unsafe extern "C" fn(Instance, u32) -> bool,
    find_get_current_sources: unsafe extern "C" fn(Instance, *mut u32) -> *const RawSource,
    recv_create_v3: unsafe extern "C" fn(*const RawRecvCreateV3) -> Instance,
    recv_destroy: unsafe extern "C" fn(Instance),
    recv_capture_v2: unsafe extern "C" fn(Instance, *mut RawVideoFrameV2, *mut c_void, *mut c_void, u32) -> i32,
    recv_free_video_v2: unsafe extern "C" fn(Instance, *const RawVideoFrameV2),
    send_create: unsafe extern "C" fn(*const RawSendCreate) -> Instance,
    send_destroy: unsafe extern "C" fn(Instance),
    send_send_video_v2: unsafe extern "C" fn(Instance, *const RawVideoFrameV2),
    send_get_no_connections: unsafe extern "C" fn(Instance, u32) -> i32,
}

/// The loaded NDI library. Function pointers stay valid because the
/// `Library` is owned alongside them and never unloaded while referenced.
pub struct NdiLib {
    fns: Fns,
    pub version: String,
    pub path: PathBuf,
    _lib: libloading::Library,
}

impl Drop for NdiLib {
    fn drop(&mut self) {
        unsafe { (self.fns.destroy)() };
    }
}

/// Library file names to try, in order (explicit paths first, then the
/// platform loader's search path).
fn candidates() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = detect_runtime().into_iter().collect();
    v.push(if cfg!(target_os = "windows") {
        "Processing.NDI.Lib.x64.dll".into()
    } else if cfg!(target_os = "macos") {
        "libndi.dylib".into()
    } else {
        "libndi.so.6".into()
    });
    if cfg!(target_os = "linux") {
        v.push("libndi.so.5".into());
    }
    v
}

impl NdiLib {
    fn load() -> Result<Self, String> {
        let mut last = String::from("NDI runtime not found");
        for path in candidates() {
            // SAFETY: loading a well-known vendor library; its initialisers are trusted.
            let lib = match unsafe { libloading::Library::new(&path) } {
                Ok(l) => l,
                Err(e) => {
                    last = format!("{}: {e}", path.display());
                    continue;
                }
            };
            macro_rules! sym {
                ($name:literal) => {
                    // SAFETY: signatures match the NDI SDK headers (see module docs / tests).
                    *unsafe { lib.get(concat!($name, "\0").as_bytes()) }.map_err(|e| format!("{}: {e}", $name))?
                };
            }
            let initialize: unsafe extern "C" fn() -> bool = sym!("NDIlib_initialize");
            let version: unsafe extern "C" fn() -> *const c_char = sym!("NDIlib_version");
            let fns = Fns {
                destroy: sym!("NDIlib_destroy"),
                find_create_v2: sym!("NDIlib_find_create_v2"),
                find_destroy: sym!("NDIlib_find_destroy"),
                find_wait_for_sources: sym!("NDIlib_find_wait_for_sources"),
                find_get_current_sources: sym!("NDIlib_find_get_current_sources"),
                recv_create_v3: sym!("NDIlib_recv_create_v3"),
                recv_destroy: sym!("NDIlib_recv_destroy"),
                recv_capture_v2: sym!("NDIlib_recv_capture_v2"),
                recv_free_video_v2: sym!("NDIlib_recv_free_video_v2"),
                send_create: sym!("NDIlib_send_create"),
                send_destroy: sym!("NDIlib_send_destroy"),
                send_send_video_v2: sym!("NDIlib_send_send_video_v2"),
                send_get_no_connections: sym!("NDIlib_send_get_no_connections"),
            };
            if !unsafe { initialize() } {
                return Err("NDIlib_initialize failed (unsupported CPU?)".into());
            }
            let version = unsafe { cstr(version()) }.unwrap_or_default();
            tracing::info!(%version, path = %path.display(), "NDI runtime loaded");
            return Ok(Self { fns, version, path, _lib: lib });
        }
        Err(last)
    }
}

unsafe fn cstr(p: *const c_char) -> Option<String> {
    (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

/// The NDI runtime, loaded once per process. `None` when not installed.
pub fn runtime() -> Option<Arc<NdiLib>> {
    static LIB: OnceLock<Option<Arc<NdiLib>>> = OnceLock::new();
    LIB.get_or_init(|| match NdiLib::load() {
        Ok(l) => Some(Arc::new(l)),
        Err(e) => {
            tracing::info!("NDI unavailable: {e}");
            None
        }
    })
    .clone()
}

/// Locates the NDI runtime library, honouring the SDK's environment variables.
pub fn detect_runtime() -> Option<PathBuf> {
    let lib = if cfg!(target_os = "windows") {
        "Processing.NDI.Lib.x64.dll"
    } else if cfg!(target_os = "macos") {
        "libndi.dylib"
    } else {
        "libndi.so.6"
    };
    let mut dirs: Vec<PathBuf> = ["NDI_RUNTIME_DIR_V6", "NDI_RUNTIME_DIR_V5"]
        .iter()
        .filter_map(|v| std::env::var_os(v).map(PathBuf::from))
        .collect();
    if cfg!(target_os = "macos") {
        dirs.push("/usr/local/lib".into());
        dirs.push("/Library/NDI SDK for Apple/lib/macOS".into());
    } else if cfg!(target_os = "linux") {
        dirs.push("/usr/lib".into());
        dirs.push("/usr/local/lib".into());
    }
    dirs.into_iter().map(|d| d.join(lib)).find(|p| p.is_file())
}

// ------------------------------------------------------------- discovery --

struct Finder {
    lib: Arc<NdiLib>,
    inst: Instance,
}

impl Finder {
    fn new(lib: Arc<NdiLib>) -> Option<Self> {
        let create = RawFindCreate { show_local_sources: true, p_groups: std::ptr::null(), p_extra_ips: std::ptr::null() };
        let inst = unsafe { (lib.fns.find_create_v2)(&create) };
        (!inst.is_null()).then_some(Self { lib, inst })
    }

    fn sources(&self, wait_ms: u32) -> Vec<NdiSourceInfo> {
        unsafe { (self.lib.fns.find_wait_for_sources)(self.inst, wait_ms) };
        let mut n = 0u32;
        let list = unsafe { (self.lib.fns.find_get_current_sources)(self.inst, &mut n) };
        if list.is_null() {
            return vec![];
        }
        // SAFETY: the SDK returns `n` sources valid until the next call on this finder.
        unsafe { std::slice::from_raw_parts(list, n as usize) }
            .iter()
            .filter_map(|s| {
                Some(NdiSourceInfo { name: unsafe { cstr(s.p_ndi_name) }?, url: unsafe { cstr(s.p_url_address) } })
            })
            .collect()
    }
}

impl Drop for Finder {
    fn drop(&mut self) {
        unsafe { (self.lib.fns.find_destroy)(self.inst) };
    }
}

/// Sources seen on the network, refreshed by a background thread started on
/// first call. Empty when NDI is unavailable.
pub fn discovered_sources() -> Vec<NdiSourceInfo> {
    static LIST: OnceLock<Arc<Mutex<Vec<NdiSourceInfo>>>> = OnceLock::new();
    let list = LIST.get_or_init(|| {
        let list: Arc<Mutex<Vec<NdiSourceInfo>>> = Default::default();
        if let Some(lib) = runtime() {
            let out = list.clone();
            let _ = std::thread::Builder::new().name("livestage-ndi-find".into()).spawn(move || {
                let Some(finder) = Finder::new(lib) else { return };
                loop {
                    let found = finder.sources(1_000);
                    *out.lock() = found;
                }
            });
        }
        list
    });
    list.lock().clone()
}

// --------------------------------------------------------------- receive --

/// An NDI source received as a video layer.
pub struct NdiVideoSource {
    name: String,
    latest: Arc<Mutex<Option<VideoFrame>>>,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl NdiVideoSource {
    /// Connects to `name` (as shown by discovery, e.g. `"STUDIO (Camera 1)"`).
    pub fn connect(name: &str) -> Option<Self> {
        let lib = runtime()?;
        let latest: Arc<Mutex<Option<VideoFrame>>> = Default::default();
        let running = Arc::new(AtomicBool::new(true));
        let (out, run, source_name) = (latest.clone(), running.clone(), name.to_string());
        let thread = std::thread::Builder::new()
            .name("livestage-ndi-recv".into())
            .spawn(move || receive_loop(&lib, &source_name, &out, &run))
            .ok()?;
        Some(Self { name: name.to_string(), latest, running, thread: Some(thread) })
    }

    pub fn source_name(&self) -> &str {
        &self.name
    }
}

fn receive_loop(lib: &NdiLib, name: &str, out: &Mutex<Option<VideoFrame>>, running: &AtomicBool) {
    let Ok(c_name) = CString::new(name) else { return };
    let recv_name = c"LiveStage";
    let create = RawRecvCreateV3 {
        source_to_connect_to: RawSource { p_ndi_name: c_name.as_ptr(), p_url_address: std::ptr::null() },
        color_format: RECV_COLOR_RGBX_RGBA,
        bandwidth: RECV_BANDWIDTH_HIGHEST,
        allow_video_fields: false,
        p_ndi_recv_name: recv_name.as_ptr(),
    };
    let inst = unsafe { (lib.fns.recv_create_v3)(&create) };
    if inst.is_null() {
        tracing::warn!(source = name, "NDI receiver could not be created");
        return;
    }
    let mut warned = false;
    while running.load(Ordering::Relaxed) {
        let mut v = RawVideoFrameV2::zeroed();
        let kind = unsafe { (lib.fns.recv_capture_v2)(inst, &mut v, std::ptr::null_mut(), std::ptr::null_mut(), 100) };
        match kind {
            FRAME_TYPE_VIDEO => {
                match convert_received(&v) {
                    Some(f) => *out.lock() = Some(f),
                    None if !warned => {
                        warned = true;
                        tracing::warn!(source = name, fourcc = v.fourcc, "unsupported NDI pixel format");
                    }
                    None => {}
                }
                unsafe { (lib.fns.recv_free_video_v2)(inst, &v) };
            }
            FRAME_TYPE_ERROR => std::thread::sleep(Duration::from_millis(100)),
            _ => {}
        }
    }
    unsafe { (lib.fns.recv_destroy)(inst) };
}

/// Copies an RGBA/RGBX NDI frame into a tightly packed `VideoFrame`.
fn convert_received(v: &RawVideoFrameV2) -> Option<VideoFrame> {
    if v.p_data.is_null() || v.xres <= 0 || v.yres <= 0 || !(v.fourcc == FOURCC_RGBA || v.fourcc == FOURCC_RGBX) {
        return None;
    }
    let (w, h) = (v.xres as usize, v.yres as usize);
    let stride = if v.line_stride_in_bytes > 0 { v.line_stride_in_bytes as usize } else { w * 4 };
    // SAFETY: the SDK guarantees `yres * stride` readable bytes until the frame is freed.
    let src = unsafe { std::slice::from_raw_parts(v.p_data, stride * h) };
    let mut data = Vec::with_capacity(w * h * 4);
    for row in src.chunks_exact(stride).take(h) {
        data.extend_from_slice(&row[..w * 4]);
    }
    if v.fourcc == FOURCC_RGBX {
        data.as_chunks_mut::<4>().0.iter_mut().for_each(|p| p[3] = 255);
    }
    Some(VideoFrame::new(w as u32, h as u32, data, next_serial()))
}

impl VideoSource for NdiVideoSource {
    fn frame_at(&mut self, _t: ShowTime) -> Option<VideoFrame> {
        self.latest.lock().clone()
    }
    fn duration(&self) -> Option<ShowTime> {
        None // live
    }
    fn size(&self) -> (u32, u32) {
        self.latest.lock().as_ref().map_or((1920, 1080), |f| (f.width, f.height))
    }
}

impl Drop for NdiVideoSource {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ------------------------------------------------------------------ send --

/// Publishes frames as an NDI source. Frames are handed to a sender thread
/// through a one-slot queue; if the network is slower than the render rate
/// the newest frame wins and older ones are dropped.
pub struct NdiOutput {
    name: String,
    tx: Option<mpsc::SyncSender<VideoFrame>>,
    connections: Arc<AtomicI32>,
    thread: Option<JoinHandle<()>>,
}

impl NdiOutput {
    pub fn new(name: &str, frame_rate: (i32, i32)) -> Option<Self> {
        let lib = runtime()?;
        let c_name = CString::new(name).ok()?;
        let create = RawSendCreate { p_ndi_name: c_name.as_ptr(), p_groups: std::ptr::null(), clock_video: false, clock_audio: false };
        let inst = unsafe { (lib.fns.send_create)(&create) };
        if inst.is_null() {
            tracing::warn!(name, "NDI sender could not be created");
            return None;
        }
        let sender = SendInstance { lib, inst };
        let (tx, rx) = mpsc::sync_channel::<VideoFrame>(1);
        let connections = Arc::new(AtomicI32::new(0));
        let conns = connections.clone();
        let thread = std::thread::Builder::new()
            .name("livestage-ndi-send".into())
            .spawn(move || {
                let s = sender;
                loop {
                    match rx.recv_timeout(Duration::from_millis(500)) {
                        Ok(frame) => s.send(&frame, frame_rate),
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                    conns.store(s.connections(), Ordering::Relaxed);
                }
            })
            .ok()?;
        tracing::info!(name, "NDI output published");
        Some(Self { name: name.to_string(), tx: Some(tx), connections, thread: Some(thread) })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Number of receivers currently connected.
    pub fn connections(&self) -> i32 {
        self.connections.load(Ordering::Relaxed)
    }

    /// Queues an RGBA frame; returns false if it was dropped (sender busy).
    pub fn send(&self, frame: VideoFrame) -> bool {
        self.tx.as_ref().is_some_and(|tx| tx.try_send(frame).is_ok())
    }
}

impl Drop for NdiOutput {
    fn drop(&mut self) {
        drop(self.tx.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct SendInstance {
    lib: Arc<NdiLib>,
    inst: Instance,
}

// SAFETY: an NDI sender instance may be used from any single thread at a time;
// it is moved into (and only used by) the sender thread.
unsafe impl Send for SendInstance {}

impl SendInstance {
    fn send(&self, frame: &VideoFrame, (n, d): (i32, i32)) {
        if frame.format != crate::source::PixelFormat::Rgba8 {
            return;
        }
        let v = RawVideoFrameV2 {
            xres: frame.width as i32,
            yres: frame.height as i32,
            fourcc: FOURCC_RGBA,
            frame_rate_n: n,
            frame_rate_d: d,
            picture_aspect_ratio: 0.0, // square pixels
            frame_format_type: FRAME_FORMAT_PROGRESSIVE,
            timecode: TIMECODE_SYNTHESIZE,
            // The SDK only reads from this buffer during the synchronous call.
            p_data: frame.data.as_ptr() as *mut u8,
            line_stride_in_bytes: frame.width as i32 * 4,
            p_metadata: std::ptr::null(),
            timestamp: 0,
        };
        unsafe { (self.lib.fns.send_send_video_v2)(self.inst, &v) };
    }

    fn connections(&self) -> i32 {
        unsafe { (self.lib.fns.send_get_no_connections)(self.inst, 0) }
    }
}

impl Drop for SendInstance {
    fn drop(&mut self) {
        unsafe { (self.lib.fns.send_destroy)(self.inst) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    /// Pins the ABI against the NDI SDK headers (64-bit targets).
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn struct_layouts_match_sdk() {
        assert_eq!(size_of::<RawSource>(), 16);
        assert_eq!((offset_of!(RawFindCreate, p_groups), size_of::<RawFindCreate>()), (8, 24));
        assert_eq!(offset_of!(RawRecvCreateV3, color_format), 16);
        assert_eq!(offset_of!(RawRecvCreateV3, bandwidth), 20);
        assert_eq!(offset_of!(RawRecvCreateV3, allow_video_fields), 24);
        assert_eq!((offset_of!(RawRecvCreateV3, p_ndi_recv_name), size_of::<RawRecvCreateV3>()), (32, 40));
        assert_eq!((offset_of!(RawSendCreate, clock_video), size_of::<RawSendCreate>()), (16, 24));
        assert_eq!(offset_of!(RawVideoFrameV2, picture_aspect_ratio), 20);
        assert_eq!(offset_of!(RawVideoFrameV2, timecode), 32);
        assert_eq!(offset_of!(RawVideoFrameV2, p_data), 40);
        assert_eq!(offset_of!(RawVideoFrameV2, line_stride_in_bytes), 48);
        assert_eq!(offset_of!(RawVideoFrameV2, p_metadata), 56);
        assert_eq!(size_of::<RawVideoFrameV2>(), 72);
        assert_eq!(FOURCC_RGBA, 0x4142_4752);
    }

    #[test]
    fn converts_rgbx_with_stride() {
        let mut buf = vec![7u8; 2 * 12]; // 2 rows, stride 12, width 2
        let v = RawVideoFrameV2 {
            xres: 2,
            yres: 2,
            fourcc: FOURCC_RGBX,
            line_stride_in_bytes: 12,
            p_data: buf.as_mut_ptr(),
            ..RawVideoFrameV2::zeroed()
        };
        let f = convert_received(&v).unwrap();
        assert_eq!(f.data.len(), 16);
        assert!(f.data.chunks(4).all(|p| p == [7, 7, 7, 255]));
    }

    /// Live loopback through the installed NDI runtime: publish a frame,
    /// discover it, receive it. Skipped when NDI isn't installed.
    #[test]
    fn loopback_send_discover_receive() {
        if runtime().is_none() {
            eprintln!("skipping: NDI runtime not installed");
            return;
        }
        let name = format!("LiveStage test {}", std::process::id());
        let out = NdiOutput::new(&name, (30, 1)).expect("sender");
        let (w, h) = (64u32, 32u32);
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..w * h {
            px.extend_from_slice(&[200, 40, 10, 255]);
        }
        let frame = VideoFrame::new(w, h, px, next_serial());

        // Discovery reports "MACHINE (name)".
        let lib = runtime().unwrap();
        let finder = Finder::new(lib).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let full = loop {
            out.send(frame.clone());
            if let Some(s) = finder.sources(200).into_iter().find(|s| s.name.ends_with(&format!("({name})"))) {
                break s.name;
            }
            assert!(std::time::Instant::now() < deadline, "sender not discovered");
        };

        let mut rx = NdiVideoSource::connect(&full).unwrap();
        let got = loop {
            out.send(frame.clone());
            if let Some(f) = rx.frame_at(ShowTime::ZERO) {
                break f;
            }
            assert!(std::time::Instant::now() < deadline, "no frame received");
            std::thread::sleep(Duration::from_millis(30));
        };
        assert_eq!((got.width, got.height), (w, h));
        let c = &got.data[((h / 2 * w + w / 2) * 4) as usize..][..4];
        // NDI's codec is lossy: allow a small error.
        assert!(c[0].abs_diff(200) < 12 && c[1].abs_diff(40) < 12 && c[2].abs_diff(10) < 12, "pixel {c:?}");
        // The sender thread samples the connection count after each send.
        while out.connections() < 1 {
            out.send(frame.clone());
            assert!(std::time::Instant::now() < deadline, "receiver connection not reported");
            std::thread::sleep(Duration::from_millis(30));
        }
    }
}

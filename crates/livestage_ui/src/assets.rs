//! The application icon (`icon/` at the repository root, embedded at build
//! time): the window / taskbar icon, the macOS Dock icon, and an egui
//! texture for the header, dialogs and About box.

// The whole set is embedded; each platform and the tests use some of it.
#![allow(dead_code)]

pub const ICON_16: &[u8] = include_bytes!("../../../icon/16.png");
pub const ICON_32: &[u8] = include_bytes!("../../../icon/32.png");
pub const ICON_64: &[u8] = include_bytes!("../../../icon/64.png");
pub const ICON_128: &[u8] = include_bytes!("../../../icon/128.png");
pub const ICON_256: &[u8] = include_bytes!("../../../icon/256.png");
pub const ICON_512: &[u8] = include_bytes!("../../../icon/512.png");
pub const ICON_1024: &[u8] = include_bytes!("../../../icon/1024.png");

/// Decodes one of the embedded PNGs to straight RGBA.
fn decode(png: &[u8]) -> Option<image::RgbaImage> {
    match image::load_from_memory_with_format(png, image::ImageFormat::Png) {
        Ok(img) => Some(img.into_rgba8()),
        Err(e) => {
            tracing::warn!("application icon could not be decoded: {e}");
            None
        }
    }
}

/// Titlebar / taskbar icon for winit windows (Windows, X11; macOS ignores
/// it and uses [`set_dock_icon`]).
pub fn window_icon() -> Option<winit::window::Icon> {
    let img = decode(ICON_256)?;
    let (w, h) = img.dimensions();
    winit::window::Icon::from_rgba(img.into_raw(), w, h).map_err(|e| tracing::warn!("window icon rejected: {e}")).ok()
}

/// Sets the Dock (and app switcher) icon. An unbundled binary otherwise
/// shows the generic executable icon. Call on the main thread once the
/// event loop exists.
#[cfg(target_os = "macos")]
pub fn set_dock_icon() {
    use objc2::AllocAnyThread as _;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;
    let Some(mtm) = MainThreadMarker::new() else {
        tracing::warn!("Dock icon not set: not on the main thread");
        return;
    };
    let data = NSData::with_bytes(ICON_1024);
    let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) else {
        tracing::warn!("Dock icon not set: AppKit could not read the PNG");
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    // SAFETY: called on the main thread (checked above) with a valid image.
    unsafe { app.setApplicationIconImage(Some(&image)) };
}

#[cfg(not(target_os = "macos"))]
pub fn set_dock_icon() {}

/// The icon as an egui texture, created on first use and kept in the
/// context's memory. Mipmapped, so it stays crisp from 16 to 128 points.
pub fn icon_texture(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let key = egui::Id::new("livestage-app-icon");
    if let Some(t) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(key)) {
        return Some(t);
    }
    let img = decode(ICON_512)?;
    let size = [img.width() as usize, img.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    let options = egui::TextureOptions { mipmap_mode: Some(egui::TextureFilter::Linear), ..egui::TextureOptions::LINEAR };
    let handle = ctx.load_texture("livestage-app-icon", color, options);
    ctx.data_mut(|d| d.insert_temp(key, handle.clone()));
    Some(handle)
}

/// The icon at `size` points, or nothing if it could not be loaded.
pub fn icon_image(ui: &mut egui::Ui, size: f32) -> egui::Response {
    match icon_texture(ui.ctx()) {
        Some(t) => ui.add(egui::Image::new(&t).fit_to_exact_size(egui::vec2(size, size))),
        None => ui.allocate_response(egui::vec2(size, size), egui::Sense::hover()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_embedded_icon_decodes_at_its_size() {
        for (png, px) in [(ICON_16, 16), (ICON_32, 32), (ICON_64, 64), (ICON_128, 128), (ICON_256, 256), (ICON_512, 512), (ICON_1024, 1024)] {
            let img = decode(png).unwrap();
            assert_eq!(img.dimensions(), (px, px));
            assert_eq!(crate::ico::png_size(png), Some((px, px)));
            // Transparent corners: the rounded square sits on nothing.
            assert_eq!(img.get_pixel(0, 0)[3], 0, "{px}px corner");
        }
        assert!(window_icon().is_some());
    }

    #[test]
    fn texture_is_created_once() {
        let ctx = egui::Context::default();
        let a = icon_texture(&ctx).unwrap();
        let b = icon_texture(&ctx).unwrap();
        assert_eq!(a.id(), b.id());
        assert_eq!(a.size(), [512, 512]);
    }
}

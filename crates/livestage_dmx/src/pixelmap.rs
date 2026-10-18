//! Pixel mapper: samples a region of an RGBA canvas onto an LED matrix and
//! writes the result into consecutive DMX universes.

use crate::universe::Universe;
use livestage_core::dmx_config::{PixelMapConfig, PixelOrder, PixelWiring};
use std::collections::BTreeMap;

/// Borrowed RGBA8 image (row-major, tightly packed).
#[derive(Clone, Copy)]
pub struct RgbaView<'a> {
    pub width: u32,
    pub height: u32,
    pub data: &'a [u8],
}

impl RgbaView<'_> {
    /// Box-filtered average over a normalised rectangle (anti-aliased
    /// downsampling, so fine video detail doesn't shimmer on LEDs).
    fn average(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> [u8; 3] {
        let px0 = ((x0 * self.width as f32).floor() as u32).min(self.width.saturating_sub(1));
        let py0 = ((y0 * self.height as f32).floor() as u32).min(self.height.saturating_sub(1));
        let px1 = ((x1 * self.width as f32).ceil() as u32).clamp(px0 + 1, self.width);
        let py1 = ((y1 * self.height as f32).ceil() as u32).clamp(py0 + 1, self.height);
        let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
        for y in py0..py1 {
            for x in px0..px1 {
                let i = ((y * self.width + x) * 4) as usize;
                if let Some(p) = self.data.get(i..i + 3) {
                    r += p[0] as u32;
                    g += p[1] as u32;
                    b += p[2] as u32;
                    n += 1;
                }
            }
        }
        if n == 0 {
            return [0; 3];
        }
        [(r / n) as u8, (g / n) as u8, (b / n) as u8]
    }
}

/// Position of matrix pixel `(x, y)` along the physical LED chain.
pub fn chain_index(wiring: PixelWiring, w: u32, h: u32, x: u32, y: u32) -> u32 {
    match wiring {
        PixelWiring::RowsLeftToRight => y * w + x,
        PixelWiring::Serpentine => y * w + if y % 2 == 1 { w - 1 - x } else { x },
        PixelWiring::ColumnSerpentine => x * h + if x % 2 == 1 { h - 1 - y } else { y },
    }
}

/// DMX location `(universe, 1-based channel)` of chain pixel `index`.
/// Pixels never straddle universes.
pub fn pixel_address(cfg: &PixelMapConfig, index: u32) -> (u16, u16) {
    let ch = cfg.order.channels() as u32;
    let start = cfg.start_address.max(1) as u32 - 1;
    let first_capacity = (512 - start.min(511)) / ch;
    if index < first_capacity {
        (cfg.start_universe, (start + index * ch + 1) as u16)
    } else {
        let per = 512 / ch;
        let rest = index - first_capacity;
        (cfg.start_universe + 1 + (rest / per) as u16, ((rest % per) * ch + 1) as u16)
    }
}

fn encode(order: PixelOrder, [r, g, b]: [u8; 3]) -> ([u8; 4], usize) {
    match order {
        PixelOrder::Rgb => ([r, g, b, 0], 3),
        PixelOrder::Grb => ([g, r, b, 0], 3),
        PixelOrder::Brg => ([b, r, g, 0], 3),
        PixelOrder::Rgbw => {
            let w = r.min(g).min(b);
            ([r - w, g - w, b - w, w], 4)
        }
    }
}

/// Renders one pixel map. Universes that don't exist yet are created.
pub fn render(cfg: &PixelMapConfig, src: RgbaView<'_>, universes: &mut BTreeMap<u16, Universe>) {
    if !cfg.enabled || cfg.width == 0 || cfg.height == 0 || src.width == 0 || src.height == 0 {
        return;
    }
    let [rx, ry, rw, rh] = cfg.region;
    let (cw, chh) = (rw / cfg.width as f32, rh / cfg.height as f32);
    for y in 0..cfg.height {
        for x in 0..cfg.width {
            let (x0, y0) = (rx + x as f32 * cw, ry + y as f32 * chh);
            let rgb = src.average(x0, y0, x0 + cw, y0 + chh);
            let idx = chain_index(cfg.wiring, cfg.width, cfg.height, x, y);
            let (uni, addr) = pixel_address(cfg, idx);
            let (bytes, n) = encode(cfg.order, rgb);
            let u = universes.entry(uni).or_default();
            for (k, b) in bytes[..n].iter().enumerate() {
                u.set(addr + k as u16, *b);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use livestage_core::id::OutputId;

    fn cfg(w: u32, h: u32) -> PixelMapConfig {
        PixelMapConfig {
            name: "Matrix".into(),
            source: OutputId::new(),
            region: [0.0, 0.0, 1.0, 1.0],
            width: w,
            height: h,
            order: PixelOrder::Rgb,
            wiring: PixelWiring::Serpentine,
            start_universe: 0,
            start_address: 1,
            enabled: true,
        }
    }

    #[test]
    fn serpentine_wiring() {
        assert_eq!(chain_index(PixelWiring::Serpentine, 4, 2, 0, 1), 7);
        assert_eq!(chain_index(PixelWiring::Serpentine, 4, 2, 3, 1), 4);
    }

    #[test]
    fn universe_spill_170_rgb_pixels() {
        let c = cfg(200, 1);
        assert_eq!(pixel_address(&c, 169), (0, 508));
        assert_eq!(pixel_address(&c, 170), (1, 1));
    }

    #[test]
    fn samples_canvas() {
        // 2x1 image: left red, right blue.
        let data = [255, 0, 0, 255, 0, 0, 255, 255];
        let mut c = cfg(2, 1);
        c.wiring = PixelWiring::RowsLeftToRight;
        let mut unis = BTreeMap::new();
        render(&c, RgbaView { width: 2, height: 1, data: &data }, &mut unis);
        assert_eq!(&unis[&0].data[..6], &[255, 0, 0, 0, 0, 255]);
    }
}

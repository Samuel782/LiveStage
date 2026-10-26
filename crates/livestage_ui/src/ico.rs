//! Minimal Windows `.ico` writer: PNG-compressed entries (Windows Vista
//! and later), used by `build.rs` to turn the PNGs in `icon/` into the
//! executable's icon resource. Also compiled into the crate for its tests.

/// Width and height of a square PNG, from its IHDR chunk.
pub fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || &png[..8] != b"\x89PNG\r\n\x1a\n" || &png[12..16] != b"IHDR" {
        return None;
    }
    let be = |i: usize| u32::from_be_bytes([png[i], png[i + 1], png[i + 2], png[i + 3]]);
    Some((be(16), be(20)))
}

/// Packs PNG images (each at most 256 × 256) into one `.ico` file.
pub fn encode(pngs: &[&[u8]]) -> Result<Vec<u8>, String> {
    let count = u16::try_from(pngs.len()).map_err(|_| "too many images")?;
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0]); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    out.extend_from_slice(&count.to_le_bytes());
    let mut offset = 6 + 16 * pngs.len();
    for png in pngs {
        let (w, h) = png_size(png).ok_or("not a PNG")?;
        if w == 0 || h == 0 || w > 256 || h > 256 {
            return Err(format!("{w}x{h} is outside 1..=256"));
        }
        // 256 is stored as 0.
        out.push((w % 256) as u8);
        out.push((h % 256) as u8);
        out.push(0); // palette size
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += png.len();
    }
    for png in pngs {
        out.extend_from_slice(png);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_the_app_icons() {
        let pngs: [&[u8]; 3] = [crate::assets::ICON_16, crate::assets::ICON_32, crate::assets::ICON_256];
        let ico = encode(&pngs).unwrap();
        assert_eq!(&ico[..6], &[0, 0, 1, 0, 3, 0]);
        // Directory entries: sizes (256 stored as 0), 32 bpp, offsets in order.
        let entry = |i: usize| &ico[6 + 16 * i..6 + 16 * (i + 1)];
        assert_eq!((entry(0)[0], entry(1)[0], entry(2)[0]), (16, 32, 0));
        let mut expected = 6 + 16 * 3;
        for (i, png) in pngs.iter().enumerate() {
            let e = entry(i);
            assert_eq!(u16::from_le_bytes([e[6], e[7]]), 32);
            let (len, at) = (u32::from_le_bytes(e[8..12].try_into().unwrap()) as usize, u32::from_le_bytes(e[12..16].try_into().unwrap()) as usize);
            assert_eq!((len, at), (png.len(), expected));
            assert_eq!(&ico[at..at + len], *png);
            expected += len;
        }
        assert_eq!(ico.len(), expected);
        assert!(encode(&[crate::assets::ICON_1024]).is_err(), "ICO entries stop at 256 px");
        assert!(encode(&[b"not a png"]).is_err());
    }
}

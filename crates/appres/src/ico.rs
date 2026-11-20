//! Multi-resolution `.ico` files.
//!
//! Layout: ICONDIR (reserved 0, type 1, count), then one 16-byte ICONDIRENTRY per frame (width, height, colour
//! count 0, reserved 0, planes 1, bit count 32, byte size, file offset; a 256-pixel side is stored as 0), then
//! the frame data in entry order. A frame is either a complete PNG file (Windows Vista and later; used from
//! 64 px up, where it is much smaller) or a DIB: a BITMAPINFOHEADER with twice the height, 32-bit BGRA rows
//! bottom-up, then the 1-bit AND mask rows (bottom-up, each padded to 32 bits, 1 = transparent).

use crate::{u16_at, u32_at};

/// The eight bytes every PNG file starts with.
pub const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// One square icon image: its side in pixels and its encoded data (PNG file or DIB, see the module header).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub size: u32,
    pub data: Vec<u8>,
}

impl Frame {
    /// Whether the frame data is a PNG file.
    pub fn is_png(&self) -> bool {
        self.data.starts_with(&PNG_SIGNATURE)
    }

    /// The 16-byte directory fields shared by ICONDIRENTRY and GRPICONDIRENTRY, without the last field (file
    /// offset, or resource id): width, height, colour count, reserved, planes, bit count, byte size.
    pub fn dir_fields(&self) -> Vec<u8> {
        let side = if self.size >= 256 { 0 } else { self.size as u8 };
        let mut e = vec![side, side, 0, 0];
        e.extend_from_slice(&1u16.to_le_bytes());
        e.extend_from_slice(&32u16.to_le_bytes());
        e.extend_from_slice(&(self.data.len() as u32).to_le_bytes());
        e
    }
}

/// Encodes a straight-alpha RGBA image (`size` x `size`, rows top-down) as an icon DIB (32-bit BGRA with an
/// AND mask that marks fully transparent pixels).
pub fn bmp_frame(size: u32, rgba: &[u8]) -> Frame {
    let n = size as usize;
    assert_eq!(rgba.len(), n * n * 4, "RGBA buffer does not match the size");
    let mask_stride = n.div_ceil(32) * 4;
    let xor_len = n * n * 4;
    let mut d = Vec::with_capacity(40 + xor_len + mask_stride * n);
    d.extend_from_slice(&40u32.to_le_bytes()); // biSize
    d.extend_from_slice(&(size as i32).to_le_bytes()); // biWidth
    d.extend_from_slice(&(2 * size as i32).to_le_bytes()); // biHeight: XOR image and AND mask
    d.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    d.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    d.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
    d.extend_from_slice(&((xor_len + mask_stride * n) as u32).to_le_bytes()); // biSizeImage
    d.extend_from_slice(&[0; 16]); // resolution, colours used, colours important
    for y in (0..n).rev() {
        for x in 0..n {
            let p = &rgba[(y * n + x) * 4..][..4];
            d.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
    }
    for y in (0..n).rev() {
        let mut row = vec![0u8; mask_stride];
        for x in 0..n {
            if rgba[(y * n + x) * 4 + 3] == 0 {
                row[x / 8] |= 0x80 >> (x % 8);
            }
        }
        d.extend_from_slice(&row);
    }
    Frame { size, data: d }
}

/// Writes an `.ico` file holding `frames` in the given order.
pub fn write(frames: &[Frame]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(frames.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * frames.len();
    for f in frames {
        out.extend_from_slice(&f.dir_fields());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += f.data.len();
    }
    for f in frames {
        out.extend_from_slice(&f.data);
    }
    out
}

/// Reads an `.ico` file back into its frames (directory order). Checks the header, the entry fields this
/// project writes (planes 1, 32 bits) and that every frame lies inside the file.
pub fn parse(bytes: &[u8]) -> Result<Vec<Frame>, String> {
    let short = || "truncated ICO file".to_string();
    if u16_at(bytes, 0).ok_or_else(short)? != 0 || u16_at(bytes, 2).ok_or_else(short)? != 1 {
        return Err("not an ICO file (ICONDIR reserved 0, type 1 expected)".into());
    }
    let count = u16_at(bytes, 4).ok_or_else(short)? as usize;
    let mut frames = Vec::with_capacity(count);
    for i in 0..count {
        let e = 6 + 16 * i;
        let w = *bytes.get(e).ok_or_else(short)?;
        let h = *bytes.get(e + 1).ok_or_else(short)?;
        if w != h {
            return Err(format!("entry {i}: not square ({w} x {h})"));
        }
        let planes = u16_at(bytes, e + 4).ok_or_else(short)?;
        let bits = u16_at(bytes, e + 6).ok_or_else(short)?;
        if planes != 1 || bits != 32 {
            return Err(format!(
                "entry {i}: planes {planes}, bit count {bits} (1 and 32 expected)"
            ));
        }
        let len = u32_at(bytes, e + 8).ok_or_else(short)? as usize;
        let at = u32_at(bytes, e + 12).ok_or_else(short)? as usize;
        let data = bytes
            .get(at..at + len)
            .ok_or_else(|| format!("entry {i}: data out of the file"))?;
        frames.push(Frame {
            size: if w == 0 { 256 } else { w as u32 },
            data: data.to_vec(),
        });
    }
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_png(tag: u8) -> Vec<u8> {
        let mut d = PNG_SIGNATURE.to_vec();
        d.extend_from_slice(&[tag; 13]);
        d
    }

    #[test]
    fn header_and_entries() {
        let frames = vec![
            bmp_frame(16, &vec![0u8; 16 * 16 * 4]),
            Frame {
                size: 64,
                data: fake_png(1),
            },
            Frame {
                size: 256,
                data: fake_png(2),
            },
        ];
        let ico = write(&frames);
        assert_eq!(&ico[0..6], &[0, 0, 1, 0, 3, 0]);
        // Entry 0: 16 x 16, 32 bits, offset right after the directory.
        assert_eq!(&ico[6..10], &[16, 16, 0, 0]);
        assert_eq!(u16_at(&ico, 10), Some(1));
        assert_eq!(u16_at(&ico, 12), Some(32));
        assert_eq!(u32_at(&ico, 14), Some(frames[0].data.len() as u32));
        assert_eq!(u32_at(&ico, 18), Some(6 + 16 * 3));
        // Entry 2: 256 is stored as 0.
        assert_eq!(&ico[38..40], &[0, 0]);
        let total: usize = 6 + 48 + frames.iter().map(|f| f.data.len()).sum::<usize>();
        assert_eq!(ico.len(), total);
        // Offsets are contiguous and the PNG frames keep their signature.
        let back = parse(&ico).unwrap();
        assert_eq!(back, frames);
        assert!(!back[0].is_png());
        assert!(back[1].is_png() && back[2].is_png());
        assert_eq!(back[2].size, 256);
        let at = u32_at(&ico, 6 + 32 + 12).unwrap() as usize;
        assert_eq!(&ico[at..at + 8], &PNG_SIGNATURE);
    }

    #[test]
    fn bmp_frame_layout() {
        // 2 x 2 image: top-left opaque red, others transparent.
        let mut rgba = vec![0u8; 2 * 2 * 4];
        rgba[0..4].copy_from_slice(&[255, 0, 0, 255]);
        let f = bmp_frame(2, &rgba);
        let d = &f.data;
        assert_eq!(u32_at(d, 0), Some(40));
        assert_eq!(u32_at(d, 4), Some(2));
        assert_eq!(u32_at(d, 8), Some(4)); // twice the height
        assert_eq!(u16_at(d, 14), Some(32));
        // XOR rows bottom-up: the top row is the second one, its first pixel BGRA = (0, 0, 255, 255).
        assert_eq!(&d[40 + 8..40 + 12], &[0, 0, 255, 255]);
        // AND mask: 4 bytes per row; bottom row all transparent (0b11 << 6), top row only the second pixel.
        assert_eq!(d.len(), 40 + 16 + 8);
        assert_eq!(d[40 + 16], 0b1100_0000);
        assert_eq!(d[40 + 16 + 4], 0b0100_0000);
    }

    #[test]
    fn rejects_bad_files() {
        assert!(parse(&[]).is_err());
        assert!(parse(&[0, 0, 2, 0, 0, 0]).is_err());
        let mut ico = write(&[Frame {
            size: 32,
            data: fake_png(3),
        }]);
        ico.truncate(ico.len() - 1);
        assert!(parse(&ico).is_err());
    }
}

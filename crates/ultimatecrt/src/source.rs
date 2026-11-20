//! Where frames come from: a captured window, a generated test pattern or an image file. The
//! pattern and image sources produce the same [`Frame`] (BGRA, 8 bits) as the capture path.

use std::path::PathBuf;

use crate::{
    capture::Frame,
    patterns::{self, Pattern},
    Res,
};

pub enum Source {
    Capture,
    Pattern { pattern: Pattern, size: (u32, u32) },
    Image(PathBuf),
}

impl Source {
    /// Picks the source from the command line. `--pattern` and `--image` are exclusive.
    pub fn choose(pattern: Option<&str>, size: Option<(u32, u32)>, image: Option<&str>) -> Res<Source> {
        match (pattern, image) {
            (Some(_), Some(_)) => Err("--pattern and --image cannot be combined".into()),
            (Some(p), None) => Ok(Source::Pattern {
                pattern: p.parse()?,
                size: size.unwrap_or(patterns::DEFAULT_SIZE),
            }),
            (None, Some(f)) => {
                if size.is_some() {
                    return Err("--pattern-size only applies to --pattern".into());
                }
                Ok(Source::Image(f.into()))
            }
            (None, None) => {
                if size.is_some() {
                    return Err("--pattern-size needs --pattern".into());
                }
                Ok(Source::Capture)
            }
        }
    }

    /// The frame number `n` of a pattern or image source; `None` for capture. Static sources
    /// return the same pixels every time (the caller may upload only once).
    pub fn frame(&self, n: u64) -> Res<Option<Frame>> {
        Ok(match self {
            Source::Capture => None,
            Source::Pattern { pattern, size } => Some(Frame {
                width: size.0,
                height: size.1,
                data: patterns::generate(pattern, size.0, size.1, n),
                seq: n + 1,
                ..Frame::default()
            }),
            Source::Image(path) => {
                let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
                let mut f = decode_png(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
                f.seq = n + 1;
                Some(f)
            }
        })
    }

    pub fn is_animated(&self) -> bool {
        matches!(self, Source::Pattern { pattern, .. } if patterns::is_animated(pattern))
    }
}

/// Decodes a PNG of any colour type and depth to a BGRA frame. Transparency is flattened over
/// black; 16-bit samples are reduced to 8 bits. Values are taken as stored (sRGB-coded).
pub fn decode_png(bytes: &[u8]) -> Res<Frame> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = dec.read_info()?;
    let mut buf = vec![0; r.output_buffer_size().ok_or("image too large")?];
    let info = r.next_frame(&mut buf)?;
    let (w, h) = (info.width, info.height);
    let src = &buf[..info.buffer_size()];
    let n = w as usize * h as usize;
    let mut data = Vec::with_capacity(n * 4);
    let flat = |c: u8, a: u8| ((c as u16 * a as u16 + 127) / 255) as u8;
    match info.color_type {
        png::ColorType::Grayscale => src.iter().for_each(|&g| data.extend_from_slice(&[g, g, g, 255])),
        png::ColorType::GrayscaleAlpha => src.as_chunks::<2>().0.iter().for_each(|p| {
            let g = flat(p[0], p[1]);
            data.extend_from_slice(&[g, g, g, 255]);
        }),
        png::ColorType::Rgb => src
            .as_chunks::<3>()
            .0
            .iter()
            .for_each(|p| data.extend_from_slice(&[p[2], p[1], p[0], 255])),
        png::ColorType::Rgba => src
            .as_chunks::<4>()
            .0
            .iter()
            .for_each(|p| data.extend_from_slice(&[flat(p[2], p[3]), flat(p[1], p[3]), flat(p[0], p[3]), 255])),
        png::ColorType::Indexed => return Err("indexed PNG was not expanded".into()),
    }
    if data.len() != n * 4 {
        return Err("unexpected PNG sample count".into());
    }
    Ok(Frame {
        width: w,
        height: h,
        data,
        seq: 0,
        ..Frame::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(
        w: u32,
        h: u32,
        color: png::ColorType,
        depth: png::BitDepth,
        bytes: &[u8],
        palette: Option<&[u8]>,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(color);
        enc.set_depth(depth);
        if let Some(p) = palette {
            enc.set_palette(p.to_vec());
        }
        let mut wr = enc.write_header().unwrap();
        wr.write_image_data(bytes).unwrap();
        wr.finish().unwrap();
        out
    }

    #[test]
    fn rgb_and_rgba() {
        let png = encode(
            2,
            1,
            png::ColorType::Rgb,
            png::BitDepth::Eight,
            &[1, 2, 3, 10, 20, 30],
            None,
        );
        let f = decode_png(&png).unwrap();
        assert_eq!((f.width, f.height), (2, 1));
        assert_eq!(f.data, [3, 2, 1, 255, 30, 20, 10, 255]);
        // Alpha 0 flattens to black, 255 keeps the colour.
        let png = encode(
            2,
            1,
            png::ColorType::Rgba,
            png::BitDepth::Eight,
            &[9, 8, 7, 0, 9, 8, 7, 255],
            None,
        );
        let f = decode_png(&png).unwrap();
        assert_eq!(f.data, [0, 0, 0, 255, 7, 8, 9, 255]);
    }

    #[test]
    fn grey_16_bit_and_indexed() {
        let png = encode(
            2,
            1,
            png::ColorType::Grayscale,
            png::BitDepth::Sixteen,
            &[0x80, 0x00, 0xFF, 0xFF],
            None,
        );
        let f = decode_png(&png).unwrap();
        assert_eq!(f.data, [128, 128, 128, 255, 255, 255, 255, 255]);
        let png = encode(
            2,
            1,
            png::ColorType::Indexed,
            png::BitDepth::Eight,
            &[1, 0],
            Some(&[0, 0, 0, 4, 5, 6]),
        );
        let f = decode_png(&png).unwrap();
        assert_eq!(f.data, [6, 5, 4, 255, 0, 0, 0, 255]);
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(decode_png(b"not a png").is_err());
    }

    #[test]
    fn choose_rules() {
        assert!(matches!(Source::choose(None, None, None).unwrap(), Source::Capture));
        assert!(Source::choose(Some("ramp"), None, Some("a.png")).is_err());
        assert!(Source::choose(None, Some((4, 4)), None).is_err());
        assert!(Source::choose(None, Some((4, 4)), Some("a.png")).is_err());
        assert!(Source::choose(Some("nope"), None, None).is_err());
        match Source::choose(Some("ramp"), None, None).unwrap() {
            Source::Pattern { size, .. } => assert_eq!(size, patterns::DEFAULT_SIZE),
            _ => panic!(),
        }
    }

    #[test]
    fn frames() {
        assert!(Source::Capture.frame(0).unwrap().is_none());
        let s = Source::choose(Some("bar:1:1"), Some((4, 1)), None).unwrap();
        assert!(s.is_animated());
        let a = s.frame(0).unwrap().unwrap();
        let b = s.frame(1).unwrap().unwrap();
        assert_eq!((a.width, a.height, a.data.len()), (4, 1, 16));
        assert_ne!(a.data, b.data);
        assert!(Source::Image("does-not-exist.png".into()).frame(0).is_err());
    }
}

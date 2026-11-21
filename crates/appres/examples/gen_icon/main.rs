//! Generates the application icons (Phoxium and pixel-demo) and a preview sheet.
//!
//! Run from anywhere: `cargo run -p appres --example gen_icon --release`. Writes, relative to the workspace root:
//! - `assets/icon/icon.ico` (Phoxium, every size), `assets/icon/icon-256.png`, `assets/icon/icon.svg` (the
//!   master design as a vector approximation, without the barrel warp and the stripe anti-aliasing),
//! - `assets/icon/pixel-demo.ico`, `assets/icon/pixel-demo-256.png`,
//! - `target/icon-preview/` (not committed): every size as PNG, 16 and 32 px enlarged, and `preview.png`, the
//!   sheet of both icons on a dark and a light background.
//!
//! The output is deterministic: the same sources give byte-identical files.

mod draw;
mod svg;

use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
};

use appres::ico;
use draw::Style;

/// Every size in the `.ico` files. Windows picks 16, 20, 24, 32, 40, 48 for small icons and title bars at 100 to
/// 300 % scaling, 32 to 96 for Alt+Tab and the taskbar, 48 to 256 for File Explorer views.
const SIZES: [u32; 10] = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256];
/// Frames at or above this side are stored as PNG in the `.ico`, smaller ones as 32-bit BMP.
const PNG_FROM: u32 = 64;

/// One rendered size: side and straight-alpha sRGB RGBA bytes.
type Image = (u32, Vec<u8>);

fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::High);
        let mut wr = enc.write_header()?;
        wr.write_image_data(rgba)?;
    }
    Ok(out)
}

fn write_png(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<(), Box<dyn Error>> {
    fs::write(path, encode_png(w, h, rgba)?)?;
    Ok(())
}

/// Nearest-neighbour enlargement.
fn enlarge(size: u32, rgba: &[u8], k: u32) -> Vec<u8> {
    let (s, k) = (size as usize, k as usize);
    let mut out = vec![0u8; s * k * s * k * 4];
    for y in 0..s * k {
        for x in 0..s * k {
            let src = ((y / k) * s + x / k) * 4;
            out[(y * s * k + x) * 4..][..4].copy_from_slice(&rgba[src..src + 4]);
        }
    }
    out
}

/// An opaque RGB canvas for the preview sheet.
struct Sheet {
    w: usize,
    h: usize,
    px: Vec<u8>,
}

impl Sheet {
    fn new(w: usize, h: usize) -> Sheet {
        Sheet {
            w,
            h,
            px: vec![0; w * h * 4],
        }
    }

    fn fill(&mut self, x0: usize, y0: usize, w: usize, h: usize, rgb: [u8; 3]) {
        for y in y0..(y0 + h).min(self.h) {
            for x in x0..(x0 + w).min(self.w) {
                self.px[(y * self.w + x) * 4..][..4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
    }

    /// Alpha-blends a straight-alpha image (sRGB-space blend, as most viewers and the shell do).
    fn blit(&mut self, x0: usize, y0: usize, size: usize, rgba: &[u8]) {
        for y in 0..size {
            for x in 0..size {
                let s = &rgba[(y * size + x) * 4..][..4];
                let d = &mut self.px[((y0 + y) * self.w + x0 + x) * 4..][..4];
                let a = s[3] as f32 / 255.0;
                for k in 0..3 {
                    d[k] = (s[k] as f32 * a + d[k] as f32 * (1.0 - a)).round() as u8;
                }
            }
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let assets = root.join("assets").join("icon");
    let preview = root.join("target").join("icon-preview");
    fs::create_dir_all(&assets)?;
    fs::create_dir_all(&preview)?;

    let mut rendered: Vec<(&str, Vec<Image>)> = Vec::new();
    for (name, style) in [("icon", Style::crt as fn(u32) -> Style), ("pixel-demo", Style::demo)] {
        let mut frames = Vec::new();
        let mut images = Vec::new();
        for size in SIZES {
            let rgba = draw::render(&style(size));
            write_png(&preview.join(format!("{name}-{size}.png")), size, size, &rgba)?;
            frames.push(if size >= PNG_FROM {
                ico::Frame {
                    size,
                    data: encode_png(size, size, &rgba)?,
                }
            } else {
                ico::bmp_frame(size, &rgba)
            });
            images.push((size, rgba));
        }
        fs::write(assets.join(format!("{name}.ico")), ico::write(&frames))?;
        let big = &images.last().unwrap().1;
        write_png(&assets.join(format!("{name}-256.png")), 256, 256, big)?;
        for (size, k) in [(16, 8), (24, 6), (32, 4), (48, 3), (256, 2)] {
            let img = &images.iter().find(|(s, _)| *s == size).unwrap().1;
            write_png(
                &preview.join(format!("{name}-{size}-x{k}.png")),
                size * k,
                size * k,
                &enlarge(size, img, k),
            )?;
        }
        rendered.push((name, images));
    }
    fs::write(assets.join("icon.svg"), svg::master(&Style::crt(256)))?;

    // Preview sheet: per icon, one band on a dark and one on a light background; every size at 1:1, then 16 px
    // enlarged 8 times and 32 px enlarged 4 times.
    let gap = 24usize;
    let band = 256 + 2 * gap;
    let width = SIZES.iter().map(|&s| s as usize + gap).sum::<usize>() + gap + 2 * (128 + gap);
    let mut sheet = Sheet::new(width, band * 2 * rendered.len());
    let backgrounds = [[0x20, 0x20, 0x20], [0xF3, 0xF3, 0xF3]];
    for (i, (_, images)) in rendered.iter().enumerate() {
        for (j, bg) in backgrounds.iter().enumerate() {
            let y0 = (i * 2 + j) * band;
            sheet.fill(0, y0, width, band, *bg);
            let mut x = gap;
            for (size, rgba) in images {
                let s = *size as usize;
                sheet.blit(x, y0 + gap + (256 - s) / 2, s, rgba);
                x += s + gap;
            }
            for (size, k) in [(16u32, 8u32), (32, 4)] {
                let img = &images.iter().find(|(s, _)| *s == size).unwrap().1;
                sheet.blit(x, y0 + gap + 64, 128, &enlarge(size, img, k));
                x += 128 + gap;
            }
        }
    }
    let sheet_path = preview.join("preview.png");
    write_png(&sheet_path, sheet.w as u32, sheet.h as u32, &sheet.px)?;
    println!("icons written to {}", assets.display());
    println!("preview sheet: {}", sheet_path.display());
    Ok(())
}

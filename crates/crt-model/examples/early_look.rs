//! T-16, SP-7 early look: a slow CPU reference render (no GPU, no shader).
//!
//! A built-in 256 x 240 pixel-art still goes through the physical chain of the
//! oracle (`crt-model`):
//! - gun law: code / 255 is the drive V, beam current I = V^2.4 (`gun_power`);
//! - beam: every source pixel is a beam segment with its own Gaussian spot, whose
//!   FWHM grows with current (`spot_fwhm_mm`, n = 0.4); horizontally the box of
//!   the pixel is convolved with the spot (erf), vertically each line is an
//!   area-normalised Gaussian, so a flat field keeps its mean (the oracle's
//!   scanline sum, `scanline_sum`, with the line weight 1 / (sigma sqrt(2 pi)));
//! - mask: the T-38 per-host-pixel area integral `Mask::area_mean` (PHY-2
//!   option A), NOT `host_value`, whose tier C path (under 3 px per triad, both
//!   cases here) returns a constant and so removes the mask;
//! - output: tube white = panel white, hard clip at 1, sRGB encoding.
//!
//! The control render is the same beam without the mask (scanlines only), so
//! the pair isolates what the mask adds. The tube image is 4:3 at full height on
//! the owner's 2560 x 1440 panel: 1920 x 1440 host pixels.
//!
//! Run: `cargo run --release -p crt-model --example early_look [out_dir]`.
//! Default output: `<workspace>/target/early_look` (gitignored).

use std::error::Error;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use crt_model::beam::{erf, spot_fwhm_mm};
use crt_model::colour::gun_power;
use crt_model::mask::{self, Grille, Mask, Rect, Slot};
use crt_model::units::sigma_from_fwhm;

const SRC_W: usize = 256;
const SRC_H: usize = 240;
const HOST_W: usize = 1920;
const HOST_H: usize = 1440;
/// Gun gamma (pure power law).
const GAMMA: f64 = 2.4;
/// Spot growth exponent n in d^2 = d0^2 + k I^(2n). [E] CRT.md.
const SPOT_N: f64 = 0.4;
/// Beam supersampling per host-pixel axis.
const SS: usize = 2;
/// Gaussian support in sigmas.
const SUPPORT: f64 = 5.0;

type Rgb = [f64; 3];

struct Case {
    id: &'static str,
    title: &'static str,
    visible_w_mm: f64,
    visible_h_mm: f64,
    mask: Mask,
    /// Spot FWHM at mid drive (V = 0.5) and at peak white. [E] CRT.md working defaults.
    fwhm_mid_mm: f64,
    fwhm_peak_mm: f64,
}

impl Case {
    fn px_per_triad(&self) -> f64 {
        HOST_W as f64 / (self.visible_w_mm / self.mask.triad_pitch_mm())
    }

    /// (d0^2, k) of the spot growth model through the mid and peak FWHM, with the
    /// current normalised to 1 at peak white.
    fn spot_model(&self) -> (f64, f64) {
        let i_mid = gun_power(0.5, GAMMA).powf(2.0 * SPOT_N);
        let (dm, dp) = (self.fwhm_mid_mm.powi(2), self.fwhm_peak_mm.powi(2));
        let k = (dp - dm) / (1.0 - i_mid);
        (dp - k, k)
    }

    fn sigma_mm(&self, current: f64) -> f64 {
        let (d0_sq, k) = self.spot_model();
        sigma_from_fwhm(spot_fwhm_mm(current, d0_sq.sqrt(), k, SPOT_N))
    }
}

fn grille(pitch_mm: f64, visible_h_mm: f64) -> Mask {
    Mask::Grille(Grille {
        pitch_mm,
        stripe_mm: 0.25 * pitch_mm,
        edge_mm: 0.07 * pitch_mm,
        damper_y_mm: mask::damper_positions_mm(2, visible_h_mm),
        damper_mm: 0.02,
        damper_edge_mm: 0.01,
    })
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            id: "pvm20_grille_1p54",
            title: "PVM-20M4U-class, 20 in, aperture grille 0.31 mm",
            visible_w_mm: 386.0,
            visible_h_mm: 290.0,
            mask: grille(0.31, 290.0),
            fwhm_mid_mm: 0.55,
            fwhm_peak_mm: 0.9,
        },
        Case {
            id: "tv26_grille_2p9",
            title: "26 in viewable consumer TV, aperture grille 0.80 mm",
            visible_w_mm: 528.0,
            visible_h_mm: 396.0,
            mask: grille(0.80, 396.0),
            fwhm_mid_mm: 1.1,
            fwhm_peak_mm: 1.75,
        },
        Case {
            id: "tv26_slot_2p9",
            title: "26 in viewable consumer TV, slot mask 0.80 mm",
            visible_w_mm: 528.0,
            visible_h_mm: 396.0,
            mask: Mask::Slot(Slot {
                pitch_mm: 0.80,
                stripe_mm: 0.25 * 0.80,
                edge_mm: 0.07 * 0.80,
                slot_period_mm: 2.5 * 0.80 / 3.0,
                bridge_fraction: 0.15,
            }),
            fwhm_mid_mm: 1.1,
            fwhm_peak_mm: 1.75,
        },
    ]
}

// ---------------------------------------------------------------- source still

fn fill(img: &mut [[u8; 3]], x0: usize, y0: usize, x1: usize, y1: usize, c: [u8; 3]) {
    for y in y0..y1.min(SRC_H) {
        for x in x0..x1.min(SRC_W) {
            img[y * SRC_W + x] = c;
        }
    }
}

fn ellipse(img: &mut [[u8; 3]], cx: f64, cy: f64, rx: f64, ry: f64, c: [u8; 3]) {
    for y in 0..SRC_H {
        for x in 0..SRC_W {
            let (dx, dy) = ((x as f64 + 0.5 - cx) / rx, (y as f64 + 0.5 - cy) / ry);
            if dx * dx + dy * dy <= 1.0 {
                img[y * SRC_W + x] = c;
            }
        }
    }
}

/// Built-in NES-like still (own drawing, no external asset): HUD colour bars and
/// grey ramp, sky, clouds, hill, pipe, block, a small sprite, bricks, and a test
/// strip (1 px lines, checker, single-colour lines, alternate lines).
fn scene() -> Vec<[u8; 3]> {
    const BLACK: [u8; 3] = [0, 0, 0];
    const WHITE: [u8; 3] = [252, 252, 252];
    let mut img = vec![[0x5c, 0x94, 0xfc]; SRC_W * SRC_H];
    // HUD.
    fill(&mut img, 0, 0, SRC_W, 24, BLACK);
    let bars = [
        WHITE,
        [252, 252, 0],
        [0, 252, 252],
        [0, 252, 0],
        [252, 0, 252],
        [252, 0, 0],
        [0, 0, 252],
        [32; 3],
    ];
    for (i, c) in bars.iter().enumerate() {
        fill(&mut img, i * 32, 4, i * 32 + 32, 12, *c);
    }
    for i in 0..16 {
        let v = (i * 17) as u8;
        fill(&mut img, i * 16, 14, i * 16 + 16, 22, [v; 3]);
    }
    // Clouds and hill.
    ellipse(&mut img, 40.0, 50.0, 18.0, 7.0, WHITE);
    ellipse(&mut img, 190.0, 40.0, 22.0, 8.0, WHITE);
    ellipse(&mut img, 200.0, 162.0, 40.0, 30.0, [0, 168, 0]);
    ellipse(&mut img, 200.0, 162.0, 38.0, 28.0, [128, 208, 16]);
    // Block.
    fill(&mut img, 150, 96, 166, 112, BLACK);
    fill(&mut img, 151, 97, 165, 111, [224, 156, 60]);
    for (x, y) in [(152, 98), (163, 98), (152, 109), (163, 109)] {
        fill(&mut img, x, y, x + 1, y + 1, BLACK);
    }
    fill(&mut img, 156, 100, 160, 108, [136, 20, 0]);
    // Pipe: lip and body, dark outline, light highlight.
    fill(&mut img, 94, 128, 130, 136, [0, 88, 0]);
    fill(&mut img, 95, 129, 129, 135, [0, 168, 0]);
    fill(&mut img, 98, 129, 100, 135, [128, 208, 16]);
    fill(&mut img, 96, 136, 128, 160, [0, 88, 0]);
    fill(&mut img, 97, 136, 127, 160, [0, 168, 0]);
    fill(&mut img, 100, 136, 102, 160, [128, 208, 16]);
    // Sprite, 12 x 16.
    let sprite = [
        "...RRRRR....",
        "..RRRRRRRRR.",
        "..BBBSSBS...",
        ".BSBSSSBSSS.",
        ".BSBBSSSBSSS",
        ".BBSSSSBBBB.",
        "...SSSSSSS..",
        "..RRBRRR....",
        ".RRRBRRBRRR.",
        "RRRRBBBBRRRR",
        "SSRBYBBYBRSS",
        "SSSBBBBBBSSS",
        "SSBBBBBBBBSS",
        "..BBB..BBB..",
        ".BBB....BBB.",
        "BBBB....BBBB",
    ];
    for (dy, row) in sprite.iter().enumerate() {
        for (dx, ch) in row.chars().enumerate() {
            let c = match ch {
                'R' => [216, 40, 0],
                'B' => [136, 112, 0],
                'S' => [252, 152, 56],
                'Y' => [252, 224, 168],
                _ => continue,
            };
            img[(144 + dy) * SRC_W + 72 + dx] = c;
        }
    }
    // Bricks.
    for y in 160..200 {
        for x in 0..SRC_W {
            let row = (y - 160) / 8;
            let mortar = (y - 160) % 8 == 0 || (x + if row % 2 == 0 { 0 } else { 8 }) % 16 == 0;
            img[y * SRC_W + x] = if mortar { BLACK } else { [200, 76, 12] };
        }
    }
    // Test strip.
    fill(&mut img, 0, 200, SRC_W, SRC_H, BLACK);
    for y in 204..236 {
        for x in 8..60 {
            if x % 4 == 0 {
                img[y * SRC_W + x] = WHITE;
            }
        }
        for x in 64..112 {
            if (x + y) % 2 == 0 {
                img[y * SRC_W + x] = WHITE;
            }
        }
        for x in 116..148 {
            if x % 4 < 2 {
                img[y * SRC_W + x] = WHITE;
            }
        }
        img[y * SRC_W + 156] = [252, 0, 0];
        img[y * SRC_W + 162] = [0, 252, 0];
        img[y * SRC_W + 168] = [0, 0, 252];
        if y % 2 == 0 {
            fill(&mut img, 176, y, 248, y + 1, WHITE);
        }
    }
    img
}

// ---------------------------------------------------------------- render

struct Renders {
    /// Linear light per host pixel: scanlines only (beam), physical (beam x
    /// area_mean), and the tier C path of `host_value` (for the record).
    beam: Vec<Rgb>,
    physical: Vec<Rgb>,
    tier_c: Vec<Rgb>,
}

fn parallel_rows<T: Send>(out: &mut [T], row_len: usize, f: impl Fn(usize, &mut [T]) + Sync) {
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let rows = out.len() / row_len;
    let per = rows.div_ceil(threads).max(1);
    std::thread::scope(|s| {
        for (chunk_i, chunk) in out.chunks_mut(per * row_len).enumerate() {
            let f = &f;
            s.spawn(move || {
                for (r, row) in chunk.chunks_mut(row_len).enumerate() {
                    f(chunk_i * per + r, row);
                }
            });
        }
    });
}

fn render(case: &Case, src: &[[u8; 3]]) -> Renders {
    let (w, h) = (case.visible_w_mm, case.visible_h_mm);
    let src_px_mm = w / SRC_W as f64;
    let line_mm = h / SRC_H as f64;
    let (hpx_w, hpx_h) = (w / HOST_W as f64, h / HOST_H as f64);
    let sigma_max = case.sigma_mm(1.0);

    // Per source pixel and channel: beam current and spot sigma.
    let current: Vec<Rgb> = src
        .iter()
        .map(|p| p.map(|v| gun_power(f64::from(v) / 255.0, GAMMA)))
        .collect();
    let sigma: Vec<Rgb> = current.iter().map(|i| i.map(|i| case.sigma_mm(i))).collect();

    // Stage 1, per line and sub-column: horizontal current A(x) (box of each pixel
    // convolved with its spot) and the current-weighted vertical sigma.
    let sub_w = HOST_W * SS;
    let mut line_profile = vec![([0.0; 3], [0.0; 3]); SRC_H * sub_w];
    parallel_rows(&mut line_profile, sub_w, |j, row| {
        for (sx, out) in row.iter_mut().enumerate() {
            let x = (sx as f64 + 0.5) / SS as f64 * hpx_w;
            let k0 = ((x - SUPPORT * sigma_max) / src_px_mm).floor().max(0.0) as usize;
            let k1 = (((x + SUPPORT * sigma_max) / src_px_mm).ceil() as usize).min(SRC_W - 1);
            for k in k0..=k1 {
                let p = j * SRC_W + k;
                let (x0, x1) = (k as f64 * src_px_mm, (k + 1) as f64 * src_px_mm);
                for c in 0..3 {
                    let s = sigma[p][c] * std::f64::consts::SQRT_2;
                    let wgt = 0.5 * (erf((x - x0) / s) - erf((x - x1) / s));
                    let a = current[p][c] * wgt;
                    out.0[c] += a;
                    out.1[c] += a * sigma[p][c];
                }
            }
            for c in 0..3 {
                out.1[c] = if out.0[c] > 1e-12 {
                    out.1[c] / out.0[c]
                } else {
                    case.sigma_mm(0.0)
                };
            }
        }
    });

    // Stage 2, per host pixel: vertical sum of area-normalised Gaussian lines,
    // averaged over SS x SS samples; then the mask.
    let mut beam = vec![[0.0; 3]; HOST_W * HOST_H];
    parallel_rows(&mut beam, HOST_W, |hy, row| {
        for (hx, out) in row.iter_mut().enumerate() {
            for sy in 0..SS {
                let y = (hy as f64 + (sy as f64 + 0.5) / SS as f64) * hpx_h;
                let j0 = ((y - SUPPORT * sigma_max) / line_mm - 0.5).floor().max(0.0) as usize;
                let j1 = (((y + SUPPORT * sigma_max) / line_mm - 0.5).ceil().max(0.0) as usize).min(SRC_H - 1);
                for sx in 0..SS {
                    let sub = hx * SS + sx;
                    for j in j0..=j1 {
                        let (a, s) = line_profile[j * sub_w + sub];
                        let d = y - (j as f64 + 0.5) * line_mm;
                        for c in 0..3 {
                            let g = (-d * d / (2.0 * s[c] * s[c])).exp() / (s[c] * (2.0 * std::f64::consts::PI).sqrt());
                            out[c] += a[c] * line_mm * g;
                        }
                    }
                }
            }
            for v in out.iter_mut() {
                *v /= (SS * SS) as f64;
            }
        }
    });

    let ppt = case.px_per_triad();
    let mut physical = vec![[0.0; 3]; HOST_W * HOST_H];
    let mut tier_c = vec![[0.0; 3]; HOST_W * HOST_H];
    let rect = |i: usize| {
        let (hx, hy) = ((i % HOST_W) as f64, (i / HOST_W) as f64);
        Rect::new(hx * hpx_w, hy * hpx_h, (hx + 1.0) * hpx_w, (hy + 1.0) * hpx_h)
    };
    parallel_rows(&mut physical, HOST_W, |hy, row| {
        for (hx, out) in row.iter_mut().enumerate() {
            let i = hy * HOST_W + hx;
            let q = rect(i);
            *out = [0, 1, 2].map(|c| beam[i][c] * case.mask.area_mean(c, &q));
        }
    });
    parallel_rows(&mut tier_c, HOST_W, |hy, row| {
        for (hx, out) in row.iter_mut().enumerate() {
            let i = hy * HOST_W + hx;
            let q = rect(i);
            *out = [0, 1, 2].map(|c| beam[i][c] * case.mask.host_value(c, &q, ppt));
        }
    });
    Renders { beam, physical, tier_c }
}

// ---------------------------------------------------------------- output

fn srgb_oetf(l: f64) -> f64 {
    let l = l.clamp(0.0, 1.0);
    if l <= 0.003_130_8 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

fn encode(img: &[Rgb]) -> Vec<[u8; 3]> {
    img.iter()
        .map(|p| p.map(|v| (srgb_oetf(v) * 255.0).round() as u8))
        .collect()
}

struct Image {
    w: usize,
    h: usize,
    px: Vec<[u8; 3]>,
}

impl Image {
    fn new(w: usize, h: usize, c: [u8; 3]) -> Self {
        Self {
            w,
            h,
            px: vec![c; w * h],
        }
    }

    /// Crop of size (cw, ch) centred on (cx, cy), clamped inside, zoomed by `z`
    /// (nearest neighbour).
    fn crop_zoom(&self, cx: usize, cy: usize, cw: usize, ch: usize, z: usize) -> Image {
        let x0 = cx.saturating_sub(cw / 2).min(self.w - cw);
        let y0 = cy.saturating_sub(ch / 2).min(self.h - ch);
        let mut out = Image::new(cw * z, ch * z, [0; 3]);
        for y in 0..ch * z {
            for x in 0..cw * z {
                out.px[y * cw * z + x] = self.px[(y0 + y / z) * self.w + x0 + x / z];
            }
        }
        out
    }

    fn blit(&mut self, other: &Image, x0: usize, y0: usize) {
        for y in 0..other.h {
            let d = (y0 + y) * self.w + x0;
            self.px[d..d + other.w].copy_from_slice(&other.px[y * other.w..(y + 1) * other.w]);
        }
    }

    fn save(&self, path: &Path) -> Result<(), Box<dyn Error>> {
        let mut enc = png::Encoder::new(BufWriter::new(File::create(path)?), self.w as u32, self.h as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header()?;
        writer.write_image_data(&self.px.concat())?;
        Ok(())
    }
}

/// Host-pixel centre of a source-pixel position.
fn host_of(sx: usize, sy: usize) -> (usize, usize) {
    (sx * HOST_W / SRC_W, sy * HOST_H / SRC_H)
}

/// Montage: left physical, right scanlines only; rows: 1:1 crop 640 x 480,
/// 2x zoom of 320 x 240, 4x zoom of 160 x 120.
fn montage(phys: &Image, ctrl: &Image) -> Image {
    const GAP: usize = 8;
    let rows = [
        (host_of(110, 175), 640, 480, 1),
        (host_of(110, 150), 320, 240, 2),
        (host_of(100, 214), 160, 120, 4),
    ];
    let mut out = Image::new(2 * 640 + 3 * GAP, 3 * 480 + 4 * GAP, [64; 3]);
    for (r, ((cx, cy), cw, ch, z)) in rows.into_iter().enumerate() {
        let y = GAP + r * (480 + GAP);
        out.blit(&phys.crop_zoom(cx, cy, cw, ch, z), GAP, y);
        out.blit(&ctrl.crop_zoom(cx, cy, cw, ch, z), 2 * GAP + 640, y);
    }
    out
}

// ---------------------------------------------------------------- measurements

fn luma(p: &Rgb) -> f64 {
    0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]
}

fn michelson(v: &[f64]) -> f64 {
    let (lo, hi) = v
        .iter()
        .fold((f64::MAX, f64::MIN), |(lo, hi), &x| (lo.min(x), hi.max(x)));
    (hi - lo) / (hi + lo)
}

/// Period in samples of the strongest non-DC DFT component.
fn dominant_period(v: &[f64]) -> f64 {
    let n = v.len();
    let mean = v.iter().sum::<f64>() / n as f64;
    let mut best = (0.0, 1);
    for f in 1..=n / 2 {
        let (mut re, mut im) = (0.0, 0.0);
        for (i, x) in v.iter().enumerate() {
            let a = 2.0 * std::f64::consts::PI * (f * i) as f64 / n as f64;
            re += (x - mean) * a.cos();
            im += (x - mean) * a.sin();
        }
        let p = re * re + im * im;
        if p > best.0 {
            best = (p, f);
        }
    }
    n as f64 / best.1 as f64
}

/// Flat white field through the mask alone (beam flat): column profile averaged
/// over 96 rows away from the damper wires, row profile averaged over 96 columns.
fn flat_field(case: &Case, report: &mut String) {
    let (hpx_w, hpx_h) = (case.visible_w_mm / HOST_W as f64, case.visible_h_mm / HOST_H as f64);
    let y_off = 100;
    let value = |hx: usize, hy: usize| -> Rgb {
        let (x, y) = (hx as f64 * hpx_w, hy as f64 * hpx_h);
        let q = Rect::new(x, y, x + hpx_w, y + hpx_h);
        [0, 1, 2].map(|c| case.mask.area_mean(c, &q))
    };
    let mut cols = vec![[0.0; 3]; HOST_W];
    for (hx, col) in cols.iter_mut().enumerate() {
        for hy in y_off..y_off + 96 {
            let v = value(hx, hy);
            for c in 0..3 {
                col[c] += v[c] / 96.0;
            }
        }
    }
    let mut rows = vec![[0.0; 3]; 512];
    for (i, row) in rows.iter_mut().enumerate() {
        for hx in 0..96 {
            let v = value(hx, y_off + i);
            for c in 0..3 {
                row[c] += v[c] / 96.0;
            }
        }
    }
    let col_y: Vec<f64> = cols.iter().map(luma).collect();
    let row_y: Vec<f64> = rows.iter().map(luma).collect();
    let ch_m: Vec<String> = (0..3)
        .map(|c| format!("{:.3}", michelson(&cols.iter().map(|p| p[c]).collect::<Vec<_>>())))
        .collect();
    let ppt = case.px_per_triad();
    let _ = writeln!(
        report,
        "  flat white through the mask (area_mean), scanlines-only has 0 here:"
    );
    let _ = writeln!(
        report,
        "    horizontal luminance ripple (Michelson) {:.4}, dominant period {:.2} px (alias_period_px n=1: {:.2}, n=2: {:.2})",
        michelson(&col_y),
        dominant_period(&col_y),
        mask::alias_period_px(ppt, 1),
        mask::alias_period_px(ppt, 2),
    );
    let _ = writeln!(
        report,
        "    horizontal per-channel ripple R/G/B (Michelson) {}",
        ch_m.join(" / ")
    );
    let _ = writeln!(
        report,
        "    vertical luminance ripple (Michelson) {:.4}, dominant period {:.2} px",
        michelson(&row_y),
        dominant_period(&row_y)
    );
    let _ = writeln!(report, "    Barten threshold {:.3}", mask::BARTEN_THRESHOLD);
}

fn compare(a: &[[u8; 3]], b: &[[u8; 3]]) -> (f64, f64, f64) {
    let n = a.len() as f64;
    let mut mad = 0.0;
    let (mut ge5, mut ge16) = (0usize, 0usize);
    for (p, q) in a.iter().zip(b) {
        let d: Vec<i32> = (0..3).map(|c| (i32::from(p[c]) - i32::from(q[c])).abs()).collect();
        mad += d.iter().sum::<i32>() as f64 / 3.0;
        let m = *d.iter().max().unwrap_or(&0);
        ge5 += usize::from(m >= 5);
        ge16 += usize::from(m >= 16);
    }
    (mad / n, ge5 as f64 / n, ge16 as f64 / n)
}

fn out_dir() -> PathBuf {
    std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join("early_look")
    })
}

fn main() -> Result<(), Box<dyn Error>> {
    let dir = out_dir();
    fs::create_dir_all(&dir)?;
    let src = scene();
    Image {
        w: SRC_W,
        h: SRC_H,
        px: src.clone(),
    }
    .save(&dir.join("source_256x240.png"))?;
    let mut report = String::new();
    let _ = writeln!(
        report,
        "T-16 SP-7 early look. Host 1920 x 1440 (4:3 at full height on 2560 x 1440)."
    );
    let _ = writeln!(
        report,
        "Mask function: crt_model::mask::Mask::area_mean (not host_value)."
    );
    let _ = writeln!(
        report,
        "Montage: left physical, right scanlines only; rows 1:1, 2x, 4x crops. View at 100 %."
    );
    for case in cases() {
        case.mask.validate()?;
        let t = std::time::Instant::now();
        let r = render(&case, &src);
        let (phys, ctrl, tierc) = (encode(&r.physical), encode(&r.beam), encode(&r.tier_c));
        let img = |px: Vec<[u8; 3]>| Image {
            w: HOST_W,
            h: HOST_H,
            px,
        };
        let (phys, ctrl, tierc) = (img(phys), img(ctrl), img(tierc));
        phys.save(&dir.join(format!("{}_physical.png", case.id)))?;
        ctrl.save(&dir.join(format!("{}_scanlines.png", case.id)))?;
        let (zx, zy) = host_of(110, 160);
        phys.crop_zoom(zx, zy, 960, 720, 2)
            .save(&dir.join(format!("{}_physical_2x.png", case.id)))?;
        ctrl.crop_zoom(zx, zy, 960, 720, 2)
            .save(&dir.join(format!("{}_scanlines_2x.png", case.id)))?;
        montage(&phys, &ctrl).save(&dir.join(format!("{}_montage.png", case.id)))?;

        let (mad, ge5, ge16) = compare(&phys.px, &ctrl.px);
        let (mad_c, ge5_c, _) = compare(&tierc.px, &ctrl.px);
        let lin_mad = r
            .physical
            .iter()
            .zip(&r.beam)
            .map(|(p, b)| (luma(p) - luma(b)).abs())
            .sum::<f64>()
            / r.beam.iter().map(luma).sum::<f64>();
        let clipped = r.physical.iter().filter(|p| p.iter().any(|&v| v > 1.0)).count() as f64 / r.physical.len() as f64;
        let mean = |v: &[Rgb]| v.iter().map(luma).sum::<f64>() / v.len() as f64;
        let (d0_sq, _) = case.spot_model();
        let _ = writeln!(report, "\n[{}] {}", case.id, case.title);
        let _ = writeln!(
            report,
            "  {:.3} px per triad (tier {:?}), line pitch {:.3} mm, spot FWHM mid {} / peak {} mm (floor {:.3} mm), render {:.1} s",
            case.px_per_triad(),
            mask::tier(case.px_per_triad()),
            case.visible_h_mm / SRC_H as f64,
            case.fwhm_mid_mm,
            case.fwhm_peak_mm,
            d0_sq.sqrt(),
            t.elapsed().as_secs_f64()
        );
        let _ = writeln!(
            report,
            "  physical vs scanlines-only: mean abs diff {:.2} codes (8 bit sRGB, mean over channels); pixels with a channel diff >= 5 codes {:.1} %, >= 16 codes {:.1} %",
            mad,
            100.0 * ge5,
            100.0 * ge16
        );
        let _ = writeln!(
            report,
            "  linear luminance: mean abs diff / mean {:.3}; mean luminance physical {:.4} vs scanlines-only {:.4}; pixels clipped (any channel > 1) {:.1} %",
            lin_mad,
            mean(&r.physical),
            mean(&r.beam),
            100.0 * clipped
        );
        let _ = writeln!(
            report,
            "  for the record, tier C host_value path vs scanlines-only: mean abs diff {:.3} codes, pixels >= 5 codes {:.3} % (damper wires only)",
            mad_c,
            100.0 * ge5_c
        );
        flat_field(&case, &mut report);
        eprintln!("{} done in {:.1} s", case.id, t.elapsed().as_secs_f64());
    }
    fs::write(dir.join("metrics.txt"), &report)?;
    println!("{report}\nOutput: {}", dir.canonicalize()?.display());
    Ok(())
}

//! The icon artwork, drawn procedurally: every pixel is the mean of `ss` x `ss` point samples of [`shade`], in
//! linear light, then encoded as straight-alpha sRGB.
//!
//! Layers, back to front: a soft drop shadow, the graphite casing (a superellipse, lit from above: light top
//! rim, darker bottom, thin dark outline), a recess ring around the screen, then the screen (a rounder
//! superellipse, so its sides bulge like a curved faceplate). The picture on the screen is a source image (a
//! pixel-art disc for Phoxium, a small sprite for pixel-demo) seen through the tube model in miniature:
//! barrel warp, scanlines (beam profile per source row), RGB aperture-grille stripes per source pixel, a
//! phosphor glow around the lit area, the dark glass with a vignette and an inner shadow at its edge, and a soft
//! glass reflection. Small sizes use their own simplified, pixel-snapped style (see [`Style::crt`]).

/// sRGB `0xRRGGBB` to linear RGB.
pub fn lin(hex: u32) -> [f32; 3] {
    let c = |v: u32| {
        let s = (v & 0xFF) as f32 / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    [c(hex >> 16), c(hex >> 8), c(hex)]
}

pub fn to_srgb8(v: f32) -> u8 {
    let v = v.clamp(0.0, 1.0);
    let s = if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0 + 0.5) as u8
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn mix3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

fn scale3(a: [f32; 3], k: f32) -> [f32; 3] {
    [a[0] * k, a[1] * k, a[2] * k]
}

fn add3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

/// Superellipse |x/ax|^n + |y/ay|^n = 1 centred on (cx, cy), in pixels.
#[derive(Clone, Copy, Debug)]
pub struct Shape {
    pub cx: f32,
    pub cy: f32,
    pub ax: f32,
    pub ay: f32,
    pub n: f32,
}

impl Shape {
    /// From a pixel box (left, top, right, bottom) and the exponent.
    pub fn from_box(l: f32, t: f32, r: f32, b: f32, n: f32) -> Shape {
        Shape {
            cx: (l + r) / 2.0,
            cy: (t + b) / 2.0,
            ax: (r - l) / 2.0,
            ay: (b - t) / 2.0,
            n,
        }
    }

    /// Approximate signed distance in pixels (negative inside), (F - 1) / |grad F| with F the homogeneous
    /// norm, and the outward unit normal.
    pub fn sdf(&self, x: f32, y: f32) -> (f32, f32, f32) {
        let dx = (x - self.cx) / self.ax;
        let dy = (y - self.cy) / self.ay;
        let (ux, uy) = (dx.abs().max(1e-6), dy.abs().max(1e-6));
        let n = self.n;
        let f = (ux.powf(n) + uy.powf(n)).powf(1.0 / n);
        let k = f.powf(1.0 - n);
        let gx = k * ux.powf(n - 1.0) * dx.signum() / self.ax;
        let gy = k * uy.powf(n - 1.0) * dy.signum() / self.ay;
        let g = (gx * gx + gy * gy).sqrt().max(1e-9);
        ((f - 1.0) / g, gx / g, gy / g)
    }

    pub fn grow(&self, d: f32) -> Shape {
        Shape {
            ax: self.ax + d,
            ay: self.ay + d,
            ..*self
        }
    }
}

/// What the screen shows.
#[derive(Clone, Copy, Debug)]
pub enum Sprite {
    /// A disc of radius `r` px centred on (cx, cy), sampled at the source pixel centres (stepped edges).
    Disc { cx: f32, cy: f32, r: f32 },
    /// A bitmap (`X` lit, anything else dark), one character per source pixel, its top-left cell at the grid
    /// origin.
    Bitmap(&'static [&'static str]),
}

/// One rendered size of one icon.
#[derive(Clone, Debug)]
pub struct Style {
    pub size: u32,
    /// Samples per pixel and axis.
    pub ss: u32,
    pub casing: Shape,
    pub screen: Shape,
    /// Width of the dark recess ring around the screen, px (0 = none).
    pub lip: f32,
    pub casing_top: [f32; 3],
    pub casing_bottom: [f32; 3],
    pub rim: [f32; 3],
    /// Width of the lit rim band and of the outline, px.
    pub rim_w: f32,
    pub outline_w: f32,
    pub outline: [f32; 3],
    pub glass_centre: [f32; 3],
    pub glass_edge: [f32; 3],
    /// Inner shadow width at the screen edge, px.
    pub edge_shadow: f32,
    /// Source grid: origin (px), source pixel width (0 = continuous horizontally) and row height (0 = no rows).
    pub grid_x0: f32,
    pub grid_y0: f32,
    pub cell_w: f32,
    pub cell_h: f32,
    /// Dark share of a row (scanline gap), 0..1, and the softness of the beam edge (share of the row).
    pub scan_gap: f32,
    pub scan_soft: f32,
    /// Aperture-grille stripe contrast (0 = off) and triad pitch (px; the mask is finer than the source pixels).
    pub triad: f32,
    pub triad_pitch: f32,
    pub sprite: Sprite,
    /// Lit colour at the top and bottom of the sprite (linear).
    pub hot_top: [f32; 3],
    pub hot_bottom: [f32; 3],
    /// Overall picture gain.
    pub gain: f32,
    /// Barrel warp of the picture (0 = flat).
    pub warp: f32,
    /// Phosphor glow: strength and radius (px), around a [`Sprite::Disc`].
    pub glow: f32,
    pub glow_r: f32,
    /// Glass reflection strength (0 = none).
    pub highlight: f32,
    /// Drop shadow: opacity, vertical offset and blur (px).
    pub shadow: f32,
    pub shadow_dy: f32,
    pub shadow_blur: f32,
}

/// Phoxium accent (theme.rs `accent`) and the warm highlight above it.
const AMBER: u32 = 0xE9A23B;
const AMBER_HOT: u32 = 0xFFCB55;
const AMBER_DEEP: u32 = 0xE0661C;

/// pixel-demo accent: a cool teal, so the two taskbar buttons differ at a glance.
const TEAL: u32 = 0x4FD1C5;
const TEAL_DEEP: u32 = 0x2BA7B8;

/// The pixel-demo sprite: a small pixel-art creature, 8 x 7.
const CREATURE: [&str; 7] = [
    "..X..X..", //
    "...XX...", //
    "..XXXX..", //
    ".XX.X.X.", //
    "XXXXXXXX", //
    "X.XXXX.X", //
    "..X..X..",
];

impl Style {
    /// Common casing and screen geometry for both icons. Small sizes are snapped to whole pixels.
    fn frame(size: u32) -> (Shape, Shape, f32) {
        let s = size as f32;
        let snap = |v: f32| if size <= 48 { v.round() } else { v };
        let (cl, ct, cr, cb) = (snap(0.03 * s), snap(0.08 * s), snap(0.97 * s), snap(0.91 * s));
        let (sl, st, sr, sb) = if size <= 32 {
            (snap(0.11 * s), snap(0.15 * s), snap(0.89 * s), snap(0.77 * s))
        } else {
            (snap(0.125 * s), snap(0.17 * s), snap(0.875 * s), snap(0.755 * s))
        };
        let casing = Shape::from_box(cl, ct, cr, cb, if size <= 24 { 7.0 } else { 5.0 });
        let screen = Shape::from_box(sl, st, sr, sb, if size <= 24 { 6.0 } else { 3.6 });
        let lip = if size <= 24 { 0.0 } else { (0.022 * s).max(1.0) };
        (casing, screen, lip)
    }

    /// The Phoxium icon at `size` px.
    pub fn crt(size: u32) -> Style {
        let s = size as f32;
        let (casing, screen, lip) = Style::frame(size);
        let sw = 2.0 * screen.ax;
        let sh = 2.0 * screen.ay;
        let large = size >= 128;
        // Source grid: 20 x 15 source pixels over the screen when large (stepped disc, aperture grille). From 20
        // to 64 px: whole-pixel rows of 3 or 4 px with a 1 px scanline gap, the disc continuous along a row.
        // 16 px: no rows, just the glowing disc.
        let (cell_w, cell_h) = if large {
            (sw / 20.0, sh / 15.0)
        } else if size >= 20 {
            (0.0, (sh / 9.0).round().max(3.0))
        } else {
            (0.0, 0.0)
        };
        let (grid_x0, grid_y0) = if cell_h > 0.0 && !large {
            // Rows centred on the screen, on whole pixels.
            let rows = (sh / cell_h).floor();
            (screen.cx, (screen.cy - rows * cell_h / 2.0).round())
        } else {
            (screen.cx - screen.ax, screen.cy - screen.ay)
        };
        let r = if large {
            5.0 * cell_h
        } else if size <= 16 {
            0.36 * sh
        } else if size <= 32 {
            0.40 * sh
        } else {
            0.35 * sh
        };
        Style {
            size,
            ss: if size <= 32 { 16 } else { 8 },
            casing,
            screen,
            lip,
            casing_top: lin(0x434954),
            casing_bottom: lin(0x22252B),
            rim: lin(0x7C8592),
            rim_w: (0.03 * s).max(1.0),
            outline_w: (0.008 * s).max(0.6),
            outline: lin(0x0B0C0E),
            glass_centre: lin(0x161B20),
            glass_edge: lin(0x07090B),
            edge_shadow: (0.05 * s).max(1.0),
            grid_x0,
            grid_y0,
            cell_w,
            cell_h,
            scan_gap: if large {
                0.30
            } else if cell_h > 0.0 {
                1.0 / cell_h
            } else {
                0.0
            },
            scan_soft: if large { 0.18 } else { 0.0 },
            triad: if large { 0.3 } else { 0.0 },
            triad_pitch: s / 64.0,
            sprite: Sprite::Disc {
                cx: screen.cx,
                cy: screen.cy,
                r,
            },
            hot_top: lin(AMBER_HOT),
            hot_bottom: lin(AMBER_DEEP),
            gain: if large { 1.15 } else { 1.0 },
            warp: if large { 0.06 } else { 0.0 },
            glow: if size >= 32 { 0.28 } else { 0.12 },
            glow_r: (0.08 * s).max(1.0),
            highlight: if size >= 32 { 0.07 } else { 0.0 },
            shadow: if size >= 32 { 0.35 } else { 0.0 },
            shadow_dy: 0.02 * s,
            shadow_blur: 0.03 * s,
        }
    }

    /// The pixel-demo icon at `size` px: same casing, flat screen, a teal sprite, no tube effects.
    pub fn demo(size: u32) -> Style {
        let mut st = Style::crt(size);
        let (_, screen, _) = Style::frame(size);
        st.screen.n = 10.0;
        let cols = CREATURE[0].len() as f32;
        let rows = CREATURE.len() as f32;
        let cell = ((2.0 * screen.ax - 2.0) / (cols + 2.0)).min((2.0 * screen.ay - 2.0) / (rows + 1.0));
        let cell = if size <= 48 { cell.floor().max(1.0) } else { cell };
        st.cell_w = cell;
        st.cell_h = cell;
        let snap = |v: f32| if size <= 48 { v.round() } else { v };
        st.grid_x0 = snap(screen.cx - cols * cell / 2.0);
        st.grid_y0 = snap(screen.cy - rows * cell / 2.0);
        st.scan_gap = 0.0;
        st.scan_soft = 0.0;
        st.triad = 0.0;
        st.warp = 0.0;
        st.glow = 0.0;
        st.gain = 1.0;
        st.sprite = Sprite::Bitmap(&CREATURE);
        st.hot_top = lin(TEAL);
        st.hot_bottom = lin(TEAL_DEEP);
        st.glass_centre = lin(0x1A2026);
        st.glass_edge = lin(0x12161A);
        st.highlight = 0.0;
        st
    }
}

/// Premultiplied "over".
fn over(dst: [f32; 4], src: [f32; 4]) -> [f32; 4] {
    let k = 1.0 - src[3];
    [
        src[0] + dst[0] * k,
        src[1] + dst[1] * k,
        src[2] + dst[2] * k,
        src[3] + dst[3] * k,
    ]
}

fn opaque(c: [f32; 3]) -> [f32; 4] {
    [c[0], c[1], c[2], 1.0]
}

/// Premultiplied linear RGBA of the point (x, y) in pixels.
pub fn shade(st: &Style, x: f32, y: f32) -> [f32; 4] {
    let mut out = [0.0f32; 4];
    if st.shadow > 0.0 {
        let sh = Shape {
            cy: st.casing.cy + st.shadow_dy,
            ..st.casing
        };
        let (d, _, _) = sh.sdf(x, y);
        let a = st.shadow * (1.0 - smoothstep(-st.shadow_blur, st.shadow_blur, d));
        out = over(out, [0.0, 0.0, 0.0, a]);
    }
    let (dc, _, ny) = st.casing.sdf(x, y);
    if dc >= 0.0 {
        return out;
    }
    // Casing: vertical gradient, light rim on the top-facing edge, darker bottom edge, thin outline.
    let t = ((y - (st.casing.cy - st.casing.ay)) / (2.0 * st.casing.ay)).clamp(0.0, 1.0);
    let mut c = mix3(st.casing_top, st.casing_bottom, t);
    let inside = -dc;
    let band = 1.0 - smoothstep(0.0, st.rim_w, inside);
    c = add3(c, scale3(st.rim, 0.55 * band * (-ny).max(0.0)));
    c = scale3(c, 1.0 - 0.35 * band * ny.max(0.0));
    let ol = 1.0 - smoothstep(0.0, st.outline_w, inside);
    c = mix3(c, st.outline, 0.85 * ol);
    out = over(out, opaque(c));

    // Recess ring: dark, its lower part catching the light from above.
    if st.lip > 0.0 {
        let (dl, _, _) = st.screen.grow(st.lip).sdf(x, y);
        if dl < 0.0 {
            let q = (y - st.screen.cy) / st.screen.ay;
            let ring = mix3(lin(0x0D0F12), lin(0x30353D), smoothstep(0.2, 1.1, q));
            out = over(out, opaque(ring));
        }
    }

    let (ds, _, _) = st.screen.sdf(x, y);
    if ds >= 0.0 {
        return out;
    }
    out = over(out, opaque(screen(st, x, y, -ds)));
    out
}

/// Linear RGB of the screen at (x, y), `inside` px from its edge.
fn screen(st: &Style, x: f32, y: f32, inside: f32) -> [f32; 3] {
    let sc = &st.screen;
    let p = (x - sc.cx) / sc.ax;
    let q = (y - sc.cy) / sc.ay;
    let r2 = p * p + q * q;
    let mut c = mix3(st.glass_centre, st.glass_edge, smoothstep(0.0, 1.6, r2));

    // Source position: barrel warp around the screen centre (the picture bulges with the faceplate).
    let w = 1.0 + st.warp * r2;
    let xs = sc.cx + (x - sc.cx) * w;
    let ys = sc.cy + (y - sc.cy) * w;

    // Source pixel and row under the point.
    let col = if st.cell_w > 0.0 {
        ((xs - st.grid_x0) / st.cell_w).floor()
    } else {
        0.0
    };
    let (row, fy) = if st.cell_h > 0.0 {
        let g = (ys - st.grid_y0) / st.cell_h;
        (g.floor(), g - g.floor())
    } else {
        (0.0, 0.5)
    };
    let sx = if st.cell_w > 0.0 {
        st.grid_x0 + (col + 0.5) * st.cell_w
    } else {
        xs
    };
    let sy = if st.cell_h > 0.0 {
        st.grid_y0 + (row + 0.5) * st.cell_h
    } else {
        ys
    };

    let (lit, t, glow_d) = match st.sprite {
        Sprite::Disc { cx, cy, r } => {
            // Continuous rows (small sizes): a row is lit as wide as the disc at its widest point inside the
            // row, so the top and bottom rows do not shrink to slivers.
            let dy = if st.cell_w == 0.0 && st.cell_h > 0.0 {
                ((sy - cy).abs() - 0.5 * st.cell_h).max(0.0)
            } else {
                sy - cy
            };
            let lit = (sx - cx).hypot(dy) <= r;
            let t = ((sy - (cy - r)) / (2.0 * r)).clamp(0.0, 1.0);
            let d = ((xs - cx).hypot(ys - cy) - r).max(0.0);
            (lit, t, Some(d))
        }
        Sprite::Bitmap(rows) => {
            let lit = row >= 0.0
                && col >= 0.0
                && rows
                    .get(row as usize)
                    .and_then(|line| line.as_bytes().get(col as usize))
                    .is_some_and(|&b| b == b'X');
            let t = (row / (rows.len() as f32 - 1.0)).clamp(0.0, 1.0);
            (lit, t, None)
        }
    };
    let hot = mix3(st.hot_top, st.hot_bottom, t);

    if lit {
        // Beam profile across the row: flat core, soft edges, dark gap.
        let beam = if st.scan_gap > 0.0 {
            let half = (1.0 - st.scan_gap) / 2.0;
            let d = (fy - 0.5).abs();
            if st.scan_soft > 0.0 {
                1.0 - smoothstep(half - st.scan_soft / 2.0, half + st.scan_soft / 2.0, d)
            } else if d <= half {
                1.0
            } else {
                0.0
            }
        } else {
            1.0
        };
        // Aperture grille: R, G, B stripes across the source pixel, each 80 % of its third.
        let mut mask = [1.0f32; 3];
        if st.triad > 0.0 {
            let ph = ((xs - st.grid_x0) / st.triad_pitch).fract() * 3.0;
            let k = ph.floor().min(2.0) as usize;
            let inner = ph - ph.floor();
            // Stripe edges softened a little so the triad never reads as a grid of gaps.
            let on = 0.75 + 0.25 * smoothstep(0.0, 0.2, inner) * (1.0 - smoothstep(0.8, 1.0, inner));
            for (ch, m) in mask.iter_mut().enumerate() {
                let stripe = if ch == k { 3.0 * on } else { 0.0 };
                *m = 1.0 + st.triad * (stripe - 1.0);
            }
        }
        let e = scale3(hot, beam * st.gain);
        c = add3(c, [e[0] * mask[0], e[1] * mask[1], e[2] * mask[2]]);
    }
    if let (Some(d), true) = (glow_d, st.glow > 0.0) {
        let g = (-(d / st.glow_r).powi(2)).exp();
        c = add3(c, scale3(mix3(lin(AMBER), hot, 0.5), st.glow * g));
    }

    // Inner shadow along the screen edge.
    let k = smoothstep(0.0, st.edge_shadow, inside);
    c = scale3(c, 0.45 + 0.55 * k);

    // Glass reflection: a soft band in the upper left, bounded by a curved edge.
    if st.highlight > 0.0 {
        // Elliptic patch centred beyond the top-left corner: crisp lower-right edge, brightest in the corner.
        let e = ((p + 0.75) / 1.15).powi(2) + ((q + 1.0) / 0.75).powi(2);
        let patch = 1.0 - smoothstep(0.94, 1.0, e);
        c = add3(
            c,
            scale3([1.0, 1.0, 1.0], st.highlight * patch * (0.3 + 0.7 * (1.0 - e).max(0.0))),
        );
    }
    // Hue-preserving compression of the hottest stripes: scale down so the largest channel stays below 1.
    let m = c[0].max(c[1]).max(c[2]);
    if m > 0.85 {
        let target = 0.85 + 0.15 * (1.0 - (-(m - 0.85) / 0.15).exp());
        c = scale3(c, target / m);
    }
    c
}

/// Renders the icon: `size` x `size` straight-alpha sRGB RGBA bytes.
pub fn render(st: &Style) -> Vec<u8> {
    let s = st.size as usize;
    let n = st.ss;
    let inv = 1.0 / (n * n) as f32;
    let mut px = vec![0u8; s * s * 4];
    for py in 0..s {
        for pxl in 0..s {
            let mut acc = [0.0f32; 4];
            for j in 0..n {
                for i in 0..n {
                    let x = pxl as f32 + (i as f32 + 0.5) / n as f32;
                    let y = py as f32 + (j as f32 + 0.5) / n as f32;
                    let c = shade(st, x, y);
                    for k in 0..4 {
                        acc[k] += c[k];
                    }
                }
            }
            let a = acc[3] * inv;
            let o = &mut px[(py * s + pxl) * 4..][..4];
            if a > 0.0 {
                for k in 0..3 {
                    o[k] = to_srgb8(acc[k] * inv / a);
                }
            }
            o[3] = (a.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
    }
    px
}

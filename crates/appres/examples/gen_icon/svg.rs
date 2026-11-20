//! Vector approximation of the master design (the 256 px style): same shapes, colours and source pixels as the
//! raster, without the barrel warp, the beam-edge softening and the hue-preserving compression. The PNG and ICO
//! files are rendered by `draw.rs`, not from this SVG.

use std::fmt::Write;

use crate::draw::{Shape, Sprite, Style};

fn hex(c: [f32; 3]) -> String {
    let b = c.map(crate::draw::to_srgb8);
    format!("#{:02x}{:02x}{:02x}", b[0], b[1], b[2])
}

/// Closed path of a superellipse, 180 points.
fn path(s: &Shape) -> String {
    let mut d = String::new();
    for i in 0..180 {
        let t = i as f32 / 180.0 * std::f32::consts::TAU;
        let (c, sn) = (t.cos(), t.sin());
        let e = 2.0 / s.n;
        let x = s.cx + s.ax * c.signum() * c.abs().powf(e);
        let y = s.cy + s.ay * sn.signum() * sn.abs().powf(e);
        let _ = write!(d, "{}{x:.2},{y:.2}", if i == 0 { "M" } else { "L" });
    }
    d.push('Z');
    d
}

pub fn master(st: &Style) -> String {
    let size = st.size;
    let screen = st.screen;
    let mut lit = String::new();
    if let Sprite::Disc { cx, cy, r } = st.sprite {
        let rows = (2.0 * screen.ay / st.cell_h).round() as i32;
        let cols = (2.0 * screen.ax / st.cell_w).round() as i32;
        for j in 0..rows {
            let sy = st.grid_y0 + (j as f32 + 0.5) * st.cell_h;
            let lit_cols: Vec<i32> = (0..cols)
                .filter(|&i| {
                    let sx = st.grid_x0 + (i as f32 + 0.5) * st.cell_w;
                    (sx - cx).hypot(sy - cy) <= r
                })
                .collect();
            if let (Some(&a), Some(&b)) = (lit_cols.first(), lit_cols.last()) {
                let h = st.cell_h * (1.0 - st.scan_gap);
                let _ = write!(
                    lit,
                    r#"<rect x="{:.2}" y="{:.2}" width="{:.2}" height="{:.2}"/>"#,
                    st.grid_x0 + a as f32 * st.cell_w,
                    sy - h / 2.0,
                    (b - a + 1) as f32 * st.cell_w,
                    h
                );
            }
        }
    }
    let (gx, gy, gr) = match st.sprite {
        Sprite::Disc { cx, cy, r } => (cx, cy, r + 2.5 * st.glow_r),
        Sprite::Bitmap(_) => (screen.cx, screen.cy, screen.ay),
    };
    let lip = screen.grow(st.lip);
    let p = st.triad_pitch;
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" viewBox="0 0 {size} {size}">
  <title>ultimateCRT</title>
  <defs>
    <filter id="shadow" x="-10%" y="-10%" width="120%" height="130%"><feGaussianBlur stdDeviation="{blur:.2}"/></filter>
    <linearGradient id="casing" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{ct}"/><stop offset="1" stop-color="{cb}"/></linearGradient>
    <linearGradient id="rim" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{rim}" stop-opacity="0.9"/><stop offset="0.35" stop-color="{rim}" stop-opacity="0"/></linearGradient>
    <linearGradient id="lip" x1="0" y1="0" x2="0" y2="1"><stop offset="0.55" stop-color="#0d0f12"/><stop offset="1" stop-color="#30353d"/></linearGradient>
    <radialGradient id="glass"><stop offset="0" stop-color="{gc}"/><stop offset="1" stop-color="{ge}"/></radialGradient>
    <radialGradient id="glow"><stop offset="0" stop-color="#e9a23b" stop-opacity="0.55"/><stop offset="1" stop-color="#e9a23b" stop-opacity="0"/></radialGradient>
    <linearGradient id="hot" x1="0" y1="0" x2="0" y2="1"><stop offset="0.2" stop-color="{ht}"/><stop offset="0.8" stop-color="{hb}"/></linearGradient>
    <radialGradient id="sheen" cx="0.1" cy="0.05" r="0.75"><stop offset="0" stop-color="#ffffff" stop-opacity="0.16"/><stop offset="1" stop-color="#ffffff" stop-opacity="0.04"/></radialGradient>
    <pattern id="triad" width="{p:.3}" height="4" patternUnits="userSpaceOnUse">
      <rect width="{p3:.3}" height="4" fill="#ff3010"/><rect x="{p3:.3}" width="{p3:.3}" height="4" fill="#30ff20"/><rect x="{p23:.3}" width="{p3:.3}" height="4" fill="#2040ff"/>
    </pattern>
    <clipPath id="screen"><path d="{screen_path}"/></clipPath>
  </defs>
  <path d="{shadow_path}" fill="#000" opacity="{shadow_op:.2}" filter="url(#shadow)"/>
  <path d="{casing_path}" fill="url(#casing)"/>
  <path d="{casing_path}" fill="url(#rim)"/>
  <path d="{casing_path}" fill="none" stroke="#0b0c0e" stroke-width="{ol:.2}" opacity="0.85"/>
  <path d="{lip_path}" fill="url(#lip)"/>
  <g clip-path="url(#screen)">
    <path d="{screen_path}" fill="url(#glass)"/>
    <circle cx="{gx:.2}" cy="{gy:.2}" r="{gr:.2}" fill="url(#glow)"/>
    <g fill="url(#hot)">{lit}</g>
    <g fill="url(#triad)" opacity="{triad_op:.2}" style="mix-blend-mode:multiply">{lit}</g>
    <ellipse cx="{hx:.2}" cy="{hy:.2}" rx="{hrx:.2}" ry="{hry:.2}" fill="url(#sheen)"/>
    <path d="{screen_path}" fill="none" stroke="#000" stroke-opacity="0.45" stroke-width="{edge:.2}"/>
  </g>
</svg>
"##,
        blur = st.shadow_blur,
        ct = hex(st.casing_top),
        cb = hex(st.casing_bottom),
        rim = hex(st.rim),
        gc = hex(st.glass_centre),
        ge = hex(st.glass_edge),
        ht = hex(st.hot_top),
        hb = hex(st.hot_bottom),
        p3 = p / 3.0,
        p23 = 2.0 * p / 3.0,
        screen_path = path(&screen),
        shadow_path = path(&Shape {
            cy: st.casing.cy + st.shadow_dy,
            ..st.casing
        }),
        shadow_op = st.shadow,
        casing_path = path(&st.casing),
        ol = 2.0 * st.outline_w,
        lip_path = path(&lip),
        triad_op = st.triad,
        hx = screen.cx - 0.75 * screen.ax,
        hy = screen.cy - screen.ay,
        hrx = 1.15 * screen.ax,
        hry = 0.75 * screen.ay,
        edge = 2.0 * st.edge_shadow,
    )
}

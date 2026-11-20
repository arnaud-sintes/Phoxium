//! Burn-in oracle (T-43, QUESTIONS PHY-14, SCP-6): a per-colour wear map on the tube face, accumulated from
//! the displayed beam energy and applied to the phosphor emission. Written before the shader
//! (`shaders/wear.comp`, `shaders/wear_accumulate.comp`) from `CRT.md` "Saturation and ageing" and the wear
//! law of [`crate::ageing`].
//!
//! # Map
//!
//! A fixed grid of `width` x `height` texels covering the visible area of the tube (independent of the host
//! resolution; the tube's mm or, for an unplaced tube, its image). Each texel holds the charge per channel in
//! reference hours (h_ref, see [`crate::ageing`]). Its placement in host pixels of the tube image is
//! [`Placement`]: texel (i, j) covers [origin + i t, origin + (i + 1) t) per axis.
//!
//! # Accumulation
//!
//! Per frame, texel (i, j) gains `scale` x the exact area mean of the frame's light over its footprint
//! (pixels outside the tube image count as black), per channel:
//!
//!   Q_c += scale x (1 / (t_x t_y)) sum_p overlap_x(p) overlap_y(p) x_c(p),
//!
//! with scale = frame hours x acceleration x L_w / 350 (h_ref per unit light). The light x is the input of
//! the wear stage: the beam current times the mask, before any efficiency (the charge, not the light the worn
//! phosphor emits). The sum is monotonic (light is never negative) and the channels are independent.
//!
//! # Application
//!
//! The efficiency of host pixel p is 1 / (1 + c_c (Q_age + Q_c(p))), Q_c(p) the bilinear interpolation of
//! the map at the pixel centre in texel coordinates (texel centres at whole numbers), with Q = 0 outside the
//! map. A pixel whose four neighbouring texels hold no charge has efficiency exactly 1.
//!
//! # Resampling
//!
//! A stored map of another size is resampled bilinearly in normalised map coordinates (texel centres at
//! (i + 1/2) / width), clamped at the edges, so a uniform map stays uniform.

use crate::ageing::WearLaw;

/// Charge per texel and channel, reference hours, rows from top to bottom.
#[derive(Clone, Debug, PartialEq)]
pub struct WearMap {
    pub width: usize,
    pub height: usize,
    pub q: Vec<[f64; 3]>,
}

/// Where the map lies in host pixels of the tube image.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// Host px position of the map's corner (texel (0, 0)'s top-left).
    pub origin_px: [f64; 2],
    /// Host px per texel, per axis.
    pub texel_px: [f64; 2],
}

/// The placement of a `map` grid over a tube image of `tube_px` host px: centred on the image, covering the
/// visible area `visible_mm` at `px_per_mm`; an unplaced tube (`px_per_mm` <= 0) covers the whole image.
pub fn placement(tube_px: [f64; 2], px_per_mm: f64, visible_mm: [f64; 2], map: [usize; 2]) -> Placement {
    let extent = if px_per_mm > 0.0 {
        [visible_mm[0] * px_per_mm, visible_mm[1] * px_per_mm]
    } else {
        tube_px
    };
    Placement {
        origin_px: [0.5 * (tube_px[0] - extent[0]), 0.5 * (tube_px[1] - extent[1])],
        texel_px: [extent[0] / map[0] as f64, extent[1] / map[1] as f64],
    }
}

impl WearMap {
    /// A new tube: no charge anywhere.
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            q: vec![[0.0; 3]; width * height],
        }
    }

    pub fn at(&self, i: usize, j: usize) -> [f64; 3] {
        self.q[j * self.width + i]
    }

    /// Q at texel (i, j), 0 outside the map.
    fn at_or_zero(&self, i: i64, j: i64) -> [f64; 3] {
        if i < 0 || j < 0 || i >= self.width as i64 || j >= self.height as i64 {
            [0.0; 3]
        } else {
            self.at(i as usize, j as usize)
        }
    }

    /// Adds `scale` x the exact area mean of `light` (`w` x `h` host px, rows top to bottom) over each
    /// texel's footprint at `place`.
    pub fn accumulate(&mut self, light: &[[f64; 3]], w: usize, h: usize, place: &Placement, scale: f64) {
        assert_eq!(light.len(), w * h);
        for j in 0..self.height {
            for i in 0..self.width {
                let m = texel_mean(light, w, h, place, i, j);
                let q = &mut self.q[j * self.width + i];
                for c in 0..3 {
                    q[c] += scale * m[c];
                }
            }
        }
    }

    /// Bilinear Q at the centre of host pixel (`px`, `py`).
    pub fn sample(&self, place: &Placement, px: f64, py: f64) -> [f64; 3] {
        let u = (px + 0.5 - place.origin_px[0]) / place.texel_px[0] - 0.5;
        let v = (py + 0.5 - place.origin_px[1]) / place.texel_px[1] - 0.5;
        let (i0, j0) = (u.floor(), v.floor());
        let (fx, fy) = (u - i0, v - j0);
        let (i0, j0) = (i0 as i64, j0 as i64);
        let mut out = [0.0; 3];
        for (di, dj, wgt) in [
            (0, 0, (1.0 - fx) * (1.0 - fy)),
            (1, 0, fx * (1.0 - fy)),
            (0, 1, (1.0 - fx) * fy),
            (1, 1, fx * fy),
        ] {
            let q = self.at_or_zero(i0 + di, j0 + dj);
            for c in 0..3 {
                out[c] += wgt * q[c];
            }
        }
        out
    }

    /// The light of host pixel (`px`, `py`) after the wear: `x` x efficiency, with the tube-wide charge
    /// `q_age` added to the map's.
    pub fn apply(&self, law: &WearLaw, q_age: f64, place: &Placement, px: f64, py: f64, x: [f64; 3]) -> [f64; 3] {
        let q = self.sample(place, px, py);
        let e = law.efficiency([q[0] + q_age, q[1] + q_age, q[2] + q_age]);
        [x[0] * e[0], x[1] * e[1], x[2] * e[2]]
    }

    /// The map resampled to `width` x `height` (bilinear in normalised coordinates, clamped at the edges).
    ///
    /// # Panics
    /// If either map is empty.
    pub fn resample(&self, width: usize, height: usize) -> WearMap {
        assert!(self.width > 0 && self.height > 0 && width > 0 && height > 0);
        let mut out = WearMap::new(width, height);
        for j in 0..height {
            let v = ((j as f64 + 0.5) / height as f64 * self.height as f64 - 0.5).clamp(0.0, (self.height - 1) as f64);
            let j0 = (v.floor() as usize).min(self.height - 1);
            let j1 = (j0 + 1).min(self.height - 1);
            let fy = v - j0 as f64;
            for i in 0..width {
                let u = ((i as f64 + 0.5) / width as f64 * self.width as f64 - 0.5).clamp(0.0, (self.width - 1) as f64);
                let i0 = (u.floor() as usize).min(self.width - 1);
                let i1 = (i0 + 1).min(self.width - 1);
                let fx = u - i0 as f64;
                let (a, b, c, d) = (self.at(i0, j0), self.at(i1, j0), self.at(i0, j1), self.at(i1, j1));
                let mut q = [0.0; 3];
                for ch in 0..3 {
                    q[ch] = (1.0 - fy) * ((1.0 - fx) * a[ch] + fx * b[ch]) + fy * ((1.0 - fx) * c[ch] + fx * d[ch]);
                }
                out.q[j * width + i] = q;
            }
        }
        out
    }
}

/// Exact area mean of `light` over the footprint of texel (i, j); pixels outside the image are black.
pub fn texel_mean(light: &[[f64; 3]], w: usize, h: usize, place: &Placement, i: usize, j: usize) -> [f64; 3] {
    let x0 = place.origin_px[0] + i as f64 * place.texel_px[0];
    let y0 = place.origin_px[1] + j as f64 * place.texel_px[1];
    let (x1, y1) = (x0 + place.texel_px[0], y0 + place.texel_px[1]);
    let mut sum = [0.0; 3];
    let (px0, px1) = (x0.floor().max(0.0) as i64, (x1.ceil() as i64).min(w as i64));
    let (py0, py1) = (y0.floor().max(0.0) as i64, (y1.ceil() as i64).min(h as i64));
    for py in py0..py1 {
        let oy = (y1.min(py as f64 + 1.0) - y0.max(py as f64)).max(0.0);
        for px in px0..px1 {
            let ox = (x1.min(px as f64 + 1.0) - x0.max(px as f64)).max(0.0);
            let l = light[py as usize * w + px as usize];
            for c in 0..3 {
                sum[c] += ox * oy * l[c];
            }
        }
    }
    let area = place.texel_px[0] * place.texel_px[1];
    sum.map(|s| s / area)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ageing::{WearLaw, DEFAULT_RATES, REF_HOURS, REF_LOSS};
    use crate::close;

    fn flat(w: usize, h: usize, x: [f64; 3]) -> Vec<[f64; 3]> {
        vec![x; w * h]
    }

    /// The map covers the visible mm centred on the tube image, or the whole image when unplaced.
    #[test]
    fn placement_centres_the_visible_area() {
        let p = placement([500.0, 400.0], 2.0, [200.0, 150.0], [100, 75]);
        assert_eq!(p.origin_px, [50.0, 50.0]);
        assert_eq!(p.texel_px, [4.0, 4.0]);
        let u = placement([128.0, 96.0], 0.0, [386.0, 290.0], [640, 480]);
        assert_eq!(u.origin_px, [0.0, 0.0]);
        assert_eq!(u.texel_px, [0.2, 0.2]);
    }

    /// A flat field gives every texel inside the image exactly the field (the overlaps sum to the area), and
    /// texels half outside the image get half.
    #[test]
    fn texel_mean_of_a_flat_field() {
        let (w, h) = (10, 8);
        let light = flat(w, h, [0.5, 1.0, 0.25]);
        let p = Placement {
            origin_px: [0.0, 0.0],
            texel_px: [2.5, 2.0],
        };
        for (i, j) in [(0, 0), (1, 1), (3, 3)] {
            let m = texel_mean(&light, w, h, &p, i, j);
            for c in 0..3 {
                assert!(close(m[c], light[0][c], 1e-15), "texel ({i}, {j})");
            }
        }
        let q = Placement {
            origin_px: [-1.25, 0.0],
            texel_px: [2.5, 2.0],
        };
        assert!(close(texel_mean(&light, w, h, &q, 0, 0)[1], 0.5, 1e-15));
    }

    /// Accumulation is linear in the frames, monotonic, and per channel independent: a red-only picture
    /// wears red only.
    #[test]
    fn accumulation_is_monotonic_and_per_colour() {
        let (w, h) = (16, 12);
        let p = placement([w as f64, h as f64], 0.0, [0.0; 2], [8, 6]);
        let mut m = WearMap::new(8, 6);
        let red = flat(w, h, [1.0, 0.0, 0.0]);
        let mut last = 0.0;
        for _ in 0..5 {
            m.accumulate(&red, w, h, &p, 10.0);
            let q = m.at(3, 3);
            assert!(q[0] > last);
            assert_eq!((q[1], q[2]), (0.0, 0.0));
            last = q[0];
        }
        assert!(close(last, 50.0, 1e-12));
    }

    /// A bar wears only its area: texels away from it keep exactly 0, so the efficiency there is exactly 1.
    #[test]
    fn a_static_bar_leaves_a_ghost_only_inside_its_area() {
        let (w, h) = (64, 48);
        let mut light = flat(w, h, [0.0; 3]);
        for y in 40..48 {
            for x in 0..w {
                light[y * w + x] = [1.0; 3];
            }
        }
        let p = placement([w as f64, h as f64], 0.0, [0.0; 2], [32, 24]);
        let mut m = WearMap::new(32, 24);
        m.accumulate(&light, w, h, &p, 1000.0);
        let law = WearLaw::calibrated(DEFAULT_RATES, [0.2126, 0.7152, 0.0722], REF_LOSS, REF_HOURS);
        // Inside the bar: worn; above it by more than one texel: exactly unchanged.
        let inside = m.apply(&law, 0.0, &p, 10.0, 44.0, [1.0; 3]);
        assert!(inside.iter().all(|v| *v < 1.0));
        for y in 0..37 {
            let e = m.apply(&law, 0.0, &p, 10.0, y as f64, [1.0; 3]);
            assert_eq!(e, [1.0; 3], "row {y}");
        }
        // Blue (fastest) shows the deepest ghost: the ghost is tinted.
        assert!(inside[2] < inside[1] && inside[1] < inside[0]);
    }

    /// Bilinear sampling: texel centres give the texel, halfway gives the mean, outside fades to 0.
    #[test]
    fn sample_is_bilinear() {
        let mut m = WearMap::new(2, 1);
        m.q = vec![[2.0; 3], [4.0; 3]];
        let p = Placement {
            origin_px: [0.0, 0.0],
            texel_px: [2.0, 2.0],
        };
        // Pixel x has its centre at x + 0.5 px, u = (x + 0.5) / 2 - 0.5 texels: pixels 0 to 3 give u = -0.25
        // (0.75 of texel 0, the rest outside), 0.25, 0.75, 1.25 (0.75 of texel 1, the rest outside); row 0.5 is\n        // on the texel centre line (v = 0).
        let want = [1.5, 2.5, 3.5, 3.0];
        for (x, w) in want.iter().enumerate() {
            assert!(close(m.sample(&p, x as f64, 0.5)[0], *w, 1e-15), "pixel {x}");
        }
        assert_eq!(m.sample(&p, 0.5, 0.5)[0], 2.0, "texel centre");
        assert_eq!(m.sample(&p, 20.0, 0.5), [0.0; 3]);
    }

    /// Resampling keeps a uniform map uniform and maps a gradient linearly.
    #[test]
    fn resampling() {
        let mut m = WearMap::new(4, 2);
        m.q = vec![[7.0, 3.0, 1.0]; 8];
        let r = m.resample(640, 480);
        assert!(r.q.iter().all(|q| *q == [7.0, 3.0, 1.0]));
        let mut g = WearMap::new(4, 1);
        g.q = (0..4).map(|i| [f64::from(i); 3]).collect();
        let r = g.resample(8, 1);
        // Centres of the 8 at (k + 0.5) / 8 * 4 - 0.5 = k / 2 - 0.25, clamped.
        let want = [0.0, 0.25, 0.75, 1.25, 1.75, 2.25, 2.75, 3.0];
        for (k, w) in want.iter().enumerate() {
            assert!(close(r.q[k][0], *w, 1e-12), "{k}: {}", r.q[k][0]);
        }
        // Same size: unchanged.
        assert_eq!(g.resample(4, 1), g);
    }
}

//! Cabinet bezel and the reflection of it in the glass (TODO Low; parameters `params/cabinet.toml`).
//!
//! The picture sits in a rectangle `rect` = (x0, y0, x1, y1) of the output (whole pixels, y down). Around it runs
//! a band of `band_px` pixels, the bezel: a moulded frame with a sloped inner edge (the bevel) and a flat face,
//! lit from the top left, with rounded outer corners. The glass reflects the bezel: inside the rectangle, within
//! `band_px` of an edge, a faint mirror image of the bezel is added to the picture's light.
//!
//! Model, for a point p outside the rectangle with `dist` its distance to the rectangle (0 on its edge) and `n`
//! the unit vector from the rectangle's nearest point to p (the outward direction), `t = dist / band_px`:
//! - the surface normal is (-n, 1) / sqrt(2) on the bevel (`t < bevel`, a 45 degree slope that rises going
//!   outward, so it faces the glass) and (0, 0, 1) on the face;
//! - the light comes from `LIGHT` (up, left, towards the viewer); the shade is Lambert with an ambient term,
//!   `AMBIENT + (1 - AMBIENT) max(N . L, 0)`, divided by its value on the flat face, so the face shows exactly the
//!   bezel colour and the bevel shows the top edge darker and the bottom edge brighter than it;
//! - the outer fifteen percent of the band roll off: the shade is multiplied by `1 - 0.5 smoothstep(0.85, 1, t)`;
//! - coverage is the pixel coverage of the rounded outer shape (the rectangle grown by `band_px`, corner radius
//!   `corner x band_px`), from its signed distance: `clamp(0.5 - sd, 0, 1)`; outside it the surround shows.
//!
//! Reflection, for a point inside the rectangle at distance d < band_px of its nearest edge (ties go to left,
//! right, top, bottom in that order): the mirror point is d outside that edge, so the reflection is
//! `reflectivity x (1 - d / band_px) x` the bezel's linear colour and shade there. Every number is an estimate
//! except the reflectivity of bare glass (Fresnel, 4 % at normal incidence for n = 1.5).
//!
//! `shaders/include/cabinet.glsl` mirrors this file; `tests/cabinet.rs` compares the output's pixels with it.

use crate::colour::srgb_eotf;

/// Direction towards the light (not normalised): x right, y down, z towards the viewer.
const LIGHT_RAW: [f64; 3] = [-0.45, -0.55, 0.70];
/// Share of the light a surface turned away from the light still reflects.
pub const AMBIENT: f64 = 0.35;
/// Start and end of the outer roll-off, in shares of the band.
pub const ROLL_START: f64 = 0.85;

/// Settings of the bezel, in output pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cabinet {
    /// Width of the band in pixels (see [`inset_px`]); 0 turns the bezel off.
    pub band_px: f64,
    /// Face colour, sRGB code values.
    pub colour: [f64; 3],
    /// Share of the band taken by the sloped inner edge, 0 to 1.
    pub bevel: f64,
    /// Outer corner radius as a share of the band, 0 to 1.
    pub corner: f64,
    /// Reflectivity of the glass (0.04 for bare glass).
    pub reflection: f64,
}

/// What the bezel shows at one point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bezel {
    /// Share of the pixel the bezel covers (0: the surround shows, 1: the bezel).
    pub coverage: f64,
    /// The bezel's linear light (panel white = 1), meaningful where `coverage` > 0.
    pub linear: [f64; 3],
}

/// The band in whole pixels for a width setting `width` (a share of the output's shorter side) and an output of
/// `target` pixels: rounded half up, never negative.
#[must_use]
pub fn inset_px(width: f64, target: (u32, u32)) -> f64 {
    let side = f64::from(target.0.min(target.1));
    (width * side + 0.5).floor().max(0.0)
}

fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn light() -> [f64; 3] {
    let n = LIGHT_RAW.iter().map(|v| v * v).sum::<f64>().sqrt();
    LIGHT_RAW.map(|v| v / n)
}

impl Cabinet {
    /// Whether the bezel draws anything.
    #[must_use]
    pub fn is_on(&self) -> bool {
        self.band_px >= 1.0
    }

    /// The shade (1 on the flat face) at distance `dist` from the rectangle, outward direction `n`.
    #[must_use]
    pub fn shade(&self, dist: f64, n: [f64; 2]) -> f64 {
        let t = dist / self.band_px;
        let normal = if t < self.bevel {
            let s = std::f64::consts::FRAC_1_SQRT_2;
            [-n[0] * s, -n[1] * s, s]
        } else {
            [0.0, 0.0, 1.0]
        };
        let l = light();
        let lit = |nz: [f64; 3]| AMBIENT + (1.0 - AMBIENT) * (nz[0] * l[0] + nz[1] * l[1] + nz[2] * l[2]).max(0.0);
        let face = lit([0.0, 0.0, 1.0]);
        lit(normal) / face * (1.0 - 0.5 * smoothstep(ROLL_START, 1.0, t))
    }

    fn face_linear(&self) -> [f64; 3] {
        self.colour.map(|c| srgb_eotf(c.clamp(0.0, 1.0)))
    }

    /// What the bezel shows at the pixel centre `p` for the picture rectangle `rect`: coverage 0 inside the
    /// rectangle and outside the rounded outer shape.
    #[must_use]
    pub fn bezel(&self, p: [f64; 2], rect: [f64; 4]) -> Bezel {
        let none = Bezel {
            coverage: 0.0,
            linear: [0.0; 3],
        };
        if !self.is_on() {
            return none;
        }
        let nearest = [p[0].clamp(rect[0], rect[2]), p[1].clamp(rect[1], rect[3])];
        let (dx, dy) = (p[0] - nearest[0], p[1] - nearest[1]);
        let dist = dx.hypot(dy);
        if dist == 0.0 {
            return none;
        }
        // Signed distance to the rectangle grown by the band, corners rounded to `corner x band`.
        let radius = self.corner * self.band_px;
        let (cx, cy) = (0.5 * (rect[0] + rect[2]), 0.5 * (rect[1] + rect[3]));
        let (hx, hy) = (
            0.5 * (rect[2] - rect[0]) + self.band_px,
            0.5 * (rect[3] - rect[1]) + self.band_px,
        );
        let q = [(p[0] - cx).abs() - (hx - radius), (p[1] - cy).abs() - (hy - radius)];
        let sd = q[0].max(0.0).hypot(q[1].max(0.0)) + q[0].max(q[1]).min(0.0) - radius;
        let coverage = (0.5 - sd).clamp(0.0, 1.0);
        if coverage == 0.0 {
            return none;
        }
        let shade = self.shade(dist, [dx / dist, dy / dist]);
        Bezel {
            coverage,
            linear: self.face_linear().map(|c| c * shade),
        }
    }

    /// The light the glass adds at the pixel centre `p` inside `rect` (linear, panel white = 1): zero outside the
    /// rectangle, farther than a band from every edge, or with no reflectivity.
    #[must_use]
    pub fn reflection(&self, p: [f64; 2], rect: [f64; 4]) -> [f64; 3] {
        let zero = [0.0; 3];
        if !self.is_on() || self.reflection <= 0.0 {
            return zero;
        }
        if p[0] < rect[0] || p[0] > rect[2] || p[1] < rect[1] || p[1] > rect[3] {
            return zero;
        }
        let d = [p[0] - rect[0], rect[2] - p[0], p[1] - rect[1], rect[3] - p[1]];
        let normals = [[-1.0, 0.0], [1.0, 0.0], [0.0, -1.0], [0.0, 1.0]];
        let mut k = 0;
        for i in 1..4 {
            if d[i] < d[k] {
                k = i;
            }
        }
        if d[k] >= self.band_px {
            return zero;
        }
        let t = d[k] / self.band_px;
        let shade = self.shade(d[k], normals[k]);
        self.face_linear().map(|c| c * shade * self.reflection * (1.0 - t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::close;

    fn cab() -> Cabinet {
        Cabinet {
            band_px: 20.0,
            colour: [0.5, 0.5, 0.5],
            bevel: 0.5,
            corner: 1.0,
            reflection: 0.1,
        }
    }

    /// The picture rectangle of a 200 x 100 output with a 20 px band on every side.
    const RECT: [f64; 4] = [20.0, 20.0, 180.0, 80.0];

    /// The band of a width: a share of the shorter side, rounded (0.07 of 1080 is 75.6, 76).
    #[test]
    fn the_band_is_a_rounded_share_of_the_shorter_side() {
        assert_eq!(inset_px(0.07, (1920, 1080)), 76.0);
        assert_eq!(inset_px(0.07, (1080, 1920)), 76.0);
        assert_eq!(inset_px(0.0, (1920, 1080)), 0.0);
        assert_eq!(inset_px(-1.0, (1920, 1080)), 0.0);
    }

    /// The flat face shows the bezel colour exactly, on the left side where the bevel is over: sRGB 0.5 is
    /// linear 0.21404 (hand value: ((0.5 + 0.055) / 1.055)^2.4), at t = 0.7 (14 px of 20, before the roll-off).
    #[test]
    fn the_flat_face_is_the_bezel_colour() {
        let b = cab().bezel([20.0 - 14.0, 50.0], RECT);
        assert_eq!(b.coverage, 1.0);
        for c in b.linear {
            assert!(close(c, 0.214_041, 1e-5), "{c}");
        }
    }

    /// The bevel is lit from the top left: at t = 0.1 (2 px) the top band's slope faces down, away from the
    /// light (darker than the face), the bottom band's faces up, towards it (brighter); the left band's slope
    /// faces right, away from the light (darker), the right band's faces left (brighter). Hand values:
    /// |L| = sqrt(0.995) = 0.99750, face = 0.35 + 0.65 x 0.70176 = 0.80614; top N = (0, 0.7071, 0.7071):
    /// N.L = (-0.55 + 0.7) x 0.7071 / 0.99750 = 0.10633, shade (0.35 + 0.06912) / 0.80614 = 0.51988; bottom
    /// N = (0, -0.7071, 0.7071): N.L = 1.25 x 0.7071 / 0.99750 = 0.88610, shade (0.35 + 0.57597) / 0.80614 =
    /// 1.14865; left N = (0.7071, 0, 0.7071): N.L = 0.25 x 0.7071 / 0.99750 = 0.17722, shade 0.57706;
    /// right N = (-0.7071, 0, 0.7071): N.L = 1.15 x 0.7071 / 0.99750 = 0.81521, shade 1.09146.
    #[test]
    fn the_bevel_is_lit_from_the_top_left() {
        let c = cab();
        let top = c.shade(2.0, [0.0, -1.0]);
        let bottom = c.shade(2.0, [0.0, 1.0]);
        let left = c.shade(2.0, [-1.0, 0.0]);
        let right = c.shade(2.0, [1.0, 0.0]);
        assert!(close(top, 0.519_88, 2e-4), "{top}");
        assert!(close(bottom, 1.148_65, 2e-4), "{bottom}");
        assert!(close(left, 0.577_06, 2e-4), "{left}");
        assert!(close(right, 1.091_46, 2e-4), "{right}");
        assert!(
            top < left && left < 1.0 && 1.0 < right && right < bottom,
            "{top} {left} {right} {bottom}"
        );
        // Past the bevel the face is flat: the same shade in every direction.
        for n in [[0.0, -1.0], [0.0, 1.0], [-1.0, 0.0], [1.0, 0.0]] {
            assert!(close(c.shade(12.0, n), 1.0, 1e-12));
        }
        // Without a bevel the whole band is the face.
        let flat = Cabinet { bevel: 0.0, ..c };
        assert!(close(flat.shade(1.0, [0.0, -1.0]), 1.0, 1e-12));
    }

    /// The outer roll-off: shade x (1 - 0.5 smoothstep(0.85, 1, t)); at t = 0.925 the smoothstep is 0.5 and the
    /// factor 0.75, at t = 1 it is 0.5.
    #[test]
    fn the_outer_edge_rolls_off() {
        let c = cab();
        assert!(close(c.shade(18.5, [-1.0, 0.0]), 0.75, 1e-12));
        assert!(close(c.shade(20.0, [-1.0, 0.0]), 0.5, 1e-12));
    }

    /// Inside the picture rectangle there is no bezel; beyond the band, or in the cut corner, the surround shows.
    /// The rounded corner (radius = the band) is a quarter circle centred on the rectangle's corner (20, 20): the
    /// diagonal point (10, 10) is 14.1 px from it, covered; (5, 5) is 21.2 px away, past the 20 px radius, so
    /// the surround shows there.
    #[test]
    fn coverage_follows_the_rounded_outer_shape() {
        let c = cab();
        assert_eq!(c.bezel([100.0, 50.0], RECT).coverage, 0.0, "inside the picture");
        assert_eq!(c.bezel([0.5, 50.0], RECT).coverage, 1.0, "the outer pixel of the band");
        assert_eq!(c.bezel([100.0, 50.0 + 60.0], RECT).coverage, 0.0, "beyond the band");
        assert_eq!(c.bezel([10.0, 10.0], RECT).coverage, 1.0);
        assert_eq!(c.bezel([5.0, 5.0], RECT).coverage, 0.0);
        // A square corner (radius 0) covers the whole corner square.
        let square = Cabinet { corner: 0.0, ..c };
        assert_eq!(square.bezel([5.0, 5.0], RECT).coverage, 1.0);
        // Off: nothing.
        let off = Cabinet { band_px: 0.0, ..c };
        assert_eq!(off.bezel([10.0, 50.0], RECT).coverage, 0.0);
    }

    /// The reflection: zero outside the rectangle, farther than the band from every edge and without
    /// reflectivity; at 4 px from the left edge (t = 0.2, on the bevel, normal (-1, 0)) it is the face colour x
    /// the shade there x reflectivity x 0.8.
    #[test]
    fn the_glass_reflects_the_bezel_near_the_edges() {
        let c = cab();
        assert_eq!(c.reflection([10.0, 50.0], RECT), [0.0; 3], "outside the picture");
        assert_eq!(c.reflection([100.0, 50.0], RECT), [0.0; 3], "far from every edge");
        assert_eq!(
            Cabinet { reflection: 0.0, ..c }.reflection([24.0, 50.0], RECT),
            [0.0; 3]
        );
        let r = c.reflection([24.0, 50.0], RECT);
        let want = 0.214_041 * c.shade(4.0, [-1.0, 0.0]) * 0.1 * 0.8;
        for v in r {
            assert!(close(v, want, 1e-7), "{v} {want}");
        }
        assert!(r[0] > 0.0);
        // The nearest edge wins: 4 px from the bottom edge (y = 80), 40 from the left: the bottom normal.
        let rb = c.reflection([60.0, 76.0], RECT);
        let wantb = 0.214_041 * c.shade(4.0, [0.0, 1.0]) * 0.1 * 0.8;
        assert!(close(rb[1], wantb, 1e-7), "{} {wantb}", rb[1]);
        // The bottom edge reflects brighter than the top (the light is above).
        let rt = c.reflection([60.0, 24.0], RECT);
        assert!(rb[0] > rt[0]);
    }
}

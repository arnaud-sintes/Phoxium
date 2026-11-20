//! Tube geometry: visible area, raster line pitch, mask triads, curvature, moire.

/// Visible width and height from a diagonal and an aspect ratio (w / h).
pub fn visible_size_mm(diagonal_mm: f64, aspect: f64) -> (f64, f64) {
    let h = diagonal_mm / (1.0 + aspect * aspect).sqrt();
    (h * aspect, h)
}

/// Line pitch P = visible height / number of active lines (raster fills the
/// visible area). CRT.md "Scanline structure". [D]
pub fn line_pitch_mm(visible_height_mm: f64, active_lines: f64) -> f64 {
    visible_height_mm / active_lines
}

/// Triads across a visible width.
pub fn triads_across(visible_width_mm: f64, triad_pitch_mm: f64) -> f64 {
    visible_width_mm / triad_pitch_mm
}

/// Triads per source pixel: triads across over the source width in pixels.
pub fn triads_per_source_px(triads: f64, source_width_px: f64) -> f64 {
    triads / source_width_px
}

/// Host pixels per triad.
pub fn px_per_triad(host_px: f64, triads: f64) -> f64 {
    host_px / triads
}

/// Delta-gun triad distances for a dot pitch `p_mm`: same-colour repeat along
/// a row P sqrt(3), half of it, P / 2 and P / sqrt(3). [D]
pub fn delta_distances_mm(p_mm: f64) -> [f64; 4] {
    let s3 = 3.0_f64.sqrt();
    [p_mm * s3, p_mm * s3 / 2.0, p_mm / 2.0, p_mm / s3]
}

/// Sagitta of a spherical cap of radius `r_mm` over a half chord `h_mm`:
/// s = R - sqrt(R^2 - h^2). [D]
///
/// # Panics
/// Unless 0 <= |`h_mm`| <= `r_mm` < inf (a chord cannot exceed the diameter).
pub fn sagitta_mm(r_mm: f64, h_mm: f64) -> f64 {
    assert!(
        r_mm.is_finite() && h_mm.abs() <= r_mm,
        "sagitta needs |h| <= R: R {r_mm}, h {h_mm}"
    );
    r_mm - (r_mm * r_mm - h_mm * h_mm).sqrt()
}

/// Moire beat period of two periods p1 and p2: p1 p2 / |p1 - p2|. [K]
pub fn moire_beat(p1: f64, p2: f64) -> f64 {
    p1 * p2 / (p1 - p2).abs()
}

/// Curved faceplate seen from the front (T-37, CRT.md "Faceplate curvature"): the phosphor surface point at
/// arc length `arc_mm` from the centre, along a plane through the tube axis, on a circle of radius `r_mm`
/// (a sphere's meridian, or the horizontal section of a cylinder), is at the polar angle phi = arc / R about
/// the centre of curvature: lateral R sin(phi), depth R (1 - cos(phi)) behind the centre. Seen from an eye on
/// the axis at `eye_mm` in front of the centre (0: at infinity, parallel view), it projects on the tangent
/// plane at the centre to lateral x eye / (eye + depth). Returns that projected distance (mm). [D]
///
/// The raster is laid on the surface uniform in arc length (CRT.md "Raster distortions": position = R theta
/// for a sphere centred on the deflection centre), so this maps a raster position to where it is seen.
pub fn projected_from_arc_mm(r_mm: f64, arc_mm: f64, eye_mm: f64) -> f64 {
    let phi = arc_mm / r_mm;
    let lateral = r_mm * phi.sin();
    if eye_mm > 0.0 {
        lateral * eye_mm / (eye_mm + r_mm * (1.0 - phi.cos()))
    } else {
        lateral
    }
}

/// Inverse of [`projected_from_arc_mm`] on |arc| < R pi / 2 (the function is increasing there), by
/// bisection to 1e-12 mm. [D]
pub fn arc_from_projected_mm(r_mm: f64, projected_mm: f64, eye_mm: f64) -> f64 {
    let (mut lo, mut hi) = (-r_mm * std::f64::consts::FRAC_PI_2, r_mm * std::f64::consts::FRAC_PI_2);
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        if projected_from_arc_mm(r_mm, mid, eye_mm) < projected_mm {
            lo = mid;
        } else {
            hi = mid;
        }
        if hi - lo < 1e-12 {
            break;
        }
    }
    0.5 * (lo + hi)
}

/// Seen position (mm from the centre, tangent plane) of the raster point `s` (arc-length coordinates, mm) on
/// a spherical faceplate of radius `r_mm`: the radial direction is kept, the radius maps through
/// [`projected_from_arc_mm`]. [D]
pub fn sphere_seen_mm(r_mm: f64, s: (f64, f64), eye_mm: f64) -> (f64, f64) {
    let rho = s.0.hypot(s.1);
    if rho == 0.0 {
        return (0.0, 0.0);
    }
    let k = projected_from_arc_mm(r_mm, rho, eye_mm) / rho;
    (s.0 * k, s.1 * k)
}

/// Seen position of the raster point `s` on a cylindrical faceplate (horizontal radius `r_mm`, flat
/// vertically) seen from infinity: x maps through [`projected_from_arc_mm`], y is unchanged. [D]
pub fn cylinder_seen_mm(r_mm: f64, s: (f64, f64)) -> (f64, f64) {
    cylinder_seen_eye_mm(r_mm, s, 0.0)
}

/// Seen position of the raster point `s` on a cylindrical faceplate (horizontal radius `r_mm`, vertical
/// axis) from an eye on the axis at `eye_mm` in front of the centre (0: at infinity). The surface point is at
/// lateral R sin(phi), height s.1, depth R (1 - cos(phi)) (phi = s.0 / R); the central projection on the
/// tangent plane scales both coordinates by eye / (eye + depth), which depends on s.0 only, so a vertical
/// raster line is seen as a vertical line. [D]
pub fn cylinder_seen_eye_mm(r_mm: f64, s: (f64, f64), eye_mm: f64) -> (f64, f64) {
    let phi = s.0 / r_mm;
    let depth = r_mm * (1.0 - phi.cos());
    let k = if eye_mm > 0.0 { eye_mm / (eye_mm + depth) } else { 1.0 };
    (projected_from_arc_mm(r_mm, s.0, eye_mm), s.1 * k)
}

/// Radial misconvergence term of one beam at the picture position `u` (mm from the centre) of a picture of
/// half size `half` (mm): `radial` (mm) times (r / rc)^`exponent` along the unit radius (outward for a positive
/// `radial`), rc the half diagonal, so the term is `radial` at the picture corner and 0 at the centre. [E]
pub fn radial_offset_mm(radial: f64, exponent: f64, u: (f64, f64), half: (f64, f64)) -> (f64, f64) {
    let r = u.0.hypot(u.1);
    if r == 0.0 || radial == 0.0 {
        return (0.0, 0.0);
    }
    let k = radial * (r / half.0.hypot(half.1)).powf(exponent) / r;
    (u.0 * k, u.1 * k)
}

/// Misconvergence of one beam at the picture position `u` (mm from the centre) of a picture of half size
/// `half` (mm): the static offset `stat` (mm) plus `radial` (mm) times (r / rc)^2 along the radius, rc the
/// half diagonal (CRT.md "Convergence": static centre offset plus a quadratic polynomial per channel [E]).
pub fn convergence_offset_mm(stat: (f64, f64), radial: f64, u: (f64, f64), half: (f64, f64)) -> (f64, f64) {
    let (x, y) = radial_offset_mm(radial, 2.0, u, half);
    (stat.0 + x, stat.1 + y)
}

/// The chromatic aberration controls (QUESTIONS PHY-17, `params/geometry.toml` group "Chromatic aberration"):
/// a misconvergence of red and blue mirrored about green, the dominant colour fringing of an in-line tube
/// (CRT.md "Convergence": "red and blue errors are roughly mirror images about green, mostly horizontal,
/// growing with distance from the centre" [E]). Red moves by half of each amount, blue by the opposite half,
/// green stays; per-gun focus and glass dispersion are not part of it (PHY-17).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Chroma {
    /// Separation of red and blue along the radius at the picture corner (mm; positive: red outward).
    pub lateral_mm: f64,
    /// Constant separation of red and blue (mm) along `angle_deg`.
    pub axis_mm: f64,
    /// Direction of the constant separation, from +x, clockwise on the screen (y down) (degrees).
    pub angle_deg: f64,
    /// Growth of the radial separation with the distance from the centre: (r / rc)^exponent.
    pub edge_exponent: f64,
}

impl Default for Chroma {
    /// Off: no separation, the quadratic growth of the convergence model.
    fn default() -> Self {
        Chroma {
            lateral_mm: 0.0,
            axis_mm: 0.0,
            angle_deg: 0.0,
            edge_exponent: 2.0,
        }
    }
}

impl Chroma {
    /// True when both amounts are zero: every beam lands as without the controls (the angle and the
    /// exponent are then inert).
    pub fn is_off(&self) -> bool {
        self.lateral_mm == 0.0 && self.axis_mm == 0.0
    }

    /// Offset (mm) of the red beam at the picture position `u` (mm from the centre) of a picture of half size
    /// `half`: half the constant separation along the angle plus half the radial one,
    /// [`radial_offset_mm`] with the edge exponent.
    pub fn red_offset_mm(&self, u: (f64, f64), half: (f64, f64)) -> (f64, f64) {
        let a = self.angle_deg.to_radians();
        let (x, y) = radial_offset_mm(0.5 * self.lateral_mm, self.edge_exponent, u, half);
        (0.5 * self.axis_mm * a.cos() + x, 0.5 * self.axis_mm * a.sin() + y)
    }

    /// Offsets (mm) of the red, green and blue beams at `u`: red, zero, minus red.
    pub fn offsets_mm(&self, u: (f64, f64), half: (f64, f64)) -> [(f64, f64); 3] {
        let (x, y) = self.red_offset_mm(u, half);
        [(x, y), (0.0, 0.0), (-x, -y)]
    }

    /// Largest displacement (mm) of one beam inside the picture, along either axis or radially: half of each
    /// amount, the radial one taken at the corner (r = rc, factor 1 for every exponent).
    pub fn max_beam_offset_mm(&self) -> f64 {
        0.5 * (self.lateral_mm.abs() + self.axis_mm.abs())
    }
}

/// Total misconvergence of beam `channel` (0 red, 1 green, 2 blue) at `u`: the convergence model
/// ([`convergence_offset_mm`] with that beam's static offset and radial amount) plus the chromatic
/// aberration controls ([`Chroma::offsets_mm`]).
pub fn misconvergence_mm(
    stat: (f64, f64),
    radial: f64,
    chroma: &Chroma,
    channel: usize,
    u: (f64, f64),
    half: (f64, f64),
) -> (f64, f64) {
    let (x, y) = convergence_offset_mm(stat, radial, u, half);
    let (cx, cy) = chroma.offsets_mm(u, half)[channel];
    (x + cx, y + cy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::close;
    use crate::units::inch_to_mm;

    #[test]
    fn visible_size_from_diagonal() {
        let (w, h) = visible_size_mm(500.0, 4.0 / 3.0);
        assert!(close(w, 400.0, 1e-9) && close(h, 300.0, 1e-9));
    }

    /// CRT.md reference-scale table, every column derived from the tube's
    /// diagonal alone (viewable = nominal - 1 in, or the stated viewable
    /// diagonal), 4:3, then compared with the printed (rounded) values: whole
    /// mm for sizes and triads, 0.01 mm for line pitch, 2 significant digits
    /// above 1.5 triads per source pixel and 3 below.
    #[test]
    fn reference_scale_table() {
        // (viewable diagonal in, source w, active lines, triad pitch,
        //  table: w, h, line pitch, triads across, triads per source px)
        let rows = [
            (13.0, 320.0, 240.0, 0.25, 264.0, 198.0, 0.83, 1056.0, 3.3),
            (19.0, 320.0, 240.0, 0.30, 386.0, 290.0, 1.21, 1287.0, 4.0),
            (19.0, 320.0, 240.0, 0.31, 386.0, 290.0, 1.21, 1245.0, 3.9),
            (19.0, 320.0, 240.0, 0.65, 386.0, 290.0, 1.21, 594.0, 1.9),
            (26.0, 320.0, 240.0, 0.80, 528.0, 396.0, 1.65, 660.0, 2.1),
            (14.0, 640.0, 480.0, 0.242, 284.0, 213.0, 0.44, 1176.0, 1.8),
            (14.0, 320.0, 400.0, 0.242, 284.0, 213.0, 0.53, 1176.0, 3.7),
            (16.0, 1024.0, 768.0, 0.25, 325.0, 244.0, 0.32, 1300.0, 1.27),
            (20.0, 1600.0, 1200.0, 0.22, 406.0, 305.0, 0.25, 1847.0, 1.15),
        ];
        for (diag_in, src, lines, triad, w, h, pitch, n, per_px) in rows {
            let (gw, gh) = visible_size_mm(inch_to_mm(diag_in), 4.0 / 3.0);
            assert!(close(gw, w, 0.5) && close(gh, h, 0.5), "{diag_in} in: {gw} x {gh}");
            let p = line_pitch_mm(gh, lines);
            let t = triads_across(gw, triad);
            let s = triads_per_source_px(t.round(), src);
            assert!(close(p, pitch, 0.005), "{diag_in} in: line pitch {p} vs {pitch}");
            // The table divides the width rounded to whole mm: +-0.5 mm / pitch.
            assert!(close(t, n, 0.5 + 0.5 / triad), "{diag_in} in: triads {t} vs {n}");
            let tol = if per_px >= 1.5 { 0.05 } else { 0.005 };
            assert!(close(s, per_px, tol), "{diag_in} in: per source px {s} vs {per_px}");
        }
        // Spot values derived by hand: 14 in viewable is 355.6 mm, 0.8 x =
        // 284.48 mm wide, / 0.242 = 1175.5 triads; 20 in = 508 mm, 406.4 mm,
        // / 0.22 = 1847.3 triads.
        assert!(close(
            triads_across(visible_size_mm(355.6, 4.0 / 3.0).0, 0.242),
            1175.537,
            0.001
        ));
        assert!(close(
            triads_across(visible_size_mm(508.0, 4.0 / 3.0).0, 0.22),
            1847.273,
            0.001
        ));
        // 256-wide source: x 1.25. 480 lines: half the 240-line pitch.
        assert!(close(
            triads_per_source_px(1245.0, 256.0) / triads_per_source_px(1245.0, 320.0),
            1.25,
            1e-12
        ));
        assert!(close(
            line_pitch_mm(290.0, 480.0),
            line_pitch_mm(290.0, 240.0) / 2.0,
            1e-12
        ));
    }

    #[test]
    fn pvm_20m4u_on_1920_px() {
        let triads = triads_across(386.0, 0.31);
        assert!(close(triads, 1245.0, 0.5));
        assert!(close(px_per_triad(1920.0, triads), 1.54, 0.005));
    }

    #[test]
    fn sagitta_19in_1r() {
        let d = inch_to_mm(19.0);
        let s = sagitta_mm(1.767 * d, d / 2.0);
        assert!(close(s, 34.85, 0.05), "sagitta {s}");
        // Table ratios: sagitta / diagonal 7.2 %, 4.8 %, 3.6 %, 0.9 %.
        for (r_over_d, ratio) in [(1.767, 0.072), (2.65, 0.048), (3.53, 0.036), (14.1, 0.009)] {
            let got = sagitta_mm(r_over_d * d, d / 2.0) / d;
            assert!(close(got, ratio, 0.0006), "R/D {r_over_d}: {got}");
        }
    }

    #[test]
    #[should_panic(expected = "|h| <= R")]
    fn sagitta_chord_beyond_diameter_panics() {
        sagitta_mm(100.0, 101.0);
    }

    #[test]
    fn sagitta_hemisphere() {
        assert!(close(sagitta_mm(100.0, 100.0), 100.0, 1e-12));
        assert!(close(sagitta_mm(100.0, 60.0), 20.0, 1e-12));
    }

    #[test]
    fn delta_pitch_0_28() {
        let got = delta_distances_mm(0.28);
        for (g, e) in got.iter().zip([0.485, 0.242, 0.140, 0.162]) {
            assert!(close(*g, e, 0.0006), "{g} vs {e}");
        }
    }

    /// T-37: on the axis of a 19 in 1R tube (R = 1.767 x 482.6 = 852.75 mm) the projection of the
    /// corner (arc length to the corner) has the corner sagitta as its depth: R (1 - cos(phi)) with
    /// R sin(phi) = 241.3 mm is 34.85 mm, so the eye at 600 mm sees the corner at 241.3 x 600 / 634.85.
    #[test]
    fn projection_of_a_1r_faceplate() {
        let d = inch_to_mm(19.0);
        let r = 1.767 * d;
        let h = d / 2.0;
        let arc = r * (h / r).asin();
        assert!(close(projected_from_arc_mm(r, arc, 0.0), h, 1e-9));
        let seen = projected_from_arc_mm(r, arc, 600.0);
        assert!(close(seen, h * 600.0 / (600.0 + sagitta_mm(r, h)), 1e-9), "{seen}");
        assert!(close(seen, 228.05, 0.01), "{seen}");
        for eye in [0.0, 600.0] {
            for x in [-200.0, -1.0, 0.0, 3.0, 240.0] {
                let a = arc_from_projected_mm(r, x, eye);
                assert!(close(projected_from_arc_mm(r, a, eye), x, 1e-9));
            }
        }
        // Radial: a raster point at 210 mm from the centre is seen at R sin(210 / R) = 207.885 mm.
        let (x, y) = sphere_seen_mm(852.8, (168.0, 126.0), 0.0);
        assert!(close(x.hypot(y), 207.885, 0.001) && close(y / x, 0.75, 1e-12));
        assert_eq!(cylinder_seen_mm(852.8, (168.0, 126.0)).1, 126.0);
    }

    #[test]
    fn convergence_quadratic() {
        let (dx, dy) = convergence_offset_mm((0.1, -0.2), 0.5, (13.0, 9.0), (16.0, 12.0));
        // (r / rc)^2 = 250 / 400 = 0.625, 0.3125 mm along (13, 9) / 15.8114.
        assert!(
            close(dx, 0.1 + 0.256_94, 1e-5) && close(dy, -0.2 + 0.177_88, 1e-5),
            "{dx} {dy}"
        );
    }

    /// PHY-17: on a 32 x 24 mm picture (half 16 x 12, rc 20 mm) red and blue land `lateral` apart at the
    /// corner, each half of it, mirrored, green fixed; the separation grows as (r / rc)^exponent.
    #[test]
    fn chroma_lateral_at_the_corner_and_symmetry() {
        let half = (16.0, 12.0);
        let corner = (16.0, 12.0);
        for e in [1.0, 2.0, 3.0] {
            let c = Chroma {
                lateral_mm: 3.0,
                edge_exponent: e,
                ..Chroma::default()
            };
            let [r, g, b] = c.offsets_mm(corner, half);
            // Red outward by 1.5 mm along (0.8, 0.6), blue inward by 1.5 mm.
            assert!(close(r.0, 1.2, 1e-12) && close(r.1, 0.9, 1e-12), "{r:?}");
            assert_eq!(g, (0.0, 0.0));
            assert_eq!(b, (-r.0, -r.1));
            assert!(close((r.0 - b.0).hypot(r.1 - b.1), 3.0, 1e-12));
            // Exponent shape: half the corner radius gives 0.5^e of the separation, the centre none.
            let [r, _, b] = c.offsets_mm((8.0, 6.0), half);
            assert!(
                close((r.0 - b.0).hypot(r.1 - b.1), 3.0 * 0.5_f64.powf(e), 1e-12),
                "e {e}"
            );
            assert_eq!(c.offsets_mm((0.0, 0.0), half), [(0.0, 0.0); 3]);
        }
        // Middle of the right edge (r = 16 = 0.8 rc), exponent 2: 0.64 of it, along +x.
        let c = Chroma {
            lateral_mm: -2.0,
            ..Chroma::default()
        };
        let r = c.red_offset_mm((16.0, 0.0), half);
        assert!(close(r.0, -0.64, 1e-12) && r.1 == 0.0, "{r:?}");
        assert!(close(c.max_beam_offset_mm(), 1.0, 1e-12));
    }

    /// PHY-17: the constant separation follows the angle (clockwise on the screen, y down) everywhere.
    #[test]
    fn chroma_axis_by_angle() {
        let half = (16.0, 12.0);
        for (deg, dir) in [
            (0.0, (1.0, 0.0)),
            (90.0, (0.0, 1.0)),
            (180.0, (-1.0, 0.0)),
            (-90.0, (0.0, -1.0)),
            (30.0, (0.866_025_403_784, 0.5)),
        ] {
            let c = Chroma {
                axis_mm: 0.8,
                angle_deg: deg,
                ..Chroma::default()
            };
            for u in [(0.0, 0.0), (16.0, 12.0), (-7.0, 3.0)] {
                let [r, g, b] = c.offsets_mm(u, half);
                assert!(
                    close(r.0, 0.4 * dir.0, 1e-9) && close(r.1, 0.4 * dir.1, 1e-9),
                    "{deg}: {r:?}"
                );
                assert_eq!(g, (0.0, 0.0));
                assert_eq!(b, (-r.0, -r.1));
            }
        }
    }

    /// PHY-17: both amounts zero is the identity (the angle and exponent are inert), and the total adds the
    /// controls to the convergence model; with exponent 2 the lateral amount is the convergence model's
    /// radial term split +/- between red and blue.
    #[test]
    fn chroma_off_is_identity_and_adds_to_convergence() {
        let half = (16.0, 12.0);
        let u = (13.0, 9.0);
        let off = Chroma {
            angle_deg: 77.0,
            edge_exponent: 3.0,
            ..Chroma::default()
        };
        assert!(off.is_off());
        for ch in 0..3 {
            assert_eq!(
                misconvergence_mm((0.1, -0.2), 0.5, &off, ch, u, half),
                convergence_offset_mm((0.1, -0.2), 0.5, u, half)
            );
        }
        let c = Chroma {
            lateral_mm: 1.0,
            ..Chroma::default()
        };
        let red = misconvergence_mm((0.0, 0.0), 0.0, &c, 0, u, half);
        let same = convergence_offset_mm((0.0, 0.0), 0.5, u, half);
        assert!(close(red.0, same.0, 1e-12) && close(red.1, same.1, 1e-12));
        let blue = misconvergence_mm((0.1, 0.0), 0.0, &c, 2, u, half);
        assert!(close(blue.0, 0.1 - same.0, 1e-12) && close(blue.1, -same.1, 1e-12));
    }

    #[test]
    fn moire() {
        assert!(close(moire_beat(3.0, 2.0), 6.0, 1e-12));
        assert!(moire_beat(1.0, 1.0).is_infinite());
    }
}

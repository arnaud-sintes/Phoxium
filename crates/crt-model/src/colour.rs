//! Colour: gun laws (drive to beam current), EOTFs, primaries to XYZ matrices.

/// Pure power-law gun (GOG with gain 1, offset 0): v^gamma. [S]
pub fn gun_power(v: f64, gamma: f64) -> f64 {
    v.max(0.0).powf(gamma)
}

/// GOG gun law (Berns, Motta and Gorzynski 1993): (gain v + offset)^gamma,
/// 0 when gain v + offset < 0. [K]
pub fn gun_gog(v: f64, gain: f64, offset: f64, gamma: f64) -> f64 {
    (gain * v + offset).max(0.0).powf(gamma)
}

/// BT.1886 constants (a, b) for white luminance `lw` and black luminance `lb`, gamma 2.4. [S]
///
/// # Panics
/// Unless 0 <= `lb` < `lw` < inf (also for [`bt1886_eotf`]).
pub fn bt1886_constants(lw: f64, lb: f64) -> (f64, f64) {
    assert!(
        lw.is_finite() && 0.0 <= lb && lb < lw,
        "BT.1886 needs 0 <= Lb < Lw: Lw {lw}, Lb {lb}"
    );
    let g = 1.0 / 2.4;
    let d = lw.powf(g) - lb.powf(g);
    (d.powf(2.4), lb.powf(g) / d)
}

/// BT.1886 EOTF: L = a max(V + b, 0)^2.4. [S]
pub fn bt1886_eotf(v: f64, lw: f64, lb: f64) -> f64 {
    let (a, b) = bt1886_constants(lw, lb);
    a * (v + b).max(0.0).powf(2.4)
}

/// Knee of BT.1886 Annex 1.
pub const ANNEX1_VC: f64 = 0.35;
/// Exponent of BT.1886 Annex 1 above the knee.
pub const ANNEX1_A1: f64 = 2.6;
/// Exponent of BT.1886 Annex 1 below the knee.
pub const ANNEX1_A2: f64 = 3.0;

/// BT.1886 Annex 1 (two exponents, closer CRT match), as written in CRT.md: [S]
/// L = k (V + b)^2.6 for V >= 0.35,
/// L = k (0.35 + b)^(2.6 - 3.0) (V + b)^3.0 below,
/// k = Lw / (1 + b)^2.6. `b` is the black-level lift.
///
/// # Panics
/// Unless `b` > -0.35 (the knee term (0.35 + b)^-0.4 needs a positive base)
/// and `lw`, `b` are finite.
pub fn bt1886_annex1_eotf(v: f64, lw: f64, b: f64) -> f64 {
    assert!(
        lw.is_finite() && b.is_finite() && b > -ANNEX1_VC,
        "BT.1886 Annex 1 needs b > -0.35: b {b}, Lw {lw}"
    );
    let k = lw / (1.0 + b).powf(ANNEX1_A1);
    let x = (v + b).max(0.0);
    if v >= ANNEX1_VC {
        k * x.powf(ANNEX1_A1)
    } else {
        k * (ANNEX1_VC + b).powf(ANNEX1_A1 - ANNEX1_A2) * x.powf(ANNEX1_A2)
    }
}

/// sRGB EOTF (IEC 61966-2-1), code value in [0, 1] to linear light. [K]
pub fn srgb_eotf(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// 3x3 matrix, row major.
pub type Mat3 = [[f64; 3]; 3];

/// xy chromaticity to XYZ with Y = 1.
///
/// # Panics
/// Unless `y` > 0 and `x` is finite.
pub fn xy_to_xyz(x: f64, y: f64) -> [f64; 3] {
    assert!(
        x.is_finite() && y.is_finite() && y > 0.0,
        "chromaticity needs y > 0: ({x}, {y})"
    );
    [x / y, 1.0, (1.0 - x - y) / y]
}

fn det3(m: &Mat3) -> f64 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

/// Inverse of a 3x3 matrix (cofactors over determinant).
///
/// # Panics
/// If the matrix is singular (determinant 0) or not finite (also for
/// [`rgb_to_xyz`] with collinear primaries).
pub fn inv3(m: &Mat3) -> Mat3 {
    let d = det3(m);
    assert!(d.is_finite() && d != 0.0, "singular matrix: {m:?}");
    let c = |r0: usize, r1: usize, c0: usize, c1: usize| m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0];
    [
        [c(1, 2, 1, 2) / d, -c(0, 2, 1, 2) / d, c(0, 1, 1, 2) / d],
        [-c(1, 2, 0, 2) / d, c(0, 2, 0, 2) / d, -c(0, 1, 0, 2) / d],
        [c(1, 2, 0, 1) / d, -c(0, 2, 0, 1) / d, c(0, 1, 0, 1) / d],
    ]
}

/// Matrix times vector.
pub fn mul3(m: &Mat3, v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

/// RGB to XYZ matrix from primaries and white chromaticities (white Y = 1). [K]
pub fn rgb_to_xyz(r: (f64, f64), g: (f64, f64), b: (f64, f64), w: (f64, f64)) -> Mat3 {
    let (pr, pg, pb) = (xy_to_xyz(r.0, r.1), xy_to_xyz(g.0, g.1), xy_to_xyz(b.0, b.1));
    let p: Mat3 = [[pr[0], pg[0], pb[0]], [pr[1], pg[1], pb[1]], [pr[2], pg[2], pb[2]]];
    let s = mul3(&inv3(&p), xy_to_xyz(w.0, w.1));
    let mut m = p;
    for row in &mut m {
        for (j, v) in row.iter_mut().enumerate() {
            *v *= s[j];
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::close;

    #[test]
    fn gog() {
        assert!(close(gun_power(0.5, 2.4), 0.18946, 0.000_01));
        assert!(close(gun_gog(0.5, 1.0, 0.0, 2.4), 0.18946, 0.000_01));
        assert_eq!(gun_gog(0.1, 1.0, -0.2, 2.4), 0.0);
    }

    #[test]
    fn bt1886() {
        let (a, b) = bt1886_constants(100.0, 0.1);
        assert!(close(a, 87.03, 0.01), "a = {a}");
        assert!(close(b, 0.0596, 0.0001), "b = {b}");
        assert!(close(bt1886_eotf(0.0, 100.0, 0.1), 0.100, 0.0005));
        assert!(close(bt1886_eotf(1.0, 100.0, 0.1), 100.0, 0.05));
    }

    #[test]
    fn bt1886_annex1() {
        // White maps to Lw for any lift.
        for b in [0.0, 0.05, 0.1] {
            assert!(close(bt1886_annex1_eotf(1.0, 100.0, b), 100.0, 1e-9));
        }
        // Continuous at the knee.
        let below = bt1886_annex1_eotf(ANNEX1_VC - 1e-12, 100.0, 0.05);
        let above = bt1886_annex1_eotf(ANNEX1_VC, 100.0, 0.05);
        assert!(close(below, above, 1e-9), "{below} vs {above}");
        // b = 0: pure 2.6 above the knee, 0.35^-0.4 V^3 below, zero at black.
        assert!(close(bt1886_annex1_eotf(0.5, 1.0, 0.0), 0.164_94, 0.000_01));
        assert!(close(bt1886_annex1_eotf(0.2, 1.0, 0.0), 0.012_18, 0.000_01));
        assert_eq!(bt1886_annex1_eotf(0.0, 1.0, 0.0), 0.0);
        // b = 0.1, Lw = 1, by hand: k = 1 / 1.1^2.6 = 1 / 1.281212 = 0.780511.
        // V = 0.5: k 0.6^2.6 = 0.780511 x 0.264968 = 0.206810.
        // V = 0.2: k 0.45^-0.4 0.3^3 = 0.780511 x 1.376306 x 0.027 = 0.029004.
        assert!(close(bt1886_annex1_eotf(0.5, 1.0, 0.1), 0.206_810, 0.000_002));
        assert!(close(bt1886_annex1_eotf(0.2, 1.0, 0.1), 0.029_004, 0.000_002));
    }

    #[test]
    #[should_panic(expected = "b > -0.35")]
    fn bt1886_annex1_lift_below_knee_panics() {
        bt1886_annex1_eotf(0.5, 100.0, -0.4);
    }

    #[test]
    #[should_panic(expected = "Lb < Lw")]
    fn bt1886_black_above_white_panics() {
        bt1886_constants(100.0, 100.0);
    }

    #[test]
    #[should_panic(expected = "y > 0")]
    fn xy_zero_y_panics() {
        xy_to_xyz(0.3, 0.0);
    }

    #[test]
    #[should_panic(expected = "singular")]
    fn coincident_primaries_panic() {
        rgb_to_xyz((0.3, 0.6), (0.3, 0.6), (0.15, 0.06), (0.3127, 0.3290));
    }

    fn assert_identity(m: &Mat3, inv: &Mat3) {
        for (i, row) in m.iter().enumerate() {
            for j in 0..3 {
                let col = inv.map(|r| r[j]);
                let p: f64 = row.iter().zip(col).map(|(a, b)| a * b).sum();
                let want = if i == j { 1.0 } else { 0.0 };
                assert!(close(p, want, 1e-12), "M inv(M) [{i}][{j}] = {p}");
            }
        }
    }

    /// SMPTE-C primaries (RP 145) with D65: reference matrix (Lindbloom,
    /// RGB/XYZ matrices page), 4 decimals.
    #[test]
    fn smpte_c_primaries_matrix() {
        let m = rgb_to_xyz((0.630, 0.340), (0.310, 0.595), (0.155, 0.070), (0.3127, 0.3290));
        let expected = [
            [0.3936, 0.3652, 0.1916],
            [0.2124, 0.7010, 0.0865],
            [0.0187, 0.1119, 0.9582],
        ];
        for (r, (got, want)) in m.iter().zip(expected).enumerate() {
            for (c, (g, w)) in got.iter().zip(want).enumerate() {
                assert!(close(*g, w, 0.0005), "[{r}][{c}] {g} vs {w}");
            }
        }
        assert_identity(&m, &inv3(&m));
    }

    #[test]
    fn srgb() {
        assert!(close(srgb_eotf(20.0 / 255.0), 0.006995, 0.000_001));
        assert!(close(srgb_eotf(1.0), 1.0, 1e-15));
    }

    /// CRT.md used to state 0.0049 (2.4) and 0.0057 (2.2) for level 20/255; the
    /// calculation gives 0.00222 and 0.00370 (corrected in CRT.md by T-14).
    #[test]
    fn gamma_example_level_20() {
        assert!(close(gun_power(20.0 / 255.0, 2.4), 0.00222, 0.000_01));
        assert!(close(gun_power(20.0 / 255.0, 2.2), 0.00370, 0.000_01));
    }

    #[test]
    fn srgb_primaries_matrix() {
        let m = rgb_to_xyz((0.64, 0.33), (0.30, 0.60), (0.15, 0.06), (0.3127, 0.3290));
        assert!(close(m[0][0], 0.4124, 0.0002), "{m:?}");
        assert!(close(m[1][1], 0.7152, 0.0002), "{m:?}");
        assert!(close(m[2][2], 0.9505, 0.0002), "{m:?}");
        let i = inv3(&m);
        assert!(close(i[0][0], 3.2406, 0.001), "{i:?}");
        // RGB white maps to the white point with Y = 1.
        let w = mul3(&m, [1.0, 1.0, 1.0]);
        assert!(close(w[1], 1.0, 1e-12) && close(w[0], 0.3127 / 0.3290, 1e-12));
        assert_identity(&m, &i);
    }
}

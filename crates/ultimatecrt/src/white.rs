//! White balance gains of the `gun` stage (T-34: QUESTIONS COL-2, CRT.md "White point").
//!
//! A white-point change is a per-channel gain in linear light, not a chromatic adaptation. The light of a
//! unit beam current of channel c is the phosphor chromaticity (x, y) at luminance 1,
//! `p_c` = (x / y, 1, (1 - x - y) / y). Drive (1, 1, 1) after the gains `g` gives `sum g_c p_c`, which must
//! have the chromaticity of the white W, `W` = (x / y, 1, (1 - x - y) / y): `P g = W` with the columns of
//! `P` the three `p_c`, solved here by Cramer's rule. The gains are divided by their maximum (a gun can be
//! attenuated, not boosted; the strongest gun stays at full drive). The solution has to be positive: a
//! white outside the triangle of the primaries, or collinear primaries, has no solution and gives the
//! neutral gains (1, 1, 1) instead of a panic in the render loop.
//!
//! The independent f64 oracle is `crt_model::gun` (inverse by cofactors, a different code path); the tests of
//! `tests/stage_gun.rs` compare this module, and the shader that consumes the result, with it.

use crate::params::Params;

/// Light (X, Y, Z) of chromaticity (x, y) at Y = 1, or `None` when y <= 0.
fn xyz(c: [f32; 2]) -> Option<[f64; 3]> {
    let (x, y) = (f64::from(c[0]), f64::from(c[1]));
    (y > 0.0 && x.is_finite() && y.is_finite()).then(|| [x / y, 1.0, (1.0 - x - y) / y])
}

fn det(m: [[f64; 3]; 3]) -> f64 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

/// Gains `g` (maximum 1) with `sum g_c p_c` of the chromaticity `white`, for the phosphor chromaticities
/// `red`, `green`, `blue` (CIE 1931 xy). `None` for collinear primaries, y <= 0, or a white outside the
/// triangle of the primaries (a non-positive gain).
pub fn white_gains_from_chromaticities(
    red: [f32; 2],
    green: [f32; 2],
    blue: [f32; 2],
    white: [f32; 2],
) -> Option<[f32; 3]> {
    let (r, g, b, w) = (xyz(red)?, xyz(green)?, xyz(blue)?, xyz(white)?);
    // Columns p_r, p_g, p_b as rows of the transposed matrix, so that row i of `m` is component i.
    let m = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
    let d = det(m);
    if !(d.is_finite() && d != 0.0) {
        return None;
    }
    let mut s = [0.0; 3];
    for (c, out) in s.iter_mut().enumerate() {
        let mut mc = m;
        for (i, row) in mc.iter_mut().enumerate() {
            row[c] = w[i];
        }
        *out = det(mc) / d;
    }
    let max = s.iter().copied().fold(f64::MIN, f64::max);
    if !s.iter().all(|v| *v > 0.0) || !max.is_finite() {
        return None;
    }
    Some(s.map(|v| (v / max) as f32))
}

/// The gains of the frame: those of the parameter table when `gun_white_balance` is on and the white is
/// reachable, else the neutral (1, 1, 1).
pub fn white_gains(params: &Params) -> [f32; 3] {
    if !params.gun_white_balance {
        return [1.0; 3];
    }
    white_gains_from_chromaticities(
        params.tube_red_xy,
        params.tube_green_xy,
        params.tube_blue_xy,
        params.tube_white_xy,
    )
    .unwrap_or([1.0; 3])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::ParamValue;

    const SMPTE_C: ([f32; 2], [f32; 2], [f32; 2]) = ([0.630, 0.340], [0.310, 0.595], [0.155, 0.070]);

    /// SMPTE-C with D65: the Y row of its RGB to XYZ matrix is 0.2124, 0.7010, 0.0865 (Lindbloom, 4
    /// decimals), which are the gains before the division by the largest.
    #[test]
    fn smpte_c_d65_gains_are_the_luminance_row_over_its_maximum() {
        let g = white_gains_from_chromaticities(SMPTE_C.0, SMPTE_C.1, SMPTE_C.2, [0.3127, 0.3290]).unwrap();
        for (got, want) in g.iter().zip([0.2124 / 0.7010, 1.0, 0.0865 / 0.7010]) {
            assert!((f64::from(*got) - want).abs() < 3e-4, "{g:?}");
        }
    }

    #[test]
    fn unreachable_whites_give_neutral_gains() {
        assert_eq!(
            white_gains_from_chromaticities(SMPTE_C.0, SMPTE_C.1, SMPTE_C.2, [0.7, 0.2]),
            None
        );
        // Collinear primaries and y = 0.
        assert_eq!(
            white_gains_from_chromaticities([0.3, 0.3], [0.4, 0.4], [0.5, 0.5], [0.4, 0.4]),
            None
        );
        assert_eq!(
            white_gains_from_chromaticities(SMPTE_C.0, SMPTE_C.1, SMPTE_C.2, [0.3, 0.0]),
            None
        );
        let mut p = Params::default();
        assert_eq!(white_gains(&p), [1.0; 3], "off by default");
        assert!(p.set("gun_white_balance", ParamValue::Bool(true)));
        assert!(white_gains(&p)[0] < 0.31);
        assert!(p.set("tube_white_xy", ParamValue::Vec2([0.7, 0.2])));
        assert_eq!(white_gains(&p), [1.0; 3], "unreachable white falls back to neutral");
    }
}

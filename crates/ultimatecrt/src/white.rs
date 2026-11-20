//! White retune gains of the `gun` stage (T-34 and its review fixes: QUESTIONS COL-2, CRT.md "White point").
//!
//! A white-point change is a per-channel gain in linear light, not a chromatic adaptation. The light of a
//! unit beam current of channel c is the phosphor chromaticity (x, y) at luminance 1,
//! `p_c` = (x / y, 1, (1 - x - y) / y); the columns of `P` are the three `p_c`, and `s(W)` = `P^-1 W` is the
//! weight vector of the primaries that makes the white W (Cramer's rule, below).
//!
//! The tube RGB (1, 1, 1) that the output transform sees is already the tube white (`tube_white_xy`, the
//! matrix of `crt_model::output::tube_to_display`), so its channel c is `s_c(tube white) p_c`. The option
//! is a relative retune, like a service-menu colour-temperature adjustment: gains
//! `g = s(target) / s(tube white)` componentwise, divided by their maximum (a gun can be attenuated, not
//! boosted; the strongest gun stays at full drive), give a white of the chromaticity of the target
//! (`gun_retune_white_xy`). Target = tube white gives exactly (1, 1, 1). The absolute solution
//! `P g = W` would apply the tube white twice. A white outside the triangle of the primaries, or
//! collinear primaries, has no solution and gives the neutral gains (1, 1, 1) instead of a panic in the
//! render loop.
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

/// Weights `s(W) = P^-1 W` (Cramer's rule) of the primaries (as the columns of `p`, see below) that make
/// the white `w`. `None` for collinear primaries or a white outside their triangle (a non-positive weight).
fn weights(p: [[f64; 3]; 3], w: [f64; 3]) -> Option<[f64; 3]> {
    let d = det(p);
    if !(d.is_finite() && d != 0.0) {
        return None;
    }
    let mut s = [0.0; 3];
    for (c, out) in s.iter_mut().enumerate() {
        let mut pc = p;
        for (i, row) in pc.iter_mut().enumerate() {
            row[c] = w[i];
        }
        *out = det(pc) / d;
    }
    (s.iter().all(|v| v.is_finite() && *v > 0.0)).then_some(s)
}

/// Retune gains `g` (maximum 1): `s(target) / s(tube_white)` componentwise, for the phosphor chromaticities
/// `red`, `green`, `blue` (CIE 1931 xy). Exactly (1, 1, 1) when `target` equals `tube_white`. `None` for
/// collinear primaries, y <= 0, or a white outside the triangle of the primaries.
pub fn white_retune_gains_from_chromaticities(
    red: [f32; 2],
    green: [f32; 2],
    blue: [f32; 2],
    tube_white: [f32; 2],
    target: [f32; 2],
) -> Option<[f32; 3]> {
    let (r, g, b) = (xyz(red)?, xyz(green)?, xyz(blue)?);
    // Columns p_r, p_g, p_b as the columns of `p`: row i of `p` is component i of the three primaries.
    let p = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
    let s_tube = weights(p, xyz(tube_white)?)?;
    let s_target = weights(p, xyz(target)?)?;
    let q = [0, 1, 2].map(|c| s_target[c] / s_tube[c]);
    let max = q.iter().copied().fold(f64::MIN, f64::max);
    (max.is_finite() && max > 0.0).then(|| q.map(|v| (v / max) as f32))
}

/// Luminance shares of the three primaries in the tube white (`tube_*_xy`): `s(tube white)`, which sums to 1
/// because every unit-luminance primary has Y = 1 and the white has Y = 1 (the wear law of T-42 and T-43 is
/// calibrated on this white). `None` when the white is not reachable (see [`white_retune_gains_from_chromaticities`]).
pub fn luminance_shares(params: &Params) -> Option<[f64; 3]> {
    let (r, g, b) = (
        xyz(params.tube_red_xy)?,
        xyz(params.tube_green_xy)?,
        xyz(params.tube_blue_xy)?,
    );
    let p = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
    weights(p, xyz(params.tube_white_xy)?)
}

/// The gains of the frame: the retune from `tube_white_xy` to `gun_retune_white_xy` of the parameter table
/// when `gun_white_balance` is on and both whites are reachable, else the neutral (1, 1, 1).
pub fn white_gains(params: &Params) -> [f32; 3] {
    if !params.gun_white_balance {
        return [1.0; 3];
    }
    white_retune_gains_from_chromaticities(
        params.tube_red_xy,
        params.tube_green_xy,
        params.tube_blue_xy,
        params.tube_white_xy,
        params.gun_retune_white_xy,
    )
    .unwrap_or([1.0; 3])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::ParamValue;

    const SMPTE_C: ([f32; 2], [f32; 2], [f32; 2]) = ([0.630, 0.340], [0.310, 0.595], [0.155, 0.070]);
    const D65: [f32; 2] = [0.3127, 0.3290];
    const D93: [f32; 2] = [0.2831, 0.2971];

    fn retune(tube: [f32; 2], target: [f32; 2]) -> Option<[f32; 3]> {
        white_retune_gains_from_chromaticities(SMPTE_C.0, SMPTE_C.1, SMPTE_C.2, tube, target)
    }

    /// SMPTE-C, D65 tube retuned to D93: s(D65) = (0.212376, 0.701060, 0.086564), s(D93) = (0.176751,
    /// 0.707232, 0.116017), ratios (0.832252, 1.008804, 1.340252), over the largest: (0.62097, 0.75270, 1).
    #[test]
    fn d65_to_d93_gains_are_the_hand_derivation() {
        let g = retune(D65, D93).unwrap();
        for (got, want) in g.iter().zip([0.620_967, 0.752_697, 1.0]) {
            assert!((f64::from(*got) - want).abs() < 2e-5, "{g:?}");
        }
    }

    /// The tube white itself as the target changes nothing: exactly (1, 1, 1), whatever the tube.
    #[test]
    fn target_equal_to_the_tube_white_is_exactly_neutral() {
        assert_eq!(retune(D65, D65), Some([1.0; 3]));
        assert_eq!(retune(D93, D93), Some([1.0; 3]));
    }

    #[test]
    fn unreachable_whites_give_neutral_gains() {
        assert_eq!(retune(D65, [0.7, 0.2]), None);
        assert_eq!(retune([0.7, 0.2], D65), None);
        // Collinear primaries and y = 0.
        assert_eq!(
            white_retune_gains_from_chromaticities([0.3, 0.3], [0.4, 0.4], [0.5, 0.5], [0.4, 0.4], [0.4, 0.4]),
            None
        );
        assert_eq!(retune(D65, [0.3, 0.0]), None);
        let mut p = Params::default();
        assert_eq!(white_gains(&p), [1.0; 3], "off by default");
        assert!(p.set("gun_white_balance", ParamValue::Bool(true)));
        assert_eq!(
            white_gains(&p),
            [1.0; 3],
            "on with the target at the tube white (the default)"
        );
        assert!(p.set("gun_retune_white_xy", ParamValue::Vec2(D93)));
        let g = white_gains(&p);
        assert!(g[0] < 0.63 && g[2] == 1.0, "{g:?}");
        assert!(p.set("gun_retune_white_xy", ParamValue::Vec2([0.7, 0.2])));
        assert_eq!(white_gains(&p), [1.0; 3], "unreachable white falls back to neutral");
    }
}

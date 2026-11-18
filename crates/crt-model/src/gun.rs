//! Gun stage oracle (T-34, QUESTIONS PHY-6 and COL-2): drive voltage V to beam current per channel, and the
//! per-channel gains that set the tube white.
//!
//! Written straight from `CRT.md` "Electro-optical transfer" (gun law, GOG, BT.1886 and its Annex 1) and
//! "White point" (white is a per-channel gain in linear light). The laws themselves are in
//! [`crate::colour`]; this module adds the normalisation of the stage (beam current 1 at full drive, so
//! BT.1886 is divided by its white luminance) and the white balance.
//!
//! White retune (COL-2), a relative 'service-menu' adjustment. A unit beam current of channel c makes the
//! light `p_c` = (x/y, 1, (1-x-y)/y), the XYZ of the phosphor chromaticity at luminance 1. Let `P` =
//! [p_r p_g p_b] and `s(W)` = `P^-1 W` for a white W (XYZ at Y = 1): the weights of the three primaries
//! that make W. The tube, as the output transform sees it (`output::tube_to_display`, built from
//! `colour::rgb_to_xyz(r, g, b, w_tube)`), already has tube RGB (1, 1, 1) = `w_tube` at Y = 1, i.e. its
//! channel c is `s_c(w_tube) p_c`. Gains `g` on the tube RGB therefore make the light
//! `sum g_c s_c(w_tube) p_c`, which has the chromaticity of the target white W exactly when
//! `g_c s_c(w_tube) = k s_c(W)`: `g = s(W) / s(w_tube)` componentwise, up to the common factor k. The
//! gains are scaled so the largest is 1 (a gun can be attenuated, not boosted, and the brightest white is
//! kept). Target = tube white gives (1, 1, 1) exactly (a ratio of equal numbers). Applying the gains
//! `s(W)` (the absolute solution of `P g = W`) on top of that tube would apply the tube white twice.
//!
//! Hand example, SMPTE-C with D65 -> D93: `s(D65)` = (0.212376, 0.701060, 0.086564) (the Y row of the
//! Lindbloom matrix, 0.2124, 0.7010, 0.0865), `s(D93)` = (0.176751, 0.707232, 0.116017), ratios
//! (0.832252, 1.008804, 1.340252), divided by the largest: gains (0.62097, 0.75270, 1).

use crate::colour::{bt1886_annex1_eotf, bt1886_eotf, gun_gog, gun_power, inv3, mul3, rgb_to_xyz, xy_to_xyz, Mat3};

/// Gun law of the `gun` stage (`params/gun.toml`, `gun_law`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Law {
    /// Pure power law, I = V^gamma (the controls below do not apply). [S]
    Power,
    /// Gain-offset-gamma with per-channel cutoff and drive. [K]
    Gog,
    /// ITU-R BT.1886, gamma 2.4 with black lift b from `lw` and `lb`, I = L / Lw. [S]
    Bt1886,
    /// BT.1886 Annex 1, exponents 2.6 above V = 0.35 and 3.0 below, black lift `lift`, I = L / Lw. [S]
    Annex1,
}

/// Parameters of the gun stage, in the units of `params/gun.toml`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gun {
    pub law: Law,
    /// Exponent of `Power` and `Gog`.
    pub gamma: f64,
    /// Contrast k_g (video gain), `Gog` only.
    pub gain: f64,
    /// Brightness k_o (bias before the power law), `Gog` only.
    pub offset: f64,
    /// Per-channel cutoff offsets added to the argument, `Gog` only.
    pub cutoff: [f64; 3],
    /// Per-channel drive (service-menu gain) multiplying the current, `Gog` only.
    pub drive: [f64; 3],
    /// BT.1886 white luminance in cd/m2.
    pub lw: f64,
    /// BT.1886 black luminance in cd/m2.
    pub lb: f64,
    /// Annex 1 black lift b.
    pub lift: f64,
    /// White balance gains multiplying the current of every law (1 when off).
    pub white: [f64; 3],
}

impl Default for Gun {
    /// The table defaults: pure power 2.4, neutral controls, no white balance.
    fn default() -> Self {
        Self {
            law: Law::Power,
            gamma: 2.4,
            gain: 1.0,
            offset: 0.0,
            cutoff: [0.0; 3],
            drive: [1.0; 3],
            lw: 100.0,
            lb: 0.1,
            lift: 0.0,
            white: [1.0; 3],
        }
    }
}

impl Gun {
    /// Relative beam current of `channel` (0 red, 1 green, 2 blue) for drive voltage `v`: 1 at full drive
    /// for `Power`, `Bt1886` and `Annex1` (and for `Gog` with neutral controls), times the white gain.
    ///
    /// # Panics
    /// If `channel` > 2, or for the domains of [`bt1886_eotf`] and [`bt1886_annex1_eotf`].
    pub fn beam_current(&self, channel: usize, v: f64) -> f64 {
        let i = match self.law {
            Law::Power => gun_power(v, self.gamma),
            Law::Gog => self.drive[channel] * gun_gog(v, self.gain, self.offset + self.cutoff[channel], self.gamma),
            Law::Bt1886 => bt1886_eotf(v, self.lw, self.lb) / self.lw,
            // Lw = 1 gives L / Lw directly (k = Lw / (1 + b)^2.6 is linear in Lw).
            Law::Annex1 => bt1886_annex1_eotf(v, 1.0, self.lift),
        };
        i * self.white[channel]
    }
}

/// Matrix of the unit-luminance phosphor lights, columns p_r, p_g, p_b.
fn phosphor_matrix(r: (f64, f64), g: (f64, f64), b: (f64, f64)) -> Mat3 {
    let (pr, pg, pb) = (xy_to_xyz(r.0, r.1), xy_to_xyz(g.0, g.1), xy_to_xyz(b.0, b.1));
    [[pr[0], pg[0], pb[0]], [pr[1], pg[1], pb[1]], [pr[2], pg[2], pb[2]]]
}

/// Weights `s(W) = P^-1 W` of the three unit-luminance primaries that make the white `w` (Y = 1).
///
/// # Panics
/// If the primaries are collinear (singular `P`, see [`inv3`]) or the white is not strictly inside their
/// triangle (a weight would be zero or negative, which no gun can do).
pub fn white_weights(r: (f64, f64), g: (f64, f64), b: (f64, f64), w: (f64, f64)) -> [f64; 3] {
    let s = mul3(&inv3(&phosphor_matrix(r, g, b)), xy_to_xyz(w.0, w.1));
    assert!(
        s.iter().all(|v| *v > 0.0),
        "the white {w:?} is outside the triangle of the primaries {r:?} {g:?} {b:?}: weights {s:?}"
    );
    s
}

/// White retune gains from the tube white `w_tube` to the target white `w_target`: `s(W) / s(w_tube)`
/// componentwise, scaled so max(g) = 1 (module documentation). (1, 1, 1) when the two whites are equal. [K]
///
/// # Panics
/// As [`white_weights`], for either white.
pub fn white_retune_gains(
    r: (f64, f64),
    g: (f64, f64),
    b: (f64, f64),
    w_tube: (f64, f64),
    w_target: (f64, f64),
) -> [f64; 3] {
    let (st, sw) = (white_weights(r, g, b, w_tube), white_weights(r, g, b, w_target));
    let q = [0, 1, 2].map(|c| sw[c] / st[c]);
    let max = q.iter().copied().fold(f64::MIN, f64::max);
    q.map(|v| v / max)
}

/// CIE xy of the light of tube drive (1, 1, 1) after `gains`, for a tube whose RGB (1, 1, 1) is the white
/// `w_tube` (`colour::rgb_to_xyz`): the chromaticity of `sum g_c s_c(w_tube) p_c`.
pub fn white_xy(gains: [f64; 3], r: (f64, f64), g: (f64, f64), b: (f64, f64), w_tube: (f64, f64)) -> (f64, f64) {
    let [x, y, z] = mul3(&rgb_to_xyz(r, g, b, w_tube), gains);
    (x / (x + y + z), y / (x + y + z))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::close;

    const SMPTE_C: [(f64, f64); 3] = [(0.630, 0.340), (0.310, 0.595), (0.155, 0.070)];
    const D65: (f64, f64) = (0.3127, 0.3290);
    const D93: (f64, f64) = (0.2831, 0.2971);

    #[test]
    fn power_and_gog_defaults_agree() {
        let p = Gun::default();
        let g = Gun { law: Law::Gog, ..p };
        for v in [0.0, 0.01, 0.2, 0.5, 1.0] {
            for c in 0..3 {
                assert_eq!(p.beam_current(c, v), g.beam_current(c, v));
            }
        }
        assert!(close(p.beam_current(0, 0.5), 0.18946, 0.000_01));
        assert_eq!(p.beam_current(1, 1.0), 1.0);
    }

    #[test]
    fn bt1886_is_relative_to_white() {
        let g = Gun {
            law: Law::Bt1886,
            ..Gun::default()
        };
        assert!(close(g.beam_current(0, 0.0), 0.1 / 100.0, 1e-6));
        assert!(close(g.beam_current(0, 1.0), 1.0, 1e-9));
    }

    /// The weights of D65 on SMPTE-C primaries are the Y row of the Lindbloom matrix (0.2124, 0.7010,
    /// 0.0865).
    #[test]
    fn weights_of_d65_are_the_luminance_row() {
        let [r, g, b] = SMPTE_C;
        let s = white_weights(r, g, b, D65);
        for (got, want) in s.iter().zip([0.2124, 0.7010, 0.0865]) {
            assert!(close(*got, want, 3e-4), "{s:?}");
        }
        assert!(
            close(s.iter().sum::<f64>(), 1.0, 1e-12),
            "luminance of white is 1: {s:?}"
        );
    }

    /// Retuning to the tube white itself changes nothing: gains exactly (1, 1, 1).
    #[test]
    fn retune_to_the_tube_white_is_neutral() {
        let [r, g, b] = SMPTE_C;
        assert_eq!(white_retune_gains(r, g, b, D65, D65), [1.0; 3]);
        assert_eq!(white_retune_gains(r, g, b, D93, D93), [1.0; 3]);
    }

    /// D65 tube retuned to D93: gains (0.62097, 0.75270, 1) from the hand derivation in the module
    /// documentation, and the light of tube drive (1, 1, 1) after them has the chromaticity of D93.
    #[test]
    fn retune_d65_to_d93() {
        let [r, g, b] = SMPTE_C;
        let gains = white_retune_gains(r, g, b, D65, D93);
        for (got, want) in gains.iter().zip([0.620_967, 0.752_697, 1.0]) {
            assert!(close(*got, want, 2e-5), "{gains:?}");
        }
        let xy = white_xy(gains, r, g, b, D65);
        assert!(close(xy.0, D93.0, 1e-12) && close(xy.1, D93.1, 1e-12), "{xy:?}");
        // And without the gains the tube white is the tube white.
        let xy = white_xy([1.0; 3], r, g, b, D65);
        assert!(close(xy.0, D65.0, 1e-12) && close(xy.1, D65.1, 1e-12), "{xy:?}");
        // The reverse direction: a D93 tube retuned to D65, bluest gun at full drive again.
        let back = white_retune_gains(r, g, b, D93, D65);
        let xy = white_xy(back, r, g, b, D93);
        assert!(close(xy.0, D65.0, 1e-12) && close(xy.1, D65.1, 1e-12), "{xy:?}");
        // 9300 K is bluer: relative to the D65 tube the red gain falls below the blue one.
        assert!(gains[0] < gains[1] && gains[1] < gains[2], "{gains:?}");
    }

    /// Applying the absolute solution `P g = W` (the T-34 design) on a tube that already has its white
    /// double-counts it: this is the bug the relative form fixes. SMPTE-C D65 with the absolute D65 gains
    /// ends at xy (0.334, 0.511) instead of D65.
    #[test]
    fn absolute_gains_on_a_balanced_tube_apply_the_white_twice() {
        let [r, g, b] = SMPTE_C;
        let s = white_weights(r, g, b, D65);
        let max = s.iter().copied().fold(f64::MIN, f64::max);
        let xy = white_xy(s.map(|v| v / max), r, g, b, D65);
        assert!((xy.0 - 0.334).abs() < 1e-3 && (xy.1 - 0.511).abs() < 1e-3, "{xy:?}");
    }

    #[test]
    #[should_panic(expected = "outside the triangle")]
    fn white_outside_the_primaries_panics() {
        let [r, g, b] = SMPTE_C;
        white_retune_gains(r, g, b, D65, (0.7, 0.2));
    }
}

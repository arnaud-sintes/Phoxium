//! Gun stage oracle (T-34, QUESTIONS PHY-6 and COL-2): drive voltage V to beam current per channel, and the
//! per-channel gains that set the tube white.
//!
//! Written straight from `CRT.md` "Electro-optical transfer" (gun law, GOG, BT.1886 and its Annex 1) and
//! "White point" (white is a per-channel gain in linear light). The laws themselves are in
//! [`crate::colour`]; this module adds the normalisation of the stage (beam current 1 at full drive, so
//! BT.1886 is divided by its white luminance) and the white balance.
//!
//! White balance (COL-2): a unit beam current of channel c makes the light `p_c` = (x/y, 1, (1-x-y)/y),
//! the XYZ of the phosphor chromaticity at luminance 1. Drive (1, 1, 1) after gains `g` gives
//! `sum g_c p_c`, which must have the chromaticity of the white W. So `P g = W` with `P` = [p_r p_g p_b]
//! and W = (x/y, 1, (1-x-y)/y) of the white: `g = P^-1 W`. Only ratios matter for chromaticity; the gains
//! are scaled so the largest is 1 (a gun can be attenuated, not boosted, and the brightest white is kept).
//! The luminance of white at full drive is then the sum of the gains (in units of one gun at full drive
//! with unit luminance per unit current; the real luminance scale belongs to the optics and output stages).

use crate::colour::{bt1886_annex1_eotf, bt1886_eotf, gun_gog, gun_power, inv3, mul3, xy_to_xyz, Mat3};

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

/// White balance gains: `P g = W` scaled so max(g) = 1 (module documentation). [D]
///
/// # Panics
/// If the primaries are collinear (singular `P`, see [`inv3`]) or the white is not strictly inside their
/// triangle (a gain would be zero or negative, which no gun can do).
pub fn white_balance_gains(r: (f64, f64), g: (f64, f64), b: (f64, f64), w: (f64, f64)) -> [f64; 3] {
    let s = mul3(&inv3(&phosphor_matrix(r, g, b)), xy_to_xyz(w.0, w.1));
    assert!(
        s.iter().all(|v| *v > 0.0),
        "the white {w:?} is outside the triangle of the primaries {r:?} {g:?} {b:?}: gains {s:?}"
    );
    let max = s.iter().copied().fold(f64::MIN, f64::max);
    s.map(|v| v / max)
}

/// CIE xy of the light of drive (1, 1, 1) after `gains`: the chromaticity of `sum g_c p_c`.
pub fn white_xy(gains: [f64; 3], r: (f64, f64), g: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    let [x, y, z] = mul3(&phosphor_matrix(r, g, b), gains);
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

    /// Drive (1, 1, 1) after the gains has the chromaticity of the target white, for SMPTE-C primaries
    /// with D65 (Y row of the Lindbloom matrix: 0.2124, 0.7010, 0.0865 over its maximum) and D93.
    #[test]
    fn gains_set_the_white() {
        let [r, g, b] = SMPTE_C;
        let gains = white_balance_gains(r, g, b, D65);
        assert!(close(gains[0], 0.2124 / 0.7010, 2e-4), "{gains:?}");
        assert_eq!(gains[1], 1.0);
        assert!(close(gains[2], 0.0865 / 0.7010, 2e-4), "{gains:?}");
        let xy = white_xy(gains, r, g, b);
        assert!(close(xy.0, D65.0, 1e-12) && close(xy.1, D65.1, 1e-12), "{xy:?}");
        let gains = white_balance_gains(r, g, b, D93);
        let xy = white_xy(gains, r, g, b);
        assert!(close(xy.0, D93.0, 1e-12) && close(xy.1, D93.1, 1e-12), "{xy:?}");
        // 9300 K is bluer: relative to D65 the blue gain rises and the red gain falls.
        let d65 = white_balance_gains(r, g, b, D65);
        assert!(gains[2] > d65[2] && gains[0] < d65[0], "{gains:?} vs {d65:?}");
    }

    #[test]
    #[should_panic(expected = "outside the triangle")]
    fn white_outside_the_primaries_panics() {
        let [r, g, b] = SMPTE_C;
        white_balance_gains(r, g, b, (0.7, 0.2));
    }
}

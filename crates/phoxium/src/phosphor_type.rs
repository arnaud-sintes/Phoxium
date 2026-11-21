//! The phosphor-type selector on the CPU side (`phosphor_type`, QUESTIONS PHY-19, CRT.md "Phosphor types").
//!
//! P22 (the default) is the colour tube: nothing here changes a P22 frame (bit for bit). A monochrome type (the
//! data rows of [`crt_model::phosphor_types`]) is resolved into the parameters the existing chain already reads,
//! with one small shader branch (the luma in `shaders/decode.comp`):
//!
//! - **One gun.** `shaders/decode.comp` replaces the source R', G', B' by the BT.601 luma Y' in all three channels
//!   (before the drive voltage and the gun law), so the three channels carry the same signal. [`effective_params`]
//!   gives every per-gun and per-phosphor control (the `_rgb` vectors of the gun, ageing and phosphor stages) its
//!   green value, and the red and blue convergence the green one, so the three channels stay identical through
//!   the beam, the persistence and the wear.
//! - **No mask** (a continuous coating): `mask_enabled` off, the existing bypass (mask weight exactly 1).
//! - **The phosphor's light.** [`mono_output_matrix`]: the equal channels (L, L, L) become the light
//!   L (x / y, 1, (1 - x - y) / y) of the type's chromaticity (Y = L, the luminance of a full white is 1 like the
//!   P22 white), then sRGB. A rank-one matrix: any remaining channel imbalance (the optics tints) changes the
//!   luminance only, never the colour. `output_match_srgb` does not apply (it would show the phosphor as grey).
//! - **No white retune**: a monochrome tube has one phosphor, its colour is not adjustable; `gun_white_balance` is
//!   off and the white gains are (1, 1, 1).
//! - **Decay**: [`decay_fit`] gives the type's fit to all three channels; `phosphor_decay_scale`, the extra
//!   afterglow, the tail strength and `phosphor_persistence_blend` act on top as for P22.
//!
//! Out of gamut: P1, P39, P31 (0.245, 0.523) and the yellow blend lie outside the sRGB triangle. The output transform
//! keeps its existing handling: a negative display channel is clipped to 0 and counted as out of gamut (`encode`).
//! The tests check the chromaticity before that step ([`mono_output_matrix`] times the sRGB-to-XYZ matrix).

use crt_model::colour::{rgb_to_xyz, Mat3};
use crt_model::output::{mul_mat3, D65, SRGB_BLUE, SRGB_GREEN, SRGB_RED};
use crt_model::phosphor::{DecayFit, KUHN_BLUE, KUHN_GREEN, KUHN_RED};
use crt_model::phosphor_types::{mono_light_xyz, mono_type, PhosphorType};

use crate::params::{ParamType, ParamValue, Params, PARAMS_META};

/// The Kuhn (2002) P22 fits per channel, red, green, blue (CRT.md "Decay and persistence").
const P22_FITS: [DecayFit<'static>; 3] = [KUHN_RED, KUHN_GREEN, KUHN_BLUE];

/// The monochrome type selected by `params`, `None` for P22 (or an unknown index, read as the default).
pub fn mono(params: &Params) -> Option<&'static PhosphorType> {
    mono_type(params.phosphor_type)
}

/// The decay fit of channel `ch` (0 red, 1 green, 2 blue) for the `phosphor_type` index `index`: the Kuhn P22 fit
/// of the channel for P22, the type's own fit on every channel otherwise.
pub fn decay_fit(index: u32, ch: usize) -> DecayFit<'static> {
    mono_type(index).map_or(P22_FITS[ch], |t| t.fit)
}

/// The stages whose `_rgb` vectors are per gun or per phosphor (a monochrome tube has one of each).
const PER_GUN_STAGES: [&str; 3] = ["gun", "ageing", "phosphor"];

/// The parameters the chain renders with for the selected phosphor type: `params` itself for P22 (bit for bit);
/// for a monochrome type, the mask off, the white retune off, every per-gun and per-phosphor `_rgb` vector set to
/// its green value and the red and blue convergence to the green one (module doc).
pub fn effective_params(params: &Params) -> Params {
    let mut p = *params;
    if mono(params).is_none() {
        return p;
    }
    p.mask_enabled = false;
    p.gun_white_balance = false;
    for m in PARAMS_META {
        if m.ty == ParamType::Vec3 && m.id.ends_with("_rgb") && PER_GUN_STAGES.contains(&m.stage) {
            if let Some(ParamValue::Vec3(v)) = p.get(m.id) {
                p.set(m.id, ParamValue::Vec3([v[1]; 3]));
            }
        }
    }
    p.geometry_conv_red_dx_mm = p.geometry_conv_green_dx_mm;
    p.geometry_conv_blue_dx_mm = p.geometry_conv_green_dx_mm;
    p.geometry_conv_red_dy_mm = p.geometry_conv_green_dy_mm;
    p.geometry_conv_blue_dy_mm = p.geometry_conv_green_dy_mm;
    p.geometry_conv_red_radial_mm = p.geometry_conv_green_radial_mm;
    p.geometry_conv_blue_radial_mm = p.geometry_conv_green_radial_mm;
    p
}

/// What the chain renders with: [`effective_params`], then the grown spot of a worn gun
/// ([`crate::ageing::effective_params`]).
pub fn render_params(params: &Params) -> Params {
    crate::ageing::effective_params(&effective_params(params))
}

/// The linear tube RGB to linear sRGB matrix of a monochrome type of chromaticity `xy`: each tube channel carries a
/// third of the light, so (L, L, L) gives XYZ = L (x / y, 1, (1 - x - y) / y), converted with the inverse of the
/// sRGB (D65) matrix. `None` when y <= 0 or a value is not finite.
pub fn mono_output_matrix(xy: (f64, f64)) -> Option<Mat3> {
    if !(xy.0.is_finite() && xy.1.is_finite() && xy.1 > 0.0) {
        return None;
    }
    let col = mono_light_xyz(xy, 1.0 / 3.0);
    let tube: Mat3 = [[col[0]; 3], [col[1]; 3], [col[2]; 3]];
    let display = rgb_to_xyz(SRGB_RED, SRGB_GREEN, SRGB_BLUE, D65);
    Some(mul_mat3(&crt_model::colour::inv3(&display), &tube))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{PHOSPHOR_TYPE_P22, PHOSPHOR_TYPE_P31};
    use crt_model::colour::mul3;
    use crt_model::phosphor_types::MONO_TYPES;

    #[test]
    fn values_follow_the_data_rows() {
        let m = crate::params::meta("phosphor_type").unwrap();
        assert_eq!(m.values[0], "p22");
        assert_eq!(Params::default().phosphor_type, PHOSPHOR_TYPE_P22);
        assert_eq!(m.values.len(), MONO_TYPES.len() + 1);
        for (i, t) in MONO_TYPES.iter().enumerate() {
            assert_eq!(m.values[i + 1], t.id);
        }
    }

    #[test]
    fn p22_changes_nothing() {
        let p = Params::default();
        assert_eq!(effective_params(&p), p);
        for (ch, fit) in P22_FITS.iter().enumerate() {
            assert_eq!(decay_fit(PHOSPHOR_TYPE_P22, ch), *fit);
        }
    }

    #[test]
    fn mono_resolves_to_one_gun() {
        let p = Params {
            phosphor_type: PHOSPHOR_TYPE_P31,
            gun_white_balance: true,
            phosphor_saturation_rgb: [0.1, 0.2, 0.3],
            gun_drive_rgb: [0.9, 0.8, 0.7],
            geometry_conv_red_dx_mm: 0.2,
            ..Params::default()
        };
        let e = effective_params(&p);
        assert!(!e.mask_enabled && !e.gun_white_balance);
        assert_eq!(e.phosphor_saturation_rgb, [0.2; 3]);
        assert_eq!(e.gun_drive_rgb, [0.8; 3]);
        assert_eq!(e.geometry_conv_red_dx_mm, e.geometry_conv_green_dx_mm);
        assert_eq!(e.optics_tint_rgb, p.optics_tint_rgb, "glass tints are not per gun");
        assert_eq!(e.phosphor_type, PHOSPHOR_TYPE_P31);
        for ch in 0..3 {
            assert_eq!(decay_fit(PHOSPHOR_TYPE_P31, ch), crt_model::phosphor_types::P31.fit);
        }
    }

    /// The light of a full white, before any gamut clip: the type's xy within 1e-9 and Y = 1.
    #[test]
    fn mono_white_has_the_type_chromaticity() {
        let display = rgb_to_xyz(SRGB_RED, SRGB_GREEN, SRGB_BLUE, D65);
        for t in MONO_TYPES {
            let m = mono_output_matrix(t.xy).unwrap();
            let xyz = mul3(&display, mul3(&m, [1.0; 3]));
            let s = xyz[0] + xyz[1] + xyz[2];
            assert!(
                (xyz[0] / s - t.xy.0).abs() < 1e-9 && (xyz[1] / s - t.xy.1).abs() < 1e-9,
                "{}",
                t.id
            );
            assert!((xyz[1] - 1.0).abs() < 1e-9, "{}", t.id);
        }
        assert!(mono_output_matrix((0.3, 0.0)).is_none());
    }
}

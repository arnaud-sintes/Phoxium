//! SDR headroom readout and the output transform of the headroom modes (T-84; audit B1/M1; PLAN D-16).
//!
//! A full-white field is not flat on the tube: the mask normalises each channel by its coverage (a lit stripe
//! reads 1 / fill, 3 to 4 times the field mean when the mask is resolved) and the scanlines peak above their
//! mean (1.35 times at 0.8 mm FWHM on 1.2 mm lines). SDR output has no room above panel white, so the encode
//! stage must clip, roll off or dim. The readout is the product of the peak/mean factors along the chain
//! ([`crt_model::output::Headroom`]):
//!
//! headroom = mask peak x scanline peak x white matrix maximum x exposure
//!
//! - mask peak: `crt_model::mask::Mask::peak_value` under the per-axis tiers the frame draws (the same CPU
//!   decision as `FrameConstants::with_mask_tiers`, blend band included); 1 when the mask is off, the tube is
//!   not placed or the `screen` slot does not run. In `mask_depth` mode: 1 + depth (peak - 1).
//! - scanline peak: `crt_model::beam::scanline_pixel_peak` at the spot width of full drive (the white gain of
//!   each channel is its current, `SpotGrowth::fwhm`), over host rows of 1 / px_per_mm mm; the largest channel.
//!   The tail is left out (it only lowers the peak). 1 when the beam does not draw lines.
//! - white matrix maximum: `crt_model::output::white_matrix_max` of the colour matrix and the white gains
//!   (positive parts when the mask or the scanlines can separate the channels: different elements,
//!   convergence offsets, per-channel spot widths; the plain row sum only for a flat field).
//!
//! Persistence (normalised to 1 on a still picture), saturation and ABL only lower a white field and are not
//! in the product, which is therefore an upper bound of the brightest output element.
//!
//! The modes (`output_headroom_mode`), applied by `encode` through [`OutputTransform`]:
//! - `shoulder` (default): exposure unchanged; above `output_knee` a roll-off that reaches panel white exactly
//!   at the white point W (`output_shoulder_white`, 0 = the readout x (1 + margin) when the readout exceeds
//!   1); no roll-off (identity up to panel white) when W <= 1 or, automatic, when the readout is at most 1.
//! - `physical`: exposure / max(1, readout x (1 + margin)) and a hard clip that nothing reaches.
//! - `mask_depth`: the mask contrast scaled by `mask_depth` in the shader, then the exposure as in `physical`
//!   from the lowered readout.
//! - `clip`: exposure unchanged, hard clip (T-46).

use crt_model::beam::{scanline_pixel_peak, ScanlineProfile, SpotGrowth};
use crt_model::output::{shoulder_rate, white_matrix_max, Headroom};

use crate::gpu::output_matrix;
use crate::params::{
    Params, OUTPUT_HEADROOM_MODE_CLIP, OUTPUT_HEADROOM_MODE_MASK_DEPTH, OUTPUT_HEADROOM_MODE_PHYSICAL,
};
use crate::placement::{mask_model, mask_tiers};

/// A readout this close above 1 counts as 1 in the automatic shoulder: the rows of a colour matrix between two
/// equal whites sum to 1 only within float noise (1e-7), as the clip counters' CLIP_EPS of `output.glsl`.
const READOUT_EPS: f64 = 1e-4;

/// FWHM over sigma of a Gaussian, 2 sqrt(2 ln 2).
const FWHM_OVER_SIGMA: f64 = 2.354_820_045_030_949;

/// The headroom readout of a frame placed at `px_per_mm` host px per tube mm with scanlines `line_pitch_mm`
/// apart, when `screen` (the beam and mask slot) runs; see the module documentation.
pub fn headroom(params: &Params, px_per_mm: f32, line_pitch_mm: f32, screen: bool) -> Headroom {
    let ppm = f64::from(px_per_mm);
    let placed = screen && ppm.is_finite() && ppm > 0.0;
    let pixel_mm = if placed { 1.0 / ppm } else { 0.0 };
    let mask_peak_physical = if placed && params.mask_enabled {
        let v = mask_model(params).peak_value(mask_tiers(px_per_mm, params), pixel_mm);
        if v.is_finite() {
            v.max(1.0)
        } else {
            1.0
        }
    } else {
        1.0
    };
    let mask_peak = if params.output_headroom_mode == OUTPUT_HEADROOM_MODE_MASK_DEPTH {
        1.0 + f64::from(params.mask_depth.clamp(0.0, 1.0)) * (mask_peak_physical - 1.0)
    } else {
        mask_peak_physical
    };
    let gains = crate::white::white_gains(params).map(f64::from);
    let pitch = f64::from(line_pitch_mm);
    let (mid, peak) = (f64::from(params.beam_fwhm_mid_mm), f64::from(params.beam_fwhm_peak_mm));
    let scanline_peak = if placed && pitch.is_finite() && pitch > 0.0 && mid > 0.0 && peak > 0.0 {
        let growth = SpotGrowth {
            fwhm_mid: mid,
            fwhm_peak: peak,
            n: f64::from(params.beam_growth_exponent),
        };
        gains
            .iter()
            .map(|&g| {
                scanline_pixel_peak(
                    &ScanlineProfile::gaussian(growth.fwhm(g) / FWHM_OVER_SIGMA),
                    pitch,
                    pixel_mm,
                )
            })
            .filter(|v| v.is_finite())
            .fold(1.0, f64::max)
    } else {
        1.0
    };
    Headroom {
        mask_peak,
        mask_peak_physical,
        scanline_peak,
        // The channels peak apart under a mask, but also under scanlines: convergence shifts each channel's
        // lines and the white gains give each its own spot width, so with any structure the positive parts
        // count (measured: the plain row sum misses the EBU preset 02 peak by 0.3 %).
        matrix_max: white_matrix_max(
            &output_matrix(params),
            gains,
            mask_peak > 1.0 + 1e-9 || scanline_peak > 1.0 + 1e-9,
        ),
        exposure: f64::from(params.exposure),
    }
}

/// What `encode` applies for a headroom mode: the effective exposure, and the shoulder as its knee and rate
/// (`crt_model::output::shoulder_with_rate`; knee 1 or rate 0 = hard clip at panel white).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutputTransform {
    pub exposure: f32,
    pub knee: f32,
    pub rate: f32,
}

impl OutputTransform {
    /// The T-46 transform with a hard clip: the exposure parameter as it is.
    pub fn clip(params: &Params) -> Self {
        Self {
            exposure: params.exposure,
            knee: 1.0,
            rate: 0.0,
        }
    }

    /// The transform of `params.output_headroom_mode` for the readout `h` (module documentation).
    pub fn new(params: &Params, h: &Headroom) -> Self {
        let margin = f64::from(params.output_headroom_margin.max(0.0));
        match params.output_headroom_mode {
            OUTPUT_HEADROOM_MODE_CLIP => Self::clip(params),
            OUTPUT_HEADROOM_MODE_PHYSICAL | OUTPUT_HEADROOM_MODE_MASK_DEPTH => Self {
                exposure: h.derived_exposure(margin) as f32,
                ..Self::clip(params)
            },
            _ => {
                // Automatic: nothing to compress when the readout stays under panel white (the margin only
                // widens a shoulder that is needed), so a chain without peaks keeps the exact identity.
                let white = if params.output_shoulder_white > 0.0 {
                    f64::from(params.output_shoulder_white)
                } else if h.total() <= 1.0 + READOUT_EPS {
                    1.0
                } else {
                    h.total() * (1.0 + margin)
                };
                let knee = f64::from(params.output_knee);
                match shoulder_rate(knee, white) {
                    Some(a) => Self {
                        exposure: params.exposure,
                        knee: params.output_knee,
                        rate: a as f32,
                    },
                    None => Self::clip(params),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{ParamValue, OUTPUT_HEADROOM_MODE_SHOULDER};

    /// Unplaced or without `screen`: only the matrix and the exposure count. Defaults: identity matrix and
    /// exposure 1, so the readout is exactly 1 and the shoulder has nothing to compress (hard clip = identity
    /// up to panel white): the identity chain of the T-20 tests holds in the default mode.
    #[test]
    fn unplaced_defaults_read_one_and_keep_the_identity() {
        let p = Params::default();
        assert_eq!(p.output_headroom_mode, OUTPUT_HEADROOM_MODE_SHOULDER);
        for (ppm, pitch, screen) in [(0.0, 0.0, true), (4.97, 1.2, false)] {
            let h = headroom(&p, ppm, pitch, screen);
            assert_eq!(
                (h.mask_peak, h.scanline_peak, h.matrix_max, h.exposure),
                (1.0, 1.0, 1.0, 1.0)
            );
            assert_eq!(OutputTransform::new(&p, &h), OutputTransform::clip(&p));
        }
    }

    /// Default PVM grille on 2560 x 1440 (4.9655 px/mm, 1.54 px per triad): prefiltered, mask peak 1. Spot at
    /// full drive 0.8 mm FWHM (sigma 0.33973 mm) on 240 lines of 1.2083 mm, rows of 0.20139 mm: the readout is
    /// the scanline factor alone, near the pointwise P / (sigma sqrt(2 pi)) = 1.4189 (the neighbours add 0.4 %,
    /// the 0.2 mm rows remove about 1.5 %) and above 1. Physical mode divides the exposure by it (and the margin).
    #[test]
    fn default_tube_on_1440_reads_the_scanline_peak() {
        let mut p = Params::default();
        let ppm = 1440.0 / 290.0;
        let pitch = 290.0 / 240.0;
        let h = headroom(&p, ppm, pitch, true);
        assert_eq!(h.mask_peak, 1.0);
        assert!(h.scanline_peak > 1.35 && h.scanline_peak < 1.43, "{h:?}");
        assert_eq!(h.matrix_max, 1.0);
        assert!(p.set("output_headroom_mode", ParamValue::Enum(OUTPUT_HEADROOM_MODE_PHYSICAL)));
        let t = OutputTransform::new(&p, &h);
        assert!(
            (f64::from(t.exposure) - 1.0 / (h.scanline_peak * 1.02)).abs() < 1e-6,
            "{t:?}"
        );
        assert_eq!((t.knee, t.rate), (1.0, 0.0));
        // Shoulder: the automatic white point is the readout x 1.02.
        assert!(p.set("output_headroom_mode", ParamValue::Enum(OUTPUT_HEADROOM_MODE_SHOULDER)));
        let s = OutputTransform::new(&p, &h);
        let a = shoulder_rate(0.6, h.total() * 1.02).unwrap();
        assert_eq!((s.exposure, s.knee, s.rate), (1.0, 0.6, a as f32));
        // A fixed white point overrides the readout.
        assert!(p.set("output_shoulder_white", ParamValue::F32(4.0)));
        assert_eq!(
            OutputTransform::new(&p, &h).rate,
            shoulder_rate(0.6, 4.0).unwrap() as f32
        );
    }

    /// Mask depth: the mask peak becomes 1 + depth (peak - 1), the exposure follows the lowered readout.
    /// Grille 0.78 mm integrated on 1440 / 380 = 3.7895 px/mm: the physical peak is p / pixel = 2.9558.
    #[test]
    fn mask_depth_lowers_the_mask_peak() {
        let mut p = Params::default();
        for (id, v) in [
            ("mask_pitch_mm", ParamValue::F32(0.78)),
            ("mask_tier", ParamValue::Enum(crate::params::MASK_TIER_INTEGRATED)),
            (
                "output_headroom_mode",
                ParamValue::Enum(OUTPUT_HEADROOM_MODE_MASK_DEPTH),
            ),
            ("mask_depth", ParamValue::F32(0.5)),
        ] {
            assert!(p.set(id, v), "{id}");
        }
        let ppm = 1440.0_f32 / 380.0;
        let h = headroom(&p, ppm, 380.0 / 240.0, true);
        let phys = 0.78 * f64::from(ppm);
        assert!((h.mask_peak_physical - phys).abs() < 1e-4, "{h:?}");
        assert!((h.mask_peak - (1.0 + 0.5 * (phys - 1.0))).abs() < 1e-4, "{h:?}");
        let t = OutputTransform::new(&p, &h);
        assert!((f64::from(t.exposure) - 1.0 / (h.total() * 1.02)).abs() < 1e-6);
        // The other modes keep the physical mask in the readout.
        assert!(p.set("output_headroom_mode", ParamValue::Enum(OUTPUT_HEADROOM_MODE_PHYSICAL)));
        assert_eq!(headroom(&p, ppm, 380.0 / 240.0, true).mask_peak, h.mask_peak_physical);
        assert!(p.set("output_headroom_mode", ParamValue::Enum(OUTPUT_HEADROOM_MODE_CLIP)));
        assert_eq!(OutputTransform::new(&p, &h), OutputTransform::clip(&p));
    }
}

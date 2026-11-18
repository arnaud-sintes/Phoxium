//! Source decode and tube placement (T-30, QUESTIONS SRC-3, SRC-4, SRC-5, OUT-7, PHY-3): the oracle of the
//! `decode` stage and of the CPU placement code of the application.
//!
//! Continuous (unrounded) maths in f64 and millimetres. The application rounds the tube rectangle to whole
//! host pixels; the tests of `tests/stage_source.rs` compare its numbers with these functions and with
//! the hand derivations in the tests below.

use crate::colour::srgb_eotf;
use crate::geometry::line_pitch_mm;

/// Display aspect (width / height) of a picture of `native_w` x `native_h` source pixels whose pixels have
/// the pixel aspect ratio `par` (width / height of one pixel): `native_w par / native_h`. CRT.md "Pixel
/// aspect ratio": NES 256 x 240 at 8:7 gives 256 x 8/7 / 240 = 1.2190. [D]
pub fn display_aspect(native_w: f64, native_h: f64, par: f64) -> f64 {
    native_w * par / native_h
}

/// Size (w, h) in mm of a picture of display aspect `aspect` drawn as large as possible inside the
/// visible area `visible_mm` (centred, so only the size matters): full height when the picture is not
/// wider than the area, else full width. [D]
pub fn raster_size_mm(visible_mm: (f64, f64), aspect: f64) -> (f64, f64) {
    let (w, h) = visible_mm;
    if aspect <= w / h {
        (h * aspect, h)
    } else {
        (w, w / aspect)
    }
}

/// Host pixels per tube mm of a raster of `raster_mm` fitted into an output of `target_px` of which at most
/// the fraction `height_fraction` of the height is used: the smaller of the width and the height fits. [D]
pub fn px_per_mm(raster_mm: (f64, f64), target_px: (f64, f64), height_fraction: f64) -> f64 {
    (target_px.0 / raster_mm.0).min(height_fraction * target_px.1 / raster_mm.1)
}

/// Scanline pitch in mm: raster height over source rows x line multiplier (`CRT.md` "Scanline structure",
/// SRC-3). Same formula as [`line_pitch_mm`] with `rows x multiplier` active lines. [D]
pub fn scanline_pitch_mm(raster_height_mm: f64, native_rows: f64, line_multiplier: f64) -> f64 {
    line_pitch_mm(raster_height_mm, native_rows * line_multiplier)
}

/// Host pixels per triad for a triad pitch in mm. [D]
pub fn px_per_triad_at(px_per_mm: f64, triad_pitch_mm: f64) -> f64 {
    px_per_mm * triad_pitch_mm
}

/// Drive voltage V in [0, 1] of an 8-bit `code` read as sRGB (SRC-5 `srgb` mode): the tube must produce the
/// sRGB luminance L = srgb_eotf(code / 255), and the pure-power gun law gives L = V^gamma, so
/// V = L^(1 / gamma). [D]
///
/// # Panics
/// Unless `tube_gamma` > 0.
pub fn drive_voltage_from_srgb(code: f64, tube_gamma: f64) -> f64 {
    assert!(tube_gamma > 0.0, "tube gamma must be positive: {tube_gamma}");
    srgb_eotf(code / 255.0).powf(1.0 / tube_gamma)
}

/// Drive voltage of an 8-bit `code` in the default `drive_voltage` mode (SRC-5): `code / 255`. [K]
pub fn drive_voltage_from_code(code: f64) -> f64 {
    code / 255.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::close;
    use crate::colour::gun_power;
    use crate::geometry::{px_per_triad, triads_across, triads_per_source_px};

    /// NES 256 x 240 at PAR 8:7: 256 x (8 / 7) = 292.571 square-pixel columns over 240 rows, 1.2190.
    #[test]
    fn nes_display_aspect() {
        assert!(close(display_aspect(256.0, 240.0, 8.0 / 7.0), 1.219_048, 1e-6));
        assert!(close(display_aspect(320.0, 200.0, 1.0), 1.6, 1e-12));
        // VGA 320 x 200 at PAR 5:6 fills a 4:3 tube: 320 x 5/6 / 200 = 1.3333.
        assert!(close(display_aspect(320.0, 200.0, 5.0 / 6.0), 4.0 / 3.0, 1e-12));
    }

    /// PVM-20M4U class, 290 mm high, 4:3: 386.667 mm wide; on a 2560 x 1440 output at full height
    /// 1440 / 290 = 4.9655 px/mm, and 1920 px wide.
    #[test]
    fn pvm_20m4u_on_2560_by_1440() {
        let vis = (290.0 * 4.0 / 3.0, 290.0);
        let raster = raster_size_mm(vis, 4.0 / 3.0);
        assert!(close(raster.0, 386.666_667, 1e-5) && close(raster.1, 290.0, 1e-12));
        let s = px_per_mm(raster, (2560.0, 1440.0), 1.0);
        assert!(close(s, 4.965_517, 1e-6), "px per mm {s}");
        assert!(close(raster.0 * s, 1920.0, 1e-9));
        // Line pitch, 240 lines: 290 / 240 = 1.20833 mm; two lines per row halves it.
        assert!(close(scanline_pitch_mm(290.0, 240.0, 1.0), 1.208_333, 1e-6));
        assert!(close(scanline_pitch_mm(290.0, 240.0, 2.0), 0.604_167, 1e-6));
        // Host pixels per scanline: 6.000 (1.20833 x 4.9655).
        assert!(close(px_per_triad_at(s, 1.208_333_333), 6.0, 1e-6));
        // 0.31 mm triads: 4.9655 x 0.31 = 1.5393 px per triad. CRT.md's 1.54 (1920 / 1245) uses the
        // rounded 386 mm width: 0.2 % apart.
        let per_triad = px_per_triad_at(s, 0.31);
        assert!(close(per_triad, 1.539_31, 1e-5), "{per_triad}");
        let crt_md = px_per_triad(1920.0, triads_across(386.0, 0.31));
        assert!(close(per_triad, crt_md, 0.004), "{per_triad} vs CRT.md {crt_md}");
        // Triads per source pixel: 1247.31 triads / 256 = 4.872 (CRT.md reference table: 3.9 at 320 wide).
        let across = triads_across(raster.0, 0.31);
        assert!(close(triads_per_source_px(across, 256.0), 4.872_3, 1e-3));
        assert!(close(triads_per_source_px(across, 320.0), 3.897_8, 1e-3));
    }

    /// A 16:9 picture on a 4:3 tube is limited by the width: 386.667 / 1.7778 = 217.5 mm high, so 180 lines
    /// are 217.5 / 180 = 1.2083 mm apart, while the full-height 290 mm would give 1.611.
    #[test]
    fn wide_picture_is_width_limited() {
        let vis = (290.0 * 4.0 / 3.0, 290.0);
        let r = raster_size_mm(vis, 16.0 / 9.0);
        assert!(close(r.0, 386.666_667, 1e-5) && close(r.1, 217.5, 1e-5), "{r:?}");
        assert!(close(scanline_pitch_mm(r.1, 180.0, 1.0), 1.208_333, 1e-5));
        // On 2560 x 1440: both limits coincide, 1440 / 217.5 = 6.6207 = 2560 / 386.667.
        assert!(close(px_per_mm(r, (2560.0, 1440.0), 1.0), 6.620_69, 1e-5));
    }

    #[test]
    fn height_fraction_scales_the_pixels_not_the_tube() {
        let vis = (290.0 * 4.0 / 3.0, 290.0);
        let r = raster_size_mm(vis, 4.0 / 3.0);
        let half = px_per_mm(r, (2560.0, 1440.0), 0.5);
        assert!(close(half, 720.0 / 290.0, 1e-12));
        // The physical pitch is a property of the tube: unchanged.
        assert_eq!(scanline_pitch_mm(r.1, 240.0, 1.0), scanline_pitch_mm(290.0, 240.0, 1.0));
    }

    /// Level 20 of 255 (above the sRGB toe, 0.0784 > 0.04045): u = (20 / 255 + 0.055) / 1.055 = 0.126475,
    /// L = u^2.4: ln u = -2.06767, x 2.4 = -4.96241, exp = 0.006995 (the plan's beam current). With a
    /// pure 2.4 gun law V = L^(1 / 2.4) = u = 0.126475 exactly. The pure 2.4 law applied to the code
    /// itself gives (20 / 255)^2.4 = 0.00222 (CRT.md).
    #[test]
    fn srgb_level_20_drive_voltage() {
        let l = srgb_eotf(20.0 / 255.0);
        assert!(close(l, 0.006_995, 2e-6), "{l}");
        let v = drive_voltage_from_srgb(20.0, 2.4);
        assert!(close(v, 0.126_475, 1e-6), "{v}");
        assert!(close(gun_power(v, 2.4), l, 1e-12), "round trip through the gun law");
        assert!(close(gun_power(drive_voltage_from_code(20.0), 2.4), 0.002_22, 1e-5));
        // White and black map to themselves.
        assert!(close(drive_voltage_from_srgb(255.0, 2.4), 1.0, 1e-12));
        assert_eq!(drive_voltage_from_srgb(0.0, 2.4), 0.0);
        // Gamma 1 (identity gun): V is the linear light.
        assert!(close(drive_voltage_from_srgb(20.0, 1.0), l, 1e-12));
    }
}

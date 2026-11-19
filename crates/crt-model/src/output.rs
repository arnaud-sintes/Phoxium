//! Output transform oracle (T-46): tube colours to the host display, soft shoulder, host encode, dither.
//!
//! Order applied by the `encode` stage: exposure, 3x3 matrix from the tube primaries and white to the
//! sRGB/Rec.709 primaries (linear light), clip counters, shoulder, host EOTF inverse, optional dither.

use crate::colour::{inv3, mul3, rgb_to_xyz, Mat3};

/// sRGB / Rec.709 red, green, blue and white (D65) chromaticities. [S] (CRT.md, Chromaticity table)
pub const SRGB_RED: (f64, f64) = (0.640, 0.330);
pub const SRGB_GREEN: (f64, f64) = (0.300, 0.600);
pub const SRGB_BLUE: (f64, f64) = (0.150, 0.060);
pub const D65: (f64, f64) = (0.3127, 0.3290);

/// The 3x3 identity.
pub const IDENTITY: Mat3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// Matrix product `a b`.
pub fn mul_mat3(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut m = [[0.0; 3]; 3];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    m
}

/// Linear tube RGB (each channel 1 = full drive, tube white = (1, 1, 1) at Y = 1) to linear sRGB:
/// `inv(M_srgb) M_tube`, both from [`rgb_to_xyz`]. A white point that differs from D65 acts as a
/// per-channel gain, not a chromatic adaptation (CRT.md, White point). [K]
///
/// # Panics
/// As [`rgb_to_xyz`] (collinear primaries, y <= 0).
pub fn tube_to_display(r: (f64, f64), g: (f64, f64), b: (f64, f64), w: (f64, f64)) -> Mat3 {
    let display = rgb_to_xyz(SRGB_RED, SRGB_GREEN, SRGB_BLUE, D65);
    mul_mat3(&inv3(&display), &rgb_to_xyz(r, g, b, w))
}

/// Applies a matrix to a linear RGB triple.
pub fn apply(m: &Mat3, rgb: [f64; 3]) -> [f64; 3] {
    mul3(m, rgb)
}

/// Soft shoulder of one linear channel. Negative values clip to 0 (gamut clip in linear light). Up to
/// `knee` the value is unchanged; above it `k + (1 - k)(1 - exp(-(x - k) / (1 - k)))`, which is continuous
/// with slope 1 at the knee, monotonic and tends to 1. `knee` >= 1 is a hard clip at 1. [E]
pub fn shoulder(x: f64, knee: f64) -> f64 {
    if x <= 0.0 {
        0.0
    } else if knee >= 1.0 {
        x.min(1.0)
    } else if x <= knee {
        x
    } else {
        knee + (1.0 - knee) * (1.0 - (-(x - knee) / (1.0 - knee)).exp())
    }
}

/// Rate `a` of the shoulder that reaches panel white exactly at the input `white` (T-84, D-16 shoulder mode),
/// for a knee `knee` < 1: with t = (x - k) / (1 - k) and T = (white - k) / (1 - k), the curve above the knee
/// is `k + (1 - k) g(t)`, g(t) = (1 - exp(-a t)) / a, so g(0) = 0, g'(0) = 1 (slope 1 at the knee), g is
/// increasing and concave, and g(T) = 1 needs 1 - exp(-a T) = a. That equation has the root a = 0 and, for
/// T > 1 only (its slope at 0 is T - 1), one root in (0, 1), found here by bisection (f(a) = 1 - exp(-aT) - a
/// is positive below the root and negative above it). As T grows the root tends to 1 - exp(-T) -> 1: a = 1 is
/// the T-46 asymptotic shoulder [`shoulder`]. `None` when nothing needs compressing (knee >= 1 or white <= 1:
/// the transform is then the identity up to 1 and a hard clip above). [D]
pub fn shoulder_rate(knee: f64, white: f64) -> Option<f64> {
    if !(knee < 1.0 && white > 1.0 && knee.is_finite() && white.is_finite()) {
        return None;
    }
    let t = (white - knee) / (1.0 - knee);
    let f = |a: f64| 1.0 - (-a * t).exp() - a;
    // Bracket: f(1) = -exp(-T) < 0. The lower end must be above the trivial root a = 0: at a = (T - 1) / T^2,
    // u = a T = (T - 1) / T < 1 and f = u (T - 1) / T - u^2 / 2 + u^3 / 6 - ... = u^2 / 2 + u^3 / 6 - ... > 0.
    let (mut lo, mut hi) = ((t - 1.0) / (t * t), 1.0);
    if f(lo) <= 0.0 {
        return Some(lo);
    }
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        if f(mid) > 0.0 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Some(0.5 * (lo + hi))
}

/// The shoulder of rate `a` ([`shoulder_rate`]): 0 for negative values, the value up to `knee`, above it
/// `k + (1 - k) min((1 - exp(-a (x - k) / (1 - k))) / a, 1)`. `rate` = None (or knee >= 1) is the hard clip
/// min(x, 1). Rate 1 is the T-46 shoulder [`shoulder`] exactly. This is what `encode.frag` evaluates (in f32).
pub fn shoulder_with_rate(x: f64, knee: f64, rate: Option<f64>) -> f64 {
    match rate {
        Some(a) if knee < 1.0 && a > 0.0 => {
            if x <= 0.0 {
                0.0
            } else if x <= knee {
                x
            } else {
                let t = (x - knee) / (1.0 - knee);
                knee + (1.0 - knee) * ((1.0 - (-a * t).exp()) / a).min(1.0)
            }
        }
        _ => x.clamp(0.0, 1.0),
    }
}

/// SDR headroom readout (T-84, audit B1/M1, D-16): how far above panel white the brightest element of a
/// full-white field goes after the output exposure and colour matrix, as the product of independent peaks
/// over their means. Every factor is a peak/mean ratio that keeps the field mean, so the product bounds the
/// output peak from above (the peaks of the mask and of the scanlines may not coincide). [D]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Headroom {
    /// Largest host-pixel mask value over the pixel phase and the channels (1 = flat: prefiltered, mask off
    /// or not drawn), after the mask depth when that mode is on.
    pub mask_peak: f64,
    /// The same without the mask depth (the physical mask).
    pub mask_peak_physical: f64,
    /// Largest host-row mean of the summed scanline profile of a flat field over its mean (1 = no raster
    /// structure), at the spot width of full white.
    pub scanline_peak: f64,
    /// Largest display channel of the tube white through the colour matrix and the white gains (row sums,
    /// or sums of the positive entries when the mask separates the channels, see [`white_matrix_max`]).
    pub matrix_max: f64,
    /// The `exposure` parameter.
    pub exposure: f64,
}

impl Headroom {
    /// The readout: mask peak x scanline peak x matrix maximum x exposure. Above 1, some element of a full-white
    /// field is brighter than panel white and the SDR output must clip, roll off or be dimmed.
    pub fn total(&self) -> f64 {
        self.mask_peak * self.scanline_peak * self.matrix_max * self.exposure
    }

    /// The exposure the `physical` and `mask_depth` modes apply: `exposure / max(1, total (1 + margin))`, so the
    /// readout peak lands at 1 / (1 + margin) of panel white and nothing clips. [D]
    pub fn derived_exposure(&self, margin: f64) -> f64 {
        self.exposure / (self.total() * (1.0 + margin.max(0.0))).max(1.0)
    }
}

/// Largest display channel of the white (tube RGB = `gains`) through the matrix `m` (T-84). When the mask is
/// drawn (`separated`), the three channels peak on different elements, so a channel i of the display can reach
/// sum_j max(m_ij, 0) g_j times the peak (a negative entry is not offset where its channel is dark); with the
/// channels coincident (no resolved mask) the white is the plain row sum sum_j m_ij g_j. The maximum over the
/// rows, at least 0. [D]
pub fn white_matrix_max(m: &Mat3, gains: [f64; 3], separated: bool) -> f64 {
    m.iter()
        .map(|row| {
            row.iter()
                .zip(gains)
                .map(|(v, g)| if separated { v.max(0.0) * g } else { v * g })
                .sum::<f64>()
        })
        .fold(0.0, f64::max)
}

/// Inverse of the pure power EOTF: V = L^(1/gamma), L clamped to >= 0. [S]
pub fn encode_power(l: f64, gamma: f64) -> f64 {
    l.max(0.0).powf(1.0 / gamma)
}

/// Inverse of the piecewise sRGB EOTF (IEC 61966-2-1), linear light to code value. [K]
pub fn encode_srgb(l: f64) -> f64 {
    let l = l.max(0.0);
    if l <= 0.003_130_8 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

/// Entry (x, y) of the 8x8 Bayer matrix, 0..=63: the bits of `x ^ y` and `y`, interleaved and read
/// reversed (a0 y0 a1 y1 a2 y2 from the most significant bit, a = x ^ y). Row 0 starts 0, 32, 8, 40, 2.
pub fn bayer8(x: u32, y: u32) -> u32 {
    let (x, y) = (x % 8, y % 8);
    let a = x ^ y;
    let bit = |v: u32, i: u32| (v >> i) & 1;
    (bit(a, 0) << 5) | (bit(y, 0) << 4) | (bit(a, 1) << 3) | (bit(y, 1) << 2) | (bit(a, 2) << 1) | bit(y, 2)
}

/// Threshold of the ordered dither at pixel (x, y), in [0, 1): (bayer8 + 0.5) / 64.
pub fn ordered_threshold(x: u32, y: u32) -> f64 {
    (f64::from(bayer8(x, y)) + 0.5) / 64.0
}

/// Output code (integer, as f64) of the value `v` in [0, 1] for a target of `levels` steps (255 for
/// 8 bits) dithered with `threshold` in [0, 1): `floor(v levels + threshold)`, so the error is below one
/// code and its mean over uniform thresholds is zero.
pub fn dithered_code(v: f64, levels: f64, threshold: f64) -> f64 {
    (v.clamp(0.0, 1.0) * levels + threshold).floor().min(levels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::close;

    #[test]
    fn srgb_primaries_with_d65_are_identity() {
        let m = tube_to_display(SRGB_RED, SRGB_GREEN, SRGB_BLUE, D65);
        for (i, row) in m.iter().enumerate() {
            for (j, v) in row.iter().enumerate() {
                assert!(close(*v, f64::from(u8::from(i == j)), 1e-9), "[{i}][{j}] = {v}");
            }
        }
    }

    /// SMPTE-C (RP 145) with D65 to sRGB. By hand from the 4-decimal matrices of the colour tests:
    /// row 0 = inv(M_srgb) row 0 (3.2406, -1.5372, -0.4986) times the SMPTE-C columns:
    /// c0 = 3.2406 x 0.3936 - 1.5372 x 0.2124 - 0.4986 x 0.0187 = 1.27553 - 0.32650 - 0.00932 = 0.9397.
    /// The published matrix (Lindbloom-style SMPTE-C to Rec.709) is
    /// [[0.9396, 0.0502, 0.0103], [0.0178, 0.9658, 0.0164], [-0.0016, -0.0044, 1.0060]].
    #[test]
    fn smpte_c_to_srgb_matrix() {
        let m = tube_to_display((0.630, 0.340), (0.310, 0.595), (0.155, 0.070), D65);
        let expected = [
            [0.9396, 0.0502, 0.0103],
            [0.0178, 0.9658, 0.0164],
            [-0.0016, -0.0044, 1.0060],
        ];
        for (i, (row, want)) in m.iter().zip(expected).enumerate() {
            for (j, (g, w)) in row.iter().zip(want).enumerate() {
                assert!(close(*g, w, 6e-4), "[{i}][{j}] {g} vs {w}");
            }
            // Same white point on both sides: white maps to white, rows sum to 1.
            assert!(close(row.iter().sum::<f64>(), 1.0, 1e-9), "row {i}");
        }
    }

    #[test]
    fn shoulder_is_identity_below_the_knee_and_monotonic_above() {
        for x in [0.0, 0.1, 0.5, 0.8] {
            assert_eq!(shoulder(x, 0.8), x);
        }
        let mut last = 0.8;
        for i in 1..=60 {
            let y = shoulder(0.8 + 0.05 * f64::from(i), 0.8);
            assert!(y > last && y < 1.0, "{y}");
            last = y;
        }
        // Continuous slope at the knee: slope 1 on both sides.
        let h = 1e-6;
        assert!(close((shoulder(0.8 + h, 0.8) - 0.8) / h, 1.0, 1e-5));
        // Knee 0.8, x = 1: 0.8 + 0.2 (1 - exp(-1)) = 0.8 + 0.2 x 0.632121 = 0.926424.
        assert!(close(shoulder(1.0, 0.8), 0.926_424, 1e-6));
        // Knee 1 is a hard clip, negatives clip to 0.
        assert_eq!(shoulder(1.7, 1.0), 1.0);
        assert_eq!(shoulder(-0.2, 0.8), 0.0);
    }

    /// T-84 shoulder to a white point. Knee 0.6, white 3: T = (3 - 0.6) / 0.4 = 6, the root of
    /// 1 - exp(-6a) = a is a = 0.99752 (exp(-5.985) = 0.002515, 1 - 0.002515 = 0.997485). The curve reaches 1 at
    /// x = 3 exactly, has slope 1 at the knee, increases, and is the identity below the knee. Rate 1 is the
    /// T-46 shoulder; no rate (white <= 1 or knee >= 1) is the hard clip.
    #[test]
    fn shoulder_reaches_white_exactly_at_the_white_point() {
        let a = shoulder_rate(0.6, 3.0).unwrap();
        assert!(close(a, 0.997_485, 2e-6), "{a}");
        assert!(close(1.0 - (-6.0 * a).exp(), a, 1e-12));
        assert!(close(shoulder_with_rate(3.0, 0.6, Some(a)), 1.0, 1e-12));
        assert_eq!(shoulder_with_rate(5.0, 0.6, Some(a)), 1.0);
        assert_eq!(shoulder_with_rate(0.5, 0.6, Some(a)), 0.5);
        assert_eq!(shoulder_with_rate(-0.5, 0.6, Some(a)), 0.0);
        let h = 1e-7;
        assert!(close((shoulder_with_rate(0.6 + h, 0.6, Some(a)) - 0.6) / h, 1.0, 1e-5));
        let mut last = 0.6;
        for i in 1..=48 {
            let y = shoulder_with_rate(0.6 + 0.05 * f64::from(i), 0.6, Some(a));
            assert!(y > last && y <= 1.0 + 1e-12, "{y}");
            last = y;
        }
        // A nearer white point compresses harder: T = 1.5 (white 1.2): 1 - exp(-1.5 a) = a at a = 0.58281
        // (exp(-0.874215) = 0.417186, 1 - 0.417186 = 0.582814).
        let b = shoulder_rate(0.6, 1.2).unwrap();
        assert!(close(b, 0.582_81, 1e-5), "{b}");
        assert!(close(shoulder_with_rate(1.2, 0.6, Some(b)), 1.0, 1e-12));
        // Rate 1 is the T-46 shoulder.
        for x in [0.7, 1.0, 2.0, 9.0] {
            assert!(close(shoulder_with_rate(x, 0.8, Some(1.0)), shoulder(x, 0.8), 1e-15));
        }
        // Nothing to compress.
        assert_eq!(shoulder_rate(0.6, 1.0), None);
        assert_eq!(shoulder_rate(1.0, 3.0), None);
        assert_eq!(shoulder_with_rate(0.9, 0.6, None), 0.9);
        assert_eq!(shoulder_with_rate(1.7, 0.6, None), 1.0);
        // Far white points tend to the asymptotic shoulder: T = 101 gives a = 1 - exp(-101) to f64.
        assert!(close(shoulder_rate(0.0, 101.0).unwrap(), 1.0, 1e-12));
        // Just above 1 the rate tends to 0 and the curve to the straight line up to the white point.
        let c = shoulder_rate(0.6, 1.0 + 1e-6).unwrap();
        assert!(c > 0.0 && c < 1e-5, "{c}");
    }

    /// The headroom product and the derived exposure (T-84). Mask 3.2 x scanlines 1.35 x matrix 1.233 x
    /// exposure 0.81 = 4.32 x 1.233 x 0.81 = 4.314514; physical exposure with margin 0.02: 0.81 / (4.314514 x
    /// 1.02) = 0.18405, whose readout peak is 1 / 1.02.
    #[test]
    fn headroom_product_and_derived_exposure() {
        let h = Headroom {
            mask_peak: 3.2,
            mask_peak_physical: 3.2,
            scanline_peak: 1.35,
            matrix_max: 1.233,
            exposure: 0.81,
        };
        assert!(close(h.total(), 3.2 * 1.35 * 1.233 * 0.81, 1e-12));
        assert!(close(h.total(), 4.314_514, 1e-6));
        let e = h.derived_exposure(0.02);
        assert!(close(e, 0.81 / (4.314_514 * 1.02), 1e-6));
        assert!(close(h.total() / 0.81 * e, 1.0 / 1.02, 1e-12));
        // Under 1 there is nothing to dim.
        let low = Headroom {
            mask_peak: 1.0,
            scanline_peak: 1.0,
            ..h
        };
        assert!(low.total() < 1.0);
        assert_eq!(low.derived_exposure(0.0), 0.81);
    }

    /// White through the matrix: SMPTE-C to sRGB has row sums 1 (same white) and negative entries in row 2;
    /// with the channels separated the positive parts count: row 0 0.9396 + 0.0502 + 0.0103 = 1.0001, row 2
    /// 1.0060, so 1.006 (the pure SMPTE-C blue of preset 01's note). Identity: 1 either way; gains scale it.
    #[test]
    fn white_matrix_maximum() {
        let m = tube_to_display((0.630, 0.340), (0.310, 0.595), (0.155, 0.070), D65);
        assert!(close(white_matrix_max(&m, [1.0; 3], false), 1.0, 1e-9));
        assert!(close(white_matrix_max(&m, [1.0; 3], true), 1.006, 6e-4));
        assert_eq!(white_matrix_max(&IDENTITY, [1.0; 3], true), 1.0);
        assert_eq!(white_matrix_max(&IDENTITY, [0.6, 0.75, 1.0], false), 1.0);
        assert_eq!(white_matrix_max(&IDENTITY, [0.6, 0.75, 0.5], true), 0.75);
    }

    #[test]
    fn encode_round_trips_through_the_eotf() {
        for code in 0..=255 {
            let v = f64::from(code) / 255.0;
            let l = crate::colour::gun_power(v, 2.2);
            assert!(close(encode_power(l, 2.2), v, 1e-12));
            let l = crate::colour::srgb_eotf(v);
            assert!(close(encode_srgb(l), v, 1e-12));
        }
    }

    /// 20/255 dark tone (CRT.md): pure 2.4 gives 0.00222, pure 2.2 gives 0.00370 linear. A host that is
    /// pure 2.2 shows the second exactly; for the first it needs code 255 x 0.00222^(1/2.2).
    #[test]
    fn dark_tone_codes() {
        let l24 = crate::colour::gun_power(20.0 / 255.0, 2.4);
        assert!(close(l24, 0.00222, 1e-5));
        // 0.00222^(1/2.2) = exp(ln(0.00222) / 2.2) = exp(-6.1105 / 2.2) = exp(-2.7775) = 0.06217: code 15.85.
        assert!(close(encode_power(l24, 2.2) * 255.0, 15.85, 0.05));
        assert!(close(encode_power(0.00370, 2.2) * 255.0, 20.0, 0.05));
    }

    #[test]
    fn bayer_matrix_is_the_standard_one() {
        let row0: Vec<u32> = (0..8).map(|x| bayer8(x, 0)).collect();
        assert_eq!(row0, [0, 32, 8, 40, 2, 34, 10, 42]);
        let row1: Vec<u32> = (0..8).map(|x| bayer8(x, 1)).collect();
        assert_eq!(row1, [48, 16, 56, 24, 50, 18, 58, 26]);
        let mut seen = [false; 64];
        for y in 0..8 {
            for x in 0..8 {
                seen[bayer8(x, y) as usize] = true;
            }
        }
        assert!(seen.iter().all(|s| *s));
    }

    #[test]
    fn dither_mean_is_unbiased_over_one_period() {
        // Flat value 100.5 codes: the 64 thresholds (k + 0.5) / 64 give floor(100.5 + t) = 101 when
        // t >= 0.5 (32 of 64), else 100: the mean is 100.5 exactly.
        let v = 100.5 / 255.0;
        let mut sum = 0.0;
        for y in 0..8 {
            for x in 0..8 {
                sum += dithered_code(v, 255.0, ordered_threshold(x, y));
            }
        }
        assert!(close(sum / 64.0, 100.5, 1e-9), "{}", sum / 64.0);
    }
}

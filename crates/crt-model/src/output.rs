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

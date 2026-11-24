//! The knobs of a television (TODO Low, "TV knobs ... refresh flicker"; parameters `params/tv.toml`), applied to the
//! drive voltage R'G'B' just before the gun, in this order:
//!
//! 1. **Sharpness**: the aperture correction of the luma, `Y' = Y + s (Y - LP(Y))`, with `LP` a triangular
//!    low-pass over `sharpness_px` source pixels (half width `R = round(sharpness_px x K)` samples, weights
//!    `(R + 1 - |j|) / (R + 1)^2`, the line's ends repeated). Positive `s` is peaking (edges overshoot), negative
//!    softens (`s = -1` is the low-pass itself). The change is added to the three channels alike, so the colour
//!    differences, and with them the hue, do not move.
//! 2. **Tint and colour**: the colour differences `U = 0.492 (B' - Y)` and `V = 0.877 (R' - Y)` (BT.601) are
//!    turned by `tint_deg` and scaled by `colour`; green is rebuilt from the luma equation. Luma is untouched.
//! 3. **Black and white level**: `V' = black + (white - black) V` per channel: the voltages of black and of white.
//!    Defaults 0 and 1 are the identity; below black the picture crushes (no clamp here, the slot clamps at the
//!    end), above white it clips later at the tube.
//! 4. **Refresh flicker**: a gain `1 - a (0.5 - 0.5 cos 2 pi phase)` on the whole picture, `phase` the cycles
//!    of `flicker_hz` after `frame / frame_rate_hz` seconds (a 50 Hz tube on a 60 Hz display beats at 10 Hz), 1 when
//!    the amount is 0.
//!
//! Then the result is clamped at 0 (no negative light). Every amount is an estimate: `CRT.md` holds no measured
//! knob law of any set. Neutral values skip each step, so the module is an exact identity until a knob moves
//! (`Tv::is_neutral`; the slot is skipped then).

use std::f64::consts::TAU;

/// BT.601 luma weights (the same as `composite::LUMA`).
pub const KR: f64 = 0.299;
pub const KG: f64 = 0.587;
pub const KB: f64 = 0.114;
/// Scale of the blue and red colour differences to U and V (BT.601).
pub const U_SCALE: f64 = 0.492;
pub const V_SCALE: f64 = 0.877;

/// Settings of the TV controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tv {
    /// Sharpness `s`, -1 (soft) to 1 (peaked); 0 off.
    pub sharpness: f64,
    /// Width of the sharpness low-pass, source pixels.
    pub sharpness_px: f64,
    /// Tint: rotation of the colour differences, degrees (0 off).
    pub tint_deg: f64,
    /// Colour: gain of the colour differences (1 off).
    pub colour: f64,
    /// Voltage of black (0 off).
    pub black: f64,
    /// Voltage of white (1 off).
    pub white: f64,
    /// Depth of the refresh flicker, 0 to 1 (0 off).
    pub flicker_amount: f64,
    /// Rate of the flicker as seen, Hz.
    pub flicker_hz: f64,
    /// Frames per second of the time base.
    pub frame_rate_hz: f64,
}

impl Default for Tv {
    fn default() -> Self {
        Self {
            sharpness: 0.0,
            sharpness_px: 1.0,
            tint_deg: 0.0,
            colour: 1.0,
            black: 0.0,
            white: 1.0,
            flicker_amount: 0.0,
            flicker_hz: 10.0,
            frame_rate_hz: 60.0,
        }
    }
}

/// Half width in samples of the sharpness low-pass: `round(px x k)`, at most [`MAX_RADIUS`].
pub const MAX_RADIUS: i64 = 32;

impl Tv {
    /// Whether every knob is at its neutral value (the module does nothing).
    #[must_use]
    pub fn is_neutral(&self) -> bool {
        !self.has_sharpness()
            && !self.has_chroma()
            && self.black == 0.0
            && self.white == 1.0
            && self.flicker_amount == 0.0
    }

    /// Whether the sharpness step runs.
    #[must_use]
    pub fn has_sharpness(&self) -> bool {
        self.sharpness != 0.0 && self.sharpness_px > 0.0
    }

    /// Whether the tint and colour step runs.
    #[must_use]
    pub fn has_chroma(&self) -> bool {
        self.tint_deg != 0.0 || self.colour != 1.0
    }

    /// Half width of the sharpness low-pass in samples for `k` samples per source pixel.
    #[must_use]
    pub fn radius(&self, k: usize) -> i64 {
        ((self.sharpness_px * k as f64).round() as i64).clamp(0, MAX_RADIUS)
    }

    /// Cycles of the flicker at `frame`, in [0, 1) (computed in f64 so a long run keeps its phase).
    #[must_use]
    pub fn flicker_phase(&self, frame: u32) -> f64 {
        (self.flicker_hz * f64::from(frame) / self.frame_rate_hz.max(1e-9)).rem_euclid(1.0)
    }

    /// The gain of the whole picture at `frame`.
    #[must_use]
    pub fn flicker_gain(&self, frame: u32) -> f64 {
        1.0 - self.flicker_amount * (0.5 - 0.5 * (TAU * self.flicker_phase(frame)).cos())
    }

    /// One line of `row` (amp samples, `k` per source pixel) through the controls at `frame`.
    #[must_use]
    pub fn line(&self, frame: u32, k: usize, row: &[[f64; 3]]) -> Vec<[f64; 3]> {
        let n = row.len() as i64;
        let luma = |p: &[f64; 3]| KR * p[0] + KG * p[1] + KB * p[2];
        let r = self.radius(k);
        let (sin, cos) = (self.tint_deg.to_radians().sin(), self.tint_deg.to_radians().cos());
        let gain = self.flicker_gain(frame);
        (0..n)
            .map(|x| {
                let mut p = row[x as usize];
                if self.has_sharpness() && r > 0 {
                    let mut lp = 0.0;
                    for j in -r..=r {
                        let w = (r + 1 - j.abs()) as f64 / ((r + 1) * (r + 1)) as f64;
                        lp += w * luma(&row[(x + j).clamp(0, n - 1) as usize]);
                    }
                    let delta = self.sharpness * (luma(&p) - lp);
                    p = p.map(|c| c + delta);
                }
                if self.has_chroma() {
                    let y = luma(&p);
                    let (u, v) = (U_SCALE * (p[2] - y), V_SCALE * (p[0] - y));
                    let (u, v) = (self.colour * (u * cos - v * sin), self.colour * (u * sin + v * cos));
                    let (red, blue) = (y + v / V_SCALE, y + u / U_SCALE);
                    p = [red, (y - KR * red - KB * blue) / KG, blue];
                }
                if self.black != 0.0 || self.white != 1.0 {
                    p = p.map(|c| self.black + (self.white - self.black) * c);
                }
                if self.flicker_amount != 0.0 {
                    p = p.map(|c| c * gain);
                }
                p.map(|c| c.max(0.0))
            })
            .collect()
    }

    /// Every line of an image (rows of amp samples).
    #[must_use]
    pub fn frame(&self, rows: &[Vec<[f64; 3]>], frame: u32, k: usize) -> Vec<Vec<[f64; 3]>> {
        rows.iter().map(|row| self.line(frame, k, row)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::close;

    const K: usize = 4;

    fn flat(len: usize, c: [f64; 3]) -> Vec<[f64; 3]> {
        vec![c; len]
    }

    /// Neutral knobs do nothing; every single knob at its neutral value leaves the rest as it was.
    #[test]
    fn neutral_is_the_identity() {
        let tv = Tv::default();
        assert!(tv.is_neutral());
        let row: Vec<[f64; 3]> = (0..16).map(|i| [i as f64 / 16.0, 0.3, 1.0 - i as f64 / 16.0]).collect();
        assert_eq!(tv.line(7, K, &row), row);
        assert!(!Tv { colour: 0.9, ..tv }.is_neutral());
        assert!(!Tv {
            flicker_amount: 0.1,
            ..tv
        }
        .is_neutral());
        // A sharpness with no width is neutral too (nothing to filter).
        assert!(Tv {
            sharpness: 0.5,
            sharpness_px: 0.0,
            ..tv
        }
        .is_neutral());
    }

    /// The levels: V' = black + (white - black) V; black 0.1 and white 0.9 map 0, 0.5, 1 to 0.1, 0.5, 0.9.
    #[test]
    fn black_and_white_level_map_the_endpoints() {
        let tv = Tv {
            black: 0.1,
            white: 0.9,
            ..Tv::default()
        };
        let out = tv.line(0, K, &[[0.0; 3], [0.5; 3], [1.0; 3]]);
        for (o, want) in out.iter().zip([0.1, 0.5, 0.9]) {
            assert!(o.iter().all(|c| close(*c, want, 1e-12)), "{o:?}");
        }
        // A black level below 0 crushes: 0.05 with black -0.1 and white 1 is -0.1 + 1.1 x 0.05 = -0.045, clamped to 0.
        let crush = Tv {
            black: -0.1,
            ..Tv::default()
        };
        assert_eq!(crush.line(0, K, &[[0.05; 3]])[0], [0.0; 3]);
    }

    /// Colour 0 removes the colour: every channel becomes the luma. Colour 1.5 scales the colour differences by 1.5 (blue
    /// stays above 0 here: 0.4968 + 1.5 x (0.2 - 0.4968) = 0.0516). A
    /// grey is untouched by tint and colour. Hand values for (0.8, 0.4, 0.2): Y = 0.2392 + 0.2348 + 0.0228 =
    /// 0.4968.
    #[test]
    fn colour_scales_the_colour_differences_and_keeps_the_luma() {
        let c = [0.8, 0.4, 0.2];
        let y = KR * 0.8 + KG * 0.4 + KB * 0.2;
        assert!(close(y, 0.4968, 1e-12));
        let grey = Tv {
            colour: 0.0,
            ..Tv::default()
        }
        .line(0, K, &[c])[0];
        assert!(grey.iter().all(|v| close(*v, y, 1e-12)), "{grey:?}");
        let twice = Tv {
            colour: 1.5,
            ..Tv::default()
        }
        .line(0, K, &[c])[0];
        assert!(close(twice[0] - y, 1.5 * (0.8 - y), 1e-12) && close(twice[2] - y, 1.5 * (0.2 - y), 1e-12));
        assert!(
            close(KR * twice[0] + KG * twice[1] + KB * twice[2], y, 1e-12),
            "luma kept"
        );
        let g = [0.5; 3];
        let tinted = Tv {
            tint_deg: 40.0,
            colour: 1.5,
            ..Tv::default()
        }
        .line(0, K, &[g])[0];
        assert!(tinted.iter().all(|v| close(*v, 0.5, 1e-12)), "{tinted:?}");
    }

    /// A tint of 90 degrees turns (U, V) to (-V, U): for a pure red difference (R - Y = 0.1, B - Y = 0) U' = -0.877
    /// x 0.1 = -0.0877 so B' - Y = U' / 0.492 = -0.17825 and R' = Y; the turn by 360 degrees is the identity.
    #[test]
    fn tint_turns_the_colour_differences() {
        // Y = 0.5 with R - Y = 0.1, B - Y = 0; G follows from the luma equation.
        let y = 0.5;
        let red = y + 0.1;
        let blue = y;
        let green = (y - KR * red - KB * blue) / KG;
        let out = Tv {
            tint_deg: 90.0,
            ..Tv::default()
        }
        .line(0, K, &[[red, green, blue]])[0];
        assert!(close(KR * out[0] + KG * out[1] + KB * out[2], y, 1e-12));
        assert!(close(out[0], y, 1e-9), "R' = Y: {out:?}");
        assert!(close(out[2] - y, -0.877 * 0.1 / 0.492, 1e-9), "{out:?}");
        let full = Tv {
            tint_deg: 360.0,
            ..Tv::default()
        }
        .line(0, K, &[[red, green, blue]])[0];
        for (a, b) in full.iter().zip([red, green, blue]) {
            assert!(close(*a, b, 1e-9));
        }
    }

    /// Sharpness on a luma step (0 then 1) with a radius of 2 samples (0.5 px at K = 4): the low-pass weights
    /// are 1, 2, 3, 2, 1 over 9. At the first sample after the step LP = (1 + 2 + 3) / 9 = 2/3 for the sample
    /// whose left neighbours are 0: s = 1 gives 1 + (1 - 2/3) = 1.3333 (overshoot), the one before the step (0)
    /// has LP = (1 + 2) / 9 = 1/3: 0 + (0 - 1/3) = -1/3, clamped to 0. s = -1 is the low-pass itself (2/3).
    #[test]
    fn sharpness_overshoots_an_edge_and_softens_with_a_negative_value() {
        let mut row = flat(8, [0.0; 3]);
        row.extend(flat(8, [1.0; 3]));
        let tv = Tv {
            sharpness: 1.0,
            sharpness_px: 0.5,
            ..Tv::default()
        };
        assert_eq!(tv.radius(K), 2);
        let out = tv.line(0, K, &row);
        assert!(close(out[8][0], 1.0 + 1.0 / 3.0, 1e-12), "{}", out[8][0]);
        assert_eq!(out[7][0], 0.0, "undershoot is clamped at black");
        let soft = Tv { sharpness: -1.0, ..tv }.line(0, K, &row);
        // A negative s is the pure low-pass: at index 8 the taps cover samples 6..10: 0, 0, 1, 1, 1 -> 3 +... =
        // (1 x 0 + 2 x 0 + 3 x 1 + 2 x 1 + 1 x 1) / 9 = 6/9.
        assert!(close(soft[8][0], 6.0 / 9.0, 1e-12), "{}", soft[8][0]);
        // The delta is added to the three channels alike: the colour differences are unchanged.
        let colour: Vec<[f64; 3]> = (0..16)
            .map(|i| if i < 8 { [0.2, 0.1, 0.3] } else { [0.6, 0.5, 0.7] })
            .collect();
        let c = tv.line(0, K, &colour);
        for (o, i) in c.iter().zip(&colour) {
            if o.iter().all(|v| *v > 0.0) {
                assert!(close(o[0] - o[1], i[0] - i[1], 1e-12) && close(o[2] - o[1], i[2] - i[1], 1e-12));
            }
        }
    }

    /// The flicker: 10 Hz at 60 frames per second is 6 frames per cycle; the gain is 1 at frame 0, the trough
    /// 1 - a at frame 3, and returns to 1 at frame 6; the phase of a frame far past f32 precision keeps its value.
    #[test]
    fn flicker_beats_at_the_chosen_rate() {
        let tv = Tv {
            flicker_amount: 0.3,
            flicker_hz: 10.0,
            ..Tv::default()
        };
        assert!(close(tv.flicker_gain(0), 1.0, 1e-12));
        assert!(close(tv.flicker_gain(3), 0.7, 1e-12));
        assert!(close(tv.flicker_gain(6), 1.0, 1e-12));
        assert!(close(
            tv.flicker_gain(1),
            1.0 - 0.3 * (0.5 - 0.5 * (TAU / 6.0).cos()),
            1e-12
        ));
        let far = 3_000_000_000_u32;
        assert!(close(
            tv.flicker_phase(far),
            (10.0 * f64::from(far) / 60.0).rem_euclid(1.0),
            1e-12
        ));
        let out = tv.line(3, K, &[[1.0, 0.5, 0.2]])[0];
        assert!(close(out[0], 0.7, 1e-12) && close(out[1], 0.35, 1e-12));
    }
}

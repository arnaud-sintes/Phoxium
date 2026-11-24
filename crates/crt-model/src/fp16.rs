//! IEEE binary16 (FP16) rounding in f64, for the accuracy study of an FP16 phosphor history (`QUESTIONS.md`,
//! PHY-12, note of 2026-11-24; TODO Low, "`phosphor` history, the second step").
//!
//! A history `h <- rho h + (1 - rho) e` stored in FP16 and rounded to nearest does two bad things: the rounding is
//! biased (a value that decays by less than half a spacing per frame does not move at all), and the black tail
//! stalls at a residue instead of reaching zero. Rounding *stochastically* (up with the probability of the
//! remainder, the draw coming from a hash of the pixel and the frame) keeps the expected value exact.
//!
//! Everything here is plain f64 arithmetic on exact binary values, so the results are reproducible and the GPU
//! version can be checked against them. The range is the binary16 one: normal numbers from 2^-14 to 65504, the
//! subnormal spacing 2^-24; a value past 65504 saturates there (no infinity is ever produced).

use crate::noise;

/// The largest finite binary16 value.
pub const F16_MAX: f64 = 65504.0;

/// The seed of the draws of [`stochastic_draw`].
pub const DRAW_SEED: u32 = 0xF161_0001;

/// The spacing of binary16 numbers at magnitude `a` (`a` finite, at least 0): 2^-24 below 2^-14, else 2^(e - 10).
fn spacing(a: f64) -> f64 {
    if a < 2f64.powi(-14) {
        2f64.powi(-24)
    } else {
        let e = ((a.to_bits() >> 52) & 0x7ff) as i32 - 1023;
        2f64.powi(e - 10)
    }
}

/// The two binary16 numbers around the magnitude `a` and the spacing used: `(lo, hi, s)` with `lo <= a <= hi`,
/// `hi = lo + s`, both clamped to [`F16_MAX`] (so past the maximum both are the maximum and `s` is 0).
fn around(a: f64) -> (f64, f64, f64) {
    if a >= F16_MAX {
        return (F16_MAX, F16_MAX, 0.0);
    }
    let s = spacing(a);
    let lo = (a / s).floor() * s;
    let hi = (lo + s).min(F16_MAX);
    (lo, hi, s)
}

/// # Panics
/// On a NaN or an infinity (the oracle's policy, see the crate documentation).
fn finite(x: f64) {
    assert!(x.is_finite(), "FP16 rounding of a non-finite value: {x}");
}

/// `x` rounded to the nearest binary16 number, ties to the even mantissa (what a GPU store does), saturating at
/// [`F16_MAX`].
///
/// # Panics
/// On a NaN or an infinity.
#[must_use]
pub fn round_nearest(x: f64) -> f64 {
    finite(x);
    let a = x.abs();
    let (lo, hi, s) = around(a);
    let r = if hi == lo {
        lo
    } else {
        let (dl, dh) = (a - lo, hi - a);
        if dl < dh {
            lo
        } else if dh < dl {
            hi
        } else if ((lo / s) as u64).is_multiple_of(2) {
            lo
        } else {
            hi
        }
    };
    r.copysign(x)
}

/// `x` rounded to one of the two binary16 numbers around it: the upper one with the probability
/// `(a - lo) / (hi - lo)` when `u` (a uniform draw in [0, 1)) is below it. The expected result is `x` (up to the
/// saturation at [`F16_MAX`]). A negative `x` is rounded as its magnitude.
///
/// # Panics
/// On a NaN or an infinity, or on a `u` outside [0, 1).
#[must_use]
pub fn round_stochastic(x: f64, u: f64) -> f64 {
    finite(x);
    assert!((0.0..1.0).contains(&u), "draw outside [0, 1): {u}");
    let a = x.abs();
    let (lo, hi, _) = around(a);
    let r = if hi > lo && u < (a - lo) / (hi - lo) { hi } else { lo };
    r.copysign(x)
}

/// The draw of a pixel at a frame, in [0, 1): the hash that a shader would compute from the pixel index and the frame
/// counter (`noise::cell`).
#[must_use]
pub fn stochastic_draw(pixel: u32, frame: u32) -> f64 {
    noise::unit(noise::cell(DRAW_SEED, pixel, 0, frame))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One step of the history of an exponential decay with a constant drive, stored with `store`.
    fn run(
        rho: f64,
        drive: f64,
        static_n: u32,
        black_n: u32,
        pixels: u32,
        store: &dyn Fn(f64, u32, u32) -> f64,
    ) -> (f64, f64) {
        let mut h = vec![0.0f64; pixels as usize];
        let mut reference = 0.0;
        for n in 0..static_n + black_n {
            let e = if n < static_n { drive } else { 0.0 };
            reference = rho * reference + (1.0 - rho) * e;
            for (p, v) in h.iter_mut().enumerate() {
                *v = store(rho * *v + (1.0 - rho) * e, p as u32, n);
            }
        }
        (h.iter().sum::<f64>() / f64::from(pixels), reference)
    }

    fn nearest(x: f64, _p: u32, _n: u32) -> f64 {
        round_nearest(x)
    }

    fn stochastic(x: f64, p: u32, n: u32) -> f64 {
        round_stochastic(x, stochastic_draw(p, n))
    }

    #[test]
    fn known_binary16_values_are_exact() {
        for v in [
            0.0,
            1.0,
            0.5,
            2f64.powi(-14),
            2f64.powi(-24),
            65504.0,
            1.0 + 2f64.powi(-10),
            3.0 * 2f64.powi(-24),
        ] {
            assert_eq!(round_nearest(v), v, "{v}");
            assert_eq!(round_stochastic(v, 0.999_999), v, "{v}");
        }
        assert_eq!(round_nearest(-1.5), -1.5);
    }

    #[test]
    fn nearest_matches_the_spacing_and_ties_go_to_even() {
        // Around 1: the spacing is 2^-10. A quarter of it rounds down, three quarters up.
        let s = 2f64.powi(-10);
        assert_eq!(round_nearest(1.0 + 0.25 * s), 1.0);
        assert_eq!(round_nearest(1.0 + 0.75 * s), 1.0 + s);
        // The tie between 1 (even mantissa) and 1 + s goes to 1; between 1 + s (odd) and 1 + 2 s (even), to 1 + 2 s.
        assert_eq!(round_nearest(1.0 + 0.5 * s), 1.0);
        assert_eq!(round_nearest(1.0 + 1.5 * s), 1.0 + 2.0 * s);
        // Subnormals: the spacing is 2^-24 and the tie between 0 and 2^-24 goes to 0.
        assert_eq!(round_nearest(2f64.powi(-25)), 0.0);
        assert_eq!(round_nearest(0.75 * 2f64.powi(-24)), 2f64.powi(-24));
        // Saturation, with the sign.
        assert_eq!(round_nearest(1.0e9), F16_MAX);
        assert_eq!(round_nearest(-1.0e9), -F16_MAX);
        assert_eq!(round_nearest(-1.0 - 0.75 * s), -(1.0 + s));
    }

    #[test]
    fn stochastic_goes_up_with_the_probability_of_the_remainder() {
        let s = 2f64.powi(-10);
        let x = 1.0 + 0.3 * s;
        assert_eq!(round_stochastic(x, 0.29), 1.0 + s);
        assert_eq!(round_stochastic(x, 0.31), 1.0);
        // Over many pixels the mean is the value (unbiased), to a few parts in 1e4 of the spacing.
        let n = 20_000u32;
        let mean: f64 = (0..n).map(|p| round_stochastic(x, stochastic_draw(p, 7))).sum::<f64>() / f64::from(n);
        assert!((mean - x).abs() < 0.02 * s, "{mean} vs {x}");
    }

    #[test]
    fn the_draws_are_uniform_and_depend_on_the_frame() {
        let n = 20_000u32;
        let mean: f64 = (0..n).map(|p| stochastic_draw(p, 3)).sum::<f64>() / f64::from(n);
        assert!((mean - 0.5).abs() < 0.01, "{mean}");
        assert_ne!(stochastic_draw(5, 1), stochastic_draw(5, 2));
        assert_ne!(stochastic_draw(5, 1), stochastic_draw(6, 1));
        assert!((0..1000u32).all(|p| (0.0..1.0).contains(&stochastic_draw(p, 9))));
    }

    /// PHY-12 study, steady state: rounding to nearest is biased on a slow decay, stochastic rounding is not.
    #[test]
    fn a_long_static_drive_is_biased_by_nearest_and_not_by_stochastic() {
        // rho 0.99, drive 1 (the f64 history reaches 0.9976 after 600 frames): mean errors 2.2e-2 and 1.4e-5 in
        // the study over 20000 pixels.
        let (rn, reference) = run(0.99, 1.0, 600, 0, 2_000, &nearest);
        let (sr, _) = run(0.99, 1.0, 600, 0, 2_000, &stochastic);
        assert!((rn - reference).abs() / reference > 5.0e-3, "RN {rn} vs {reference}");
        assert!((sr - reference).abs() / reference < 2.0e-4, "SR {sr} vs {reference}");
        // A dim drive: 0.01 at rho 0.9.
        let (rn, reference) = run(0.9, 0.01, 600, 0, 2_000, &nearest);
        let (sr, _) = run(0.9, 0.01, 600, 0, 2_000, &stochastic);
        assert!((rn - reference).abs() / reference > 1.0e-3, "RN {rn} vs {reference}");
        assert!((sr - reference).abs() / reference < 5.0e-4, "SR {sr} vs {reference}");
    }

    /// PHY-12 study, the black tail: a static picture then black. Rounding to nearest stalls at a residue (about
    /// 1e-7, a faint afterglow that never ends) where the exact history is long gone; stochastic rounding reaches
    /// zero, and on a slow decay follows the exact history.
    #[test]
    fn the_black_tail_stalls_with_nearest_and_follows_with_stochastic() {
        let (rn, exact) = run(0.9, 1.0, 600, 400, 2_000, &nearest);
        let (sr, _) = run(0.9, 1.0, 600, 400, 2_000, &stochastic);
        assert!(exact < 1.0e-15);
        assert!(rn > 1.0e-7, "round to nearest should stall: {rn}");
        assert!(sr < 1.0e-8, "stochastic should reach zero: {sr}");
        // A slow decay (rho 0.99): 400 black frames leave 1.79e-2 of the drive; both stay close, stochastic closer.
        let (rn, exact) = run(0.99, 1.0, 600, 400, 2_000, &nearest);
        let (sr, _) = run(0.99, 1.0, 600, 400, 2_000, &stochastic);
        assert!((sr - exact).abs() / exact < 2.0e-3, "SR {sr} vs {exact}");
        assert!((rn - exact).abs() > (sr - exact).abs(), "RN {rn} SR {sr} exact {exact}");
    }

    /// The spread of one pixel under stochastic rounding stays near one 8-bit output level at rho 0.99 (the study's
    /// worst pixel was 3.95e-3 of the value), so the noise is at the edge of visibility there and invisible below.
    #[test]
    fn the_per_pixel_spread_is_about_one_output_level() {
        let pixels = 2_000u32;
        let mut h = vec![0.0f64; pixels as usize];
        let rho = 0.99;
        let mut reference = 0.0;
        for n in 0..600u32 {
            reference = rho * reference + (1.0 - rho);
            for (p, v) in h.iter_mut().enumerate() {
                *v = stochastic(rho * *v + (1.0 - rho), p as u32, n);
            }
        }
        let worst = h.iter().map(|v| (v - reference).abs() / reference).fold(0.0, f64::max);
        assert!(worst < 8.0e-3, "worst relative error {worst}");
    }

    #[test]
    #[should_panic(expected = "non-finite")]
    fn a_nan_is_refused() {
        let _ = round_nearest(f64::NAN);
    }
}

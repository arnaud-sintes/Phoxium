//! Electron beam: Gaussian scanlines, spot growth with current, video amplifier.

use std::f64::consts::PI;

/// Largest sigma/P for which the 81 summed lines (k = -40..40) are exact to f64.
pub const SCANLINE_SIGMA_MAX: f64 = 5.0;

/// Sum of unit-peak Gaussian scanlines of sigma `sigma_over_p` (in line pitches),
/// evaluated at `y` (in line pitches, 0 = centre of a line). The profile is
/// periodic, so `y` is first reduced to [-0.5, 0.5).
///
/// # Panics
/// Unless 0 < `sigma_over_p` <= [`SCANLINE_SIGMA_MAX`] and `y` is finite.
pub fn scanline_sum(sigma_over_p: f64, y: f64) -> f64 {
    assert!(
        sigma_over_p > 0.0 && sigma_over_p <= SCANLINE_SIGMA_MAX,
        "sigma/P = {sigma_over_p} outside (0, {SCANLINE_SIGMA_MAX}]"
    );
    assert!(y.is_finite(), "y = {y}");
    let y = (y + 0.5).rem_euclid(1.0) - 0.5;
    (-40..=40)
        .map(|k| {
            let d = y - f64::from(k);
            (-d * d / (2.0 * sigma_over_p * sigma_over_p)).exp()
        })
        .sum()
}

/// Valley over peak of the summed scanline profile (flat field). [D]
pub fn scanline_valley_over_peak(sigma_over_p: f64) -> f64 {
    scanline_sum(sigma_over_p, 0.5) / scanline_sum(sigma_over_p, 0.0)
}

/// Michelson contrast (peak - valley) / (peak + valley) of the summed profile. [D]
pub fn scanline_michelson(sigma_over_p: f64) -> f64 {
    let peak = scanline_sum(sigma_over_p, 0.0);
    let valley = scanline_sum(sigma_over_p, 0.5);
    (peak - valley) / (peak + valley)
}

/// Small-ripple approximation of the Michelson modulation of the summed
/// profile (first Fourier harmonic): 2 exp(-2 pi^2 sigma^2 / P^2). [D]
/// Only valid for sigma/P >= 0.3; it exceeds 1 at sigma/P = 0.15.
pub fn small_ripple_modulation(sigma_over_p: f64) -> f64 {
    2.0 * (-2.0 * PI * PI * sigma_over_p * sigma_over_p).exp()
}

/// Fraction of a unit-area Gaussian of sigma `sigma`, centred at 0, that lies between `a` and `b`
/// (any length unit, a <= b): (erf(b / (sigma sqrt 2)) - erf(a / (sigma sqrt 2))) / 2. [K]
///
/// # Panics
/// Unless `sigma` > 0 and a <= b (all finite).
pub fn gaussian_fraction(a: f64, b: f64, sigma: f64) -> f64 {
    assert!(
        sigma.is_finite() && sigma > 0.0 && a.is_finite() && b.is_finite() && a <= b,
        "gaussian_fraction: sigma = {sigma}, [{a}, {b}]"
    );
    let k = 1.0 / (sigma * 2.0_f64.sqrt());
    0.5 * (erf(b * k) - erf(a * k))
}

/// Vertical profile of one scanline (T-36a, CRT.md "Spot profile"): a unit-area Gaussian core of
/// sigma `sigma` plus a wider unit-area Gaussian tail of sigma `tail_ratio * sigma` and weight
/// `tail_weight` (0 = pure Gaussian), mixed so the total stays one. [K][E]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScanlineProfile {
    /// Core sigma (same length unit as the positions it is used with).
    pub sigma: f64,
    /// Fraction of the energy in the tail, 0 to 1.
    pub tail_weight: f64,
    /// Tail sigma over core sigma.
    pub tail_ratio: f64,
}

impl ScanlineProfile {
    /// A pure Gaussian of sigma `sigma`.
    pub fn gaussian(sigma: f64) -> Self {
        Self {
            sigma,
            tail_weight: 0.0,
            tail_ratio: 1.0,
        }
    }

    /// Fraction of the line's energy between `a` and `b` (a <= b), measured from the line centre.
    ///
    /// # Panics
    /// As [`gaussian_fraction`], or unless 0 <= `tail_weight` <= 1 and `tail_ratio` > 0.
    pub fn fraction(&self, a: f64, b: f64) -> f64 {
        assert!(
            (0.0..=1.0).contains(&self.tail_weight) && self.tail_ratio > 0.0,
            "scanline profile: {self:?}"
        );
        let core = gaussian_fraction(a, b, self.sigma);
        if self.tail_weight == 0.0 {
            return core;
        }
        let tail = gaussian_fraction(a, b, self.sigma * self.tail_ratio);
        (1.0 - self.tail_weight) * core + self.tail_weight * tail
    }
}

/// Area-mean light over the host row [`y0`, `y1`) of a finite scanline raster (T-36a): line n
/// (0-based) is centred at `first + n * pitch` and carries the beam current `currents[n]`. Each
/// line deposits its current times `pitch` per unit length (one line pitch of screen per line),
/// spread vertically by `profile`; every line is summed (no truncation). A flat field of current I
/// therefore has a mean light of I over a line period, whatever the spot width. Lengths in one
/// unit (host pixels in the GPU tests). [D]
///
/// # Panics
/// Unless `pitch` > 0 and `y0` < `y1`, or as [`ScanlineProfile::fraction`].
pub fn scanline_row_mean(currents: &[f64], first: f64, pitch: f64, profile: &ScanlineProfile, y0: f64, y1: f64) -> f64 {
    assert!(pitch > 0.0 && y0 < y1, "scanline_row_mean: pitch {pitch}, [{y0}, {y1})");
    let sum: f64 = currents
        .iter()
        .enumerate()
        .filter(|(_, &i)| i != 0.0)
        .map(|(n, &i)| {
            let c = first + n as f64 * pitch;
            i * profile.fraction(y0 - c, y1 - c)
        })
        .sum();
    sum * pitch / (y1 - y0)
}

/// Spot FWHM against beam current: d^2 = d0^2 + k I^(2n). [E]
/// `d0_mm` is the floor set by the gun optics, n between 0.3 and 0.5.
pub fn spot_fwhm_mm(current_ua: f64, d0_mm: f64, k: f64, n: f64) -> f64 {
    (d0_mm * d0_mm + k * current_ua.max(0.0).powf(2.0 * n)).sqrt()
}

/// Least-squares fit of a pure power law d = c I^m in log-log space.
/// Returns (c, m).
///
/// # Panics
/// With fewer than two points, a non-positive or non-finite value, or all
/// currents equal.
pub fn fit_power_law(points: &[(f64, f64)]) -> (f64, f64) {
    assert!(points.len() >= 2, "power-law fit needs at least two points");
    assert!(
        points
            .iter()
            .all(|&(i, d)| i.is_finite() && d.is_finite() && i > 0.0 && d > 0.0),
        "power-law fit needs finite positive values: {points:?}"
    );
    let n = points.len() as f64;
    let (sx, sy) = points
        .iter()
        .fold((0.0, 0.0), |(sx, sy), (i, d)| (sx + i.ln(), sy + d.ln()));
    let (mx, my) = (sx / n, sy / n);
    let (sxy, sxx) = points.iter().fold((0.0, 0.0), |(sxy, sxx), (i, d)| {
        let dx = i.ln() - mx;
        (sxy + dx * (d.ln() - my), sxx + dx * dx)
    });
    assert!(sxx > 0.0, "power-law fit needs distinct currents");
    let m = sxy / sxx;
    ((my - m * mx).exp(), m)
}

/// Exact fit of the floor model d^2 = d0^2 + k I^(2n) through three points
/// whose currents are in geometric progression (I2 / I1 = I3 / I2).
/// Returns (d0^2, k, n). d0^2 is returned as is, even when negative: a
/// negative value means the points do not support a positive floor.
///
/// # Panics
/// Unless 0 < I1 < I2 < I3 in geometric progression and 0 < d1 < d2 < d3
/// (all finite).
pub fn fit_spot_growth_floor(points: [(f64, f64); 3]) -> (f64, f64, f64) {
    let [(i1, d1), (i2, d2), (i3, d3)] = points;
    assert!(
        points.iter().all(|&(i, d)| i.is_finite() && d.is_finite()) && 0.0 < i1 && i1 < i2 && i2 < i3,
        "currents must be finite, positive and increasing: {points:?}"
    );
    let r = i2 / i1;
    assert!(
        (i3 / i2 - r).abs() < 1e-9 * r,
        "currents not in geometric progression: {points:?}"
    );
    assert!(
        0.0 < d1 && d1 < d2 && d2 < d3,
        "widths must be positive and increasing: {points:?}"
    );
    let (q1, q2, q3) = (d1 * d1, d2 * d2, d3 * d3);
    let two_n = ((q3 - q2) / (q2 - q1)).ln() / r.ln();
    let k = (q2 - q1) / (i1.powf(two_n) * (r.powf(two_n) - 1.0));
    (q1 - k * i1.powf(two_n), k, two_n / 2.0)
}

/// Exponent of spot width against video level above the floor, with the gun
/// law I ~ V^gamma and d ~ I^n: d ~ V^(gamma n). [D]
pub fn width_vs_video_exponent(gamma: f64, n: f64) -> f64 {
    gamma * n
}

fn check_bandwidth(bandwidth_mhz: f64) {
    assert!(
        bandwidth_mhz.is_finite() && bandwidth_mhz > 0.0,
        "bandwidth = {bandwidth_mhz} MHz, must be finite and positive"
    );
}

/// Error function, f64 accuracy. Series erf(x) = 2/sqrt(pi) exp(-x^2)
/// sum 2^k x^(2k+1) / (1 3 5 ... (2k+1)), all terms positive.
pub fn erf(x: f64) -> f64 {
    if x < 0.0 {
        return -erf(-x);
    }
    if x > 6.0 {
        return 1.0;
    }
    let (mut term, mut sum) = (x, x);
    for k in 1..1000 {
        term *= 2.0 * x * x / (2.0 * f64::from(k) + 1.0);
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    2.0 / PI.sqrt() * (-x * x).exp() * sum
}

/// Time sigma of a Gaussian low-pass whose -3 dB bandwidth is `bandwidth_mhz`:
/// sqrt(ln 2) / (2 pi B), about 0.1325 / B. [D]
///
/// # Panics
/// Unless `bandwidth_mhz` is finite and positive (same contract for
/// [`gaussian_amp_step`], [`gaussian_amp_rise_ns`], [`single_pole_rise_ns`]).
pub fn gaussian_amp_sigma_ns(bandwidth_mhz: f64) -> f64 {
    check_bandwidth(bandwidth_mhz);
    1000.0 * 2.0_f64.ln().sqrt() / (2.0 * PI * bandwidth_mhz)
}

/// Step response of the Gaussian amplifier at time `t_ns` after the step
/// (0.5 at t = 0).
pub fn gaussian_amp_step(t_ns: f64, bandwidth_mhz: f64) -> f64 {
    let s = gaussian_amp_sigma_ns(bandwidth_mhz);
    0.5 * (1.0 + erf(t_ns / (s * 2.0_f64.sqrt())))
}

/// Tail integral of the Gaussian step, T(z) = integral over [z, inf) of (1 - Phi(t)) dt =
/// phi(z) - z (1 - Phi(z)), for z >= 0 (T(0) = 1 / sqrt(2 pi), T decreases to 0). [D]
fn gaussian_step_tail(z: f64) -> f64 {
    let phi = (-0.5 * z * z).exp() / (2.0 * PI).sqrt();
    let q = 0.5 * (1.0 - erf(z / 2.0_f64.sqrt()));
    (phi - z * q).max(0.0)
}

/// Mean over [`a`, `b`) (a < b, any length unit) of the Gaussian step response Phi(u / `sigma`): the
/// box integral of a smoothed edge (T-33 sub-sample integral). With J(z) = z Phi(z) + phi(z) the
/// antiderivative is sigma J(u / sigma), and J(z) = max(z, 0) + T(|z|) with the tail integral T, so
/// the mean is (max(b, 0) - max(a, 0) + sigma (T(|b| / sigma) - T(|a| / sigma))) / (b - a), a form
/// without cancellation on either side of the edge. [D]
///
/// # Panics
/// Unless `sigma` > 0 and a < b (all finite).
pub fn gaussian_step_box_mean(a: f64, b: f64, sigma: f64) -> f64 {
    assert!(
        sigma.is_finite() && sigma > 0.0 && a.is_finite() && b.is_finite() && a < b,
        "gaussian_step_box_mean: sigma = {sigma}, [{a}, {b})"
    );
    let ramp = b.max(0.0) - a.max(0.0);
    (ramp + sigma * (gaussian_step_tail(b.abs() / sigma) - gaussian_step_tail(a.abs() / sigma))) / (b - a)
}

/// 10-90 % rise time of the Gaussian amplifier, found numerically on the step
/// response (bisection), so it checks the closed form 0.3396 / B. [D]
pub fn gaussian_amp_rise_ns(bandwidth_mhz: f64) -> f64 {
    let crossing = |level: f64| {
        let s = gaussian_amp_sigma_ns(bandwidth_mhz);
        let (mut lo, mut hi) = (-10.0 * s, 10.0 * s);
        for _ in 0..200 {
            let mid = 0.5 * (lo + hi);
            if gaussian_amp_step(mid, bandwidth_mhz) < level {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        0.5 * (lo + hi)
    };
    crossing(0.9) - crossing(0.1)
}

/// 10-90 % rise time of a single-pole low-pass: ln 9 / (2 pi B), about 0.35 / B. [K]
pub fn single_pole_rise_ns(bandwidth_mhz: f64) -> f64 {
    check_bandwidth(bandwidth_mhz);
    1000.0 * 9.0_f64.ln() / (2.0 * PI * bandwidth_mhz)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::close;
    use crate::units::{fwhm_from_sigma, sigma_from_fwhm};

    /// CRT.md scanline table: sigma/P, FWHM/P, valley/peak, Michelson.
    #[test]
    fn scanline_table_from_crt_md() {
        let rows = [
            (0.15, 0.35, 0.008, 0.98),
            (0.20, 0.47, 0.09, 0.84),
            (0.25, 0.59, 0.27, 0.57),
            (0.30, 0.71, 0.495, 0.34),
            (0.35, 0.82, 0.70, 0.18),
            (0.40, 0.94, 0.84, 0.085),
            (0.45, 1.06, 0.93, 0.035),
            (0.50, 1.18, 0.97, 0.014),
        ];
        for (s, fwhm, vp, mich) in rows {
            let got = scanline_valley_over_peak(s);
            assert!(close(got, vp, 0.005), "sigma/P {s}: valley/peak {got} vs {vp}");
            let m = scanline_michelson(s);
            assert!(close(m, mich, 0.005), "sigma/P {s}: Michelson {m} vs {mich}");
            assert!(close(fwhm_from_sigma(s), fwhm, 0.005), "sigma/P {s}: FWHM/P");
        }
        // 0.60: valley/peak 0.997, Michelson 0.00164 (small-ripple form
        // 2 exp(-2 pi^2 0.36) = 2 exp(-7.106) = 0.00164, exact to 1e-9 there).
        assert!(close(scanline_valley_over_peak(0.6), 0.997, 0.0005));
        let m = scanline_michelson(0.6);
        assert!(close(m, 0.00164, 0.000_005), "Michelson at 0.60: {m}");
    }

    /// The profile is periodic and even: y, y + 37 and -y give the same sum.
    #[test]
    fn scanline_periodic() {
        let base = scanline_sum(0.3, 0.2);
        for y in [37.2, -0.2, -36.8, 1.0e6 + 0.2] {
            assert!(close(scanline_sum(0.3, y), base, 1e-9), "y {y}");
        }
        // Peak of a single line at sigma 0.2 plus its two neighbours at 1 P:
        // 1 + 2 exp(-1 / 0.08) = 1 + 2 exp(-12.5).
        assert!(close(scanline_sum(0.2, 0.0), 1.0 + 2.0 * (-12.5_f64).exp(), 1e-15));
    }

    #[test]
    #[should_panic(expected = "sigma/P")]
    fn scanline_zero_sigma_panics() {
        scanline_sum(0.0, 0.5);
    }

    #[test]
    #[should_panic(expected = "y = ")]
    fn scanline_infinite_y_panics() {
        scanline_sum(0.3, f64::INFINITY);
    }

    /// Normal-distribution masses: +/- 1 sigma 0.682689, +/- 2 sigma 0.954500, half 0.5. [K]
    #[test]
    fn gaussian_fraction_known_masses() {
        assert!(close(gaussian_fraction(-1.0, 1.0, 1.0), 0.682_689_492, 1e-9));
        assert!(close(gaussian_fraction(-4.0, 4.0, 2.0), 0.954_499_736, 1e-9));
        assert!(close(gaussian_fraction(0.0, 100.0, 3.0), 0.5, 1e-15));
        assert_eq!(gaussian_fraction(0.5, 0.5, 1.0), 0.0);
    }

    #[test]
    #[should_panic(expected = "gaussian_fraction")]
    fn gaussian_fraction_reversed_interval_panics() {
        gaussian_fraction(1.0, 0.0, 1.0);
    }

    /// The tail moves energy outwards but keeps the total: the whole line is 1 for every weight, and
    /// the +/- 1 sigma mass is (1 - w) 0.682689 + w erf(1 / (r sqrt 2)) (r = 3: erf(0.2357) = 0.261117).
    #[test]
    fn tail_conserves_energy() {
        for w in [0.0, 0.02, 0.1, 0.5] {
            let p = ScanlineProfile {
                sigma: 2.0,
                tail_weight: w,
                tail_ratio: 3.0,
            };
            assert!(close(p.fraction(-200.0, 200.0), 1.0, 1e-12), "w {w}");
            let want = (1.0 - w) * 0.682_689_492 + w * 0.261_117_319;
            assert!(close(p.fraction(-2.0, 2.0), want, 1e-8), "w {w}");
        }
    }

    /// Flat field of unit current, 6 host pixels per line, line n centred at (n + 0.5) 6: the six pixel
    /// means of one period, derived independently (pixel [a, b): 6 sum_k (Phi((b - c_k) / sigma) -
    /// Phi((a - c_k) / sigma)), series erf, recorded in the T-36a report). sigma/P = 0.25 (CRT.md 240p
    /// example) and 0.158150 (PVM-20M4U: FWHM 0.45 mm / 2.35482 / (290 / 240) mm). Each period
    /// averages to 1: the flat-field mean does not depend on sigma.
    #[test]
    fn flat_field_pixel_means_at_6_px_per_line() {
        let ones = vec![1.0; 41];
        for (s, half) in [
            (0.25, [0.524_285, 0.988_105, 1.487_610]),
            (
                sigma_from_fwhm(0.45) / (290.0 / 240.0),
                [0.105_094, 0.770_754, 2.124_152],
            ),
        ] {
            let p = ScanlineProfile::gaussian(s * 6.0);
            let px: Vec<f64> = (120..126)
                .map(|j| scanline_row_mean(&ones, 3.0, 6.0, &p, f64::from(j), f64::from(j + 1)))
                .collect();
            let want = [half[0], half[1], half[2], half[2], half[1], half[0]];
            for (j, (g, w)) in px.iter().zip(want).enumerate() {
                assert!(close(*g, w, 2e-6), "sigma/P {s}: pixel {j} {g} vs {w}");
            }
            assert!(
                close(px.iter().sum::<f64>() / 6.0, 1.0, 1e-9),
                "sigma/P {s}: period mean"
            );
        }
    }

    /// A vanishing pixel at a line centre gives the point profile: P / (sigma sqrt(2 pi)) times the
    /// unit-peak sum of the CRT.md table oracle ([`scanline_sum`]).
    #[test]
    fn row_mean_tends_to_the_point_profile() {
        let ones = vec![1.0; 81];
        for s in [0.15, 0.3, 0.5] {
            let p = ScanlineProfile::gaussian(s * 10.0);
            let c = 40.5 * 10.0;
            for (y, at) in [(c, 0.0), (c + 5.0, 0.5)] {
                let got = scanline_row_mean(&ones, 5.0, 10.0, &p, y - 1e-4, y + 1e-4);
                let want = 1.0 / (s * (2.0 * PI).sqrt()) * scanline_sum(s, at);
                assert!(close(got, want, 1e-6 * want), "sigma/P {s}, y {at}: {got} vs {want}");
            }
        }
    }

    /// Area-integrated Michelson at 32 px per line (line centres on pixel boundaries), derived
    /// independently like the 6 px case: 0.98341, 0.83466, 0.57072, 0.33574, 0.17703, 0.08445 for
    /// sigma/P 0.15 to 0.40 and 0.01429 at 0.50, each within 0.6 points of the point-sampled CRT.md
    /// table (98, 84, 57, 34, 18, 8.5, 1.4).
    #[test]
    fn area_michelson_at_32_px_per_line() {
        let ones = vec![1.0; 41];
        for (s, want) in [
            (0.15, 0.98341),
            (0.20, 0.83466),
            (0.25, 0.57072),
            (0.30, 0.33574),
            (0.35, 0.17703),
            (0.40, 0.08445),
            (0.50, 0.01429),
        ] {
            let p = ScanlineProfile::gaussian(s * 32.0);
            let px: Vec<f64> = (640..672)
                .map(|j| scanline_row_mean(&ones, 16.0, 32.0, &p, f64::from(j), f64::from(j + 1)))
                .collect();
            let (max, min) = px.iter().fold((f64::MIN, f64::MAX), |(a, b), &v| (a.max(v), b.min(v)));
            let m = (max - min) / (max + min);
            assert!(close(m, want, 1e-5), "sigma/P {s}: {m} vs {want}");
            assert!(close(m, scanline_michelson(s), 0.006), "sigma/P {s}: vs point table");
        }
    }

    /// Linear in the currents (superposition) and a dark raster is dark.
    #[test]
    fn row_mean_is_linear_in_current() {
        let p = ScanlineProfile {
            sigma: 3.0,
            tail_weight: 0.05,
            tail_ratio: 3.0,
        };
        let a = [0.2, 1.0, 0.0, 0.7, 0.3];
        let b = [0.5, 0.0, 0.9, 0.1, 0.4];
        let ab: Vec<f64> = a.iter().zip(&b).map(|(x, y)| 2.0 * x + 3.0 * y).collect();
        for j in 0..40 {
            let (y0, y1) = (f64::from(j), f64::from(j + 1));
            let f = |c: &[f64]| scanline_row_mean(c, 4.0, 8.0, &p, y0, y1);
            assert!(close(f(&ab), 2.0 * f(&a) + 3.0 * f(&b), 1e-12), "row {j}");
            assert_eq!(f(&[0.0; 5]), 0.0);
        }
    }

    #[test]
    fn michelson_consistent_with_valley_over_peak() {
        let r = scanline_valley_over_peak(0.3);
        assert!(close(scanline_michelson(0.3), (1.0 - r) / (1.0 + r), 1e-12));
    }

    #[test]
    fn small_ripple_validity() {
        assert!(close(small_ripple_modulation(0.15), 1.28, 0.01));
        for s in [0.3, 0.4, 0.5] {
            let (a, e) = (small_ripple_modulation(s), scanline_michelson(s));
            assert!(close(a, e, 0.01), "sigma/P {s}: {a} vs exact {e}");
        }
    }

    /// Scanlines vanish when FWHM >= P: sigma/P = 1 / 2.355 = 0.425 ("about 0.45").
    /// 480i at sigma/P 0.5 is the same spot as 240p at 0.25 (57 % modulation).
    #[test]
    fn scanline_rules() {
        assert!(close(sigma_from_fwhm(1.0), 0.4247, 0.0001));
        assert!(close(scanline_michelson(0.5 / 2.0), 0.57, 0.005));
        assert!(scanline_michelson(0.5) < 0.02);
    }

    /// Datasheet points 0.4, 0.5, 0.6 mm at 100, 200, 400 uA: exponent about 0.29.
    #[test]
    fn spot_growth_power_fit() {
        let pts = [(100.0, 0.4), (200.0, 0.5), (400.0, 0.6)];
        let (c, m) = fit_power_law(&pts);
        assert!(close(m, 0.29, 0.005), "exponent {m}");
        assert!(close(c * 200.0_f64.powf(m), 0.49, 0.01));
        // Projection-gun rule: d ~ sqrt(I), +41 % from 1 to 2 mA ("about +40 %").
        let (_, m) = fit_power_law(&[(1000.0, 1.0), (2000.0, 2.0_f64.sqrt())]);
        assert!(close(m, 0.5, 1e-12));
    }

    /// The floor model fitted exactly through the same three datasheet points
    /// needs a negative floor d0^2 (n about 0.145). That does not rule out a
    /// positive floor: with n fixed at 0.3 / 0.4 / 0.5, least squares on d^2
    /// gives d0^2 = +0.011 / +0.070 / +0.105 mm^2 with residuals of at most
    /// about 0.015 mm (0.0152 at n = 0.5), well inside the 0.05 mm rounding of
    /// the points, so three rounded points cannot tell the models apart.
    #[test]
    fn spot_growth_floor_fit_recorded() {
        let pts = [(100.0, 0.4), (200.0, 0.5), (400.0, 0.6)];
        let (d0_sq, k, n) = fit_spot_growth_floor(pts);
        println!("floor fit: d0^2 = {d0_sq}, k = {k}, n = {n}");
        assert!(d0_sq < 0.0, "d0^2 = {d0_sq}");
        assert!(close(n, 0.1448, 0.001), "n = {n}");
        // Fixed n: d^2 = d0^2 + k x with x = I^(2n), linear least squares.
        for (n, d0_sq_expected) in [(0.3, 0.011), (0.4, 0.070), (0.5, 0.105)] {
            let xs = pts.map(|(i, _)| f64::powf(i, 2.0 * n));
            let qs = pts.map(|(_, d)| d * d);
            let (mx, mq) = (xs.iter().sum::<f64>() / 3.0, qs.iter().sum::<f64>() / 3.0);
            let sxq: f64 = xs.iter().zip(&qs).map(|(x, q)| (x - mx) * (q - mq)).sum();
            let sxx: f64 = xs.iter().map(|x| (x - mx) * (x - mx)).sum();
            let k = sxq / sxx;
            let d0_sq = mq - k * mx;
            println!("n {n}: d0^2 = {d0_sq}, k = {k}");
            assert!(close(d0_sq, d0_sq_expected, 0.0015), "n {n}: d0^2 = {d0_sq}");
            for (i, d) in pts {
                let r = spot_fwhm_mm(i, d0_sq.sqrt(), k, n) - d;
                assert!(r.abs() <= 0.0155, "n {n}, I {i}: residual {r} mm");
            }
        }
        // Round trip on a synthetic model with a real floor.
        let pts = [100.0, 200.0, 400.0].map(|i| (i, spot_fwhm_mm(i, 0.3, 0.002, 0.4)));
        let (d0_sq, k, n) = fit_spot_growth_floor(pts);
        assert!(close(d0_sq, 0.09, 1e-9) && close(k, 0.002, 1e-9) && close(n, 0.4, 1e-9));
    }

    /// "Width versus video level goes roughly as V^0.7 to V^1.2" for gamma 2.4, n 0.3 to 0.5.
    #[test]
    fn width_vs_video() {
        assert!(close(width_vs_video_exponent(2.4, 0.3), 0.72, 1e-12));
        assert!(close(width_vs_video_exponent(2.4, 0.5), 1.2, 1e-12));
        assert!(close(spot_fwhm_mm(0.0, 0.3, 0.002, 0.4), 0.3, 1e-15));
    }

    #[test]
    fn erf_values() {
        assert_eq!(erf(0.0), 0.0);
        assert!(close(erf(0.5), 0.520_499_877_813_046_5, 1e-14));
        assert!(close(erf(1.0), 0.842_700_792_949_714_9, 1e-14));
        assert!(close(erf(2.0), 0.995_322_265_018_952_7, 1e-14));
        assert!(close(erf(1.281_551_565_544_600_5 / 2.0_f64.sqrt()), 0.8, 1e-12));
        // Reference values (Abramowitz and Stegun table 7.1 / high-precision).
        assert!(close(erf(-0.5), -0.520_499_877_813_046_5, 1e-14));
        assert!(close(erf(3.0), 0.999_977_909_503_001_4, 1e-14));
        assert!(close(erf(0.1), 0.112_462_916_018_284_9, 1e-15));
    }

    #[test]
    #[should_panic(expected = "points")]
    fn power_law_one_point_panics() {
        fit_power_law(&[(100.0, 0.4)]);
    }

    #[test]
    #[should_panic(expected = "finite positive")]
    fn power_law_non_positive_panics() {
        fit_power_law(&[(100.0, 0.4), (0.0, 0.5)]);
    }

    #[test]
    #[should_panic(expected = "distinct currents")]
    fn power_law_equal_currents_panics() {
        fit_power_law(&[(100.0, 0.4), (100.0, 0.5)]);
    }

    #[test]
    #[should_panic(expected = "widths must be")]
    fn floor_fit_non_monotone_panics() {
        fit_spot_growth_floor([(100.0, 0.5), (200.0, 0.4), (400.0, 0.6)]);
    }

    #[test]
    #[should_panic(expected = "geometric progression")]
    fn floor_fit_non_geometric_panics() {
        fit_spot_growth_floor([(100.0, 0.4), (200.0, 0.5), (300.0, 0.6)]);
    }

    #[test]
    #[should_panic(expected = "bandwidth")]
    fn amplifier_zero_bandwidth_panics() {
        gaussian_amp_sigma_ns(0.0);
    }

    #[test]
    #[should_panic(expected = "bandwidth")]
    fn single_pole_negative_bandwidth_panics() {
        single_pole_rise_ns(-1.0);
    }

    /// Gaussian amplifier: sigma = 0.1325 / B, 10-90 % rise = 0.3396 / B.
    #[test]
    fn gaussian_amplifier() {
        assert!(close(gaussian_amp_sigma_ns(1000.0), 0.1325, 0.0001));
        for b in [4.0, 10.0, 100.0] {
            let rise = gaussian_amp_rise_ns(b);
            assert!(close(rise * b / 1000.0, 0.3396, 0.0001), "B {b}: {rise} ns");
        }
        assert!(close(gaussian_amp_step(0.0, 10.0), 0.5, 1e-15));
    }

    /// Box mean of the Gaussian step: hand values (sigma 1 on [0, 1): 1 + T(1) - T(0) = 1 + 0.0833155 -
    /// 0.3989423 = 0.6843732; sigma 0.3 on [0.2, 0.7): 0.9113199), symmetry (0.5 on [-c, c)), the far
    /// sides (0 and 1, as limits), and a midpoint-rule quadrature of the step response.
    #[test]
    fn gaussian_step_box_mean_values() {
        assert!(close(gaussian_step_box_mean(0.0, 1.0, 1.0), 0.684_373_2, 1e-7));
        assert!(close(gaussian_step_box_mean(0.2, 0.7, 0.3), 0.911_319_9, 1e-7));
        assert!(close(gaussian_step_box_mean(-0.5, 0.5, 0.3), 0.5, 1e-15));
        assert_eq!(gaussian_step_box_mean(8.0, 8.25, 0.1), 1.0);
        assert_eq!(gaussian_step_box_mean(-8.25, -8.0, 0.1), 0.0);
        for (a, b, s) in [
            (-0.3, -0.05, 0.14),
            (-0.1, 0.15, 0.09),
            (0.0, 0.25, 0.02),
            (1.0, 1.5, 0.6),
        ] {
            let n = 20_000;
            let h = (b - a) / f64::from(n);
            let quad: f64 = (0..n)
                .map(|i| 0.5 * (1.0 + erf((a + (f64::from(i) + 0.5) * h) / (s * 2.0_f64.sqrt()))))
                .sum::<f64>()
                / f64::from(n);
            let got = gaussian_step_box_mean(a, b, s);
            assert!(close(got, quad, 1e-8), "[{a}, {b}) sigma {s}: {got} vs {quad}");
        }
    }

    #[test]
    #[should_panic(expected = "gaussian_step_box_mean")]
    fn gaussian_step_box_mean_empty_interval_panics() {
        gaussian_step_box_mean(0.5, 0.5, 1.0);
    }

    /// CRT.md rise-time table with 0.35 / B: consumer 4 to 8 MHz 44 to 88 ns
    /// (corrected from 58 ns, which is 6 MHz), PVM 8 to 15 MHz 23 to 44 ns,
    /// high-res 20 to 30 MHz 12 to 18 ns.
    #[test]
    fn rise_time_table() {
        assert!(close(single_pole_rise_ns(1.0) / 1000.0, 0.35, 0.001));
        let r = single_pole_rise_ns;
        assert!(close(r(4.0), 88.0, 1.0));
        assert!(close(r(8.0), 44.0, 1.0), "consumer 8 MHz: {}", r(8.0));
        assert!(close(r(6.0), 58.0, 1.0));
        assert!(close(r(15.0), 23.0, 1.0));
        assert!(close(r(20.0), 18.0, 0.6) && close(r(30.0), 12.0, 0.6));
    }
}

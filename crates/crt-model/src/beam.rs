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

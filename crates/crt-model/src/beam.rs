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

/// Normalised beam current at mid drive (T-36b; QUESTIONS PHY-7 "normalised drive with FWHM at mid and
/// at peak"): drive voltage V = 0.5 through the reference gun law I = V^2.4 (CRT.md Gun law, gamma 2.4;
/// the PLAN T-34 check GOG V = 0.5 -> 0.18946), so 0.5^2.4. Peak drive V = 1 is I = 1. [D]
pub const MID_DRIVE_CURRENT: f64 = 0.189_464_570_813_799_78;

/// Smallest spot FWHM in mm: a guard for parameters whose law would reach zero (a spot shrinking with
/// current below 0). Not reached by any physical setting. [E]
pub const MIN_SPOT_FWHM_MM: f64 = 0.001;

/// Smallest growth exponent n (guard: n = 0 would divide by zero). [E]
pub const MIN_GROWTH_EXPONENT: f64 = 0.05;

/// Spot size against the normalised beam current I (CRT.md "Spot growth with beam current"):
/// FWHM^2 = d0^2 + k I^(2n). As QUESTIONS PHY-7 and PLAN T-36b set it, the law is given by its FWHM at
/// mid drive (I = [`MID_DRIVE_CURRENT`]), its FWHM at peak (I = 1) and n; d0 and k follow. [E][D]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpotGrowth {
    /// FWHM at mid drive, mm.
    pub fwhm_mid: f64,
    /// FWHM at peak drive, mm.
    pub fwhm_peak: f64,
    /// Growth exponent n (CRT.md: 0.2 to 0.5).
    pub n: f64,
}

impl SpotGrowth {
    /// No growth: the same FWHM at every current (k = 0).
    pub fn constant(fwhm: f64) -> Self {
        Self {
            fwhm_mid: fwhm,
            fwhm_peak: fwhm,
            n: 0.5,
        }
    }

    /// The exponent the law uses. The two widths and n give d0^2 = (d_mid^2 - d_peak^2 x) / (1 - x)
    /// with x = I_mid^(2n) (see [`Self::floor_sq_and_k`]), negative when d_peak / d_mid > I_mid^(-n),
    /// that is n < ln(d_peak / d_mid) / ln(1 / I_mid). A negative floor is not physical (the spot would
    /// vanish at a low current), so n is raised to that bound, where d0 = 0 and the law is the pure
    /// power d = d_mid (I / I_mid)^n. n is also kept above [`MIN_GROWTH_EXPONENT`]. [D]
    pub fn exponent(&self) -> f64 {
        let n_min = if self.fwhm_peak > self.fwhm_mid {
            (self.fwhm_peak / self.fwhm_mid).ln() / (1.0 / MID_DRIVE_CURRENT).ln()
        } else {
            0.0
        };
        self.n.max(MIN_GROWTH_EXPONENT).max(n_min)
    }

    /// (d0^2, k), in mm^2 and mm^2 per unit current^(2n). From d_peak^2 = d0^2 + k (I = 1) and
    /// d_mid^2 = d0^2 + k x (x = I_mid^(2n)): k = (d_peak^2 - d_mid^2) / (1 - x), d0^2 = d_peak^2 - k. [D]
    ///
    /// # Panics
    /// Unless both widths are finite and positive.
    pub fn floor_sq_and_k(&self) -> (f64, f64) {
        assert!(
            self.fwhm_mid.is_finite() && self.fwhm_mid > 0.0 && self.fwhm_peak.is_finite() && self.fwhm_peak > 0.0,
            "spot growth: {self:?}"
        );
        let x = MID_DRIVE_CURRENT.powf(2.0 * self.exponent());
        let (dm2, dp2) = (self.fwhm_mid * self.fwhm_mid, self.fwhm_peak * self.fwhm_peak);
        let k = (dp2 - dm2) / (1.0 - x);
        (dp2 - k, k)
    }

    /// FWHM (mm) at the normalised beam current `i` (negative currents count as 0), at least
    /// [`MIN_SPOT_FWHM_MM`].
    pub fn fwhm(&self, i: f64) -> f64 {
        let (d0_sq, k) = self.floor_sq_and_k();
        let d_sq = d0_sq + k * i.max(0.0).powf(2.0 * self.exponent());
        d_sq.max(MIN_SPOT_FWHM_MM * MIN_SPOT_FWHM_MM).sqrt()
    }
}

/// Standard normal distribution function Phi(t) = (1 + erf(t / sqrt 2)) / 2. [K]
fn normal_cdf(t: f64) -> f64 {
    0.5 * (1.0 + erf(t / 2.0_f64.sqrt()))
}

/// Standard normal density phi(t) = exp(-t^2 / 2) / sqrt(2 pi). [K]
fn normal_pdf(t: f64) -> f64 {
    (-0.5 * t * t).exp() / (2.0 * PI).sqrt()
}

/// Energy that a uniform segment [a0, a1] of unit linear density, blurred by a unit-area Gaussian of
/// sigma `sigma`, puts into [b0, b1]: the integral over x' in [a0, a1] and x in [b0, b1] of
/// g(x - x') (length squared; divide by b1 - b0 for the mean over [b0, b1]). The inner integral is
/// Phi((b1 - x') / sigma) - Phi((b0 - x') / sigma); G(u) = u Phi(u / sigma) + sigma phi(u / sigma) has the
/// derivative Phi(u / sigma), so the result is G(b1 - a0) - G(b1 - a1) - G(b0 - a0) + G(b0 - a1). [D]
///
/// # Panics
/// Unless `sigma` > 0, a0 <= a1 and b0 <= b1 (all finite).
pub fn box_gaussian_overlap(a0: f64, a1: f64, b0: f64, b1: f64, sigma: f64) -> f64 {
    assert!(
        sigma.is_finite() && sigma > 0.0 && a0.is_finite() && a1.is_finite() && b0.is_finite() && b1.is_finite(),
        "box_gaussian_overlap: sigma = {sigma}, [{a0}, {a1}] x [{b0}, {b1}]"
    );
    assert!(
        a0 <= a1 && b0 <= b1,
        "box_gaussian_overlap: [{a0}, {a1}] x [{b0}, {b1}]"
    );
    let g = |u: f64| u * normal_cdf(u / sigma) + sigma * normal_pdf(u / sigma);
    g(b1 - a0) - g(b1 - a1) - g(b0 - a0) + g(b0 - a1)
}

/// One channel of the beam-current image of the amp domain placed on the tube (T-36b), host px units:
/// `currents` holds the native rows one after the other, `samples` amp samples per row; sample j of a
/// line covers [j a, (j + 1) a) with a = `sample_width` (the current is held over the sample, zero-order
/// hold); line n is centred at `first + n * pitch` and draws native row n / `mult`.
#[derive(Clone, Copy, Debug)]
pub struct AmpRaster<'a> {
    pub currents: &'a [f64],
    pub samples: usize,
    pub sample_width: f64,
    pub first: f64,
    pub pitch: f64,
    pub mult: usize,
}

/// The 2D spot of the oracle (CRT.md "Spot profile"; round at the screen centre, 0.4 x 0.4 mm in the
/// datasheet table): core sigma in px as a function of the beam current (spot growth), and the optional
/// wide tail of [`ScanlineProfile`] applied to both axes (core and tail are 2D Gaussians). [K][E]
pub struct Spot<'a> {
    pub sigma_px: &'a dyn Fn(f64) -> f64,
    pub tail_weight: f64,
    pub tail_ratio: f64,
}

/// The 2D beam oracle of one channel (T-36b; CRT.md "Horizontal versus vertical sharpness": along the
/// line the held signal convolved with the spot x-profile, across it the scanlines blurred by the spot
/// y-profile). [`BeamOracle::new`] evaluates the spot sigma of every sample once; [`BeamOracle::pixel_mean`]
/// gives the light of one host pixel.
pub struct BeamOracle<'a> {
    raster: AmpRaster<'a>,
    tail_weight: f64,
    tail_ratio: f64,
    /// Core sigma (px) of every sample, same layout as the currents.
    sigma: Vec<f64>,
    /// Widest sigma (core or tail) of each native row.
    row_widest: Vec<f64>,
}

impl<'a> BeamOracle<'a> {
    /// # Panics
    /// Unless the raster is consistent (pitch and sample width > 0, mult >= 1, whole rows), 0 <=
    /// tail weight <= 1 and tail ratio > 0, and every sigma is finite and positive.
    pub fn new(raster: AmpRaster<'a>, spot: &Spot) -> Self {
        let AmpRaster {
            currents,
            samples,
            sample_width,
            pitch,
            mult,
            ..
        } = raster;
        assert!(
            pitch > 0.0 && sample_width > 0.0 && mult >= 1 && samples >= 1 && currents.len() % samples == 0,
            "beam oracle: raster {raster:?}"
        );
        let (w, r) = (spot.tail_weight, spot.tail_ratio);
        assert!((0.0..=1.0).contains(&w) && r > 0.0, "beam oracle: tail {w}, {r}");
        let sigma: Vec<f64> = currents.iter().map(|&i| (spot.sigma_px)(i)).collect();
        assert!(
            sigma.iter().all(|s| s.is_finite() && *s > 0.0),
            "beam oracle: sigma not finite and positive"
        );
        let widen = if w > 0.0 { r.max(1.0) } else { 1.0 };
        let row_widest = sigma
            .chunks(samples)
            .map(|row| row.iter().fold(0.0_f64, |m, &s| m.max(s)) * widen)
            .collect();
        Self {
            raster,
            tail_weight: w,
            tail_ratio: r,
            sigma,
            row_widest,
        }
    }

    /// Area-mean light of the host pixel [x0, x1) x [y0, y1). While the beam crosses sample j of line
    /// n it carries i = i_nj, draws a spot of core sigma s = sigma_px(i) and deposits i * pitch of
    /// light per unit length (one line pitch of screen per line, as [`scanline_row_mean`]). With the
    /// convergence offset `offset` = (dx, dy) px of this channel's beam, the pixel receives
    ///   sum over n, j of i P [(1 - w) X(s) Y(s) + w X(r s) Y(r s)] / ((x1 - x0) (y1 - y0)),
    /// X(s) = [`box_gaussian_overlap`](j a + dx, (j + 1) a + dx, x0, x1, s), Y(s) =
    /// [`gaussian_fraction`](y0 - c, y1 - c, s), c = first + n P + dy. Terms farther than 12 widest
    /// sigmas from the pixel are skipped (each below 2e-33 of i P). A flat field of current I gives I;
    /// an isolated sample keeps its energy i a P over the whole plane. [D]
    ///
    /// # Panics
    /// Unless the pixel is not empty.
    pub fn pixel_mean(&self, offset: (f64, f64), x: (f64, f64), y: (f64, f64)) -> f64 {
        assert!(x.0 < x.1 && y.0 < y.1, "beam oracle: pixel {x:?} x {y:?}");
        let AmpRaster {
            currents,
            samples,
            sample_width: a,
            first,
            pitch,
            mult,
        } = self.raster;
        let (w, r) = (self.tail_weight, self.tail_ratio);
        let (dx, dy) = offset;
        let rows = currents.len() / samples;
        // X depends on the sample and sigma only: remembered per sample for the last sigma seen.
        let mut x_memo: Vec<(f64, f64, f64)> = vec![(f64::NAN, 0.0, 0.0); samples];
        let mut sum = 0.0;
        for n in 0..rows * mult {
            let row = n / mult;
            let c = first + n as f64 * pitch + dy;
            let far_y = (y.0 - c).max(c - y.1);
            if far_y > 12.0 * self.row_widest[row] {
                continue;
            }
            let mut y_memo = (f64::NAN, 0.0, 0.0);
            for j in 0..samples {
                let i = currents[row * samples + j];
                if i == 0.0 {
                    continue;
                }
                let s = self.sigma[row * samples + j];
                let widest = if w > 0.0 { s * r.max(1.0) } else { s };
                let (s0, s1) = (j as f64 * a + dx, (j + 1) as f64 * a + dx);
                let far_x = (x.0 - s1).max(s0 - x.1);
                if far_x > 12.0 * widest || far_y > 12.0 * widest {
                    continue;
                }
                if x_memo[j].0 != s {
                    let core = box_gaussian_overlap(s0, s1, x.0, x.1, s);
                    let tail = if w > 0.0 {
                        box_gaussian_overlap(s0, s1, x.0, x.1, s * r)
                    } else {
                        0.0
                    };
                    x_memo[j] = (s, core, tail);
                }
                if y_memo.0 != s {
                    let core = gaussian_fraction(y.0 - c, y.1 - c, s);
                    let tail = if w > 0.0 {
                        gaussian_fraction(y.0 - c, y.1 - c, s * r)
                    } else {
                        0.0
                    };
                    y_memo = (s, core, tail);
                }
                let (_, xc, xt) = x_memo[j];
                let (_, yc, yt) = y_memo;
                let f = if w > 0.0 {
                    (1.0 - w) * xc * yc + w * xt * yt
                } else {
                    xc * yc
                };
                sum += i * f;
            }
        }
        sum * pitch / ((x.1 - x.0) * (y.1 - y.0))
    }
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

    /// Mid drive: V = 0.5 through I = V^2.4, 0.18946 (PLAN T-34 GOG check).
    #[test]
    fn mid_drive_current() {
        assert!(close(MID_DRIVE_CURRENT, 0.5_f64.powf(2.4), 1e-16));
        assert!(close(MID_DRIVE_CURRENT, 0.18946, 1e-5));
    }

    /// PVM-20M4U (CRT.md Starting presets 0.45 -> 0.8 mm) with n = 0.5: x = I_mid = 0.189465,
    /// k = (0.64 - 0.2025) / (1 - 0.189465) = 0.539767, d0^2 = 0.64 - 0.539767 = 0.100233
    /// (d0 = 0.316596 mm, black to white 0.8 / 0.3166 = 2.53, CRT.md 1.5 to 2.5); at I = 0.5:
    /// d^2 = 0.100233 + 0.539767 x 0.5 = 0.370117, d = 0.608373 mm. Hand-derived.
    #[test]
    fn spot_growth_pvm_defaults() {
        let g = SpotGrowth {
            fwhm_mid: 0.45,
            fwhm_peak: 0.8,
            n: 0.5,
        };
        assert_eq!(g.exponent(), 0.5);
        let (d0_sq, k) = g.floor_sq_and_k();
        assert!(
            close(k, 0.539_767, 1e-6) && close(d0_sq, 0.100_233, 1e-6),
            "{d0_sq} {k}"
        );
        assert!(close(g.fwhm(MID_DRIVE_CURRENT), 0.45, 1e-12));
        assert!(close(g.fwhm(1.0), 0.8, 1e-12));
        assert!(close(g.fwhm(0.0), 0.316_596, 1e-6));
        assert!(close(g.fwhm(-1.0), g.fwhm(0.0), 0.0));
        assert!(close(g.fwhm(0.5), 0.608_373, 1e-6));
    }

    /// With n = 0.27 (CRT.md T-63, monitor class) the same widths would need a negative floor:
    /// d0^2 = (0.2025 - 0.64 x) / (1 - x), x = 0.189465^0.54 = 0.40724, gives -0.0980. The exponent is
    /// raised to ln(0.8 / 0.45) / ln(1 / 0.189465) = 0.575364 / 1.663553 = 0.345865, where d0 = 0 and
    /// d = 0.45 (I / I_mid)^0.345865.
    #[test]
    fn spot_growth_negative_floor_raises_n() {
        let g = SpotGrowth {
            fwhm_mid: 0.45,
            fwhm_peak: 0.8,
            n: 0.27,
        };
        let x = MID_DRIVE_CURRENT.powf(0.54);
        assert!(close((0.2025 - 0.64 * x) / (1.0 - x), -0.0980, 5e-4));
        assert!(close(g.exponent(), 0.345_865, 1e-6), "n = {}", g.exponent());
        let (d0_sq, _) = g.floor_sq_and_k();
        assert!(d0_sq.abs() < 1e-12, "d0^2 = {d0_sq}");
        let n_min = (0.8_f64 / 0.45).ln() / (1.0 / MID_DRIVE_CURRENT).ln();
        for i in [0.01, 0.1, 0.5, 2.0] {
            let want = 0.45 * (i / MID_DRIVE_CURRENT).powf(n_min);
            assert!(close(g.fwhm(i), want, 1e-9 * want), "I {i}");
        }
        assert_eq!(g.fwhm(0.0), MIN_SPOT_FWHM_MM);
    }

    /// The datasheet points 0.4, 0.5, 0.6 mm at relative currents 1, 2, 4 (PLAN T-36b: n = 0.29, d0 = 0):
    /// peak = relative 4, d(r) = 0.4 r^0.29 = 0.4, 0.489056, 0.597940 mm, each within 0.011 mm of the
    /// datasheet. Set by d_mid = 0.4 (4 I_mid)^0.29 and d_peak = 0.4 4^0.29; the floor is then 0.
    #[test]
    fn spot_growth_datasheet_with_zero_floor() {
        let g = SpotGrowth {
            fwhm_mid: 0.4 * (4.0 * MID_DRIVE_CURRENT).powf(0.29),
            fwhm_peak: 0.4 * 4.0_f64.powf(0.29),
            n: 0.29,
        };
        assert!(close(g.exponent(), 0.29, 1e-12));
        assert!(g.floor_sq_and_k().0.abs() < 1e-12);
        for (r, model, sheet) in [(1.0, 0.4, 0.4), (2.0, 0.489_056, 0.5), (4.0, 0.597_940, 0.6)] {
            let d = g.fwhm(r / 4.0);
            assert!(close(d, model, 1e-6), "r {r}: {d}");
            assert!(close(d, sheet, 0.011), "r {r}: {d} vs datasheet {sheet}");
        }
    }

    /// No growth: k = 0 and the width is the same at every current; a spot that shrinks with current
    /// (peak < mid) keeps a positive floor and is clamped at the minimum width at high current.
    #[test]
    fn spot_growth_constant_and_shrinking() {
        let g = SpotGrowth::constant(0.45);
        assert_eq!(g.floor_sq_and_k().1, 0.0);
        for i in [0.0, 0.2, 1.0, 3.0] {
            assert!(close(g.fwhm(i), 0.45, 1e-15));
        }
        let s = SpotGrowth {
            fwhm_mid: 0.6,
            fwhm_peak: 0.3,
            n: 0.5,
        };
        assert_eq!(s.exponent(), 0.5);
        assert!(close(s.fwhm(1.0), 0.3, 1e-12) && s.fwhm(0.0) > 0.6);
        assert_eq!(s.fwhm(100.0), MIN_SPOT_FWHM_MM);
    }

    /// The box overlap equals a numeric integral of [`gaussian_fraction`] over the source box
    /// (Simpson, 2000 intervals), tends to the overlap length for a tiny sigma, and a box much wider
    /// than sigma on both sides keeps all of a unit segment's energy.
    #[test]
    fn box_gaussian_overlap_checks() {
        for (a0, a1, b0, b1, s) in [
            (0.0, 1.0, 0.0, 1.0, 0.5),
            (0.0, 0.25, 3.0, 4.0, 1.3),
            (-2.0, 5.0, 1.0, 1.5, 0.7),
            (10.0, 12.0, -1.0, 0.0, 4.0),
        ] {
            let m = 2000;
            let h = (a1 - a0) / f64::from(m);
            let f = |x: f64| gaussian_fraction(b0 - x, b1 - x, s);
            let simpson = (0..=m)
                .map(|k| {
                    let wgt = if k == 0 || k == m {
                        1.0
                    } else if k % 2 == 1 {
                        4.0
                    } else {
                        2.0
                    };
                    wgt * f(a0 + f64::from(k) * h)
                })
                .sum::<f64>()
                * h
                / 3.0;
            let got = box_gaussian_overlap(a0, a1, b0, b1, s);
            assert!(
                close(got, simpson, 1e-12),
                "{a0} {a1} {b0} {b1} {s}: {got} vs {simpson}"
            );
        }
        assert!(close(box_gaussian_overlap(0.0, 2.0, 1.5, 3.0, 1e-6), 0.5, 1e-9));
        assert!(close(box_gaussian_overlap(0.0, 1.0, -50.0, 51.0, 2.0), 1.0, 1e-12));
    }

    /// The 2D oracle: an isolated sample keeps its energy i a P (sum over a large pixel grid), its
    /// centroid sits at the sample centre plus the convergence offset, a flat field gives its current
    /// at every interior pixel for any growth, and with an x-flat raster it reduces to
    /// [`scanline_row_mean`].
    #[test]
    fn beam_oracle_energy_centroid_flat() {
        let growth = SpotGrowth {
            fwhm_mid: 0.45,
            fwhm_peak: 0.8,
            n: 0.5,
        };
        let ppm = 10.0;
        let sigma = |i: f64| sigma_from_fwhm(growth.fwhm(i)) * ppm;
        let spot = Spot {
            sigma_px: &sigma,
            tail_weight: 0.05,
            tail_ratio: 3.0,
        };
        let mut one = vec![0.0; 8 * 12];
        one[4 * 12 + 5] = 0.7;
        let raster = AmpRaster {
            currents: &one,
            samples: 12,
            sample_width: 2.0,
            first: 4.0,
            pitch: 8.0,
            mult: 1,
        };
        let oracle = BeamOracle::new(raster, &spot);
        for offset in [(0.0, 0.0), (1.5, -2.25)] {
            let (mut e, mut mx, mut my) = (0.0, 0.0, 0.0);
            for py in -40..104 {
                for px in -40..64 {
                    let (x, y) = (f64::from(px), f64::from(py));
                    let v = oracle.pixel_mean(offset, (x, x + 1.0), (y, y + 1.0));
                    e += v;
                    mx += v * (x + 0.5);
                    my += v * (y + 0.5);
                }
            }
            assert!(close(e, 0.7 * 2.0 * 8.0, 1e-6), "energy {e}");
            assert!(close(mx / e, 11.0 + offset.0, 1e-6), "x centroid {}", mx / e);
            assert!(close(my / e, 36.0 + offset.1, 1e-6), "y centroid {}", my / e);
        }
        // Flat field 128 x 128 px: the pixels near (64, 68) are 7.7 tail sigmas (8.3 px) from every edge.
        let flat = vec![0.6; 16 * 64];
        let raster = AmpRaster {
            currents: &flat,
            samples: 64,
            ..raster
        };
        let oracle = BeamOracle::new(raster, &spot);
        let p = ScanlineProfile {
            sigma: sigma(0.6),
            tail_weight: 0.05,
            tail_ratio: 3.0,
        };
        let line = vec![0.6; 16];
        for (px, py) in [(62, 20), (63, 33), (64, 70)] {
            let (x, y) = (f64::from(px), f64::from(py));
            let got = oracle.pixel_mean((0.0, 0.0), (x, x + 1.0), (y, y + 1.0));
            let want = scanline_row_mean(&line, 4.0, 8.0, &p, y, y + 1.0);
            assert!(close(got, want, 1e-12), "({px}, {py}): {got} vs {want}");
        }
        let period: f64 = (64..72)
            .map(|py| {
                let y = f64::from(py);
                oracle.pixel_mean((0.0, 0.0), (63.0, 64.0), (y, y + 1.0))
            })
            .sum();
        assert!(close(period / 8.0, 0.6, 1e-9), "flat mean {}", period / 8.0);
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

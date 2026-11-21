//! GPU tests of the `amp` slot (T-33, video amplifier, voltage domain), headless, validation on and zero
//! validation messages. Each test injects its own `FrameConstants` (K) and a native image after `decode`,
//! runs only `amp`, reads back the amp-domain image and compares it with the `crt-model` oracle
//! (`crt_model::beam`: the Gaussian amplifier sigma and rise time, `erf` and the sub-sample box mean
//! `gaussian_step_box_mean`, in f64) and with numbers
//! derived by hand from `CRT.md` below, never with the shader's formula.
//!
//! # Model under test (`CRT.md` "Signal path", rule 1; PLAN T-33)
//!
//! A native row is a zero-order hold at the pixel clock f (pixel j holds V_j over [j, j + 1) source px,
//! black outside the active line), low-passed by a Gaussian amplifier of -3 dB bandwidth B. A hold is a
//! sum of steps at the pixel edges, so the amplifier output is the sum of the edge step responses:
//! v(x) = sum_e (V_e - V_{e-1}) step(x - e - d_e). Rising edges use B, falling edges B / r with
//! r = `amp_fall_rise_ratio` (fall time = r x rise time). Asymmetric slew: both kinds of transition start
//! together after the input edge (their 10 % points coincide) and the slower one ends later, so the slower
//! kind is delayed by d = (rise_slow - rise_fast) / 2 (half the difference of the 10-90 % times), the
//! faster kind not (d = 0); with r = 1 every response is centred on its edge. Optional peaking, off by
//! default, is described in D5. The slot outputs, for each of the K sub-samples q of every native pixel,
//! the mean of v over the sub-sample interval [q / K, (q + 1) / K) (box integral, D6), clamped to the
//! range of the input levels within the kernel reach (D7). When every edge sigma is at most 0.01 source
//! px and peaking is off, the amplifier is ideal and the output is the exact hold. The gun law is not
//! part of the slot: the luminance checks apply the power law to the read-back voltages on the CPU, in
//! the order of the chain (box mean of the voltage in `amp`, then the gun law per sub-sample).
//!
//! # Derivations (by hand, for the independent check)
//!
//! **D1, sigma and rise time.** A Gaussian impulse response of time sigma s has |H(f)| =
//! exp(-2 pi^2 s^2 f^2). At the -3 dB bandwidth |H(B)| = 1/sqrt(2): 2 pi^2 s^2 B^2 = (ln 2)/2, so
//! s B = sqrt(ln 2) / (2 pi) = 0.832555 / 6.283185 = 0.132505 (`CRT.md`: "about 0.1325 / B"). The step
//! response is Phi(t / s); Phi(z) = 0.9 at z = 1.281552, so the 10-90 % rise is 2 x 1.281552 s =
//! 2.563103 s = 0.339624 / B. One source pixel lasts 1 / f, so in source px: sigma_px = 0.132505 f / B and
//! rise_px = 0.339624 f / B. Example: B = 5 MHz, f = 5.37 MHz: sigma_px = 0.142311, rise 0.364759 px.
//! The other bandwidths used: B = 4 MHz: sigma_px = 0.177888; B = 3 MHz: sigma_px = 0.237184; B = 8 MHz:
//! sigma_px = 0.088944; B = 15 MHz: sigma_px = 0.047437.
//!
//! **D2, DC and energy at gamma 1.** Inside a flat run every step is 0, so v = V exactly (DC gain 1). For
//! an isolated pixel (on [0, 1)), v = Phi((x - d_r) / s_r) - Phi((x - 1 - d_f) / s_f). Each edge response
//! differs from the delayed ideal step H(u - d) by an odd function of u - d, whose integral is 0, so the
//! voltage-time area is 1 + d_f - d_r pixel x V. Symmetric slew (r = 1): d = 0, the area is exactly the
//! hold's, 1 pixel x V, as for a pixel of a horizontal line (a run of equal pixels integrates to exactly
//! 1 pixel x V): at gamma 1 the isolated vertical line and the horizontal line carry the same light. The
//! K samples sit at +/-(m + 1/2) / K around every edge, so the odd parts cancel pairwise and the discrete
//! sum (1/K) sum_q v_q is exact too (with box means it is the integral itself, D6). Asymmetric slew: the slower edge is delayed by
//! d = 1.281552 |s_f - s_r| (Phi^-1(0.9) = 1.281552: the 10 % point lies 1.281552 sigma before the
//! centre), so the area is 1 + 1.281552 (s_f - s_r), more with a slow fall, less with a slow rise, as for
//! a slew-limited amplifier. B = 4 MHz, s_r = 0.177888: r = 2 (s_f = 0.355776, d_f = 0.227973): area
//! 1.227973; r = 1/2 (s_f = 0.088944, d_r = 0.113986): area 0.886014.
//!
//! **D3, vertical line dimming at gamma 2.4** (`CRT.md` rule 1: "isolated 1-pixel vertical lines come out
//! 5 % to over 50 % dimmer"). Luminance of the isolated pixel: L = integral of v(x)^gamma dx, against 1 for
//! a pixel of a horizontal line. While the two edges barely overlap, each edge adds
//! s x integral of (Phi(z)^gamma - H(z)) dz = -s mu(gamma), where mu(gamma) is the mean of the distribution
//! whose CDF is Phi^gamma (mean = integral of (1 - F) over z > 0 minus integral of F over z < 0). For an
//! integer n, mu(n) is the expected maximum of n standard normals: mu(1) = 0, mu(2) = 1/sqrt(pi) =
//! 0.5642, mu(3) = 3 / (2 sqrt(pi)) = 0.8463; for gamma = 2.4, mu = gamma x integral of
//! z Phi(z)^(gamma - 1) phi(z) dz = 0.6948 (quadrature; between mu(2) and mu(3), above the chord 0.6770
//! since mu is concave in n; the CPU test `hand_constants_of_the_dimming_derivation` checks these). The
//! rising and the falling edge lose the same amount, so the dimming is
//! 1 - L = 2 mu(2.4) sigma_px = 1.3896 sigma_px:
//! - B = 5 MHz: 1.3896 x 0.142311 = 0.1978, 19.8 %;
//! - B = 4 MHz: 1.3896 x 0.177888 = 0.2472, 24.7 %;
//! - B = 3 MHz: 1.3896 x 0.237184 = 0.3296; the edges start to overlap (centre voltage
//!   2 Phi(0.5 / 0.237184) - 1 = 0.965 instead of 1), and the full integral gives 0.3291, 32.9 %;
//! - B = 8 MHz: 1.3896 x 0.088944 = 0.1236, 12.36 %; B = 15 MHz: 1.3896 x 0.047437 = 0.0659, 6.59 %.
//!
//! These are the PLAN T-33 values (19.8, 24.7, 32.9 %, +/- 1 point) and the review's 12.36 and 6.59 %.
//! The test also integrates v^gamma with the crt-model step response in f64 (continuous oracle) and checks
//! it against these numbers.
//!
//! Finite K: the slot delivers one voltage per sub-sample, the gun law then acts per sub-sample, so the
//! chain's luminance is (1/K) sum_q (mean_q v)^gamma, not the integral of v^gamma. Point samples (the
//! first T-33 version) missed transitions narrower than a sub-sample (K = 4: 19.4, 8.95, 0.50, 0.00 % at
//! 5, 8, 15, 30 MHz). Box means keep every transition's light, but by Jensen's inequality
//! ((mean v)^gamma <= mean of v^gamma for gamma > 1) they over-dim slightly: at f = 5.37 MHz the f64 box
//! oracle gives, in %, (continuous / K = 4 / K = 8 / K = 16): 3 MHz 32.91 / 34.39 / 33.29 / 33.01;
//! 4 MHz 24.72 / 26.67 / 25.22 / 24.85; 5 MHz 19.78 / 22.08 / 20.40 / 19.93; 8 MHz 12.36 / 14.92 / 13.34 /
//! 12.61; 15 MHz 6.59 / 8.51 / 7.88 / 7.05; 30 MHz 3.30 / 4.40 / 4.25 / 3.94. The excess over the
//! continuous value is at most 2.6 points at K = 4, 1.3 at K = 8, 0.5 at K = 16 (before T-fp16 the truncating
//! FP16 output added a bias of +0.03 to 0.07 point; stores now round to nearest and the bias is nil, the
//! margins below are kept as they were). So the PLAN +/- 1 point holds at K = 16 for every bandwidth
//! and at K = 8 up to 5 MHz; at K = 4 the box result is within +2.6 points (always dimmer), against
//! -12 points for point samples at 15 MHz.
//!
//! **D6, box integral of an edge.** Let Phi, phi be the standard normal CDF and density. Since
//! phi'(z) = -z phi(z), d/dz (z Phi(z) + phi(z)) = Phi(z) + z phi(z) - z phi(z) = Phi(z): J(z) =
//! z Phi(z) + phi(z) is an antiderivative of Phi, and integral over [a, b) of Phi(u / s) du =
//! s (J(b / s) - J(a / s)). With T(z) = J(-z) = phi(z) - z (1 - Phi(z)) (the integral of 1 - Phi over
//! [z, inf), positive and decreasing for z >= 0), J(z) - J(-z) = z (Phi(z) + Phi(-z)) = z, so
//! J(z) = max(z, 0) + T(|z|) for every z. Hence the mean over [a, b) is
//! (max(b, 0) - max(a, 0) + s (T(|b| / s) - T(|a| / s))) / (b - a), with no cancellation on either side
//! (`crt_model::beam::gaussian_step_box_mean`; checked there against hand values and quadrature: s = 1 on
//! [0, 1): 1 + T(1) - T(0) = 1 + 0.0833155 - 0.3989423 = 0.6843732). An edge of delay d integrates over
//! [x0 - e - d, x1 - e - d); a peaked edge is (1 + g) mean(s) - g mean(s_c); the line is the sum over its
//! edges (black blanking outside). Summed over all sub-samples the means give the exact integral of v,
//! so D2 holds for any K. An ideal edge (s -> 0) on a sub-sample boundary still moves s T(0) = s /
//! sqrt(2 pi) of voltage-time across the edge; below sigma_px = 0.01 the slot outputs the exact hold
//! instead (the defaults: 0.132505 x 5.37 / 100 = 0.0071 px).
//!
//! **D7, range clamp.** The asymmetric model superposes edges independently: in 1-0-1 (pixel 16 dark in
//! a bright row) at B = 3 MHz, r = 3, the slow fall at x = 16 (s_f = 0.711552, delay 1.281552 x 0.474368
//! = 0.607930) has not finished when the fast rise at x = 17 (s_r = 0.237184) arrives, and the sum peaks
//! at 1.089 (point model; 1.0889 as K = 16 box means) above full drive; the mirror 0-1-0 dips to
//! -1.4e-4. A slew-limited amplifier never leaves the range of its input levels: every sub-sample is
//! clamped to [lo, hi], the extremes of the input levels (blanking included) of the pixels within the
//! kernel reach ceil(6 sigma_max + delay_max) + 1 of its pixel; with peaking the range is widened by
//! g (hi - lo) (the largest overshoot of one peaked edge).
//!
//! **D4, asymmetry and the centroid.** With v = Phi((x - d_r) / s_r) - Phi((x - 1 - d_f) / s_f),
//! integration by parts gives integral of x v dx = -1/2 integral of x^2 v'(x) dx with
//! v' = phi_r(x - d_r) - phi_f(x - 1 - d_f) (Gaussian densities of sigma s_r and s_f, whose second moments
//! about 0 are d_r^2 + s_r^2 and (1 + d_f)^2 + s_f^2), so
//! M = integral of x v dx = ((1 + d_f)^2 - d_r^2) / 2 + (s_f^2 - s_r^2) / 2, and the gamma-1 centroid is
//! M / A with the area A of D2; the pixel centre is 1/2. B = 4 MHz:
//! - r = 2: A = 1.227973, M = 1.507927 / 2 + (0.126577 - 0.031644) / 2 = 0.753963 + 0.047466 = 0.801430,
//!   centroid 0.652645, shift +0.152645 px;
//! - r = 1/2: A = 0.886014, M = (1 - 0.012993) / 2 + (0.007911 - 0.031644) / 2 = 0.493504 - 0.011867 =
//!   0.481637, centroid 0.543600, shift +0.043600 px;
//! - r = 1: shift 0.
//!
//! Both asymmetries move the light to the right (`CRT.md`: "the smear trails to the right of an edge"): a
//! slow fall extends bright detail to the right, a slow rise starts it late. With gamma 2.4 the luminance
//! centroid moves right as well (checked by sign).
//!
//! **D5, peaking** (`CRT.md` controls table: consumer sharpness, +3 to 8 dB around 3 to 4 MHz, 5 to 20 %
//! overshoot; off by default). The response is multiplied by 1 + g (1 - G_p(f)), G_p a Gaussian low-pass
//! of corner f_p, so an edge response is (1 + g) Phi(u / s) - g Phi(u / s_c), s_c^2 = s^2 + s_p^2
//! (cascaded Gaussians add variances), s_p = 0.132505 f / f_p. Both terms are unit steps centred on the
//! edge: flat fields and pulse areas are unchanged (D2). The overshoot is the maximum of the response
//! minus 1, where (1 + g) phi(u / s) / s = g phi(u / s_c) / s_c, i.e.
//! u*^2 = 2 ln((1 + g) s_c / (g s)) / (1 / s^2 - 1 / s_c^2). B = 8 MHz, f_p = 3.5 MHz, g = 0.5:
//! s = 0.088944, s_p = 0.203300, s_c = 0.221906; ln(3 x 2.494892) = 2.012857;
//! 1 / s^2 - 1 / s_c^2 = 126.4057 - 20.3078 = 106.0979; u* = 0.194791 px; the response there is
//! 1.5 Phi(2.19004) - 0.5 Phi(0.877810) = 1.5 x 0.985740 - 0.5 x 0.809971 = 1.073624: overshoot 7.36 %
//! (inside the 5 to 20 % of `CRT.md`), mirrored as a 7.36 % undershoot before the edge.

mod common;

use crt_model::beam::{erf, gaussian_amp_rise_ns, gaussian_amp_sigma_ns, gaussian_step_box_mean};
use phoxium::gpu::{FrameConstants, Headless, ImageF32, Settings, StageInput, StageRun, TargetFormat};
use phoxium::params::Params;

/// Pixel clock of the PLAN T-33 checks: 240p consoles, 5.37 MHz (`CRT.md` raster timing table).
const F_PX: f64 = 5.37;
/// Tube gamma of the dimming check (PLAN T-33, `CRT.md` gun law).
const GAMMA: f64 = 2.4;

/// Parameters with the amplifier set to `b` MHz, pixel clock `f` MHz, fall/rise ratio `r`.
fn amp_params(b: f32, f: f32, r: f32) -> Params {
    Params {
        amp_bandwidth_mhz: b,
        amp_pixel_clock_mhz: f,
        amp_fall_rise_ratio: r,
        ..Params::default()
    }
}

/// A native image whose rows are `rows` (R, G, B per pixel), alpha 1.
fn native_image(rows: &[Vec<[f32; 3]>]) -> ImageF32 {
    let width = rows[0].len() as u32;
    let mut rgba = Vec::new();
    for row in rows {
        assert_eq!(row.len() as u32, width);
        for p in row {
            rgba.extend_from_slice(&[p[0], p[1], p[2], 1.0]);
        }
    }
    ImageF32 {
        width,
        height: rows.len() as u32,
        rgba,
    }
}

fn grey(row: &[f32]) -> Vec<[f32; 3]> {
    row.iter().map(|&v| [v, v, v]).collect()
}

/// Runs only `amp` on `native` (injected as the decode output) with K = `k`; returns the amp image.
fn run_amp(gpu: &mut Headless, native: &ImageF32, k: i32, params: &Params) -> ImageF32 {
    let mut frame = FrameConstants::new((native.width, native.height), (native.width, native.height), false);
    frame.amp_k = k;
    let settings = Settings::identity();
    let run = StageRun {
        input: StageInput::Image {
            after: "decode",
            image: native,
        },
        frame,
        settings: &settings,
        params,
        format: TargetFormat::Rgba32F,
    };
    let r = gpu.render_stages(&run).expect("render_stages");
    let amp = r.stage("amp").expect("amp image").clone();
    assert_eq!(
        (amp.width, amp.height),
        (native.width * k as u32, native.height),
        "amp image is K x native wide"
    );
    amp
}

/// Oracle: amplifier output of a native `row` (black outside), as the sum of the crt-model Gaussian step
/// responses of its edges, in source px; rising edges at `b` MHz, falling edges at `b / r` (fall time
/// r x rise time); the slower kind delayed by half the difference of the crt-model 10-90 % rise times,
/// so the 10 % points of both kinds coincide (model above). With peaking (D5) each edge response is
/// (1 + g) step(s_e) - g step(s_c), s_c^2 = s_e^2 + s_p^2. [`Oracle::sample`] is the continuous voltage,
/// [`Oracle::box_mean`] the slot's sub-sample value (D6 box integral with crt-model's
/// `gaussian_step_box_mean`, then the D7 range clamp).
struct Oracle {
    f: f64,
    sigma_rise: f64,
    sigma_fall: f64,
    delay_rise: f64,
    delay_fall: f64,
    peaking_gain: f64,
    sigma_peak: f64,
}

impl Oracle {
    fn new(b: f64, f: f64, r: f64) -> Self {
        let px = |ns: f64| ns * f / 1000.0;
        let (rise_ns, fall_ns) = (gaussian_amp_rise_ns(b), gaussian_amp_rise_ns(b / r));
        let fastest = rise_ns.min(fall_ns);
        Self {
            f,
            sigma_rise: px(gaussian_amp_sigma_ns(b)),
            sigma_fall: px(gaussian_amp_sigma_ns(b / r)),
            delay_rise: px((rise_ns - fastest) / 2.0),
            delay_fall: px((fall_ns - fastest) / 2.0),
            peaking_gain: 0.0,
            sigma_peak: 1.0,
        }
    }

    /// The same amplifier with peaking of gain `g` and corner `fp` MHz.
    fn with_peaking(self, g: f64, fp: f64) -> Self {
        Self {
            peaking_gain: g,
            sigma_peak: gaussian_amp_sigma_ns(fp) * self.f / 1000.0,
            ..self
        }
    }

    /// The edges of `row` (black outside): (position, height, sigma, delay), in source px.
    fn edges(&self, row: &[f64]) -> Vec<(f64, f64, f64, f64)> {
        (0..=row.len() as i64)
            .map(|e| (e, level(row, e) - level(row, e - 1)))
            .filter(|&(_, dv)| dv != 0.0)
            .map(|(e, dv)| {
                let (s, d) = if dv < 0.0 {
                    (self.sigma_fall, self.delay_fall)
                } else {
                    (self.sigma_rise, self.delay_rise)
                };
                (e as f64, dv, s, d)
            })
            .collect()
    }

    /// Peaked combination of an edge's unit-step response `resp(sigma)`.
    fn peaked(&self, s: f64, resp: impl Fn(f64) -> f64) -> f64 {
        let g = self.peaking_gain;
        if g == 0.0 {
            return resp(s);
        }
        (1.0 + g) * resp(s) - g * resp((s * s + self.sigma_peak * self.sigma_peak).sqrt())
    }

    /// Continuous amplifier voltage at `x` (no clamp).
    fn sample(&self, row: &[f64], x: f64) -> f64 {
        self.edges(row)
            .into_iter()
            .map(|(e, dv, s, d)| dv * self.peaked(s, |s| phi_cdf((x - e - d) / s)))
            .sum()
    }

    /// Mean of the continuous voltage over [x0, x1) (D6, no clamp).
    fn box_mean_unclamped(&self, row: &[f64], x0: f64, x1: f64) -> f64 {
        self.edges(row)
            .into_iter()
            .map(|(e, dv, s, d)| dv * self.peaked(s, |s| gaussian_step_box_mean(x0 - e - d, x1 - e - d, s)))
            .sum()
    }

    /// Kernel reach in px of the shader: ceil(6 sigma_max + delay_max) + 1, at most 64.
    fn reach(&self) -> i64 {
        let mut s = self.sigma_rise.max(self.sigma_fall);
        if self.peaking_gain != 0.0 {
            s = (s * s + self.sigma_peak * self.sigma_peak).sqrt();
        }
        ((6.0 * s + self.delay_rise.max(self.delay_fall)).ceil() as i64 + 1).min(64)
    }

    /// The slot's sub-sample value over [x0, x1) of pixel floor(x0): D6 box mean, clamped to the range of
    /// the input levels within the reach (D7), widened by g (hi - lo) with peaking.
    fn box_mean(&self, row: &[f64], x0: f64, x1: f64) -> f64 {
        let p = x0.floor() as i64;
        let r = self.reach();
        let levels = (p - r..=p + r).map(|j| level(row, j));
        let lo = levels.clone().fold(f64::MAX, f64::min);
        let hi = levels.fold(f64::MIN, f64::max);
        let margin = self.peaking_gain * (hi - lo);
        self.box_mean_unclamped(row, x0, x1).clamp(lo - margin, hi + margin)
    }
}

/// Level of pixel `j` of `row`, black outside the active line.
fn level(row: &[f64], j: i64) -> f64 {
    if j < 0 || j >= row.len() as i64 {
        0.0
    } else {
        row[j as usize]
    }
}

/// Standard normal CDF with the crt-model erf.
fn phi_cdf(z: f64) -> f64 {
    0.5 * (1.0 + erf(z / std::f64::consts::SQRT_2))
}

/// Sample centre of amp texel `q` in source px.
fn sample_x(q: u32, k: i32) -> f64 {
    (f64::from(q) + 0.5) / f64::from(k)
}

/// Interval [q / K, (q + 1) / K) of amp texel `q` in source px.
fn sub_interval(q: u32, k: i32) -> (f64, f64) {
    (f64::from(q) / f64::from(k), f64::from(q + 1) / f64::from(k))
}

/// Channel `c` of row `y` of an amp image, as f64.
fn amp_row(img: &ImageF32, y: u32, c: usize) -> Vec<f64> {
    (0..img.width).map(|q| f64::from(img.pixel(q, y)[c])).collect()
}

/// Every sample of every channel of `amp` within `tol` of the oracle for the native `rows`.
fn assert_matches_oracle(amp: &ImageF32, rows: &[Vec<[f32; 3]>], k: i32, oracle: &Oracle, tol: f64, what: &str) {
    for (y, row) in rows.iter().enumerate() {
        for c in 0..3 {
            let native: Vec<f64> = row.iter().map(|p| f64::from(p[c])).collect();
            for (q, got) in amp_row(amp, y as u32, c).into_iter().enumerate() {
                let (x0, x1) = sub_interval(q as u32, k);
                let want = oracle.box_mean(&native, x0, x1);
                assert!(
                    (got - want).abs() <= tol,
                    "{what}: row {y} channel {c} sample {q}: GPU {got} vs oracle {want}"
                );
            }
        }
    }
}

/// x positions (source px) where the samples `v` cross `level`, by linear interpolation.
fn crossings(v: &[f64], k: i32, level: f64) -> Vec<f64> {
    v.windows(2)
        .enumerate()
        .filter(|(_, w)| (w[0] - level) * (w[1] - level) < 0.0)
        .map(|(q, w)| {
            let t = (level - w[0]) / (w[1] - w[0]);
            sample_x(q as u32, k) + t / f64::from(k)
        })
        .collect()
}

/// Hand values of D1: sigma_px and the 10-90 % rise in source px.
fn sigma_px_hand(b: f64) -> f64 {
    0.132505 * F_PX / b
}

/// D1: step response. A rising step (row 0) and a falling step (row 1) at x = 8 px, K = 64 so the 10 and
/// 90 % crossings are interpolated finely. The 10-90 % width equals 0.3396 f / B source px (+/- 5 %,
/// PLAN T-33) and the crt-model numerical rise (+/- 2 %); every sample matches the oracle. With r = 2
/// the falling edge is twice as wide and the rising edge unchanged.
#[test]
fn step_rise_time_is_0_3396_f_over_b() {
    let test = "stage_amp step";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.enable_only(&["amp"]).unwrap();
    let k = 64;
    let rising: Vec<f32> = (0..16).map(|j| if j >= 8 { 1.0 } else { 0.0 }).collect();
    let falling: Vec<f32> = rising.iter().map(|v| 1.0 - v).collect();
    let rows = [grey(&rising), grey(&falling)];
    let native = native_image(&rows);
    for (b, r) in [(3.0, 1.0), (5.0, 1.0), (8.0, 1.0), (15.0, 1.0), (5.0, 2.0)] {
        let amp = run_amp(&mut gpu, &native, k, &amp_params(b as f32, F_PX as f32, r as f32));
        let what = format!("{test} B {b} MHz r {r}");
        assert_matches_oracle(&amp, &rows, k, &Oracle::new(b, F_PX, r), 1e-3, &what);
        let hand = 0.339624 * F_PX / b;
        let model = gaussian_amp_rise_ns(b) * F_PX / 1000.0;
        for (y, ratio) in [(0u32, 1.0), (1, r)] {
            let v = amp_row(&amp, y, 1);
            // Only the edge at x = 8 (the blanking edges at 0 and 16 are 8 px away).
            let near = |xs: Vec<f64>| -> f64 {
                let hits: Vec<f64> = xs.into_iter().filter(|x| (x - 8.0).abs() < 4.0).collect();
                assert_eq!(hits.len(), 1, "{what}: one crossing near the edge");
                hits[0]
            };
            let width = (near(crossings(&v, k, 0.9)) - near(crossings(&v, k, 0.1))).abs();
            println!("{what} row {y}: 10-90 % width {width:.5} px, hand {:.5}", hand * ratio);
            assert!(
                (width / (hand * ratio) - 1.0).abs() <= 0.05,
                "{what} row {y}: width {width} px vs {} px",
                hand * ratio
            );
            assert!(
                (width / (model * ratio) - 1.0).abs() <= 0.02,
                "{what} row {y}: width {width} px vs crt-model {} px",
                model * ratio
            );
            // The transition starts (10 % of the way) half the fastest rise time before the edge, for
            // both kinds; its 50 % point lies at the edge plus the delay (rise - fastest) / 2 (0 when
            // r = 1: zero-phase response).
            let fastest = hand * r.min(1.0);
            let start_level = if y == 0 { 0.1 } else { 0.9 };
            let start = near(crossings(&v, k, start_level));
            assert!(
                (start - (8.0 - fastest / 2.0)).abs() < 0.01,
                "{what} row {y}: transition starts at {start}"
            );
            let mid = near(crossings(&v, k, 0.5));
            let delay = (hand * ratio - fastest) / 2.0;
            assert!(
                (mid - (8.0 + delay)).abs() < 0.01,
                "{what} row {y}: 50 % at {mid}, expected {}",
                8.0 + delay
            );
        }
    }
    // The hand sigma of D1 agrees with crt-model.
    for b in [3.0, 4.0, 5.0] {
        let model = gaussian_amp_sigma_ns(b) * F_PX / 1000.0;
        assert!((sigma_px_hand(b) / model - 1.0).abs() < 1e-5, "sigma B {b}: {model}");
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Luminance (1/K) sum of max(v, 0)^gamma over `v`.
fn luminance(v: &[f64], k: i32, gamma: f64) -> f64 {
    v.iter().map(|x| x.max(0.0).powf(gamma)).sum::<f64>() / f64::from(k)
}

/// Continuous oracle of D3: integral of v^gamma for an isolated pixel (on [6, 7) of a black row),
/// crt-model step responses, midpoint rule with 2e-4 px steps over [3, 10] (3 px is more than 12 sigma
/// for the bandwidths used).
fn oracle_line_luminance(b: f64, r: f64, gamma: f64) -> f64 {
    let row = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    let h = 2e-4;
    let n = (7.0 / h) as usize;
    let oracle = Oracle::new(b, F_PX, r);
    (0..n)
        .map(|i| oracle.sample(&row, 3.0 + (i as f64 + 0.5) * h).max(0.0).powf(gamma) * h)
        .sum()
}

/// mu(gamma) of D3: the mean of the distribution with CDF Phi^gamma, integral of (H - Phi^gamma) dz, by
/// the midpoint rule (1e-4 steps over [-9, 9]) with the crt-model erf.
fn mu(gamma: f64) -> f64 {
    let h = 1e-4;
    let n = (18.0 / h) as usize;
    (0..n)
        .map(|i| {
            let z = -9.0 + (i as f64 + 0.5) * h;
            // Clamped: the f64 series can round erf a hair past -1 in the far tail.
            let phi = (0.5 * (1.0 + erf(z / std::f64::consts::SQRT_2))).max(0.0);
            let step = if z > 0.0 { 1.0 } else { 0.0 };
            (step - phi.powf(gamma)) * h
        })
        .sum()
}

/// CPU check of the hand constants of D3 (no GPU): mu(1) = 0, mu(2) = 1/sqrt(pi), mu(3) =
/// 3 / (2 sqrt(pi)), mu(2.4) = 0.6948, and the edge approximation 2 mu(2.4) sigma_px of the dimming.
#[test]
fn hand_constants_of_the_dimming_derivation() {
    let sqrt_pi = std::f64::consts::PI.sqrt();
    assert!(mu(1.0).abs() < 1e-6, "mu(1) = {}", mu(1.0));
    assert!((mu(2.0) - 1.0 / sqrt_pi).abs() < 1e-6, "mu(2) = {}", mu(2.0));
    assert!((mu(3.0) - 1.5 / sqrt_pi).abs() < 1e-6, "mu(3) = {}", mu(3.0));
    let m = mu(GAMMA);
    println!("mu(2.4) = {m:.6}");
    assert!((m - 0.6948).abs() < 1e-4, "mu(2.4) = {m}");
    for (b, edge_approx) in [
        (5.0, 0.1978),
        (4.0, 0.2472),
        (3.0, 0.3296),
        (8.0, 0.1236),
        (15.0, 0.0659),
    ] {
        let d = 2.0 * m * sigma_px_hand(b);
        assert!((d - edge_approx).abs() < 1e-4, "B {b}: 2 mu sigma = {d}");
    }
}

const W: usize = 32;
const LINE: usize = 16;

/// Row 0: an isolated 1-px vertical line (pixel 16 of 32); row 1: a pixel of a horizontal line (every
/// pixel lit). Both at full drive (V = 1).
fn line_rows() -> [Vec<[f32; 3]>; 2] {
    let vertical: Vec<f32> = (0..W).map(|j| if j == LINE { 1.0 } else { 0.0 }).collect();
    [grey(&vertical), grey(&[1.0; W])]
}

/// Luminance of the horizontal-line pixel 16 (its K samples) and of the whole vertical-line row.
fn line_luminances(amp: &ImageF32, k: i32, gamma: f64, c: usize) -> (f64, f64) {
    let vertical = luminance(&amp_row(amp, 0, c), k, gamma);
    let row = amp_row(amp, 1, c);
    let ku = k as usize;
    let horizontal = luminance(&row[LINE * ku..(LINE + 1) * ku], k, gamma);
    (vertical, horizontal)
}

/// D3: with gamma 2.4 and f = 5.37 MHz an isolated 1-px vertical line is dimmer than a 1-px horizontal
/// line; continuous values 19.8 % (B = 5 MHz), 24.7 % (4 MHz), 32.9 % (3 MHz), 12.36 % (8 MHz), 6.59 %
/// (15 MHz): the continuous crt-model integral is within 0.1 point of these hand values. The chain's
/// luminance (box means, then the gun law per sub-sample) is within 0.2 point of the f64 box oracle
/// computed the same way, at K = 4 (contract default), 8 and 16; it is never brighter than the continuous
/// value (Jensen, D3) and exceeds its dimming by at most 2.8 / 1.5 / 0.6 points at K = 4 / 8 / 16 (table
/// of D3, with the margin that covered the former truncation bias); the PLAN +/- 1 point holds at K = 16 for every bandwidth and at K = 8 up to
/// 5 MHz.
#[test]
fn vertical_line_dimming_at_gamma_2_4() {
    let test = "stage_amp vertical line";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.enable_only(&["amp"]).unwrap();
    let rows = line_rows();
    let native = native_image(&rows);
    let vertical: Vec<f64> = rows[0].iter().map(|p| f64::from(p[0])).collect();
    for (b, hand) in [(5.0, 0.198), (4.0, 0.247), (3.0, 0.329), (8.0, 0.1236), (15.0, 0.0659)] {
        let continuous = 1.0 - oracle_line_luminance(b, 1.0, GAMMA);
        println!(
            "{test}: B {b} MHz continuous oracle dimming {:.3} %",
            100.0 * continuous
        );
        assert!(
            (continuous - hand).abs() <= 0.001,
            "B {b}: continuous oracle {continuous} vs hand {hand}"
        );
        let oracle = Oracle::new(b, F_PX, 1.0);
        for (k, excess) in [(4, 0.028), (8, 0.015), (16, 0.006)] {
            let what = format!("{test} B {b} MHz K {k}");
            let amp = run_amp(&mut gpu, &native, k, &amp_params(b as f32, F_PX as f32, 1.0));
            assert_matches_oracle(&amp, &rows, k, &oracle, 1e-3, &what);
            let boxed: Vec<f64> = (0..amp.width)
                .map(|q| {
                    let (x0, x1) = sub_interval(q, k);
                    oracle.box_mean(&vertical, x0, x1)
                })
                .collect();
            let oracle_dim = 1.0 - luminance(&boxed, k, GAMMA);
            for c in 0..3 {
                let (lv, lh) = line_luminances(&amp, k, GAMMA, c);
                assert!((lh - 1.0).abs() <= 1e-3, "{what}: horizontal line pixel {lh}");
                let dim = 1.0 - lv / lh;
                if c == 0 {
                    println!(
                        "{what}: GPU dimming {:.3} %, box oracle {:.3} %, continuous {:.3} %",
                        100.0 * dim,
                        100.0 * oracle_dim,
                        100.0 * continuous
                    );
                }
                assert!(
                    (dim - oracle_dim).abs() <= 0.002,
                    "{what} channel {c}: dimming {dim} vs box oracle {oracle_dim}"
                );
                assert!(
                    dim >= continuous - 0.001 && dim - continuous <= excess,
                    "{what} channel {c}: dimming {dim} vs continuous {continuous}"
                );
                if k == 16 || (k == 8 && b <= 5.0) {
                    assert!(
                        (dim - hand).abs() <= 0.01,
                        "{what} channel {c}: dimming {dim} vs {hand}"
                    );
                }
            }
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// D2: with symmetric slew, at gamma 1 the isolated vertical line integrates to the light of a
/// horizontal-line pixel (+/- 0.5 %, PLAN T-33; observed to FP16 precision), at K = 4 and 16. With
/// asymmetric slew the pulse area is 1 + 1.281552 (s_f - s_r) (hand values of D2, +/- 0.1 %, K = 16).
#[test]
fn gamma_1_vertical_and_horizontal_lines_carry_the_same_light() {
    let test = "stage_amp gamma 1 energy";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.enable_only(&["amp"]).unwrap();
    let native = native_image(&line_rows());
    for b in [5.0, 4.0, 3.0] {
        for k in [4, 16] {
            let amp = run_amp(&mut gpu, &native, k, &amp_params(b, F_PX as f32, 1.0));
            for c in 0..3 {
                let (lv, lh) = line_luminances(&amp, k, 1.0, c);
                let what = format!("{test} B {b} MHz K {k} channel {c}");
                assert!((lh - 1.0).abs() <= 1e-3, "{what}: horizontal {lh}");
                assert!(
                    (lv / lh - 1.0).abs() <= 0.005,
                    "{what}: vertical {lv} vs horizontal {lh}"
                );
                assert!((lv - 1.0).abs() <= 1e-3, "{what}: vertical line area {lv}");
            }
        }
    }
    let k = 16;
    for (r, hand) in [(2.0, 1.227973), (0.5, 0.886014)] {
        let amp = run_amp(&mut gpu, &native, k, &amp_params(4.0, F_PX as f32, r));
        // The same area from crt-model: 1 + (fall - rise) / 2 in px (10-90 % times).
        let model = 1.0 + (gaussian_amp_rise_ns(4.0 / f64::from(r)) - gaussian_amp_rise_ns(4.0)) / 2.0 * F_PX / 1000.0;
        assert!(
            (model / hand - 1.0).abs() <= 1e-5,
            "r {r}: crt-model area {model} vs hand {hand}"
        );
        for c in 0..3 {
            let (lv, _) = line_luminances(&amp, k, 1.0, c);
            println!("{test} r {r} channel {c}: pulse area {lv:.6} (hand {hand})");
            assert!(
                (lv / hand - 1.0).abs() <= 1e-3,
                "{test} r {r} channel {c}: area {lv} vs {hand}"
            );
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// D2: a flat field keeps its level (DC gain 1 +/- 1e-3) away from the line ends, per channel, for
/// symmetric and asymmetric slew; at the line ends the field meets the black blanking through the
/// amplifier's own edge (50 % at the border, as the oracle).
#[test]
fn flat_field_dc_gain_is_one() {
    let test = "stage_amp flat field";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.enable_only(&["amp"]).unwrap();
    let levels: [[f32; 3]; 4] = [[0.0, 0.0, 0.0], [0.2, 0.5, 0.8], [1.0, 1.0, 1.0], [0.75, 0.05, 0.4]];
    let rows: Vec<Vec<[f32; 3]>> = levels.iter().map(|l| vec![*l; 24]).collect();
    let native = native_image(&rows);
    let k = 4;
    for (b, r) in [(3.0, 1.0), (3.0, 2.5), (8.0, 0.5)] {
        let amp = run_amp(&mut gpu, &native, k, &amp_params(b, F_PX as f32, r));
        let what = format!("{test} B {b} MHz r {r}");
        assert_matches_oracle(
            &amp,
            &rows,
            k,
            &Oracle::new(f64::from(b), F_PX, f64::from(r)),
            1e-3,
            &what,
        );
        // Interior: more than 6 sigma_fall + 1 px from both ends (sigma_fall <= 0.593 px here).
        for (y, l) in levels.iter().enumerate() {
            for q in (5 * k as u32)..(19 * k as u32) {
                let got = amp.pixel(q, y as u32);
                for (c, &v) in l.iter().enumerate() {
                    assert!(
                        (got[c] - v).abs() <= 1e-3 * v.max(1e-3),
                        "{what}: level {v} channel {c} sample {q}: {}",
                        got[c]
                    );
                }
            }
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Luminance centroid (source px) of `v` at `gamma`.
fn centroid(v: &[f64], k: i32, gamma: f64) -> f64 {
    let w: Vec<f64> = v.iter().map(|x| x.max(0.0).powf(gamma)).collect();
    let m: f64 = w.iter().enumerate().map(|(q, w)| sample_x(q as u32, k) * w).sum();
    m / w.iter().sum::<f64>()
}

/// D4: rise/fall asymmetry puts the smear on the right of bright detail: the gamma-1 centroid of an
/// isolated pixel moves right by the hand values of D4 (+/- 5 %; r = 2 slow fall, r = 1/2 slow rise),
/// and the gamma-2.4 luminance centroid moves right too; r = 1 keeps it on the pixel centre.
#[test]
fn asymmetric_slew_moves_the_smear_to_the_right() {
    let test = "stage_amp asymmetry";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.enable_only(&["amp"]).unwrap();
    let rows = line_rows();
    let native = native_image(&rows);
    let k = 32;
    let b = 4.0;
    let centre = LINE as f64 + 0.5;
    for (r, hand) in [(2.0, 0.152645), (0.5, 0.043600), (1.0, 0.0)] {
        let amp = run_amp(&mut gpu, &native, k, &amp_params(b as f32, F_PX as f32, r as f32));
        let what = format!("{test} r {r}");
        assert_matches_oracle(&amp, &rows, k, &Oracle::new(b, F_PX, r), 1e-3, &what);
        let v = amp_row(&amp, 0, 0);
        let shift1 = centroid(&v, k, 1.0) - centre;
        let shift_g = centroid(&v, k, GAMMA) - centre;
        println!("{what}: centroid shift gamma 1 {shift1:.5} px (hand {hand}), gamma 2.4 {shift_g:.5} px");
        if r == 1.0 {
            assert!(
                shift1.abs() < 1e-3 && shift_g.abs() < 1e-3,
                "{what}: {shift1}, {shift_g}"
            );
        } else {
            assert!((shift1 - hand).abs() <= 0.05 * hand.abs(), "{what}: {shift1} vs {hand}");
            assert!(shift_g > 0.0, "{what}: gamma 2.4 shift {shift_g}");
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// A row with distinct levels per channel: steps up and down of several sizes, with runs and isolated
/// pixels, compared sample by sample with the oracle at K = 4 (contract default) with asymmetric slew.
#[test]
fn mixed_rows_match_the_oracle() {
    let test = "stage_amp mixed rows";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.enable_only(&["amp"]).unwrap();
    let base = [
        0.0, 1.0, 0.0, 0.5, 0.5, 0.25, 1.0, 1.0, 1.0, 0.0, 0.75, 0.125, 0.0, 0.0, 1.0, 0.5,
    ];
    let row: Vec<[f32; 3]> = base
        .iter()
        .enumerate()
        .map(|(j, &v)| [v, 1.0 - v, base[(j * 5) % base.len()]])
        .collect();
    let rows = vec![row.clone(), row.iter().rev().copied().collect()];
    let native = native_image(&rows);
    for (b, r, k) in [(4.0, 1.7, 4), (6.0, 0.8, 4), (3.0, 1.0, 8)] {
        let amp = run_amp(&mut gpu, &native, k, &amp_params(b, F_PX as f32, r));
        assert_matches_oracle(
            &amp,
            &rows,
            k,
            &Oracle::new(f64::from(b), F_PX, f64::from(r)),
            1e-3,
            &format!("{test} B {b} r {r} K {k}"),
        );
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// D7: a dark pixel in a bright run (1-0-1) at B = 3 MHz, r = 3 (slow fall), K = 16: the unclamped box
/// means overshoot full drive (1.0889, D7), the slot stays within [0, 1] and matches the clamped oracle;
/// the mirror 0-1-0 stays at or above black (unclamped -1.4e-4).
#[test]
fn slew_limiter_stays_within_the_input_range() {
    let test = "stage_amp 1-0-1";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.enable_only(&["amp"]).unwrap();
    let k = 16;
    let dark: Vec<f32> = (0..W).map(|j| if j == LINE { 0.0 } else { 1.0 }).collect();
    let rows = [grey(&dark), line_rows()[0].clone()];
    let native = native_image(&rows);
    let (b, r) = (3.0, 3.0);
    let oracle = Oracle::new(b, F_PX, r);
    let amp = run_amp(&mut gpu, &native, k, &amp_params(b as f32, F_PX as f32, r as f32));
    assert_matches_oracle(&amp, &rows, k, &oracle, 1e-3, test);
    let row: Vec<f64> = dark.iter().map(|&v| f64::from(v)).collect();
    let unclamped = (0..amp.width)
        .map(|q| {
            let (x0, x1) = sub_interval(q, k);
            oracle.box_mean_unclamped(&row, x0, x1)
        })
        .fold(f64::MIN, f64::max);
    println!("{test}: unclamped box maximum {unclamped:.5}");
    assert!(
        (unclamped - 1.0889).abs() <= 1e-3,
        "{test}: unclamped overshoot {unclamped}"
    );
    for y in 0..2 {
        for c in 0..3 {
            let v = amp_row(&amp, y, c);
            let (lo, hi) = v.iter().fold((f64::MAX, f64::MIN), |(a, b), &x| (a.min(x), b.max(x)));
            assert!(lo >= 0.0 && hi <= 1.0, "{test} row {y} channel {c}: range [{lo}, {hi}]");
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Bypass: with the slot off the amp image is the zero-order hold bit for bit (each native texel K
/// times); with the default parameters (PC-monitor bandwidth: edge sigma 0.0071 px, below the 0.01 px of
/// the ideal amplifier) the slot on gives the same exact hold, which keeps the chain's identity tests
/// valid; with a low bandwidth the slot on does filter.
#[test]
fn bypass_and_defaults_are_the_exact_zero_order_hold() {
    let test = "stage_amp bypass";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.enable_only(&["amp"]).unwrap();
    // Exact FP16 values (multiples of 1/64), distinct per texel and channel.
    let rows: Vec<Vec<[f32; 3]>> = (0..3)
        .map(|y| {
            (0..20)
                .map(|x| {
                    let i = (y * 20 + x) as f32;
                    [((i * 7.0) % 64.0) / 64.0, ((i * 13.0) % 64.0) / 64.0, (i % 5.0) / 4.0]
                })
                .collect()
        })
        .collect();
    let native = native_image(&rows);
    let low = amp_params(3.0, F_PX as f32, 1.5);
    for k in [1, 2, 4, 7] {
        let hold = |img: &ImageF32, what: &str| {
            for y in 0..native.height {
                for q in 0..img.width {
                    assert_eq!(
                        img.pixel(q, y),
                        native.pixel(q / k as u32, y),
                        "{test} K {k} {what}: texel ({q}, {y})"
                    );
                }
            }
        };
        gpu.enable_only(&[]).unwrap();
        hold(&run_amp(&mut gpu, &native, k, &low), "slot off");
        gpu.enable_only(&["amp"]).unwrap();
        hold(&run_amp(&mut gpu, &native, k, &Params::default()), "defaults");
        let filtered = run_amp(&mut gpu, &native, k, &low);
        let differs = (0..filtered.width).any(|q| filtered.pixel(q, 0) != native.pixel(q / k as u32, 0));
        assert!(differs, "{test} K {k}: 3 MHz filters");
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// D5: peaking (g = 0.5, f_p = 3.5 MHz, B = 8 MHz, K = 64): every sample matches the oracle; the step
/// overshoots by 7.36 % after the edge and undershoots by as much before it (+/- 0.2 point, the oracle's
/// own maximum within 0.05 point of the hand value); a flat field keeps its level (DC gain 1 +/- 1e-3)
/// and an isolated pixel keeps its area (1 +/- 1e-3).
#[test]
fn peaking_overshoots_and_keeps_dc_and_area() {
    let test = "stage_amp peaking";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.enable_only(&["amp"]).unwrap();
    let k = 64;
    let step: Vec<f32> = (0..16).map(|j| if j >= 8 { 1.0 } else { 0.0 }).collect();
    let pulse: Vec<f32> = (0..16).map(|j| if j == 8 { 1.0 } else { 0.0 }).collect();
    let rows = [grey(&step), grey(&[0.6; 16]), grey(&pulse)];
    let native = native_image(&rows);
    let (b, g, fp) = (8.0, 0.5, 3.5);
    let params = Params {
        amp_peaking_gain: g as f32,
        amp_peaking_mhz: fp as f32,
        ..amp_params(b as f32, F_PX as f32, 1.0)
    };
    let amp = run_amp(&mut gpu, &native, k, &params);
    let oracle = Oracle::new(b, F_PX, 1.0).with_peaking(g, fp);
    assert_matches_oracle(&amp, &rows, k, &oracle, 1e-3, test);

    let hand = 0.073624;
    // The oracle's maximum after the edge on a fine grid (1e-4 px).
    let row: Vec<f64> = step.iter().map(|&v| f64::from(v)).collect();
    let oracle_max = (0..20_000)
        .map(|i| oracle.sample(&row, 8.0 + f64::from(i) * 1e-4))
        .fold(f64::MIN, f64::max);
    assert!(
        (oracle_max - 1.0 - hand).abs() <= 5e-4,
        "{test}: oracle overshoot {}",
        oracle_max - 1.0
    );
    let v = amp_row(&amp, 0, 1);
    let ku = k as usize;
    let after = v[8 * ku..12 * ku].iter().copied().fold(f64::MIN, f64::max) - 1.0;
    let before = -v[4 * ku..8 * ku].iter().copied().fold(f64::MAX, f64::min);
    println!("{test}: overshoot {after:.5}, undershoot {before:.5} (hand {hand})");
    assert!((after - hand).abs() <= 0.002, "{test}: overshoot {after}");
    assert!((before - hand).abs() <= 0.002, "{test}: undershoot {before}");

    for q in (4 * k as u32)..(12 * k as u32) {
        let got = amp.pixel(q, 1)[0];
        assert!((got - 0.6).abs() <= 6e-4, "{test}: flat field sample {q}: {got}");
    }
    // Voltage area, undershoot included (no clamp at 0).
    let area = amp_row(&amp, 2, 0).iter().sum::<f64>() / f64::from(k);
    assert!((area - 1.0).abs() <= 1e-3, "{test}: pulse area {area}");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

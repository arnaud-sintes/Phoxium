//! GPU tests of the beam's horizontal spot, spot growth and convergence hook (T-36b, `beam_eval` in
//! `shaders/include/beam.glsl`), headless, validation on, zero validation messages.
//!
//! Every test injects its own `FrameConstants` and a linear beam-current image after `gun` (amp domain,
//! gun-independent), runs `screen` alone and compares the tube image with the `crt-model` f64 oracle
//! ([`BeamOracle`], [`SpotGrowth`]) and with numbers derived by hand from `CRT.md` (below).
//!
//! Set-up: a native raster of `cols` x `rows` letterboxed into a target of `n` times its size gives a tube
//! of exactly `n` host px per native pixel in both axes; line pitch `n / ppm` mm, so line k is centred at
//! (k + 0.5) n px. With K amp samples per native pixel (`amp_k`), amp sample j covers [j a, (j + 1) a),
//! a = n / K px, and holds its current there (zero-order hold). FWHM = 2 sqrt(2 ln 2) sigma = 2.35482 sigma
//! [K] (CRT.md, Spot profile); sigma_px = FWHM_mm / 2.35482 x ppm.
//!
//! Model (CRT.md "Horizontal versus vertical sharpness" and "Spot growth with beam current"): the beam
//! crossing sample j of line n draws a round Gaussian spot whose FWHM depends on the sample's current I
//! (normalised, 1 = peak drive): FWHM^2 = d0^2 + k I^(2n). It is set by the FWHM at mid drive (drive 0.5
//! through the reference gun law I = V^2.4: I_mid = 0.5^2.4 = 0.189465) and at peak (I = 1) and n:
//! d_peak^2 = d0^2 + k and d_mid^2 = d0^2 + k I_mid^(2n), so k = (d_peak^2 - d_mid^2) / (1 - I_mid^(2n)) and
//! d0^2 = d_peak^2 - k.
//!
//! Hand-derived expectations:
//! - PVM-20M4U defaults (CRT.md Starting presets 0.45 -> 0.8 mm, n = 0.5): I_mid^(2n) = 0.189465,
//!   k = 0.4375 / 0.810535 = 0.539767, d0^2 = 0.100233 (d0 = 0.316596 mm). FWHM at I = 1, 0.5, 0.25:
//!   0.8, sqrt(0.100233 + 0.269883) = 0.608372, sqrt(0.100233 + 0.134942) = 0.484949 mm; at I_mid 0.45.
//!   Ratio peak / mid = 0.8 / 0.45 = 1.7778 (PLAN T-36b: +/- 2 % on isolated lines).
//! - Datasheet fit (CRT.md 0.4, 0.5, 0.6 mm at relative currents 1, 2, 4; PLAN T-36b: n = 0.29, d0 = 0):
//!   relative current r = 4 I, d = 0.4 r^0.29 = 0.4, 0.4 x 2^0.29 = 0.489056, 0.4 x 4^0.29 = 0.597940 mm
//!   (max error to the datasheet 0.0109 mm). Parameters: d_peak = 0.597940, d_mid = 0.4 (4 x 0.189465)^0.29
//!   = 0.4 x 0.757859^0.29 = 0.369097 mm; then d_peak / d_mid = (1 / I_mid)^0.29 exactly, so d0^2 = 0.
//! - Isolated source pixel (K samples of current I on one line, source pixel width W = K a px, pitch P):
//!   its light over the plane is I W P (energy per source pixel conserved: each unit-area spot spreads
//!   the light of its sample, I a P); its centroid is the source pixel centre; the variance of the
//!   column-summed profile is sigma^2 + W^2 / 12 + 1/12 px^2 (Gaussian convolved with the W box, then 1-px
//!   host bins: Sheppard's correction 1/12) and of the row-summed profile sigma^2 + 1/12.
//! - Sinusoidal current along x, one period per source pixel (K samples), DC I0 and depth m, sampled at
//!   the sample centres: the held signal's fundamental is m sinc(pi f a) (sinc x = sin x / x, the
//!   hold), the spot multiplies it by its MTF exp(-2 pi^2 sigma^2 f^2) (CRT.md, Spot profile), the 1-px
//!   area integration by sinc(pi f); harmonics alias onto f only from h = M +/- 1 (M px per period), whose
//!   MTF is below 1e-14 here. With sigma f = sqrt(ln 2 / (2 pi^2)) = 0.187390 the spot MTF is 0.5:
//!   - K = 32, 32 px per period (a = 1): 0.5 x sinc(pi / 32)^2 = 0.5 x 0.998394^2 = 0.498395;
//!   - K = 4, 8 px per period (a = 2): 0.5 x sinc(pi / 4) x sinc(pi / 8) = 0.5 x 0.900316 x 0.974495 =
//!     0.438677 (the 0.5 of the PLAN line holds for the spot alone; the hold and the pixel cost 6 points).
//! - Flat field: the horizontal factor of every interior pixel is exactly 1 (the sample boxes tile the
//!   line), so the mean over a line period equals the current for any spot width, growth or tail.

mod common;

use crt_model::beam::{AmpRaster, BeamOracle, Spot, SpotGrowth, MID_DRIVE_CURRENT};
use crt_model::units::{fwhm_from_sigma, sigma_from_fwhm};
use std::f64::consts::PI;
use ultimatecrt::gpu::{Domain, FrameConstants, Headless, ImageF32, Settings, StageInput, StageRun, TargetFormat};
use ultimatecrt::params::Params;

/// A tube of `cols` x `rows` native pixels at `n` host px per native pixel, `ppm` px per mm, line pitch
/// `n / ppm` mm, `k` amp samples per native pixel.
fn tube(cols: u32, rows: u32, n: u32, ppm: f32, k: i32) -> FrameConstants {
    let mut fc = FrameConstants::new((cols, rows), (cols * n, rows * n), false);
    assert_eq!(fc.tube_size, [(cols * n) as i32, (rows * n) as i32]);
    fc.px_per_mm = ppm;
    fc.line_pitch_mm = n as f32 / ppm;
    fc.amp_k = k;
    fc
}

/// Beam parameters: FWHM at mid and peak (mm), exponent n, tail.
fn beam(mid: f64, peak: f64, n: f64, tail_weight: f64) -> Params {
    Params {
        beam_fwhm_mid_mm: mid as f32,
        beam_fwhm_peak_mm: peak as f32,
        beam_growth_exponent: n as f32,
        beam_tail_weight: tail_weight as f32,
        beam_tail_sigma_ratio: 3.0,
        ..Params::default()
    }
}

/// The spot growth law of `p`, as the oracle sees it.
fn growth(p: &Params) -> SpotGrowth {
    SpotGrowth {
        fwhm_mid: f64::from(p.beam_fwhm_mid_mm),
        fwhm_peak: f64::from(p.beam_fwhm_peak_mm),
        n: f64::from(p.beam_growth_exponent),
    }
}

/// The amp-domain beam-current image of `fc` with value `f(amp column, row)` per channel.
fn currents(fc: &FrameConstants, f: impl Fn(u32, u32) -> [f32; 3]) -> ImageF32 {
    let (w, h) = fc.domain_size(Domain::Amp);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = f(x, y);
            rgba.extend_from_slice(&[r, g, b, 1.0]);
        }
    }
    ImageF32 {
        width: w,
        height: h,
        rgba,
    }
}

/// Runs `screen` alone on `amp` injected after `gun`; returns the tube image.
fn screen(gpu: &mut Headless, fc: FrameConstants, params: &Params, amp: &ImageF32) -> ImageF32 {
    gpu.enable_only(&["screen"]).unwrap();
    let settings = Settings::identity();
    let run = StageRun {
        input: StageInput::Image {
            after: "gun",
            image: amp,
        },
        frame: fc,
        settings: &settings,
        params,
        format: TargetFormat::Rgba32F,
    };
    let r = gpu.render_stages(&run).expect("render_stages");
    r.stage("screen").expect("screen ran").clone()
}

/// Light, centroid (x, y) and variances (x, y) of channel `c` over the window `xs` x `ys` (pixel centres
/// at index + 0.5).
struct Moments {
    energy: f64,
    cx: f64,
    cy: f64,
    vx: f64,
    vy: f64,
}

fn moments(img: &ImageF32, c: usize, xs: std::ops::Range<u32>, ys: std::ops::Range<u32>) -> Moments {
    let (mut e, mut sx, mut sy, mut sxx, mut syy) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for y in ys {
        for x in xs.clone() {
            let v = f64::from(img.pixel(x, y)[c]);
            let (px, py) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
            e += v;
            sx += v * px;
            sy += v * py;
            sxx += v * px * px;
            syy += v * py * py;
        }
    }
    let (cx, cy) = (sx / e, sy / e);
    Moments {
        energy: e,
        cx,
        cy,
        vx: sxx / e - cx * cx,
        vy: syy / e - cy * cy,
    }
}

/// Compares the pixels (`xs` x `ys`) of every channel with the oracle (round spot, growth law of `p`,
/// tail of `p`, no convergence offset). Tolerance: FP16 storage (1e-3 relative), the shader's erf
/// (1.5e-7 absolute per call) within a 2e-4 floor, and the shader's truncation of the sums at 4 sigma of
/// the spot at twice the peak current (the oracle does not truncate): on each side the dropped lines
/// carry at most I_max (P phi(4) / sigma_min + (1 - Phi(4))) and the dropped samples (1 - Phi(4)) I_max
/// times the pixel's vertical factor (bounded by P / (sigma_min sqrt(2 pi)) + 1), phi(4) = 1.338e-4,
/// 1 - Phi(4) = 3.17e-5 [K], sigma_min = the core sigma at zero current (the smallest of a growing spot).
fn assert_matches_oracle(
    what: &str,
    fc: &FrameConstants,
    p: &Params,
    amp: &ImageF32,
    out: &ImageF32,
    xs: &[u32],
    ys: std::ops::Range<u32>,
) {
    let g = growth(p);
    let ppm = f64::from(fc.px_per_mm);
    let sigma = |i: f64| sigma_from_fwhm(g.fwhm(i)) * ppm;
    let spot = Spot {
        sigma_px: &sigma,
        tail_weight: f64::from(p.beam_tail_weight),
        tail_ratio: f64::from(p.beam_tail_sigma_ratio),
    };
    let pitch = f64::from(fc.line_pitch_mm) * ppm;
    let rows = fc.native_size[1] as u32;
    let mult = (f64::from(fc.tube_size[1]) / (pitch * f64::from(rows))).round() as usize;
    let first = 0.5 * (f64::from(fc.tube_size[1]) - (rows as usize * mult) as f64 * pitch) + 0.5 * pitch;
    let a = f64::from(fc.tube_size[0]) / f64::from(amp.width);
    let s_min = sigma(0.0);
    for c in 0..3 {
        let cur: Vec<f64> = amp.rgba.chunks(4).map(|v| f64::from(v[c])).collect();
        let i_max = cur.iter().cloned().fold(0.0, f64::max);
        let oracle = BeamOracle::new(
            AmpRaster {
                currents: &cur,
                samples: amp.width as usize,
                sample_width: a,
                first,
                pitch,
                mult,
            },
            &spot,
        );
        let vertical_max = pitch / (s_min * (2.0 * PI).sqrt()) + 1.0;
        let truncation = 2.0 * i_max * (pitch * 1.338e-4 / s_min + 3.17e-5) + 2.0 * 3.17e-5 * i_max * vertical_max;
        for &x in xs {
            for y in ys.clone() {
                let want = oracle.pixel_mean(
                    (0.0, 0.0),
                    (f64::from(x), f64::from(x + 1)),
                    (f64::from(y), f64::from(y + 1)),
                );
                let got = f64::from(out.pixel(x, y)[c]);
                assert!(
                    (got - want).abs() <= 1e-3 * want.abs() + 2e-4 + truncation,
                    "{what}: pixel ({x}, {y}) channel {c}: GPU {got} vs oracle {want}"
                );
            }
        }
    }
}

const FLAT: [f32; 3] = [1.0, 0.5, 0.25];

/// One lit source pixel (native (7, 5) of 16 x 12, 16 px per native pixel, K = 4, 10 px per mm) with the
/// currents 1, 0.5, 0.25 on R, G, B: per channel the light I x 16 x 16 px (energy per source pixel
/// conserved), the centroid at the pixel centre (120, 88) for every channel (no convergence offset), and
/// the variances sigma^2 + 256/12 + 1/12 (x) and sigma^2 + 1/12 (y) with sigma from the growth law
/// (module header: FWHM 0.8, 0.608372, 0.484949 mm with the defaults) and, growth off, from the mid FWHM;
/// every pixel of the spot's neighbourhood against the oracle.
#[test]
fn isolated_pixel_energy_centroid_and_spot_width() {
    let test = "stage_beam_horizontal isolated pixel";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let fc = tube(16, 12, 16, 10.0, 4);
    let amp = currents(&fc, |x, y| if y == 5 && x / 4 == 7 { FLAT } else { [0.0; 3] });
    let defaults = Params::default();
    assert_eq!(
        (
            defaults.beam_fwhm_mid_mm,
            defaults.beam_fwhm_peak_mm,
            defaults.beam_growth_exponent
        ),
        (0.45, 0.8, 0.5),
        "PVM-20M4U defaults"
    );
    let no_growth = beam(0.45, 0.45, 0.5, 0.0);
    for (name, p, fwhm) in [
        ("growth (defaults)", defaults, [0.8, 0.608_372, 0.484_949]),
        ("no growth", no_growth, [0.45, 0.45, 0.45]),
    ] {
        let out = screen(&mut gpu, fc, &p, &amp);
        assert_eq!((out.width, out.height), (256, 192));
        let what = format!("{test} {name}");
        for (c, &i) in FLAT.iter().enumerate() {
            let m = moments(&out, c, 0..256, 0..192);
            let sigma = fwhm[c] / fwhm_from_sigma(1.0) * 10.0;
            let oracle_fwhm = growth(&p).fwhm(f64::from(i));
            assert!(
                (oracle_fwhm - fwhm[c]).abs() <= 2e-6,
                "{what}: oracle FWHM {oracle_fwhm} vs {}",
                fwhm[c]
            );
            let (want_vx, want_vy) = (sigma * sigma + 256.0 / 12.0 + 1.0 / 12.0, sigma * sigma + 1.0 / 12.0);
            println!(
                "{what}: channel {c} I {i}: energy {:.4} (want {}), centroid ({:.4}, {:.4}), var x {:.4} ({want_vx:.4}), \
                 var y {:.4} ({want_vy:.4})",
                m.energy,
                f64::from(i) * 256.0,
                m.cx,
                m.cy,
                m.vx,
                m.vy
            );
            assert!(
                (m.energy / (f64::from(i) * 256.0) - 1.0).abs() <= 0.002,
                "{what}: channel {c} energy {}",
                m.energy
            );
            assert!(
                (m.cx - 120.0).abs() <= 0.01 && (m.cy - 88.0).abs() <= 0.01,
                "{what}: channel {c} centroid"
            );
            // The spot's own variance (the box and bin terms removed) within 1 %.
            let (sx2, sy2) = (m.vx - 256.0 / 12.0 - 1.0 / 12.0, m.vy - 1.0 / 12.0);
            assert!(
                (sx2 / (sigma * sigma) - 1.0).abs() <= 0.01,
                "{what}: channel {c} sigma_x^2 {sx2} vs {}",
                sigma * sigma
            );
            assert!(
                (sy2 / (sigma * sigma) - 1.0).abs() <= 0.01,
                "{what}: channel {c} sigma_y^2 {sy2} vs {}",
                sigma * sigma
            );
        }
        let xs: Vec<u32> = (96..144).step_by(3).collect();
        assert_matches_oracle(&what, &fc, &p, &amp, &out, &xs, 64..112);
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Measured FWHM (mm) of the vertical profile of host column `x` around a lit line at `centre` px:
/// 2.35482 sqrt(variance - 1/12) / ppm (the line is a delta in y; 1-px bins add 1/12).
fn line_fwhm_mm(img: &ImageF32, c: usize, x: u32, centre: u32, ppm: f64) -> (f64, f64) {
    let m = moments(img, c, x..x + 1, centre - 48..centre + 48);
    (fwhm_from_sigma((m.vy - 1.0 / 12.0).sqrt()) / ppm, m.energy)
}

/// Spot growth on isolated lines with the PVM-20M4U defaults (0.45 -> 0.8 mm, n = 0.5): a line at the mid
/// drive current 0.189465 and one at peak (1.0), far apart; FWHM(peak) / FWHM(mid) = 0.8 / 0.45 = 1.7778
/// +/- 2 % (PLAN T-36b), each width within 1 % of its parameter, each line's light per column I x P.
#[test]
fn spot_growth_ratio_on_isolated_lines() {
    let test = "stage_beam_horizontal growth ratio";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let fc = tube(16, 24, 16, 10.0, 4);
    let mid = MID_DRIVE_CURRENT as f32;
    let amp = currents(&fc, |_, y| match y {
        6 => [mid; 3],
        17 => [1.0; 3],
        _ => [0.0; 3],
    });
    let p = Params::default();
    let out = screen(&mut gpu, fc, &p, &amp);
    for c in 0..3 {
        let (d_mid, e_mid) = line_fwhm_mm(&out, c, 128, 104, 10.0);
        let (d_peak, e_peak) = line_fwhm_mm(&out, c, 128, 280, 10.0);
        let ratio = d_peak / d_mid;
        println!("{test}: channel {c} FWHM mid {d_mid:.5} mm, peak {d_peak:.5} mm, ratio {ratio:.4} (want 1.7778)");
        assert!((ratio / (0.8 / 0.45) - 1.0).abs() <= 0.02, "{test}: ratio {ratio}");
        assert!(
            (d_mid / 0.45 - 1.0).abs() <= 0.01 && (d_peak / 0.8 - 1.0).abs() <= 0.01,
            "{test}: widths"
        );
        assert!(
            (e_mid / (MID_DRIVE_CURRENT * 16.0) - 1.0).abs() <= 0.002,
            "{test}: mid line light {e_mid}"
        );
        assert!((e_peak / 16.0 - 1.0).abs() <= 0.002, "{test}: peak line light {e_peak}");
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// The datasheet fit of PLAN T-36b (module header): n = 0.29, d0 = 0, isolated source pixels at the
/// relative currents 1, 2, 4 (beam currents 0.25, 0.5, 1). Both axes: FWHM = 0.4, 0.489056, 0.597940 mm
/// +/- 1 % (the law) and 0.4, 0.5, 0.6 mm +/- 0.02 mm (the datasheet; max error of the law 0.011 mm).
#[test]
fn spot_growth_reproduces_the_datasheet_fit() {
    let test = "stage_beam_horizontal datasheet fit";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let d_peak = 0.4 * 4.0_f64.powf(0.29);
    let d_mid = 0.4 * (4.0 * MID_DRIVE_CURRENT).powf(0.29);
    assert!(
        (d_peak - 0.597_940).abs() < 1e-6 && (d_mid - 0.369_097).abs() < 1e-6,
        "{d_peak} {d_mid}"
    );
    let p = beam(d_mid, d_peak, 0.29, 0.0);
    let g = growth(&p);
    assert!(g.floor_sq_and_k().0.abs() < 1e-6, "floor {:?}", g.floor_sq_and_k());
    // Source pixels (4, 5), (12, 5), (20, 5) of 24 x 12 at 16 px, 20 px per mm: centres x = 72, 200, 328.
    let ppm = 20.0;
    let fc = tube(24, 12, 16, ppm as f32, 4);
    let amp = currents(&fc, |x, y| match (y, x / 4) {
        (5, 4) => [0.25; 3],
        (5, 12) => [0.5; 3],
        (5, 20) => [1.0; 3],
        _ => [0.0; 3],
    });
    let out = screen(&mut gpu, fc, &p, &amp);
    for (centre, i, model, sheet) in [
        (72, 0.25, 0.4, 0.4),
        (200, 0.5, 0.489_056, 0.5),
        (328, 1.0, 0.597_940, 0.6),
    ] {
        for c in 0..3 {
            let m = moments(&out, c, centre - 56..centre + 56, 28..148);
            let dx = fwhm_from_sigma((m.vx - 256.0 / 12.0 - 1.0 / 12.0).sqrt()) / ppm;
            let dy = fwhm_from_sigma((m.vy - 1.0 / 12.0).sqrt()) / ppm;
            println!("{test}: I {i} channel {c}: FWHM x {dx:.5} y {dy:.5} mm (law {model}, datasheet {sheet})");
            for d in [dx, dy] {
                assert!(
                    (d / model - 1.0).abs() <= 0.01,
                    "{test}: I {i}: FWHM {d} vs law {model}"
                );
                assert!(
                    (d - sheet).abs() <= 0.02,
                    "{test}: I {i}: FWHM {d} vs datasheet {sheet}"
                );
            }
            assert!((g.fwhm(i) - model).abs() < 1e-5, "oracle {} vs {model}", g.fwhm(i));
            assert!(
                (m.energy / (i * 256.0) - 1.0).abs() <= 0.002,
                "{test}: I {i} energy {}",
                m.energy
            );
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Fundamental of row `y` over `periods` periods of `period` px from `x0`, over its mean: A1 / A0.
fn fundamental_over_mean(img: &ImageF32, c: usize, y: u32, x0: u32, period: u32, periods: u32) -> f64 {
    let n = period * periods;
    let (mut re, mut im, mut dc) = (0.0, 0.0, 0.0);
    for k in 0..n {
        let x = x0 + k;
        let v = f64::from(img.pixel(x, y)[c]);
        let phase = 2.0 * PI * (f64::from(x) + 0.5) / f64::from(period);
        re += v * phase.cos();
        im += v * phase.sin();
        dc += v;
    }
    2.0 * re.hypot(im) / dc
}

/// Horizontal response to a sinusoidal current, one period per source pixel, depth 0.5 around 0.5, growth
/// off, spot sigma set for a spot MTF of 0.5 (module header): measured transfer (fundamental over mean,
/// over the depth) = 0.498395 at K = 32 (32 px per period) and 0.438677 at K = 4 (8 px per period), each
/// +/- 0.002; at K = 32 also the PLAN line, exp(-2 pi^2 sigma^2 f^2) = 0.5 +/- 1 point.
#[test]
fn sinusoid_modulation_is_the_gaussian_mtf() {
    let test = "stage_beam_horizontal sinusoid";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let sigma_f = (2.0_f64.ln() / (2.0 * PI * PI)).sqrt();
    assert!((sigma_f - 0.187_390).abs() < 1e-6, "sigma f {sigma_f}");
    let sinc = |x: f64| x.sin() / x;
    for (k, period, want) in [(32, 32u32, 0.498_395), (4, 8, 0.438_677)] {
        let a = f64::from(period) / f64::from(k);
        let f = 1.0 / f64::from(period);
        let exact = 0.5 * sinc(PI * f * a) * sinc(PI * f);
        assert!((exact - want).abs() < 1e-6, "hand value {want} vs {exact}");
        let ppm = 10.0;
        let fwhm = fwhm_from_sigma(sigma_f * f64::from(period)) / ppm;
        let p = beam(fwhm, fwhm, 0.5, 0.0);
        // 32 periods wide (16 interior periods measured from period 8), 2 lines of `period` px.
        let fc = tube(32, 2, period, ppm as f32, k);
        let amp = currents(&fc, |x, _| {
            let v = 0.5 * (1.0 + 0.5 * (2.0 * PI * (f64::from(x) + 0.5) / f64::from(k)).cos());
            [v as f32; 3]
        });
        let out = screen(&mut gpu, fc, &p, &amp);
        let what = format!("{test} K {k}");
        for c in 0..3 {
            let mtf = fundamental_over_mean(&out, c, period / 2, 8 * period, period, 16) / 0.5;
            println!("{what}: channel {c} transfer {mtf:.5} (hand-derived {want}, spot alone 0.5)");
            assert!((mtf - want).abs() <= 0.002, "{what}: transfer {mtf} vs {want}");
            if k == 32 {
                assert!(
                    (mtf - 0.5).abs() <= 0.01,
                    "{what}: transfer {mtf} vs exp(-2 pi^2 sigma^2 f^2) = 0.5"
                );
            }
        }
        let xs: Vec<u32> = (0..4)
            .map(|i| 8 * period + i * period / 4)
            .chain([0, 32 * period - 1])
            .collect();
        assert_matches_oracle(&what, &fc, &p, &amp, &out, &xs, 0..2 * period);
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// A flat field is unchanged by the horizontal spot: with growth on (defaults, per-channel widths for the
/// currents 1, 0.5, 0.25) and with a 10 % tail, at 8 px per line: the mean over 12 interior line periods
/// equals the current +/- 0.5 % and every interior column of a row has the same value (+/- 1e-3 relative);
/// pixels against the oracle, including the left and right ends of the raster.
#[test]
fn flat_field_is_unchanged() {
    let test = "stage_beam_horizontal flat field";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let fc = tube(16, 32, 8, 6.0, 4);
    let amp = currents(&fc, |_, _| FLAT);
    for (name, p) in [("defaults", Params::default()), ("tail", beam(0.45, 0.8, 0.5, 0.1))] {
        let out = screen(&mut gpu, fc, &p, &amp);
        let what = format!("{test} {name}");
        for (c, &i) in FLAT.iter().enumerate() {
            let rows = 80..176;
            let mean = rows.clone().map(|y| f64::from(out.pixel(64, y)[c])).sum::<f64>() / 96.0;
            println!("{what}: channel {c} period mean {mean:.5} (current {i})");
            assert!(
                (mean / f64::from(i) - 1.0).abs() <= 0.005,
                "{what}: channel {c} mean {mean}"
            );
            for y in rows {
                let v0 = f64::from(out.pixel(64, y)[c]);
                for x in 40..88 {
                    let v = f64::from(out.pixel(x, y)[c]);
                    assert!((v - v0).abs() <= 1e-3 * v0 + 1e-6, "{what}: ({x}, {y}) {v} vs {v0}");
                }
            }
        }
        assert_matches_oracle(&what, &fc, &p, &amp, &out, &[0, 1, 2, 64, 125, 126, 127], 0..256);
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Convergence hook off (zero until T-37) is the identity: with the same currents on R, G and B (an
/// arbitrary pattern, growth on, 5 % tail) the three channels of every pixel are equal; with distinct
/// arbitrary currents per channel, every checked pixel matches the oracle with zero offsets. The
/// isolated-pixel test checks that each channel's centroid stays at the source pixel centre.
#[test]
fn convergence_hook_off_is_identity_and_arbitrary_currents_match_the_oracle() {
    let test = "stage_beam_horizontal convergence off";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let fc = tube(6, 10, 6, 1440.0 / 290.0, 4);
    let value = |x: u32, y: u32, k: u32| 0.05 + 0.09 * ((x * k + y * 3 + k) % 11) as f32;
    let p = beam(0.45, 0.8, 0.5, 0.05);
    let same = currents(&fc, |x, y| [value(x, y, 5); 3]);
    let out = screen(&mut gpu, fc, &p, &same);
    for px in out.rgba.chunks(4) {
        assert!(px[0] == px[1] && px[1] == px[2], "{test}: channels differ {px:?}");
    }
    let distinct = currents(&fc, |x, y| [value(x, y, 5), value(x, y, 7), value(x, y, 2)]);
    let out = screen(&mut gpu, fc, &p, &distinct);
    let xs: Vec<u32> = (0..36).collect();
    assert_matches_oracle(test, &fc, &p, &distinct, &out, &xs, 0..60);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Indicative GPU time of `screen` (PLAN gate 10; printed, not asserted): a 256 x 240 raster on 1440 host
/// rows (1536 x 1440 tube, 6 px per line, K = 4, 4.966 px per mm), full white (one run of equal samples
/// per line) and every sample distinct (the worst case: no run to merge, no dark sample to skip),
/// defaults (growth on) without and with a 5 % tail. The GPU may be shared with other work and the
/// validation layer is on, so the numbers are indicative only.
#[test]
fn screen_gpu_time_indicative() {
    let test = "stage_beam_horizontal timing";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    if let Some(why) = gpu.timing_unavailable() {
        eprintln!("{test} skipped: {why}");
        return;
    }
    let counter = gpu.validation_counter();
    let mut fc = FrameConstants::new((256, 240), (1536, 1440), false);
    fc.px_per_mm = 1440.0 / 290.0;
    fc.line_pitch_mm = 290.0 / 240.0;
    let white = currents(&fc, |_, _| [1.0; 3]);
    let v = |x: u32, y: u32, k: u32| 0.2 + 0.05 * ((x * k + y * 3 + k) % 13) as f32;
    let busy = currents(&fc, |x, y| [v(x, y, 5), v(x, y, 7), v(x, y, 2)]);
    for (name, p, amp) in [
        ("white, defaults", Params::default(), &white),
        ("white, tail 5 %", beam(0.45, 0.8, 0.5, 0.05), &white),
        ("every sample distinct, defaults", Params::default(), &busy),
        ("every sample distinct, tail 5 %", beam(0.45, 0.8, 0.5, 0.05), &busy),
    ] {
        let mut ms = Vec::new();
        for _ in 0..5 {
            let out = screen(&mut gpu, fc, &p, amp);
            assert_eq!((out.width, out.height), (1536, 1440));
            if let Some(t) = gpu.last_timings().and_then(|t| t.stage_ms("screen")) {
                ms.push(t);
            }
        }
        ms.sort_by(f64::total_cmp);
        println!(
            "{test} {name} on {}: screen GPU ms per frame (sorted) {ms:.3?}",
            gpu.context().gpu.name
        );
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

//! GPU tests of the beam's vertical scanline profile (T-36a, `beam_eval` in `shaders/include/beam.glsl`),
//! headless, validation on, zero validation messages.
//!
//! Every test injects its own `FrameConstants` (T-30 is not needed) and a beam-current image after `gun`
//! (amp domain, linear values: gun-independent), runs `screen` alone and compares the tube image with the
//! `crt-model` f64 oracle ([`scanline_row_mean`]: every line summed, no truncation) and with numbers
//! derived by hand from `CRT.md`.
//!
//! Set-up: a native raster of `rows` rows letterboxed into a target of `n` times its size gives a tube of
//! exactly `n` host pixels per native row; with line pitch `n / px_per_mm` mm the raster fills it, so line
//! k is centred at (k + 0.5) n px (the raster rule of `beam.glsl`, centred in the tube). The spot is set
//! by its FWHM in mm: FWHM = 2 sqrt(2 ln 2) sigma [K] (CRT.md, Spot profile).
//!
//! Hand-derived expectations (independent of the shader; pixel [a, b) of a flat field of unit current:
//! n / (b - a) sum_k (Phi((b - c_k) / sigma) - Phi((a - c_k) / sigma)), Phi by a series erf):
//! - CRT.md scanline table (point-sampled Michelson): 98, 84, 57, 34, 18, 8.5 % at sigma/P 0.15 to 0.40
//!   (+/- 1.5 points) and 1.4 % at 0.50 (+/- 0.3 points). At 32 px per line, line centres on pixel
//!   boundaries, the area-integrated values are 98.34, 83.47, 57.07, 33.57, 17.70, 8.445 and 1.429 %,
//!   within 0.6 points of the table. At 16 px per line the 0.20 value would be 82.32 % (1.7 points
//!   off), so the table check runs at 32 px per line (">= 16" in PLAN T-36a).
//! - Flat-field mean: each line deposits current x pitch per unit length, so the mean over whole periods
//!   is the current, for any sigma and tail (+/- 0.5 %).
//! - Isolated line: column sum = current x pitch (32 px), centroid at (8 + 0.5) 32 = 272 px, variance
//!   (1 - w) sigma^2 + w (r sigma)^2 + 1/12 px^2 (Sheppard's correction for 1-px bins), FWHM =
//!   2.35482 sigma = FWHM parameter x px per mm (w = 0).
//! - PVM-20M4U 240p on 1440 px (6 px per line, px per mm 1440 / 290, pitch 290 / 240 mm): the default
//!   FWHM 0.45 mm gives sigma/P = 0.45 / 2.35482 / 1.20833 = 0.158150; the six pixel means of a period
//!   are 0.105094, 0.770754, 2.124152, 2.124152, 0.770754, 0.105094, Michelson 90.571 %; at sigma/P 0.25
//!   (CRT.md 240p example) 0.524285, 0.988105, 1.487610 (mirrored), Michelson 47.881 % (57 % point-
//!   sampled: area integration at 6 px per line removes about 9 points).
//!
//! T-36b (horizontal spot, spot growth): every test here runs with growth off (FWHM at peak = FWHM at mid,
//! PLAN T-36a "growth off"), so the hand-derived numbers above are unchanged. The beam now also blurs each
//! line horizontally with the same round spot over the held amp samples, so the oracle is the 2D
//! [`BeamOracle`] (the amp sample j covers [j a, (j + 1) a), a = tube width / amp width) and the checks of
//! x-flat fields read interior columns: the tubes are 8 to 16 native columns wide (was 1 to 3), so that
//! the spot does not reach the raster's left and right ends there (at least 8 sigma away); for an x-flat
//! field the horizontal factor is then exactly 1 and the values are those of T-36a.

mod common;

use crt_model::beam::{AmpRaster, BeamOracle, ScanlineProfile, Spot};
use crt_model::units::fwhm_from_sigma;
use ultimatecrt::gpu::{Domain, FrameConstants, Headless, ImageF32, Settings, StageInput, StageRun, TargetFormat};
use ultimatecrt::params::Params;

/// A tube of `cols` x `rows` native pixels at `n` host pixels per line, `ppm` px per mm, line pitch
/// `n / ppm` mm.
fn tube(cols: u32, rows: u32, n: u32, ppm: f32) -> FrameConstants {
    let mut fc = FrameConstants::new((cols, rows), (cols * n, rows * n), false);
    assert_eq!(fc.tube_size, [(cols * n) as i32, (rows * n) as i32]);
    assert_eq!(fc.tube_rect_px, [0.0, 0.0, (cols * n) as f32, (rows * n) as f32]);
    fc.px_per_mm = ppm;
    fc.line_pitch_mm = n as f32 / ppm;
    fc
}

/// Parameters for a core sigma of `s` line pitches (pitch `pitch_mm`) and a tail, spot growth off.
fn params(s: f64, pitch_mm: f64, tail_weight: f64, tail_ratio: f64) -> Params {
    let fwhm = fwhm_from_sigma(s * pitch_mm) as f32;
    Params {
        beam_fwhm_mid_mm: fwhm,
        beam_fwhm_peak_mm: fwhm,
        beam_tail_weight: tail_weight as f32,
        beam_tail_sigma_ratio: tail_ratio as f32,
        mask_enabled: false,
        ..Params::default()
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

/// Amp column under the centre of host column `x` (image contract, ARCHITECTURE 3.7: the unplaced fallback
/// takes the amp sample under the host pixel centre).
fn amp_col(fc: &FrameConstants, x: u32) -> u32 {
    let (aw, _) = fc.domain_size(Domain::Amp);
    (((f64::from(x) + 0.5) * f64::from(aw) / f64::from(fc.tube_size[0] as u32)).floor() as u32).min(aw - 1)
}

/// Every row of the tube equals the oracle: native rows x `mult` lines of pitch `pitch` px centred from
/// `pitch / 2`, line k drawing native row k / `mult`; along the line the amp samples held over
/// [j a, (j + 1) a), a = tube width / amp width, blurred by the same spot (T-36b, [`BeamOracle`], spot
/// growth off: sigma = `profile.sigma` at every current). Checked on the first and last host columns
/// and on the first host column of the first four, the last four and the middle amp columns (the oracle
/// is slow in debug builds). Tolerance:
/// FP16 storage (relative 2^-11, taken as 1e-3), the shader's erf (absolute 1.5e-7 per call, within the
/// 1e-4 floor) and the shader's truncation of the sums at +/- 4 sigma (PLAN T-36a), which the oracle
/// does not do: the dropped lines on one side carry at most I_max P (phi(4) / sigma + (1 - Phi(4)) / P)
/// with phi(4) = 1.338e-4 and 1 - Phi(4) = 3.17e-5 [K] (sigma = core sigma, conservative when a tail
/// widens the reach), counted twice for both sides; the dropped samples on one side lie beyond 4 sigma
/// of the pixel, so they carry at most (1 - Phi(4)) I_max of the pixel's horizontal factor, times the
/// vertical factor of the row (the row mean of a unit flat field, from the oracle), twice.
fn assert_matches_oracle(
    what: &str,
    fc: &FrameConstants,
    amp: &ImageF32,
    out: &ImageF32,
    profile: &ScanlineProfile,
    mult: u32,
) {
    let rows = fc.native_size[1] as u32;
    let pitch = f64::from(fc.tube_size[1]) / f64::from(rows * mult);
    let samples = amp.width as usize;
    let a = f64::from(fc.tube_size[0]) / f64::from(amp.width);
    let sigma = |_: f64| profile.sigma;
    let spot = Spot {
        sigma_px: &sigma,
        tail_weight: profile.tail_weight,
        tail_ratio: profile.tail_ratio,
    };
    let ones = vec![1.0; samples * rows as usize];
    let unit = BeamOracle::new(
        AmpRaster {
            currents: &ones,
            samples,
            sample_width: a,
            first: pitch / 2.0,
            pitch,
            mult: mult as usize,
        },
        &spot,
    );
    // Amp columns near both ends (where the spot meets the raster's ends) and the middle one.
    let near_end = |k: u32| k < 4 || k + 4 >= amp.width || k == amp.width / 2;
    let checked: Vec<u32> = (0..out.width)
        .filter(|&x| x == 0 || x == out.width - 1 || (amp_col(fc, x) != amp_col(fc, x - 1) && near_end(amp_col(fc, x))))
        .collect();
    // Vertical factor of each row (horizontal factor <= 1 at every column, 1 in the middle).
    let mid = f64::from(out.width / 2);
    let vertical: Vec<f64> = (0..out.height)
        .map(|y| unit.pixel_mean((0.0, 0.0), (mid, mid + 1.0), (f64::from(y), f64::from(y + 1))))
        .collect();
    for c in 0..3 {
        let currents: Vec<f64> = amp.rgba.chunks(4).map(|p| f64::from(p[c])).collect();
        let i_max = currents.iter().cloned().fold(0.0, f64::max);
        let oracle = BeamOracle::new(
            AmpRaster {
                currents: &currents,
                samples,
                sample_width: a,
                first: pitch / 2.0,
                pitch,
                mult: mult as usize,
            },
            &spot,
        );
        let line_truncation = 2.0 * i_max * (pitch * 1.338e-4 / profile.sigma + 3.17e-5);
        for &x in &checked {
            let px = (f64::from(x), f64::from(x + 1));
            for y in 0..out.height {
                let py = (f64::from(y), f64::from(y + 1));
                let want = oracle.pixel_mean((0.0, 0.0), px, py);
                let truncation = line_truncation + 2.0 * 3.17e-5 * i_max * vertical[y as usize].max(1.0);
                let got = f64::from(out.pixel(x, y)[c]);
                assert!(
                    (got - want).abs() <= 1e-3 * want.abs() + 1e-4 + truncation,
                    "{what}: pixel ({x}, {y}) channel {c}: GPU {got} vs oracle {want}"
                );
            }
        }
    }
}

/// Michelson (max - min) / (max + min) of channel `c` of column `x`, host rows `rows`.
fn michelson(img: &ImageF32, x: u32, c: usize, rows: std::ops::Range<u32>) -> f64 {
    let (max, min) = rows.fold((f64::MIN, f64::MAX), |(a, b), y| {
        let v = f64::from(img.pixel(x, y)[c]);
        (a.max(v), b.min(v))
    });
    (max - min) / (max + min)
}

const FLAT: [f32; 3] = [1.0, 0.5, 0.25];

/// CRT.md scanline table at 32 px per line (see the module header for the area-integrated values),
/// every channel (the modulation does not depend on the current), and every pixel against the oracle.
#[test]
fn scanline_michelson_matches_the_crt_md_table() {
    let test = "stage_beam_vertical table";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let fc = tube(8, 16, 32, 4.0);
    let amp = currents(&fc, |_, _| FLAT);
    for (s, table, tol) in [
        (0.15, 0.98, 0.015),
        (0.20, 0.84, 0.015),
        (0.25, 0.57, 0.015),
        (0.30, 0.34, 0.015),
        (0.35, 0.18, 0.015),
        (0.40, 0.085, 0.015),
        (0.50, 0.014, 0.003),
    ] {
        let p = params(s, 8.0, 0.0, 3.0);
        let out = screen(&mut gpu, fc, &p, &amp);
        assert_eq!((out.width, out.height), (256, 512));
        let what = format!("{test} sigma/P {s}");
        for c in 0..3 {
            let m = michelson(&out, 128, c, 128..384);
            println!("{what}: channel {c} Michelson {m:.5} (table {table})");
            assert!((m - table).abs() <= tol, "{what}: channel {c} Michelson {m} vs {table}");
        }
        assert_matches_oracle(&what, &fc, &amp, &out, &ScanlineProfile::gaussian(s * 32.0), 1);
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Flat field at 16 px per line: the mean over 12 interior line periods equals the current within 0.5 %
/// for sigma/P 0.15 to 0.60 by 0.05, without and with a 10 % tail of 3 sigma.
#[test]
fn flat_field_mean_does_not_depend_on_sigma_or_tail() {
    let test = "stage_beam_vertical flat mean";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let fc = tube(16, 32, 16, 4.0);
    let amp = currents(&fc, |_, _| FLAT);
    for (w, r) in [(0.0, 3.0), (0.1, 3.0)] {
        for i in 0..10 {
            let s = 0.15 + 0.05 * f64::from(i);
            let out = screen(&mut gpu, fc, &params(s, 4.0, w, r), &amp);
            let what = format!("{test} sigma/P {s:.2} tail {w}");
            for (c, want) in FLAT.iter().enumerate() {
                let rows = 160..352;
                let n = f64::from(rows.end - rows.start);
                let mean = rows.map(|y| f64::from(out.pixel(128, y)[c])).sum::<f64>() / n;
                let rel = mean / f64::from(*want) - 1.0;
                assert!(rel.abs() <= 0.005, "{what}: channel {c} mean {mean} vs {want}");
            }
            if i % 3 == 0 {
                let profile = ScanlineProfile {
                    sigma: s * 16.0,
                    tail_weight: w,
                    tail_ratio: r,
                };
                assert_matches_oracle(&what, &fc, &amp, &out, &profile, 1);
            }
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// One lit line (native row 8 of 16) at 32 px per line, sigma/P 0.3 (9.6 px): energy, centroid,
/// second moment and FWHM (module header), for tail weights 0, 5 and 10 % (ratio 3).
#[test]
fn isolated_line_energy_position_and_width() {
    let test = "stage_beam_vertical isolated line";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let fc = tube(8, 16, 32, 4.0);
    let amp = currents(&fc, |_, y| if y == 8 { FLAT } else { [0.0; 3] });
    let sigma = 0.3 * 32.0;
    for w in [0.0, 0.05, 0.1] {
        let p = params(0.3, 8.0, w, 3.0);
        let out = screen(&mut gpu, fc, &p, &amp);
        let what = format!("{test} tail {w}");
        for (c, &i) in FLAT.iter().enumerate() {
            let col: Vec<f64> = (0..out.height).map(|y| f64::from(out.pixel(128, y)[c])).collect();
            let energy: f64 = col.iter().sum();
            let centroid = col.iter().enumerate().map(|(y, v)| (y as f64 + 0.5) * v).sum::<f64>() / energy;
            let var = col
                .iter()
                .enumerate()
                .map(|(y, v)| (y as f64 + 0.5 - centroid).powi(2) * v)
                .sum::<f64>()
                / energy;
            let want_var = (1.0 - w) * sigma * sigma + w * (3.0 * sigma).powi(2) + 1.0 / 12.0;
            println!("{what}: channel {c} energy {energy:.4} centroid {centroid:.4} variance {var:.3} ({want_var:.3})");
            let want_energy = f64::from(i) * 32.0;
            assert!(
                (energy / want_energy - 1.0).abs() <= 0.005,
                "{what}: energy {energy} vs {want_energy}"
            );
            assert!((centroid - 272.0).abs() <= 0.01, "{what}: centroid {centroid}");
            assert!(
                (var / want_var - 1.0).abs() <= 0.01,
                "{what}: variance {var} vs {want_var}"
            );
            if w == 0.0 {
                // Half-maximum crossings by linear interpolation between pixel centres.
                let peak = col.iter().cloned().fold(f64::MIN, f64::max);
                let half = peak / 2.0;
                let top = col.iter().position(|&v| v >= half).unwrap();
                let bottom = col.iter().rposition(|&v| v >= half).unwrap();
                let up = top as f64 + 0.5 - (col[top] - half) / (col[top] - col[top - 1]);
                let down = bottom as f64 + 0.5 + (col[bottom] - half) / (col[bottom] - col[bottom + 1]);
                let fwhm = down - up;
                let want = f64::from(p.beam_fwhm_mid_mm) * f64::from(fc.px_per_mm);
                println!(
                    "{what}: channel {c} FWHM {fwhm:.3} px (parameter {want:.3} px, 2.35482 sigma = {:.3})",
                    2.354_82 * sigma
                );
                assert!((fwhm / want - 1.0).abs() <= 0.01, "{what}: FWHM {fwhm} vs {want}");
                assert!((want / (2.354_82 * sigma) - 1.0).abs() <= 1e-4);
            }
        }
        let profile = ScanlineProfile {
            sigma,
            tail_weight: w,
            tail_ratio: 3.0,
        };
        assert_matches_oracle(&what, &fc, &amp, &out, &profile, 1);
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// PVM-20M4U 240p on 1440 px, 6 px per line: flat-field ripple equals the area-integrated value (module
/// header) within 1 point, the period's pixel means within 0.5 %, every pixel against the oracle. The
/// first case uses the default FWHM (0.45 mm, CRT.md preset PVM-20M4U).
#[test]
fn pvm_240p_on_1440_px_ripple_is_area_integrated() {
    let test = "stage_beam_vertical PVM 240p";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let mut fc = tube(4, 240, 6, 1440.0 / 290.0);
    fc.line_pitch_mm = 290.0 / 240.0;
    let pitch_mm = 290.0 / 240.0;
    let amp = currents(&fc, |_, _| FLAT);
    let s_default = f64::from(
        Params {
            mask_enabled: false,
            ..Params::default()
        }
        .beam_fwhm_mid_mm,
    ) / fwhm_from_sigma(1.0)
        / pitch_mm;
    assert!((s_default - 0.158_150).abs() < 1e-6, "default sigma/P {s_default}");
    // The default FWHM at mid drive, growth off (T-36b: FWHM at peak = at mid).
    let default_no_growth = Params {
        beam_fwhm_peak_mm: Params {
            mask_enabled: false,
            ..Params::default()
        }
        .beam_fwhm_mid_mm,
        mask_enabled: false,
        ..Params::default()
    };
    for (s, p, m_want, half) in [
        (s_default, default_no_growth, 0.90571, [0.105_094, 0.770_754, 2.124_152]),
        (
            0.25,
            params(0.25, pitch_mm, 0.0, 3.0),
            0.47881,
            [0.524_285, 0.988_105, 1.487_610],
        ),
    ] {
        let out = screen(&mut gpu, fc, &p, &amp);
        assert_eq!((out.width, out.height), (24, 1440));
        let what = format!("{test} sigma/P {s:.6}");
        for (c, &i) in FLAT.iter().enumerate() {
            let m = michelson(&out, 12, c, 600..840);
            println!("{what}: channel {c} Michelson {m:.5} (hand-derived {m_want})");
            assert!(
                (m - m_want).abs() <= 0.01,
                "{what}: channel {c} Michelson {m} vs {m_want}"
            );
            let want = [half[0], half[1], half[2], half[2], half[1], half[0]];
            for (j, w) in want.iter().enumerate() {
                let got = f64::from(out.pixel(12, 720 + j as u32)[c]) / f64::from(i);
                assert!(
                    (got / w - 1.0).abs() <= 0.005,
                    "{what}: channel {c} pixel {j}: {got} vs {w}"
                );
            }
        }
        assert_matches_oracle(&what, &fc, &amp, &out, &ScanlineProfile::gaussian(s * 6.0), 1);
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Arbitrary linear currents (gun-independent), distinct per amp column, row and channel, with a 5 % tail:
/// every checked pixel matches the 2D oracle (horizontal spot over the amp samples); doubling the currents
/// doubles the light; line multiplier 2 (half the line pitch) draws each native row twice; with the tube
/// not placed (px per mm 0) the beam falls back to the nearest amp sample.
#[test]
fn linear_currents_line_multiplier_and_fallback() {
    let test = "stage_beam_vertical linear";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let fc = tube(3, 12, 8, 4.0);
    let value = |x: u32, y: u32| {
        let v = |k: u32| 0.1 + 0.07 * ((x * k + y * 3 + k) % 11) as f32;
        [v(5), v(7), v(2)]
    };
    let amp = currents(&fc, value);
    let p = params(0.35, 2.0, 0.05, 3.0);
    let profile = ScanlineProfile {
        sigma: 0.35 * 8.0,
        tail_weight: 0.05,
        tail_ratio: 3.0,
    };
    let out = screen(&mut gpu, fc, &p, &amp);
    assert_matches_oracle(test, &fc, &amp, &out, &profile, 1);

    let amp2 = currents(&fc, |x, y| value(x, y).map(|v| 2.0 * v));
    let out2 = screen(&mut gpu, fc, &p, &amp2);
    for (a, b) in out.rgba.chunks(4).zip(out2.rgba.chunks(4)) {
        for c in 0..3 {
            let (a, b) = (a[c], b[c]);
            assert!(
                (b - 2.0 * a).abs() <= 2e-3 * a.abs() + 1e-4,
                "{test}: doubling {a} -> {b}"
            );
        }
    }

    // Line multiplier 2: pitch 4 px in the same tube, 24 lines, line k draws native row k / 2.
    let mut fc_m = fc;
    fc_m.line_pitch_mm = 1.0;
    let out_m = screen(&mut gpu, fc_m, &p, &amp);
    let profile_m = ScanlineProfile {
        sigma: 0.35 * 8.0,
        ..profile
    };
    assert_matches_oracle(&format!("{test} multiplier 2"), &fc_m, &amp, &out_m, &profile_m, 2);

    // Tube not placed: nearest amp sample under the pixel centre, exactly.
    let mut fc_0 = fc;
    fc_0.px_per_mm = 0.0;
    fc_0.line_pitch_mm = 0.0;
    let out_0 = screen(&mut gpu, fc_0, &p, &amp);
    for y in 0..out_0.height {
        for x in 0..out_0.width {
            let want = amp.pixel(amp_col(&fc, x), y / 8);
            let got = out_0.pixel(x, y);
            for c in 0..3 {
                let half = f64::from(want[c]);
                assert!(
                    (f64::from(got[c]) - half).abs() <= 1e-3 * half,
                    "{test}: fallback ({x}, {y})"
                );
            }
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

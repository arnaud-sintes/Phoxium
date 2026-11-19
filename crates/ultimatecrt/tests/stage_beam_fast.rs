//! Cost and accuracy of the standard-quality beam (T-36c, `beam_eval_workgroup` in
//! `shaders/include/beam.glsl`) against the exact reference form (`beam_quality = reference`, T-36a and
//! T-36b) and the `crt-model` f64 oracle ([`BeamOracle`]), headless, validation on, zero validation
//! messages.
//!
//! Standard quality evaluates the spot separably: per workgroup tile, the horizontal profile of every
//! line within reach at every column of the tile (the sum over the amp samples of i X(sigma(i)), exact),
//! then per pixel the vertical sum over those lines with one Gaussian per (line, column) whose variance
//! is the energy-weighted mean of the sample variances (moment matching), plus the second-order term in
//! the spread of those variances (heat equation, see beam.glsl). Exact for a flat stretch of one current
//! and for no spot growth; with growth the error is at the edges between different currents.
//!
//! Tolerance contract (PLAN T-48 quality tiers ask MAE <= 1 % of white between tiers; this is tighter):
//! on every test pattern the standard result is within a mean absolute error of 0.5 % of white and a max
//! absolute error of 4 % of white of the exact form, for any alignment of the pixel grid to the samples and
//! lines (white = current 1, light 1 per unit area of a flat field; ARCHITECTURE 3.7, T-37 review: the T-36c
//! max of 2.5 % held only at the identity alignment). At the identity alignment the tests keep the tighter
//! regression bound [`CONTRACT_MAX`] = 2.5 %; under a geometry they use the contract, [`GEOMETRY_MAX`]. Measured (T-36c, PVM-20M4U growth 0.45 -> 0.8 mm, 6 px per line, K = 4): MAE <= 0.008 % on
//! every pattern; max 0 on flat fields, checker, window and pluge (one current per column), 0.05 % on the
//! ramp, 2.04 % on pixel art (one pixel on the flank of a line where a bright and a dim current share a
//! column; a single moment-matched Gaussian gave 4.7 %); against the oracle on a small pixel-art tube max
//! 0.16 %.

mod common;

use crt_model::beam::{AmpRaster, BeamOracle, Spot, SpotGrowth};
use crt_model::units::sigma_from_fwhm;
use ultimatecrt::gpu::{Domain, FrameConstants, Headless, ImageF32, Settings, StageInput, StageRun, TargetFormat};
use ultimatecrt::params::{
    Params, BEAM_QUALITY_REFERENCE, BEAM_QUALITY_STANDARD, GEOMETRY_CURVATURE_SPHERICAL, MASK_TIER_PREFILTERED,
};
use ultimatecrt::patterns::{generate, Pattern};

/// Tolerance of the standard quality against the exact form (module header), in units of white: the contract's
/// MAE, and the regression bound on the max at the identity alignment (2.04 % measured; the contract's max is
/// [`GEOMETRY_MAX`]).
const CONTRACT_MAE: f64 = 0.005;
const CONTRACT_MAX: f64 = 0.025;

/// Quality tiers measured: (name, setter).
type Quality = (&'static str, fn(&mut Params));

const QUALITIES: [Quality; 2] = [
    ("reference", |p| p.beam_quality = BEAM_QUALITY_REFERENCE),
    ("standard", |p| p.beam_quality = BEAM_QUALITY_STANDARD),
];

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

/// Runs `screen` alone on `amp` injected after `gun`; returns the tube image and the stage's GPU ms.
fn screen(gpu: &mut Headless, fc: FrameConstants, params: &Params, amp: &ImageF32) -> (ImageF32, Option<f64>) {
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
    let out = r.stage("screen").expect("screen ran").clone();
    (out, gpu.last_timings().and_then(|t| t.stage_ms("screen")))
}

/// Beam current of an 8-bit code through the reference gun law I = (v / 255)^2.4 (CRT.md Gun law).
fn gun(v: u8) -> f32 {
    (f32::from(v) / 255.0).powf(2.4)
}

/// A native image (sRGB-like codes per channel) as amp currents: K samples per native pixel.
fn native_currents(fc: &FrameConstants, img: &[[u8; 3]]) -> ImageF32 {
    let w = fc.native_size[0] as u32;
    let k = fc.amp_k as u32;
    currents(fc, |x, y| {
        let c = img[(y * w + x / k) as usize];
        [gun(c[0]), gun(c[1]), gun(c[2])]
    })
}

/// A pattern of the generator (`patterns.rs`) at the native size of `fc`, as codes.
fn pattern(fc: &FrameConstants, p: &Pattern) -> Vec<[u8; 3]> {
    let (w, h) = (fc.native_size[0] as u32, fc.native_size[1] as u32);
    generate(p, w, h, 0).chunks(4).map(|b| [b[2], b[1], b[0]]).collect()
}

fn hash(a: u32, b: u32) -> u32 {
    let mut v = a.wrapping_mul(0x9E37_79B1) ^ b.wrapping_mul(0x85EB_CA77) ^ 0x1234_5678;
    v ^= v >> 15;
    v = v.wrapping_mul(0x2C1B_3C6D);
    v ^= v >> 12;
    v = v.wrapping_mul(0x297A_2D39);
    v ^ (v >> 15)
}

/// A real-like pixel-art scene (stand-in for the T-16 early-look source): a banded sky, a brick ground of
/// 16 px tiles in a 16-colour palette with dark mortar lines, and symmetric 8 x 8 sprites with an outline
/// on some tiles; flat runs, hard edges and many distinct neighbours, as in 1:1 game art.
fn pixel_art(w: u32, h: u32) -> Vec<[u8; 3]> {
    const PAL: [[u8; 3]; 16] = [
        [0, 0, 0],
        [29, 43, 83],
        [126, 37, 83],
        [0, 135, 81],
        [171, 82, 54],
        [95, 87, 79],
        [194, 195, 199],
        [255, 241, 232],
        [255, 0, 77],
        [255, 163, 0],
        [255, 236, 39],
        [0, 228, 54],
        [41, 173, 255],
        [131, 118, 156],
        [255, 119, 168],
        [255, 204, 170],
    ];
    let mut img = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let c = if y < h / 3 {
                // Sky: bands of 8 rows with a 2-px dither at the band edges.
                let band = (y / 8) % 3;
                let dither = y % 8 == 7 && (x + y) % 2 == 0;
                PAL[[1, 12, 6][((band + u32::from(dither)) % 3) as usize]]
            } else {
                let (tx, ty) = (x / 16, y / 16);
                let t = hash(tx, ty);
                let (lx, ly) = (x % 16, y % 16);
                if t.is_multiple_of(5) && (4..12).contains(&lx) && (4..12).contains(&ly) {
                    // Sprite: 4 x 8 random bits mirrored to 8 x 8, outline where it meets the background.
                    let (sx, sy) = (lx - 4, ly - 4);
                    let bit = |sx: u32, sy: u32| (hash(t, sy * 4 + sx.min(7 - sx)) & 1) == 1;
                    if bit(sx, sy) {
                        PAL[[8, 10, 14, 11][(t >> 8) as usize % 4]]
                    } else if (sx > 0 && bit(sx - 1, sy)) || (sx < 7 && bit(sx + 1, sy)) {
                        PAL[0]
                    } else {
                        PAL[[4, 5, 13, 3][(t >> 4) as usize % 4]]
                    }
                } else if ly % 8 == 0 || (lx + 8 * ((ly / 8) % 2)) % 16 == 0 {
                    PAL[5]
                } else {
                    PAL[[4, 5, 13, 3][(t >> 4) as usize % 4]]
                }
            };
            img.push(c);
        }
    }
    img
}

/// The profile tube: `cols` x `rows` native pixels on 1440 host rows at 6 px per line (4.966 px per mm,
/// a 290 mm tall picture), K = 4.
fn profile_tube(cols: u32) -> FrameConstants {
    let rows = 240;
    let mut fc = FrameConstants::new((cols, rows), (cols * 6, rows * 6), false);
    fc.px_per_mm = 1440.0 / 290.0;
    fc.line_pitch_mm = 290.0 / 240.0;
    fc
}

/// The patterns of the profile at the native size of `fc`: (name, amp currents).
fn profile_patterns(fc: &FrameConstants) -> Vec<(&'static str, ImageF32)> {
    let (w, h) = (fc.native_size[0] as u32, fc.native_size[1] as u32);
    let v = |x: u32, y: u32, k: u32| 0.2 + 0.05 * ((x * k + y * 3 + k) % 13) as f32;
    vec![
        ("white", currents(fc, |_, _| [1.0; 3])),
        ("black", currents(fc, |_, _| [0.0; 3])),
        ("ramp", native_currents(fc, &pattern(fc, &Pattern::Ramp))),
        ("checker 1 px", native_currents(fc, &pattern(fc, &Pattern::Checker(1)))),
        ("window 25 %", native_currents(fc, &pattern(fc, &Pattern::Window(25)))),
        ("pluge", native_currents(fc, &pattern(fc, &Pattern::Pluge))),
        ("pixel art", native_currents(fc, &pixel_art(w, h))),
        (
            "every sample distinct",
            currents(fc, |x, y| [v(x, y, 5), v(x, y, 7), v(x, y, 2)]),
        ),
    ]
}

/// Mean and max absolute difference over every pixel and channel of two tube images.
fn errors(a: &ImageF32, b: &ImageF32) -> (f64, f64) {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let (mut sum, mut max, mut n, mut at) = (0.0, 0.0_f64, 0usize, (0, 0.0, 0.0));
    for (k, (pa, pb)) in a.rgba.chunks(4).zip(b.rgba.chunks(4)).enumerate() {
        for c in 0..3 {
            let d = (f64::from(pa[c]) - f64::from(pb[c])).abs();
            sum += d;
            if d > max {
                at = (k * 3 + c, pa[c], pb[c]);
            }
            max = max.max(d);
            n += 1;
        }
    }
    if max > 0.005 {
        let (k, va, vb) = at;
        let px = k / 3;
        println!(
            "  max at ({}, {}) channel {}: {va} vs {vb}",
            px as u32 % a.width,
            px as u32 / a.width,
            k % 3
        );
    }
    (sum / n as f64, max)
}

fn quality(p: &Params, q: u32) -> Params {
    Params { beam_quality: q, ..*p }
}

/// Without spot growth every sample has the same sigma, so the separable form is the exact one: the
/// standard result equals the reference on the busiest pattern (every sample distinct) and on pixel art,
/// with and without a 5 % tail, within the FP16 storage of the stage image (1e-3 relative) and 1e-5.
#[test]
fn standard_is_exact_without_spot_growth() {
    let test = "stage_beam_fast exact without growth";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let fc = profile_tube(64);
    let pats = profile_patterns(&fc);
    for (name, amp) in pats
        .iter()
        .filter(|(n, _)| ["pixel art", "every sample distinct"].contains(n))
    {
        for tail in [0.0, 0.05] {
            let p = Params {
                beam_fwhm_mid_mm: 0.6,
                beam_fwhm_peak_mm: 0.6,
                beam_tail_weight: tail,
                mask_enabled: false,
                ..Params::default()
            };
            let (r, _) = screen(&mut gpu, fc, &quality(&p, BEAM_QUALITY_REFERENCE), amp);
            let (s, _) = screen(&mut gpu, fc, &quality(&p, BEAM_QUALITY_STANDARD), amp);
            let (mae, max) = errors(&r, &s);
            println!("{test}: {name}, tail {tail}: MAE {mae:.2e}, max {max:.2e}");
            for (pr, ps) in r.rgba.chunks(4).zip(s.rgba.chunks(4)) {
                for c in 0..3 {
                    assert!(
                        (pr[c] - ps[c]).abs() <= 1e-3 * pr[c].abs() + 1e-5,
                        "{test}: {name}, tail {tail}: reference {} vs standard {}",
                        pr[c],
                        ps[c]
                    );
                }
            }
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// The tolerance contract against the exact form, at the profile size (1536 x 1440 tube, 6 px per line,
/// K = 4) with the PVM-20M4U spot growth (0.45 -> 0.8 mm), without and with a 5 % tail, on every profile
/// pattern: mean absolute error <= 0.5 % of white, max <= 2.5 % of white (white: current 1, light 1).
#[test]
fn standard_is_within_the_contract_of_the_reference() {
    let test = "stage_beam_fast contract vs reference";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let fc = profile_tube(256);
    for (name, amp) in profile_patterns(&fc) {
        for tail in [0.0, 0.05] {
            let p = Params {
                beam_tail_weight: tail,
                mask_enabled: false,
                ..Params::default()
            };
            let (r, _) = screen(&mut gpu, fc, &quality(&p, BEAM_QUALITY_REFERENCE), &amp);
            let (s, _) = screen(&mut gpu, fc, &quality(&p, BEAM_QUALITY_STANDARD), &amp);
            let (mae, max) = errors(&r, &s);
            println!(
                "{test}: {name}, tail {tail}: MAE {:.4} %, max {:.4} % of white",
                mae * 100.0,
                max * 100.0
            );
            assert!(
                mae <= CONTRACT_MAE && max <= CONTRACT_MAX,
                "{test}: {name}, tail {tail}: MAE {mae}, max {max}"
            );
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// The same contract against the f64 oracle (round spot, growth, tail; it does not truncate) on a small
/// tube, every pixel: pixel art and the 1-px checker (the sharpest current edges), defaults and a 5 % tail.
#[test]
fn standard_is_within_the_contract_of_the_oracle() {
    let test = "stage_beam_fast contract vs oracle";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let rows = 16;
    let mut fc = FrameConstants::new((24, rows), (24 * 6, rows * 6), false);
    fc.px_per_mm = 1440.0 / 290.0;
    fc.line_pitch_mm = 6.0 / fc.px_per_mm;
    let amps = [
        ("pixel art", native_currents(&fc, &pixel_art(24, rows))),
        (
            "checker 1 px",
            native_currents(&fc, &pattern(&fc, &Pattern::Checker(1))),
        ),
    ];
    for (name, amp) in &amps {
        for tail in [0.0, 0.05] {
            let p = Params {
                beam_tail_weight: tail,
                mask_enabled: false,
                ..Params::default()
            };
            let (s, _) = screen(&mut gpu, fc, &quality(&p, BEAM_QUALITY_STANDARD), amp);
            let g = SpotGrowth {
                fwhm_mid: f64::from(p.beam_fwhm_mid_mm),
                fwhm_peak: f64::from(p.beam_fwhm_peak_mm),
                n: f64::from(p.beam_growth_exponent),
            };
            let ppm = f64::from(fc.px_per_mm);
            let sigma = |i: f64| sigma_from_fwhm(g.fwhm(i)) * ppm;
            let spot = Spot {
                sigma_px: &sigma,
                tail_weight: f64::from(p.beam_tail_weight),
                tail_ratio: f64::from(p.beam_tail_sigma_ratio),
            };
            let (mut sum, mut max, mut n) = (0.0, 0.0_f64, 0usize);
            for c in 0..3 {
                let cur: Vec<f64> = amp.rgba.chunks(4).map(|v| f64::from(v[c])).collect();
                let oracle = BeamOracle::new(
                    AmpRaster {
                        currents: &cur,
                        samples: amp.width as usize,
                        sample_width: f64::from(fc.tube_size[0]) / f64::from(amp.width),
                        first: 3.0,
                        pitch: 6.0,
                        mult: 1,
                    },
                    &spot,
                );
                for y in 0..s.height {
                    for x in 0..s.width {
                        let want = oracle.pixel_mean(
                            (0.0, 0.0),
                            (f64::from(x), f64::from(x + 1)),
                            (f64::from(y), f64::from(y + 1)),
                        );
                        let d = (f64::from(s.pixel(x, y)[c]) - want).abs();
                        sum += d;
                        max = max.max(d);
                        n += 1;
                    }
                }
            }
            let mae = sum / n as f64;
            println!(
                "{test}: {name}, tail {tail}: MAE {:.4} %, max {:.4} % of white",
                mae * 100.0,
                max * 100.0
            );
            assert!(
                mae <= CONTRACT_MAE && max <= CONTRACT_MAX,
                "{test}: {name}, tail {tail}: MAE {mae}, max {max}"
            );
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Max error of the standard quality against the exact form for any alignment (T-37, the T-48 contract, ARCHITECTURE
/// 3.7), in units of white. The 2.5 % regression bound (module header) holds with the pixel grid on its identity
/// alignment to the amp samples and lines; overscan moves every pixel footprint against them by a fraction of a pixel, and
/// the worst pixel (on the flank of a line where a bright and a dim current share a column) then reaches
/// 3.22 % (measured, pixel art, overscan 3.1 % x -1.7 %; identity 2.04 %, red dx 0.3 mm 2.22 %; after the T-37
/// review, which keeps the spot's size in mm under overscan: overscan 3.13 %, radial convergence 2 mm 3.52 %).
/// The mean error keeps the contract (0.008 %, PLAN T-48 asks <= 1 %).
const GEOMETRY_MAX: f64 = 0.04;

/// `fc` grown by a raster margin of `m` px per side (T-37: the tube image is the raster plus the margin).
fn with_margin(mut fc: FrameConstants, m: [i32; 2]) -> FrameConstants {
    fc.tube_size = [fc.tube_size[0] + 2 * m[0], fc.tube_size[1] + 2 * m[1]];
    fc.raster_margin = m;
    fc
}

/// Geometry (T-37) on the standard quality. A separable geometry (raster margin, overscan per axis, static
/// misconvergence: per-axis maps, the rows and columns of the raster stay rows and columns) keeps the
/// separable path, and without spot growth it is still the exact form: standard equals reference within the
/// FP16 storage. With the PVM-20M4U growth the mean error stays within the contract and the max within
/// [`GEOMETRY_MAX`] (each separable part alone, then all together). A curved faceplate (or rotation,
/// trapezoid, pincushion) bends the raster's rows and columns across a tile: the standard quality takes the
/// warp path (T-91, `beam_warp.glsl`; until T-91 it fell back to the reference form bit for bit, which this test
/// asserted) and keeps the same contract against the reference (tests/stage_beam_warp.rs for the full matrix).
#[test]
fn standard_follows_the_geometry() {
    let test = "stage_beam_fast geometry";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let fc = with_margin(profile_tube(64), [7, 5]);
    let amp = native_currents(&fc, &pixel_art(64, 240));
    let separable = |p: Params| Params {
        geometry_overscan_x: 0.031,
        geometry_overscan_y: -0.017,
        geometry_conv_red_dx_mm: 0.3,
        geometry_conv_blue_dy_mm: -0.2,
        mask_enabled: false,
        ..p
    };
    let no_growth = separable(Params {
        beam_fwhm_mid_mm: 0.6,
        beam_fwhm_peak_mm: 0.6,
        ..Params::default()
    });
    let (r, _) = screen(&mut gpu, fc, &quality(&no_growth, BEAM_QUALITY_REFERENCE), &amp);
    let (s, _) = screen(&mut gpu, fc, &quality(&no_growth, BEAM_QUALITY_STANDARD), &amp);
    let (mae, max) = errors(&r, &s);
    println!("{test}: separable, no growth: MAE {mae:.2e}, max {max:.2e}");
    for (pr, ps) in r.rgba.chunks(4).zip(s.rgba.chunks(4)) {
        for c in 0..3 {
            assert!(
                (pr[c] - ps[c]).abs() <= 1e-3 * pr[c].abs() + 1e-5,
                "{test}: separable, no growth: reference {} vs standard {}",
                pr[c],
                ps[c]
            );
        }
    }
    // The raster (384 x 1440 px) is narrower than the 4:3 tube: the visible opening is the tube's full width
    // (T-37 review F1), so the side margin columns, inside it, hold the skirt (the geometry reached the
    // separable path); the opening's height is the raster's, so the top margin rows are exactly 0.
    let lit = (396..408).any(|y| s.pixel(3, y)[..3].iter().any(|&v| v > 0.0));
    assert!(lit, "{test}: no light in the left margin");
    assert!(
        (0..5).all(|y| (0..s.width).all(|x| s.pixel(x, y)[..3].iter().all(|&v| v == 0.0))),
        "{test}: light above the opening"
    );

    let growth = separable(Params::default());
    let d = Params {
        mask_enabled: false,
        ..Params::default()
    };
    for (name, p) in [
        ("identity", d),
        (
            "overscan",
            Params {
                geometry_overscan_x: 0.031,
                geometry_overscan_y: -0.017,
                ..d
            },
        ),
        (
            "red dx",
            Params {
                geometry_conv_red_dx_mm: 0.3,
                ..d
            },
        ),
        (
            "blue dy",
            Params {
                geometry_conv_blue_dy_mm: -0.2,
                ..d
            },
        ),
        // The radial convergence term is not checked by geometry_separable (it is an offset, not part of the
        // host -> raster map): the standard path takes it at the column and line centre (T-37 review F5).
        (
            "radial",
            Params {
                geometry_conv_red_radial_mm: 2.0,
                geometry_conv_blue_radial_mm: -1.0,
                ..d
            },
        ),
        ("all", growth),
    ] {
        let (r, _) = screen(&mut gpu, fc, &quality(&p, BEAM_QUALITY_REFERENCE), &amp);
        let (s, _) = screen(&mut gpu, fc, &quality(&p, BEAM_QUALITY_STANDARD), &amp);
        let (mae, max) = errors(&r, &s);
        println!(
            "{test}: separable {name}, growth: MAE {:.4} %, max {:.4} % of white",
            mae * 100.0,
            max * 100.0
        );
        assert!(
            mae <= CONTRACT_MAE && max <= GEOMETRY_MAX,
            "{test}: {name}: MAE {mae}, max {max}"
        );
    }

    for (name, curved) in [
        (
            "spherical",
            Params {
                geometry_curvature: GEOMETRY_CURVATURE_SPHERICAL,
                geometry_eye_distance_mm: 600.0,
                ..growth
            },
        ),
        (
            "pincushion",
            Params {
                geometry_pincushion: 0.02,
                ..growth
            },
        ),
    ] {
        let (r, _) = screen(&mut gpu, fc, &quality(&curved, BEAM_QUALITY_REFERENCE), &amp);
        let (s, _) = screen(&mut gpu, fc, &quality(&curved, BEAM_QUALITY_STANDARD), &amp);
        let (mae, max) = errors(&r, &s);
        println!(
            "{test}: {name}, warp path: MAE {:.4} %, max {:.4} % of white",
            mae * 100.0,
            max * 100.0
        );
        assert!(
            mae <= CONTRACT_MAE && max <= GEOMETRY_MAX && max > 0.0,
            "{test}: {name}: MAE {mae}, max {max}"
        );
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// GPU time of `screen` with the geometry (T-37), pixel art at 1536 x 1440 (256 x 240 native), mask tier C,
/// best / median / worst of `UCRT_PROFILE_FRAMES` frames (default 9), per quality: identity; a separable
/// geometry (raster margin 7 px, overscan, static misconvergence: the standard path still applies); a
/// spherical faceplate (19 in 1R, eye at 600 mm) and a pincushion: the standard quality takes the warp path
/// (T-91; before T-91 it fell back to the reference form). Presets 01 and 05: tests/stage_beam_warp.rs.
/// Printed, not asserted. Run with `cargo test -p ultimatecrt --test stage_beam_fast screen_profile_geometry
/// -- --ignored --nocapture`.
#[test]
#[ignore = "profile: run explicitly, prints a table"]
fn screen_profile_geometry() {
    let test = "stage_beam_fast profile geometry";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    if let Some(why) = gpu.timing_unavailable() {
        eprintln!("{test} skipped: {why}");
        return;
    }
    let counter = gpu.validation_counter();
    let frames: usize = std::env::var("UCRT_PROFILE_FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(9);
    println!(
        "{test} on {}: screen ms min / median / max over {frames} frames",
        gpu.context().gpu.name
    );
    let base = profile_tube(256);
    let amp = native_currents(&base, &pixel_art(256, 240));
    let p = Params {
        mask_enabled: true,
        mask_tier: MASK_TIER_PREFILTERED,
        ..Params::default()
    };
    let cases: [(&str, FrameConstants, Params); 7] = [
        ("identity", base, p),
        ("margin 7 px", with_margin(base, [7, 0]), p),
        (
            "overscan 3 %",
            base,
            Params {
                geometry_overscan_x: 0.03,
                geometry_overscan_y: 0.03,
                ..p
            },
        ),
        (
            "red dx 0.3 mm",
            base,
            Params {
                geometry_conv_red_dx_mm: 0.3,
                ..p
            },
        ),
        (
            "separable (margin, overscan, convergence)",
            with_margin(base, [7, 0]),
            Params {
                geometry_overscan_x: 0.03,
                geometry_overscan_y: 0.03,
                geometry_conv_red_dx_mm: 0.3,
                ..p
            },
        ),
        (
            "pincushion 0.02",
            base,
            Params {
                geometry_pincushion: 0.02,
                ..p
            },
        ),
        (
            "spherical 1R, eye 600 mm",
            base,
            Params {
                geometry_curvature: GEOMETRY_CURVATURE_SPHERICAL,
                geometry_eye_distance_mm: 600.0,
                ..p
            },
        ),
    ];
    // Every variant once per round, `frames` rounds after a warm-up round: a clock change or other work on the
    // shared GPU then hits all variants alike.
    let runs: Vec<(String, FrameConstants, Params)> = cases
        .iter()
        .flat_map(|(name, fc, params)| {
            QUALITIES.iter().map(move |(qname, set)| {
                let mut q = *params;
                set(&mut q);
                (format!("{name} | {qname}"), *fc, q)
            })
        })
        .collect();
    let mut ms: Vec<Vec<f64>> = vec![Vec::new(); runs.len()];
    for round in 0..=frames {
        for (k, (_, fc, q)) in runs.iter().enumerate() {
            if let (Some(t), true) = (screen(&mut gpu, *fc, q, &amp).1, round > 0) {
                ms[k].push(t);
            }
        }
    }
    for ((label, fc, _), mut ms) in runs.into_iter().zip(ms) {
        ms.sort_by(f64::total_cmp);
        if ms.is_empty() {
            continue;
        }
        println!(
            "| {}x{} | {label} | {:.3} | {:.3} | {:.3} |",
            fc.tube_size[0],
            fc.tube_size[1],
            ms[0],
            ms[ms.len() / 2],
            ms[ms.len() - 1]
        );
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// GPU time of `screen` per pattern (best, median and worst of `UCRT_PROFILE_FRAMES` frames, default 9),
/// for each quality tier, without and with a 5 % tail, mask off and mask tier C (prefiltered), at
/// 1536 x 1440 (256 x 240 native) and 1920 x 1440 (320 x 240), and the stage's floor (tube not placed).
/// Printed, not asserted; the GPU may be shared, so take the best. Run with
/// `cargo test -p ultimatecrt --test stage_beam_fast screen_profile -- --ignored --nocapture` (the GPU time
/// does not depend on the Rust build profile); filters: `UCRT_PROFILE_ONLY` (pattern prefixes),
/// `UCRT_PROFILE_QUALITY` (tier names), `UCRT_PROFILE_MASK_C_ONLY`.
#[test]
#[ignore = "profile: run explicitly, prints a table"]
fn screen_profile() {
    let test = "stage_beam_fast profile";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    if let Some(why) = gpu.timing_unavailable() {
        eprintln!("{test} skipped: {why}");
        return;
    }
    let counter = gpu.validation_counter();
    let frames: usize = std::env::var("UCRT_PROFILE_FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(9);
    // Optional filters (comma lists): pattern name prefixes, quality names.
    let only = std::env::var("UCRT_PROFILE_ONLY").unwrap_or_default();
    let tiers = std::env::var("UCRT_PROFILE_QUALITY").unwrap_or_default();
    let masks: &[bool] = if std::env::var("UCRT_PROFILE_MASK_C_ONLY").is_ok() {
        &[true]
    } else {
        &[false, true]
    };
    println!(
        "{test} on {}: screen ms min / median / max over {frames} frames",
        gpu.context().gpu.name
    );
    for cols in [256, 320] {
        let fc = profile_tube(cols);
        // Floor of the stage: tube not placed (beam_eval's nearest-sample fallback), mask tier C.
        let mut unplaced = fc;
        unplaced.px_per_mm = 0.0;
        let p = Params {
            mask_tier: MASK_TIER_PREFILTERED,
            ..Params::default()
        };
        let white = currents(&fc, |_, _| [1.0; 3]);
        let mut ms: Vec<f64> = (0..frames)
            .filter_map(|_| screen(&mut gpu, unplaced, &p, &white).1)
            .collect();
        ms.sort_by(f64::total_cmp);
        if let (Some(lo), Some(hi)) = (ms.first(), ms.last()) {
            println!(
                "| {}x1440 | floor (nearest sample) | - | mask C | - | {lo:.3} | {:.3} | {hi:.3} |",
                cols * 6,
                ms[ms.len() / 2]
            );
        }
        for (pname, amp) in profile_patterns(&fc) {
            if !only.is_empty() && !only.split(',').any(|o| pname.starts_with(o)) {
                continue;
            }
            let configs = [0.0, 0.05].into_iter().flat_map(|t| masks.iter().map(move |&m| (t, m)));
            for (tail, mask) in configs {
                for (qname, set) in QUALITIES {
                    if !tiers.is_empty() && !tiers.split(',').any(|t| t == qname) {
                        continue;
                    }
                    let mut p = Params {
                        beam_tail_weight: tail,
                        mask_enabled: mask,
                        mask_tier: MASK_TIER_PREFILTERED,
                        ..Params::default()
                    };
                    set(&mut p);
                    let _ = screen(&mut gpu, fc, &p, &amp);
                    let mut ms: Vec<f64> = (0..frames).filter_map(|_| screen(&mut gpu, fc, &p, &amp).1).collect();
                    ms.sort_by(f64::total_cmp);
                    if ms.is_empty() {
                        continue;
                    }
                    println!(
                        "| {}x1440 | {pname} | tail {:.0} % | mask {} | {qname} | {:.3} | {:.3} | {:.3} |",
                        cols * 6,
                        tail * 100.0,
                        if mask { "C" } else { "off" },
                        ms[0],
                        ms[ms.len() / 2],
                        ms[ms.len() - 1]
                    );
                }
            }
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

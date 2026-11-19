//! Curved geometry on the separable beam (T-91, warp path of the `screen` slot: `shaders/screen_lines.comp`,
//! `shaders/screen_warp.comp`, `shaders/include/beam_warp.glsl`, `src/gpu/stages/screen.rs`), headless,
//! validation on, zero validation messages. The oracle is the exact per-pixel form (`beam_quality =
//! reference`) with the same geometry; the T-37 geometry tests (`tests/stage_geometry.rs`, G1 to G19) run
//! unchanged on the warp path, which the standard quality now takes for every non-separable geometry.
//!
//! What the warp path computes. light(q) = P sum_n H_n(x0) Y_n(y0, y0 + im_y), (x0, y0) the raster footprint
//! of host pixel q through the full geometry (the reference's footprint), H_n the horizontal energy of line n
//! in a footprint of width im_x (with the sigma moments of the tile path, T-36c), Y_n the vertical integral.
//! Pass 1 evaluates H_n exactly on a grid of raster columns (ox per raster px), pass 2 interpolates it at x0
//! (Catmull-Rom; moments linear) and integrates Y_n exactly. Its error against the reference is the tile
//! path's (spot growth mixing currents in a column, measured max 2.0 to 3.5 % of white, tests/stage_beam_fast.rs)
//! plus the horizontal interpolation, derived below.
//!
//! Hand derivations (an independent model re-derives them; P = 6 px per line, white = current 1, light 1 per
//! unit area of a flat field):
//!
//! W1. Why not a 2D raster-space image. A lit line of sigma s gives the host row [y, y + 1) the light
//!     P (Phi((y + 1 - c) / s) - Phi((y - c) / s)) (Phi the unit normal CDF): 2.66 at the line centre for
//!     s = 0.85 px (P erf(0.5 / (0.85 sqrt 2)) = 6 x 0.4436). Sampled every h raster rows and resampled with a
//!     bilinear filter at an arbitrary offset, the max error over the offset (f64, 64 offsets per step) is 38 %
//!     of white at h = 1, 9.0 % at h = 1/2, 2.5 % at h = 1/4 (s = 1.0: 24.9, 6.0, 1.6 %; s = 1.69: 5.8, 1.4,
//!     0.4 %). The 4 % contract would need 4 times the vertical resolution (4 times the beam cost) for the
//!     sharp PVM spot; the line image keeps the vertical integral exact instead (no vertical sampling at all).
//! W2. Horizontal sampling error. A current step of height 1 at x = 0 gives H(x0) = G(x0 + 1) - G(x0), G the
//!     second antiderivative of the unit Gaussian of sigma s (beam.glsl beam_h: G(u) = u Phi(u / s) + s phi(u /
//!     s)). Sampled every h px and interpolated with Catmull-Rom, the max error over x0 (f64) times the line's
//!     peak row factor P erf(0.5 / (s sqrt 2)) is, for s / h = 1.0, 1.2, 1.6, 2.0: at h = 1, 1.84, 0.92, 0.28,
//!     0.11 % of white; at h = 1/2, 2.11, 1.18, 0.42, 0.18 %; at h = 1/4, 0.91, 0.66, 0.37, 0.22 %. The warp
//!     path takes the smallest ox in {1, 2, 4} with s(peak) ox >= 1.2 (`WARP_SIGMA_STEPS`): at most 1.2 % of
//!     white. With spot growth the error is i times the step error at s(i): PVM-20M4U (0.45 -> 0.8 mm, n 0.5) at
//!     1440 lines, s(1) = 1.687 px, ox 1: worst 0.43 % at i = 0.24 (s 1.011 px); slot TV 26 in (1.0 -> 1.7 mm,
//!     n 0.42), s(1) = 2.625 px: 0.06 %. A flat stretch is exact: the weights sum to 1.
//! W3. Flat field (luminance kept, Lambertian). A flat field of current I gives H = I im_x at every entry, so
//!     the interpolation returns it exactly and the light is I im_x im_y sum_n P Y_n = I / ((1 + ov_x)(1 + ov_y)),
//!     as the reference form: the curvature's local magnification is not applied (ARCHITECTURE 3.7). With a
//!     spot much wider than the line pitch (6 mm FWHM, sigma 5.10 px at 2 px per mm, on 4 px lines: scanline
//!     ripple of relative amplitude 2 exp(-2 pi^2 (5.10 / 4)^2) = 2e-14) and overscan 3 % x 2 %, every pixel
//!     inside the picture is 1 / (1.03 x 1.02) = 0.951837, for a spherical 1R faceplate as for a flat one.
//! W4. Contract (T-48, ARCHITECTURE 3.7): mean absolute error <= 0.5 % of white and max <= 4 % of white
//!     against the reference, every pixel and channel. Tube: 160 x 120 native at 6 px per line, 960 x 720 px at
//!     1440 / 290 px per mm (193.3 x 145 mm, diagonal 241.7 mm); 1R = 1.767 x 241.7 = 427 mm (CRT.md Faceplate
//!     curvature); eye 300 mm is 1.24 diagonals, the ratio of the 19 in tube seen at 600 mm (G4). Geometries:
//!     spherical 1R (eye at infinity, 600 mm, 300 mm), cylindrical 1R, pincushion 0.02, barrel -0.02, trapezoid
//!     0.03, rotation 1.5 deg, and the preset-like cylindrical 1R with convergence 0.075 mm static and 0.275 mm
//!     radial (red out, blue in) plus 2 % overscan, all with the PVM-20M4U spot growth.

mod common;

use ultimatecrt::gpu::{
    warp_plan, Domain, FrameConstants, Headless, ImageF32, Settings, SourceFrame, StageInput, StageRun, TargetFormat,
};
use ultimatecrt::params::{
    Params, BEAM_QUALITY_REFERENCE, BEAM_QUALITY_STANDARD, GEOMETRY_CURVATURE_CYLINDRICAL, GEOMETRY_CURVATURE_FLAT,
    GEOMETRY_CURVATURE_SPHERICAL,
};
use ultimatecrt::patterns::{generate, Pattern};
use ultimatecrt::presets::Library;
use ultimatecrt::scaling::View;

/// Contract of the standard quality against the reference (W4), in units of white.
const CONTRACT_MAE: f64 = 0.005;
const CONTRACT_MAX: f64 = 0.04;

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

/// Beam current of an 8-bit code through the reference gun law I = (v / 255)^2.4 (CRT.md Gun law).
fn gun(v: u8) -> f32 {
    (f32::from(v) / 255.0).powf(2.4)
}

/// A native image (codes per channel) as amp currents: K samples per native pixel.
fn native_currents(fc: &FrameConstants, img: &[[u8; 3]]) -> ImageF32 {
    let w = fc.native_size[0] as u32;
    let k = fc.amp_k as u32;
    currents(fc, |x, y| {
        let c = img[(y * w + x / k) as usize];
        [gun(c[0]), gun(c[1]), gun(c[2])]
    })
}

/// A pattern of the generator (`patterns.rs`) at `w` x `h`, as codes.
fn pattern(w: u32, h: u32, p: &Pattern) -> Vec<[u8; 3]> {
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

/// Pixel art (the scene of tests/stage_beam_fast.rs): banded sky, brick tiles in a 16-colour palette, sprites.
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
                let band = (y / 8) % 3;
                let dither = y % 8 == 7 && (x + y) % 2 == 0;
                PAL[[1, 12, 6][((band + u32::from(dither)) % 3) as usize]]
            } else {
                let (tx, ty) = (x / 16, y / 16);
                let t = hash(tx, ty);
                let (lx, ly) = (x % 16, y % 16);
                if t.is_multiple_of(5) && (4..12).contains(&lx) && (4..12).contains(&ly) {
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

/// Mean and max absolute difference over every pixel and channel of two tube images (prints the worst pixel).
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
    if max > 0.01 {
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

/// The W4 tube: 160 x 120 native at 6 px per line, 1440 / 290 px per mm, K = 4.
fn contract_tube() -> FrameConstants {
    let (cols, rows) = (160, 120);
    let mut fc = FrameConstants::new((cols, rows), (cols * 6, rows * 6), false);
    fc.px_per_mm = 1440.0 / 290.0;
    fc.line_pitch_mm = 6.0 / fc.px_per_mm;
    fc
}

/// 1R of the W4 tube: 1.767 x its diagonal (mm).
fn one_r(fc: &FrameConstants) -> f32 {
    let w = fc.tube_size[0] as f32 / fc.px_per_mm;
    let h = fc.tube_size[1] as f32 / fc.px_per_mm;
    1.767 * w.hypot(h)
}

/// The W4 geometries on the PVM-20M4U spot (parameter defaults), mask off.
fn geometries(fc: &FrameConstants) -> Vec<(&'static str, Params)> {
    let d = Params {
        mask_enabled: false,
        ..Params::default()
    };
    let r = one_r(fc);
    let sphere = |eye: f32| Params {
        geometry_curvature: GEOMETRY_CURVATURE_SPHERICAL,
        geometry_radius_mm: r,
        geometry_eye_distance_mm: eye,
        ..d
    };
    vec![
        ("spherical 1R, eye at infinity", sphere(0.0)),
        ("spherical 1R, eye 600 mm", sphere(600.0)),
        ("spherical 1R, eye 300 mm", sphere(300.0)),
        (
            "cylindrical 1R",
            Params {
                geometry_curvature: GEOMETRY_CURVATURE_CYLINDRICAL,
                geometry_radius_mm: r,
                ..d
            },
        ),
        (
            "pincushion 0.02",
            Params {
                geometry_pincushion: 0.02,
                ..d
            },
        ),
        (
            "barrel -0.02",
            Params {
                geometry_pincushion: -0.02,
                ..d
            },
        ),
        (
            "trapezoid 0.03",
            Params {
                geometry_trapezoid: 0.03,
                ..d
            },
        ),
        (
            "rotation 1.5 deg",
            Params {
                geometry_rotation_deg: 1.5,
                ..d
            },
        ),
        (
            "cylindrical, convergence, overscan",
            Params {
                geometry_curvature: GEOMETRY_CURVATURE_CYLINDRICAL,
                geometry_radius_mm: r,
                geometry_overscan_x: 0.02,
                geometry_overscan_y: 0.02,
                geometry_conv_red_dx_mm: 0.075,
                geometry_conv_blue_dx_mm: -0.075,
                geometry_conv_red_radial_mm: 0.275,
                geometry_conv_blue_radial_mm: -0.275,
                ..d
            },
        ),
    ]
}

/// The W4 patterns: (name, amp currents).
fn contract_patterns(fc: &FrameConstants) -> Vec<(&'static str, ImageF32)> {
    let (w, h) = (fc.native_size[0] as u32, fc.native_size[1] as u32);
    vec![
        ("pixel art", native_currents(fc, &pixel_art(w, h))),
        ("ramp", native_currents(fc, &pattern(w, h, &Pattern::Ramp))),
        (
            "checker 1 px",
            native_currents(fc, &pattern(w, h, &Pattern::Checker(1))),
        ),
        ("flat white (scanlines)", currents(fc, |_, _| [1.0; 3])),
        (
            "columns 1 on 1 off",
            native_currents(fc, &pattern(w, h, &Pattern::Grating(2))),
        ),
        (
            "rows 1 on 1 off",
            native_currents(fc, &pattern(w, h, &Pattern::HLine(2))),
        ),
    ]
}

/// W4: the warp path against the reference on every geometry and pattern: MAE <= 0.5 %, max <= 4 % of white;
/// the warp path is the one that ran (`warp_plan`, ox 1 for this spot) and it is not the reference bit for bit.
#[test]
fn warp_is_within_the_contract_of_the_reference() {
    let test = "stage_beam_warp contract";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let fc = contract_tube();
    assert!((one_r(&fc) - 427.1).abs() < 0.2, "1R {}", one_r(&fc));
    let pats = contract_patterns(&fc);
    let (mut worst_mae, mut worst_max) = (0.0_f64, 0.0_f64);
    for (gname, p) in geometries(&fc) {
        let plan = warp_plan(&fc, &p).unwrap_or_else(|| panic!("{test}: {gname}: no warp plan"));
        assert_eq!(plan.ox, 1, "{test}: {gname}: {plan:?}");
        for (pname, amp) in &pats {
            let (r, _) = screen(&mut gpu, fc, &quality(&p, BEAM_QUALITY_REFERENCE), amp);
            let (s, _) = screen(&mut gpu, fc, &quality(&p, BEAM_QUALITY_STANDARD), amp);
            let (mae, max) = errors(&r, &s);
            println!(
                "{test}: {gname}, {pname}: MAE {:.4} %, max {:.4} % of white",
                mae * 100.0,
                max * 100.0
            );
            assert!(
                mae <= CONTRACT_MAE && max <= CONTRACT_MAX,
                "{test}: {gname}, {pname}: MAE {mae}, max {max}"
            );
            assert!(max > 0.0, "{test}: {gname}, {pname}: identical to the reference");
            worst_mae = worst_mae.max(mae);
            worst_max = worst_max.max(max);
        }
    }
    println!(
        "{test}: worst MAE {:.4} %, worst max {:.4} % of white",
        worst_mae * 100.0,
        worst_max * 100.0
    );
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// The contract with a 5 % beam tail and with the oversampled grid (a spot of 0.6 mm without growth at the
/// W4 scale: sigma 1.27 px, ox 1; at half the px per mm, sigma 0.64 px, ox 2), spherical 1R at 300 mm.
#[test]
fn warp_contract_with_tail_and_oversampling() {
    let test = "stage_beam_warp tail and oversampling";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let fc = contract_tube();
    let mut half = fc;
    half.px_per_mm /= 2.0;
    half.line_pitch_mm = 6.0 / half.px_per_mm;
    let sphere = |fc: &FrameConstants, p: Params| Params {
        geometry_curvature: GEOMETRY_CURVATURE_SPHERICAL,
        geometry_radius_mm: one_r(fc),
        geometry_eye_distance_mm: 300.0 * one_r(fc) / one_r(&contract_tube()),
        mask_enabled: false,
        ..p
    };
    let narrow = Params {
        beam_fwhm_mid_mm: 0.6,
        beam_fwhm_peak_mm: 0.6,
        ..Params::default()
    };
    let cases = [
        (
            "tail 5 %",
            fc,
            sphere(
                &fc,
                Params {
                    beam_tail_weight: 0.05,
                    ..Params::default()
                },
            ),
            1,
        ),
        ("no growth, ox 1", fc, sphere(&fc, narrow), 1),
        ("no growth, ox 2", half, sphere(&half, narrow), 2),
    ];
    for (name, fc, p, ox) in cases {
        assert_eq!(warp_plan(&fc, &p).map(|w| w.ox), Some(ox), "{test}: {name}");
        let (w, h) = (fc.native_size[0] as u32, fc.native_size[1] as u32);
        for (pname, amp) in [
            ("pixel art", native_currents(&fc, &pixel_art(w, h))),
            (
                "checker 1 px",
                native_currents(&fc, &pattern(w, h, &Pattern::Checker(1))),
            ),
        ] {
            let (r, _) = screen(&mut gpu, fc, &quality(&p, BEAM_QUALITY_REFERENCE), &amp);
            let (s, _) = screen(&mut gpu, fc, &quality(&p, BEAM_QUALITY_STANDARD), &amp);
            let (mae, max) = errors(&r, &s);
            println!(
                "{test}: {name}, {pname}: MAE {:.4} %, max {:.4} % of white",
                mae * 100.0,
                max * 100.0
            );
            assert!(
                mae <= CONTRACT_MAE && max <= CONTRACT_MAX,
                "{test}: {name}, {pname}: MAE {mae}, max {max}"
            );
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// W3: a flat field keeps its luminance I / ((1 + ov_x)(1 + ov_y)) on a curved faceplate (Lambertian, no
/// local magnification), on the warp path as on the reference; with the PVM spot (scanlines), the mean over
/// the picture equals the reference's.
#[test]
fn flat_field_keeps_its_luminance() {
    let test = "stage_beam_warp flat field";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    // 64 x 48 native at 4 px per line, 2 px per mm: 256 x 192 px, 128 x 96 mm, 1R = 1.767 x 160 = 282.7 mm.
    let mut fc = FrameConstants::new((64, 48), (256, 192), false);
    fc.px_per_mm = 2.0;
    fc.line_pitch_mm = 2.0;
    let white = currents(&fc, |_, _| [1.0; 3]);
    let broad = |curv: u32| Params {
        beam_fwhm_mid_mm: 6.0,
        beam_fwhm_peak_mm: 6.0,
        geometry_curvature: curv,
        geometry_radius_mm: 282.7,
        geometry_overscan_x: 0.03,
        geometry_overscan_y: 0.02,
        mask_enabled: false,
        ..Params::default()
    };
    let want: f64 = 1.0 / (1.03 * 1.02);
    assert!((want - 0.951_837).abs() < 1e-6);
    for (name, curv) in [
        ("flat", GEOMETRY_CURVATURE_FLAT),
        ("spherical", GEOMETRY_CURVATURE_SPHERICAL),
    ] {
        for q in [BEAM_QUALITY_STANDARD, BEAM_QUALITY_REFERENCE] {
            let p = quality(&broad(curv), q);
            assert_eq!(
                warp_plan(&fc, &p).is_some(),
                curv != GEOMETRY_CURVATURE_FLAT && q == BEAM_QUALITY_STANDARD
            );
            let (out, _) = screen(&mut gpu, fc, &p, &white);
            // Inside the picture, 40 px (about 8 sigma) from the tube image's edges; the curved raster's edge is
            // seen at most about 3 px inward (corner of this 1R tube).
            let mut worst = 0.0_f64;
            for y in 40..152 {
                for x in 40..216 {
                    let v = f64::from(out.pixel(x, y)[1]);
                    worst = worst.max((v / want - 1.0).abs());
                }
            }
            println!("{test}: broad spot, {name}, quality {q}: worst relative deviation from {want:.6}: {worst:.2e}");
            assert!(worst <= 1e-3, "{test}: {name}, quality {q}: {worst}");
        }
    }
    // PVM spot, scanlines visible: the picture's mean luminance is the reference's.
    let fc = contract_tube();
    let white = currents(&fc, |_, _| [1.0; 3]);
    let p = Params {
        geometry_curvature: GEOMETRY_CURVATURE_SPHERICAL,
        geometry_radius_mm: one_r(&fc),
        geometry_eye_distance_mm: 300.0,
        mask_enabled: false,
        ..Params::default()
    };
    let mean = |img: &ImageF32| img.rgba.chunks(4).map(|v| f64::from(v[1])).sum::<f64>() / img.rgba.len() as f64 * 4.0;
    let (r, _) = screen(&mut gpu, fc, &quality(&p, BEAM_QUALITY_REFERENCE), &white);
    let (s, _) = screen(&mut gpu, fc, &quality(&p, BEAM_QUALITY_STANDARD), &white);
    let (mr, ms) = (mean(&r), mean(&s));
    println!("{test}: PVM spot, spherical 1R: mean luminance reference {mr:.6}, warp {ms:.6}");
    assert!((ms / mr - 1.0).abs() <= 1e-3, "{test}: {ms} vs {mr}");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// The identity and the separable geometries never take the warp path (bit-exact `screen.comp`, the T-36c
/// and T-37 tests); a spot under 0.3 raster px keeps the reference form (bit for bit).
#[test]
fn warp_path_is_chosen_only_where_it_applies() {
    let test = "stage_beam_warp choice";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let mut fc = FrameConstants::new((64, 48), (256, 192), false);
    fc.px_per_mm = 2.0;
    fc.line_pitch_mm = 2.0;
    let amp = native_currents(&fc, &pixel_art(64, 48));
    // FWHM 0.12 mm at 2 px per mm: sigma 0.102 px, 0.41 grid steps at ox 4: reference form.
    let tiny = Params {
        beam_fwhm_mid_mm: 0.12,
        beam_fwhm_peak_mm: 0.12,
        geometry_pincushion: 0.02,
        mask_enabled: false,
        ..Params::default()
    };
    assert_eq!(warp_plan(&fc, &tiny), None);
    let (r, _) = screen(&mut gpu, fc, &quality(&tiny, BEAM_QUALITY_REFERENCE), &amp);
    let (s, _) = screen(&mut gpu, fc, &quality(&tiny, BEAM_QUALITY_STANDARD), &amp);
    assert_eq!(r.rgba, s.rgba, "{test}: tiny spot keeps the reference form");
    let separable = Params {
        geometry_overscan_x: 0.03,
        geometry_conv_red_radial_mm: 0.5,
        mask_enabled: false,
        ..Params::default()
    };
    assert_eq!(warp_plan(&fc, &separable), None);
    assert_eq!(warp_plan(&fc, &Params::default()), None);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Frames in flight share the stage's line images: a burst of overlapping frames through the whole chain that
/// stays on the warp path, leaves it (flat faceplate: the images are freed) and comes back (reallocated, new
/// layout transition), under synchronization validation; the last frame equals a single render of the same
/// parameters on a fresh renderer, bit for bit (every entry is rewritten each frame).
#[test]
fn warp_frames_in_flight() {
    let test = "stage_beam_warp frames in flight";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let (w, h) = (160_u32, 120_u32);
    let bgra = generate(&Pattern::Checker(1), w, h, 0);
    let target = (640, 480);
    let curved = Params {
        geometry_curvature: GEOMETRY_CURVATURE_SPHERICAL,
        geometry_radius_mm: 400.0,
        geometry_eye_distance_mm: 300.0,
        ..Params::default()
    };
    let flat = Params {
        geometry_curvature: GEOMETRY_CURVATURE_FLAT,
        ..curved
    };
    let source = |first: bool| {
        first.then_some(SourceFrame {
            width: w,
            height: h,
            bgra: &bgra,
        })
    };
    let seq = [&curved, &curved, &curved, &flat, &flat, &curved, &curved];
    let frames: Vec<(Option<SourceFrame<'_>>, View, &Params)> = seq
        .iter()
        .enumerate()
        .map(|(i, p)| (source(i == 0), View::full(w, h), *p))
        .collect();
    let got = gpu
        .render_burst_params(&frames, &Settings::default(), target, TargetFormat::Rgba32F)
        .expect("burst");
    let mut fresh = common::gpu_or_skip(test).expect("device");
    let want = fresh
        .render(
            source(true),
            View::full(w, h),
            &Settings::default(),
            &curved,
            target,
            TargetFormat::Rgba32F,
        )
        .expect("single");
    assert!(
        got.output.rgba == want.output.rgba,
        "{test}: the burst's last frame differs from a single render"
    );
    let lit = got.output.rgba.chunks(4).filter(|p| p[1] > 0.0).count();
    assert!(lit > 1000, "{test}: picture is black");
    drop(fresh);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// GPU time of `screen` (beam + warp, best of `UCRT_PROFILE_FRAMES` frames, default 7, interleaved rounds on a
/// shared GPU) for presets 01 (pvm-20m4u, cylindrical) and 05 (slot-tv-26in, spherical) as shipped
/// (`Library::embedded`), on 256 x 240 and 320 x 240 sources (tubes 1536 x 1440 and 1920 x 1440 in a
/// 1920 x 1440 target, PAR 1) with the `ramp` and `checker:1` patterns through `amp` and `gun` (the preset's
/// K = 16 and bandwidth): flat (`geometry_curvature = flat`), curved on the standard quality (the warp path),
/// curved on the reference quality (the T-47 cost of the curved standard path, which fell back to it). Prints
/// the table and the error of the warp path against the reference, asserts the cost contract curved <= 1.3 x
/// flat. Run with `cargo test -p ultimatecrt --test stage_beam_warp screen_profile_warp -- --ignored
/// --nocapture`.
#[test]
#[ignore = "profile: run explicitly, prints a table"]
fn screen_profile_warp() {
    let test = "stage_beam_warp profile";
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
        .unwrap_or(7);
    println!(
        "{test} on {}: screen ms, best of {frames} frames",
        gpu.context().gpu.name
    );
    let lib = Library::embedded();
    let target = (1920, 1440);
    let mut worst_ratio = 0.0_f64;
    for id in ["pvm-20m4u", "slot-tv-26in"] {
        let preset = lib.find(id).expect("preset");
        let curved = preset.params();
        let flat = Params {
            geometry_curvature: GEOMETRY_CURVATURE_FLAT,
            ..curved
        };
        for cols in [256_u32, 320] {
            let native = (cols, 240);
            let fc = FrameConstants::placed(native, target, false, &curved).with_skirt_margin(&curved);
            let plan = warp_plan(&fc, &curved);
            for (pname, pat) in [("ramp", Pattern::Ramp), ("checker:1", Pattern::Checker(1))] {
                // Drive voltage V = code / 255 injected after decode; amp and gun run as in the chain.
                let codes = pattern(cols, 240, &pat);
                let mut rgba = Vec::with_capacity(codes.len() * 4);
                for c in &codes {
                    rgba.extend(c.iter().map(|&v| f32::from(v) / 255.0));
                    rgba.push(1.0);
                }
                let native_img = ImageF32 {
                    width: cols,
                    height: 240,
                    rgba,
                };
                let variants = [
                    ("flat standard", flat),
                    ("curved standard (warp)", curved),
                    ("curved reference (before)", quality(&curved, BEAM_QUALITY_REFERENCE)),
                ];
                let mut ms = vec![Vec::new(); variants.len()];
                let mut images = Vec::new();
                for round in 0..=frames {
                    for (k, (_, p)) in variants.iter().enumerate() {
                        gpu.enable_only(&["amp", "gun", "screen"]).unwrap();
                        let settings = Settings::identity();
                        let run = StageRun {
                            input: StageInput::Image {
                                after: "decode",
                                image: &native_img,
                            },
                            frame: fc,
                            settings: &settings,
                            params: p,
                            format: TargetFormat::Rgba32F,
                        };
                        let r = gpu.render_stages(&run).expect("render_stages");
                        if round == 0 && k > 0 {
                            images.push(r.stage("screen").expect("screen").clone());
                        }
                        if let (Some(t), true) = (gpu.last_timings().and_then(|t| t.stage_ms("screen")), round > 0) {
                            ms[k].push(t);
                        }
                    }
                }
                let best: Vec<f64> = ms
                    .iter()
                    .map(|v| v.iter().copied().fold(f64::INFINITY, f64::min))
                    .collect();
                let (mae, max) = errors(&images[1], &images[0]);
                let ratio = best[1] / best[0];
                worst_ratio = worst_ratio.max(ratio);
                println!(
                    "| {id} | {}x{} | {pname} | flat {:.3} | warp {:.3} | reference {:.3} | warp / flat {ratio:.2} | \
                     reference / flat {:.2} | ox {:?} | warp vs reference MAE {:.4} %, max {:.3} % |",
                    fc.tube_size[0],
                    fc.tube_size[1],
                    best[0],
                    best[1],
                    best[2],
                    best[2] / best[0],
                    plan.map(|w| w.ox),
                    mae * 100.0,
                    max * 100.0
                );
                assert!(
                    mae <= CONTRACT_MAE && max <= CONTRACT_MAX,
                    "{test}: {id} {pname}: {mae} {max}"
                );
            }
        }
    }
    println!("{test}: worst warp / flat {worst_ratio:.2}");
    assert!(worst_ratio <= 1.3, "{test}: curved costs {worst_ratio:.2} x flat");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

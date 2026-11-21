//! Tests of source decode, raster and tube placement (T-30: SRC-3, SRC-4, SRC-5, OUT-7, PHY-3).
//!
//! GPU tests (headless, validation layer, zero messages) for `decode`, and pure CPU tests of the placement
//! maths against the `crt-model` oracle (`tube`, `geometry`). Expected numbers are derived by hand in the
//! oracle's tests and here.
//!
//! # Decode (SRC-5)
//!
//! The native raster holds V, the drive voltage (contract of `decode.comp`, in [0, 1]); since T-34 the gun
//! law is applied by the `gun` slot, whose image holds the beam current T(V) (default: pure power 2.4):
//! - `drive_voltage`: V = code / 255, exact to FP16 precision. Stores round to nearest since T-fp16
//!   (`round_f16`), so the relative error is below 2^-11 = 4.9e-4 (see `gpu_identity.rs`); the `gun` image
//!   V^2.4 has the error of V amplified by 2.4 plus its own rounding, (2.4 + 1) 2^-11 = 1.7e-3.
//! - `srgb`: V = srgb_eotf(code / 255)^(1 / gun_gamma), so the pure power gun gives T(V) =
//!   srgb_eotf(code / 255): the `gun` image holds the sRGB linear light. Code 20 at gamma 2.4:
//!   u = (20 / 255 + 0.055) / 1.055 = 0.126475, L = u^2.4 = 0.006995 (the plan's value, +-2 %),
//!   V = u = 0.126475. The pure 2.4 law on the code itself would give 0.00222.
//!
//! # Placement (OUT-7, PHY-3)
//!
//! PVM-20M4U class tube, 290 mm high, 4:3 (386.667 mm wide), 2560 x 1440 output, raster filling the tube:
//! 1440 / 290 = 4.9655 px/mm; 1920 px wide; 240 lines at 290 / 240 = 1.2083 mm = 6.000 px; 0.31 mm triads
//! 1.5393 px (derived from CRT.md's 1245 triads (rounded 386 mm width) = 1.542; exact value 1.5393 for 4:3 on 290 mm); 1247.31 triads over 256 source
//! columns = 4.872 triads per source pixel.

mod common;

use crt_model::colour::srgb_eotf;
use crt_model::geometry::{px_per_triad, triads_across, triads_per_source_px};
use crt_model::tube;
use phoxium::gpu::{FrameConstants, Headless, Rendered, Settings, SourceFrame, TargetFormat, Transfer, AMP_K};
use phoxium::params::{ParamValue, Params, RASTER_ASPECT_FILL_TUBE, SOURCE_EOTF_SRGB};
use phoxium::placement::{place, HostFit, PlacementInput, RasterAspect};
use phoxium::scaling::View;

const W: u32 = 256;
const H: u32 = 4;

/// FP16 relative rounding error bound, 2^-11 (half an ulp; stores round to nearest since T-fp16).
const FP16_REL: f64 = 1.0 / 2048.0;
/// Half the FP16 subnormal step, 2^-25: the absolute error below the smallest normal value.
const FP16_SUBNORMAL_STEP: f64 = 2.9802322387695312e-8;

/// A `w` x `h` BGRA8 image where pixel (x, y) has code `x % 256` in red, `255 - x % 256` in green and
/// `(x + 64 y) % 256` in blue.
fn ramp(w: u32, h: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let (r, g, b) = ((x % 256) as u8, (255 - x % 256) as u8, ((x + 64 * y) % 256) as u8);
            v.extend_from_slice(&[b, g, r, 255]);
        }
    }
    v
}

/// The code of channel `c` of pixel (x, y) of [`ramp`].
fn code(c: usize, x: u32, y: u32) -> u8 {
    [(x % 256) as u8, (255 - x % 256) as u8, ((x + 64 * y) % 256) as u8][c]
}

fn render(
    gpu: &mut Headless,
    bgra: &[u8],
    size: (u32, u32),
    t: Transfer,
    params: &Params,
    target: (u32, u32),
) -> Rendered {
    let settings = Settings {
        source: Transfer::Identity,
        host: t,
        integer_scale: false,
    };
    gpu.render(
        Some(SourceFrame {
            width: size.0,
            height: size.1,
            bgra,
        }),
        View::full(size.0, size.1),
        &settings,
        params,
        target,
        TargetFormat::Bgra8Unorm,
    )
    .expect("headless render")
}

/// Asserts that every native texel equals `expect(code)` within the FP16 relative bound.
fn assert_native(r: &Rendered, what: &str, expect: impl Fn(f64) -> f64) {
    assert_eq!((r.native.width, r.native.height), (W, H), "{what}: native size");
    for y in 0..H {
        for x in 0..W {
            let p = r.native.pixel(x, y);
            for (c, &texel) in p.iter().take(3).enumerate() {
                let want = expect(f64::from(code(c, x, y)));
                let got = f64::from(texel);
                assert!(
                    (got - want).abs() <= want * FP16_REL + FP16_SUBNORMAL_STEP,
                    "{what}: pixel ({x}, {y}) channel {c} code {}: {got} vs {want}",
                    code(c, x, y)
                );
            }
        }
    }
}

/// Asserts that every texel of the `gun` image (beam current) equals `expect(code)` within the error of the
/// drive voltage amplified by the exponent `gamma` plus the FP16 rounding: (gamma + 1) 2^-11 relative.
fn assert_gun(r: &Rendered, what: &str, gamma: f64, expect: impl Fn(f64) -> f64) {
    let gun = r.stage("gun").expect("gun slot ran");
    assert_eq!((gun.width, gun.height), (W * AMP_K, H), "{what}: gun size");
    for y in 0..H {
        for x in 0..W * AMP_K {
            let p = gun.pixel(x, y);
            for (c, &texel) in p.iter().take(3).enumerate() {
                let k = code(c, x / AMP_K, y);
                let want = expect(f64::from(k));
                let got = f64::from(texel);
                assert!(
                    (got - want).abs() <= want * (gamma + 1.0) * FP16_REL + FP16_SUBNORMAL_STEP,
                    "{what}: gun ({x}, {y}) channel {c} code {k}: {got} vs {want}"
                );
            }
        }
    }
}

fn identity_srgb_params() -> Params {
    let mut p = srgb_params();
    assert!(p.set("gun_gamma", ParamValue::F32(1.0)));
    p
}

fn srgb_params() -> Params {
    let mut p = Params::default();
    assert!(p.set("source_eotf", ParamValue::Enum(SOURCE_EOTF_SRGB)));
    p
}

/// `drive_voltage` (the default): the native raster holds V = code / 255 for all 256 codes of every
/// channel, and the `gun` image holds the default law V^2.4.
#[test]
fn drive_voltage_mode_holds_code_over_255() {
    let test = "stage_source drive_voltage";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let r = render(
        &mut gpu,
        &ramp(W, H),
        (W, H),
        Transfer::Identity,
        &Params::default(),
        (W, H),
    );
    assert_native(&r, test, tube::drive_voltage_from_code);
    // And through the gun law: V^2.4 (the default of the live picture).
    assert_gun(&r, test, 2.4, |c| tube::drive_voltage_from_code(c).powf(2.4));
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// `srgb` with gun gamma 2.4: the `gun` image holds the sRGB linear light of every code, code 20 holds
/// 0.006995 +-2 %, and the drive voltage in the native raster, L^(1/2.4), matches the oracle (V in
/// [0, 1] contract).
#[test]
fn srgb_mode_gives_srgb_luminance_through_the_gun_gamma() {
    let test = "stage_source srgb";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let r = render(
        &mut gpu,
        &ramp(W, H),
        (W, H),
        Transfer::Power(2.4),
        &srgb_params(),
        (W, H),
    );
    assert_gun(&r, test, 2.4, |c| srgb_eotf(c / 255.0));
    // Code 20 is the red channel of pixel x = 20 (the plan's acceptance number): beam current.
    let l20 = f64::from(r.stage("gun").unwrap().pixel(20 * AMP_K, 0)[0]);
    assert!((l20 - 0.006995).abs() <= 0.006995 * 0.02, "code 20: beam current {l20}");
    // The drive voltage behind it, in the native raster: within [0, 1], equal to the oracle's.
    for x in 0..W {
        let v = f64::from(r.native.pixel(x, 0)[0]);
        let want = tube::drive_voltage_from_srgb(f64::from(code(0, x, 0)), 2.4);
        assert!((0.0..=1.0 + 1e-3).contains(&v), "code {x}: V {v}");
        assert!(
            (v - want).abs() <= want * 1e-3 + 1e-9,
            "code {x}: V {v} vs oracle {want}"
        );
    }
    // The same codes in drive_voltage mode are darker in the shadows (CRT.md: level 20 gives 0.00222).
    let d = render(
        &mut gpu,
        &ramp(W, H),
        (W, H),
        Transfer::Power(2.4),
        &Params::default(),
        (W, H),
    );
    let darker = f64::from(d.stage("gun").unwrap().pixel(20 * AMP_K, 0)[0]);
    assert!((darker - 0.00222).abs() <= 0.00222 * 0.01, "{darker}");
    assert!(l20 > 3.0 * darker);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// `srgb` with an identity gun law (gun gamma 1): V = L, the native raster and the `gun` image hold the
/// linear light.
#[test]
fn srgb_mode_with_identity_tube_is_linear_light() {
    let test = "stage_source srgb identity";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let r = render(
        &mut gpu,
        &ramp(W, H),
        (W, H),
        Transfer::Identity,
        &identity_srgb_params(),
        (W, H),
    );
    assert_native(&r, test, |c| srgb_eotf(c / 255.0));
    assert_gun(&r, test, 1.0, |c| srgb_eotf(c / 255.0));
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// A source frame of `w` x `h` white pixels.
fn white(w: u32, h: u32) -> Vec<u8> {
    vec![255; (w * h * 4) as usize]
}

/// Placement through the real pipeline (`FrameConstants::placed` in `record`): the tube image has the
/// rectangle's size and the output is exactly black outside it (the black surround), lit inside.
/// 2560 x 1440 output.
/// - default (`par`, PAR 1) of 64 x 32: aspect 2 is wider than the 4:3 tube, so the picture is 386.667 mm
///   wide, 193.333 mm high; s = 2560 / 386.667 = 6.6207 px/mm; 2560 x round(1280.0) = 1280 rows, from
///   y0 = (1440 - 1280) / 2 = 80 to 1360 (exclusive).
/// - `fill_tube`: 1920 x 1440 from x0 = 320 to 2240 (exclusive).
#[test]
fn tube_rectangle_and_black_surround_in_the_output() {
    let test = "stage_source placement";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    // The test is about where the tube rectangle is: bypass the beam (T-36a), which would modulate the
    // white field with scanlines; a bypassed screen slot copies the amp image into the tube rectangle. A
    // bypassed beam has no skirt, so the live pipeline adds no margin (T-37): the tube rectangle is the
    // raster's (the margin with the beam on: `skirt_margin_in_the_output_when_the_beam_runs`).
    gpu.set_stage_enabled("screen", false).unwrap();
    let target = (2560, 1440);
    let lit = |r: &Rendered, x: u32, y: u32| r.output.pixel(x, y)[..3].iter().all(|&v| v == 1.0);
    let black = |r: &Rendered, x: u32, y: u32| r.output.pixel(x, y)[..3].iter().all(|&v| v == 0.0);

    let r = render(
        &mut gpu,
        &white(64, 32),
        (64, 32),
        Transfer::Power(2.4),
        &Params::default(),
        target,
    );
    let screen = r.stage("screen").unwrap();
    assert_eq!((screen.width, screen.height), (2560, 1280));
    assert!(black(&r, 1000, 79) && lit(&r, 1000, 80), "top edge");
    assert!(lit(&r, 1000, 1359) && black(&r, 1000, 1360), "bottom edge");
    assert!(lit(&r, 0, 700) && lit(&r, 2559, 700), "full width");

    let mut params = Params::default();
    assert!(params.set("raster_aspect", ParamValue::Enum(RASTER_ASPECT_FILL_TUBE)));
    let r = render(
        &mut gpu,
        &white(64, 32),
        (64, 32),
        Transfer::Power(2.4),
        &params,
        target,
    );
    let screen = r.stage("screen").unwrap();
    assert_eq!((screen.width, screen.height), (1920, 1440));
    assert!(black(&r, 319, 700) && lit(&r, 320, 700), "left edge");
    assert!(lit(&r, 2239, 700) && black(&r, 2240, 700), "right edge");
    assert!(lit(&r, 1000, 0) && lit(&r, 1000, 1439), "full height");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// The skirt margin in the live output (T-37, `FrameConstants::with_skirt_margin`, applied by
/// `Pipeline::record` only when the `screen` slot runs): `fill_tube` on 2560 x 1440 places the 1920 x 1440
/// raster at x0 = 320 (previous test). With the beam on, the tube image grows by m px on the left and the
/// right (none vertically: the raster fills the target height), m = ceil(4 sigma_max px_per_mm) with sigma_max
/// the spot sigma (mm) at twice the peak current (`BEAM_REACH_CURRENT`), from the crt-model spot growth law
/// (oracle, independent of `with_skirt_margin`; default parameters: no tail, no misconvergence). The output's
/// tube rectangle is the grown one, and the black surround starts m px further out. Since the T-37 review
/// (F1) the margin lies outside the visible opening (the raster at zero overscan): the columns just left of
/// the raster (319) and just right of it (2240) are exactly 0, the raster's edge columns are lit.
#[test]
fn skirt_margin_in_the_output_when_the_beam_runs() {
    let test = "stage_source skirt margin";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let target = (2560, 1440);
    let mut params = Params::default();
    assert!(params.set("raster_aspect", ParamValue::Enum(RASTER_ASPECT_FILL_TUBE)));
    assert_eq!((params.beam_tail_weight, params.geometry_conv_red_dx_mm), (0.0, 0.0));
    let fc = FrameConstants::placed((64, 32), target, false, &params);
    assert_eq!(fc.tube_rect_px, [320.0, 0.0, 2240.0, 1440.0]);
    let growth = crt_model::beam::SpotGrowth {
        fwhm_mid: f64::from(params.beam_fwhm_mid_mm),
        fwhm_peak: f64::from(params.beam_fwhm_peak_mm),
        n: f64::from(params.beam_growth_exponent),
    };
    let sigma_mm = crt_model::units::sigma_from_fwhm(growth.fwhm(2.0).max(growth.fwhm(0.0)));
    let m = (4.0 * sigma_mm * f64::from(fc.px_per_mm)).ceil() as u32;
    assert!((1..320).contains(&m), "margin {m}");
    assert_eq!(fc.with_skirt_margin(&params).raster_margin, [m as i32, 0]);

    // Line 0 of the 32-line raster is centred on row 1440 / 32 / 2 = 22.5.
    let y = 22;
    let any = |r: &Rendered, x: u32| r.output.pixel(x, y)[..3].iter().any(|&v| v > 0.0);
    let black = |r: &Rendered, x: u32| r.output.pixel(x, y)[..3].iter().all(|&v| v == 0.0);
    let r = render(
        &mut gpu,
        &white(64, 32),
        (64, 32),
        Transfer::Power(2.4),
        &params,
        target,
    );
    let screen = r.stage("screen").unwrap();
    assert_eq!(
        (screen.width, screen.height),
        (1920 + 2 * m, 1440),
        "tube image grown by the margin"
    );
    // T-37 review F1: the margin is evaluated but lies outside the visible opening (the raster at zero
    // overscan), so the skirt past the raster's ends is clipped to exactly 0; the raster's own edge is lit.
    assert!(
        black(&r, 319) && black(&r, 2240),
        "skirt clipped by the visible opening"
    );
    assert!(any(&r, 320) && any(&r, 2239), "raster edge lit");
    assert!(
        black(&r, 320 - m - 1) && black(&r, 2240 + m),
        "black surround past the margin"
    );

    // The same frame with `screen` bypassed: no margin (a nearest resample would stretch the raster).
    gpu.set_stage_enabled("screen", false).unwrap();
    let r = render(
        &mut gpu,
        &white(64, 32),
        (64, 32),
        Transfer::Power(2.4),
        &params,
        target,
    );
    let screen = r.stage("screen").unwrap();
    assert_eq!(
        (screen.width, screen.height),
        (1920, 1440),
        "no margin with the beam bypassed"
    );
    assert!(black(&r, 319) && black(&r, 2240), "surround at the raster's ends");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

fn pvm_input(native: (u32, u32), aspect: RasterAspect) -> PlacementInput {
    PlacementInput {
        native,
        target: (2560, 1440),
        visible_mm: (290.0 * 4.0 / 3.0, 290.0),
        height_fraction: 1.0,
        aspect,
        line_multiplier: 1,
        integer: false,
        fit: HostFit::default(),
    }
}

/// px per mm, line pitch and triads of the PVM-20M4U class tube on 2560 x 1440, from the application's
/// placement against the oracle and the `CRT.md` numbers (derivation in the module documentation).
#[test]
fn pvm_20m4u_scale_on_2560_by_1440() {
    let p = place(&pvm_input((256, 240), RasterAspect::FillTube));
    // 1440 / 290 = 4.966 px/mm +-0.1 %.
    assert!((p.px_per_mm - 4.966).abs() / 4.966 <= 1e-3, "{}", p.px_per_mm);
    let oracle = tube::px_per_mm(
        tube::raster_size_mm((290.0 * 4.0 / 3.0, 290.0), 4.0 / 3.0),
        (2560.0, 1440.0),
        1.0,
    );
    assert!((p.px_per_mm - oracle).abs() < 1e-9);
    // Line pitch, 240 lines: 1.208 mm; two lines per row halves it.
    assert!((p.line_pitch_mm - 1.208).abs() < 1e-3);
    assert!((p.line_pitch_mm - tube::scanline_pitch_mm(290.0, 240.0, 1.0)).abs() < 1e-12);
    let two = place(&PlacementInput {
        line_multiplier: 2,
        ..pvm_input((256, 240), RasterAspect::FillTube)
    });
    assert!((two.line_pitch_mm - p.line_pitch_mm / 2.0).abs() < 1e-12);
    // Triads: 386.667 / 0.31 = 1247.31 across, 1920 px wide, 1.539 px per triad (derived from CRT.md's 1245 triads = 1.542), and
    // 4.872 per 256-column source pixel.
    let width_px = f64::from(p.rect_px[2] - p.rect_px[0]);
    assert_eq!(width_px, 1920.0);
    let across = triads_across(p.raster_mm.0, 0.31);
    assert!((across - 1247.31).abs() < 0.01);
    let per_triad = px_per_triad(width_px, across);
    assert!((per_triad - 1.5393).abs() < 1e-4, "{per_triad}");
    assert!((per_triad - p.px_per_mm * 0.31).abs() < 1e-9);
    assert!(
        (per_triad - 1.542).abs() / 1.542 < 2e-3,
        "against the 1.542 derived from CRT.md's 1245 triads: {per_triad}"
    );
    assert!((triads_per_source_px(across, 256.0) - 4.872).abs() < 1e-3);
}

/// PAR 8:7 on 256 x 240: displayed aspect 1.219 +-0.5 % (oracle: 256 x 8/7 / 240 = 1.219048).
#[test]
fn par_8_7_displayed_aspect() {
    let par = 8.0 / 7.0;
    let p = place(&pvm_input((256, 240), RasterAspect::Par(par)));
    assert!((p.display_aspect - 1.219).abs() / 1.219 <= 5e-3, "{}", p.display_aspect);
    assert!((p.display_aspect - tube::display_aspect(256.0, 240.0, par)).abs() < 1e-12);
    let (w, h) = (p.rect_px[2] - p.rect_px[0], p.rect_px[3] - p.rect_px[1]);
    assert!((f64::from(w / h) - 1.219).abs() / 1.219 <= 5e-3, "{w} x {h}");
    // The same PAR through the parameter table.
    let mut params = Params::default();
    assert!(params.set("source_par", ParamValue::F32(par as f32)));
    let fc = FrameConstants::placed((256, 240), (2560, 1440), false, &params);
    let (w, h) = (fc.tube_size[0] as f64, fc.tube_size[1] as f64);
    assert!((w / h - 1.219).abs() / 1.219 <= 5e-3, "{w} x {h}");
}

/// The line multiplier and the fraction through `FrameConstants`: `px_per_mm` and `line_pitch_mm` are filled
/// (they were 0 before T-30), a half height tube has half the px per mm and the same line pitch.
#[test]
fn frame_constants_carry_the_scale() {
    let mut params = Params::default();
    assert!(params.set("raster_aspect", ParamValue::Enum(RASTER_ASPECT_FILL_TUBE)));
    let fc = FrameConstants::placed((256, 240), (2560, 1440), false, &params);
    assert!((f64::from(fc.px_per_mm) - 4.9655).abs() < 1e-3);
    assert!((f64::from(fc.line_pitch_mm) - 1.2083).abs() < 1e-3);
    assert!(params.set("tube_height_fraction", ParamValue::F32(0.5)));
    let half = FrameConstants::placed((256, 240), (2560, 1440), false, &params);
    assert!((f64::from(half.px_per_mm) - f64::from(fc.px_per_mm) / 2.0).abs() < 1e-3);
    assert_eq!(half.line_pitch_mm, fc.line_pitch_mm);
    assert!(params.set("line_multiplier", ParamValue::I32(2)));
    let double = FrameConstants::placed((256, 240), (2560, 1440), false, &params);
    assert!((f64::from(double.line_pitch_mm) - f64::from(fc.line_pitch_mm) / 2.0).abs() < 1e-6);
}

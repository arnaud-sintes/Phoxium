//! Tests of the `gun` slot: gun law and white balance (T-34: PHY-6, COL-2).
//!
//! Headless GPU tests (validation layer on, zero messages). Drive voltages V are injected as an FP32 image
//! after the `amp` slot (so the shader reads exact `f32` values, not FP16 ones), only `gun` is enabled, and
//! the `gun` image (FP16, converted exactly to `f32`) is compared with the `crt_model::gun` oracle (f64,
//! straight from `CRT.md`) and with the numbers derived below. The only error of the GPU against the oracle
//! is the FP16 storage of the result, which truncates on the NVIDIA driver (see `gpu_identity.rs`): relative
//! 2^-10 for a normal value, absolute 2^-24 for a subnormal one; `tol` is their sum. The `f32` arithmetic
//! and the driver's `pow` are far inside it.
//!
//! # Hand derivations (from `CRT.md`, "Gun law" and "White point")
//!
//! - Pure power, level 20 of 255: V = 20 / 255 = 0.0784314, ln V = -2.54553. Gamma 2.4: exp(-6.10927) =
//!   0.002222 (CRT.md corrected value 0.00222); gamma 2.2: exp(-5.60017) = 0.003697 (CRT.md 0.00370).
//! - GOG, neutral controls, V = 0.5: 0.5^2.4 = exp(-1.663553) = 0.189465 (plan: 0.18946 +-0.2 %). V = 1 gives 1;
//!   V = 0.01: 0.01^2.4 = 10^-4.8 = 1.585e-5 < 1.6e-5: zero slope at cutoff, no linear toe (sRGB would
//!   give 0.01 / 12.92 = 7.7e-4).
//! - GOG with controls, V = 0.5 on every channel, gamma 2.4, gain k_g = 1.2, offset k_o = -0.1, cutoff
//!   (+0.05, 0, -0.05), drive (0.9, 1, 1.1): the arguments are 1.2 x 0.5 - 0.1 + cutoff = 0.55, 0.50, 0.45;
//!   0.55^2.4 = exp(2.4 ln 0.55) = exp(-1.434809) = 0.238161, x 0.9 = 0.214345; 0.5^2.4 = 0.189465;
//!   0.45^2.4 = exp(2.4 ln 0.45) = exp(-1.916358) = 0.147133, x 1.1 = 0.161846. Under cutoff the argument
//!   is negative and the current is 0 (red: 1.2 V - 0.05 < 0 below V = 0.0417).
//! - BT.1886, Lw = 100, Lb = 0.1: Lw^(1/2.4) = 100^0.416667 = 6.81292, Lb^(1/2.4) = 0.1^0.416667 =
//!   0.383119, their difference 6.42980; a = 6.42980^2.4 = 87.0311 and b = 0.383119 / 6.42980 = 0.0595848.
//!   L(0) = a b^2.4 = 0.1000, L(1) = a (1 + b)^2.4 = 100.00, L(0.5) = a 0.5595848^2.4 = 21.6049. The stage
//!   outputs L / Lw: 0.001, 1, 0.216049.
//! - BT.1886 Annex 1, black lift b = 0 (L / Lw, k = 1): V = 0.5: 0.5^2.6 = 0.164938; V = 0.2 (below the
//!   knee): 0.35^-0.4 x 0.2^3 = 1.52173 x 0.008 = 0.012175; V = 0.35: 0.35^2.6 = 0.065249 from both sides
//!   (0.35^-0.4 0.35^3 = 0.35^2.6). With b = 0.1: k = 1.1^-2.6 = 0.780511; V = 0.5: k 0.6^2.6 = 0.780511 x
//!   0.264968 = 0.206810; V = 0.2: k 0.45^-0.4 0.3^3 = 0.780511 x 1.376306 x 0.027 = 0.029004. Continuity at
//!   the knee for b = 0.05: k = 1.05^-2.6 = 0.880862, I(0.35) = k 0.4^2.6 = 0.081332 from both sides.
//! - White retune (relative, review fix of T-34). The unit-luminance lights are p_c = (x / y, 1,
//!   (1 - x - y) / y); `s(W)` = P^-1 W are the weights of the primaries that make the white W (same form).
//!   The tube RGB (1, 1, 1) that the output transform assumes is already the tube white, so its channel c is
//!   `s_c(tube white) p_c`, and gains g on it give the target white T when g = s(T) / s(tube white), scaled
//!   to a maximum of 1. SMPTE-C (0.630, 0.340), (0.310, 0.595), (0.155, 0.070): s(D65 = (0.3127, 0.3290)) is the
//!   Y row of the RGB to XYZ matrix (0.2124, 0.7010, 0.0865, Lindbloom, exactly (0.212376, 0.701060,
//!   0.086564)); s(D93 = (0.2831, 0.2971)) = (0.176751, 0.707232, 0.116017); the ratios are (0.832252,
//!   1.008804, 1.340252), over the largest (blue): gains (0.62097, 0.75270, 1). A target equal to the tube
//!   white gives gains (1, 1, 1) exactly, so the output equals the gains-off output bit for bit. The
//!   test checks that the gains the GPU wrote equal the oracle's and that the light of the tube RGB
//!   (1, 1, 1) after them has the target chromaticity to +-0.0005 (the plan's acceptance).
//! - Cross-stage (decode, gun, encode matrix, output): SMPTE-C D65 tube retuned to D93, white input (code
//!   255, V = 1, power law gives 1): the gun writes the gains, the T-46 matrix M = inv(M_sRGB) M_tube maps
//!   them to the display RGB `e M g` (e the exposure), whose XYZ through the sRGB matrix (primaries
//!   (0.64, 0.33), (0.30, 0.60), (0.15, 0.06), white D65) must have the chromaticity (0.2831, 0.2971). By hand:
//!   the tube light is sum g_c s_c(D65) p_c with weights (0.620967 x 0.212376, 0.752697 x 0.701060, 0.086564) =
//!   (0.131878, 0.527685, 0.086564), sum 0.746127 = Y; X = 0.2831 / 0.2971 Y = 0.71094, Z =
//!   (1 - 0.2831 - 0.2971) / 0.2971 Y = 1.05423; the sRGB RGB of that XYZ is (0.6315, 0.7546, 1.0017), and the
//!   exposure 0.9 keeps blue below white (1.0017 would clip and move the chromaticity).
//! - Decode inverse, `srgb` source mode: for each law the gun image of all 256 codes holds the sRGB EOTF of
//!   the code, relative to white, within the FP16 bound (V is stored in FP16 and the law magnifies its
//!   relative error by at most its exponent, 3 for Annex 1, so (3 + 1) 2^-10 relative plus 2^-24). Code
//!   20: sRGB 0.006995; the old power-only inverse gave 0.0154 with BT.1886 (Lb = 0.1, Lw = 100) and 0.0031
//!   with Annex 1 b = 0. Where the sRGB value is under the black floor I(0) of a law (BT.1886 with Lb > 0:
//!   0.001; Annex 1 with b > 0) the law shows its floor.
//! - Gamma guard: gun_gamma <= 0 is clamped to 0.1 in `gun.comp` and in `decode`, with finite output.
//! - Exact identity: a power law of exponent 1 returns V unchanged (no `pow`), so for values that FP16
//!   stores exactly (k / 1024, k <= 1024) the output equals the input bit for bit; so does a disabled `gun`
//!   slot, whatever the law (bypass of a same-domain slot).

mod common;

use crt_model::colour::{mul3, rgb_to_xyz, srgb_eotf};
use crt_model::gun::{white_retune_gains, white_xy, Gun, Law};
use crt_model::output::{D65, SRGB_BLUE, SRGB_GREEN, SRGB_RED};
use ultimatecrt::gpu::{
    output_matrix, ClipCounts, FrameConstants, Headless, ImageF32, Rendered, Settings, SourceFrame, StageInput,
    StageRun, TargetFormat, Transfer, AMP_K,
};
use ultimatecrt::params::{
    ParamValue, Params, GUN_LAW_BT1886, GUN_LAW_BT1886_ANNEX1, GUN_LAW_GOG, GUN_LAW_POWER, SOURCE_EOTF_SRGB,
};
use ultimatecrt::scaling::View;
use ultimatecrt::white::white_gains;

/// Rows of the injected image (the tube domain needs a few rows; every row is the same).
const ROWS: u32 = 4;
/// FP16 subnormal step, 2^-24.
const STEP: f64 = 5.960464477539063e-8;

/// Tolerance of an FP16 result of value `want` (module documentation).
fn tol(want: f64) -> f64 {
    want.abs() * 2f64.powi(-10) + STEP
}

/// An `f32`-exact `f64`: the GPU reads `f32`, so the oracle must see the same drive voltage.
fn f(x: f64) -> f64 {
    f64::from(x as f32)
}

/// All 256 codes on each channel, in three different orders (V = code / 255).
fn all_codes() -> Vec<[f64; 3]> {
    (0..256u32)
        .map(|x| {
            [
                f(f64::from(x) / 255.0),
                f(f64::from(255 - x) / 255.0),
                f(f64::from((x * 7 + 3) % 256) / 255.0),
            ]
        })
        .collect()
}

fn set(p: &mut Params, id: &str, v: ParamValue) {
    assert!(p.set(id, v), "{id}");
}

fn params(law: u32) -> Params {
    let mut p = Params::default();
    set(&mut p, "gun_law", ParamValue::Enum(law));
    p
}

/// Runs the `gun` slot alone (or bypassed with `enabled` false) over the drive voltages `v` (one texel
/// each, K = 1, every row the same), with the white gains of `params`.
fn run(gpu: &mut Headless, params: &Params, v: &[[f64; 3]], enabled: bool) -> Rendered {
    let n = v.len() as u32;
    let mut rgba = Vec::new();
    for _ in 0..ROWS {
        for px in v {
            rgba.extend_from_slice(&[px[0] as f32, px[1] as f32, px[2] as f32, 1.0]);
        }
    }
    let image = ImageF32 {
        width: n,
        height: ROWS,
        rgba,
    };
    let mut fc = FrameConstants::new((n, ROWS), (n, ROWS), false);
    fc.amp_k = 1;
    fc.white_gain = white_gains(params);
    gpu.enable_only(if enabled { &["gun"] } else { &[] }).unwrap();
    let settings = Settings::identity();
    gpu.render_stages(&StageRun {
        input: StageInput::Image {
            after: "amp",
            image: &image,
        },
        frame: fc,
        settings: &settings,
        params,
        format: TargetFormat::Rgba32F,
    })
    .expect("render_stages")
}

/// The `gun` image of `r`.
fn gun(r: &Rendered) -> &ImageF32 {
    r.stage("gun").expect("gun slot")
}

/// Asserts that the `gun` image equals the oracle for every texel of `v`, on every row.
fn assert_oracle(r: &Rendered, v: &[[f64; 3]], oracle: &Gun, what: &str) {
    let img = gun(r);
    assert_eq!((img.width as usize, img.height), (v.len(), ROWS), "{what}: size");
    for y in 0..ROWS {
        for (x, px) in v.iter().enumerate() {
            let got = img.pixel(x as u32, y);
            for c in 0..3 {
                let want = oracle.beam_current(c, px[c]);
                assert!(
                    (f64::from(got[c]) - want).abs() <= tol(want),
                    "{what}: x {x} row {y} channel {c}: V {} -> {}, oracle {want}",
                    px[c],
                    got[c]
                );
            }
            assert_eq!(got[3], 1.0, "{what}: alpha");
        }
    }
}

/// The value of channel `c` of the first row, texel `x`.
fn at(r: &Rendered, x: usize, c: usize) -> f64 {
    f64::from(gun(r).pixel(x as u32, 0)[c])
}

fn close(got: f64, want: f64, rel: f64, what: &str) {
    assert!(
        (got - want).abs() <= want.abs() * rel,
        "{what}: {got} vs {want} (+-{} %)",
        rel * 100.0
    );
}

#[test]
fn power_law_all_codes_and_the_dark_tone() {
    let test = "stage_gun power";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let v = all_codes();
    for gamma in [2.4_f32, 2.2, 3.0, 1.5] {
        let mut p = params(GUN_LAW_POWER);
        set(&mut p, "gun_gamma", ParamValue::F32(gamma));
        let oracle = Gun {
            gamma: f64::from(gamma),
            ..Gun::default()
        };
        let r = run(&mut gpu, &p, &v, true);
        assert_oracle(&r, &v, &oracle, &format!("{test} gamma {gamma}"));
        // Level 20 of 255 is the red channel of texel x = 20 (CRT.md, corrected example).
        if gamma == 2.4 {
            assert!((at(&r, 20, 0) - 0.00222).abs() <= 1e-5, "{}", at(&r, 20, 0));
        }
        if gamma == 2.2 {
            assert!((at(&r, 20, 0) - 0.00370).abs() <= 1e-5, "{}", at(&r, 20, 0));
        }
    }
    // The default (Params::default) is the pure power 2.4 (a power law ignores the GOG controls).
    let mut p = Params::default();
    set(&mut p, "gun_gain", ParamValue::F32(1.7));
    set(&mut p, "gun_offset", ParamValue::F32(0.2));
    set(&mut p, "gun_cutoff_rgb", ParamValue::Vec3([0.1, -0.1, 0.05]));
    set(&mut p, "gun_drive_rgb", ParamValue::Vec3([0.6, 1.4, 1.0]));
    let r = run(&mut gpu, &p, &v, true);
    assert_oracle(&r, &v, &Gun::default(), &format!("{test} default ignores GOG controls"));
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

#[test]
fn gog_numbers_and_controls() {
    let test = "stage_gun gog";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    // Neutral controls: the plan's acceptance numbers.
    let p = params(GUN_LAW_GOG);
    let v = [[0.5; 3], [1.0; 3], [0.01; 3], [0.0; 3]].map(|t| t.map(f));
    let r = run(&mut gpu, &p, &v, true);
    close(at(&r, 0, 1), 0.18946, 0.002, "GOG V = 0.5");
    assert!((at(&r, 1, 1) - 1.0).abs() <= 1e-3, "GOG V = 1: {}", at(&r, 1, 1));
    assert!(
        at(&r, 2, 1) < 1.6e-5,
        "zero slope at cutoff: I(0.01) = {}",
        at(&r, 2, 1)
    );
    assert!(at(&r, 2, 1) > 1.4e-5, "I(0.01) = 0.01^2.4 = 1.585e-5: {}", at(&r, 2, 1));
    assert_eq!(at(&r, 3, 1), 0.0);

    // Controls: gain, offset, per-channel cutoff and drive.
    let mut p = params(GUN_LAW_GOG);
    set(&mut p, "gun_gain", ParamValue::F32(1.2));
    set(&mut p, "gun_offset", ParamValue::F32(-0.1));
    set(&mut p, "gun_cutoff_rgb", ParamValue::Vec3([0.05, 0.0, -0.05]));
    set(&mut p, "gun_drive_rgb", ParamValue::Vec3([0.9, 1.0, 1.1]));
    let half = [[f(0.5); 3]];
    let r = run(&mut gpu, &p, &half, true);
    for (c, want) in [0.214_345, 0.189_465, 0.161_846].into_iter().enumerate() {
        assert!(
            (at(&r, 0, c) - want).abs() <= tol(want) + 1e-6,
            "controls V = 0.5 channel {c}: {} vs {want}",
            at(&r, 0, c)
        );
    }
    let oracle = Gun {
        law: Law::Gog,
        gain: 1.2,
        offset: -0.1,
        cutoff: [0.05, 0.0, -0.05],
        drive: [0.9, 1.0, 1.1],
        ..Gun::default()
    };
    let codes = all_codes();
    let r = run(&mut gpu, &p, &codes, true);
    assert_oracle(&r, &codes, &oracle, &format!("{test} controls, all codes"));
    // Red is cut off below V = 0.0417: code 10 (V = 0.0392) gives exactly 0.
    assert_eq!(at(&r, 10, 0), 0.0, "red below cutoff");
    assert!(at(&r, 11, 0) > 0.0, "red just above cutoff");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

#[test]
fn bt1886_matches_the_standard() {
    let test = "stage_gun bt1886";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    // Lw = 100, Lb = 0.1 (the defaults): L(0) = 0.100, L(1) = 100.0 +-0.5 % (the stage outputs L / Lw).
    let p = params(GUN_LAW_BT1886);
    let v = [[0.0; 3], [0.5; 3], [1.0; 3]];
    let r = run(&mut gpu, &p, &v, true);
    close(at(&r, 0, 0) * 100.0, 0.100, 0.005, "L(0)");
    close(at(&r, 1, 0) * 100.0, 21.6049, 0.005, "L(0.5)");
    close(at(&r, 2, 0) * 100.0, 100.0, 0.005, "L(1)");
    let oracle = Gun {
        law: Law::Bt1886,
        ..Gun::default()
    };
    let codes = all_codes();
    let r = run(&mut gpu, &p, &codes, true);
    assert_oracle(&r, &codes, &oracle, &format!("{test} all codes"));
    // Other luminances: only Lb / Lw shapes the relative output.
    let mut p = params(GUN_LAW_BT1886);
    set(&mut p, "gun_bt1886_white", ParamValue::F32(120.0));
    set(&mut p, "gun_bt1886_black", ParamValue::F32(0.05));
    let oracle = Gun {
        law: Law::Bt1886,
        lw: 120.0,
        lb: 0.05,
        ..Gun::default()
    };
    let r = run(&mut gpu, &p, &codes, true);
    assert_oracle(&r, &codes, &oracle, &format!("{test} Lw 120 Lb 0.05"));
    // Lb = 0 is a pure 2.4 power law.
    set(&mut p, "gun_bt1886_black", ParamValue::F32(0.0));
    let r = run(&mut gpu, &p, &codes, true);
    assert_oracle(&r, &codes, &Gun::default(), &format!("{test} Lb 0"));
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

#[test]
fn annex1_numbers_and_continuity_at_the_knee() {
    let test = "stage_gun annex1";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let v = [[0.5; 3], [0.2; 3]].map(|t| t.map(f));
    let p = params(GUN_LAW_BT1886_ANNEX1);
    let r = run(&mut gpu, &p, &v, true);
    close(at(&r, 0, 0), 0.164_938, 1e-3, "b = 0, V = 0.5");
    close(at(&r, 1, 0), 0.012_175, 1e-3, "b = 0, V = 0.2");
    let mut p = params(GUN_LAW_BT1886_ANNEX1);
    set(&mut p, "gun_annex1_lift", ParamValue::F32(0.1));
    let r = run(&mut gpu, &p, &v, true);
    close(at(&r, 0, 0), 0.206_810, 1e-3, "b = 0.1, V = 0.5");
    close(at(&r, 1, 0), 0.029_004, 1e-3, "b = 0.1, V = 0.2");

    let codes = all_codes();
    for lift in [0.0_f32, 0.05, 0.1, 0.2] {
        let mut p = params(GUN_LAW_BT1886_ANNEX1);
        set(&mut p, "gun_annex1_lift", ParamValue::F32(lift));
        let oracle = Gun {
            law: Law::Annex1,
            lift: f64::from(lift),
            ..Gun::default()
        };
        let r = run(&mut gpu, &p, &codes, true);
        assert_oracle(&r, &codes, &oracle, &format!("{test} lift {lift} all codes"));
        // Continuous at V = 0.35 (+-1e-4): the two branches meet. Values 1e-5 either side.
        let knee = [[f(0.35 - 1e-5); 3], [f(0.35 + 1e-5); 3]];
        let r = run(&mut gpu, &p, &knee, true);
        let (below, above) = (at(&r, 0, 0), at(&r, 1, 0));
        assert!(
            (below - above).abs() <= 1e-4,
            "lift {lift}: knee {below} vs {above}, difference {}",
            (below - above).abs()
        );
        if lift == 0.05 {
            close(above, 0.081_332, 2e-3, "b = 0.05 at the knee");
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

fn to64(a: [f32; 2]) -> (f64, f64) {
    (f64::from(a[0]), f64::from(a[1]))
}

/// Chromaticity of the light of the tube RGB (1, 1, 1) after the gains the GPU wrote (texel 0 of `r`), for
/// the tube (primaries and white) of `p`.
fn gpu_white_xy(r: &Rendered, p: &Params) -> (f64, f64) {
    let g = [at(r, 0, 0), at(r, 0, 1), at(r, 0, 2)];
    white_xy(
        g,
        to64(p.tube_red_xy),
        to64(p.tube_green_xy),
        to64(p.tube_blue_xy),
        to64(p.tube_white_xy),
    )
}

/// The oracle gains (f64, cofactor inverse) of the retune of the parameter table.
fn oracle_gains(p: &Params) -> [f64; 3] {
    white_retune_gains(
        to64(p.tube_red_xy),
        to64(p.tube_green_xy),
        to64(p.tube_blue_xy),
        to64(p.tube_white_xy),
        to64(p.gun_retune_white_xy),
    )
}

const D93: [f32; 2] = [0.2831, 0.2971];

fn assert_xy(got: (f64, f64), want: [f32; 2], tol: f64, what: &str) {
    let want = to64(want);
    assert!(
        (got.0 - want.0).abs() <= tol && (got.1 - want.1).abs() <= tol,
        "{what}: xy {got:?} vs {want:?} (+-{tol})"
    );
}

#[test]
fn white_retune_sets_the_white_relative_to_the_tube_white() {
    let test = "stage_gun white retune";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let full = [[1.0; 3]];
    // Off (the default): drive (1, 1, 1) is beam current (1, 1, 1) exactly.
    let off = run(&mut gpu, &Params::default(), &full, true);
    for c in 0..3 {
        assert_eq!(at(&off, 0, c), 1.0, "white retune off, channel {c}");
    }
    // On, with the target at the tube white (the default target): gains exactly (1, 1, 1).
    let mut p = Params::default();
    set(&mut p, "gun_white_balance", ParamValue::Bool(true));
    let same = run(&mut gpu, &p, &full, true);
    for c in 0..3 {
        assert_eq!(at(&same, 0, c), 1.0, "target = tube white, channel {c}");
    }
    // SMPTE-C D65 tube retuned to D93: gains (0.62097, 0.75270, 1) (module documentation).
    let mut p93 = p;
    set(&mut p93, "gun_retune_white_xy", ParamValue::Vec2(D93));
    let r93 = run(&mut gpu, &p93, &full, true);
    for (c, want) in [0.620_967, 0.752_697, 1.0].into_iter().enumerate() {
        assert!(
            (at(&r93, 0, c) - want).abs() <= tol(want) + 1e-6,
            "D65 -> D93 gain {c}: {} vs {want}",
            at(&r93, 0, c)
        );
    }
    // The GPU gains are the oracle's and give the target white +-0.0005 (the plan's acceptance).
    for (c, want) in oracle_gains(&p93).into_iter().enumerate() {
        assert!(
            (at(&r93, 0, c) - want).abs() <= tol(want),
            "D93 gain {c}: {} vs oracle {want}",
            at(&r93, 0, c)
        );
    }
    assert_xy(gpu_white_xy(&r93, &p93), D93, 5e-4, "D65 -> D93");
    // Without retune the tube shows its own white.
    assert_xy(gpu_white_xy(&same, &p), [0.3127, 0.3290], 5e-4, "tube white");
    // The other way: a D93 tube retuned to D65.
    let mut p_back = p;
    set(&mut p_back, "tube_white_xy", ParamValue::Vec2(D93));
    set(&mut p_back, "gun_retune_white_xy", ParamValue::Vec2([0.3127, 0.3290]));
    let r_back = run(&mut gpu, &p_back, &full, true);
    assert_xy(gpu_white_xy(&r_back, &p_back), [0.3127, 0.3290], 5e-4, "D93 -> D65");
    // s(D65) / s(D93) = (1.2016, 0.9913, 0.7462): red is the strongest gun now, blue drops to 0.621.
    assert_eq!(at(&r_back, 0, 0), 1.0, "red at full drive");
    assert!((at(&r_back, 0, 2) - 0.621).abs() <= 1e-3, "blue {}", at(&r_back, 0, 2));
    // Another tube (Rec. 709 primaries, D65): the gains follow the primaries.
    let mut p709 = p93;
    set(&mut p709, "tube_red_xy", ParamValue::Vec2([0.64, 0.33]));
    set(&mut p709, "tube_green_xy", ParamValue::Vec2([0.30, 0.60]));
    set(&mut p709, "tube_blue_xy", ParamValue::Vec2([0.15, 0.06]));
    let r709 = run(&mut gpu, &p709, &full, true);
    assert_xy(gpu_white_xy(&r709, &p709), D93, 5e-4, "709 D93");
    // The gains multiply every law: pure power 2.4 at V = 0.5 with the D93 gains.
    let half = [[f(0.5); 3]];
    let r = run(&mut gpu, &p93, &half, true);
    for (c, g) in [0.620_967, 0.752_697, 1.0].into_iter().enumerate() {
        let want = 0.189_465 * g;
        assert!(
            (at(&r, 0, c) - want).abs() <= want * 1.5e-3 + 1e-6,
            "gain x V^2.4, channel {c}: {} vs {want}",
            at(&r, 0, c)
        );
    }
    // And BT.1886 white (V = 1) is 1 x the gain.
    let mut pb = p93;
    set(&mut pb, "gun_law", ParamValue::Enum(GUN_LAW_BT1886));
    let r = run(&mut gpu, &pb, &full, true);
    assert!((at(&r, 0, 0) - 0.620_967).abs() <= tol(0.620_967) + 1e-6);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// `FrameConstants::placed` (the live path) carries the gains of the parameter table.
#[test]
fn frame_constants_carry_the_white_gains() {
    let mut p = Params::default();
    let fc = FrameConstants::placed((256, 240), (2560, 1440), false, &p);
    assert_eq!(fc.white_gain, [1.0; 3], "off");
    assert_eq!(
        FrameConstants::new((256, 240), (2560, 1440), false).white_gain,
        [1.0; 3]
    );
    set(&mut p, "gun_white_balance", ParamValue::Bool(true));
    let fc = FrameConstants::placed((256, 240), (2560, 1440), false, &p);
    assert_eq!(fc.white_gain, [1.0; 3], "on, target = tube white");
    set(&mut p, "gun_retune_white_xy", ParamValue::Vec2(D93));
    let fc = FrameConstants::placed((256, 240), (2560, 1440), false, &p);
    let oracle = oracle_gains(&p);
    for c in 0..3 {
        assert!(
            (f64::from(fc.white_gain[c]) - oracle[c]).abs() <= 1e-6,
            "{:?} vs {oracle:?}",
            fc.white_gain
        );
    }
    assert_eq!(fc.white_gain, white_gains(&p));
}

#[test]
fn identity_cases_are_bit_exact() {
    let test = "stage_gun identity";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    // k / 1024 is exact in FP16 up to 1: gamma 1 must not change a bit.
    let v: Vec<[f64; 3]> = (0..=1024u32)
        .map(|k| {
            let a = f64::from(k) / 1024.0;
            [a, 1.0 - a, f64::from((k * 5) % 1025) / 1024.0]
        })
        .collect();
    for law in [GUN_LAW_POWER, GUN_LAW_GOG] {
        let mut p = params(law);
        set(&mut p, "gun_gamma", ParamValue::F32(1.0));
        let r = run(&mut gpu, &p, &v, true);
        for (x, px) in v.iter().enumerate() {
            for (c, want) in px.iter().enumerate() {
                assert_eq!(at(&r, x, c), *want, "law {law} gamma 1: x {x} channel {c}");
            }
        }
    }
    // A disabled gun is a bypass whatever the law: the slot shows its input, bit for bit.
    let codes = all_codes();
    for law in [GUN_LAW_POWER, GUN_LAW_GOG, GUN_LAW_BT1886, GUN_LAW_BT1886_ANNEX1] {
        let mut p = params(law);
        set(&mut p, "gun_white_balance", ParamValue::Bool(true));
        let r = run(&mut gpu, &p, &codes, false);
        let img = gun(&r);
        for (x, px) in codes.iter().enumerate() {
            for (c, want) in px.iter().enumerate() {
                assert_eq!(
                    f64::from(img.pixel(x as u32, 0)[c]),
                    *want,
                    "gun off, law {law}: x {x} channel {c}"
                );
            }
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// A gray ramp of all 256 codes (code x at column x), `h` identical rows, as captured BGRA8.
fn ramp_bgra(w: u32, h: u32) -> Vec<u8> {
    let mut bgra = Vec::new();
    for _ in 0..h {
        for x in 0..w {
            bgra.extend_from_slice(&[x as u8, x as u8, x as u8, 255]);
        }
    }
    bgra
}

/// Renders the 256-code ramp in the `srgb` source mode through the chain up to the optics and returns the
/// `gun` image (one code per `AMP_K` texels).
fn render_srgb_ramp(gpu: &mut Headless, p: &Params) -> Rendered {
    let (w, h) = (256u32, 4u32);
    let bgra = ramp_bgra(w, h);
    gpu.enable_only(&["signal", "amp", "gun", "screen", "phosphor", "optics"])
        .unwrap();
    gpu.render(
        Some(SourceFrame {
            width: w,
            height: h,
            bgra: &bgra,
        }),
        View::full(w, h),
        &Settings {
            source: Transfer::Identity,
            host: Transfer::Srgb,
            integer_scale: false,
        },
        p,
        (w, h),
        TargetFormat::Rgba32F,
    )
    .expect("render")
}

/// The `srgb` source mode inverts the nominal curve of the ACTIVE law, so for every law the `gun` image
/// holds the sRGB luminance of each of the 256 codes (relative to white), within the FP16 bound. Where the
/// sRGB value is under the black floor I(0) of the law, the floor is shown. The user controls of the gog
/// law are not inverted: a per-gun drive multiplies the sRGB value.
#[test]
fn srgb_source_mode_inverts_the_active_law() {
    let test = "stage_gun srgb inverse";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    // (name, parameters, oracle of the law for its black floor, the exponent that magnifies the FP16
    // error of V in the law (2.4 for the power and BT.1886 laws, 3 below the Annex 1 knee)).
    let mut cases: Vec<(String, Params, Gun, f64)> = Vec::new();
    for gamma in [2.4_f32, 2.2, 1.0, 3.0] {
        let mut p = params(GUN_LAW_POWER);
        set(&mut p, "gun_gamma", ParamValue::F32(gamma));
        cases.push((
            format!("power {gamma}"),
            p,
            Gun {
                gamma: f64::from(gamma),
                ..Gun::default()
            },
            f64::from(gamma),
        ));
    }
    let mut p = params(GUN_LAW_GOG);
    set(&mut p, "gun_gamma", ParamValue::F32(2.2));
    cases.push((
        "gog neutral 2.2".into(),
        p,
        Gun {
            law: Law::Gog,
            gamma: 2.2,
            ..Gun::default()
        },
        2.2,
    ));
    for (lw, lb) in [(100.0_f32, 0.1_f32), (100.0, 0.0), (120.0, 0.05), (100.0, 1.0)] {
        let mut p = params(GUN_LAW_BT1886);
        set(&mut p, "gun_bt1886_white", ParamValue::F32(lw));
        set(&mut p, "gun_bt1886_black", ParamValue::F32(lb));
        cases.push((
            format!("bt1886 Lw {lw} Lb {lb}"),
            p,
            Gun {
                law: Law::Bt1886,
                lw: f64::from(lw),
                lb: f64::from(lb),
                ..Gun::default()
            },
            2.4,
        ));
    }
    for lift in [0.0_f32, 0.05, 0.1, 0.2] {
        let mut p = params(GUN_LAW_BT1886_ANNEX1);
        set(&mut p, "gun_annex1_lift", ParamValue::F32(lift));
        cases.push((
            format!("annex1 b {lift}"),
            p,
            Gun {
                law: Law::Annex1,
                lift: f64::from(lift),
                ..Gun::default()
            },
            3.0,
        ));
    }
    for (name, mut p, oracle, exponent) in cases {
        set(&mut p, "source_eotf", ParamValue::Enum(SOURCE_EOTF_SRGB));
        let r = render_srgb_ramp(&mut gpu, &p);
        let g = gun(&r);
        assert_eq!(g.width, 256 * AMP_K, "{name}: width");
        let floor = oracle.beam_current(0, 0.0);
        for x in 0..256 * AMP_K {
            let k = f64::from(x / AMP_K);
            let want = srgb_eotf(k / 255.0).max(floor);
            for c in 0..3 {
                let got = f64::from(g.pixel(x, 0)[c]);
                assert!(
                    (got - want).abs() <= want * (exponent + 1.0) * 2f64.powi(-10) + STEP,
                    "{name}: code {k} channel {c}: {got} vs sRGB luminance {want}"
                );
            }
        }
        // Code 20 of the plan: sRGB 0.006995 (the shadows are where the power-only inverse failed).
        if floor < 0.5 * 0.006_995 {
            close(
                f64::from(g.pixel(20 * AMP_K, 0)[0]),
                0.006_995,
                0.02,
                &format!("{name}: code 20"),
            );
        }
    }
    // The gog user controls are tube controls, not inverted: drive (0.9, 1, 1.1) multiplies the sRGB value.
    let mut p = params(GUN_LAW_GOG);
    set(&mut p, "source_eotf", ParamValue::Enum(SOURCE_EOTF_SRGB));
    set(&mut p, "gun_drive_rgb", ParamValue::Vec3([0.9, 1.0, 1.1]));
    let r = render_srgb_ramp(&mut gpu, &p);
    let g = gun(&r);
    for x in 0..256 * AMP_K {
        let k = f64::from(x / AMP_K);
        for (c, drive) in [0.9, 1.0, 1.1].into_iter().enumerate() {
            let want = srgb_eotf(k / 255.0) * drive;
            let got = f64::from(g.pixel(x, 0)[c]);
            assert!(
                (got - want).abs() <= want * (2.4 + 1.0) * 2f64.powi(-10) + STEP,
                "gog drive: code {k} channel {c}: {got} vs {want}"
            );
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// The old behaviour of the review finding, as numbers: with BT.1886 (Lb = 0.1, Lw = 100) code 20 gives the
/// sRGB 0.006995 now, not 0.0154; with Annex 1 b = 0 it gives 0.006995 now, not 0.0031.
#[test]
fn srgb_source_mode_code_20_and_128_hit_the_srgb_target_for_every_law() {
    let test = "stage_gun srgb code 20 and 128";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let want20 = srgb_eotf(20.0 / 255.0);
    let want128 = srgb_eotf(128.0 / 255.0);
    assert!((want20 - 0.006_995).abs() < 1e-6 && (want128 - 0.215_861).abs() < 1e-5);
    for law in [GUN_LAW_POWER, GUN_LAW_GOG, GUN_LAW_BT1886, GUN_LAW_BT1886_ANNEX1] {
        let mut p = params(law);
        set(&mut p, "source_eotf", ParamValue::Enum(SOURCE_EOTF_SRGB));
        let r = render_srgb_ramp(&mut gpu, &p);
        let g = gun(&r);
        close(
            f64::from(g.pixel(20 * AMP_K, 0)[0]),
            want20,
            0.015,
            &format!("law {law} code 20"),
        );
        close(
            f64::from(g.pixel(128 * AMP_K, 0)[0]),
            want128,
            0.005,
            &format!("law {law} code 128"),
        );
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// A gamma of 0 or less is clamped to 0.1 in `gun.comp` and in `decode`: same image as 0.1, all finite.
#[test]
fn gamma_out_of_range_is_clamped_in_gun_and_decode() {
    let test = "stage_gun gamma guard";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let codes = all_codes();
    for law in [GUN_LAW_POWER, GUN_LAW_GOG] {
        let mut p = params(law);
        set(&mut p, "gun_gamma", ParamValue::F32(0.1));
        let reference = run(&mut gpu, &p, &codes, true);
        for gamma in [0.0_f32, -2.4, 0.05] {
            set(&mut p, "gun_gamma", ParamValue::F32(gamma));
            let r = run(&mut gpu, &p, &codes, true);
            assert_eq!(gun(&r), gun(&reference), "law {law} gamma {gamma}: gun.comp clamp");
            assert!(
                gun(&r).rgba.iter().all(|v| v.is_finite()),
                "law {law} gamma {gamma}: finite"
            );
        }
        // The srgb source mode (decode inverse, then gun law) with the same guard.
        let mut p = params(law);
        set(&mut p, "source_eotf", ParamValue::Enum(SOURCE_EOTF_SRGB));
        set(&mut p, "gun_gamma", ParamValue::F32(0.1));
        let reference = render_srgb_ramp(&mut gpu, &p);
        for gamma in [0.0_f32, -1.0] {
            set(&mut p, "gun_gamma", ParamValue::F32(gamma));
            let r = render_srgb_ramp(&mut gpu, &p);
            assert_eq!(gun(&r), gun(&reference), "law {law} gamma {gamma}: decode clamp");
            assert_eq!(r.native, reference.native, "law {law} gamma {gamma}: decode V");
            assert!(
                r.native.rgba.iter().chain(gun(&r).rgba.iter()).all(|v| v.is_finite()),
                "law {law} gamma {gamma}: finite"
            );
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Linear sRGB (Rec. 709 primaries, D65) to CIE xy.
fn display_xy(rgb: [f64; 3]) -> (f64, f64) {
    let [x, y, z] = mul3(&rgb_to_xyz(SRGB_RED, SRGB_GREEN, SRGB_BLUE, D65), rgb);
    (x / (x + y + z), y / (x + y + z))
}

/// Renders the source frame through decode, `signal`, `amp`, `gun`, `phosphor`, `optics` and the `encode`
/// output transform (`screen` is off: with the tube placed it draws scanlines, which is not an identity),
/// as FP32 with identity transfers.
fn render_chain(gpu: &mut Headless, bgra: &[u8], size: (u32, u32), p: &Params, target: (u32, u32)) -> Rendered {
    gpu.enable_only(&["signal", "amp", "gun", "phosphor", "optics"])
        .unwrap();
    gpu.render(
        Some(SourceFrame {
            width: size.0,
            height: size.1,
            bgra,
        }),
        View::full(size.0, size.1),
        &Settings::identity(),
        p,
        target,
        TargetFormat::Rgba32F,
    )
    .expect("render")
}

/// Cross-stage (review gate 11): with the tube-faithful encode matrix on (`output_match_srgb` off), a D65
/// SMPTE-C tube retuned to D93 shows a white input as a display white of chromaticity D93 (the module
/// documentation has the hand numbers), and a retune to the tube white is an exact no-op.
#[test]
fn retune_through_the_real_chain_gives_the_target_display_white() {
    let test = "stage_gun retune cross-stage";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let (w, h) = (64u32, 48u32);
    let white = vec![255u8; (w * h * 4) as usize];
    let target = (640u32, 480u32);
    let centre = |r: &Rendered| {
        let px = r.output.pixel(320, 240);
        [f64::from(px[0]), f64::from(px[1]), f64::from(px[2])]
    };
    // Tube-faithful output (the colour matrix is not the identity), exposure 0.9 so that the blue channel
    // of the retuned white (1.0017 before exposure) stays under panel white.
    let mut base = Params::default();
    set(&mut base, "output_match_srgb", ParamValue::Bool(false));
    set(&mut base, "exposure", ParamValue::F32(0.9));
    let m = output_matrix(&base);
    assert!(m[0][1] > 0.04, "the encode matrix is active: {m:?}");

    // Gains off: the tube white (D65) is the display white D65, 0.9 on every channel.
    let off = render_chain(&mut gpu, &white, (w, h), &base, target);
    for v in centre(&off) {
        assert!((v - 0.9).abs() <= 2e-3, "retune off: {:?}", centre(&off));
    }
    assert_xy(display_xy(centre(&off)), [0.3127, 0.3290], 5e-4, "retune off");

    // Retune to D93.
    let mut p93 = base;
    set(&mut p93, "gun_white_balance", ParamValue::Bool(true));
    set(&mut p93, "gun_retune_white_xy", ParamValue::Vec2(D93));
    let r = render_chain(&mut gpu, &white, (w, h), &p93, target);
    let got = centre(&r);
    // By hand (module documentation): sRGB (0.6315, 0.7546, 1.0017) x exposure 0.9.
    for (c, want) in [0.6315, 0.7546, 1.0017].into_iter().enumerate() {
        assert!((got[c] - 0.9 * want).abs() <= 3e-3, "channel {c}: {got:?}");
    }
    // By the oracle: e M g.
    let gains = oracle_gains(&p93);
    let expected = mul3(&m, gains).map(|v| 0.9 * v);
    for c in 0..3 {
        assert!(
            (got[c] - expected[c]).abs() <= 2e-3,
            "channel {c}: {got:?} vs oracle {expected:?}"
        );
    }
    // The display white has the chromaticity of the target (not the double-counted (0.334, 0.511) of the
    // absolute gains).
    assert_xy(display_xy(got), D93, 1e-3, "retune D93 display white");
    assert_eq!(r.clip, ClipCounts::default(), "nothing clipped");

    // Target = tube white: gains exactly 1 and the output equals the gains-off output, bit for bit, for all
    // 256 codes (a ramp) as well as for white.
    let mut same = base;
    set(&mut same, "gun_white_balance", ParamValue::Bool(true));
    assert_eq!(white_gains(&same), [1.0; 3]);
    let on = render_chain(&mut gpu, &white, (w, h), &same, target);
    assert_eq!(
        on.output, off.output,
        "white: target = tube white is the gains-off output"
    );
    let bgra = ramp_bgra(256, 4);
    let ramp_off = render_chain(&mut gpu, &bgra, (256, 4), &base, (256, 4));
    let ramp_on = render_chain(&mut gpu, &bgra, (256, 4), &same, (256, 4));
    assert_eq!(gun(&ramp_on), gun(&ramp_off), "ramp: gun image");
    assert_eq!(ramp_on.output, ramp_off.output, "ramp: output");
    // And the retune is not a no-op elsewhere: the D93 ramp differs.
    let ramp_93 = render_chain(&mut gpu, &bgra, (256, 4), &p93, (256, 4));
    assert_ne!(ramp_93.output, ramp_off.output);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

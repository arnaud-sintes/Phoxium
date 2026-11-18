//! GPU tests of the pipeline core (T-20a, updated by T-34): the decode -> gun -> encode chain through the
//! FP16 images, headless, with the validation layer (synchronization validation on) and zero validation
//! messages.
//!
//! # Identity: gun law = host EOTF
//!
//! The chain on one channel, for code k in 0..=255 (T-34 moved the tube transfer from `decode` to the
//! `gun` slot, the default gun law is the pure power 2.4 of `params/gun.toml`):
//! - `decode`: V = k / 255 (UNORM read), the drive voltage, stored as FP16 in the native raster;
//! - `gun`: L = T(V) with T the gun law (power law of exponent `gun_gamma`), stored as FP16;
//! - `encode`: v = T^-1(min(max(L * exposure, 0), 1)) with exposure = 1 (the `params/output.toml`
//!   default) and the same T as host EOTF, written to the target.
//!
//! For each transfer used here (identity: gun gamma 1, pure power 2.4 and 2.2, and piecewise sRGB per
//! IEC 61966-2-1 through the `srgb` source mode of `decode` with gun gamma 2.4), T^-1(T(V)) = V on [0, 1],
//! so v = k / 255 in exact arithmetic and the output code is k. As before T-34 the live default
//! (`Settings::default`, `Params::default`: gun 2.4, host 2.2) gives round(255 (k / 255)^(2.4 / 2.2)).
//!
//! Error budget (why `round(v * 255) == k` must hold exactly). T-34 adds one FP16 rounding (of V, before
//! the gun law), so the budget is the old one plus the propagated error of V:
//! - FP16 storage: the NVIDIA driver truncates to FP16 (observed in T-09: 254/255 reads 253.879/255), so
//!   for a normal value the relative error is below e = 2^-10; for a subnormal value (< 2^-14) the
//!   absolute error is below 2^-24.
//! - V' = V (1 - e1), e1 <= e. The gun gives L' = V'^g (1 - e2) with e2 <= e: relative error g e1 + e2.
//!   Through the host's T^-1 = L^(1/g) the relative error is divided by g: v' = v (1 - e1 - e2 / g), at
//!   most e (1 + 1 / 2.2) = 1.42e-3 relative, 0.362 of a code at k = 255. The `srgb` source mode adds
//!   nothing worse: dv = (v + 0.055) rel(L) / 2.4 with rel(L) <= 3.4 e gives 0.372 of a code.
//! - An RGBA16F target adds its own truncation: below 255 * 2^-11 = 0.1245 of a code. Total below
//!   0.4968 of a code (tolerance 0.5, and `round` must still give k). A `B8G8R8A8_UNORM` target (the
//!   swapchain format) rounds to the nearest code: error below 0.372, exact.
//! - Before T-34 the budget was 0.24 of a code (one FP16 rounding of L only).
//! - Worst subnormal, code 1 at gamma 2.4: L = (1/255)^2.4 = 1.6761e-6 = 28.12 * 2^-24, stored as
//!   28 * 2^-24 at worst (relative -0.43 %), giving v * 255 = 0.9982: rounds to 1; V' = V (1 - e1)
//!   moves L by 2.4 e1 = 0.23 % more (0.4 % of a code at the end), still rounding to 1.
//!
//! # FP16 subnormals (hand derivation, IEEE 754 binary16)
//!
//! The smallest normal FP16 value is 2^-14 = 6.1035e-5. With L = (k/255)^gamma, L < 2^-14 exactly
//! when k < 255 * 2^(-14/gamma):
//! - gamma 2.4: 255 * 2^-5.8333 = 4.472, so codes 1 to 4 are subnormal; code 5 gives
//!   (5/255)^2.4 = 7.977e-5, normal. (The plan's "codes 1 to 5" counts one code too many.)
//! - gamma 2.2: 255 * 2^-6.3636 = 3.097, so codes 1 to 3 are subnormal; code 4 gives 1.072e-4.
//! - sRGB: the linear toe gives L = k / 255 / 12.92 >= 3.035e-4 for k >= 1: none.
//!
//! A device that flushed FP16 subnormals to zero would store 0 for those codes and the identity would
//! output code 0: the tests read back the `gun` image and assert 0 < L < 2^-14 for them.
//!
//! The linear values are also compared with the `crt-model` oracle (`gun_power`, `srgb_eotf`, f64),
//! not with a copy of the shader formula.

mod common;

use crt_model::colour::{gun_power, srgb_eotf};
use ultimatecrt::gpu::{Headless, Rendered, Settings, SourceFrame, TargetFormat, Transfer, AMP_K};
use ultimatecrt::params::{ParamValue, Params, SOURCE_EOTF_SRGB};
use ultimatecrt::scaling::{Rect, View};

const W: u32 = 256;
const H: u32 = 4;

/// Smallest normal FP16 value.
const FP16_MIN_NORMAL: f64 = 6.103515625e-5; // 2^-14
/// FP16 subnormal step.
const FP16_SUBNORMAL_STEP: f64 = 5.960464477539063e-8; // 2^-24

/// Codes (r, g, b) of pixel (x, y): every channel takes all 256 codes on each row, in different
/// orders, and blue shifts by 64 per row so a row swap or a flip is detected.
fn codes(x: u32, y: u32) -> [u8; 3] {
    [x as u8, (255 - x) as u8, ((x + 64 * y) % 256) as u8]
}

/// A `w` x `h` BGRA8 image with the codes of `codes` (x taken modulo 256), alpha 255.
fn ramp(w: u32, h: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = codes(x % 256, y % 4);
            v.extend_from_slice(&[b, g, r, 255]);
        }
    }
    v
}

/// The oracle: linear light of code `k` through `t`.
fn linear(t: Transfer, k: u8) -> f64 {
    let v = f64::from(k) / 255.0;
    match t {
        Transfer::Identity => v,
        Transfer::Power(g) => gun_power(v, f64::from(g)),
        Transfer::Srgb => srgb_eotf(v),
    }
}

/// The gun parameters that make `t` the tube law: gun gamma 1 for the identity (`gun.comp` returns V
/// unchanged), the exponent for a power law, and the `srgb` source mode with the default gun gamma 2.4
/// for the piecewise sRGB (the tube then shows the sRGB luminance of the code).
fn params_for(t: Transfer) -> Params {
    let mut p = Params::default();
    let ok = match t {
        Transfer::Identity => p.set("gun_gamma", ParamValue::F32(1.0)),
        Transfer::Power(g) => p.set("gun_gamma", ParamValue::F32(g)),
        Transfer::Srgb => p.set("source_eotf", ParamValue::Enum(SOURCE_EOTF_SRGB)),
    };
    assert!(ok);
    p
}

/// Renders the ramp once with the gun law of `t` and `t` as the host EOTF into a `format` target of the
/// native size.
fn render_identity(gpu: &mut Headless, t: Transfer, format: TargetFormat) -> Rendered {
    let settings = Settings {
        source: Transfer::Identity,
        host: t,
        integer_scale: false,
    };
    let bgra = ramp(W, H);
    gpu.render(
        Some(SourceFrame {
            width: W,
            height: H,
            bgra: &bgra,
        }),
        View::full(W, H),
        &settings,
        &params_for(t),
        (W, H),
        format,
    )
    .expect("headless render")
}

/// Asserts that every output code equals its input code, with `|v * 255 - k| <= tol`.
fn assert_identity(out: &ultimatecrt::gpu::ImageF32, what: &str, tol: f32) {
    assert_eq!((out.width, out.height), (W, H), "{what}: size");
    for y in 0..H {
        for x in 0..W {
            let p = out.pixel(x, y);
            for (c, &k) in codes(x, y).iter().enumerate() {
                let v = p[c] * 255.0;
                assert_eq!(
                    v.round(),
                    f32::from(k),
                    "{what}: pixel ({x}, {y}) channel {c}: {k} -> {v}"
                );
                assert!(
                    (v - f32::from(k)).abs() <= tol,
                    "{what}: pixel ({x}, {y}) channel {c}: {k} -> {v}"
                );
            }
            assert_eq!(p[3], 1.0, "{what}: pixel ({x}, {y}) alpha");
        }
    }
}

/// The oracle: drive voltage V of code `k` (what `decode` stores): k / 255, or for the `srgb` source mode
/// the inverse of the gun power law of exponent 2.4 applied to the sRGB luminance.
fn drive(t: Transfer, k: u8) -> f64 {
    match t {
        Transfer::Srgb => srgb_eotf(f64::from(k) / 255.0).powf(1.0 / 2.4),
        _ => f64::from(k) / 255.0,
    }
}

/// Exponent of the gun law behind `t` (relative error amplification of V, see the error budget).
fn gun_exponent(t: Transfer) -> f64 {
    match t {
        Transfer::Identity => 1.0,
        Transfer::Power(g) => f64::from(g),
        Transfer::Srgb => 2.4,
    }
}

/// Asserts that the native raster holds the drive voltage V within FP16 precision (the `decode` contract:
/// the gun law is not applied there any more).
fn assert_native_drive(r: &Rendered, t: Transfer, what: &str) {
    assert_eq!((r.native.width, r.native.height), (W, H), "{what}: native size");
    for y in 0..H {
        for x in 0..W {
            let p = r.native.pixel(x, y);
            for (c, &k) in codes(x, y).iter().enumerate() {
                let want = drive(t, k);
                let got = f64::from(p[c]);
                // Truncation or rounding to FP16: below one ULP, 2^-10 relative (normal) or 2^-24.
                let tol = (want * 2f64.powi(-10)).max(FP16_SUBNORMAL_STEP);
                assert!(
                    (got - want).abs() <= tol,
                    "{what}: native ({x}, {y}) channel {c}: code {k} -> {got}, V oracle {want}"
                );
            }
        }
    }
}

/// The `gun` slot image of a render: the beam current, K texels wide per native texel.
fn gun_image(r: &Rendered) -> &ultimatecrt::gpu::ImageF32 {
    let g = r.stage("gun").expect("gun slot ran");
    assert_eq!((g.width, g.height), (W * AMP_K, H), "gun image size");
    g
}

/// Asserts that the `gun` image holds the oracle's linear values within FP16 precision: the error of V
/// (rel. e) amplified by the exponent g, plus the truncation of the result: (g + 1) e relative, or a
/// subnormal step of absolute error on top.
fn assert_gun_linear(r: &Rendered, t: Transfer, what: &str) {
    let gun = gun_image(r);
    let rel = (gun_exponent(t) + 1.0) * 2f64.powi(-10);
    for y in 0..H {
        for x in 0..W * AMP_K {
            let p = gun.pixel(x, y);
            for (c, &k) in codes(x / AMP_K, y).iter().enumerate() {
                let want = linear(t, k);
                let got = f64::from(p[c]);
                let tol = want * rel + FP16_SUBNORMAL_STEP;
                assert!(
                    (got - want).abs() <= tol,
                    "{what}: gun ({x}, {y}) channel {c}: code {k} -> {got}, oracle {want}"
                );
            }
        }
    }
}

/// The FP16 value the `gun` slot stored for code `k` on the red channel (pixel x = k of row 0).
fn gun_red(r: &Rendered, k: u8) -> f64 {
    f64::from(gun_image(r).pixel(u32::from(k) * AMP_K, 0)[0])
}

fn identity_case(t: Transfer, subnormal_codes: &[u8]) {
    let test = format!("gpu_identity {t:?}");
    let Some(mut gpu) = common::gpu_or_skip(&test) else {
        return;
    };
    let counter = gpu.validation_counter();
    eprintln!("{test}: on {}", gpu.context().gpu.name);
    let half = render_identity(&mut gpu, t, TargetFormat::Rgba16F);
    let unorm = render_identity(&mut gpu, t, TargetFormat::Bgra8Unorm);
    drop(gpu);

    assert_identity(&half.output, &format!("{test} RGBA16F"), 0.5);
    assert_identity(&unorm.output, &format!("{test} BGRA8"), 0.0);
    assert_native_drive(&half, t, &test);
    assert_gun_linear(&half, t, &test);
    assert_eq!(
        half.native, unorm.native,
        "{test}: the native raster does not depend on the target"
    );

    // Hand-derived subnormal codes: the oracle agrees they are below 2^-14 (and the next code is not),
    // and the GPU stored them as non-zero subnormals.
    for &k in subnormal_codes {
        assert!(linear(t, k) < FP16_MIN_NORMAL, "{test}: code {k} should be subnormal");
        let got = gun_red(&half, k);
        eprintln!(
            "{test}: code {k} stored as {got:e} = {} * 2^-24 (oracle {:e})",
            got / FP16_SUBNORMAL_STEP,
            linear(t, k)
        );
        assert!(
            got > 0.0 && got < FP16_MIN_NORMAL,
            "{test}: code {k} stored as {got}, expected a non-zero FP16 subnormal (flushed to zero?)"
        );
    }
    let next = subnormal_codes.last().map_or(1, |k| k + 1);
    assert!(
        linear(t, next) >= FP16_MIN_NORMAL,
        "{test}: code {next} should be normal"
    );
    common::assert_no_validation_messages(&counter);
}

#[test]
fn identity_gamma_2_4_all_codes_exact_and_subnormals_kept() {
    identity_case(Transfer::Power(2.4), &[1, 2, 3, 4]);
}

#[test]
fn identity_gamma_2_2_all_codes_exact_and_subnormals_kept() {
    identity_case(Transfer::Power(2.2), &[1, 2, 3]);
}

#[test]
fn identity_srgb_all_codes_exact() {
    identity_case(Transfer::Srgb, &[]);
}

#[test]
fn identity_without_transfer_all_codes_exact() {
    identity_case(Transfer::Identity, &[]);
}

/// Pixel (x, y) of a BGRA8 image of width `w`, as RGB codes.
fn rgb_at(bgra: &[u8], w: u32, x: u32, y: u32) -> [u8; 3] {
    let i = ((y * w + x) * 4) as usize;
    [bgra[i + 2], bgra[i + 1], bgra[i]]
}

/// Asserts that `out` (BGRA8 target, values k / 255) equals the native raster `expect(x, y)`, inside
/// the tube rectangle starting at (`x0`, 0) with the native size, and black outside.
fn assert_frame(out: &ultimatecrt::gpu::ImageF32, native: (u32, u32), x0: u32, expect: impl Fn(u32, u32) -> [u8; 3]) {
    for y in 0..out.height {
        for x in 0..out.width {
            let p = out.pixel(x, y);
            let got = [p[0], p[1], p[2]].map(|v| (v * 255.0).round() as u8);
            let inside = x >= x0 && x < x0 + native.0 && y < native.1;
            let want = if inside { expect(x - x0, y) } else { [0, 0, 0] };
            assert_eq!(got, want, "pixel ({x}, {y}), inside the tube: {inside}");
            assert_eq!(p[3], 1.0, "pixel ({x}, {y}) alpha");
        }
    }
}

/// Frames in flight: six frames submitted back to back (each waits only for its own slot), with new
/// uploads, a frame without upload and the tube gamma path, so successive frames overlap on the GPU
/// and share the source and native images; synchronization validation must stay silent and the last
/// frame must be exact.
#[test]
fn frames_in_flight_burst_with_silent_sync_validation() {
    let test = "gpu_identity frames in flight";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let a = ramp(W, H);
    // `b` is `a` mirrored horizontally, so a stale upload is detected.
    let mut b = Vec::with_capacity(a.len());
    for y in 0..H {
        for x in (0..W).rev() {
            let i = ((y * W + x) * 4) as usize;
            b.extend_from_slice(&a[i..i + 4]);
        }
    }
    let fa = SourceFrame {
        width: W,
        height: H,
        bgra: &a,
    };
    let fb = SourceFrame {
        width: W,
        height: H,
        bgra: &b,
    };
    let v = View::full(W, H);
    let frames = [
        (Some(fa), v),
        (Some(fb), v),
        (None, v),
        (Some(fa), v),
        (None, v),
        (Some(fb), v),
    ];
    let t = Transfer::Power(2.4);
    let settings = Settings {
        source: Transfer::Identity,
        host: t,
        integer_scale: false,
    };
    let r = gpu
        .render_burst(&frames, &settings, &params_for(t), (W, H), TargetFormat::Bgra8Unorm)
        .expect("headless burst");
    drop(gpu);
    for y in 0..H {
        for x in 0..W {
            let p = r.output.pixel(x, y);
            let got = [p[0], p[1], p[2]].map(|v| (v * 255.0).round() as u8);
            assert_eq!(got, codes(W - 1 - x, y), "pixel ({x}, {y})");
        }
    }
    common::assert_no_validation_messages(&counter);
}

/// Native size change 256x240 -> 320x240 -> 256x240 on one pipeline (reallocation of the source and
/// native images), frames in flight reused, a frame without a new upload, a 2x upscaled capture read
/// at block centres, and a black surround: validation silent throughout.
#[test]
fn native_size_changes_and_surround_with_silent_validation() {
    let test = "gpu_identity native size change";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let params = Params::identity_gun();
    let settings = Settings::identity();
    let mut render = |w: u32, h: u32, bgra: Option<&[u8]>, view: View, target: (u32, u32)| {
        gpu.render(
            bgra.map(|b| SourceFrame {
                width: w,
                height: h,
                bgra: b,
            }),
            view,
            &settings,
            &params,
            target,
            TargetFormat::Bgra8Unorm,
        )
        .expect("headless render")
    };

    let a = ramp(256, 240);
    let b = ramp(320, 240);
    let r = render(256, 240, Some(&a), View::full(256, 240), (256, 240));
    assert_frame(&r.output, (256, 240), 0, |x, y| rgb_at(&a, 256, x, y));
    let r = render(320, 240, Some(&b), View::full(320, 240), (320, 240));
    assert_eq!((r.native.width, r.native.height), (320, 240));
    assert_frame(&r.output, (320, 240), 0, |x, y| rgb_at(&b, 320, x, y));
    // No new frame: the last upload is decoded again (the other frame slot this time).
    let r = render(320, 240, None, View::full(320, 240), (320, 240));
    assert_frame(&r.output, (320, 240), 0, |x, y| rgb_at(&b, 320, x, y));
    // Back to 256x240, into a wider target: scale min(300/256, 240/240) = 1, so the tube rectangle is
    // 256x240 at x0 = floor((300 - 256) / 2) = 22, black elsewhere.
    let r = render(256, 240, Some(&a), View::full(256, 240), (300, 240));
    assert_eq!((r.native.width, r.native.height), (256, 240));
    assert_frame(&r.output, (256, 240), 22, |x, y| rgb_at(&a, 256, x, y));

    // A 2x nearest-neighbour upscale of `a` with a 1-pixel odd border on the right and bottom (the
    // centre of block p is 2p + 1): decoding with divisor 2 gives back `a`.
    let (uw, uh) = (513, 481);
    let mut up = vec![0u8; (uw * uh * 4) as usize];
    for y in 0..uh {
        for x in 0..uw {
            let [r, g, b] = rgb_at(&a, 256, (x / 2).min(255), (y / 2).min(239));
            let i = ((y * uw + x) * 4) as usize;
            up[i..i + 4].copy_from_slice(&[b, g, r, 255]);
        }
    }
    let view = View::new(
        Rect {
            x: 0,
            y: 0,
            w: 512,
            h: 480,
        },
        (2, 2),
    );
    assert_eq!(view.native, (256, 240));
    let r = render(uw, uh, Some(&up), view, (256, 240));
    assert_frame(&r.output, (256, 240), 0, |x, y| rgb_at(&a, 256, x, y));
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

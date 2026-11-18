//! GPU tests of the pipeline core (T-20a): the decode -> encode chain through the FP16 native raster,
//! headless, with the validation layer (synchronization validation on) and zero validation messages.
//!
//! # Identity: tube gamma = host gamma
//!
//! The chain on one channel, for code k in 0..=255:
//! - `decode`: V = k / 255 (UNORM read), L = T(V) with the source transfer T, stored as FP16;
//! - `encode`: v = T^-1(min(max(L * exposure, 0), 1)) with exposure = 1 (the `params/output.toml`
//!   default) and the same T as host EOTF, written to the target.
//!
//! For each transfer used here (identity, pure power 2.4 and 2.2, piecewise sRGB per IEC 61966-2-1),
//! T^-1(T(V)) = V on [0, 1], so v = k / 255 in exact arithmetic and the output code is k.
//!
//! Error budget (why `round(v * 255) == k` must hold exactly):
//! - FP16 storage of L: the NVIDIA driver truncates to FP16 (observed in T-09: 254/255 reads
//!   253.879/255), so for a normal L the relative error is below 2^-10; for a subnormal L
//!   (L < 2^-14) the absolute error is below 2^-24.
//! - Through T^-1 = L^(1/gamma) the relative error is divided by gamma: below 2^-10 / 2.2 = 4.4e-4,
//!   at most 0.113 of a code at k = 255.
//! - An RGBA16F target adds its own truncation: below 255 * 2^-11 = 0.1245 of a code.
//!   Total below 0.24 of a code, so rounding gives k. A `B8G8R8A8_UNORM` target (the swapchain
//!   format) rounds to the nearest code: exact.
//! - Worst subnormal, code 1 at gamma 2.4: L = (1/255)^2.4 = 1.6761e-6 = 28.12 * 2^-24, stored as
//!   28 * 2^-24 at worst (relative -0.43 %), giving v * 255 = 0.9982: rounds to 1.
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
//! output code 0: the tests read back the native raster and assert 0 < L < 2^-14 for them.
//!
//! The linear values are also compared with the `crt-model` oracle (`gun_power`, `srgb_eotf`, f64),
//! not with a copy of the shader formula.

mod common;

use crt_model::colour::{gun_power, srgb_eotf};
use ultimatecrt::gpu::{Headless, Rendered, Settings, SourceFrame, TargetFormat, Transfer};
use ultimatecrt::params::Params;
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

/// Renders the ramp once with `t` on both sides into a `format` target of the native size.
fn render_identity(gpu: &mut Headless, t: Transfer, format: TargetFormat) -> Rendered {
    let settings = Settings {
        source: t,
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
        &Params::default(),
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

/// Asserts that the native raster holds the oracle's linear values within FP16 precision.
fn assert_native_linear(r: &Rendered, t: Transfer, what: &str) {
    assert_eq!((r.native.width, r.native.height), (W, H), "{what}: native size");
    for y in 0..H {
        for x in 0..W {
            let p = r.native.pixel(x, y);
            for (c, &k) in codes(x, y).iter().enumerate() {
                let want = linear(t, k);
                let got = f64::from(p[c]);
                // Truncation or rounding to FP16: below one ULP, 2^-10 relative (normal) or 2^-24.
                let tol = (want * 2f64.powi(-10)).max(FP16_SUBNORMAL_STEP);
                assert!(
                    (got - want).abs() <= tol,
                    "{what}: native ({x}, {y}) channel {c}: code {k} -> {got}, oracle {want}"
                );
            }
        }
    }
}

/// The FP16 value stored for code `k` on the red channel (pixel x = k of row 0).
fn native_red(r: &Rendered, k: u8) -> f64 {
    f64::from(r.native.pixel(u32::from(k), 0)[0])
}

fn identity_case(t: Transfer, subnormal_codes: &[u8]) {
    let test = format!("gpu_identity {t:?}");
    let Some(mut gpu) = common::gpu_or_skip(&test) else {
        return;
    };
    let counter = gpu.validation_counter();
    eprintln!("{test}: on {}", gpu.context().gpu.name);
    // These tests are about decode and encode: since T-36a the beam draws scanlines once the tube is
    // placed, so every chain slot is bypassed (a bypassed slot is an exact identity, A11).
    gpu.enable_only(&[]).expect("bypass every slot");
    let half = render_identity(&mut gpu, t, TargetFormat::Rgba16F);
    let unorm = render_identity(&mut gpu, t, TargetFormat::Bgra8Unorm);
    drop(gpu);

    assert_identity(&half.output, &format!("{test} RGBA16F"), 0.25);
    assert_identity(&unorm.output, &format!("{test} BGRA8"), 0.0);
    assert_native_linear(&half, t, &test);
    assert_eq!(
        half.native, unorm.native,
        "{test}: the native raster does not depend on the target"
    );

    // Hand-derived subnormal codes: the oracle agrees they are below 2^-14 (and the next code is not),
    // and the GPU stored them as non-zero subnormals.
    for &k in subnormal_codes {
        assert!(linear(t, k) < FP16_MIN_NORMAL, "{test}: code {k} should be subnormal");
        let got = native_red(&half, k);
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
    // Decode and encode only: the beam (T-36a) is a separate stage with its own tests.
    gpu.enable_only(&[]).expect("bypass every slot");
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
        source: t,
        host: t,
        integer_scale: false,
    };
    let r = gpu
        .render_burst(&frames, &settings, &Params::default(), (W, H), TargetFormat::Bgra8Unorm)
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
    // Decode and encode only: since T-36b the beam's spot spills past the raster's ends (stage tests
    // `stage_beam_*`), so every slot is bypassed (an exact identity, A11).
    gpu.enable_only(&[]).expect("bypass every slot");
    let params = Params::default();
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

/// Code (r, g, b) of pixel (x, y) in the cropped and scaled cases: every neighbour differs in every
/// channel, so an off-by-one in the origin, the block centre or the resampling shows.
fn pix(x: u32, y: u32) -> [u8; 3] {
    [
        (x * 17 + y * 3) as u8,
        (x * 5 + y * 29 + 40) as u8,
        (x * 11 + y * 7 + 100) as u8,
    ]
}

/// A `w` x `h` BGRA8 image of [`pix`].
fn pix_image(w: u32, h: u32) -> Vec<u8> {
    let mut v = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = pix(x, y);
            v.extend_from_slice(&[b, g, r, 255]);
        }
    }
    v
}

/// Renders `bgra` (`w` x `h`) with identity transfers into an 8-bit target and returns the output.
fn render_pix(
    gpu: &mut Headless,
    frame: Option<(u32, u32)>,
    view: View,
    settings: &Settings,
    target: (u32, u32),
) -> Rendered {
    let bgra = frame.map(|(w, h)| pix_image(w, h));
    gpu.render(
        frame
            .zip(bgra.as_deref())
            .map(|((width, height), bgra)| SourceFrame { width, height, bgra }),
        view,
        settings,
        &Params::default(),
        target,
        TargetFormat::Bgra8Unorm,
    )
    .expect("headless render")
}

/// Cropped views (non-zero `View.origin`): native pixel p is read at `origin + p * div + div / 2`.
/// Source 14x9; origin (3, 2).
/// - divisor 1, native 5x3: native (x, y) = source (3 + x, 2 + y);
/// - divisor 2, native 4x3: native (x, y) = source (3 + 2x + 1, 2 + 2y + 1) (block centres).
///
/// The second frame of each view reuses the upload (no new frame).
#[test]
fn cropped_views_read_the_source_at_the_origin() {
    let test = "gpu_identity cropped view";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    // Decode and encode only: the beam (T-36b: spot spill at the raster's ends) has its own tests.
    gpu.enable_only(&[]).expect("bypass every slot");
    let settings = Settings::identity();
    let a = View {
        origin: (3, 2),
        div: (1, 1),
        native: (5, 3),
    };
    let b = View {
        origin: (3, 2),
        div: (2, 2),
        native: (4, 3),
    };
    for (view, name, expect) in [
        (a, "divisor 1", (|x, y| pix(3 + x, 2 + y)) as fn(u32, u32) -> [u8; 3]),
        (b, "divisor 2", |x, y| pix(3 + 2 * x + 1, 2 + 2 * y + 1)),
    ] {
        let first = render_pix(&mut gpu, Some((14, 9)), view, &settings, view.native);
        let again = render_pix(&mut gpu, None, view, &settings, view.native);
        assert_eq!(first.output, again.output, "{name}: reused upload");
        assert_eq!(
            (first.native.width, first.native.height),
            view.native,
            "{name}: native size"
        );
        assert_frame(&first.output, view.native, 0, expect);
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Native 4x3 raster into larger targets, every chain slot off (bypassed: the tube image is the nearest
/// resample of the native raster, see `resample.comp`). The tube pixel q of a tube rectangle takes the
/// amp texel floor((q + 0.5) * 16 / tube_w) (native -> amp repeats each texel K = 4 times) and the native
/// texel amp / 4 horizontally, and the native row floor((q + 0.5) * 3 / tube_h).
///
/// - 2x (integer scale on a 10x8 target): tube rectangle 8x6 at (1, 1); column q = native q / 2 and row
///   q = native q / 2, since the amp texel is 2q + 1.
/// - 4.5x (18x20 target, not integer): tube rectangle 18 wide, round(13.5) = 14 high at x0 = 0,
///   y0 = (20 - 14) / 2 = 3. Amp texel of column q = floor((2q + 1) * 4 / 9) = 0, 1, 2, 3, 4, 4, 5, 6, 7,
///   8, 9, 10, 11, 12, 12, 13, 14, 15 for q = 0..17 (q = 4 and 13 sit exactly on a texel boundary, the
///   upper texel wins), so the native columns are 0 x4, 1 x5, 2 x4, 3 x5. Row of q = floor((2q + 1) * 3 / 28)
///   = 0 x5, 1 x4, 2 x5 for q = 0..13.
#[test]
fn scaled_targets_resample_the_native_raster() {
    let test = "gpu_identity scaled target";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.enable_only(&[]).expect("bypass every slot");
    let view = View::full(4, 3);

    let integer = Settings {
        integer_scale: true,
        ..Settings::identity()
    };
    let r = render_pix(&mut gpu, Some((4, 3)), view, &integer, (10, 8));
    assert_scaled(&r.output, (1, 1, 8, 6), |q| q / 2, |q| q / 2);

    let fractional = Settings::identity();
    let cols = [0, 0, 0, 0, 1, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 3];
    let rows = [0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 2];
    let r = render_pix(&mut gpu, Some((4, 3)), view, &fractional, (18, 20));
    assert_scaled(&r.output, (0, 3, 18, 14), |q| cols[q as usize], |q| rows[q as usize]);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Asserts that `out` holds [`pix`] of native (`col(qx)`, `row(qy)`) at tube pixel (qx, qy) of the tube
/// rectangle (x0, y0, w, h), and black elsewhere.
fn assert_scaled(
    out: &ultimatecrt::gpu::ImageF32,
    rect: (u32, u32, u32, u32),
    col: impl Fn(u32) -> u32,
    row: impl Fn(u32) -> u32,
) {
    let (x0, y0, w, h) = rect;
    for y in 0..out.height {
        for x in 0..out.width {
            let p = out.pixel(x, y);
            let got = [p[0], p[1], p[2]].map(|v| (v * 255.0).round() as u8);
            let inside = x >= x0 && x < x0 + w && y >= y0 && y < y0 + h;
            let want = if inside {
                pix(col(x - x0), row(y - y0))
            } else {
                [0, 0, 0]
            };
            assert_eq!(got, want, "pixel ({x}, {y}), inside the tube: {inside}");
        }
    }
}

/// A power-law exponent that is not finite and positive is refused (the shaders would compute
/// `pow(l, 1 / g)`), and the refusal loses nothing: the frame staged by the refused call is still
/// uploaded by the next valid one.
#[test]
fn invalid_power_exponent_is_refused_and_keeps_the_staged_frame() {
    let test = "gpu_identity invalid exponent";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let view = View::full(5, 4);
    for g in [0.0, -2.2, f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1e-45] {
        for (source, host) in [
            (Transfer::Power(g), Transfer::Identity),
            (Transfer::Identity, Transfer::Power(g)),
        ] {
            let s = Settings {
                source,
                host,
                integer_scale: false,
            };
            assert!(s.validate().is_err(), "exponent {g} accepted");
            let bgra = pix_image(5, 4);
            let frame = SourceFrame {
                width: 5,
                height: 4,
                bgra: &bgra,
            };
            let r = gpu.render(
                Some(frame),
                view,
                &s,
                &Params::default(),
                (5, 4),
                TargetFormat::Bgra8Unorm,
            );
            assert!(r.is_err(), "exponent {g}: frame rendered");
        }
    }
    assert!(Settings::default().validate().is_ok());
    // No new upload: the frame of a refused call was not lost.
    let r = render_pix(&mut gpu, None, view, &Settings::identity(), (5, 4));
    assert_frame(&r.output, (5, 4), 0, pix);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

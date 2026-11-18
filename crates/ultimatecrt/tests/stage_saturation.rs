//! Phosphor saturation (T-40): `shaders/saturation.comp`, `shaders/phosphor.comp` (saturation before the
//! persistence history), `src/gpu/stages/phosphor.rs`, `params/phosphor.toml`, against the oracle
//! `crt_model::phosphor::saturate` and the hand-derived values below.
//!
//! # Model (CRT.md "Saturation and ageing", QUESTIONS PHY-14)
//!
//! Per channel, on the light I >= 0 of a tube pixel (1 = white) with the strength s of
//! `phosphor_saturation_rgb`:
//!
//! L = f(I) = I / (1 + s I)
//!
//! f(0) = 0, f'(I) = 1 / (1 + s I)^2, so f'(0) = 1 (small-signal gain 1), f' > 0 (strictly increasing),
//! f(I) < 1 / s and f -> 1 / s (finite maximum light). The relative loss at I is 1 - f / I = s I / (1 + s I).
//! There is no hard knee: "unchanged below the knee" reads as a relative loss below s I (1e-4 at s I = 1e-4,
//! which an FP16 output, resolution 2^-10 relative, cannot show; the oracle test checks the formula).
//!
//! # Hand-derived values (s = (0.25, 0.5, 1.0) for r, g, b: s = 1/4, 1/2, 1)
//!
//! | I   | red (s = 1/4)      | green (s = 1/2)    | blue (s = 1)      |
//! |-----|--------------------|--------------------|-------------------|
//! | 0.5 | 0.5 / 1.125 = 4/9  | 0.5 / 1.25 = 0.4   | 0.5 / 1.5 = 1/3   |
//! | 1   | 1 / 1.25 = 0.8     | 1 / 1.5 = 2/3      | 1 / 2 = 0.5       |
//! | 2   | 2 / 1.5 = 4/3      | 2 / 2 = 1          | 2 / 3 = 2/3       |
//! | 4   | 4 / 2 = 2          | 4 / 3 = 4/3        | 4 / 5 = 0.8       |
//!
//! Limits 1 / s = 4, 2, 1 (I = 64: 64 / 17 = 3.7647, 64 / 33 = 1.9394, 64 / 65 = 0.98462).
//!
//! Default strengths (0.01, 0.03, 0.06): white I = 1 gives 1 / 1.01 = 0.990099, 1 / 1.03 = 0.970874,
//! 1 / 1.06 = 0.943396; blue relative to red 1.01 / 1.06 = 0.952830 (a 4.7 % warm shift of the white); a
//! highlight I = 4 gives 4 / 1.04 = 3.84615, 4 / 1.12 = 3.57143, 4 / 1.24 = 3.22581: blue / red 0.83871.
//!
//! # Order with the persistence (derived by hand)
//!
//! The stage computes x' = f(x) first, then the recurrence of T-41 on x'. So for a single excitation X
//! followed by black (f(0) = 0): y_0 = w0 f(X), y_k = w0 f(X) t_k, i.e. y_k / y_0 = t_k = E_k / E_0 whatever
//! X is. With the opposite order (persistence, then saturation) y_k / y_0 = f(w0 t_k X) / f(w0 X)
//! = t_k (1 + s w0 X) / (1 + s w0 t_k X), which for blue s = 1, X = 3, w0 = 0.788 and t_1 = 0.0472 is
//! 0.0472 x 3.365 / 1.1115 = 0.143: three times the persistence ratio, which this test would catch. A
//! static picture: S_j -> c_j f(X) / (1 - rho_j), y -> w0 (1 + sum_j c_j / (1 - rho_j)) f(X) = f(X) (the
//! persistence has steady-state gain 1, T-41).
//!
//! # Tolerances
//!
//! The output image is FP16 and FP16 stores truncate on the dev GPU: a value is within 2^-10 relative
//! (one FP16 step) below its exact value; a ratio of two stored values within 2 x 2^-10.

mod common;

use crt_model::phosphor::{
    saturate, saturate_rgb, DecayFit, FrameFeedback, KUHN_BLUE, KUHN_GREEN, KUHN_RED, SATURATION_DEFAULT,
    TRAIL_HORIZON_MS,
};
use crt_model::units::frame_period_ms;
use ultimatecrt::gpu::{FrameConstants, Headless, ImageF32, Settings, StageInput, StageRun, TargetFormat};
use ultimatecrt::params::Params;

/// One FP16 step relative (truncating stores).
const FP16: f64 = 1.0 / 1024.0;
/// Strengths of the hand-derived table: 1/4, 1/2, 1.
const S: [f32; 3] = [0.25, 0.5, 1.0];

fn frame(w: u32, h: u32) -> FrameConstants {
    let fc = FrameConstants::new((w, h), (w, h), false);
    assert_eq!(fc.tube_size, [w as i32, h as i32]);
    fc
}

/// Tube-domain light: every texel (r, g, b) of `px(x, y)`.
fn image(w: u32, h: u32, px: impl Fn(u32, u32) -> [f32; 3]) -> ImageF32 {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = px(x, y);
            rgba.extend_from_slice(&[r, g, b, 1.0]);
        }
    }
    ImageF32 {
        width: w,
        height: h,
        rgba,
    }
}

fn uniform(w: u32, h: u32, rgb: [f32; 3]) -> ImageF32 {
    image(w, h, |_, _| rgb)
}

fn params(saturation: bool, strengths: [f32; 3], persistence: bool) -> Params {
    Params {
        phosphor_saturation: saturation,
        phosphor_saturation_rgb: strengths,
        phosphor_persistence: persistence,
        phosphor_frame_rate_hz: 60.0,
        phosphor_tail_strength_rgb: [1.0; 3],
        ..Params::default()
    }
}

/// One frame of `phosphor` alone on `input` (injected after `screen`); its output image.
fn run(gpu: &mut Headless, params: &Params, input: &ImageF32) -> ImageF32 {
    gpu.enable_only(&["phosphor"]).unwrap();
    let settings = Settings::identity();
    let r = gpu
        .render_stages(&StageRun {
            input: StageInput::Image {
                after: "screen",
                image: input,
            },
            frame: frame(input.width, input.height),
            settings: &settings,
            params,
            format: TargetFormat::Rgba32F,
        })
        .expect("render_stages");
    r.stage("phosphor").expect("phosphor output").clone()
}

fn rel(a: f64, b: f64) -> f64 {
    ((a - b) / b).abs()
}

fn texel(img: &ImageF32, x: u32, y: u32, ch: usize) -> f64 {
    f64::from(img.rgba[4 * (y * img.width + x) as usize + ch])
}

/// Both switches off (the defaults), or saturation off with strengths set: an exact copy, also after a
/// run with saturation on, and with the strengths 0 the curve is the identity.
#[test]
fn disabled_is_bit_exact_identity() {
    let Some(mut gpu) = common::gpu_or_skip("disabled_is_bit_exact_identity") else {
        return;
    };
    let counter = gpu.validation_counter();
    let d = Params::default();
    assert!(!d.phosphor_saturation, "saturation is off by default");
    let pic = image(8, 4, |x, y| match (x + y) % 3 {
        0 => [1.0, 1.0, 1.0],
        1 => [0.5, 0.75, 0.25],
        _ => [2.0, 3.0, 1.5],
    });
    assert_eq!(run(&mut gpu, &d, &pic), pic, "defaults: exact copy");
    assert_eq!(
        run(&mut gpu, &params(false, S, false), &pic),
        pic,
        "off with strengths set: exact copy"
    );
    let on = run(&mut gpu, &params(true, S, false), &pic);
    assert_ne!(on, pic, "on: the picture changes");
    assert_eq!(
        run(&mut gpu, &params(false, S, false), &pic),
        pic,
        "off after on: exact copy"
    );
    // On with s = 0: the identity (within one FP16 step; the shader divides by exactly 1).
    let zero = run(&mut gpu, &params(true, [0.0; 3], false), &pic);
    for i in 0..pic.rgba.len() {
        assert!(
            rel(f64::from(zero.rgba[i]), f64::from(pic.rgba[i])) <= FP16,
            "s = 0 texel {i}: {} vs {}",
            zero.rgba[i],
            pic.rgba[i]
        );
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// The curve against the hand table and the oracle, per channel, at several currents (row 0 of the
/// picture; the columns are I = 0, 2^-10, 2^-6, 0.5, 1, 2, 4, 64 on all three channels), and the
/// small-signal gain: f(I) / I within s I + one FP16 step of 1 at the small currents.
#[test]
fn curve_values_match_oracle_and_hand_table() {
    let Some(mut gpu) = common::gpu_or_skip("curve_values_match_oracle_and_hand_table") else {
        return;
    };
    let counter = gpu.validation_counter();
    const I: [f32; 8] = [0.0, 1.0 / 1024.0, 1.0 / 64.0, 0.5, 1.0, 2.0, 4.0, 64.0];
    let pic = image(8, 4, |x, _| [I[x as usize]; 3]);
    let out = run(&mut gpu, &params(true, S, false), &pic);
    // Hand table: columns 0.5, 1, 2, 4 (indices 3 to 6) per channel, and I = 64 (index 7).
    let hand: [[f64; 5]; 3] = [
        [4.0 / 9.0, 0.8, 4.0 / 3.0, 2.0, 64.0 / 17.0],
        [0.4, 2.0 / 3.0, 1.0, 4.0 / 3.0, 64.0 / 33.0],
        [1.0 / 3.0, 0.5, 2.0 / 3.0, 0.8, 64.0 / 65.0],
    ];
    for ch in 0..3 {
        let s = f64::from(S[ch]);
        for (col, &i) in I.iter().enumerate() {
            let i = f64::from(i);
            let y = texel(&out, col as u32, 2, ch);
            let want = saturate(i, s);
            if i == 0.0 {
                assert_eq!(y, 0.0, "f(0) = 0, channel {ch}");
                continue;
            }
            assert!(rel(y, want) <= 1.05 * FP16, "ch {ch} I {i}: {y} vs oracle {want}");
            if col >= 3 {
                let h = hand[ch][col - 3];
                assert!(rel(want, h) < 1e-12, "oracle vs hand table, ch {ch} I {i}");
                assert!(rel(y, h) <= 1.05 * FP16, "ch {ch} I {i}: {y} vs hand {h}");
            }
            if i < 0.1 {
                // Small signal: gain 1 to first order, f / I = 1 / (1 + s I) in [1 - s I, 1].
                let gain = y / i;
                assert!((gain - 1.0).abs() <= s * i + 2.0 * FP16, "ch {ch} I {i}: gain {gain}");
            }
        }
        // Every row is the same (the image is constant down the columns).
        for col in 0..8 {
            assert_eq!(texel(&out, col, 0, ch), texel(&out, col, 3, ch));
        }
    }
    // The alpha passes unchanged.
    assert!(out.rgba.chunks(4).all(|p| p[3] == 1.0));
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Monotonic and bounded: 128 increasing currents (i / 16, then 8 i), every channel non-decreasing and
/// below 1 / s, blue below green below red above zero.
#[test]
fn monotonic_and_bounded() {
    let Some(mut gpu) = common::gpu_or_skip("monotonic_and_bounded") else {
        return;
    };
    let counter = gpu.validation_counter();
    for scale in [1.0 / 16.0, 8.0] {
        let level = |i: u32| scale * (i + 1) as f32;
        let pic = image(16, 8, |x, y| [level(y * 16 + x); 3]);
        let out = run(&mut gpu, &params(true, S, false), &pic);
        for (ch, &s) in S.iter().enumerate() {
            let limit = 1.0 / f64::from(s);
            let mut prev = 0.0;
            for i in 0..128 {
                let y = texel(&out, i % 16, i / 16, ch);
                assert!(y >= prev, "scale {scale} ch {ch} index {i}: {y} < {prev}");
                assert!(y <= limit, "scale {scale} ch {ch} index {i}: {y} above 1 / s = {limit}");
                prev = y;
            }
            // The whole curve moves: the last is well above the first.
            assert!(prev > texel(&out, 0, 0, ch));
        }
        for i in 0..128 {
            let (r, g, b) = (
                texel(&out, i % 16, i / 16, 0),
                texel(&out, i % 16, i / 16, 1),
                texel(&out, i % 16, i / 16, 2),
            );
            assert!(b < g && g < r, "index {i}: r {r} g {g} b {b}");
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// With the default strengths a white highlight moves toward yellow-red: blue relative to red falls to
/// 1.01 / 1.06 at I = 1 (a 4.7 % shift) and to 1.04 / 1.24 at I = 4, blue loses more than green and
/// green more than red at equal drive.
#[test]
fn default_strengths_warm_the_highlights() {
    let Some(mut gpu) = common::gpu_or_skip("default_strengths_warm_the_highlights") else {
        return;
    };
    let counter = gpu.validation_counter();
    let d = Params::default();
    assert_eq!(
        d.phosphor_saturation_rgb,
        SATURATION_DEFAULT.map(|x| x as f32),
        "the default strengths are the oracle's"
    );
    let on = Params {
        phosphor_saturation: true,
        ..d
    };
    let mut shift = [0.0; 2];
    for (k, level) in [1.0_f32, 4.0].into_iter().enumerate() {
        let out = run(&mut gpu, &on, &uniform(4, 2, [level; 3]));
        let want = saturate_rgb([f64::from(level); 3], SATURATION_DEFAULT);
        let got: [f64; 3] = std::array::from_fn(|ch| texel(&out, 1, 1, ch));
        for ch in 0..3 {
            assert!(rel(got[ch], want[ch]) <= 1.05 * FP16, "level {level} ch {ch}");
        }
        assert!(got[2] < got[1] && got[1] < got[0], "blue < green < red at {level}");
        shift[k] = got[2] / got[0];
    }
    assert!(rel(shift[0], 1.01 / 1.06) <= 2.1 * FP16, "white: {}", shift[0]);
    assert!(rel(shift[1], 1.04 / 1.24) <= 2.1 * FP16, "highlight: {}", shift[1]);
    assert!(shift[1] < shift[0], "a brighter highlight is warmer");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Saturation and persistence together: saturation first (hand derivation in the module doc).
/// (1) A single excitation then black: y_0 = w0 f(X) and y_k / y_0 = E_k / E_0 for k = 1 to 3 whatever
/// the level, which the opposite order would not give. (2) A static picture settles at f(x), the
/// persistence not changing it. (3) Switching saturation restarts the history.
#[test]
fn interaction_with_persistence() {
    let Some(mut gpu) = common::gpu_or_skip("interaction_with_persistence") else {
        return;
    };
    let counter = gpu.validation_counter();
    let p = params(true, S, true);
    let t = frame_period_ms(60.0);
    let fits: [DecayFit<'static>; 3] = [KUHN_RED, KUHN_GREEN, KUHN_BLUE];
    let x = [2.0_f32, 3.0, 3.0];
    let pic = uniform(8, 4, x);
    let mut out = vec![run(&mut gpu, &p, &pic)];
    for _ in 0..3 {
        out.push(run(&mut gpu, &p, &uniform(8, 4, [0.0; 3])));
    }
    for ch in 0..3 {
        let fb: FrameFeedback = fits[ch].frame_feedback(t, TRAIL_HORIZON_MS);
        let s = f64::from(S[ch]);
        let fx = saturate(f64::from(x[ch]), s);
        let y0 = texel(&out[0], 3, 2, ch);
        assert!(
            rel(y0, fb.w0 * fx) <= 2.5 * FP16,
            "ch {ch}: y0 {y0} vs w0 f(X) = {}",
            fb.w0 * fx
        );
        let ratios = fits[ch].frame_energy_ratios(t);
        for k in 1..=3usize {
            let yk = texel(&out[k], 3, 2, ch);
            if ch == 0 {
                assert_eq!(yk, 0.0, "red has no trail");
                continue;
            }
            assert!(
                rel(yk / y0, ratios[k - 1]) <= 2.5 * FP16,
                "ch {ch} k {k}: ratio {} vs E_k / E_0 {}",
                yk / y0,
                ratios[k - 1]
            );
        }
        if ch == 2 {
            // Sanity of the test: the reversed order would give a ratio far from the measured one.
            let (w, x) = (fb.w0, f64::from(x[ch]));
            let reversed = saturate(w * ratios[0] * x, s) / saturate(w * x, s);
            assert!(reversed > 2.5 * ratios[0], "reversed order {reversed} vs {}", ratios[0]);
        }
    }
    // (2) Static picture: 400 frames, the slowest term has decayed to 3e-4 of its gap.
    let mut last = out.pop().unwrap();
    for _ in 0..400 {
        last = run(&mut gpu, &p, &pic);
    }
    for ch in 0..3 {
        let fx = saturate(f64::from(x[ch]), f64::from(S[ch]));
        let y = texel(&last, 5, 1, ch);
        assert!(rel(y, fx) <= 2.5 * FP16 + 1e-4, "ch {ch}: steady {y} vs f(x) = {fx}");
    }
    // (3) The same picture with saturation switched off restarts the history: the next black frame is
    // exactly black although the history held light (the first frame after the switch has no trail).
    let off = params(false, S, true);
    let first = run(&mut gpu, &off, &pic);
    for ch in 0..3 {
        // w0 x only: the history was dropped.
        let w0 = fits[ch].frame_feedback(t, TRAIL_HORIZON_MS).w0;
        assert!(
            rel(texel(&first, 0, 0, ch), w0 * f64::from(x[ch])) <= 2.5 * FP16,
            "ch {ch}: first frame after the switch"
        );
    }
    assert_eq!(
        run(&mut gpu, &params(true, S, true), &uniform(8, 4, [0.0; 3])),
        uniform(8, 4, [0.0; 3]),
        "switching saturation on again: no stale history"
    );
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

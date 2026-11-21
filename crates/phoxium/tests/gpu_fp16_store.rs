//! GPU test of the FP16 stage stores (T-fp16), headless, with the validation layer (synchronization
//! validation on) and zero validation messages.
//!
//! The Vulkan spec leaves the rounding of a float to FP16 conversion to the implementation: this NVIDIA
//! driver truncates (toward zero), others round to nearest. Every chain stage therefore stores
//! `round_f16(value)` (`shaders/include/common.glsl`): the value already rounded to the nearest FP16
//! value, ties to even, so the hardware conversion is exact and the stored bits are the same on any
//! device. This file checks that, bit for bit, against a CPU reference rounding (f64 arithmetic).
//!
//! Method. A float image of awkward values is injected (FP32 on the GPU, so the shader reads the exact
//! `f32`), a stage that copies or computes exactly stores it, and the FP16 image read back (converted
//! exactly to `f32`) must equal the reference for every value:
//! - `abl` with the limiter off (and the breathing on, so that the slot is not skipped as an identity)
//!   copies the texel: all four channels, any value (negative, huge, inf, NaN);
//! - `gun` with `gun_gamma` 1 returns V unchanged, `amp` at its defaults is an exact hold, and `screen`
//!   with the tube not placed is `1.0 * x` (beam fallback, mask 1): the values of [0, 1];
//! - `decode` (the real stage, source frame): every code k / 255 read through UNORM.
//!
//! The values: every k / 255, exact ties between two FP16 neighbours in every binade (and one `f32` ulp
//! either side), the subnormal ties (j + 1/2) 2^-24, the boundaries (2^-14, 65504, 65519.996, 65520 which
//! is the tie that rounds to infinity, 65536), zero, tiny values, `f32` subnormals, +-inf, NaN, and a
//! pseudo-random sweep over 2^-27 to 2^16.
//!
//! The reference. For |x| in a binade 2^e (e clamped to -14, the subnormal step) the FP16 quantum is
//! q = 2^(e - 10); the result is roundTiesToEven(|x| / q) q with the sign of x, and |x| >= 65520 (half way
//! from 65504 to 2^16, a tie that goes to the even neighbour 2^16) is infinite, as in IEEE 754 binary16.
//! The reference is itself checked against hand-derived values first.

mod common;

use phoxium::gpu::{FrameConstants, Headless, ImageF32, Settings, SourceFrame, StageInput, StageRun, TargetFormat};
use phoxium::params::{ParamValue, Params};
use phoxium::scaling::View;

/// Width of the injected images; the height follows from the number of values.
const WIDTH: u32 = 512;

/// The `f32` nearest to `x` under IEEE 754 binary16 roundTiesToEven, as an `f32` (exact: every FP16 value
/// is an `f32`). Infinite beyond 65520, NaN and inf unchanged.
fn f16_nearest(x: f32) -> f32 {
    if !x.is_finite() {
        return x;
    }
    let a = f64::from(x).abs();
    let r = if a == 0.0 {
        0.0
    } else if a >= 65520.0 {
        f64::INFINITY
    } else {
        let e = ((a.to_bits() >> 52) & 0x7ff) as i32 - 1023;
        let q = 2f64.powi(e.max(-14) - 10);
        (a / q).round_ties_even() * q
    };
    (r as f32).copysign(x)
}

/// The reference against numbers worked out by hand (2^-10 is the step in [1, 2), 2^-24 the subnormal one).
#[test]
fn reference_matches_hand_derived_values() {
    let ulp1 = 2f32.powi(-10);
    let cases: [(f32, f32); 17] = [
        (1.0, 1.0),
        (1.0 + ulp1 * 0.5, 1.0),                      // tie, 1.0 has the even mantissa 0
        (1.0 + ulp1 * 1.5, 1.0 + 2.0 * ulp1),         // tie between 1 and 2 ulp: the even one
        (1.0 + ulp1 * 0.75, 1.0 + ulp1),              // above half: up
        (1.0 + ulp1 * 0.25, 1.0),                     // below half: down
        (254.0 / 255.0, 0.996_093_75),                // 0.99607843: ulp 2^-11 below 1, 2040/2048
        (2f32.powi(-14), 2f32.powi(-14)),             // smallest normal
        (1023.5 * 2f32.powi(-24), 2f32.powi(-14)),    // tie between 1023 and 1024 steps: 1024 is even
        (0.5 * 2f32.powi(-24), 0.0),                  // tie 0 / 1 step: 0 is even
        (1.5 * 2f32.powi(-24), 2.0 * 2f32.powi(-24)), // tie 1 / 2 steps: 2
        (2.5 * 2f32.powi(-24), 2.0 * 2f32.powi(-24)), // tie 2 / 3 steps: 2
        (-1e-10, -0.0),
        (65504.0, 65504.0),
        (65519.99, 65504.0),
        (65520.0, f32::INFINITY),
        (-70000.0, f32::NEG_INFINITY),
        (3.0e-8, 2f32.powi(-24) * 1.0), // 0.503 steps: one step
    ];
    for (x, want) in cases {
        let got = f16_nearest(x);
        assert!(
            got == want && got.is_sign_negative() == want.is_sign_negative(),
            "{x:e} -> {got:e}, hand {want:e}"
        );
    }
    assert!(f16_nearest(f32::NAN).is_nan());
}

/// xorshift64*, deterministic.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
}

/// The awkward values (module documentation), all channels.
fn values() -> Vec<f32> {
    let mut v: Vec<f32> = vec![
        0.0,
        -0.0,
        1.0,
        -1.0,
        0.5,
        2f32.powi(-14),
        2f32.powi(-14) * (1.0 - 2f32.powi(-12)),
        2f32.powi(-14) * (1.0 + 2f32.powi(-11)),
        2f32.powi(-24),
        65504.0,
        65519.996,
        65520.0,
        65520.004,
        65536.0,
        1.0e30,
        -1.0e30,
        f32::MAX,
        f32::MIN_POSITIVE,
        1.0e-40, // f32 subnormal
        -1.0e-40,
        1.0e-10,
        -1.0e-10,
        1.0e-8,
        3.0e-8,
        5.0e-8,
        2.9802322e-8, // 2^-25 exactly: the tie 0 / 1 step
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NAN,
    ];
    v.extend((0..=255u32).map(|k| k as f32 / 255.0));
    // Exact ties in every binade 2^e (e = -14 .. 15), every mantissa, and one f32 ulp either side.
    for e in -14..=15i32 {
        for m in 0..1024u32 {
            let tie = (1.0 + (2 * m + 1) as f32 / 2048.0) * 2f32.powi(e);
            v.push(tie);
            v.push(f32::from_bits(tie.to_bits() + 1));
            v.push(f32::from_bits(tie.to_bits() - 1));
            if m % 16 == 0 {
                v.push(-tie);
            }
        }
    }
    // Subnormal ties (j + 1/2) 2^-24 and their f32 neighbours.
    for j in 0..1100u32 {
        let tie = (j as f32 + 0.5) * 2f32.powi(-24);
        v.extend([
            tie,
            f32::from_bits(tie.to_bits() + 1),
            f32::from_bits(tie.to_bits() - 1),
            -tie,
        ]);
    }
    // Pseudo-random sweep, exponents 2^-27 to 2^16.
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..40_000 {
        let r = rng.next();
        let exp = 100 + (r % 44) as u32; // 2^-27 .. 2^16
        let mant = ((r >> 8) & 0x7f_ffff) as u32;
        let sign = ((r >> 40) & 1) as u32;
        v.push(f32::from_bits(sign << 31 | exp << 23 | mant));
    }
    v
}

/// Packs `v` four per pixel into an image `WIDTH` wide (padded with zeros).
fn image(v: &[f32]) -> ImageF32 {
    let pixels = v.len().div_ceil(4);
    let height = pixels.div_ceil(WIDTH as usize).max(2) as u32;
    let mut rgba = v.to_vec();
    rgba.resize((WIDTH * height * 4) as usize, 0.0);
    ImageF32 {
        width: WIDTH,
        height,
        rgba,
    }
}

/// Runs only the slot `slot` over `v` injected after `after` (K = 1, tube not placed) and returns the
/// stored values, one per input value.
fn run(gpu: &mut Headless, slot: &str, after: &str, params: &Params, v: &[f32]) -> Vec<f32> {
    let img = image(v);
    let mut fc = FrameConstants::new((img.width, img.height), (img.width, img.height), false);
    fc.amp_k = 1;
    fc.px_per_mm = 0.0;
    gpu.enable_only(&[slot]).unwrap();
    let r = gpu
        .render_stages(&StageRun {
            input: StageInput::Image { after, image: &img },
            frame: fc,
            settings: &Settings::identity(),
            params,
            format: TargetFormat::Rgba32F,
        })
        .expect("render_stages");
    let out = r.stage(slot).expect("slot ran");
    assert_eq!((out.width, out.height), (img.width, img.height), "{slot}: size");
    out.rgba[..v.len()].to_vec()
}

/// Asserts that `got` is the reference rounding of `v` for the channels `check` (index mod 4).
fn assert_nearest(what: &str, v: &[f32], got: &[f32], check: impl Fn(usize) -> bool) {
    let mut bad = Vec::new();
    for (i, (&x, &g)) in v.iter().zip(got).enumerate() {
        if !check(i % 4) {
            continue;
        }
        let want = f16_nearest(x);
        let same = if want.is_nan() { g.is_nan() } else { g == want };
        if !same {
            bad.push(format!("{x:e} (bits {:#010x}) -> {g:e}, nearest {want:e}", x.to_bits()));
        }
    }
    assert!(
        bad.is_empty(),
        "{what}: {} of {} values differ from the nearest FP16 value, first: {:#?}",
        bad.len(),
        v.len(),
        &bad[..bad.len().min(8)]
    );
}

/// The values of [0, 1] (what the gun law, amp and screen accept unchanged), padded to whole pixels.
fn unit_values(v: &[f32]) -> Vec<f32> {
    let mut u: Vec<f32> = v
        .iter()
        .copied()
        .filter(|x| (0.0..=1.0).contains(x) && !x.is_sign_negative())
        .collect();
    u.resize(u.len().div_ceil(4) * 4, 0.0);
    u
}

#[test]
fn every_stage_store_is_the_nearest_fp16_value() {
    let test = "gpu_fp16_store";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    eprintln!("{test}: on {}", gpu.context().gpu.name);
    let all = values();
    let unit = unit_values(&all);
    eprintln!("{test}: {} values, {} in [0, 1]", all.len(), unit.len());

    // abl, limiter off: a copy of every channel, any value. With every feature off the slot is skipped as
    // an identity (no store at all), so the breathing is on to make the apply pass run.
    let mut breathing = Params::default();
    assert!(breathing.set("abl_breathing", ParamValue::F32(0.03)));
    let got = run(&mut gpu, "abl", "gun", &breathing, &all);
    assert_nearest("abl copy", &all, &got, |_| true);

    // gun with gamma 1: V unchanged (rgb), alpha passed through.
    let mut p = Params::default();
    assert!(p.set("gun_gamma", ParamValue::F32(1.0)));
    let got = run(&mut gpu, "gun", "amp", &p, &unit);
    assert_nearest("gun gamma 1", &unit, &got, |_| true);

    // amp at its defaults: the exact hold.
    let got = run(&mut gpu, "amp", "signal", &Params::default(), &unit);
    assert_nearest("amp hold", &unit, &got, |_| true);

    // screen, tube not placed: rgb = 1.0 * x, alpha 1.
    let got = run(&mut gpu, "screen", "abl", &Params::default(), &unit);
    assert_nearest("screen", &unit, &got, |c| c < 3);
    for a in got.iter().skip(3).step_by(4) {
        assert_eq!(*a, 1.0, "screen alpha");
    }

    // decode, the real stage: every code k / 255 read through UNORM.
    let (w, h) = (256u32, 2u32);
    let mut bgra = Vec::new();
    for _ in 0..h {
        for x in 0..w {
            bgra.extend_from_slice(&[x as u8, (255 - x) as u8, x as u8, 255]);
        }
    }
    gpu.enable_only(&[]).unwrap();
    let r = gpu
        .render(
            Some(SourceFrame {
                width: w,
                height: h,
                bgra: &bgra,
            }),
            View::full(w, h),
            &Settings::identity(),
            &Params::default(),
            (w, h),
            TargetFormat::Bgra8Unorm,
        )
        .expect("headless render");
    let mut worst = 0f64;
    let mut bias = 0f64;
    for x in 0..w {
        let p = r.native.pixel(x, 0);
        for (c, k) in [(0, x), (1, 255 - x)] {
            let exact = f64::from(k) / 255.0;
            let want = f16_nearest(exact as f32);
            assert_eq!(p[c], want, "decode: code {k} stored as {} instead of {want}", p[c]);
            if k > 0 {
                let rel = (f64::from(p[c]) - exact) / exact;
                worst = worst.max(rel.abs());
                bias += rel;
            }
        }
    }
    eprintln!(
        "{test}: decode relative error: worst {worst:e}, mean {:e}",
        bias / 510.0
    );
    assert!(
        worst <= 2f64.powi(-11),
        "decode: relative error {worst:e} above half an FP16 ulp"
    );
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

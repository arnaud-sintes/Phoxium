//! Tests of the `abl` slot: automatic beam limiting and raster breathing (T-35, QUESTIONS PHY-13).
//!
//! Headless GPU tests (validation layer on, zero messages). Beam currents are injected as an FP32 image
//! after `gun` (the slot's input), only `abl` is enabled, and the `abl` image (FP16, converted exactly to
//! `f32`) is compared with the `crt_model::abl` oracle (f64, written from `CRT.md` "ABL, blooming and EHT
//! regulation"). Patterns are the `window` test patterns of `ultimatecrt::patterns` (code 255 -> current 1,
//! 0 -> 0, the pure 2.4 power law of the gun maps them so), at K = 1. The GPU error against the oracle is
//! the FP16 storage of the result (round to nearest, relative 2^-11) plus `f32` arithmetic (`exp`, the FP32 sum);
//! `tol` covers both. The probe (`abl_probe`) writes the state into texel (0, 0): (g, filtered APL x,
//! raster_scale, last frame APL); it is one frame old in the apply pass, like the gain.
//!
//! # Hand derivations (CRT.md and the model in `crt_model::abl`)
//!
//! - Windows on 320 x 240: side scale sqrt(APL), rounded: 10 % -> 101 x 76 = 7676 px, APL 0.0999479;
//!   25 % -> 160 x 120, 0.25 exactly; 50 % -> 226 x 170 = 38420 px, 0.500260; 100 % -> 1.
//! - Gain with I_fw = 4 mA, I_th = 1.4 mA, ratio 0.5: k = (2 - 1) / 2.6 = 0.384615 per mA. 10 % and 25 %:
//!   0.4 and 1.0 mA, below the threshold, g = 1 exactly; 50 %: 4 x 0.500260 - 1.4 = 0.601042 mA over,
//!   g = 1 / (1 + 0.231170) = 0.812236; 100 %: 2.6 over, g = 1 / 2 = 0.5.
//! - Defaults (I_fw 2.8, I_th 1.4, ratio 0.5): a 10 % window is 0.28 mA (g = 1), full white gives g = 0.5,
//!   so full white / 10 % window = 0.5 (plan: within 30 to 60 %, +-2 %); ratio 0.35 gives 0.35.
//! - Step 0 -> 1 at 60 Hz (dt = 16.667 ms): x_n = 1 - exp(-n dt / tau). tau = 0.25 s = 15 frames:
//!   x_15 = 0.632121 (63.2 % after tau); the first n with x_n >= 0.632 is 15. Default tau 0.1667 s: the
//!   anode current 2.8 x_n crosses 1.4 mA when x_n > 0.5, n dt > tau ln 2 = 0.11555 s, n = 7 (0.1167 s,
//!   the 0.1 s onset of CRT.md); x_n >= 0.95 first at n = 31 (3 tau = 0.5001 s = 30.006 frames; the
//!   0.5 s settling of CRT.md, +-1 frame). The gain of frame n + 1 is g(x_n).
//! - Breathing b = 0.03, ref 0.25, 50 % window: s = 1 + 0.03 (0.500260 - 0.25) = 1.0075078; b = 0 gives
//!   exactly 1.

mod common;

use crt_model::abl::{window_apl, Abl};
use ultimatecrt::gpu::{FrameConstants, Headless, ImageF32, Rendered, Settings, StageInput, StageRun, TargetFormat};
use ultimatecrt::params::{ParamValue, Params};
use ultimatecrt::patterns::{generate, window_size, Pattern};

const W: u32 = 320;
const H: u32 = 240;
/// 60 Hz frame time, ms.
const DT_MS: f32 = 1000.0 / 60.0;

fn set(p: &mut Params, id: &str, v: ParamValue) {
    assert!(p.set(id, v), "{id}");
}

fn f32p(p: &mut Params, id: &str, v: f32) {
    set(p, id, ParamValue::F32(v));
}

fn on(p: &mut Params, id: &str, v: bool) {
    set(p, id, ParamValue::Bool(v));
}

/// The oracle with the constants of `p` (read back as `f32` values, as the GPU sees them).
fn oracle(p: &Params) -> Abl {
    let g = |id: &str| match p.get(id) {
        Some(ParamValue::F32(v)) => f64::from(v),
        other => panic!("{id}: {other:?}"),
    };
    Abl {
        full_white_ma: g("abl_full_white_ma"),
        threshold_ma: g("abl_threshold_ma"),
        full_white_ratio: g("abl_full_white_ratio"),
        tau_s: g("abl_tau_s"),
        breathing: g("abl_breathing"),
        breathing_ref: g("abl_breathing_ref"),
    }
}

/// Tolerance of an FP16 result of `want`, with room for the `f32` arithmetic.
fn tol(want: f64) -> f64 {
    want.abs() * (2f64.powi(-11) + 1e-5) + 1e-6
}

/// The window pattern of `apl` percent as beam currents (code / 255), and its exact APL.
fn window(apl: u32) -> (ImageF32, f64) {
    let bgra = generate(&Pattern::Window(apl), W, H, 0);
    let rgba = bgra
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|px| {
            let v = f32::from(px[0]) / 255.0;
            [v, v, v, 1.0]
        })
        .collect();
    let (ww, wh) = window_size(W, H, apl);
    (
        ImageF32 {
            width: W,
            height: H,
            rgba,
        },
        window_apl(W, H, ww, wh),
    )
}

/// One frame of the `abl` slot alone over `image` (injected after `gun`), with frame time `dt_ms`.
fn frame(gpu: &mut Headless, p: &Params, image: &ImageF32, dt_ms: f32) -> Rendered {
    let mut fc = FrameConstants::new((image.width, image.height), (image.width, image.height), false);
    fc.amp_k = 1;
    fc.frame_time_ms = dt_ms;
    gpu.enable_only(&["abl"]).unwrap();
    gpu.render_stages(&StageRun {
        input: StageInput::Image { after: "gun", image },
        frame: fc,
        settings: &Settings::identity(),
        params: p,
        format: TargetFormat::Rgba32F,
    })
    .expect("render_stages")
}

fn abl(r: &Rendered) -> &ImageF32 {
    r.stage("abl").expect("abl slot")
}

/// Value of channel 0 at the centre (lit in every window).
fn centre(r: &Rendered) -> f64 {
    f64::from(abl(r).pixel(W / 2, H / 2)[0])
}

/// The probe texel: (g, x, raster_scale, frame APL).
fn probe(r: &Rendered) -> [f64; 4] {
    abl(r).pixel(0, 0).map(f64::from)
}

fn near(got: f64, want: f64, what: &str) {
    assert!((got - want).abs() <= tol(want), "{what}: {got} vs oracle {want}");
}

#[test]
fn disabled_is_exact_identity() {
    let test = "stage_abl identity";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    // FP16-exact currents (k / 1024) in a varied pattern, bright enough that an active limiter would show.
    let rgba: Vec<f32> = (0..W * H)
        .flat_map(|i| {
            let v = |s: u32| ((i * s + 7) % 1025) as f32 / 1024.0;
            [v(1), v(7), v(13), 1.0]
        })
        .collect();
    let img = ImageF32 {
        width: W,
        height: H,
        rgba,
    };
    // Default parameters (ABL off), then breathing on with the limiter off: the picture is unchanged.
    let mut breathing = Params::default();
    f32p(&mut breathing, "abl_breathing", 0.03);
    for (what, p) in [("defaults", Params::default()), ("breathing only", breathing)] {
        for _ in 0..3 {
            let r = frame(&mut gpu, &p, &img, DT_MS);
            assert_eq!(abl(&r).rgba, img.rgba, "{test} {what}: bit-exact identity");
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

#[test]
fn gain_follows_apl() {
    let test = "stage_abl gain vs APL";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let mut p = Params::default();
    on(&mut p, "abl_enable", true);
    f32p(&mut p, "abl_full_white_ma", 4.0);
    f32p(&mut p, "abl_tau_s", 0.0);
    let o = oracle(&p);
    let mut probed = p;
    on(&mut probed, "abl_probe", true);
    let expect = [(10, 1.0), (25, 1.0), (50, 0.812_236), (100, 0.5)];
    for (apl, hand) in expect {
        let (img, a) = window(apl);
        // tau 0: the state after one frame is the frame APL; the next frame applies its gain.
        frame(&mut gpu, &p, &img, DT_MS);
        let r = frame(&mut gpu, &p, &img, DT_MS);
        let want = o.gain(a);
        assert!(
            (want - hand).abs() <= 1e-6,
            "{test} {apl} %: oracle {want} vs hand {hand}"
        );
        eprintln!(
            "{test}: {apl} % window APL {a:.6} gain GPU {} oracle {want:.6}",
            centre(&r)
        );
        near(centre(&r), want, &format!("{test} {apl} %"));
        if hand == 1.0 {
            // Below the threshold g == 1 exactly, so the lit texels are exactly 1.
            assert_eq!(centre(&r), 1.0, "{test} {apl} %: g == 1 exactly");
            assert_eq!(abl(&r).rgba, img.rgba, "{test} {apl} %: identity below the threshold");
        }
        // Reduction: the frame APL equals the analytic APL within 0.1 % (the readout is FP16).
        let s = probe(&frame(&mut gpu, &probed, &img, DT_MS));
        eprintln!("{test}: {apl} % reduction {} analytic {a:.6}", s[3]);
        assert!(
            (s[3] - a).abs() <= a * 1e-3,
            "{test} {apl} %: reduction {} vs {a}",
            s[3]
        );
        assert!((s[1] - a).abs() <= a * 1e-3, "{test} {apl} %: filtered {} vs {a}", s[1]);
        near(s[0], want, &format!("{test} {apl} %: probe gain"));
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

#[test]
fn full_white_over_small_window_is_the_configured_ratio() {
    let test = "stage_abl full white ratio";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    for ratio in [0.5_f32, 0.35, 0.6] {
        let mut p = Params::default();
        on(&mut p, "abl_enable", true);
        f32p(&mut p, "abl_full_white_ratio", ratio);
        f32p(&mut p, "abl_tau_s", 0.0);
        let (small, _) = window(10);
        let (full, _) = window(100);
        frame(&mut gpu, &p, &small, DT_MS);
        let l_small = centre(&frame(&mut gpu, &p, &small, DT_MS));
        frame(&mut gpu, &p, &full, DT_MS);
        let l_full = centre(&frame(&mut gpu, &p, &full, DT_MS));
        let got = l_full / l_small;
        eprintln!("{test}: configured {ratio} measured {got:.6}");
        let want = f64::from(ratio);
        assert!(
            (got - want).abs() <= want * 0.02,
            "{test}: full white / 10 % window = {got}, configured {want}"
        );
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Runs `black` black frames then `white` full-white frames; returns the (centre gain, probe) of each white
/// frame.
fn step(gpu: &mut Headless, p: &Params, black: usize, white: usize) -> Vec<(f64, [f64; 4])> {
    let (dark, _) = window(1);
    let dark = ImageF32 {
        rgba: vec![0.0; dark.rgba.len()],
        ..dark
    };
    let (full, _) = window(100);
    for _ in 0..black {
        frame(gpu, p, &dark, DT_MS);
    }
    (0..white)
        .map(|_| {
            let r = frame(gpu, p, &full, DT_MS);
            (centre(&r), probe(&r))
        })
        .collect()
}

#[test]
fn step_response_follows_the_time_constant() {
    let test = "stage_abl step";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let dt = f64::from(DT_MS) / 1000.0;
    // tau = 0.25 s = 15 frames: 63.2 % after tau, +-1 frame.
    let mut p = Params::default();
    on(&mut p, "abl_enable", true);
    on(&mut p, "abl_probe", true);
    f32p(&mut p, "abl_tau_s", 0.25);
    let o = oracle(&p);
    let frames = step(&mut gpu, &p, 3, 40);
    let x_oracle = o.response(&[1.0; 40], dt);
    // Frame i (0-based) shows the state after frames 0..i-1 (one frame of latency).
    for (i, (g, s)) in frames.iter().enumerate() {
        let x = if i == 0 { 0.0 } else { x_oracle[i - 1] };
        assert!(
            (s[1] - x).abs() <= tol(x) + 2e-6,
            "{test} frame {i}: x {} vs oracle {x}",
            s[1]
        );
        near(*g, o.gain(x), &format!("{test} frame {i}: gain"));
    }
    let first = |th: f64| frames.iter().skip(1).position(|(_, s)| s[1] >= th).map(|n| n + 1);
    let n63 = first(1.0 - (-1.0f64).exp() - 2e-3).expect("reaches 63.2 %");
    assert!(
        n63.abs_diff(15) <= 1,
        "{test}: 63.2 % after {n63} frames, tau = 15 frames"
    );

    // Defaults (tau 0.1667 s, I_fw 2.8 mA, I_th 1.4 mA): onset about 0.1 s, settled about 0.5 s.
    let Some(mut gpu2) = common::gpu_or_skip(test) else {
        return;
    };
    let counter2 = gpu2.validation_counter();
    let mut d = Params::default();
    on(&mut d, "abl_enable", true);
    on(&mut d, "abl_probe", true);
    let frames = step(&mut gpu2, &d, 2, 45);
    // The limiter acts from the first frame whose gain is below 1: frame n + 1 for the state of frame n.
    let onset = frames.iter().position(|(g, _)| *g < 1.0).expect("limiter acts");
    let onset_s = onset as f64 * dt;
    let od = oracle(&d);
    let want_onset = -od.tau_s * (1.0 - od.threshold_ma / od.full_white_ma).ln();
    assert!(
        (onset_s - want_onset).abs() <= dt,
        "{test}: onset after {onset_s} s, oracle {want_onset} s (CRT.md about 0.1 s)"
    );
    let settled = first_at(&frames, 0.95);
    eprintln!("{test}: 63.2 % after {n63} frames (tau 15); onset {onset} frames = {:.4} s (oracle {want_onset:.4} s); 95 % after {settled} frames = {:.4} s", onset as f64 * dt, settled as f64 * dt);
    let settled_s = settled as f64 * dt;
    assert!(
        (settled_s - 0.5).abs() <= dt * 1.001,
        "{test}: 95 % after {settled_s} s (CRT.md about 0.5 s)"
    );
    drop(gpu);
    drop(gpu2);
    common::assert_no_validation_messages(&counter);
    common::assert_no_validation_messages(&counter2);
}

/// Frames of white after which the filtered APL first reaches `th` (the probe of frame n + 1).
fn first_at(frames: &[(f64, [f64; 4])], th: f64) -> usize {
    frames
        .iter()
        .skip(1)
        .position(|(_, s)| s[1] >= th)
        .expect("reaches the threshold")
        + 1
}

#[test]
fn raster_breathing_scale() {
    let test = "stage_abl breathing";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let (img, a) = window(50);
    // b = 0 (default): raster_scale exactly 1, with the limiter on or off.
    for enable in [false, true] {
        let mut p = Params::default();
        on(&mut p, "abl_enable", enable);
        on(&mut p, "abl_probe", true);
        f32p(&mut p, "abl_tau_s", 0.0);
        frame(&mut gpu, &p, &img, DT_MS);
        let s = probe(&frame(&mut gpu, &p, &img, DT_MS));
        assert_eq!(s[2], 1.0, "{test}: b = 0 -> raster_scale exactly 1 (limiter {enable})");
    }
    // b = 0.03, ref 0.25, limiter off: s = 1 + b (x - ref), +-0.1 %.
    let mut p = Params::default();
    on(&mut p, "abl_probe", true);
    f32p(&mut p, "abl_tau_s", 0.0);
    f32p(&mut p, "abl_breathing", 0.03);
    f32p(&mut p, "abl_breathing_ref", 0.25);
    frame(&mut gpu, &p, &img, DT_MS);
    let s = probe(&frame(&mut gpu, &p, &img, DT_MS));
    let want = oracle(&p).raster_scale(a);
    assert!((want - 1.007_507_8).abs() <= 1e-6, "{test}: oracle {want}");
    assert!(
        (s[2] - want).abs() <= want * 1e-3,
        "{test}: raster_scale {} vs {want}",
        s[2]
    );
    eprintln!("{test}: raster_scale GPU {} oracle {want:.7}", s[2]);
    assert_eq!(s[0], 1.0, "{test}: no gain with the limiter off");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

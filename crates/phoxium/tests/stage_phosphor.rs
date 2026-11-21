//! Phosphor persistence stage (T-41): `shaders/phosphor.comp`, `src/gpu/stages/phosphor.rs`,
//! `params/phosphor.toml`, against the closed-form oracle of `crt_model::phosphor` and the CRT.md table.
//!
//! # Model and derivations (CRT.md "Decay and persistence", rule 8; QUESTIONS PHY-12)
//!
//! Impulse response per channel, normalised to I(0) = 1 (Kuhn 2002, one P22 monitor):
//! I(t) = sum_i A_i exp(-t / tau_i) + B (1 + t / t0)^-n. Red: six exponentials, slowest tau = 442 us, no
//! tail. Green: B = 0.3609, t0 = 5.5 us, n = 1.1. Blue: B = 0.0627, t0 = 5 us, n = 1.11.
//!
//! Frame energies for one excitation at t = 0 (frame k covers [kT, (k + 1)T]):
//! - exponential term: E_k = A tau (exp(-kT/tau) - exp(-(k+1)T/tau)) = A tau (1 - rho) rho^k with
//!   rho = exp(-T / tau): a geometric sequence, so one recurrence state per term is exact;
//! - power-law term: E_k = B t0 / (1 - n) ((1 + (k+1)T/t0)^(1-n) - (1 + kT/t0)^(1-n)).
//!
//! At 60 Hz (T = 16.667 ms) the slowest red term has rho = exp(-16.667 / 0.442) = 4.2e-17, so red's
//! E_1 / E_0 = 3.7e-17 (CRT.md: below 1e-6) and the red trail is 0 in FP32; green and blue exponential terms
//! have tau <= 1.6 us, rho = exp(-10^4) = 0, so their trail is the power law alone. Closed-form ratios
//! (oracle `DecayFit::frame_energy_ratios`, cross-checked against a numerical integral in crt-model):
//! green E_1/E_0, E_2/E_0, E_3/E_0 = 5.3571 %, 2.9662 %, 2.0329 %; blue 4.7216 %, 2.5999 %, 1.7756 %
//! (CRT.md table 5.36 / 2.97 / 2.03 and 4.72 / 2.60 / 1.78 %). 50 Hz: green / blue E_1/E_0 = 5.19 / 4.57 %;
//! 120 Hz: 6.09 / 5.37 %. Within 1 s, 1 - E[0, T] / E[0, 1 s] = 21.2 % (green) and 18.9 % (blue).
//!
//! # Recurrence (derived by hand, implemented in the shader)
//!
//! Let x[m] be the light of frame m without persistence and y[m] the output. A single excitation of
//! frame m - k reaches frame m with the weight E_k / E_0 relative to its own frame, so
//! y[m] = w0 (x[m] + sum_{k >= 1} t_k x[m - k]) with t_k the trail ratios. If t_k = sum_j c_j rho_j^(k-1),
//! define S_j[m] = sum_{k >= 1} c_j rho_j^(k-1) x[m - k]; then
//! S_j[m + 1] = c_j x[m] + sum_{k >= 2} c_j rho_j^(k-1) x[m + 1 - k] = c_j x[m] + rho_j S_j[m], so
//! y[m] = w0 (x[m] + sum_j S_j[m]) and S_j <- rho_j S_j + c_j x[m] after the frame (S_j = 0 at a reset).
//! Step response: x = (X, 0, 0, ...) gives y_0 = w0 X and y_k = w0 X sum_j c_j rho_j^(k-1), so
//! y_k / y_0 = t_k. Static picture: S_j -> c_j X / (1 - rho_j), y -> w0 (1 + sum_j c_j / (1 - rho_j)) X,
//! which is X exactly for w0 = 1 / (1 + sum_j c_j / (1 - rho_j)).
//!
//! The power law is not geometric: its trail t_k (k = 1 .. 59 at 60 Hz, the 1 s window) is fitted by four
//! geometric terms, exact at k = 1, 2, 3 (Vandermonde solve) and within about 1.2 % relative up to 1 s at 60 Hz (2.5 % at 120 Hz)
//! (`crt_model::phosphor::DecayFit::frame_feedback`, rates by search). A sum of exponentials cannot stop at
//! 1 s: the fit rolls off smoothly after the window, so its total trail is 10.9 to 16.8 % above the 1 s
//! truncated sum (the infinite power-law total is about 3 times larger). The normalisation uses the
//! realised total (steady state exactly 1), so w0 is about 3 % below the truncated-integral value
//! E[0, T] / E[0, 1 s] (green 60 Hz: 0.7628 against 0.7881, blue 0.7883 against 0.8109; recorded by `step_response`).
//!
//! Fit at 60 Hz, tail strength 1 (crt-model test `frame_feedback_kuhn_tails` prints it): green rho =
//! 0.1866, 0.6088, 0.8810, 0.9797 and c = 0.016258, 0.023984, 0.010455, 0.002874 (c sums to E_1/E_0 =
//! 0.053571); blue rho = 0.1802, 0.6022, 0.8797, 0.9795 and c = 0.014056, 0.021451, 0.009226, 0.002482.
//!
//! # Tolerances
//!
//! The output image is FP16, stored through `round_f16` (round to nearest): one value is within 2^-11
//! relative (half an FP16 step), a ratio of two within 2^-10, plus 1e-5 for the FP32 arithmetic. The
//! history is FP32.
//!
//! # Cleared history
//!
//! A cleared history is primed to the steady state of the first frame's input x: S_j = c_j x / (1 - rho_j),
//! so y = w0 (x + sum_j c_j x / (1 - rho_j)) = x at once, and S_j <- rho_j S_j + c_j x = S_j: a static
//! picture shows no fade-in. A black first frame primes it to 0 (the impulse tests start with one). If the
//! picture changes after the clear, the trail is that of the first frame as if shown forever before (the
//! real past is unknown; acceptable).

mod common;

use crt_model::phosphor::{
    blend_trail, blended_trail_ratio, DecayControls, DecayFit, FEEDBACK_TERMS, KUHN_BLUE, KUHN_GREEN, KUHN_RED,
    TRAIL_HORIZON_MS,
};
use crt_model::units::frame_period_ms;
use phoxium::gpu::{FrameConstants, Headless, ImageF32, Settings, StageInput, StageRun, TargetFormat};
use phoxium::params::Params;

const W: u32 = 8;
const H: u32 = 4;
/// One FP16 step relative (truncating stores).
const FP16: f64 = 1.0 / 1024.0;

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

/// A test picture with FP16-exact values: white, a colour and a highlight above 1.
fn picture(w: u32, h: u32) -> ImageF32 {
    image(w, h, |x, y| match (x + y) % 3 {
        0 => [1.0, 1.0, 1.0],
        1 => [0.5, 0.75, 0.25],
        _ => [2.0, 3.0, 1.5],
    })
}

fn black(w: u32, h: u32) -> ImageF32 {
    image(w, h, |_, _| [0.0; 3])
}

/// Persistence `on` at `rate_hz` with tail strengths `tail` and the full trail (`phosphor_persistence_blend` 1, so
/// the output is the recurrence itself; the blend has its own test, [`blend_follows_the_oracle`]).
fn params(on: bool, rate_hz: f32, tail: [f32; 3]) -> Params {
    Params {
        phosphor_persistence: on,
        phosphor_frame_rate_hz: rate_hz,
        phosphor_tail_strength_rgb: tail,
        phosphor_persistence_blend: 1.0,
        ..Params::default()
    }
}

/// One frame of `phosphor` alone on `input` (injected after `screen`); its output image.
fn run(gpu: &mut Headless, params: &Params, input: &ImageF32) -> ImageF32 {
    run_at(gpu, params, input, 0.0)
}

/// As [`run`], the frame's measured period being `frame_ms` (0: none, as in [`run`]).
fn run_at(gpu: &mut Headless, params: &Params, input: &ImageF32, frame_ms: f32) -> ImageF32 {
    gpu.enable_only(&["phosphor"]).unwrap();
    let settings = Settings::identity();
    let mut fc = frame(input.width, input.height);
    fc.frame_time_ms = frame_ms;
    let r = gpu
        .render_stages(&StageRun {
            input: StageInput::Image {
                after: "screen",
                image: input,
            },
            frame: fc,
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

fn fits() -> [(&'static str, DecayFit<'static>); 3] {
    [("red", KUHN_RED), ("green", KUHN_GREEN), ("blue", KUHN_BLUE)]
}

/// Persistence off (the default): the stage is an exact copy, before and after a persistence run, and
/// a run with persistence on leaves no history behind once it is switched off and on again.
#[test]
fn disabled_is_bit_exact_identity() {
    let Some(mut gpu) = common::gpu_or_skip("disabled_is_bit_exact_identity") else {
        return;
    };
    let counter = gpu.validation_counter();
    assert!(!Params::default().phosphor_persistence, "persistence is off by default");
    let off = Params::default();
    let pic = picture(W, H);
    assert_eq!(run(&mut gpu, &off, &pic), pic, "off: exact copy");
    let on = params(true, 60.0, [1.0; 3]);
    let _ = run(&mut gpu, &on, &pic);
    assert_eq!(run(&mut gpu, &off, &pic), pic, "off after on: exact copy");
    // Back on with a black frame: the history was dropped while off.
    assert_eq!(run(&mut gpu, &on, &black(W, H)), black(W, H), "no stale history");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// An isolated frame followed by black frames: y_k / y_0 is the oracle E_k / E_0 at 60 Hz (exact at
/// k = 1, 2, 3 within FP16, the fit beyond), red has no trail, the green residue of a white texel falls
/// below 1/1024 at the oracle frame (+/- 1) and the trail ends exactly at 0.
#[test]
fn step_response() {
    let Some(mut gpu) = common::gpu_or_skip("step_response") else {
        return;
    };
    let counter = gpu.validation_counter();
    let p = params(true, 60.0, [1.0; 3]);
    let t = frame_period_ms(60.0);
    let pic = picture(W, H);
    let zero = black(W, H);
    // A black first frame primes the cleared history to 0 (a cleared history is primed to the steady
    // state of its first frame), so the picture is a single excitation.
    assert_eq!(run(&mut gpu, &p, &zero), zero);
    let mut out = vec![run(&mut gpu, &p, &pic)];
    for _ in 1..60 {
        out.push(run(&mut gpu, &p, &zero));
    }
    let table = [[0.0; 3], [5.36, 2.97, 2.03], [4.72, 2.60, 1.78]];
    for (ch, (name, fit)) in fits().into_iter().enumerate() {
        let oracle = fit.frame_energy_ratios(t);
        let trail = fit.trail_ratios(t, TRAIL_HORIZON_MS);
        let w0_trunc = 1.0 - fit.fraction_after_first_frame(t, TRAIL_HORIZON_MS);
        let mut worst = 0.0_f64;
        for i in 0..(W * H) as usize {
            let x = f64::from(pic.rgba[4 * i + ch]);
            let y0 = f64::from(out[0].rgba[4 * i + ch]);
            // Fraction of the light in the first frame: the realised w0 (see the module doc).
            if ch > 0 {
                assert!(
                    rel(y0 / x, w0_trunc) < 0.04,
                    "{name}: w0 {} vs 1 s truncated {w0_trunc}",
                    y0 / x
                );
            } else {
                assert!(rel(y0, x) <= FP16, "red keeps its frame: {y0} vs {x}");
            }
            for k in 1..60usize {
                let yk = f64::from(out[k].rgba[4 * i + ch]);
                if ch == 0 {
                    assert_eq!(yk, 0.0, "red trail at frame {k}");
                    continue;
                }
                let ratio = yk / y0;
                if k <= 3 {
                    assert!(
                        rel(ratio, oracle[k - 1]) < FP16 + 1e-5,
                        "{name} k {k}: {ratio} vs oracle {}",
                        oracle[k - 1]
                    );
                    assert!(
                        (ratio * 100.0 - table[ch][k - 1]).abs() < 0.005 + 0.015,
                        "{name} k {k} table"
                    );
                } else {
                    worst = worst.max(rel(ratio, trail[k - 1]));
                }
            }
        }
        println!(
            "{name}: y0/x = {:.5} (1 s truncated {w0_trunc:.5}), y1..3/y0 = {:.5} {:.5} {:.5}, worst k >= 4 error {worst:.4}",
            out[0].rgba[ch] / pic.rgba[ch],
            out[1].rgba[ch] / out[0].rgba[ch],
            out[2].rgba[ch] / out[0].rgba[ch],
            out[3].rgba[ch] / out[0].rgba[ch],
        );
        if ch > 0 {
            assert!(
                worst < 0.02 + 2.0 * FP16,
                "{name}: trail beyond k = 3 off the oracle by {worst}"
            );
        }
    }
    // Residue of the white texel 0 (green) below 1/1024: measured frame against the oracle frame.
    let y0 = f64::from(out[0].rgba[1]);
    let trail = KUHN_GREEN.trail_ratios(t, TRAIL_HORIZON_MS);
    let oracle_k = (1..60)
        .find(|&k| y0 * trail[k - 1] < 1.0 / 1024.0)
        .expect("oracle crossing");
    let measured_k = (1..60)
        .find(|&k| f64::from(out[k].rgba[1]) < 1.0 / 1024.0)
        .expect("crossing");
    println!("green residue below 1/1024: frame {measured_k}, oracle {oracle_k}");
    assert!(measured_k.abs_diff(oracle_k) <= 1);
    // Flush: every term ends exactly at 0 (below 2^-24 it is flushed), and stays there.
    let mut last = out.pop().unwrap();
    let mut frames = 59;
    while last != zero {
        assert!(frames < 2000, "trail not flushed after {frames} frames");
        last = run(&mut gpu, &p, &zero);
        frames += 1;
    }
    println!("trail exactly 0 after {frames} frames");
    for _ in 0..3 {
        assert_eq!(run(&mut gpu, &p, &zero), zero);
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// 50 and 120 Hz: changing the frame rate recomputes the weights (same stage, new rate), and E_1 / E_0
/// follows the oracle and the CRT.md rows (green / blue 5.19 / 4.57 % at 50 Hz, 6.09 / 5.37 % at 120 Hz).
#[test]
fn frame_rate_rows() {
    let Some(mut gpu) = common::gpu_or_skip("frame_rate_rows") else {
        return;
    };
    let counter = gpu.validation_counter();
    let pic = image(W, H, |_, _| [1.0, 1.0, 1.0]);
    for (hz, g1, b1) in [
        (60.0, 5.36, 4.72),
        (50.0, 5.19, 4.57),
        (120.0, 6.09, 5.37),
        (60.0, 5.36, 4.72),
    ] {
        let p = params(true, hz, [1.0; 3]);
        gpu.clear_history();
        let _ = run(&mut gpu, &p, &black(W, H));
        let y0 = run(&mut gpu, &p, &pic);
        let y1 = run(&mut gpu, &p, &black(W, H));
        let t = frame_period_ms(f64::from(hz));
        for (ch, fit, table) in [(1, KUHN_GREEN, g1), (2, KUHN_BLUE, b1)] {
            let ratio = f64::from(y1.rgba[ch]) / f64::from(y0.rgba[ch]);
            let oracle = fit.frame_energy_ratios(t)[0];
            println!(
                "{hz} Hz channel {ch}: E1/E0 {:.4} % (oracle {:.4} %)",
                ratio * 100.0,
                oracle * 100.0
            );
            assert!(
                rel(ratio, oracle) < FP16 + 1e-5,
                "{hz} Hz channel {ch}: {ratio} vs {oracle}"
            );
            assert!(
                (ratio * 100.0 - table).abs() < 0.005 + 0.015,
                "{hz} Hz channel {ch} table {table}"
            );
        }
        assert_eq!(y1.rgba[0], 0.0, "{hz} Hz: no red trail");
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// A static picture after a clear equals the no-persistence picture from frame 0 on (the cleared history
/// is primed to the steady state; within half an FP16 step plus FP32 arithmetic), every frame for 200
/// frames, after an explicit clear too. Moving content after a clear: the second frame b shows
/// w0 b + (1 - w0) a (the primed history of the first frame a, then the trail), per channel.
#[test]
fn static_picture_keeps_its_brightness() {
    let Some(mut gpu) = common::gpu_or_skip("static_picture_keeps_its_brightness") else {
        return;
    };
    let counter = gpu.validation_counter();
    let p = params(true, 60.0, [1.0; 3]);
    let t = frame_period_ms(60.0);
    let pic = picture(W, H);
    let check = |y: &ImageF32, what: &str| {
        for (i, (&got, &x)) in y.rgba.iter().zip(&pic.rgba).enumerate() {
            assert!(
                rel(f64::from(got), f64::from(x)) <= FP16 / 2.0 + 1e-5,
                "{what}: texel {} channel {}: {got} vs {x}",
                i / 4,
                i % 4
            );
        }
    };
    for n in 0..200 {
        check(&run(&mut gpu, &p, &pic), &format!("frame {n}"));
    }
    gpu.clear_history();
    for n in 0..3 {
        check(&run(&mut gpu, &p, &pic), &format!("frame {n} after clear_history"));
    }
    // Moving content after a clear.
    gpu.clear_history();
    let a = image(W, H, |_, _| [1.0, 1.0, 1.0]);
    let b = image(W, H, |_, _| [0.25, 0.5, 2.0]);
    let _ = run(&mut gpu, &p, &a);
    let y = run(&mut gpu, &p, &b);
    for (ch, (name, fit)) in fits().into_iter().enumerate() {
        let w0 = fit.frame_feedback(t, TRAIL_HORIZON_MS).w0;
        let (xa, xb) = (f64::from(a.rgba[ch]), f64::from(b.rgba[ch]));
        let want = w0 * xb + (1.0 - w0) * xa;
        let got = f64::from(y.rgba[ch]);
        println!("{name}: frame after a change following a clear {got:.5}, predicted {want:.5}");
        assert!(rel(got, want) <= FP16 / 2.0 + 1e-5, "{name}: {got} vs {want}");
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// The history is cleared on a tube size change, on request (`clear_history`, the window loop's SRC-8
/// hook) and when the stage did not run on the previous frame; without a clear the trail shows.
#[test]
fn history_is_cleared() {
    let Some(mut gpu) = common::gpu_or_skip("history_is_cleared") else {
        return;
    };
    let counter = gpu.validation_counter();
    let p = params(true, 60.0, [1.0; 3]);
    let (a, b) = ((W, H), (W + 4, H + 2));
    // Control: a trail without a clear.
    let _ = run(&mut gpu, &p, &picture(a.0, a.1));
    let trail = run(&mut gpu, &p, &black(a.0, a.1));
    assert!(
        trail.rgba[1] > 0.0 && trail.rgba[2] > 0.0,
        "control: green and blue trail"
    );
    // Size change: the new history starts cleared.
    let _ = run(&mut gpu, &p, &picture(a.0, a.1));
    assert_eq!(run(&mut gpu, &p, &black(b.0, b.1)), black(b.0, b.1), "size change");
    // Explicit clear.
    let _ = run(&mut gpu, &p, &picture(b.0, b.1));
    gpu.clear_history();
    assert_eq!(run(&mut gpu, &p, &black(b.0, b.1)), black(b.0, b.1), "clear_history");
    // A frame where the slot did not run.
    let _ = run(&mut gpu, &p, &picture(b.0, b.1));
    gpu.enable_only(&[]).unwrap();
    let settings = Settings::identity();
    let pic = picture(b.0, b.1);
    gpu.render_stages(&StageRun {
        input: StageInput::Image {
            after: "screen",
            image: &pic,
        },
        frame: frame(b.0, b.1),
        settings: &settings,
        params: &p,
        format: TargetFormat::Rgba32F,
    })
    .unwrap();
    assert_eq!(
        run(&mut gpu, &p, &black(b.0, b.1)),
        black(b.0, b.1),
        "slot skipped a frame"
    );
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// The green output of white texel 0 after a static white picture shown for the frame periods `lit_ms` is
/// switched to black for the periods `black_ms`, all frames with their measured period: (time since the
/// last lit frame began, output) per black frame. Each lit frame keeps the picture (within FP16).
fn trail_in_time(gpu: &mut Headless, p: &Params, lit_ms: &[f32], black_ms: &[f32]) -> Vec<(f64, f64)> {
    let white = image(W, H, |_, _| [1.0; 3]);
    let zero = black(W, H);
    gpu.clear_history();
    for &dt in lit_ms {
        let y = run_at(gpu, p, &white, dt);
        assert!(
            rel(f64::from(y.rgba[1]), 1.0) <= FP16 / 2.0 + 1e-5,
            "static picture {} ({dt} ms)",
            y.rgba[1]
        );
    }
    let mut t = 0.0;
    let mut prev = *lit_ms.last().unwrap();
    let mut out = Vec::new();
    for &dt in black_ms {
        t += f64::from(prev);
        out.push((t, f64::from(run_at(gpu, p, &zero, dt).rgba[1])));
        prev = dt;
    }
    out
}

fn sample_at(curve: &[(f64, f64)], ms: f64) -> f64 {
    curve.iter().find(|(t, _)| (t - ms).abs() < 1e-3).expect("sample").1
}

/// Review R-11, on the GPU: the default time base is real time (`phosphor_frame_rate_hz` 0). After a static
/// white picture is switched off, the green trail at the same time since the last lit frame began (83.3 and
/// 250 ms) agrees within 1 % of the steady state (plus FP16) at 60 Hz, at 144 Hz and at 144 Hz with jittered
/// frame periods (0.8 / 1.2 of the period), each frame stepping by its measured period; the manual 60 Hz
/// mode at 144 Hz frames (the previous default, decay per frame) gives the 2.4 times shorter trail. The
/// oracle of the same comparison is `crt_model::phosphor` `trail_is_real_time_at_60_and_144_hz`.
#[test]
fn real_time_trail_at_60_and_144_hz() {
    let Some(mut gpu) = common::gpu_or_skip("real_time_trail_at_60_and_144_hz") else {
        return;
    };
    let counter = gpu.validation_counter();
    assert_eq!(Params::default().phosphor_frame_rate_hz, 0.0, "real time by default");
    let auto = params(true, 0.0, [1.0; 3]);
    let (p60, p144) = (1000.0_f32 / 60.0, 1000.0_f32 / 144.0);
    // Enough lit frames for the automatic rate to settle on the new rate (a refit restarts the history
    // primed to the static picture, so the lit frames stay at 1).
    let c60 = trail_in_time(&mut gpu, &auto, &[p60; 80], &[p60; 16]);
    let c144 = trail_in_time(&mut gpu, &auto, &[p144; 80], &[p144; 36]);
    let jit: Vec<f32> = (0..80)
        .map(|k| if k % 2 == 0 { 0.8 * p144 } else { 1.2 * p144 })
        .collect();
    let cj = trail_in_time(&mut gpu, &auto, &jit, &jit[..36]);
    let old = trail_in_time(&mut gpu, &params(true, 60.0, [1.0; 3]), &[p144; 4], &[p144; 36]);
    for ms in [1000.0 / 12.0, 250.0] {
        let (a, b, j, o) = (
            sample_at(&c60, ms),
            sample_at(&c144, ms),
            sample_at(&cj, ms),
            sample_at(&old, ms),
        );
        println!(
            "green trail at {ms:.1} ms: 60 Hz {a:.5}, 144 Hz {b:.5}, jittered {j:.5}, manual 60 Hz at 144 Hz {o:.5}"
        );
        assert!((a - b).abs() < 0.01 + FP16 * a, "{ms} ms: 60 Hz {a} vs 144 Hz {b}");
        assert!((a - j).abs() < 0.01 + FP16 * a, "{ms} ms: 60 Hz {a} vs jittered {j}");
        assert!((a - o).abs() > 0.03, "{ms} ms: the per-frame decay is separated");
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Review R-10, R-11 on the GPU: in real time a static picture keeps its brightness (within FP16) through
/// jitter, dropped frames and a 10 minute gap (600,000 ms), and a trail decays over the gap like over
/// 250 ms (the clamp), not like over 10 minutes (it would be exactly 0 then). The frame whose measured
/// period is the gap decays the history it leaves to the next frame by that period (module doc of
/// `src/gpu/stages/phosphor.rs`: the step lags the measured periods by one frame).
#[test]
fn real_time_static_picture_and_long_gap() {
    let Some(mut gpu) = common::gpu_or_skip("real_time_static_picture_and_long_gap") else {
        return;
    };
    let counter = gpu.validation_counter();
    let p = params(true, 0.0, [1.0; 3]);
    let pic = picture(W, H);
    for (k, dt) in [16.7_f32, 16.2, 33.4, 17.0, 600_000.0, 16.6, 0.0, 16.8]
        .iter()
        .cycle()
        .take(40)
        .enumerate()
    {
        let y = run_at(&mut gpu, &p, &pic, *dt);
        for (i, (&got, &x)) in y.rgba.iter().zip(&pic.rgba).enumerate() {
            assert!(
                rel(f64::from(got), f64::from(x)) <= FP16 / 2.0 + 1e-5,
                "frame {k} ({dt} ms): texel {} channel {}: {got} vs {x}",
                i / 4,
                i % 4
            );
        }
    }
    // A trail across the gap: white, a black frame measured after the gap, then a 60 Hz black frame.
    let white = image(W, H, |_, _| [1.0; 3]);
    let zero = black(W, H);
    let mut after = Vec::new();
    for gap in [600_000.0_f32, 250.0, 1000.0 / 60.0] {
        gpu.clear_history();
        for _ in 0..40 {
            let _ = run_at(&mut gpu, &p, &white, 1000.0 / 60.0);
        }
        let first = run_at(&mut gpu, &p, &zero, gap).rgba[1];
        after.push((first, run_at(&mut gpu, &p, &zero, 1000.0 / 60.0).rgba[1]));
    }
    println!(
        "green trail (first black, next black): 10 minute gap {:?}, 250 ms {:?}, 16.7 ms {:?}",
        after[0], after[1], after[2]
    );
    assert_eq!(after[0], after[1], "a 10 minute gap decays like 250 ms");
    assert!(after[0].1 > 0.0, "a pause does not decay the trail by hours");
    assert!(after[0].1 < after[2].1, "the gap decays more than a 60 Hz frame");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

// ---------------------------------------------------------------------------------------------------------
// Artistic controls (owner feedback 2026-11-20, QUESTIONS PHY-17): `phosphor_decay_scale`,
// `phosphor_extra_afterglow_rgb`, `phosphor_extra_afterglow_time_ms`; oracle `DecayFit::controlled_feedback` and
// `controlled_trail_ratios` (crt-model, which also checks that the scale multiplies every time constant exactly).

/// Persistence on with the artistic controls: decay scale `scale`, extra afterglow share `extra` per channel at
/// `extra_ms`.
fn controlled(rate_hz: f32, scale: f32, extra: [f32; 3], extra_ms: f32) -> Params {
    Params {
        phosphor_decay_scale: scale,
        phosphor_extra_afterglow_rgb: extra,
        phosphor_extra_afterglow_time_ms: extra_ms,
        ..params(true, rate_hz, [1.0; 3])
    }
}

/// The oracle controls of a [`controlled`] parameter set, channel `ch`.
fn decay_controls(p: &Params, ch: usize) -> DecayControls {
    DecayControls {
        time_scale: f64::from(p.phosphor_decay_scale),
        extra_share: f64::from(p.phosphor_extra_afterglow_rgb[ch]),
        extra_tau_ms: f64::from(p.phosphor_extra_afterglow_time_ms),
    }
}

/// GPU against the oracle with the controls, manual 60 Hz: a single excitation then 60 black frames. Per
/// channel y_k / y_0 equals the closed-form trail of the time-scaled response plus the extra afterglow
/// (`controlled_trail_ratios`) at k = 1, 2, 3 within FP16 (red at every k where it is above 1e-4: its terms are
/// exact), follows the realised recurrence (`controlled_feedback`, what the GPU runs) at every k within FP16 plus
/// 1e-6 of y_0, and y_0 / x is its w0. Scales 1, 5 and 20 without extra afterglow, scale 1 and 5 with one.
#[test]
fn controls_follow_the_oracle() {
    let Some(mut gpu) = common::gpu_or_skip("controls_follow_the_oracle") else {
        return;
    };
    let counter = gpu.validation_counter();
    let t = frame_period_ms(60.0);
    let pic = image(W, H, |_, _| [1.0, 1.0, 1.0]);
    let zero = black(W, H);
    let cases = [
        controlled(60.0, 1.0, [0.0; 3], 100.0),
        controlled(60.0, 5.0, [0.0; 3], 100.0),
        controlled(60.0, 20.0, [0.0; 3], 100.0),
        controlled(60.0, 1.0, [1.0, 0.5, 0.25], 200.0),
        controlled(60.0, 5.0, [0.5, 1.0, 2.0], 50.0),
    ];
    for p in &cases {
        gpu.clear_history();
        assert_eq!(run(&mut gpu, p, &zero), zero);
        let mut out = vec![run(&mut gpu, p, &pic)];
        for _ in 1..60 {
            out.push(run(&mut gpu, p, &zero));
        }
        for (ch, (name, fit)) in fits().into_iter().enumerate() {
            let ctl = decay_controls(p, ch);
            let r = fit.controlled_trail_ratios(t, TRAIL_HORIZON_MS, ctl);
            let fb = fit.controlled_feedback(t, TRAIL_HORIZON_MS, ctl);
            let y0 = f64::from(out[0].rgba[ch]);
            let what = format!("{name} scale {} extra {}", ctl.time_scale, ctl.extra_share);
            assert!(rel(y0, fb.w0) <= FP16 + 1e-5, "{what}: y0 {y0} vs w0 {}", fb.w0);
            let exact = if ch == 0 { 59 } else { 3 };
            for k in 1..60usize {
                let yk = f64::from(out[k].rgba[ch]);
                let realised = fb.trail(k as u32);
                assert!(
                    (yk / y0 - realised).abs() <= (FP16 + 1e-5) * realised + 1e-6 / y0,
                    "{what} k {k}: {} vs recurrence {realised}",
                    yk / y0
                );
                if k <= exact && r[k - 1] > 1e-4 {
                    assert!(
                        rel(yk / y0, r[k - 1]) <= FP16 + 1e-5,
                        "{what} k {k}: {} vs closed form {}",
                        yk / y0,
                        r[k - 1]
                    );
                }
            }
            println!(
                "{what}: w0 {:.4}, y1..3 / y0 = {:.4} {:.4} {:.4} (closed form {:.4} {:.4} {:.4}), y30 / y0 = {:.5}",
                y0,
                out[1].rgba[ch] / out[0].rgba[ch],
                out[2].rgba[ch] / out[0].rgba[ch],
                out[3].rgba[ch] / out[0].rgba[ch],
                r[0],
                r[1],
                r[2],
                out[30].rgba[ch] / out[0].rgba[ch],
            );
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// A static picture keeps its brightness (within FP16) whatever the controls, in real time with jittered
/// frame periods, from the first frame and after a control change (the change restarts the history primed to
/// the picture, so nothing flashes).
#[test]
fn static_picture_at_any_control() {
    let Some(mut gpu) = common::gpu_or_skip("static_picture_at_any_control") else {
        return;
    };
    let counter = gpu.validation_counter();
    let pic = picture(W, H);
    let cases = [
        controlled(0.0, 1.0, [0.0; 3], 100.0),
        controlled(0.0, 5.0, [0.0; 3], 100.0),
        controlled(0.0, 30.0, [0.0; 3], 100.0),
        controlled(0.0, 1.0, [1.0; 3], 300.0),
        controlled(0.0, 30.0, [2.0, 0.5, 10.0], 1000.0),
        controlled(0.0, 100.0, [10.0; 3], 5.0),
    ];
    let periods = [16.7_f32, 6.9, 13.9, 8.3, 16.6, 7.0];
    for (i, p) in cases.iter().enumerate() {
        for (k, dt) in periods.iter().cycle().take(30).enumerate() {
            let y = run_at(&mut gpu, p, &pic, *dt);
            for (j, (&got, &x)) in y.rgba.iter().zip(&pic.rgba).enumerate() {
                assert!(
                    rel(f64::from(got), f64::from(x)) <= FP16 / 2.0 + 1e-5,
                    "case {i} frame {k} ({dt} ms): texel {} channel {}: {got} vs {x}",
                    j / 4,
                    j % 4
                );
            }
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Review R-11 with the controls: in the default real-time mode the green trail after a static white picture
/// is switched off agrees, at the same time since the last lit frame began (83.3 and 250 ms), within 1 % of the
/// steady state (plus FP16) at 60 Hz and at 144 Hz, with decay scale 5 and with an extra afterglow (0.5 at
/// 200 ms); and the controls lengthen the trail (decay scale 5 above scale 1, the afterglow above both).
#[test]
fn controlled_trail_is_real_time_at_60_and_144_hz() {
    let Some(mut gpu) = common::gpu_or_skip("controlled_trail_is_real_time_at_60_and_144_hz") else {
        return;
    };
    let counter = gpu.validation_counter();
    let (p60, p144) = (1000.0_f32 / 60.0, 1000.0_f32 / 144.0);
    let mut at250 = Vec::new();
    for p in [
        controlled(0.0, 1.0, [0.0; 3], 100.0),
        controlled(0.0, 5.0, [0.0; 3], 100.0),
        controlled(0.0, 1.0, [0.5; 3], 200.0),
    ] {
        let c60 = trail_in_time(&mut gpu, &p, &[p60; 80], &[p60; 16]);
        let c144 = trail_in_time(&mut gpu, &p, &[p144; 80], &[p144; 36]);
        for ms in [1000.0 / 12.0, 250.0] {
            let (a, b) = (sample_at(&c60, ms), sample_at(&c144, ms));
            println!(
                "scale {} extra {}: green trail at {ms:.1} ms: 60 Hz {a:.5}, 144 Hz {b:.5}",
                p.phosphor_decay_scale, p.phosphor_extra_afterglow_rgb[1]
            );
            assert!((a - b).abs() < 0.01 + FP16 * a, "{ms} ms: 60 Hz {a} vs 144 Hz {b}");
        }
        at250.push(sample_at(&c60, 250.0));
    }
    assert!(at250[1] > at250[0] && at250[2] > at250[1], "{at250:?}");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

// The global trail balance (owner feedback 2026-11-21, QUESTIONS PHY-18): `phosphor_persistence_blend`, oracle
// `crt_model::phosphor::blend_trail` and `blended_trail_ratio`.

/// The blend against the oracle, manual 60 Hz, with the physical decay and with an extra afterglow: a single
/// excitation then black frames, run at blend 0, 0.5 and 1 from the same cleared history.
/// - 0: no trail at all, every output is the frame's own light exactly (bit for bit);
/// - 1: the recurrence itself, y_k / y_0 within FP16 of `FrameFeedback::trail` (the previous behaviour);
/// - 0.5: every output within FP16 of the oracle `x + 0.5 (y - x)` run in f64 on the same inputs, the ratios
///   y_k / y_0 equal `blended_trail_ratio`, and the trail level is half the blend-1 level.
///
/// Then the live switch: the value changed in the middle of a trail (0 to 1, after three frames at 0) shows the
/// full trail of the earlier excitation at once (the history kept running while the trail was hidden: no
/// restart), and a static picture keeps its brightness at every value.
#[test]
fn blend_follows_the_oracle() {
    let Some(mut gpu) = common::gpu_or_skip("blend_follows_the_oracle") else {
        return;
    };
    let counter = gpu.validation_counter();
    let t = frame_period_ms(60.0);
    let pic = image(W, H, |_, _| [1.0, 1.0, 1.0]);
    let zero = black(W, H);
    for base in [
        params(true, 60.0, [1.0; 3]),
        controlled(60.0, 1.0, [1.0, 0.5, 0.25], 200.0),
    ] {
        let mut runs = Vec::new();
        for blend in [0.0_f32, 0.5, 1.0] {
            let p = Params {
                phosphor_persistence_blend: blend,
                ..base
            };
            gpu.clear_history();
            assert_eq!(run(&mut gpu, &p, &zero), zero);
            let mut out = vec![run(&mut gpu, &p, &pic)];
            for _ in 1..20 {
                out.push(run(&mut gpu, &p, &zero));
            }
            runs.push(out);
        }
        for (ch, (name, fit)) in fits().into_iter().enumerate() {
            let fb = fit.controlled_feedback(t, TRAIL_HORIZON_MS, decay_controls(&base, ch));
            let what = format!("{name} extra {}", base.phosphor_extra_afterglow_rgb[ch]);
            let (none, half, full) = (&runs[0], &runs[1], &runs[2]);
            // Blend 0: the frame's own light, exactly.
            for (k, img) in none.iter().enumerate() {
                let want = if k == 0 { pic.rgba[ch] } else { 0.0 };
                assert_eq!(img.rgba[ch], want, "{what} blend 0 frame {k}");
            }
            // Blend 0.5 against the f64 oracle of the same frames; blend 1 against the plain recurrence.
            let mut s = [0.0; FEEDBACK_TERMS];
            let y0_full = f64::from(full[0].rgba[ch]);
            for k in 0..20usize {
                let x = if k == 0 { 1.0 } else { 0.0 };
                let y = fb.step(&mut s, x);
                let want = blend_trail(x, y, 0.5);
                let got = f64::from(half[k].rgba[ch]);
                assert!(
                    (got - want).abs() <= (FP16 + 1e-5) * want.abs() + 1e-7,
                    "{what} blend 0.5 frame {k}: {got} vs oracle {want}"
                );
                if k == 0 {
                    continue;
                }
                let tk = fb.trail(k as u32);
                let yk_full = f64::from(full[k].rgba[ch]);
                assert!(
                    (yk_full / y0_full - tk).abs() <= (FP16 + 1e-5) * tk + 1e-6,
                    "{what} blend 1 k {k}: {} vs recurrence {tk}",
                    yk_full / y0_full
                );
                if tk > 1e-4 {
                    let ratio = got / f64::from(half[0].rgba[ch]);
                    let oracle = blended_trail_ratio(fb.w0, tk, 0.5);
                    assert!(
                        rel(ratio, oracle) <= 2.0 * FP16 + 1e-5,
                        "{what} k {k}: {ratio} vs {oracle}"
                    );
                    assert!(
                        rel(got, 0.5 * yk_full) <= 2.0 * FP16 + 1e-5,
                        "{what} k {k}: half the trail"
                    );
                }
            }
            println!(
                "{what}: frame 0 / trail 1 to 3 (share of the excitation): blend 1 {:.4} / {:.4} {:.4} {:.4}; \
                 blend 0.5 {:.4} / {:.4} {:.4} {:.4}; blend 0 {:.4} / {:.4}",
                full[0].rgba[ch],
                full[1].rgba[ch],
                full[2].rgba[ch],
                full[3].rgba[ch],
                half[0].rgba[ch],
                half[1].rgba[ch],
                half[2].rgba[ch],
                half[3].rgba[ch],
                none[0].rgba[ch],
                none[1].rgba[ch],
            );
        }
    }
    // Live switch: three trail frames hidden at blend 0, then blend 1 shows the trail at once, the same as a run
    // at blend 1 throughout (no restart of the history).
    let shown = params(true, 60.0, [1.0; 3]);
    let hidden = Params {
        phosphor_persistence_blend: 0.0,
        ..shown
    };
    gpu.clear_history();
    let _ = run(&mut gpu, &hidden, &zero);
    let _ = run(&mut gpu, &hidden, &pic);
    for _ in 0..3 {
        assert_eq!(run(&mut gpu, &hidden, &zero), zero);
    }
    let switched = run(&mut gpu, &shown, &zero);
    gpu.clear_history();
    let _ = run(&mut gpu, &shown, &zero);
    let _ = run(&mut gpu, &shown, &pic);
    for _ in 0..3 {
        let _ = run(&mut gpu, &shown, &zero);
    }
    let reference = run(&mut gpu, &shown, &zero);
    assert!(switched.rgba[1] > 0.0, "the trail is there right after the switch");
    assert_eq!(
        switched, reference,
        "no restart: the same trail as with blend 1 throughout"
    );
    // A static picture at every blend, real time with jittered periods.
    let still = picture(W, H);
    for blend in [0.0_f32, 0.25, 0.5, 1.0] {
        let p = Params {
            phosphor_persistence_blend: blend,
            phosphor_frame_rate_hz: 0.0,
            ..shown
        };
        gpu.clear_history();
        for dt in [16.7_f32, 6.9, 13.9, 8.3, 16.6, 7.0] {
            let y = run_at(&mut gpu, &p, &still, dt);
            for (&got, &x) in y.rgba.iter().zip(&still.rgba) {
                assert!(
                    rel(f64::from(got), f64::from(x)) <= FP16 / 2.0 + 1e-5,
                    "blend {blend}: {got} vs {x}"
                );
            }
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// GPU time of the stage on a 1920 x 1440 tube (gate 10), persistence off (copy) and on.
#[test]
fn stage_cost() {
    let Some(mut gpu) = common::gpu_or_skip("stage_cost") else {
        return;
    };
    let counter = gpu.validation_counter();
    let pic = picture(1920, 1440);
    for on in [false, true] {
        let p = params(on, 60.0, [1.0; 3]);
        let mut ms = Vec::new();
        for _ in 0..12 {
            let _ = run(&mut gpu, &p, &pic);
            if let Some(m) = gpu.last_timings().and_then(|t| t.stage_ms("phosphor")) {
                ms.push(m);
            }
        }
        // Single frames with readbacks in between: the GPU clocks ramp, so the minimum is the estimate.
        ms.sort_by(f64::total_cmp);
        println!(
            "phosphor 1920x1440, persistence {on}: GPU ms min {:?}, median {:?}",
            ms.first(),
            ms.get(ms.len() / 2)
        );
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

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
//! 1 s: the fit rolls off smoothly after the window, so its total trail is 13 to 17 % above the 1 s
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
//! The output image is FP16 and FP16 stores truncate on the dev GPU: one value is within 2^-10 relative
//! (one FP16 step), a ratio of two within 2 x 2^-10, hence 2.5e-3 relative for the exact ratios. The
//! history is FP32.

mod common;

use crt_model::phosphor::{DecayFit, KUHN_BLUE, KUHN_GREEN, KUHN_RED, TRAIL_HORIZON_MS};
use crt_model::units::frame_period_ms;
use ultimatecrt::gpu::{FrameConstants, Headless, ImageF32, Settings, StageInput, StageRun, TargetFormat};
use ultimatecrt::params::Params;

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

fn params(on: bool, rate_hz: f32, tail: [f32; 3]) -> Params {
    Params {
        phosphor_persistence: on,
        phosphor_frame_rate_hz: rate_hz,
        phosphor_tail_strength_rgb: tail,
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
    let mut out = vec![run(&mut gpu, &p, &pic)];
    let zero = black(W, H);
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
                        rel(ratio, oracle[k - 1]) < 2.5 * FP16,
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
                rel(ratio, oracle) < 2.5 * FP16,
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

/// A static picture converges to the no-persistence picture (steady-state gain 1, within one FP16 step
/// plus 1e-4 after 400 frames); at frame 120 it is still short by the predicted remainder of the
/// fitted tail, w0 sum_{k > 120} t_k, taken from the crt-model fit (about 1 %, see the module doc).
#[test]
fn static_picture_keeps_its_brightness() {
    let Some(mut gpu) = common::gpu_or_skip("static_picture_keeps_its_brightness") else {
        return;
    };
    let counter = gpu.validation_counter();
    let p = params(true, 60.0, [1.0; 3]);
    let t = frame_period_ms(60.0);
    let pic = picture(W, H);
    let mut y = ImageF32 {
        width: 0,
        height: 0,
        rgba: Vec::new(),
    };
    for n in 0..400 {
        y = run(&mut gpu, &p, &pic);
        if n == 120 {
            for (ch, (name, fit)) in fits().into_iter().enumerate() {
                let fb = fit.frame_feedback(t, TRAIL_HORIZON_MS);
                let short: f64 = fb.w0 * (121..5000).map(|k| fb.trail(k)).sum::<f64>();
                let got = f64::from(y.rgba[ch]) / f64::from(pic.rgba[ch]);
                println!("{name} at frame 120: {got:.5}, predicted {:.5}", 1.0 - short);
                assert!((got - (1.0 - short)).abs() <= FP16 + 1e-5, "{name} frame 120");
            }
        }
    }
    for (i, (&got, &x)) in y.rgba.iter().zip(&pic.rgba).enumerate() {
        assert!(
            rel(f64::from(got), f64::from(x)) <= FP16 + 1e-4,
            "texel {} channel {}: {got} vs {x}",
            i / 4,
            i % 4
        );
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

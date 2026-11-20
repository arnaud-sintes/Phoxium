//! Phosphor persistence through the whole live chain (owner feedback 2026-11-20, "the trail is too short / there
//! is no trail at all"; QUESTIONS PHY-17). The stage tests (`tests/stage_phosphor.rs`) inject light right before
//! the `phosphor` slot; here a captured BGRA frame goes through every slot the window loop runs (decode, gun,
//! beam, mask, phosphor, optics, geometry, encode) with the parameters of the defaults (persistence switched on)
//! and of every shipped preset, the frame period given to the real-time stages as at 60 and 144 Hz
//! (`Headless::set_frame_time_ms`; the default automatic trail rate, `phosphor_frame_rate_hz` 0).
//!
//! # Single excitation (derived by hand)
//!
//! Every slot after `phosphor` is linear and memoryless per channel (optics, geometry, the last chain image
//! before the colour matrix of `encode`), and every slot before it is memoryless (ABL, the only one with a time
//! constant, is switched off: its low-pass would change the drive between frames, see `tests/presets.rs`). A
//! black frame primes the cleared history to the black light b (not exactly 0: flare, black level), then one
//! frame with a white bar x0, then black frames. The phosphor output is y_0 = w0 x0 + (1 - w0) b and
//! y_k - b = w0 R_k (x0 - b) for k >= 1 (R_k the trail ratios), so per channel, summed over the whole last chain
//! image L, (sum L_k - sum L_b) / (sum L_0 - sum L_b) = R_k whatever the beam, mask, glow and geometry do, and
//! must equal the oracle `DecayFit::controlled_trail_ratios` (physical: the CRT.md table, green 5.36 / 2.97 /
//! 2.03 %, blue 4.72 / 2.60 / 1.78 % at 60 Hz, red none).
//!
//! # Moving bar (what the eye sees)
//!
//! A bar moving by more than its blurred width per frame, so the trail positions do not overlap: the display
//! output (encoded, 0..1) at the bar and at the places it was 1 to 4 frames earlier, as 8-bit codes, printed
//! for the defaults, two presets and the artistic controls. Recorded in QUESTIONS PHY-17.

mod common;

use crt_model::phosphor::{DecayControls, DecayFit, KUHN_BLUE, KUHN_GREEN, KUHN_RED, TRAIL_HORIZON_MS};
use ultimatecrt::gpu::{Headless, ImageF32, Settings, SourceFrame, StageReadback, TargetFormat};
use ultimatecrt::params::{ParamValue, Params};
use ultimatecrt::presets::Library;
use ultimatecrt::scaling::View;

const SRC: (u32, u32) = (320, 240);
const TARGET: (u32, u32) = (960, 720);
const BAR: u32 = 12;

/// A black BGRA source with a white bar of [`BAR`] columns starting at column `x` (`None`: all black).
fn bar(x: Option<u32>) -> Vec<u8> {
    let mut v = vec![0u8; (SRC.0 * SRC.1 * 4) as usize];
    for row in 0..SRC.1 {
        for col in 0..SRC.0 {
            let i = ((row * SRC.0 + col) * 4) as usize;
            let lit = x.is_some_and(|x| col >= x && col < x + BAR);
            v[i..i + 4].copy_from_slice(&if lit { [255; 4] } else { [0, 0, 0, 255] });
        }
    }
    v
}

fn frame(data: &[u8]) -> SourceFrame<'_> {
    SourceFrame {
        width: SRC.0,
        height: SRC.1,
        bgra: data,
    }
}

/// One frame of the full chain: (the last chain image, the encoded output).
fn render(gpu: &mut Headless, p: &Params, data: &[u8]) -> (ImageF32, ImageF32) {
    let r = gpu
        .render(
            Some(frame(data)),
            View::full(SRC.0, SRC.1),
            &Settings::default(),
            p,
            TARGET,
            TargetFormat::Rgba32F,
        )
        .expect("render");
    let (_, last) = r.stages.last().expect("chain image").clone();
    (last, r.output)
}

fn sums(img: &ImageF32) -> [f64; 3] {
    let mut s = [0.0; 3];
    for t in img.rgba.as_chunks::<4>().0 {
        for c in 0..3 {
            s[c] += f64::from(t[c]);
        }
    }
    s
}

fn fits() -> [(&'static str, DecayFit<'static>); 3] {
    [("red", KUHN_RED), ("green", KUHN_GREEN), ("blue", KUHN_BLUE)]
}

/// The oracle controls of `p`, channel `ch`.
fn controls(p: &Params, ch: usize) -> DecayControls {
    DecayControls {
        time_scale: f64::from(p.phosphor_decay_scale),
        extra_share: f64::from(p.phosphor_extra_afterglow_rgb[ch]),
        extra_tau_ms: f64::from(p.phosphor_extra_afterglow_time_ms),
    }
}

/// The parameter sets of the checks: the defaults with persistence on, every shipped preset (ABL off, see the
/// module doc), and the defaults with the artistic controls.
fn configs() -> Vec<(String, Params)> {
    let mut out = Vec::new();
    let mut d = Params::default();
    assert!(!d.phosphor_persistence, "persistence is off in the table defaults");
    d.phosphor_persistence = true;
    out.push(("defaults + persistence".to_string(), d));
    for preset in Library::embedded().list() {
        let mut p = preset.params();
        assert!(p.phosphor_persistence, "{}", preset.id);
        assert!(p.set("abl_enable", ParamValue::Bool(false)));
        out.push((preset.id.clone(), p));
    }
    let mut s = d;
    s.phosphor_decay_scale = 20.0;
    out.push(("defaults, decay scale 20".to_string(), s));
    let mut g = d;
    g.phosphor_extra_afterglow_rgb = [1.0; 3];
    g.phosphor_extra_afterglow_time_ms = 200.0;
    out.push(("defaults, extra afterglow 1 at 200 ms".to_string(), g));
    out
}

/// Single excitation through the whole chain (module doc): per channel the summed trail ratios of frames 1 to 3
/// equal the oracle within 1 % relative (FP16 images along the chain, sums over the frame) for green and blue,
/// red's trail is exactly 0 with the physical decay; at 60 and 144 Hz frame periods, for the defaults with
/// persistence on, the ten presets and the artistic controls.
#[test]
fn full_chain_trail_equals_the_oracle() {
    let Some(mut gpu) = common::gpu_or_skip("persistence full chain") else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.set_read_stages(StageReadback::Last);
    let (black, lit) = (bar(None), bar(Some(SRC.0 / 2)));
    let mut bad = Vec::new();
    for (name, p) in configs() {
        for hz in [60.0_f64, 144.0] {
            let t = 1000.0 / hz;
            gpu.set_frame_time_ms(Some(t as f32));
            // The automatic trail rate follows a new display rate over its 31-frame window, refitting (and so
            // restarting the history) while it moves, and a last move under 5 % refits only once the new value
            // held for a full window: settle it first (stage unit test
            // `rate_change_restarts_the_trail_only_while_it_settles`). Measured with 40 frames instead of 80:
            // every 60 Hz run after a 144 Hz one used a fit for about 63 Hz, trail ratios 2.9 % high.
            for _ in 0..80 {
                let _ = render(&mut gpu, &p, &black);
            }
            gpu.clear_history();
            let _ = render(&mut gpu, &p, &black);
            let lb = sums(&render(&mut gpu, &p, &black).0);
            let l0 = sums(&render(&mut gpu, &p, &lit).0);
            let lk: Vec<[f64; 3]> = (0..3).map(|_| sums(&render(&mut gpu, &p, &black).0)).collect();
            let mut line = format!("{name} {hz} Hz:");
            for (ch, (cname, fit)) in fits().into_iter().enumerate() {
                let ctl = controls(&p, ch);
                let tail = fit.with_tail_scale(f64::from(p.phosphor_tail_strength_rgb[ch]));
                let oracle = tail.controlled_trail_ratios(t, TRAIL_HORIZON_MS, ctl);
                let lit_sum = l0[ch] - lb[ch];
                assert!(lit_sum > 0.0, "{name} {hz} Hz {cname}: the bar is not lit");
                let got: Vec<f64> = lk.iter().map(|l| (l[ch] - lb[ch]) / lit_sum).collect();
                line += &format!(
                    " {cname} {:.4} {:.4} {:.4} (oracle {:.4} {:.4} {:.4});",
                    got[0], got[1], got[2], oracle[0], oracle[1], oracle[2]
                );
                for k in 0..3 {
                    let ok = if oracle[k] > 1e-4 {
                        ((got[k] - oracle[k]) / oracle[k]).abs() < 0.01
                    } else {
                        got[k].abs() < 1e-4
                    };
                    if !ok {
                        bad.push(format!(
                            "{name} {hz} Hz {cname} k {}: {} vs oracle {}",
                            k + 1,
                            got[k],
                            oracle[k]
                        ));
                    }
                }
            }
            println!("{line}");
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
    gpu.set_frame_time_ms(None);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// The display output where a moving bar is now and where it was 1 to 4 frames ago (module doc), as 8-bit codes
/// of the green channel (the brightest trail of the P22). Each place is the set of output pixels a static bar there
/// lights above half its peak; the value is the mean there. Checks: the physical trail is there, below the bar
/// and falling, and an extra afterglow keeps it clearly brighter four frames behind.
#[test]
fn moving_bar_trail_in_the_output() {
    let Some(mut gpu) = common::gpu_or_skip("persistence moving bar") else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.set_read_stages(StageReadback::Last);
    let step = 40;
    let positions: Vec<u32> = (0..6).map(|i| 30 + i * step).collect();
    let lib = Library::embedded();
    let mut cases: Vec<(String, Params)> = configs()
        .into_iter()
        .filter(|(n, _)| n.starts_with("defaults"))
        .collect();
    for id in ["pvm-20m4u", "kv-trinitron-tv"] {
        let mut p = lib.find(id).expect(id).params();
        assert!(p.set("abl_enable", ParamValue::Bool(false)));
        cases.push((id.to_string(), p));
    }
    // The manual trail rate found in the owner's settings file (397 Hz: each 60 Hz output frame decays the
    // trail by 2.5 ms of phosphor time only).
    let mut manual = cases[0].1;
    manual.phosphor_frame_rate_hz = 397.0;
    cases.push(("defaults, manual trail rate 397 Hz".to_string(), manual));
    let mut strong = cases[0].1;
    strong.phosphor_extra_afterglow_rgb = [2.0; 3];
    strong.phosphor_extra_afterglow_time_ms = 300.0;
    cases.push(("defaults, extra afterglow 2 at 300 ms".to_string(), strong));
    let mut first_trail = Vec::new();
    for (name, p) in &cases {
        gpu.set_frame_time_ms(Some(1000.0 / 60.0));
        // Regions: a static bar at each position (a cleared history is primed: the static picture as is).
        let regions: Vec<Vec<usize>> = positions
            .iter()
            .map(|&x| {
                gpu.clear_history();
                let (_, out) = render(&mut gpu, p, &bar(Some(x)));
                let g: Vec<f32> = out.rgba.as_chunks::<4>().0.iter().map(|t| t[1]).collect();
                let peak = g.iter().cloned().fold(0.0, f32::max);
                (0..g.len()).filter(|&i| g[i] > 0.5 * peak).collect()
            })
            .collect();
        gpu.clear_history();
        let mut out = None;
        let _ = render(&mut gpu, p, &bar(None));
        for &x in &positions {
            out = Some(render(&mut gpu, p, &bar(Some(x))).1);
        }
        let out = out.unwrap();
        let mean = |r: &Vec<usize>| r.iter().map(|&i| f64::from(out.rgba[4 * i + 1])).sum::<f64>() / r.len() as f64;
        let n = positions.len() - 1;
        let codes: Vec<f64> = (0..=4).map(|k| 255.0 * mean(&regions[n - k])).collect();
        println!(
            "{name}, 60 Hz, green output (8-bit): bar {:.1}, 1 to 4 frames behind {:.1} {:.1} {:.1} {:.1}",
            codes[0], codes[1], codes[2], codes[3], codes[4]
        );
        assert!(codes[1] > 0.5 && codes[1] < codes[0], "{name}: {codes:?}");
        assert!(
            codes.windows(2).skip(1).all(|w| w[1] <= w[0] + 0.5),
            "{name}: {codes:?}"
        );
        first_trail.push((name.clone(), codes[4]));
    }
    // Four frames behind (67 ms at 60 Hz) the extra afterglow keeps the trail clearly brighter than the P22.
    let of = |n: &str| first_trail.iter().find(|(m, _)| m.as_str() == n).unwrap().1;
    let phys = of("defaults + persistence");
    assert!(
        of("defaults, extra afterglow 1 at 200 ms") > 1.4 * phys,
        "{first_trail:?}"
    );
    assert!(of("defaults, extra afterglow 2 at 300 ms") > of("defaults, extra afterglow 1 at 200 ms"));
    gpu.set_frame_time_ms(None);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

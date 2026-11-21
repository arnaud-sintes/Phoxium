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
//! With the global trail balance b (`phosphor_persistence_blend`, default 0.5, owner feedback 2026-11-21) the
//! phosphor output is x + b (y - x): y_0 - b_l = (1 - b (1 - w0)) (x0 - b_l) and y_k - b_l = b w0 R_k (x0 - b_l), so
//! the ratio becomes b w0 R_k / (1 - b (1 - w0)), the oracle `blended_trail_ratio` (R_k at b = 1).
//!
//! # Moving bar (what the eye sees)
//!
//! A bar moving by more than its blurred width per frame, so the trail positions do not overlap: the display
//! output (encoded, 0..1) at the bar and at the places it was 1 to 4 frames earlier, as 8-bit codes, printed
//! for the defaults, two presets and the artistic controls. Recorded in QUESTIONS PHY-17.

mod common;

use crt_model::phosphor::{
    blended_trail_ratio, DecayControls, DecayFit, KUHN_BLUE, KUHN_GREEN, KUHN_RED, TRAIL_HORIZON_MS,
};
use phoxium::gpu::{Headless, ImageF32, Settings, SourceFrame, StageReadback, TargetFormat};
use phoxium::params::{ParamValue, Params};
use phoxium::presets::Library;
use phoxium::scaling::View;

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

/// The oracle trail ratios (y_k - b) / (y_0 - b), k = 1, 2, ..., of `p` for channel `ch` at the frame period `t`
/// (ms): the closed-form trail of the controlled decay, under the blend of `p` (module doc).
fn oracle_ratios(fit: &DecayFit<'_>, p: &Params, ch: usize, t: f64) -> Vec<f64> {
    let ctl = controls(p, ch);
    let tail = fit.with_tail_scale(f64::from(p.phosphor_tail_strength_rgb[ch]));
    let w0 = tail.controlled_feedback(t, TRAIL_HORIZON_MS, ctl).w0;
    let blend = f64::from(p.phosphor_persistence_blend);
    tail.controlled_trail_ratios(t, TRAIL_HORIZON_MS, ctl)
        .into_iter()
        .map(|r| blended_trail_ratio(w0, r, blend))
        .collect()
}

/// The parameter sets of the checks: the defaults with persistence on (the default blend 0.5, and the full trail),
/// every shipped preset (ABL off, see the module doc), and the defaults with the artistic controls.
fn configs() -> Vec<(String, Params)> {
    let mut out = Vec::new();
    let mut d = Params::default();
    assert!(!d.phosphor_persistence, "persistence is off in the table defaults");
    d.phosphor_persistence = true;
    assert_eq!(
        d.phosphor_persistence_blend, 0.5,
        "the default blend shows half the trail"
    );
    out.push(("defaults + persistence".to_string(), d));
    let mut full = d;
    full.phosphor_persistence_blend = 1.0;
    out.push(("defaults + persistence, blend 1".to_string(), full));
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
            // The automatic trail rate follows a new display rate once its 64-frame mean settled, 16 frames later
            // (one history restart, about frame 78 of the new rate): settle it first (stage unit test
            // `rate_change_restarts_the_trail_only_while_it_settles`).
            for _ in 0..100 {
                let _ = render(&mut gpu, &p, &black);
            }
            gpu.clear_history();
            let _ = render(&mut gpu, &p, &black);
            let lb = sums(&render(&mut gpu, &p, &black).0);
            let l0 = sums(&render(&mut gpu, &p, &lit).0);
            let lk: Vec<[f64; 3]> = (0..3).map(|_| sums(&render(&mut gpu, &p, &black).0)).collect();
            let mut line = format!("{name} {hz} Hz:");
            for (ch, (cname, fit)) in fits().into_iter().enumerate() {
                let oracle = oracle_ratios(&fit, &p, ch, t);
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
        .map(|(n, mut p)| {
            // The artistic controls at the full trail, as when they were recorded (QUESTIONS PHY-18).
            if n.contains("extra afterglow") || n.contains("decay scale") {
                p.phosphor_persistence_blend = 1.0;
            }
            (n, p)
        })
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
    strong.phosphor_persistence_blend = 1.0;
    strong.phosphor_extra_afterglow_rgb = [2.0; 3];
    strong.phosphor_extra_afterglow_time_ms = 300.0;
    cases.push(("defaults, extra afterglow 2 at 300 ms".to_string(), strong));
    // The global trail balance (owner feedback 2026-11-21): 0 (no trail), the default 0.5, 1 (the full trail).
    let mut none = cases[0].1;
    none.phosphor_persistence_blend = 0.0;
    cases.push(("defaults + persistence, blend 0".to_string(), none));
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
        if p.phosphor_persistence_blend > 0.0 {
            assert!(codes[1] > 0.5 && codes[1] < codes[0], "{name}: {codes:?}");
        } else {
            assert!(
                codes[1..].iter().all(|&c| c < 0.5),
                "{name}: no trail at blend 0: {codes:?}"
            );
        }
        assert!(
            codes.windows(2).skip(1).all(|w| w[1] <= w[0] + 0.5),
            "{name}: {codes:?}"
        );
        first_trail.push((name.clone(), codes[4]));
    }
    // Four frames behind (67 ms at 60 Hz) the extra afterglow keeps the trail clearly brighter than the P22.
    let of = |n: &str| first_trail.iter().find(|(m, _)| m.as_str() == n).unwrap().1;
    // The blend: 0 leaves no trail; the default 0.5 lies between 0 and the full trail (1).
    let (b0, b05, b1) = (
        of("defaults + persistence, blend 0"),
        of("defaults + persistence"),
        of("defaults + persistence, blend 1"),
    );
    assert!(b0 < 0.5 && b05 > b0 + 1.0 && b1 > b05 + 1.0, "{b0} {b05} {b1}");
    let phys = of("defaults + persistence, blend 1");
    assert!(
        of("defaults, extra afterglow 1 at 200 ms") > 1.4 * phys,
        "{first_trail:?}"
    );
    assert!(of("defaults, extra afterglow 2 at 300 ms") > of("defaults, extra afterglow 1 at 200 ms"));
    gpu.set_frame_time_ms(None);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Column means of channel `ch` (0 red, 1 green, 2 blue) of an 8-bit output over the middle 20 % of its height, as
/// 8-bit codes (the live screenshot method).
fn column_means(img: &ImageF32, ch: usize) -> Vec<f64> {
    let (y0, y1) = ((img.height as f64 * 0.4) as u32, (img.height as f64 * 0.6) as u32);
    (0..img.width)
        .map(|x| {
            (y0..y1)
                .map(|y| 255.0 * f64::from(img.rgba[4 * (y * img.width + x) as usize + ch]))
                .sum::<f64>()
                / f64::from(y1 - y0)
        })
        .collect()
}

/// The bar of a green column profile: (left, right) columns around the maximum above half of it.
fn bar_span(g: &[f64]) -> (usize, usize) {
    let (xm, mx) = g
        .iter()
        .enumerate()
        .fold((0, 0.0), |(i, m), (j, &v)| if v > m { (j, v) } else { (i, m) });
    let (mut l, mut r) = (xm, xm);
    while l > 0 && g[l - 1] > 0.5 * mx {
        l -= 1;
    }
    while r + 1 < g.len() && g[r + 1] > 0.5 * mx {
        r += 1;
    }
    (l, r)
}

/// Mean of `v` over the columns [a, a + w], `None` when out of range.
fn span_mean(v: &[f64], a: i64, w: i64) -> Option<f64> {
    (a >= 0 && a + w < v.len() as i64).then(|| v[a as usize..=(a + w) as usize].iter().sum::<f64>() / (w + 1) as f64)
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// Live review 2026-11-20, the discrepancy of the moving-bar trail: live (`--pattern bar:8:24`, preset `delta-17in`,
/// 60 Hz windowed, output 1264 x 1181, screenshots) gave the bar 203 and 57, 39, 29, 24 one to four frames behind
/// (green, 8-bit), against 212 and 57, 44, 37, 33 for this file's `moving_bar_trail_in_the_output` (the defaults with
/// persistence on, 320 x 240 source, a 12 px bar stepping 40 px, 960 x 720 target, regions from static renders).
/// Here the live conditions headlessly: the same pattern generator, preset and output size, the 8-bit display
/// output (encode with the preset's noise dither), 60 Hz periods with +/- 0.3 ms jitter, the bar running for 200
/// frames (several laps, so older laps' trails add as live). Two measurements on the same frames:
/// - the live script's: the bar's columns above half its peak give its width bw, the step is round(24 bw / 8),
///   and the trail k frames behind is the mean over [L - k step + 0.15 bw, + 0.7 bw];
/// - the places the bar really was: its left edge found on a static render of each earlier position.
///
/// The script's step comes from the bar's blurred half-peak width (33 px for 8 source px of 3.95 px), so it is
/// about 99 px instead of 94.8, and its window drifts 4 px per frame off the old bar: at four frames it covers half
/// of it. Printed for the default, extra afterglow 0.5 at 150 ms, and decay scale 20 (the live cases); checked: the
/// script method on the headless output gives the live numbers within a few codes, and the true places give
/// clearly more from two frames on.
#[test]
fn live_bar_measurement_explained() {
    let Some(mut gpu) = common::gpu_or_skip("live bar measurement") else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.set_read_stages(StageReadback::None);
    let out = (1264, 1181);
    let pat: phoxium::patterns::Pattern = "bar:8:24".parse().unwrap();
    let mut base = Library::embedded().find("delta-17in").expect("delta-17in").params();
    // The live numbers above were measured with the full trail (before the blend existed).
    base.phosphor_persistence_blend = 1.0;
    let mut glow = base;
    glow.phosphor_extra_afterglow_rgb = [0.5; 3];
    glow.phosphor_extra_afterglow_time_ms = 150.0;
    let mut slow = base;
    slow.phosphor_decay_scale = 20.0;
    // The red trail of the live test: the preset's tube primaries are SMPTE-C, so the light of the green and blue
    // phosphors has a small red component in the panel's sRGB. With sRGB tube primaries it goes away.
    let mut srgb = base;
    srgb.tube_red_xy = [0.64, 0.33];
    srgb.tube_green_xy = [0.30, 0.60];
    srgb.tube_blue_xy = [0.15, 0.06];
    let frame_of = |n: u64| phoxium::patterns::generate(&pat, SRC.0, SRC.1, n);
    let draw = |gpu: &mut Headless, p: &Params, n: u64| {
        gpu.render(
            Some(frame(&frame_of(n))),
            View::full(SRC.0, SRC.1),
            &Settings::default(),
            p,
            out,
            TargetFormat::Bgra8Unorm,
        )
        .expect("render")
        .output
    };
    // The left edge and width of the static bar at each of the 40 positions of a lap (24 n mod 320).
    gpu.set_frame_time_ms(Some(1000.0 / 60.0));
    let places: Vec<(usize, usize)> = (0..40)
        .map(|n| {
            gpu.clear_history();
            let g = column_means(&draw(&mut gpu, &base, n), 1);
            let (l, r) = bar_span(&g);
            (l, r - l + 1)
        })
        .collect();
    let mut seed = 0x9e37_79b9_u32;
    let mut jitter = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (f64::from(seed) / f64::from(u32::MAX) - 0.5) * 0.6
    };
    let mut results = Vec::new();
    for (name, p) in [
        ("default", base),
        ("extra afterglow 0.5 at 150 ms", glow),
        ("decay scale 20", slow),
        ("sRGB tube primaries", srgb),
    ] {
        gpu.clear_history();
        let mut script: Vec<Vec<f64>> = vec![Vec::new(); 5];
        let mut truth: Vec<Vec<f64>> = vec![Vec::new(); 5];
        let mut red: Vec<Vec<f64>> = vec![Vec::new(); 5];
        for n in 0..240u64 {
            gpu.set_frame_time_ms(Some((1000.0 / 60.0 + jitter()) as f32));
            let img = draw(&mut gpu, &p, n);
            if n < 200 {
                continue;
            }
            let (g, r) = (column_means(&img, 1), column_means(&img, 0));
            // The live script.
            let (l, rr) = bar_span(&g);
            let bw = (rr - l + 1) as f64;
            let step = (24.0 * bw / 8.0).round() as i64;
            let (inl, inr) = (l as i64 + (bw * 0.25) as i64, rr as i64 - (bw * 0.25) as i64);
            script[0].push(span_mean(&g, inl, inr - inl).unwrap());
            for k in 1..5 {
                let a = l as i64 - k * step + (bw * 0.15) as i64;
                if let Some(v) = span_mean(&g, a, (bw * 0.7) as i64) {
                    script[k as usize].push(v);
                }
            }
            // The places the bar really was (same window inside the bar).
            for k in 0..5u64 {
                let (pl, pw) = places[((n - k) % 40) as usize];
                let a = pl as i64 + (pw as f64 * 0.15) as i64;
                // Positions that wrapped past the bar's own place are not a clean trail.
                if k > 0 && pl >= l {
                    continue;
                }
                if let Some(v) = span_mean(&g, a, (pw as f64 * 0.7) as i64) {
                    truth[k as usize].push(v);
                }
                if let Some(v) = span_mean(&r, a, (pw as f64 * 0.7) as i64) {
                    red[k as usize].push(v);
                }
            }
        }
        let med = |v: &Vec<Vec<f64>>| -> Vec<f64> { v.iter().map(|s| median(s.clone())).collect() };
        let (s, t, rd) = (med(&script), med(&truth), med(&red));
        println!(
            "{name}: live script method: bar {:.1}, 1 to 4 behind {:.1} {:.1} {:.1} {:.1}; at the true places: bar \
             {:.1}, {:.1} {:.1} {:.1} {:.1}; red at the true places {:.1} {:.1} {:.1} {:.1}",
            s[0], s[1], s[2], s[3], s[4], t[0], t[1], t[2], t[3], t[4], rd[1], rd[2], rd[3], rd[4]
        );
        results.push((name, s, t, rd));
    }
    // The default against the live medians (bar 203, 57 39 29 24): the script method reproduces them within 4 codes,
    // the true places are higher from two frames on.
    let (_, s, t, _) = &results[0];
    let live = [203.0, 57.0, 39.0, 29.0, 24.0];
    for k in 0..5 {
        assert!(
            (s[k] - live[k]).abs() < 4.0,
            "script method k {k}: {} vs live {}",
            s[k],
            live[k]
        );
    }
    for k in 2..5 {
        assert!(t[k] > s[k] + 3.0, "true places k {k}: {} vs script {}", t[k], s[k]);
    }
    // Red: a trail with the SMPTE-C primaries of the preset, none (dither noise only) with sRGB primaries.
    let (_, _, _, red_preset) = &results[0];
    let (_, _, _, red_srgb) = &results[3];
    assert!(red_preset[1] > 8.0, "{red_preset:?}");
    assert!(red_srgb[1..].iter().all(|&v| v < 2.0), "{red_srgb:?}");
    gpu.set_frame_time_ms(None);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// The window loop's measured frame periods under uneven pacing (live review 2026-11-20, diagnostics logs:
/// 0.3 to 2 ms, then about 33 ms, at 60 fps): strictly alternating 1 ms and 2 T - 1 ms, mean T = 16.67 ms.
struct Pacing {
    frame: usize,
}

impl Pacing {
    /// One frame of the full chain with the next paced period.
    fn render(&mut self, gpu: &mut Headless, p: &Params, data: &[u8]) -> (ImageF32, ImageF32) {
        let t = 1000.0_f32 / 60.0;
        gpu.set_frame_time_ms(Some(if self.frame.is_multiple_of(2) {
            1.0
        } else {
            2.0 * t - 1.0
        }));
        self.frame += 1;
        render(gpu, p, data)
    }

    /// Green and blue trail ratios R_1 to R_3 of a single excitation through the whole chain (module doc), from
    /// the history as it is: two black frames (the second is the black reference), one frame with the bar, three
    /// black frames.
    fn trail(&mut self, gpu: &mut Headless, p: &Params) -> [[f64; 3]; 2] {
        let (black, lit) = (bar(None), bar(Some(SRC.0 / 2)));
        let _ = self.render(gpu, p, &black);
        let lb = sums(&self.render(gpu, p, &black).0);
        let l0 = sums(&self.render(gpu, p, &lit).0);
        let lk: Vec<[f64; 3]> = (0..3).map(|_| sums(&self.render(gpu, p, &black).0)).collect();
        [1, 2].map(|ch| std::array::from_fn(|k| (lk[k][ch] - lb[ch]) / (l0[ch] - lb[ch])))
    }
}

/// Live review 2026-11-20 (bug: "the trail stays 0 after the A/B bypass", and the owner's "no trail at all"): the
/// preset `delta-17in` of the live test, the frame periods of uneven pacing ([`Pacing`]), the A/B bypass switched
/// on for 60 frames then off (every chain slot disabled, then the saved flags restored, as
/// `controls::bypass_plan` does). Before the bypass and once the history settled after it, the green and blue
/// trail ratios equal the 60 Hz oracle within 1 %: the stage steps by the mean period and does not restart its
/// history on the uneven intervals. With the trimmed-mean estimate of the earlier build the trail was 0 (the
/// history restarted, primed to the picture, at nearly every frame).
#[test]
fn trail_survives_uneven_pacing_and_the_bypass() {
    let Some(mut gpu) = common::gpu_or_skip("persistence under uneven pacing and the bypass") else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.set_read_stages(StageReadback::Last);
    let p = Library::embedded().find("delta-17in").expect("delta-17in").params();
    assert!(p.phosphor_persistence && !p.abl_enable);
    let mut pacing = Pacing { frame: 0 };
    let t = 1000.0 / 60.0;
    let oracle: Vec<Vec<f64>> = [KUHN_GREEN, KUHN_BLUE]
        .iter()
        .enumerate()
        .map(|(i, fit)| oracle_ratios(fit, &p, i + 1, t))
        .collect();
    let check = |when: &str, got: [[f64; 3]; 2]| {
        println!(
            "{when}: green {:.4?}, blue {:.4?} (oracle {:.4?} {:.4?})",
            got[0],
            got[1],
            &oracle[0][..3],
            &oracle[1][..3]
        );
        for (c, name) in ["green", "blue"].iter().enumerate() {
            for k in 0..3 {
                let rel = (got[c][k] - oracle[c][k]) / oracle[c][k];
                assert!(
                    rel.abs() < 0.01,
                    "{when} {name} k {}: {} vs oracle {}",
                    k + 1,
                    got[c][k],
                    oracle[c][k]
                );
            }
        }
    };
    // The automatic rate settles over its window (one restart at the end of the first window at most).
    for _ in 0..150 {
        let _ = pacing.render(&mut gpu, &p, &bar(None));
    }
    let before = pacing.trail(&mut gpu, &p);
    check("before the bypass", before);
    let names = gpu.stage_names();
    let saved: Vec<(&str, bool)> = names.iter().map(|&n| (n, gpu.stage_enabled(n).unwrap())).collect();
    for &n in &names {
        gpu.set_stage_enabled(n, false).unwrap();
    }
    for _ in 0..60 {
        let _ = pacing.render(&mut gpu, &p, &bar(None));
    }
    for &(n, on) in &saved {
        gpu.set_stage_enabled(n, on).unwrap();
    }
    // The first frame after the bypass restarts the history (primed to the picture): measure right after it.
    let after = pacing.trail(&mut gpu, &p);
    check("after the bypass", after);
    // The menu's switch: persistence off for 60 frames (the slot is skipped, its history freed), then on again.
    let mut off = p;
    off.phosphor_persistence = false;
    for _ in 0..60 {
        let _ = pacing.render(&mut gpu, &off, &bar(None));
    }
    let toggled = pacing.trail(&mut gpu, &p);
    check("after phosphor_persistence off then on", toggled);
    gpu.set_frame_time_ms(None);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

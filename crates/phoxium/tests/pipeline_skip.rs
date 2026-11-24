//! Identity stages are skipped (ARCHITECTURE 3.7, `Stage::is_identity`), headless, with the validation
//! layer (synchronization validation on) and zero validation messages.

mod common;

use phoxium::gpu::{Headless, Rendered, Settings, SourceFrame, TargetFormat};
use phoxium::params::{ParamValue, Params};
use phoxium::scaling::View;

/// Measurement only (runs only when `PHOXIUM_BENCH` is set): GPU time of the chain at 1920x1440 for the default parameters,
/// best of many frames (the GPU may be shared). Prints, asserts nothing.
#[test]
fn measure_default_chain_cost() {
    if std::env::var_os("PHOXIUM_BENCH").is_none() {
        return;
    }
    let Some(mut gpu) = common::gpu_or_skip("measure_default_chain_cost") else {
        return;
    };
    if gpu.timing_unavailable().is_some() {
        return;
    }
    let (src, target) = ((640u32, 480u32), (1920u32, 1440u32));
    let bgra: Vec<u8> = (0..src.0 * src.1)
        .flat_map(|i| [i as u8, (i / src.0) as u8, (i % src.0) as u8, 255])
        .collect();
    let chain = gpu.stage_names();
    let names = gpu.timing_names();
    let mut best: Vec<f64> = vec![f64::INFINITY; names.len()];
    let mut best_chain = f64::INFINITY;
    let mut best_frame = f64::INFINITY;
    for n in 0..80 {
        let frame = (n == 0).then_some(SourceFrame {
            width: src.0,
            height: src.1,
            bgra: &bgra,
        });
        gpu.render(
            frame,
            View::full(src.0, src.1),
            &Settings::default(),
            &Params::default(),
            target,
            TargetFormat::Bgra8Unorm,
        )
        .expect("render");
        if n < 20 {
            continue;
        }
        let t = gpu.last_timings().expect("timings");
        for (i, s) in t.stages.iter().enumerate() {
            if let Some(ms) = s.ms {
                best[i] = best[i].min(ms);
            }
        }
        let chain_ms: f64 = t
            .stages
            .iter()
            .filter(|s| chain.contains(&s.name))
            .filter_map(|s| s.ms)
            .sum();
        best_chain = best_chain.min(chain_ms);
        best_frame = best_frame.min(t.frame_ms);
    }
    let per_stage: Vec<String> = names
        .iter()
        .zip(&best)
        .map(|(n, ms)| {
            if ms.is_finite() {
                format!("{n} {ms:.3}")
            } else {
                format!("{n} -")
            }
        })
        .collect();
    eprintln!(
        "BENCH {}x{}: chain {best_chain:.3} ms (best frame), frame {best_frame:.3} ms | {}",
        target.0,
        target.1,
        per_stage.join(" | ")
    );
}

// ---- new tests (appended after the measurement test) ----

const SRC: (u32, u32) = (96, 64);
const TARGET: (u32, u32) = (288, 192);
/// Frames per configuration phase.
const PHASE: usize = 4;

fn set(p: &mut Params, id: &str, v: ParamValue) {
    assert!(p.set(id, v), "{id}");
}

fn picture() -> Vec<u8> {
    // Bright, varied content so that the limiter, the saturation and the breathing all show.
    (0..SRC.0 * SRC.1)
        .flat_map(|i| {
            let (x, y) = (i % SRC.0, i / SRC.0);
            [
                (140 + (x * 3 + y) % 116) as u8,
                (120 + (x + y * 5) % 136) as u8,
                (100 + (x * 7) % 156) as u8,
                255,
            ]
        })
        .collect()
}

/// The named configurations of the toggling sequence, each a function of the defaults.
fn config(name: &str) -> Params {
    let mut p = Params::default();
    let on = |p: &mut Params, id: &str| set(p, id, ParamValue::Bool(true));
    let f = |p: &mut Params, id: &str, v: f32| set(p, id, ParamValue::F32(v));
    // tau 0 and a manual trail rate: no dependence on the wall-clock frame time.
    f(&mut p, "abl_tau_s", 0.0);
    f(&mut p, "abl_full_white_ma", 8.0);
    match name {
        "default" => {}
        "persistence" => on(&mut p, "phosphor_persistence"),
        "saturation" => on(&mut p, "phosphor_saturation"),
        "both" => {
            on(&mut p, "phosphor_persistence");
            on(&mut p, "phosphor_saturation");
        }
        "limiter" => on(&mut p, "abl_enable"),
        "breathing" => f(&mut p, "abl_breathing", 0.08),
        "probe" => on(&mut p, "abl_probe"),
        "limiter+breathing" => {
            on(&mut p, "abl_enable");
            f(&mut p, "abl_breathing", 0.08);
        }
        other => panic!("unknown configuration {other}"),
    }
    p
}

fn frame<'a>(bgra: &'a [u8], new: bool) -> Option<SourceFrame<'a>> {
    new.then_some(SourceFrame {
        width: SRC.0,
        height: SRC.1,
        bgra,
    })
}

/// `n` frames of `p` on `gpu` (the picture is uploaded on the first when `upload`), the last one read back
/// in FP32 with every chain image.
fn run(gpu: &mut Headless, bgra: &[u8], p: &Params, n: usize, upload: bool) -> Rendered {
    let mut last = None;
    for i in 0..n {
        last = Some(
            gpu.render(
                frame(bgra, upload && i == 0),
                View::full(SRC.0, SRC.1),
                &Settings::default(),
                p,
                TARGET,
                TargetFormat::Rgba32F,
            )
            .expect("render"),
        );
    }
    last.expect("at least one frame")
}

fn same(a: &Rendered, b: &Rendered, what: &str) {
    assert_eq!(a.output.rgba, b.output.rgba, "{what}: output");
    assert_eq!(a.stages.len(), b.stages.len(), "{what}: routed stages");
    for ((na, ia), (nb, ib)) in a.stages.iter().zip(&b.stages) {
        assert_eq!(na, nb, "{what}: stage order");
        assert_eq!(ia.rgba, ib.rgba, "{what}: image of `{na}`");
    }
}

#[test]
fn defaults_record_fewer_passes_and_report_skipped_stages_as_absent() {
    let Some(mut gpu) = common::gpu_or_skip("defaults_record_fewer_passes") else {
        return;
    };
    let counter = gpu.validation_counter();
    let bgra = picture();
    run(&mut gpu, &bgra, &Params::default(), 3, true);
    let skipped = gpu.last_skipped_slots();
    assert_eq!(gpu.last_chain_passes(), 3, "amp, gun, screen only; skipped {skipped:?}");
    for s in ["signal", "abl", "phosphor", "optics"] {
        assert!(skipped.contains(&s), "{s} is skipped with the defaults: {skipped:?}");
    }
    for s in ["amp", "gun", "screen"] {
        assert!(!skipped.contains(&s), "{s} runs: {skipped:?}");
    }
    if gpu.timing_unavailable().is_none() {
        let t = gpu.last_timings().expect("timings").clone();
        for s in ["abl", "phosphor"] {
            assert_eq!(t.stage_ms(s), None, "{s}: a skipped stage reports no time");
        }
        for s in ["amp", "gun", "screen"] {
            assert!(t.stage_ms(s).is_some(), "{s} ran");
        }
        // `amp` and `screen` keep their flag: their stage ran, they are not bypassed copies.
        assert!(
            t.stages.iter().all(|s| !s.bypassed),
            "no resample bypass with the defaults"
        );
    }
    // Each feature on brings its pass back, and only its.
    for (cfg, back) in [
        ("persistence", vec!["phosphor"]),
        ("saturation", vec!["phosphor"]),
        ("limiter", vec!["abl"]),
        ("breathing", vec!["abl"]),
        ("probe", vec!["abl"]),
    ] {
        run(&mut gpu, &bgra, &config(cfg), 2, false);
        let skipped = gpu.last_skipped_slots();
        assert_eq!(gpu.last_chain_passes(), 4, "{cfg}: skipped {skipped:?}");
        for s in back {
            assert!(!skipped.contains(&s), "{cfg}: {s} runs: {skipped:?}");
        }
    }
    // Disabling a slot is still a skip (same domain) or a resample (domain change).
    gpu.set_stage_enabled("gun", false).unwrap();
    run(&mut gpu, &bgra, &Params::default(), 2, false);
    assert_eq!(gpu.last_chain_passes(), 2, "amp and screen: gun is off");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Switching features on and off on one pipeline gives, after `PHASE` frames in a configuration, exactly
/// the images a fresh pipeline gives after `PHASE` frames in it: no stale routing, no stale history, no
/// stale ABL state.
#[test]
fn toggling_features_matches_a_fresh_pipeline() {
    let Some(mut gpu) = common::gpu_or_skip("toggling_features_matches_a_fresh_pipeline") else {
        return;
    };
    let counter = gpu.validation_counter();
    let bgra = picture();
    let sequence = [
        "default",
        "persistence",
        "default",
        "saturation",
        "default",
        "both",
        "persistence",
        "default",
        "limiter",
        "default",
        "breathing",
        "default",
        "probe",
        "limiter+breathing",
        "breathing",
        "default",
        "both",
        "limiter",
        "default",
    ];
    let mut expected: std::collections::HashMap<&str, Rendered> = Default::default();
    for (k, name) in sequence.iter().enumerate() {
        let p = config(name);
        let got = run(&mut gpu, &bgra, &p, PHASE, k == 0);
        let want = expected.entry(name).or_insert_with(|| {
            let mut fresh = common::gpu_or_skip("fresh").expect("device");
            let r = run(&mut fresh, &bgra, &p, PHASE, true);
            drop(fresh);
            r
        });
        same(&got, want, &format!("phase {k} `{name}`"));
    }
    // The features did something: the configurations are not all the same picture.
    let d = &expected["default"].output.rgba;
    for name in ["saturation", "limiter", "breathing"] {
        assert_ne!(&expected[name].output.rgba, d, "{name} changes the picture");
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// A burst whose frames flip the parameters (frames in flight overlap) is valid under synchronization
/// validation, and ends on the stateless default picture exactly like a fresh pipeline.
#[test]
fn a_burst_with_parameter_flips_is_valid() {
    let Some(mut gpu) = common::gpu_or_skip("a_burst_with_parameter_flips_is_valid") else {
        return;
    };
    let counter = gpu.validation_counter();
    let bgra = picture();
    let names = [
        "default",
        "persistence",
        "limiter",
        "default",
        "both",
        "breathing",
        "default",
        "probe",
        "saturation",
        "limiter+breathing",
        "default",
        "persistence",
        "default",
        "default",
    ];
    let configs: Vec<Params> = names.iter().map(|n| config(n)).collect();
    let frames: Vec<(Option<SourceFrame<'_>>, View, &Params)> = configs
        .iter()
        .enumerate()
        .map(|(i, p)| (frame(&bgra, i == 0), View::full(SRC.0, SRC.1), p))
        .collect();
    let got = gpu
        .render_burst_params(&frames, &Settings::default(), TARGET, TargetFormat::Rgba32F)
        .expect("burst");
    let mut fresh = common::gpu_or_skip("fresh").expect("device");
    let want = run(&mut fresh, &bgra, &config("default"), 2, true);
    drop(fresh);
    assert_eq!(got.output.rgba, want.output.rgba, "burst ending in the defaults");
    // Second burst on the same pipeline: back and forth between skipped and run every frame.
    let flips: Vec<Params> = (0..10)
        .map(|i| config(if i % 2 == 0 { "both" } else { "limiter+breathing" }))
        .collect();
    let frames: Vec<(Option<SourceFrame<'_>>, View, &Params)> =
        flips.iter().map(|p| (None, View::full(SRC.0, SRC.1), p)).collect();
    gpu.render_burst_params(&frames, &Settings::default(), TARGET, TargetFormat::Rgba32F)
        .expect("flip burst");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// The ABL state keeps updating while the breathing is on and the limiter is off (the reduce pass is not
/// skipped), and starts cold after the stage was skipped.
#[test]
fn abl_state_updates_with_breathing_on_and_limiter_off() {
    let Some(mut gpu) = common::gpu_or_skip("abl_state_updates_with_breathing") else {
        return;
    };
    let counter = gpu.validation_counter();
    let bgra = picture();
    let mut p = config("breathing");
    set(&mut p, "abl_probe", ParamValue::Bool(true));
    set(&mut p, "abl_breathing_ref", ParamValue::F32(0.25));
    let probe = |r: &Rendered| r.stage("abl").expect("abl").pixel(0, 0).map(f64::from);
    // The state is first made non-zero, then the slot is skipped for a while (defaults), then the
    // breathing is on again: a stale state would show in the first probe.
    let warm = probe(&run(&mut gpu, &bgra, &p, 3, true));
    assert!(warm[1] > 0.1, "the state is warm: {warm:?}");
    run(&mut gpu, &bgra, &Params::default(), 3, false);
    assert!(gpu.last_skipped_slots().contains(&"abl"));
    let r1 = run(&mut gpu, &bgra, &p, 1, false);
    // The apply pass of the first frame reads the state the cold start set (one frame of latency): with the
    // breathing on, x = abl_breathing_ref (T-37 review F2), so the raster scale starts at exactly 1.
    assert_eq!(probe(&r1)[1], 0.25, "cold start after the skip");
    assert_eq!(probe(&r1)[2], 1.0, "cold start: raster scale 1");
    let r2 = run(&mut gpu, &bgra, &p, 1, false);
    let s = probe(&r2);
    assert!(s[1] > 0.1, "the filtered APL follows the picture: {s:?}");
    assert_eq!(s[0], 1.0, "limiter off: gain 1");
    let want = 1.0 + 0.08 * (s[1] - 0.25);
    assert!((s[2] - want).abs() < 1e-3, "raster scale {} vs {want}", s[2]);
    assert!((s[3] - s[1]).abs() < 1e-3, "tau 0: filtered APL = frame APL: {s:?}");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

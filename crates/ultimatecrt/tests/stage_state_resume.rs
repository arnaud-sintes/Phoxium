//! Stateful stages after the A/B bypass and after a stage is switched off and on again (live review 2026-11-20:
//! the phosphor trail stayed 0 after the bypass; the cause was the persistence stage's frame-rate estimate under
//! uneven pacing, `tests/persistence_chain.rs` `trail_survives_uneven_pacing_and_the_bypass`). Here the other
//! stages that keep something from one frame to the next, through the whole chain (`Headless::render`, the
//! window loop's path), with the bypass done as `controls::bypass_plan` does (every chain slot disabled, then the
//! saved flags restored) and with the parameter toggles of the menu:
//! - `abl` (APL state, low-pass): a slot that did not run restarts cold (state on black) and converges back to
//!   the steady state of the picture within its time constant;
//! - `wear` (burn-in map): the map is kept while the slot does not run and receives no charge then, so a run with
//!   a bypass or a toggle in the middle ends with the same map as the run without those frames;
//! - `optics` (glow and halo buffers, weights): no state between frames, the picture after the bypass or the toggle
//!   is the picture before, bit for bit.
//!
//! `composite` and the interlace field choice depend on the frame index only (no history), `screen`'s line images
//! are recomputed every frame: nothing to resume.

mod common;

use ultimatecrt::gpu::{Headless, ImageF32, Settings, SourceFrame, StageReadback, TargetFormat};
use ultimatecrt::params::Params;
use ultimatecrt::scaling::View;
use ultimatecrt::tube_state::WearHost;

const SRC: (u32, u32) = (160, 120);
const TARGET: (u32, u32) = (640, 480);
const T60: f32 = 1000.0 / 60.0;

/// A BGRA source: black with a white window of the middle half (`lit` false: all black).
fn picture(lit: bool) -> Vec<u8> {
    let mut v = vec![0u8; (SRC.0 * SRC.1 * 4) as usize];
    for row in 0..SRC.1 {
        for col in 0..SRC.0 {
            let i = ((row * SRC.0 + col) * 4) as usize;
            let on = lit && col >= SRC.0 / 4 && col < 3 * SRC.0 / 4 && row >= SRC.1 / 4 && row < 3 * SRC.1 / 4;
            v[i..i + 4].copy_from_slice(&if on { [255; 4] } else { [0, 0, 0, 255] });
        }
    }
    v
}

/// One frame of the whole chain: the last chain image.
fn render(gpu: &mut Headless, p: &Params, data: &[u8]) -> ImageF32 {
    let r = gpu
        .render(
            Some(SourceFrame {
                width: SRC.0,
                height: SRC.1,
                bgra: data,
            }),
            View::full(SRC.0, SRC.1),
            &Settings::default(),
            p,
            TARGET,
            TargetFormat::Rgba32F,
        )
        .expect("render");
    r.stages.last().expect("chain image").1.clone()
}

fn sum(img: &ImageF32) -> f64 {
    img.rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|t| f64::from(t[0]) + f64::from(t[1]) + f64::from(t[2]))
        .sum()
}

/// `frames` frames with the A/B bypass on (every chain slot off), then the saved flags back.
fn bypassed(gpu: &mut Headless, p: &Params, data: &[u8], frames: usize) {
    let names = gpu.stage_names();
    let saved: Vec<(&str, bool)> = names.iter().map(|&n| (n, gpu.stage_enabled(n).unwrap())).collect();
    for &n in &names {
        gpu.set_stage_enabled(n, false).unwrap();
    }
    for _ in 0..frames {
        let _ = render(gpu, p, data);
    }
    for &(n, on) in &saved {
        gpu.set_stage_enabled(n, on).unwrap();
    }
}

/// `abl`: the limiter on a bright window settles (120 frames at 60 Hz, 12 time constants); after the bypass and
/// after `abl_enable` off then on, the first frame is a cold start (the state on black: no limiting yet, at least as
/// bright as the settled frame) and 120 frames later the picture is the settled one again within 0.1 %.
#[test]
fn abl_restarts_cold_and_settles_again() {
    let Some(mut gpu) = common::gpu_or_skip("abl after bypass") else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.set_read_stages(StageReadback::Last);
    gpu.set_frame_time_ms(Some(T60));
    let p = Params {
        abl_enable: true,
        ..Params::default()
    };
    // A full white field: the limiter acts.
    let lit = vec![255u8; (SRC.0 * SRC.1 * 4) as usize];
    let mut settled = 0.0;
    for _ in 0..120 {
        settled = sum(&render(&mut gpu, &p, &lit));
    }
    let mut off = p;
    off.abl_enable = false;
    let unlimited = sum(&render(&mut gpu, &off, &lit));
    assert!(settled < 0.98 * unlimited, "the limiter acts: {settled} vs {unlimited}");
    for case in ["bypass", "abl_enable off then on"] {
        if case == "bypass" {
            bypassed(&mut gpu, &p, &lit, 30);
        } else {
            for _ in 0..30 {
                let _ = render(&mut gpu, &off, &lit);
            }
        }
        let first = sum(&render(&mut gpu, &p, &lit));
        let mut last = 0.0;
        for _ in 0..120 {
            last = sum(&render(&mut gpu, &p, &lit));
        }
        println!("{case}: settled {settled:.1}, first frame after {first:.1}, 120 frames after {last:.1}");
        assert!(first >= settled, "{case}: cold start {first} vs settled {settled}");
        assert!((last - settled).abs() < 1e-3 * settled, "{case}: {last} vs {settled}");
    }
    gpu.set_frame_time_ms(None);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// The burn-in map (hours per texel and colour) after 20 frames of the window with burn-in on (accelerated), then
/// `middle` (the frames in between: none, a bypass, or burn-in switched off), then 20 more frames of the window.
fn burn_map(gpu: &mut Headless, middle: &str) -> Vec<f32> {
    let p = Params {
        burnin_enable: true,
        burnin_acceleration: 1.0e6,
        ..Params::default()
    };
    let lit = picture(true);
    gpu.reset_wear();
    for _ in 0..20 {
        let _ = render(gpu, &p, &lit);
    }
    match middle {
        "bypass" => bypassed(gpu, &p, &lit, 20),
        "burn-in off" => {
            let mut off = p;
            off.burnin_enable = false;
            for _ in 0..20 {
                let _ = render(gpu, &off, &lit);
            }
        }
        _ => {}
    }
    for _ in 0..20 {
        let _ = render(gpu, &p, &lit);
    }
    gpu.read_wear_map().expect("read").expect("map").q
}

/// `wear`: the burn-in map is kept while the slot does not run and receives no charge then: a run with 20 bypassed
/// frames, or 20 frames with `burnin_enable` off, in the middle ends with the same map as the run without them,
/// bit for bit (the same accumulations in the same order), and the map did wear.
#[test]
fn burn_in_map_is_kept_and_not_charged_while_off() {
    let Some(mut gpu) = common::gpu_or_skip("burn-in after bypass") else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.set_read_stages(StageReadback::Last);
    gpu.set_frame_time_ms(Some(T60));
    let reference = burn_map(&mut gpu, "none");
    let worn = reference.iter().cloned().fold(0.0_f32, f32::max);
    // 40 frames of 16.7 ms at x 1,000,000: 185 h of the window's light.
    println!("burn-in reference: most worn texel {worn:.1} h");
    assert!(worn > 50.0, "{worn}");
    assert!(reference.contains(&0.0), "the border did not wear");
    for middle in ["bypass", "burn-in off"] {
        assert_eq!(burn_map(&mut gpu, middle), reference, "{middle} in the middle");
    }
    gpu.set_frame_time_ms(None);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}
/// `optics` (glow on, vignette, halation at the defaults of the slot): no state between frames: after the bypass
/// and after `optics_enable` off then on, the picture equals the one before, bit for bit.
#[test]
fn optics_picture_is_unchanged_after_bypass_and_toggle() {
    let Some(mut gpu) = common::gpu_or_skip("optics after bypass") else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.set_read_stages(StageReadback::Last);
    gpu.set_frame_time_ms(Some(T60));
    let p = Params {
        optics_enable: true,
        optics_bloom_weight: 0.3,
        ..Params::default()
    };
    let lit = picture(true);
    let before = render(&mut gpu, &p, &lit);
    let mut off = p;
    off.optics_enable = false;
    assert_ne!(
        render(&mut gpu, &off, &lit).rgba,
        before.rgba,
        "optics changes the picture"
    );
    bypassed(&mut gpu, &p, &lit, 5);
    assert_eq!(render(&mut gpu, &p, &lit).rgba, before.rgba, "after the bypass");
    for _ in 0..5 {
        let _ = render(&mut gpu, &off, &lit);
    }
    assert_eq!(
        render(&mut gpu, &p, &lit).rgba,
        before.rgba,
        "after optics_enable off then on"
    );
    gpu.set_frame_time_ms(None);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

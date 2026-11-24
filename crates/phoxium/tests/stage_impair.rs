//! GPU tests of the analogue impairments (slot `impair`, `shaders/impair.comp`), headless, validation on and zero
//! validation messages, one shared context. Each case injects an amp-domain image after `amp`, runs only `impair`,
//! reads back its image and compares it with the f64 oracle `crt_model::impair` (whose own unit tests carry the
//! hand derivations: the ghost copy, the hum phase, the jitter centre shift, the tracking band, the noise and snow
//! statistics).
//!
//! Budget: the slot stores FP16 (round to nearest, at most 2.4e-4 at 0.5 to 1, 4.9e-4 from 1 to 2) on top of
//! the f32 shader arithmetic; max abs 1e-3, mean abs 2e-4 per channel. The noise, the jitter and the snow come
//! from the shared integer hash, so the GPU and the oracle draw the same numbers; a snow cell is hit or not on both.

mod common;

use crt_model::impair::Impair;
use phoxium::gpu::{FrameConstants, Headless, ImageF32, Settings, StageInput, StageRun, TargetFormat, AMP_K};
use phoxium::params::Params;

const MAX_ERR: f64 = 1e-3;
const MEAN_ERR: f64 = 2e-4;
const K: usize = AMP_K as usize;

/// The amp-domain rows of `w` x `h` source pixels: blocks of a small palette (hard edges), each pixel held over K
/// samples, with a soft ramp on some lines so that interpolation errors would show.
fn amp_rows(w: usize, h: usize) -> Vec<Vec<[f64; 3]>> {
    const PALETTE: [[f64; 3]; 8] = [
        [0.0, 0.0, 0.0],
        [1.0, 1.0, 1.0],
        [0.85, 0.15, 0.1],
        [0.1, 0.7, 0.2],
        [0.15, 0.25, 0.9],
        [0.95, 0.85, 0.2],
        [0.5, 0.5, 0.5],
        [0.2, 0.75, 0.8],
    ];
    let mut s: u32 = 4242;
    let blocks: Vec<usize> = (0..(w / 4 + 1) * (h / 2 + 1))
        .map(|_| {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (s >> 24) as usize % PALETTE.len()
        })
        .collect();
    (0..h)
        .map(|y| {
            (0..w * K)
                .map(|x| {
                    let c = PALETTE[blocks[(y / 2) * (w / 4 + 1) + x / K / 4]];
                    if y % 5 == 0 {
                        let r = (x as f64) / (w * K) as f64;
                        [c[0] * r, c[1] * r, c[2] * r]
                    } else {
                        c
                    }
                })
                .collect()
        })
        .collect()
}

fn image(rows: &[Vec<[f64; 3]>]) -> ImageF32 {
    let mut rgba = Vec::new();
    for row in rows {
        for p in row {
            rgba.extend_from_slice(&[p[0] as f32, p[1] as f32, p[2] as f32, 1.0]);
        }
    }
    ImageF32 {
        width: rows[0].len() as u32,
        height: rows.len() as u32,
        rgba,
    }
}

/// The oracle for `p`, written independently of the stage's mapping.
fn oracle_of(p: &Params) -> Impair {
    Impair {
        field_rate_hz: f64::from(p.impair_field_rate_hz),
        hum_amount: f64::from(p.impair_hum_amount),
        hum_hz: f64::from(p.impair_hum_hz),
        jitter_px: f64::from(p.impair_jitter_px),
        crawl_px: f64::from(p.impair_crawl_px),
        crawl_period_lines: f64::from(p.impair_crawl_period_lines),
        crawl_speed_hz: f64::from(p.impair_crawl_speed_hz),
        ghost_gain: f64::from(p.impair_ghost_gain),
        ghost_delay_px: f64::from(p.impair_ghost_delay_px),
        noise: f64::from(p.impair_noise),
        snow: f64::from(p.impair_snow),
        snow_length: p.impair_snow_length as u32,
        tracking_px: f64::from(p.impair_tracking_px),
        tracking_lines: f64::from(p.impair_tracking_lines),
        tracking_position: f64::from(p.impair_tracking_position),
        tracking_roll_hz: f64::from(p.impair_tracking_roll_hz),
        tracking_noise: f64::from(p.impair_tracking_noise),
    }
}

/// Every amount at 0, the module on: the base of the cases.
fn quiet() -> Params {
    Params {
        impair_enable: true,
        impair_hum_amount: 0.0,
        impair_jitter_px: 0.0,
        impair_crawl_px: 0.0,
        impair_ghost_gain: 0.0,
        impair_noise: 0.0,
        impair_snow: 0.0,
        impair_tracking_px: 0.0,
        ..Params::default()
    }
}

/// Runs only `impair` on `img` (injected after `amp`, native size `w` x `h`) at frame index `frame`.
fn run(gpu: &mut Headless, img: &ImageF32, size: (u32, u32), p: &Params, frame: u32) -> ImageF32 {
    // The source frame `frame` at the field rate, in real time (the plans take their time from `elapsed_ms`).
    let fc = FrameConstants::new(size, size, false).at_source_frame(frame, f64::from(p.impair_field_rate_hz));
    let settings = Settings::identity();
    let r = gpu
        .render_stages(&StageRun {
            input: StageInput::Image {
                after: "amp",
                image: img,
            },
            frame: fc,
            settings: &settings,
            params: p,
            format: TargetFormat::Rgba32F,
        })
        .expect("render_stages");
    r.stage("impair").expect("impair image").clone()
}

fn compare(gpu: &mut Headless, rows: &[Vec<[f64; 3]>], size: (u32, u32), p: &Params, frame: u32, what: &str) {
    let img = image(rows);
    let out = run(gpu, &img, size, p, frame);
    assert_eq!((out.width, out.height), (img.width, img.height), "{what}");
    let want = oracle_of(p).frame(rows, frame, K);
    let (mut max, mut sum, mut n) = (0.0_f64, 0.0, 0);
    let mut worst = (0, 0);
    for (y, row) in want.iter().enumerate() {
        for (x, w) in row.iter().enumerate() {
            let g = out.pixel(x as u32, y as u32);
            for c in 0..3 {
                let e = (f64::from(g[c]) - w[c]).abs();
                if e > max {
                    max = e;
                    worst = (x, y);
                }
                sum += e;
                n += 1;
            }
            assert_eq!(g[3], 1.0, "{what}: alpha");
        }
    }
    let mean = sum / n as f64;
    println!("{what}: max {max:.2e} at {worst:?}, mean {mean:.2e}");
    assert!(max <= MAX_ERR, "{what}: max error {max} at {worst:?}");
    assert!(mean <= MEAN_ERR, "{what}: mean error {mean}");
}

#[test]
fn impair_slot_matches_the_oracle() {
    let Some(mut gpu) = common::gpu_or_skip("impair_slot_matches_the_oracle") else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.enable_only(&["impair"]).unwrap();
    let (w, h) = (64usize, 48usize);
    let size = (w as u32, h as u32);
    let rows = amp_rows(w, h);
    let img = image(&rows);

    // Off (the default), or on with every amount at 0: skipped, the image passes bit for bit.
    let off = run(&mut gpu, &img, size, &Params::default(), 0);
    assert_eq!(off.rgba, img.rgba, "off: bit-exact identity");
    assert!(gpu.last_skipped_slots().contains(&"impair"), "off: the slot is skipped");
    let zero = run(&mut gpu, &img, size, &quiet(), 0);
    assert_eq!(zero.rgba, img.rgba, "zero amounts: bit-exact identity");
    assert!(gpu.last_skipped_slots().contains(&"impair"), "zero amounts: skipped");
    let defaults = run(
        &mut gpu,
        &img,
        size,
        &Params {
            impair_enable: true,
            ..Params::default()
        },
        0,
    );
    assert!(!gpu.last_skipped_slots().contains(&"impair"), "the default amounts run");
    assert_ne!(defaults.rgba, img.rgba, "the default amounts change the picture");

    let q = quiet();
    let cases: Vec<(&str, Params, u32)> = vec![
        (
            "defaults, frame 0",
            Params {
                impair_enable: true,
                ..Params::default()
            },
            0,
        ),
        (
            "defaults, frame 7",
            Params {
                impair_enable: true,
                ..Params::default()
            },
            7,
        ),
        (
            "hum 50 Hz on 60",
            Params {
                impair_hum_amount: 0.2,
                ..q
            },
            5,
        ),
        (
            "hum 60 Hz, 3 bars (180 Hz)",
            Params {
                impair_hum_amount: 0.15,
                impair_hum_hz: 180.0,
                ..q
            },
            2,
        ),
        (
            "jitter 1.7 px",
            Params {
                impair_jitter_px: 1.7,
                ..q
            },
            3,
        ),
        (
            "crawl 2.5 px, period 30 lines, 1 Hz",
            Params {
                impair_crawl_px: 2.5,
                impair_crawl_period_lines: 30.0,
                impair_crawl_speed_hz: 1.0,
                ..q
            },
            11,
        ),
        (
            "ghost +0.3 at 7.5 px",
            Params {
                impair_ghost_gain: 0.3,
                impair_ghost_delay_px: 7.5,
                ..q
            },
            0,
        ),
        (
            "ghost -0.4 at 3 px",
            Params {
                impair_ghost_gain: -0.4,
                impair_ghost_delay_px: 3.0,
                ..q
            },
            0,
        ),
        (
            "noise 0.08",
            Params {
                impair_noise: 0.08,
                ..q
            },
            4,
        ),
        (
            "snow 0.25, streaks of 5",
            Params {
                impair_snow: 0.25,
                impair_snow_length: 5,
                ..q
            },
            6,
        ),
        (
            "tracking 9 px, 14 lines, rolling",
            Params {
                impair_tracking_px: 9.0,
                impair_tracking_lines: 14.0,
                impair_tracking_position: 0.6,
                impair_tracking_roll_hz: 0.5,
                impair_tracking_noise: 0.3,
                ..q
            },
            20,
        ),
        (
            "everything together",
            Params {
                impair_hum_amount: 0.1,
                impair_jitter_px: 0.8,
                impair_crawl_px: 1.2,
                impair_ghost_gain: 0.2,
                impair_noise: 0.04,
                impair_snow: 0.05,
                impair_tracking_px: 5.0,
                impair_field_rate_hz: 50.0,
                ..q
            },
            13,
        ),
    ];
    for (name, p, frame) in cases {
        compare(&mut gpu, &rows, size, &p, frame, name);
    }
    common::assert_no_validation_messages(&counter);
}

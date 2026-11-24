//! GPU tests of the TV controls (slot `tv`, `shaders/tv.comp`), headless, validation on and zero validation messages,
//! one shared context. Each case injects an amp-domain image after `amp`, runs only `tv`, reads back its image and
//! compares it with the f64 oracle `crt_model::tv` (whose own unit tests carry the hand derivations: the levels, the
//! colour differences, the tint turn, the sharpness overshoot, the flicker's beat).
//!
//! Budget: the slot stores FP16 (round to nearest, at most 2.4e-4 at 0.5 to 1, 4.9e-4 from 1 to 2) on top of the f32
//! shader arithmetic; max abs 1e-3, mean abs 2e-4 per channel.

mod common;

use crt_model::tv::Tv;
use phoxium::gpu::{FrameConstants, Headless, ImageF32, Settings, StageInput, StageRun, TargetFormat, AMP_K};
use phoxium::params::Params;

const MAX_ERR: f64 = 1e-3;
const MEAN_ERR: f64 = 2e-4;
const K: usize = AMP_K as usize;

/// The amp-domain rows of `w` x `h` source pixels: blocks of a small palette (hard edges), each pixel held over K
/// samples, with a soft ramp on some lines so that filter errors would show.
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
    let mut s: u32 = 777;
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
fn oracle_of(p: &Params) -> Tv {
    Tv {
        sharpness: f64::from(p.tv_sharpness),
        sharpness_px: f64::from(p.tv_sharpness_px),
        tint_deg: f64::from(p.tv_tint_deg),
        colour: f64::from(p.tv_colour),
        black: f64::from(p.tv_black_level),
        white: f64::from(p.tv_white_level),
        flicker_amount: f64::from(p.tv_flicker_amount),
        flicker_hz: f64::from(p.tv_flicker_hz),
        frame_rate_hz: f64::from(p.tv_frame_rate_hz),
    }
}

/// Runs only `tv` on `img` (injected after `amp`) at frame index `frame`.
fn run(gpu: &mut Headless, img: &ImageF32, size: (u32, u32), p: &Params, frame: u32) -> ImageF32 {
    let mut fc = FrameConstants::new(size, size, false);
    fc.frame_index = frame;
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
    r.stage("tv").expect("tv image").clone()
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
fn tv_slot_matches_the_oracle() {
    let Some(mut gpu) = common::gpu_or_skip("tv_slot_matches_the_oracle") else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.enable_only(&["tv"]).unwrap();
    let (w, h) = (64usize, 24usize);
    let size = (w as u32, h as u32);
    let rows = amp_rows(w, h);
    let img = image(&rows);

    // Every knob at its neutral value (the default): skipped, the image passes bit for bit.
    let off = run(&mut gpu, &img, size, &Params::default(), 0);
    assert_eq!(off.rgba, img.rgba, "neutral: bit-exact identity");
    assert!(gpu.last_skipped_slots().contains(&"tv"), "neutral: the slot is skipped");

    let cases: Vec<(&str, Params, u32)> = vec![
        (
            "sharpness +0.8, 1 px",
            Params {
                tv_sharpness: 0.8,
                ..Params::default()
            },
            0,
        ),
        (
            "sharpness +1, 2 px",
            Params {
                tv_sharpness: 1.0,
                tv_sharpness_px: 2.0,
                ..Params::default()
            },
            0,
        ),
        (
            "sharpness -1 (the low-pass), 1.5 px",
            Params {
                tv_sharpness: -1.0,
                tv_sharpness_px: 1.5,
                ..Params::default()
            },
            0,
        ),
        (
            "tint 25 degrees",
            Params {
                tv_tint_deg: 25.0,
                ..Params::default()
            },
            0,
        ),
        (
            "tint -40 degrees, colour 1.4",
            Params {
                tv_tint_deg: -40.0,
                tv_colour: 1.4,
                ..Params::default()
            },
            0,
        ),
        (
            "colour 0.3",
            Params {
                tv_colour: 0.3,
                ..Params::default()
            },
            0,
        ),
        (
            "black +0.08, white 0.85",
            Params {
                tv_black_level: 0.08,
                tv_white_level: 0.85,
                ..Params::default()
            },
            0,
        ),
        (
            "black -0.1, white 1.3 (crushes and overdrives)",
            Params {
                tv_black_level: -0.1,
                tv_white_level: 1.3,
                ..Params::default()
            },
            0,
        ),
        (
            "flicker 0.3 at 10 Hz, frame 3 (the trough)",
            Params {
                tv_flicker_amount: 0.3,
                ..Params::default()
            },
            3,
        ),
        (
            "flicker 0.5 at 7.5 Hz, frame 11",
            Params {
                tv_flicker_amount: 0.5,
                tv_flicker_hz: 7.5,
                ..Params::default()
            },
            11,
        ),
        (
            "everything together",
            Params {
                tv_sharpness: 0.5,
                tv_tint_deg: 12.0,
                tv_colour: 1.2,
                tv_black_level: 0.03,
                tv_white_level: 0.95,
                tv_flicker_amount: 0.2,
                ..Params::default()
            },
            4,
        ),
    ];
    for (name, p, frame) in cases {
        compare(&mut gpu, &rows, size, &p, frame, name);
    }
    common::assert_no_validation_messages(&counter);
}

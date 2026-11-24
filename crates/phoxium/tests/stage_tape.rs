//! GPU tests of the tape module (slot `tape`, `shaders/tape.comp`), headless, validation on and zero validation
//! messages, one shared context. Each case injects an amp-domain image after `amp`, runs only `tape`, reads back its
//! image and compares it with the f64 oracle `crt_model::tape` (whose own unit tests carry the hand derivations: the
//! triangular blur, the lagging chroma, the roll and its bar, the head band's ramp, the dropout statistics).
//!
//! Budget: the slot stores FP16 on top of the f32 shader arithmetic; max abs 1e-3, mean abs 2e-4 per channel. The
//! head noise, the tear and the dropouts come from the shared integer hash, so the GPU and the oracle draw the same
//! numbers; a dropout covers the same samples on both (the streaks' ends are not at a sample boundary, so no case
//! sits on one).

mod common;

use crt_model::tape::Tape;
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
    let mut s: u32 = 31337;
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
fn oracle_of(p: &Params) -> Tape {
    Tape {
        field_rate_hz: f64::from(p.impair_field_rate_hz),
        luma_blur_px: f64::from(p.tape_luma_blur_px),
        chroma_blur_px: f64::from(p.tape_chroma_blur_px),
        chroma_shift_px: f64::from(p.tape_chroma_shift_px),
        head_lines: f64::from(p.tape_head_lines),
        head_shift_px: f64::from(p.tape_head_shift_px),
        head_noise: f64::from(p.tape_head_noise),
        dropout: f64::from(p.tape_dropout),
        dropout_length_px: f64::from(p.tape_dropout_length_px),
        roll_hz: f64::from(p.tape_roll_hz),
        roll_bar_lines: f64::from(p.tape_roll_bar_lines),
    }
}

/// Every amount at 0, the module on: the base of the cases.
fn quiet() -> Params {
    Params {
        tape_enable: true,
        tape_luma_blur_px: 0.0,
        tape_chroma_blur_px: 0.0,
        tape_chroma_shift_px: 0.0,
        tape_head_lines: 0.0,
        tape_dropout: 0.0,
        tape_roll_hz: 0.0,
        ..Params::default()
    }
}

/// Runs only `tape` on `img` (injected after `amp`) at frame index `frame`.
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
    r.stage("tape").expect("tape image").clone()
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
    if max > MAX_ERR {
        for (y, row) in want.iter().enumerate() {
            let bad = row
                .iter()
                .enumerate()
                .filter(|(x, w)| (0..3).any(|c| (f64::from(out.pixel(*x as u32, y as u32)[c]) - w[c]).abs() > MAX_ERR))
                .count();
            if bad > 0 {
                println!(
                    "  line {y}: {bad} samples differ; first sample gpu {:?} oracle {:?}",
                    out.pixel(0, y as u32),
                    row[0]
                );
            }
        }
    }
    assert!(max <= MAX_ERR, "{what}: max error {max} at {worst:?}");
    assert!(mean <= MEAN_ERR, "{what}: mean error {mean}");
}

#[test]
fn tape_slot_matches_the_oracle() {
    let Some(mut gpu) = common::gpu_or_skip("tape_slot_matches_the_oracle") else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.enable_only(&["tape"]).unwrap();
    let (w, h) = (64usize, 48usize);
    let size = (w as u32, h as u32);
    let rows = amp_rows(w, h);
    let img = image(&rows);

    // Off (the default), or on with every amount at 0: skipped, the image passes bit for bit.
    let off = run(&mut gpu, &img, size, &Params::default(), 0);
    assert_eq!(off.rgba, img.rgba, "off: bit-exact identity");
    assert!(gpu.last_skipped_slots().contains(&"tape"), "off: the slot is skipped");
    let zero = run(&mut gpu, &img, size, &quiet(), 0);
    assert_eq!(zero.rgba, img.rgba, "zero amounts: bit-exact identity");
    assert!(gpu.last_skipped_slots().contains(&"tape"), "zero amounts: skipped");
    let defaults = run(
        &mut gpu,
        &img,
        size,
        &Params {
            tape_enable: true,
            ..Params::default()
        },
        0,
    );
    assert!(!gpu.last_skipped_slots().contains(&"tape"), "the default amounts run");
    assert_ne!(defaults.rgba, img.rgba, "the default amounts change the picture");

    let q = quiet();
    let cases: Vec<(&str, Params, u32)> = vec![
        (
            "defaults, frame 0",
            Params {
                tape_enable: true,
                ..Params::default()
            },
            0,
        ),
        (
            "defaults, frame 9",
            Params {
                tape_enable: true,
                ..Params::default()
            },
            9,
        ),
        (
            "luma blur 0.75 px",
            Params {
                tape_luma_blur_px: 0.75,
                ..q
            },
            0,
        ),
        (
            "chroma blur 3 px",
            Params {
                tape_chroma_blur_px: 3.0,
                ..q
            },
            0,
        ),
        (
            "chroma lag 1.3 px",
            Params {
                tape_chroma_shift_px: 1.3,
                ..q
            },
            0,
        ),
        (
            "chroma lead -0.6 px with blur 1 px",
            Params {
                tape_chroma_shift_px: -0.6,
                tape_chroma_blur_px: 1.0,
                ..q
            },
            0,
        ),
        (
            "head band 8 lines, tear 9 px, noise 0.2",
            Params {
                tape_head_lines: 8.0,
                tape_head_shift_px: 9.0,
                tape_head_noise: 0.2,
                ..q
            },
            5,
        ),
        (
            "dropouts 0.25, 10 px",
            Params {
                tape_dropout: 0.25,
                tape_dropout_length_px: 10.0,
                ..q
            },
            2,
        ),
        (
            "roll 0.5 Hz at frame 30 (offset 12 of 48), bar 5",
            Params {
                tape_roll_hz: 0.5,
                tape_roll_bar_lines: 5.0,
                ..q
            },
            30,
        ),
        (
            "everything together, 50 Hz time base",
            Params {
                tape_luma_blur_px: 0.5,
                tape_chroma_blur_px: 2.0,
                tape_chroma_shift_px: 0.75,
                tape_head_lines: 6.0,
                tape_head_shift_px: 6.0,
                tape_head_noise: 0.12,
                tape_dropout: 0.1,
                tape_dropout_length_px: 12.0,
                tape_roll_hz: 0.3,
                tape_roll_bar_lines: 4.0,
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

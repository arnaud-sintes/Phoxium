//! GPU tests of the composite video module (T-31, slot `signal`, `shaders/composite.comp`), headless,
//! validation on and zero validation messages, one shared context. Each case injects a native image
//! after `decode`, runs only `signal`, reads back its image and compares it with the f64 oracle
//! `crt_model::composite` (whose own unit tests carry the hand derivations: flat colour and grey round
//! trips, chroma bleed width 0.3396 f_px / B, cross-colour hue of luma stripes at f_sc, phase advance
//! per line and frame, hue and saturation, ACC).
//!
//! Budget: the slot stores FP16 (round to nearest, at most 2.4e-4 at 0.5 to 1) on top of the f32 shader
//! arithmetic (phases reduced with `fract` per sample, kernels summed in f32); max abs 5e-4 of full drive
//! per channel, mean abs 1.5e-4 (measured on the RTX A2000 Laptop: max 2.56e-4, the FP16 step, mean at most
//! 1.02e-4, every case). Runs in about 2 s.

mod common;

use crt_model::composite::{
    rgb_to_yuv, Composite, Connection, Separation, Standard, NTSC_LINE_RATE_KHZ, NTSC_SUBCARRIER_MHZ,
    PAL_LINE_RATE_KHZ, PAL_SUBCARRIER_MHZ,
};
use phoxium::gpu::{FrameConstants, Headless, ImageF32, Settings, StageInput, StageRun, TargetFormat};
use phoxium::params::{self, Params};

const MAX_ERR: f64 = 5e-4;
const MEAN_ERR: f64 = 1.5e-4;

/// A native image of `rows` (R', G', B'), alpha 1.
fn image(rows: &[Vec<[f64; 3]>]) -> ImageF32 {
    let width = rows[0].len() as u32;
    let mut rgba = Vec::new();
    for row in rows {
        for p in row {
            rgba.extend_from_slice(&[p[0] as f32, p[1] as f32, p[2] as f32, 1.0]);
        }
    }
    ImageF32 {
        width,
        height: rows.len() as u32,
        rgba,
    }
}

/// Pixel art: 4 x 2 blocks of a 12-colour palette from a fixed LCG (hard colour and luma edges).
fn pixel_art(w: usize, h: usize) -> Vec<Vec<[f64; 3]>> {
    const PALETTE: [[f64; 3]; 12] = [
        [0.0, 0.0, 0.0],
        [1.0, 1.0, 1.0],
        [0.85, 0.15, 0.1],
        [0.1, 0.7, 0.2],
        [0.15, 0.25, 0.9],
        [0.95, 0.85, 0.2],
        [0.6, 0.2, 0.7],
        [0.2, 0.75, 0.8],
        [0.5, 0.5, 0.5],
        [0.95, 0.6, 0.7],
        [0.35, 0.2, 0.05],
        [0.25, 0.25, 0.3],
    ];
    let mut s: u32 = 12345;
    let blocks: Vec<usize> = (0..(w / 4 + 1) * (h / 2 + 1))
        .map(|_| {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (s >> 24) as usize % PALETTE.len()
        })
        .collect();
    (0..h)
        .map(|y| (0..w).map(|x| PALETTE[blocks[(y / 2) * (w / 4 + 1) + x / 4]]).collect())
        .collect()
}

/// 1-px black/white columns.
fn stripes(w: usize, h: usize) -> Vec<Vec<[f64; 3]>> {
    vec![(0..w).map(|j| [if j % 2 == 0 { 1.0 } else { 0.0 }; 3]).collect(); h]
}

/// The oracle for `p`, written independently of the stage's mapping (0 = the standard's subcarrier
/// and line rate; the amplifier's pixel clock).
fn oracle_of(p: &Params) -> Composite {
    let pal = p.composite_standard == params::COMPOSITE_STANDARD_PAL;
    let base = if pal {
        Composite::pal(f64::from(p.amp_pixel_clock_mhz))
    } else {
        Composite::ntsc(f64::from(p.amp_pixel_clock_mhz))
    };
    Composite {
        standard: if pal { Standard::Pal } else { Standard::Ntsc },
        connection: if p.composite_connection == params::COMPOSITE_CONNECTION_SVIDEO {
            Connection::SVideo
        } else {
            Connection::Composite
        },
        separation: if p.composite_separation == params::COMPOSITE_SEPARATION_COMB {
            Separation::Comb
        } else {
            Separation::Notch
        },
        subcarrier_mhz: if p.composite_subcarrier_mhz > 0.0 {
            f64::from(p.composite_subcarrier_mhz)
        } else if pal {
            PAL_SUBCARRIER_MHZ
        } else {
            NTSC_SUBCARRIER_MHZ
        },
        line_rate_khz: if p.composite_line_rate_khz > 0.0 {
            f64::from(p.composite_line_rate_khz)
        } else if pal {
            PAL_LINE_RATE_KHZ
        } else {
            NTSC_LINE_RATE_KHZ
        },
        frame_advance_deg: f64::from(p.composite_frame_advance_deg),
        phase_deg: f64::from(p.composite_phase_deg),
        i_bandwidth_mhz: f64::from(p.composite_i_bandwidth_mhz),
        q_bandwidth_mhz: f64::from(p.composite_q_bandwidth_mhz),
        channel_bandwidth_mhz: f64::from(p.composite_bandwidth_mhz),
        hue_deg: f64::from(p.composite_hue_deg),
        saturation: f64::from(p.composite_saturation),
        hue_slope_deg: f64::from(p.composite_hue_slope_deg),
        hue_jitter_deg: f64::from(p.composite_hue_jitter_deg),
        decoder_axis_error_deg: f64::from(p.composite_decoder_axis_error_deg),
        decoder_v_gain: f64::from(p.composite_decoder_v_gain),
        decoder_green_ry: f64::from(p.composite_decoder_green_ry),
        decoder_green_by: f64::from(p.composite_decoder_green_by),
        ..base
    }
}

/// Runs only `signal` on `img` (injected after `decode`) at frame index `frame`; its output image.
fn run(gpu: &mut Headless, img: &ImageF32, p: &Params, frame: u32) -> ImageF32 {
    let mut fc = FrameConstants::new((img.width, img.height), (img.width, img.height), false);
    fc.frame_index = frame;
    let settings = Settings::identity();
    let r = gpu
        .render_stages(&StageRun {
            input: StageInput::Image {
                after: "decode",
                image: img,
            },
            frame: fc,
            settings: &settings,
            params: p,
            format: TargetFormat::Rgba32F,
        })
        .expect("render_stages");
    r.stage("signal").expect("signal image").clone()
}

/// GPU against the oracle; returns (max, mean) abs error.
fn compare(gpu: &mut Headless, rows: &[Vec<[f64; 3]>], p: &Params, frame: u32, what: &str) -> ImageF32 {
    let img = image(rows);
    let out = run(gpu, &img, p, frame);
    assert_eq!((out.width, out.height), (img.width, img.height), "{what}");
    let want = oracle_of(p).decode_frame(rows, frame);
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
    out
}

fn on(p: Params) -> Params {
    Params {
        composite_enable: true,
        ..p
    }
}

#[test]
fn composite_slot_matches_the_oracle() {
    let test = "stage_composite";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.enable_only(&["signal"]).unwrap();
    let art = pixel_art(256, 64);

    // Off (the default): skipped, the image passes bit for bit.
    let img = image(&art);
    let off = run(&mut gpu, &img, &Params::default(), 0);
    assert_eq!(off.rgba, img.rgba, "off: bit-exact identity");
    assert!(gpu.last_skipped_slots().contains(&"signal"), "off: the slot is skipped");
    let enabled = run(&mut gpu, &img, &on(Params::default()), 0);
    assert!(!gpu.last_skipped_slots().contains(&"signal"), "on: the slot runs");
    assert_ne!(enabled.rgba, img.rgba, "on: the picture changes");

    let ntsc = on(Params::default());
    let cases: Vec<(&str, Params, u32)> = vec![
        ("NTSC notch, frame 0", ntsc, 0),
        ("NTSC notch, frame 1", ntsc, 1),
        (
            "NTSC comb",
            Params {
                composite_separation: params::COMPOSITE_SEPARATION_COMB,
                ..ntsc
            },
            3,
        ),
        (
            "NTSC S-Video",
            Params {
                composite_connection: params::COMPOSITE_CONNECTION_SVIDEO,
                ..ntsc
            },
            0,
        ),
        (
            "NES timing, still frame, phase 45, tint 15, colour 1.3",
            Params {
                amp_pixel_clock_mhz: 5.369318,
                composite_line_rate_khz: 5369.318 / 341.0,
                composite_frame_advance_deg: 0.0,
                composite_phase_deg: 45.0,
                composite_hue_deg: 15.0,
                composite_saturation: 1.3,
                ..ntsc
            },
            7,
        ),
        (
            "PAL delay line, tint 20",
            Params {
                composite_standard: params::COMPOSITE_STANDARD_PAL,
                composite_separation: params::COMPOSITE_SEPARATION_COMB,
                composite_hue_deg: 20.0,
                amp_pixel_clock_mhz: 7.375,
                ..ntsc
            },
            2,
        ),
        (
            "PAL-S, no channel filter",
            Params {
                composite_standard: params::COMPOSITE_STANDARD_PAL,
                composite_bandwidth_mhz: 0.0,
                amp_pixel_clock_mhz: 7.375,
                ..ntsc
            },
            0,
        ),
        (
            "NTSC 14.318 MHz, equal I and Q at 1 MHz",
            Params {
                amp_pixel_clock_mhz: 14.31818,
                composite_i_bandwidth_mhz: 1.0,
                composite_q_bandwidth_mhz: 1.0,
                ..ntsc
            },
            0,
        ),
        (
            "NTSC decoder imperfections: hue slope 25, burst jitter 12, axis error 8, V gain 1.15, green matrix",
            Params {
                composite_hue_slope_deg: 25.0,
                composite_hue_jitter_deg: 12.0,
                composite_decoder_axis_error_deg: 8.0,
                composite_decoder_v_gain: 1.15,
                composite_decoder_green_ry: 0.45,
                composite_decoder_green_by: 0.22,
                ..ntsc
            },
            5,
        ),
        (
            "PAL delay line, burst jitter 20, hue slope -30, axis error -6",
            Params {
                composite_standard: params::COMPOSITE_STANDARD_PAL,
                composite_separation: params::COMPOSITE_SEPARATION_COMB,
                composite_hue_jitter_deg: 20.0,
                composite_hue_slope_deg: -30.0,
                composite_decoder_axis_error_deg: -6.0,
                amp_pixel_clock_mhz: 7.375,
                ..ntsc
            },
            9,
        ),
    ];
    for (what, p, frame) in &cases {
        compare(&mut gpu, &art, p, *frame, what);
    }

    // Review D-03: narrow channels (ACC gain capped, channel sigma lifted to the shader's cap): finite, and
    // the same as the oracle; also with the comb and at a 14.3 MHz clock (a higher bandwidth floor).
    for bw in [0.05_f32, 0.1, 0.2, 0.22, 0.5] {
        for (what, p) in [
            ("notch", ntsc),
            (
                "comb",
                Params {
                    composite_separation: params::COMPOSITE_SEPARATION_COMB,
                    ..ntsc
                },
            ),
            (
                "14.318 MHz",
                Params {
                    amp_pixel_clock_mhz: 14.31818,
                    ..ntsc
                },
            ),
        ] {
            let p = Params {
                composite_bandwidth_mhz: bw,
                ..p
            };
            let out = compare(&mut gpu, &art, &p, 1, &format!("channel {bw} MHz, {what}"));
            assert!(
                out.rgba.iter().all(|c| c.is_finite()),
                "channel {bw} MHz, {what}: non-finite"
            );
        }
    }

    // A width that is not a multiple of the 64-pixel tile.
    compare(&mut gpu, &pixel_art(200, 8), &ntsc, 0, "NTSC notch, 200 px wide");

    // Flat colour and flat grey round trip on the GPU (PLAN acceptance; S-Video within 1 %).
    for (what, p) in [
        ("composite", ntsc),
        (
            "S-Video",
            Params {
                composite_connection: params::COMPOSITE_CONNECTION_SVIDEO,
                ..ntsc
            },
        ),
    ] {
        for colour in [[0.8, 0.3, 0.2], [0.5, 0.5, 0.5]] {
            let rows = vec![vec![colour; 128]; 4];
            let out = compare(&mut gpu, &rows, &p, 0, &format!("{what} flat {colour:?}"));
            for y in 0..4 {
                for x in 32..96 {
                    let g = out.pixel(x, y);
                    for c in 0..3 {
                        assert!(
                            (f64::from(g[c]) - colour[c]).abs() < 0.01,
                            "{what} flat {colour:?} at {x},{y}: {g:?}"
                        );
                    }
                }
            }
        }
    }

    // Luma stripes at the subcarrier (pixel clock 2 f_sc): cross-colour above 10 % in composite, below
    // 1 % in S-Video and with the comb (PLAN acceptance), and the hue alternates per line and per frame.
    let two_fsc = Params {
        amp_pixel_clock_mhz: (2.0 * NTSC_SUBCARRIER_MHZ) as f32,
        ..ntsc
    };
    let st = stripes(128, 4);
    for (what, p, frame, crosstalk) in [
        ("stripes composite", two_fsc, 0, true),
        ("stripes composite frame 1", two_fsc, 1, true),
        (
            "stripes S-Video",
            Params {
                composite_connection: params::COMPOSITE_CONNECTION_SVIDEO,
                ..two_fsc
            },
            0,
            false,
        ),
        (
            "stripes comb",
            Params {
                composite_separation: params::COMPOSITE_SEPARATION_COMB,
                ..two_fsc
            },
            0,
            false,
        ),
    ] {
        let out = compare(&mut gpu, &st, &p, frame, what);
        for y in 0..4u32 {
            if !crosstalk && y == 0 && what.contains("comb") {
                continue; // line 0 has no previous line: notch
            }
            let g = out.pixel(64, y);
            let [_, u, v] = rgb_to_yuv([f64::from(g[0]), f64::from(g[1]), f64::from(g[2])]);
            let chroma = u.hypot(v);
            if crosstalk {
                assert!(chroma > 0.1, "{what} line {y}: chroma {chroma}");
                // +U (blue) on line 0 of frame 0, -U on line 1; frame 1 swaps.
                let blue = (y % 2 == 0) == (frame == 0);
                assert_eq!(u > 0.0, blue, "{what} line {y}: U {u}");
            } else {
                assert!(chroma < 0.01, "{what} line {y}: chroma {chroma}");
            }
        }
    }

    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

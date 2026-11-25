//! GPU tests of the colour targets (C64, NES) together with the composite video module: the slots `target` and `signal`,
//! headless, validation on and zero validation messages. A native image after `decode` goes through the colour target
//! (each pixel recognised as a hardware colour) and then through the composite model set for the machine (its pixel
//! clock, subcarrier, line rate and standard). The checks of design-note step 3c:
//!
//! - **a flat field of every hardware colour returns unchanged** through the composite model (the interior of a wide
//!   block, within 1 % of drive), on the C64's PAL settings with the delay line and on the NES's NTSC settings with its
//!   227 1/3 colour cycles a line: the target gives the colours, the composite model adds only the artefacts;
//! - **the artefacts exist**: a colour edge bleeds over the chroma low-pass (the pixel at the edge is neither colour);
//! - **the dot crawl follows the line advance**: the picture at a colour edge changes from line to line with the phase of
//!   the subcarrier, repeating every second line for the standard 227.5 cycles a line and every third for the NES's
//!   227 1/3 (`composite_line_rate_khz` takes the 15.746 kHz of 341 pixels at 5.369318 MHz).

mod common;

use crt_model::target::Transform;
use phoxium::gpu::{FrameConstants, Headless, ImageF32, Settings, StageInput, StageRun, TargetFormat};
use phoxium::params::{self, Params};

/// The reference colours of a signal target (what an emulator shows for each hardware colour) for the variant of `p`.
fn references(p: &Params) -> Vec<[f32; 3]> {
    let t = phoxium::targets::for_value(p.source_colour_target).expect("a signal target");
    let Transform::Signal(s) = t.select(p.source_colour_variant) else {
        panic!("signal")
    };
    s.references[..s.count].iter().map(|c| c.map(|v| v as f32)).collect()
}

/// An image of `rows` rows of blocks of `block` pixels, block `k` of colour `colours[k]`.
fn blocks(colours: &[[f32; 3]], block: usize, rows: usize) -> ImageF32 {
    let width = colours.len() * block;
    let mut rgba = Vec::new();
    for _ in 0..rows {
        for x in 0..width {
            let c = colours[x / block];
            rgba.extend_from_slice(&[c[0], c[1], c[2], 1.0]);
        }
    }
    ImageF32 {
        width: width as u32,
        height: rows as u32,
        rgba,
    }
}

/// Runs `target` and `signal` on `img` (injected after `decode`): the `signal` image and the `target` image.
fn run(gpu: &mut Headless, img: &ImageF32, p: &Params) -> (ImageF32, ImageF32) {
    let fc = FrameConstants::new((img.width, img.height), (img.width, img.height), false);
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
    (
        r.stage("signal").expect("signal image").clone(),
        r.stage("target").expect("target image").clone(),
    )
}

/// The C64 PAL: dot clock 7.8819 MHz (8 x 0.98524), 504 pixels a line (63 cycles of 8), 15.6387 kHz, 283.5 colour cycles a
/// line (a line advance of half a cycle), 312 lines (a whole number of cycles a frame), the delay line.
fn c64_pal() -> Params {
    Params {
        source_colour_target: params::SOURCE_COLOUR_TARGET_COMMODORE_64,
        composite_enable: true,
        composite_standard: params::COMPOSITE_STANDARD_PAL,
        composite_separation: params::COMPOSITE_SEPARATION_COMB,
        amp_pixel_clock_mhz: 7.8819,
        composite_line_rate_khz: 15.6387,
        composite_frame_advance_deg: 0.0,
        ..Params::default()
    }
}

/// The NES NTSC: pixel clock 5.369318 MHz, 341 pixels a line, 15.746 kHz, 227 1/3 colour cycles a line (a line advance of a
/// third of a cycle), the notch (the comb needs half a cycle), the frame advance of the mean of its odd and even frames.
fn nes_ntsc() -> Params {
    Params {
        source_colour_target: params::SOURCE_COLOUR_TARGET_NINTENDO_ENTERTAINMENT_SYSTEM,
        composite_enable: true,
        composite_standard: params::COMPOSITE_STANDARD_NTSC,
        composite_separation: params::COMPOSITE_SEPARATION_NOTCH,
        amp_pixel_clock_mhz: 5.369318,
        composite_line_rate_khz: 15.746,
        composite_frame_advance_deg: 180.0,
        ..Params::default()
    }
}

/// The worst difference between the `signal` image and the `target` image over the interior of the blocks (8 to 16 pixels
/// in from the edges of blocks of 24), every colour of `p`'s target.
fn flat_interior(gpu: &mut Headless, p: &Params) -> f32 {
    let colours = references(p);
    let img = blocks(&colours, 24, 6);
    let (after, target) = run(gpu, &img, p);
    let mut worst = 0.0f32;
    for k in 0..colours.len() as u32 {
        for y in 0..6 {
            for x in (k * 24 + 8)..(k * 24 + 16) {
                let (a, t) = (after.pixel(x, y), target.pixel(x, y));
                for c in 0..3 {
                    worst = worst.max((a[c] - t[c]).abs());
                }
            }
        }
    }
    worst
}

/// The pixel at the edge between two colours of 32 pixels each, on each of the first `rows` lines.
fn edge_per_line(gpu: &mut Headless, p: &Params, a: [f32; 3], b: [f32; 3], rows: usize) -> Vec<[f32; 3]> {
    let mut rgba = Vec::new();
    for _ in 0..rows {
        for x in 0..64 {
            let c = if x < 32 { a } else { b };
            rgba.extend_from_slice(&[c[0], c[1], c[2], 1.0]);
        }
    }
    let img = ImageF32 {
        width: 64,
        height: rows as u32,
        rgba,
    };
    let (after, _) = run(gpu, &img, p);
    (0..rows as u32)
        .map(|y| {
            let g = after.pixel(33, y);
            [g[0], g[1], g[2]]
        })
        .collect()
}

#[test]
fn the_composite_model_adds_only_the_artefacts() {
    let Some(mut gpu) = common::gpu_or_skip("the_composite_model_adds_only_the_artefacts") else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.enable_only(&["target", "signal"]).unwrap();

    // The flat interior of every hardware colour comes back unchanged.
    for (name, p) in [("C64 PAL, delay line", c64_pal()), ("NES NTSC", nes_ntsc())] {
        let worst = flat_interior(&mut gpu, &p);
        println!("{name}: flat interior, worst difference {worst:.4}");
        assert!(worst < 0.01, "{name}: {worst}");
    }

    // The edge bleeds: with a bright red next to a blue the pixel after the edge is neither, line by line.
    let colours = references(&nes_ntsc());
    let (a, b) = (colours[0x16], colours[0x12]);
    let edge = edge_per_line(&mut gpu, &nes_ntsc(), a, b, 6);
    println!("NES edge, lines 0 to 5: {edge:?}");
    let target_b = {
        let (_, t) = run(&mut gpu, &blocks(&[a, b], 32, 1), &nes_ntsc());
        t.pixel(33, 0)
    };
    assert!(
        edge.iter().all(|e| (0..3).any(|c| (e[c] - target_b[c]).abs() > 0.005)),
        "the edge shows no bleed: {edge:?} against {target_b:?}"
    );

    // The dot crawl: the picture at an edge repeats every third line for the NES, every second for 227.5 cycles a line.
    let spread = |v: &[[f32; 3]], i: usize, j: usize| (0..3).map(|c| (v[i][c] - v[j][c]).abs()).fold(0.0f32, f32::max);
    let nes = edge_per_line(&mut gpu, &nes_ntsc(), a, b, 7);
    let std = edge_per_line(
        &mut gpu,
        &Params {
            composite_line_rate_khz: 0.0,
            ..nes_ntsc()
        },
        a,
        b,
        7,
    );
    println!(
        "NES: line 0 vs 1 {:.4}, 0 vs 3 {:.4}, 0 vs 2 {:.4}; standard NTSC: 0 vs 1 {:.4}, 0 vs 2 {:.4}, 0 vs 3 {:.4}",
        spread(&nes, 0, 1),
        spread(&nes, 0, 3),
        spread(&nes, 0, 2),
        spread(&std, 0, 1),
        spread(&std, 0, 2),
        spread(&std, 0, 3)
    );
    // The NES repeats after three lines (a third of a cycle a line) and not after two; the standard line advance of half
    // a cycle repeats after two lines exactly, and the edge changes from line to line in both.
    assert!(spread(&nes, 0, 3) < 0.005, "NES: line 3 is line 0");
    assert!(
        spread(&nes, 0, 1) > 0.02 && spread(&nes, 0, 2) > 0.005,
        "NES: lines 1 and 2 differ"
    );
    assert!(spread(&std, 0, 2) < 1e-3, "standard: line 2 is line 0");
    assert!(
        spread(&std, 0, 1) > 0.003 && spread(&std, 0, 3) > 0.003,
        "standard: lines 1 and 3 differ"
    );

    common::assert_no_validation_messages(&counter);
}

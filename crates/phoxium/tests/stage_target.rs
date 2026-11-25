//! GPU tests of the source colour target (slot `target`, `shaders/target.comp`), headless, validation on and zero
//! validation messages, one shared context. Each case injects a native-domain image after `decode`, runs only `target`,
//! reads back its image and compares it with the f64 oracle `crt_model::target` (whose own unit tests carry the hand
//! derivations: white at 240 of 255, the primaries' leaks, grey).
//!
//! Budget: the slot stores FP16 (round to nearest, at most 4.9e-4 from 0.5 to 1) on top of the f32 shader arithmetic;
//! max abs 1e-3, mean abs 2e-4 per channel.

mod common;

use crt_model::target::{Curve, Dac, Matrix, Palette, Transform};
use phoxium::gpu::{FrameConstants, Headless, ImageF32, Settings, StageInput, StageRun, TargetFormat};
use phoxium::params::{
    Params, SOURCE_COLOUR_TARGET_GAME_BOY_ADVANCE, SOURCE_COLOUR_TARGET_GAME_BOY_COLOR,
    SOURCE_COLOUR_TARGET_SEGA_MEGA_DRIVE, SOURCE_COLOUR_TARGET_SUPER_GAME_BOY,
    SOURCE_COLOUR_TARGET_SUPER_NINTENDO_BLACK_BOOST,
};

const MAX_ERR: f64 = 1e-3;
const MEAN_ERR: f64 = 2e-4;

/// The transforms of the targets, written here independently of the data files (the numbers of the near.sh article).
/// A Super Game Boy palette from its four RGB555 words (red in bits 0 to 4, green 5 to 9, blue 10 to 14), lightest first,
/// written here independently of the data file.
fn sgb_palette(words: [u32; 4]) -> Transform {
    let colour = |w: u32| [(w & 31) as u8, ((w >> 5) & 31) as u8, ((w >> 10) & 31) as u8];
    Transform::Palette(Palette {
        colours: words.map(colour),
        output_max: 31.0,
    })
}

/// The Mega Drive DAC behind the emulator that shows `step` per 3-bit code, written independently of the data file.
fn md_dac(step: u8) -> Transform {
    Transform::Dac(Dac {
        input: std::array::from_fn(|i| step * i as u8),
        output: [0, 52, 87, 116, 144, 172, 206, 255],
        output_max: 255.0,
    })
}

fn transform(target: u32, palette: i32, input: &str) -> Transform {
    match target {
        SOURCE_COLOUR_TARGET_GAME_BOY_COLOR => Transform::Matrix(Matrix {
            rows: [[26.0, 4.0, 2.0], [0.0, 24.0, 8.0], [6.0, 4.0, 22.0]],
            input_bits: 5,
            input_range: 31.0,
            input_gamma: 1.0,
            clamp: 960.0,
            divisor: 1020.0,
            output_gamma: 1.0,
            output_scale: 1.0,
        }),
        SOURCE_COLOUR_TARGET_GAME_BOY_ADVANCE => Transform::Matrix(Matrix {
            rows: [[255.0, 50.0, 0.0], [10.0, 230.0, 30.0], [50.0, 10.0, 220.0]],
            input_bits: 5,
            input_range: 1.0,
            input_gamma: 4.0,
            clamp: f64::INFINITY,
            divisor: 255.0,
            output_gamma: 2.2,
            output_scale: 255.0 / 280.0,
        }),
        SOURCE_COLOUR_TARGET_SUPER_NINTENDO_BLACK_BOOST => Transform::Curve(Curve {
            table: [
                0x00, 0x01, 0x03, 0x06, 0x0a, 0x0f, 0x15, 0x1c, 0x24, 0x2d, 0x37, 0x42, 0x4e, 0x5b, 0x69, 0x78, 0x88,
                0x90, 0x98, 0xa0, 0xa8, 0xb0, 0xb8, 0xc0, 0xc8, 0xd0, 0xd8, 0xe0, 0xe8, 0xf0, 0xf8, 0xff,
            ],
            output_max: 255.0,
        }),
        // Palette 1 is 1-A (the power-on palette), palette 9 is 2-A, palette 32 is 4-H.
        SOURCE_COLOUR_TARGET_SUPER_GAME_BOY => match palette {
            1 => sgb_palette([0x67BF, 0x265B, 0x10B5, 0x2866]),
            9 => sgb_palette([0x533E, 0x2638, 0x01E5, 0x0000]),
            32 => sgb_palette([0x67FF, 0x2F17, 0x2230, 0x1548]),
            _ => unreachable!(),
        },
        SOURCE_COLOUR_TARGET_SEGA_MEGA_DRIVE => match input {
            "genesis_plus_gx" | "default" => md_dac(34),
            "kega_fusion" => md_dac(33),
            "regen" => md_dac(32),
            _ => unreachable!(),
        },
        _ => unreachable!(),
    }
}
/// A native raster of `w` x `h` pixels: the 5-bit expanded codes of every channel in turn (grey ramp, red, green and
/// blue ramps), then a pseudo-random fill of any 8-bit drive voltage (a frame that was not 5-bit quantised), with
/// black and white in the corners.
fn native(w: usize, h: usize) -> ImageF32 {
    let expand = |c: u32| f64::from((c << 3) | (c >> 2)) / 255.0;
    let mut s: u32 = 4242;
    let mut rgba = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let c = (x % 32) as u32;
            let v = match (y, x) {
                (0, 0) => [0.0; 3],
                (0, 1) => [1.0; 3],
                (0, _) => [expand(c); 3],
                (1, _) => [expand(c), 0.0, 0.0],
                (2, _) => [0.0, expand(c), 0.0],
                (3, _) => [0.0, 0.0, expand(c)],
                _ => {
                    s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    let r = f64::from((s >> 24) & 255) / 255.0;
                    s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    let g = f64::from((s >> 24) & 255) / 255.0;
                    s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    let b = f64::from((s >> 24) & 255) / 255.0;
                    [r, g, b]
                }
            };
            rgba.extend_from_slice(&[v[0] as f32, v[1] as f32, v[2] as f32, 1.0]);
        }
    }
    ImageF32 {
        width: w as u32,
        height: h as u32,
        rgba,
    }
}

/// Runs only `target` on `img` (injected after `decode`).
fn run(gpu: &mut Headless, img: &ImageF32, p: &Params) -> ImageF32 {
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
    r.stage("target").expect("target image").clone()
}

#[test]
fn target_slot_matches_the_oracle() {
    let Some(mut gpu) = common::gpu_or_skip("target_slot_matches_the_oracle") else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.enable_only(&["target"]).unwrap();
    let img = native(64, 24);

    // No target (the default): skipped, the image passes bit for bit.
    let off = run(&mut gpu, &img, &Params::default());
    assert_eq!(off.rgba, img.rgba, "none: bit-exact identity");
    assert!(
        gpu.last_skipped_slots().contains(&"target"),
        "none: the slot is skipped"
    );

    for (name, target, palette, input) in [
        ("gbc", SOURCE_COLOUR_TARGET_GAME_BOY_COLOR, 1, "default"),
        ("gba", SOURCE_COLOUR_TARGET_GAME_BOY_ADVANCE, 1, "default"),
        ("snes", SOURCE_COLOUR_TARGET_SUPER_NINTENDO_BLACK_BOOST, 1, "default"),
        ("sgb 1-A", SOURCE_COLOUR_TARGET_SUPER_GAME_BOY, 1, "default"),
        ("sgb 2-A", SOURCE_COLOUR_TARGET_SUPER_GAME_BOY, 9, "default"),
        ("sgb 4-H", SOURCE_COLOUR_TARGET_SUPER_GAME_BOY, 32, "default"),
        ("md gpgx", SOURCE_COLOUR_TARGET_SEGA_MEGA_DRIVE, 1, "genesis_plus_gx"),
        ("md kega", SOURCE_COLOUR_TARGET_SEGA_MEGA_DRIVE, 1, "kega_fusion"),
        ("md regen", SOURCE_COLOUR_TARGET_SEGA_MEGA_DRIVE, 1, "regen"),
        ("md default", SOURCE_COLOUR_TARGET_SEGA_MEGA_DRIVE, 1, "default"),
    ] {
        let p = Params {
            source_colour_target: target,
            source_colour_palette: palette,
            source_colour_input: phoxium::params::meta("source_colour_input")
                .unwrap()
                .values
                .iter()
                .position(|v| *v == input)
                .unwrap() as u32,
            ..Params::default()
        };
        let out = run(&mut gpu, &img, &p);
        assert!(!gpu.last_skipped_slots().contains(&"target"), "{name}: the slot runs");
        assert_eq!((out.width, out.height), (img.width, img.height));
        let t = transform(target, palette, input);
        let (mut max, mut sum, mut n, mut worst) = (0.0_f64, 0.0, 0, (0, 0));
        for y in 0..img.height {
            for x in 0..img.width {
                let i = img.pixel(x, y);
                let want = t.apply([f64::from(i[0]), f64::from(i[1]), f64::from(i[2])]);
                let g = out.pixel(x, y);
                for c in 0..3 {
                    let e = (f64::from(g[c]) - want[c]).abs();
                    if e > max {
                        max = e;
                        worst = (x, y);
                    }
                    sum += e;
                    n += 1;
                }
                assert_eq!(g[3], 1.0, "{name}: alpha");
            }
        }
        let mean = sum / f64::from(n);
        println!("{name}: max {max:.2e} at {worst:?}, mean {mean:.2e}");
        assert!(max <= MAX_ERR, "{name}: max error {max} at {worst:?}");
        assert!(mean <= MEAN_ERR, "{name}: mean error {mean}");
        // Black: itself for the colour correction (a matrix or a curve keeps black black), the darkest colour for a palette.
        let black = out.pixel(0, 0);
        let want_black = t.apply([0.0; 3]);
        for c in 0..3 {
            assert!(
                (f64::from(black[c]) - want_black[c]).abs() <= MAX_ERR,
                "{name}: black {black:?}"
            );
        }
        // White: the GBC clips at 240 of 255, the GBA is tinted (0.988, 0.935, 0.950), the SNES table ends at 255.
        let white = out.pixel(1, 0);
        let want_white = t.apply([1.0; 3]);
        for c in 0..3 {
            assert!(
                (f64::from(white[c]) - want_white[c]).abs() <= MAX_ERR,
                "{name}: white {white:?}"
            );
        }
    }
    common::assert_no_validation_messages(&counter);
}

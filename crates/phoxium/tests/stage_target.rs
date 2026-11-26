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
    Params, SOURCE_COLOUR_TARGET_ATARI_8BIT_GTIA, SOURCE_COLOUR_TARGET_COMMODORE_64,
    SOURCE_COLOUR_TARGET_GAME_BOY_ADVANCE, SOURCE_COLOUR_TARGET_GAME_BOY_ADVANCE_SP_AGS_101,
    SOURCE_COLOUR_TARGET_GAME_BOY_COLOR, SOURCE_COLOUR_TARGET_GAME_BOY_COLOR_COLORIMETRIC,
    SOURCE_COLOUR_TARGET_IBM_CGA_RGBI, SOURCE_COLOUR_TARGET_NINTENDO_ENTERTAINMENT_SYSTEM,
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

fn transform(target: u32, variant: i32) -> Transform {
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
        // Variant 1 is 1-A (the power-on palette), variant 9 is 2-A, variant 32 is 4-H.
        SOURCE_COLOUR_TARGET_SUPER_GAME_BOY => match variant {
            1 => sgb_palette([0x67BF, 0x265B, 0x10B5, 0x2866]),
            9 => sgb_palette([0x533E, 0x2638, 0x01E5, 0x0000]),
            32 => sgb_palette([0x67FF, 0x2F17, 0x2230, 0x1548]),
            _ => unreachable!(),
        },
        // Variant 1 is Genesis Plus GX (steps of 34, the default), 2 Kega Fusion (33), 3 Regen (32).
        SOURCE_COLOUR_TARGET_SEGA_MEGA_DRIVE => match variant {
            1 => md_dac(34),
            2 => md_dac(33),
            3 => md_dac(32),
            _ => unreachable!(),
        },
        // The C64: the data file's own variant (the oracle is `crt_model::target::Signal`, built from the file).
        SOURCE_COLOUR_TARGET_COMMODORE_64
        | SOURCE_COLOUR_TARGET_ATARI_8BIT_GTIA
        | SOURCE_COLOUR_TARGET_NINTENDO_ENTERTAINMENT_SYSTEM
        | SOURCE_COLOUR_TARGET_IBM_CGA_RGBI
        | SOURCE_COLOUR_TARGET_GAME_BOY_COLOR_COLORIMETRIC
        | SOURCE_COLOUR_TARGET_GAME_BOY_ADVANCE_SP_AGS_101 => phoxium::targets::for_value(target)
            .expect("signal target")
            .transform_for(usize::try_from(variant - 1).unwrap()),
        _ => unreachable!(),
    }
}

/// Pepto's 16 colours (a published palette), as an emulator shows them.
const PEPTO: [u32; 16] = [
    0x000000, 0xFFFFFF, 0x68372B, 0x70A4B2, 0x6F3D86, 0x588D43, 0x352879, 0xB8C76F, 0x6F4F25, 0x433900, 0x9A6759,
    0x444444, 0x6C6C6C, 0x9AD284, 0x6C5EB5, 0x959595,
];

/// The 64 colours of pally's 2C02G palette for Japan (MIT-0), as an emulator shows an NES picture.
const PALLY_J: [&str; 64] = [
    "626262", "001C95", "1904AC", "42009D", "61006B", "6E0025", "650500", "491E00", "223700", "004900", "004F00",
    "004816", "00355E", "000000", "000000", "000000", "ABABAB", "0C4EDB", "3D2EFF", "7115F3", "9B0BB9", "B01262",
    "A92704", "894600", "576600", "237F00", "008900", "008332", "006D90", "000000", "000000", "000000", "FFFFFF",
    "57A5FF", "8287FF", "B46DFF", "DF60FF", "F863C6", "F8746D", "DE9020", "B3AE00", "81C800", "56D522", "3DD36F",
    "3EC1C8", "4E4E4E", "000000", "000000", "FFFFFF", "BEE0FF", "CDD4FF", "E0CAFF", "F1C4FF", "FCC4EF", "FDCACE",
    "F5D4AF", "E6DF9C", "D3E99A", "C2EFA8", "B7EFC4", "B6EAE5", "B8B8B8", "000000", "000000",
];

fn nes(i: usize) -> [f64; 3] {
    let v = u32::from_str_radix(PALLY_J[i % 64], 16).unwrap();
    [(v >> 16) & 255, (v >> 8) & 255, v & 255].map(|c| f64::from(c) / 255.0)
}

/// The canonical CGA RGBI colours: each channel 0, 0x55, 0xAA or 0xFF, the brown with half the green.
fn cga(i: usize) -> [f64; 3] {
    let i = i % 16;
    let (on, bright) = (|b: usize| i & b != 0, i & 8 != 0);
    let level = |set: bool| f64::from(u8::from(set) * 0xAA + u8::from(bright) * 0x55) / 255.0;
    let mut c = [level(on(4)), level(on(2)), level(on(1))];
    if i == 6 {
        c[1] = f64::from(0x55u8) / 255.0;
    }
    c
}

fn pepto(i: usize) -> [f64; 3] {
    let c = PEPTO[i % 16];
    [(c >> 16) & 255, (c >> 8) & 255, c & 255].map(|v| f64::from(v) / 255.0)
}

/// The reference colour `i % 256` of the Atari 8-bit target (what an emulator shows for the colour register `i`), read from
/// the data file: the classification of the shader is checked on it and on the same a little off.
fn atari(i: usize) -> [f64; 3] {
    let Transform::Signal(s) = phoxium::targets::for_value(SOURCE_COLOUR_TARGET_ATARI_8BIT_GTIA)
        .expect("atari")
        .transform
    else {
        panic!("signal")
    };
    s.references[i % 256]
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
                // The C64 palette of Pepto, then the same a little off (a pixel the classification still takes).
                (5, _) => pepto(x),
                (6, _) => pepto(x).map(|v| (v + 0.012).min(1.0)),
                // The NES palette of pally, then the same a little off.
                (7, _) => nes(x),
                (8, _) => nes(x).map(|v| (v + 0.012).min(1.0)),
                // The canonical CGA palette (each channel 0, 1/3, 2/3 or 1, the brown AA5500), then the dark yellow.
                (9, _) => cga(x),
                // The Atari 8-bit reference palette: the colour registers 4 x to 4 x + 3 over four rows, two of them a little
                // off (the classification still takes them).
                (10, _) => atari(x * 4),
                (11, _) => atari(x * 4 + 1).map(|v| (v + 0.012).min(1.0)),
                (12, _) => atari(x * 4 + 2),
                (13, _) => atari(x * 4 + 3).map(|v| (v - 0.012).max(0.0)),
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

    for (name, target, variant) in [
        ("gbc", SOURCE_COLOUR_TARGET_GAME_BOY_COLOR, 1),
        ("gba", SOURCE_COLOUR_TARGET_GAME_BOY_ADVANCE, 1),
        ("snes", SOURCE_COLOUR_TARGET_SUPER_NINTENDO_BLACK_BOOST, 1),
        ("sgb 1-A", SOURCE_COLOUR_TARGET_SUPER_GAME_BOY, 1),
        ("sgb 2-A", SOURCE_COLOUR_TARGET_SUPER_GAME_BOY, 9),
        ("sgb 4-H", SOURCE_COLOUR_TARGET_SUPER_GAME_BOY, 32),
        ("md gpgx", SOURCE_COLOUR_TARGET_SEGA_MEGA_DRIVE, 1),
        ("md kega", SOURCE_COLOUR_TARGET_SEGA_MEGA_DRIVE, 2),
        ("md regen", SOURCE_COLOUR_TARGET_SEGA_MEGA_DRIVE, 3),
        ("c64 6569R3", SOURCE_COLOUR_TARGET_COMMODORE_64, 1),
        ("c64 6569R4", SOURCE_COLOUR_TARGET_COMMODORE_64, 2),
        ("c64 6569R1", SOURCE_COLOUR_TARGET_COMMODORE_64, 4),
        ("nes 2C02G blank", SOURCE_COLOUR_TARGET_NINTENDO_ENTERTAINMENT_SYSTEM, 1),
        ("nes 2C02G setup", SOURCE_COLOUR_TARGET_NINTENDO_ENTERTAINMENT_SYSTEM, 2),
        ("nes 2C02E blank", SOURCE_COLOUR_TARGET_NINTENDO_ENTERTAINMENT_SYSTEM, 3),
        ("cga 5153", SOURCE_COLOUR_TARGET_IBM_CGA_RGBI, 1),
        (
            "gbc colorimetric above",
            SOURCE_COLOUR_TARGET_GAME_BOY_COLOR_COLORIMETRIC,
            1,
        ),
        (
            "gbc colorimetric central",
            SOURCE_COLOUR_TARGET_GAME_BOY_COLOR_COLORIMETRIC,
            2,
        ),
        ("gbc mGBA sRGB", SOURCE_COLOUR_TARGET_GAME_BOY_COLOR_COLORIMETRIC, 4),
        ("ags 2023 measured", SOURCE_COLOUR_TARGET_GAME_BOY_ADVANCE_SP_AGS_101, 1),
        ("ags 2026 D65", SOURCE_COLOUR_TARGET_GAME_BOY_ADVANCE_SP_AGS_101, 4),
        ("atari 800XL", SOURCE_COLOUR_TARGET_ATARI_8BIT_GTIA, 1),
        ("atari 800", SOURCE_COLOUR_TARGET_ATARI_8BIT_GTIA, 3),
        ("atari 800XL delay 27.5", SOURCE_COLOUR_TARGET_ATARI_8BIT_GTIA, 6),
    ] {
        let p = Params {
            source_colour_target: target,
            source_colour_variant: variant,
            ..Params::default()
        };
        let out = run(&mut gpu, &img, &p);
        assert!(!gpu.last_skipped_slots().contains(&"target"), "{name}: the slot runs");
        assert_eq!((out.width, out.height), (img.width, img.height));
        let t = transform(target, variant);
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

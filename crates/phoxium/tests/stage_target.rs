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
    Params, SOURCE_COLOUR_TARGET_AMSTRAD_CPC, SOURCE_COLOUR_TARGET_AMSTRAD_CPC_GREEN_SCREEN,
    SOURCE_COLOUR_TARGET_APPLE_II_LORES, SOURCE_COLOUR_TARGET_ATARI_2600_TIA, SOURCE_COLOUR_TARGET_ATARI_7800_MARIA,
    SOURCE_COLOUR_TARGET_ATARI_8BIT_GTIA, SOURCE_COLOUR_TARGET_COMMODORE_64, SOURCE_COLOUR_TARGET_GAME_BOY_ADVANCE,
    SOURCE_COLOUR_TARGET_GAME_BOY_ADVANCE_SP_AGS_101, SOURCE_COLOUR_TARGET_GAME_BOY_COLOR,
    SOURCE_COLOUR_TARGET_GAME_BOY_COLOR_COLORIMETRIC, SOURCE_COLOUR_TARGET_GAME_BOY_LCD,
    SOURCE_COLOUR_TARGET_IBM_CGA_COMPOSITE, SOURCE_COLOUR_TARGET_IBM_CGA_RGBI, SOURCE_COLOUR_TARGET_MSX2_V9958,
    SOURCE_COLOUR_TARGET_NEC_PC_ENGINE_COMPOSITE, SOURCE_COLOUR_TARGET_NINTENDO_ENTERTAINMENT_SYSTEM,
    SOURCE_COLOUR_TARGET_NINTENDO_VIRTUAL_BOY, SOURCE_COLOUR_TARGET_SEGA_MASTER_SYSTEM,
    SOURCE_COLOUR_TARGET_SEGA_MEGA_DRIVE, SOURCE_COLOUR_TARGET_SNK_NEO_GEO, SOURCE_COLOUR_TARGET_SUPER_GAME_BOY,
    SOURCE_COLOUR_TARGET_SUPER_NINTENDO_BLACK_BOOST, SOURCE_COLOUR_TARGET_TEXAS_INSTRUMENTS_TMS9928,
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

/// A Game Boy LCD palette from its four 0xRRGGBB colours, lightest first (the hex of BGB's swatches and SameBoy's source),
/// written here independently of the data file.
fn gb_lcd_palette(hex: [u32; 4]) -> Transform {
    Transform::Palette(Palette {
        colours: hex.map(|h| [(h >> 16) as u8, (h >> 8 & 255) as u8, (h & 255) as u8]),
        output_max: 255.0,
    })
}

/// The Mega Drive DAC behind the emulator that shows `step` per 3-bit code (the 8 normal levels, the last point repeated to
/// fill the 15 of the arrays), written independently of the data file.
fn md_dac(step: u8) -> Transform {
    Transform::Dac(Dac {
        input: std::array::from_fn(|i| step * i.min(7) as u8),
        output: [0, 52, 87, 116, 144, 172, 206, 255, 255, 255, 255, 255, 255, 255, 255],
        output_green: None,
        output_blue: None,
        output_max: 255.0,
    })
}

/// The Mega Drive's 15 levels behind Genesis Plus GX (17 per level: normal 34 n, shadow 17 n, highlight 119 + 17 n), the
/// levels written here from TmEE's millivolts (0, 108, 192, 261, 324, 378, 432, 483, 537, 588, 642, 699, 768, 849, 951)
/// scaled so that 951 mV is 255 and rounded (the published table has 52 for 51.5), independently of the data file.
fn md_dac15() -> Transform {
    let mv = [
        0.0f64, 108.0, 192.0, 261.0, 324.0, 378.0, 432.0, 483.0, 537.0, 588.0, 642.0, 699.0, 768.0, 849.0, 951.0,
    ];
    Transform::Dac(Dac {
        input: std::array::from_fn(|k| 17 * k as u8),
        output: std::array::from_fn(|k| {
            if k == 2 {
                52
            } else {
                (mv[k] / 951.0 * 255.0).round() as u8
            }
        }),
        output_green: None,
        output_blue: None,
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
            scale: 1.0,
        }),
        // The Neo Geo ladder written here from the resistors (not read from the data file): the levels are the divider of
        // the set bits rounded to whole codes, the variant scales the dark bit (8200 ohm), the shadow (150 ohm) and both.
        SOURCE_COLOUR_TARGET_SNK_NEO_GEO => {
            let r = [3900.0f64, 2200.0, 1000.0, 470.0, 220.0];
            let ladder = |code: u32, pull: f64| {
                let on: f64 = (0..5).filter(|b| code >> b & 1 == 1).map(|b| 1.0 / r[b]).sum();
                on / (r.iter().map(|x| 1.0 / x).sum::<f64>() + pull)
            };
            let pull =
                [0.0, 1.0 / 8200.0, 1.0 / 150.0, 1.0 / 8200.0 + 1.0 / 150.0][usize::try_from(variant - 1).unwrap()];
            Transform::Curve(Curve {
                table: std::array::from_fn(|c| (255.0 * ladder(c as u32, 0.0)).round() as u8),
                output_max: 255.0,
                scale: ladder(31, pull),
            })
        }
        // Variant 1 is 1-A (the power-on palette), variant 9 is 2-A, variant 32 is 4-H.
        SOURCE_COLOUR_TARGET_SUPER_GAME_BOY => match variant {
            1 => sgb_palette([0x67BF, 0x265B, 0x10B5, 0x2866]),
            9 => sgb_palette([0x533E, 0x2638, 0x01E5, 0x0000]),
            32 => sgb_palette([0x67FF, 0x2F17, 0x2230, 0x1548]),
            _ => unreachable!(),
        },
        // Variant 1 is BGB reality DMG, 2 BGB reality Pocket, 3 SameBoy DMG, 4 SameBoy Pocket, 5 SameBoy Light.
        SOURCE_COLOUR_TARGET_GAME_BOY_LCD => match variant {
            1 => gb_lcd_palette([0x759833, 0x588F51, 0x3B7560, 0x2E615A]),
            2 => gb_lcd_palette([0xADBF92, 0x96A67C, 0x727E64, 0x5A635C]),
            3 => gb_lcd_palette([0xC6DE8C, 0x84A563, 0x396139, 0x081810]),
            5 => gb_lcd_palette([0x7FE2C3, 0x56B495, 0x357862, 0x0A1C15]),
            _ => unreachable!(),
        },
        // Variant 1 is Genesis Plus GX (17 per level on 15 levels, the default), 2 Kega Fusion (steps of 33 on the 8 normal
        // levels), 3 Regen (32).
        SOURCE_COLOUR_TARGET_SEGA_MEGA_DRIVE => match variant {
            1 => md_dac15(),
            2 => md_dac(33),
            3 => md_dac(32),
            _ => unreachable!(),
        },
        // The Master System, written here independently of the data file: a plain 2-bit picture (85 per code), four codes
        // padded to eight with the last point. Variant 1 the first VDP with TmEE's levels and a blue first step of 110,
        // 2 the first VDP of the scope (linear red and green, blue 104), 3 the second VDP (89 and 174 on the three).
        SOURCE_COLOUR_TARGET_SEGA_MASTER_SYSTEM => {
            let pad = |a: [u8; 4]| std::array::from_fn(|i| a[i.min(3)]);
            let (red, blue) = match variant {
                1 => ([0, 90, 173, 255], Some([0, 110, 173, 255])),
                2 => ([0, 85, 170, 255], Some([0, 104, 170, 255])),
                3 => ([0, 89, 174, 255], None),
                _ => unreachable!(),
            };
            Transform::Dac(Dac {
                input: pad([0, 85, 170, 255]),
                output: pad(red),
                output_green: None,
                output_blue: blue.map(pad),
                output_max: 255.0,
            })
        }
        // The CPC, written here independently of the data file: three codes padded with the last point, the half level an
        // emulator shows (128, 127, MAME's 96) going to 0.40 of 255 = 102, black and full kept.
        SOURCE_COLOUR_TARGET_AMSTRAD_CPC => {
            let half = [128u8, 127, 96][usize::try_from(variant - 1).unwrap()];
            let pad = |a: [u8; 3]| std::array::from_fn(|i| a[i.min(2)]);
            Transform::Dac(Dac {
                input: pad([0, half, 255]),
                output: pad([0, (0.40f64 * 255.0).round() as u8, 255]),
                output_green: None,
                output_blue: None,
                output_max: 255.0,
            })
        }
        // The V9958 palette, written here independently of the data file: the 3-bit level n as the 5-bit code
        // floor(4.5 n) shown at code x 255 / 31 (rounded), behind an emulator expansion of the 3 bits (variant 1 the bit
        // repetition, variant 2 the truncated 255 n / 7).
        SOURCE_COLOUR_TARGET_MSX2_V9958 => {
            let level = |n: u32| {
                let n = n as usize;
                (
                    if variant == 1 {
                        ((n << 5) | (n << 2) | (n >> 1)) as u8
                    } else {
                        (n * 255 / 7) as u8
                    },
                    ((f64::from((n * 9 / 2) as u32) * 255.0) / 31.0).round() as u8,
                )
            };
            Transform::Dac(Dac {
                input: std::array::from_fn(|i| level(i.min(7) as u32).0),
                output: std::array::from_fn(|i| level(i.min(7) as u32).1),
                output_green: None,
                output_blue: None,
                output_max: 255.0,
            })
        }
        // The CPC green monitor: the 9 : 3 : 1 sum over 13 on all three channels, with an exponent that puts the emulator's
        // half level (128, or MAME's 96) on 0.5 (linear steps) or on 0.40 (the Gate Array's measured half level).
        SOURCE_COLOUR_TARGET_AMSTRAD_CPC_GREEN_SCREEN => {
            let (half, level) =
                [(128.0f64, 0.5f64), (128.0, 0.40), (96.0, 0.5), (96.0, 0.40)][usize::try_from(variant - 1).unwrap()];
            Transform::Matrix(Matrix {
                rows: [[9.0, 3.0, 1.0]; 3],
                input_bits: 8,
                input_range: 1.0,
                input_gamma: if variant == 1 {
                    1.0
                } else {
                    level.ln() / (half / 255.0).ln()
                },
                clamp: f64::INFINITY,
                divisor: 13.0,
                output_gamma: 1.0,
                output_scale: 1.0,
            })
        }
        // The Virtual Boy, written here independently of the data file: the emulator's 2.2 undone, red only, encoded again;
        // variant 1 takes the BT.601 luma of the linearised channels, variant 2 the red channel alone.
        SOURCE_COLOUR_TARGET_NINTENDO_VIRTUAL_BOY => Transform::Matrix(Matrix {
            rows: [
                if variant == 1 {
                    [0.299, 0.587, 0.114]
                } else {
                    [1.0, 0.0, 0.0]
                },
                [0.0; 3],
                [0.0; 3],
            ],
            input_bits: 8,
            input_range: 1.0,
            input_gamma: 2.2,
            clamp: f64::INFINITY,
            divisor: 1.0,
            output_gamma: 2.2,
            output_scale: 1.0,
        }),
        // The C64: the data file's own variant (the oracle is `crt_model::target::Signal`, built from the file).
        SOURCE_COLOUR_TARGET_COMMODORE_64
        | SOURCE_COLOUR_TARGET_ATARI_8BIT_GTIA
        | SOURCE_COLOUR_TARGET_NINTENDO_ENTERTAINMENT_SYSTEM
        | SOURCE_COLOUR_TARGET_IBM_CGA_RGBI
        | SOURCE_COLOUR_TARGET_IBM_CGA_COMPOSITE
        | SOURCE_COLOUR_TARGET_ATARI_7800_MARIA
        | SOURCE_COLOUR_TARGET_APPLE_II_LORES
        | SOURCE_COLOUR_TARGET_ATARI_2600_TIA
        | SOURCE_COLOUR_TARGET_NEC_PC_ENGINE_COMPOSITE
        | SOURCE_COLOUR_TARGET_GAME_BOY_COLOR_COLORIMETRIC
        | SOURCE_COLOUR_TARGET_TEXAS_INSTRUMENTS_TMS9928
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

/// The reference colour `i % 256` of the Atari 7800 target (MAME's 25.7 degree palette, in the data file): the register
/// values 4 x to 4 x + 3 over two rows, one of them a little off.
fn atari_7800(i: usize) -> [f64; 3] {
    let Transform::Signal(s) = phoxium::targets::for_value(SOURCE_COLOUR_TARGET_ATARI_7800_MARIA)
        .expect("atari 7800")
        .transform
    else {
        panic!("signal")
    };
    s.references[i % 256]
}

/// The reference colour `i % 16` of the Apple II lo-res target (MAME's palette, in the data file).
fn apple_ii(i: usize) -> [f64; 3] {
    let Transform::Signal(s) = phoxium::targets::for_value(SOURCE_COLOUR_TARGET_APPLE_II_LORES)
        .expect("apple ii")
        .transform
    else {
        panic!("signal")
    };
    s.references[i % 16]
}

/// The reference colour `i % 128` of the Atari 2600 target (MAME's TIA palette, in the data file): the entries 2 x and 2 x + 1
/// over two rows, one of them a little off.
fn atari_2600(i: usize) -> [f64; 3] {
    let Transform::Signal(s) = phoxium::targets::for_value(SOURCE_COLOUR_TARGET_ATARI_2600_TIA)
        .expect("atari 2600")
        .transform
    else {
        panic!("signal")
    };
    s.references[i % 128]
}

/// The reference entry `i % 30` of the TMS9928A target (15 colours in two reference sets of exponents 1.1 and 1.5), read from
/// the data file.
fn tms(i: usize) -> [f64; 3] {
    let Transform::Signal(s) = phoxium::targets::for_value(SOURCE_COLOUR_TARGET_TEXAS_INSTRUMENTS_TMS9928)
        .expect("tms9928")
        .transform
    else {
        panic!("signal")
    };
    s.references[i % 30]
}

/// The PC Engine colour code `i % 512` (index G x 64 + R x 8 + B) as an emulator shows it: each 3-bit level spread by repeating
/// its bits (255 n / 7 rounded) or, with `times36`, as 36 n.
fn pc_engine_code(i: usize, times36: bool) -> [f64; 3] {
    let i = i % 512;
    let level = |n: usize| {
        f64::from(if times36 {
            36 * n as u32
        } else {
            (n << 5 | n << 2 | n >> 1) as u32
        }) / 255.0
    };
    [level((i >> 3) & 7), level(i >> 6), level(i & 7)]
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
                // The 2-bit codes of a Master System picture (0, 85, 170, 255 per channel, the three channels on different
                // codes so that the blue first step meets every red and green). The random generator is advanced as the
                // row used to, so that the pixels below stay what they were.
                (4, _) => {
                    for _ in 0..3 {
                        s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    }
                    let l = |k: usize| f64::from((k % 4) as u8 * 85) / 255.0;
                    [l(x), l(x / 4), l(x / 2)]
                }
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
                // The TMS9928A reference entries (both exponent sets), then the same a little off.
                (14, _) => tms(x),
                (15, _) => tms(x).map(|v| (v + 0.012).min(1.0)),
                // The Atari 7800 reference colours (MAME's palette) four over two rows, the second a little off.
                (16, _) => atari_7800(x * 4 + 1),
                (17, _) => atari_7800(x * 4 + 2).map(|v| (v - 0.012).max(0.0)),
                // The PC Engine codes in both expansions of an emulator (64 of the 512 each).
                (18, _) => pc_engine_code(x * 8 + 5, false),
                (19, _) => pc_engine_code(x * 8 + 258, true),
                // The Atari 2600 reference colours (MAME's TIA palette) two over two rows, the second a little off.
                (20, _) => atari_2600(x * 2),
                (21, _) => atari_2600(x * 2 + 1).map(|v| (v + 0.012).min(1.0)),
                // The Apple II lo-res reference colours (MAME's palette) twice over, then a little off.
                (22, _) if x < 32 => apple_ii(x),
                (22, _) => apple_ii(x).map(|v| (v + 0.012).min(1.0)),
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
        ("game boy lcd bgb dmg", SOURCE_COLOUR_TARGET_GAME_BOY_LCD, 1),
        ("game boy lcd bgb pocket", SOURCE_COLOUR_TARGET_GAME_BOY_LCD, 2),
        ("game boy lcd sameboy dmg", SOURCE_COLOUR_TARGET_GAME_BOY_LCD, 3),
        ("game boy lcd sameboy light", SOURCE_COLOUR_TARGET_GAME_BOY_LCD, 5),
        ("md gpgx 15 levels", SOURCE_COLOUR_TARGET_SEGA_MEGA_DRIVE, 1),
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
        ("tms9928 datasheet", SOURCE_COLOUR_TARGET_TEXAS_INSTRUMENTS_TMS9928, 1),
        ("tms9929 scope", SOURCE_COLOUR_TARGET_TEXAS_INSTRUMENTS_TMS9928, 2),
        ("neo geo normal", SOURCE_COLOUR_TARGET_SNK_NEO_GEO, 1),
        ("neo geo dark", SOURCE_COLOUR_TARGET_SNK_NEO_GEO, 2),
        ("neo geo shadow", SOURCE_COLOUR_TARGET_SNK_NEO_GEO, 3),
        ("neo geo shadow dark", SOURCE_COLOUR_TARGET_SNK_NEO_GEO, 4),
        ("sms first vdp bench", SOURCE_COLOUR_TARGET_SEGA_MASTER_SYSTEM, 1),
        ("sms first vdp scope", SOURCE_COLOUR_TARGET_SEGA_MASTER_SYSTEM, 2),
        ("sms second vdp", SOURCE_COLOUR_TARGET_SEGA_MASTER_SYSTEM, 3),
        ("cga composite old", SOURCE_COLOUR_TARGET_IBM_CGA_COMPOSITE, 1),
        ("cga composite new", SOURCE_COLOUR_TARGET_IBM_CGA_COMPOSITE, 2),
        (
            "cga composite old burst gain",
            SOURCE_COLOUR_TARGET_IBM_CGA_COMPOSITE,
            3,
        ),
        (
            "cga composite new burst gain",
            SOURCE_COLOUR_TARGET_IBM_CGA_COMPOSITE,
            4,
        ),
        ("atari 7800 25.7", SOURCE_COLOUR_TARGET_ATARI_7800_MARIA, 1),
        ("atari 7800 27.7", SOURCE_COLOUR_TARGET_ATARI_7800_MARIA, 4),
        ("atari 7800 ladder", SOURCE_COLOUR_TARGET_ATARI_7800_MARIA, 5),
        ("atari 2600 25.7", SOURCE_COLOUR_TARGET_ATARI_2600_TIA, 1),
        ("atari 2600 24", SOURCE_COLOUR_TARGET_ATARI_2600_TIA, 2),
        ("atari 2600 chroma 0.23", SOURCE_COLOUR_TARGET_ATARI_2600_TIA, 5),
        ("pc engine kitrinx", SOURCE_COLOUR_TARGET_NEC_PC_ENGINE_COMPOSITE, 1),
        ("pc engine v 14/15", SOURCE_COLOUR_TARGET_NEC_PC_ENGINE_COMPOSITE, 2),
        ("cpc half 128", SOURCE_COLOUR_TARGET_AMSTRAD_CPC, 1),
        ("cpc half 127", SOURCE_COLOUR_TARGET_AMSTRAD_CPC, 2),
        ("cpc mame 96", SOURCE_COLOUR_TARGET_AMSTRAD_CPC, 3),
        ("msx2 v9958 rounded", SOURCE_COLOUR_TARGET_MSX2_V9958, 1),
        ("msx2 v9958 truncated", SOURCE_COLOUR_TARGET_MSX2_V9958, 2),
        ("cpc green linear", SOURCE_COLOUR_TARGET_AMSTRAD_CPC_GREEN_SCREEN, 1),
        ("cpc green half 0.40", SOURCE_COLOUR_TARGET_AMSTRAD_CPC_GREEN_SCREEN, 2),
        (
            "cpc green mame linear",
            SOURCE_COLOUR_TARGET_AMSTRAD_CPC_GREEN_SCREEN,
            3,
        ),
        (
            "cpc green mame half 0.40",
            SOURCE_COLOUR_TARGET_AMSTRAD_CPC_GREEN_SCREEN,
            4,
        ),
        ("virtual boy grey", SOURCE_COLOUR_TARGET_NINTENDO_VIRTUAL_BOY, 1),
        ("virtual boy red ramp", SOURCE_COLOUR_TARGET_NINTENDO_VIRTUAL_BOY, 2),
        ("apple ii wave burst gain", SOURCE_COLOUR_TARGET_APPLE_II_LORES, 1),
        ("apple ii munafo burst gain", SOURCE_COLOUR_TARGET_APPLE_II_LORES, 2),
        ("apple ii wave fixed gain", SOURCE_COLOUR_TARGET_APPLE_II_LORES, 3),
        ("apple ii munafo fixed gain", SOURCE_COLOUR_TARGET_APPLE_II_LORES, 4),
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

/// The Mega Drive behind Genesis Plus GX (variant 1, the 15-point map of shader kind 5): a picture of the three modes
/// (shadow n as 17 n, normal n as 34 n, highlight n as 119 + 17 n, for the codes 0 to 7, the channels on different codes)
/// comes out at the measured levels (0, 29, 52, 70, 87, 101, 116, 130, 144, 158, 172, 187, 206, 228, 255 of 255), and Kega
/// Fusion (variant 2, kind 3) still puts the first shadow half step on the straight line (26).
#[test]
fn mega_drive_shadow_and_highlight_levels_come_out_at_the_measured_levels() {
    let Some(mut gpu) = common::gpu_or_skip("mega_drive_shadow_and_highlight_levels") else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.enable_only(&["target"]).unwrap();
    let measured = [0u32, 29, 52, 70, 87, 101, 116, 130, 144, 158, 172, 187, 206, 228, 255];
    // One row per mode, 8 codes across; the three channels are on the codes n, n + 3 and n + 5 (mod 8) of the same mode.
    let mode = |m: usize, n: usize| -> f64 {
        let level = match m {
            0 => 17 * n,
            1 => 34 * n,
            _ => 119 + 17 * n,
        };
        level as f64 / 255.0
    };
    let mut rgba = Vec::new();
    for m in 0..3 {
        for n in 0..8 {
            rgba.extend_from_slice(&[
                mode(m, n) as f32,
                mode(m, (n + 3) % 8) as f32,
                mode(m, (n + 5) % 8) as f32,
                1.0,
            ]);
        }
    }
    let img = ImageF32 {
        width: 8,
        height: 3,
        rgba,
    };
    let p = Params {
        source_colour_target: SOURCE_COLOUR_TARGET_SEGA_MEGA_DRIVE,
        source_colour_variant: 1,
        ..Params::default()
    };
    let out = run(&mut gpu, &img, &p);
    // The level index of a code in each mode: shadow n, normal 2 n, highlight 7 + n.
    let index = |m: usize, n: usize| [n, 2 * n, 7 + n][m];
    let mut worst = 0.0f64;
    for m in 0..3 {
        for n in 0..8usize {
            let px = out.pixel(n as u32, m as u32);
            for (c, code) in [n, (n + 3) % 8, (n + 5) % 8].into_iter().enumerate() {
                let want = f64::from(measured[index(m, code)]) / 255.0;
                let e = (f64::from(px[c]) - want).abs();
                worst = worst.max(e);
                assert!(e <= MAX_ERR, "mode {m} code {n} channel {c}: {} want {want}", px[c]);
            }
        }
    }
    println!("md shadow and highlight: max {worst:.2e}");
    // The first shadow step (17) is 29, the first highlight step (136) 144.
    assert!((f64::from(out.pixel(1, 0)[0]) * 255.0 - 29.0).abs() < 0.3);
    assert!((f64::from(out.pixel(1, 2)[0]) * 255.0 - 144.0).abs() < 0.3);
    // Kega Fusion keeps the eight normal levels: a picture value of 17 is half way from 0 to 33, so 26 on the line 0 to 52.
    let kega = Params {
        source_colour_variant: 2,
        ..p
    };
    let half = ImageF32 {
        width: 2,
        height: 1,
        rgba: vec![
            16.5 / 255.0,
            16.5 / 255.0,
            16.5 / 255.0,
            1.0,
            33.0 / 255.0,
            33.0 / 255.0,
            33.0 / 255.0,
            1.0,
        ],
    };
    let out = run(&mut gpu, &half, &kega);
    assert!((f64::from(out.pixel(0, 0)[1]) * 255.0 - 26.0).abs() < 0.3);
    assert!((f64::from(out.pixel(1, 0)[1]) * 255.0 - 52.0).abs() < 0.3);
    common::assert_no_validation_messages(&counter);
}

/// The CPC's 27 colours (every channel 0, 128 or 255) through the shader: the colour target puts the half level on 102 of
/// 255 on every channel and keeps black and full; the green monitor, on the variant with the half level at 0.40, makes the
/// 9 : 3 : 1 sum of the three levels (0, 0.40, 1) over 13 on all three channels (variant 1 gives i / 26 for the exact
/// levels 0, 0.5, 1, checked on the oracle's hand derivation in `targets.rs`).
#[test]
fn cpc_colours_come_out_at_the_half_level_of_the_gate_array() {
    let Some(mut gpu) = common::gpu_or_skip("cpc_colours_come_out_at_the_half_level") else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.enable_only(&["target"]).unwrap();
    let level = [0.0f64, 128.0 / 255.0, 1.0];
    let mut rgba = Vec::new();
    for r in 0..3 {
        for g in 0..3 {
            for b in 0..3 {
                rgba.extend_from_slice(&[level[r] as f32, level[g] as f32, level[b] as f32, 1.0]);
            }
        }
    }
    let img = ImageF32 {
        width: 27,
        height: 1,
        rgba,
    };
    let want = [0.0f64, 102.0 / 255.0, 1.0];
    let p = Params {
        source_colour_target: SOURCE_COLOUR_TARGET_AMSTRAD_CPC,
        source_colour_variant: 1,
        ..Params::default()
    };
    let out = run(&mut gpu, &img, &p);
    for r in 0..3usize {
        for g in 0..3usize {
            for b in 0..3usize {
                let px = out.pixel((9 * r + 3 * g + b) as u32, 0);
                for (c, w) in [want[r], want[g], want[b]].into_iter().enumerate() {
                    assert!(
                        (f64::from(px[c]) - w).abs() <= MAX_ERR,
                        "cpc {r}{g}{b} channel {c}: {} want {w}",
                        px[c]
                    );
                }
            }
        }
    }
    // The green monitor with the half level at 0.40: grey = (9 R + 3 G + B) / 13 of the levels 0, 0.40, 1, on all channels.
    let green = Params {
        source_colour_target: SOURCE_COLOUR_TARGET_AMSTRAD_CPC_GREEN_SCREEN,
        source_colour_variant: 2,
        ..Params::default()
    };
    let out = run(&mut gpu, &img, &green);
    let mut worst = 0.0f64;
    for r in 0..3usize {
        for g in 0..3usize {
            for b in 0..3usize {
                let want = (9.0 * [0.0, 0.4, 1.0][r] + 3.0 * [0.0, 0.4, 1.0][g] + [0.0, 0.4, 1.0][b]) / 13.0;
                let px = out.pixel((9 * r + 3 * g + b) as u32, 0);
                for (c, v) in px.iter().take(3).enumerate() {
                    let e = (f64::from(*v) - want).abs();
                    worst = worst.max(e);
                    assert!(e <= MAX_ERR, "green {r}{g}{b} channel {c}: {v} want {want}");
                }
            }
        }
    }
    println!("cpc colours: green max {worst:.2e}");
    common::assert_no_validation_messages(&counter);
}

/// The V9958's 512 palette colours (every channel at one of the 8 steps of an emulator that expands the 3 bits by bit
/// repetition, and of blueMSX's truncated division) through the shader: each channel comes out at its 5-bit DAC level
/// 0, 33, 74, 107, 148, 181, 222, 255 over 255, whatever the other two are.
#[test]
fn msx2_v9958_palette_comes_out_at_the_five_bit_dac_levels() {
    let Some(mut gpu) = common::gpu_or_skip("msx2_v9958_palette_comes_out_at_the_five_bit_dac_levels") else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.enable_only(&["target"]).unwrap();
    let want = [0u8, 33, 74, 107, 148, 181, 222, 255];
    for (variant, input) in [
        (1, [0u8, 36, 73, 109, 146, 182, 219, 255]),
        (2, [0u8, 36, 72, 109, 145, 182, 218, 255]),
    ] {
        let mut rgba = Vec::new();
        for r in 0..8 {
            for g in 0..8 {
                for b in 0..8 {
                    rgba.extend_from_slice(&[
                        f32::from(input[r]) / 255.0,
                        f32::from(input[g]) / 255.0,
                        f32::from(input[b]) / 255.0,
                        1.0,
                    ]);
                }
            }
        }
        let img = ImageF32 {
            width: 512,
            height: 1,
            rgba,
        };
        let p = Params {
            source_colour_target: SOURCE_COLOUR_TARGET_MSX2_V9958,
            source_colour_variant: variant,
            ..Params::default()
        };
        let out = run(&mut gpu, &img, &p);
        let mut worst = 0.0f64;
        for r in 0..8usize {
            for g in 0..8usize {
                for b in 0..8usize {
                    let px = out.pixel((64 * r + 8 * g + b) as u32, 0);
                    for (c, n) in [r, g, b].into_iter().enumerate() {
                        let w = f64::from(want[n]) / 255.0;
                        let e = (f64::from(px[c]) - w).abs();
                        worst = worst.max(e);
                        assert!(
                            e <= MAX_ERR,
                            "msx2 variant {variant} {r}{g}{b} channel {c}: {} want {w}",
                            px[c]
                        );
                    }
                }
            }
        }
        println!("msx2 v9958 variant {variant}: max {worst:.2e}");
    }
    common::assert_no_validation_messages(&counter);
}

/// The PC Engine lookup (shader kind 6) on all 512 colour codes in both expansions of an emulator (255 n / 7 rounded, 36 n),
/// against the oracle's table, for both variants; and a row of pixels that are not codes (one channel a few codes off a
/// level, a 5-bit expansion, a half level) that must come out as they went in. The 8-bit bytes of the converter's published
/// palette are checked on the CPU side (`targets.rs`); here the shader must read the same table at the same index.
#[test]
fn pc_engine_lookup_matches_the_oracle_on_every_code_and_passes_the_rest() {
    let Some(mut gpu) = common::gpu_or_skip("pc_engine_lookup_matches_the_oracle") else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.enable_only(&["target"]).unwrap();
    // Rows 0 to 7: the 512 codes with the repeated-bits expansion, 8 to 15 with 36 n, 16 and 17: pixels that are not codes.
    let (w, h) = (64usize, 18usize);
    let mut rgba = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let v = match y {
                0..=7 => pc_engine_code(y * w + x, false),
                8..=15 => pc_engine_code((y - 8) * w + x, true),
                // A channel eight codes off its level (the other two on levels; the levels are at least 36 apart, so it is
                // on none), then 5-bit expansions (codes 1 to 16: 8, 16, 24, 33 ... 132, none is a level), a half level, a
                // channel at 76.5 and one at 253.
                16 => {
                    let mut c = pc_engine_code(x * 8 + 3, false);
                    let k = x % 3;
                    c[k] += if c[k] > 0.5 { -8.0 } else { 8.0 } / 255.0;
                    c
                }
                _ => match x % 4 {
                    0 => [f64::from((((x / 4) + 1) as u32) << 3 | (((x / 4) + 1) as u32) >> 2) / 255.0; 3],
                    1 => [0.5, 0.5, 0.5],
                    2 => [pc_engine_code(x, false)[0], 0.3, pc_engine_code(x, false)[2]],
                    _ => [1.0, 1.0, 253.0 / 255.0],
                },
            };
            rgba.extend_from_slice(&[v[0] as f32, v[1] as f32, v[2] as f32, 1.0]);
        }
    }
    let img = ImageF32 {
        width: w as u32,
        height: h as u32,
        rgba,
    };
    for variant in [1, 2] {
        let p = Params {
            source_colour_target: SOURCE_COLOUR_TARGET_NEC_PC_ENGINE_COMPOSITE,
            source_colour_variant: variant,
            ..Params::default()
        };
        let out = run(&mut gpu, &img, &p);
        assert!(!gpu.last_skipped_slots().contains(&"target"));
        let t = transform(SOURCE_COLOUR_TARGET_NEC_PC_ENGINE_COMPOSITE, variant);
        let Transform::Lut(lut) = t else { panic!("lut") };
        let (mut max, mut replaced, mut passed) = (0.0_f64, 0, 0);
        for y in 0..h as u32 {
            for x in 0..w as u32 {
                let i = img.pixel(x, y);
                let px = [f64::from(i[0]), f64::from(i[1]), f64::from(i[2])];
                let want = t.apply(px);
                let index = crt_model::target::lut_index(px);
                // The oracle's own reading of the picture: the first 16 rows are all codes, the last two none.
                assert_eq!(index.is_some(), y < 16, "variant {variant} pixel ({x}, {y}): {px:?}");
                if let Some(index) = index {
                    assert_eq!(want, lut.colours[index]);
                    replaced += 1;
                } else {
                    assert_eq!(want, px);
                    passed += 1;
                }
                let g = out.pixel(x, y);
                for c in 0..3 {
                    let e = (f64::from(g[c]) - want[c]).abs();
                    max = max.max(e);
                    assert!(
                        e <= MAX_ERR,
                        "variant {variant} ({x}, {y}) channel {c}: {} want {}",
                        g[c],
                        want[c]
                    );
                }
            }
        }
        assert_eq!((replaced, passed), (1024, 128));
        println!("pc engine variant {variant}: max {max:.2e} over {replaced} codes and {passed} other pixels");
        // White and black of the table, and the grey diagonal (n, n, n) at the first variant: 148 of 255 for 4.
        assert!(out
            .pixel(63, 7)
            .iter()
            .take(3)
            .all(|c| (f64::from(*c) - 1.0).abs() <= MAX_ERR));
        assert!(out.pixel(0, 0).iter().take(3).all(|c| f64::from(*c).abs() <= MAX_ERR));
        if variant == 1 {
            let grey = out.pixel(36, 4);
            assert!(
                (0..3).all(|c| (f64::from(grey[c]) * 255.0 - 148.0).abs() < 0.6),
                "{grey:?}"
            );
        }
    }
    common::assert_no_validation_messages(&counter);
}

/// The Virtual Boy: Mednafen's four levels of the on-times 0, A, 2A, 3A (BRTC = 0, BRTB = 2 A) are the greys
/// (k / 3)^(1 / 2.2) x 240 / 255 (its 1 / 2.2 encode and its default tint 0xF0F0F0, hand derivation). Through the shader the
/// red is that value, its linear light (the 2.2 power) is in the ratio 0 : 1 : 2 : 3, and green and blue are 0; the red ramp
/// variant leaves the same red and drops a green and a blue part.
#[test]
fn virtual_boy_levels_come_out_as_red_light_proportional_to_the_on_time() {
    let Some(mut gpu) = common::gpu_or_skip("virtual_boy_levels_come_out_as_red_light") else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.enable_only(&["target"]).unwrap();
    let tint = 240.0f64 / 255.0;
    let grey: Vec<f64> = (0..4).map(|k| (f64::from(k) / 3.0).powf(1.0 / 2.2) * tint).collect();
    let mut rgba = Vec::new();
    for g in &grey {
        rgba.extend_from_slice(&[*g as f32; 3]);
        rgba.push(1.0);
    }
    // The same levels as a red picture with some green and blue in the tint.
    for g in &grey {
        rgba.extend_from_slice(&[*g as f32, 0.4, 0.2, 1.0]);
    }
    let img = ImageF32 {
        width: 8,
        height: 1,
        rgba,
    };
    let p = |variant| Params {
        source_colour_target: SOURCE_COLOUR_TARGET_NINTENDO_VIRTUAL_BOY,
        source_colour_variant: variant,
        ..Params::default()
    };
    let grey_out = run(&mut gpu, &img, &p(1));
    let mut light = Vec::new();
    for (k, g) in grey.iter().enumerate() {
        let px = grey_out.pixel(k as u32, 0);
        assert!(
            (f64::from(px[0]) - g).abs() <= MAX_ERR,
            "level {k}: red {} want {g}",
            px[0]
        );
        assert!(
            px[1].abs() <= 1e-6 && px[2].abs() <= 1e-6,
            "level {k}: green or blue {px:?}"
        );
        light.push(f64::from(px[0]).max(0.0).powf(2.2));
    }
    // Light ratios 0 : 1 : 2 : 3 (the FP16 store and the power move them by a few parts in a thousand).
    assert!(light[0] <= 1e-6);
    for k in 1..4 {
        let r = light[k] / light[1];
        println!("virtual boy level {k}: light ratio {r:.4}");
        assert!((r - k as f64).abs() <= 0.02 * k as f64, "level {k}: ratio {r}");
    }
    // The red ramp variant: the red as it is, no green or blue.
    let red_out = run(&mut gpu, &img, &p(2));
    for (k, g) in grey.iter().enumerate() {
        let px = red_out.pixel(4 + k as u32, 0);
        assert!((f64::from(px[0]) - g).abs() <= MAX_ERR, "red level {k}: {px:?}");
        assert!(px[1].abs() <= 1e-6 && px[2].abs() <= 1e-6, "red level {k}: {px:?}");
    }
    common::assert_no_validation_messages(&counter);
}

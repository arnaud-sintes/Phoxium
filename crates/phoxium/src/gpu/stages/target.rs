//! Slot `target` (voltage domain, native -> native, the head of the chain): the source colour target, the colours
//! of the source as the device it came from would have shown them (`shaders/target.comp`, parameter
//! `source_colour_target` of `params/source.toml`, data files `presets/targets/`, oracle `crt_model::target`, tests
//! `tests/stage_target.rs`).
//!
//! The CPU turns the target's transform into the shader's constants ([`plan`], in f64) and pushes them. The slot is an
//! identity, and skipped, while the target is `none`.

use ash::vk;
use crt_model::target::{Transform, CURVE_LEN, DAC_CHANNEL_LEN};

use super::{ComputePass, Domain, Registration, Stage};
use crate::gpu::{Context, FrameConstants};
use crate::params::Params;
use crate::Res;

const SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/target.comp.spv"));

/// Registration of the slot (see [`super::CHAIN`]).
pub(in crate::gpu) const SLOT: Registration = Registration {
    name: "target",
    input: Domain::Native,
    output: Domain::Native,
    new,
};

fn new() -> Box<dyn Stage> {
    Box::<TargetStage>::default()
}

const KIND_MATRIX: i32 = 0;
const KIND_CURVE: i32 = 1;
const KIND_PALETTE: i32 = 2;
const KIND_DAC: i32 = 3;
const KIND_SIGNAL: i32 = 4;
const KIND_DAC15: i32 = 5;
const KIND_LUT: i32 = 6;

/// The push constants of `target.comp` (std430, 100 bytes): the matrix row by row and its scalars, the curve (or the
/// bytes of a palette) packed four entries to a word, the bits per channel of the device code (0 = no recovery) and the
/// kind (0 matrix, 1 curve, 2 palette, 3 dac, 4 signal, 5 dac15: a dac of 9 to 15 points, 6 lut: 512 colours by 9-bit code).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Push {
    k: [[f32; 3]; 3],
    input_range: f32,
    input_gamma: f32,
    clamp_sum: f32,
    divisor: f32,
    inv_out_gamma: f32,
    out_scale: f32,
    table: [u32; CURVE_LEN / 4],
    bits: i32,
    kind: i32,
}

/// Whether the slot does nothing for `p`: no target, or a value with no data file.
pub(crate) fn is_identity(p: &Params) -> bool {
    crate::targets::for_value(p.source_colour_target).is_none()
}

/// The recovery width pushed for `bits` (0 when there is nothing to recover).
fn recovery_bits(bits: u32) -> i32 {
    if bits < 8 {
        bits as i32
    } else {
        0
    }
}

/// The push constants for `p`; a pass-through matrix when there is no target.
pub(super) fn plan(p: &Params) -> Push {
    let identity = Push {
        k: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        input_range: 1.0,
        input_gamma: 1.0,
        clamp_sum: f32::MAX,
        divisor: 1.0,
        inv_out_gamma: 1.0,
        out_scale: 1.0,
        kind: KIND_MATRIX,
        ..Push::default()
    };
    let Some(t) = crate::targets::for_value(p.source_colour_target) else {
        return identity;
    };
    // The variant (1 based) of a target that has some: its palette, or the emulator whose picture is captured.
    match t.select(p.source_colour_variant) {
        Transform::Matrix(m) => Push {
            k: m.rows.map(|row| row.map(|w| w as f32)),
            input_range: m.input_range as f32,
            input_gamma: m.input_gamma as f32,
            clamp_sum: if m.clamp.is_finite() { m.clamp as f32 } else { f32::MAX },
            divisor: m.divisor as f32,
            inv_out_gamma: (1.0 / m.output_gamma) as f32,
            out_scale: m.output_scale as f32,
            bits: recovery_bits(m.input_bits),
            ..identity
        },
        Transform::Curve(c) => {
            let mut table = [0u32; CURVE_LEN / 4];
            for (i, v) in c.table.iter().enumerate() {
                table[i / 4] |= u32::from(*v) << (8 * (i % 4));
            }
            Push {
                table,
                out_scale: (c.scale / c.output_max) as f32,
                bits: recovery_bits(t.transform.input_bits()),
                kind: KIND_CURVE,
                ..identity
            }
        }
        Transform::Signal(sig) => {
            // The hardware and reference colours travel in the frame uniform buffer (`target_table`,
            // `targets::frame_table`); here the distance threshold in `clamp_sum` and the count in word 0.
            let mut table = [0u32; CURVE_LEN / 4];
            table[0] = sig.count as u32;
            Push {
                clamp_sum: sig.threshold as f32,
                table,
                bits: 0,
                kind: KIND_SIGNAL,
                ..identity
            }
        }
        Transform::Lut(_) => {
            // The 512 colours travel in the frame uniform buffer (`target_table`, `targets::frame_table`, entry `G x 64 + R x
            // 8 + B`); nothing else is pushed, and the shader recovers the 3-bit levels itself (`bits` stays 0).
            Push {
                bits: 0,
                kind: KIND_LUT,
                ..identity
            }
        }
        Transform::Dac(d) if d.len() > DAC_CHANNEL_LEN => {
            // More than eight points (the Mega Drive's 15 levels): the 15 input levels, then the 15 output codes, the same
            // on the three channels (a file with an output of its own for green or blue is refused above eight points),
            // one byte each, four to a word: 30 of the 32 bytes.
            debug_assert!(d.output_green.is_none() && d.output_blue.is_none());
            let mut table = [0u32; CURVE_LEN / 4];
            for (i, v) in d.input.iter().chain(&d.output).enumerate() {
                table[i / 4] |= u32::from(*v) << (8 * (i % 4));
            }
            Push {
                table,
                out_scale: (1.0 / d.output_max) as f32,
                bits: 0,
                kind: KIND_DAC15,
                ..identity
            }
        }
        Transform::Dac(d) => {
            // Eight input levels, then the eight output codes of red, of green and of blue (the same eight three
            // times for a DAC with no channel of its own), one byte each, four to a word. A DAC of 8 points or fewer
            // repeats its last point in the arrays, so the first eight entries hold all of it.
            let mut table = [0u32; CURVE_LEN / 4];
            let bytes = d
                .input
                .iter()
                .take(DAC_CHANNEL_LEN)
                .chain(d.output_of(0).iter().take(DAC_CHANNEL_LEN))
                .chain(d.output_of(1).iter().take(DAC_CHANNEL_LEN))
                .chain(d.output_of(2).iter().take(DAC_CHANNEL_LEN));
            for (i, v) in bytes.enumerate() {
                table[i / 4] |= u32::from(*v) << (8 * (i % 4));
            }
            Push {
                table,
                out_scale: (1.0 / d.output_max) as f32,
                bits: 0,
                kind: KIND_DAC,
                ..identity
            }
        }
        Transform::Palette(pal) => {
            // Twelve bytes, colour by colour, lightest first, channel by channel, four to a word.
            let mut table = [0u32; CURVE_LEN / 4];
            for (i, v) in pal.colours.iter().flatten().enumerate() {
                table[i / 4] |= u32::from(*v) << (8 * (i % 4));
            }
            Push {
                table,
                out_scale: (1.0 / pal.output_max) as f32,
                bits: 0,
                kind: KIND_PALETTE,
                ..identity
            }
        }
    }
}

/// The source colour target stage (see the module documentation).
#[derive(Default)]
struct TargetStage {
    pass: ComputePass,
    push: Push,
}

impl Stage for TargetStage {
    fn pass(&self) -> Option<&ComputePass> {
        Some(&self.pass)
    }

    unsafe fn create(&mut self, d: &ash::Device) -> Res<()> {
        self.pass.create(d, SPV, size_of::<Push>() as u32)
    }

    /// No target: the exact identity, the pipeline skips the slot.
    fn is_identity(&self, params: &Params, _fc: &FrameConstants) -> bool {
        is_identity(params)
    }

    unsafe fn prepare(&mut self, _ctx: &Context, _fc: &FrameConstants, params: &Params, runs: bool) -> Res<()> {
        if runs {
            self.push = plan(params);
        }
        Ok(())
    }

    unsafe fn record(&self, d: &ash::Device, cb: vk::CommandBuffer, set: vk::DescriptorSet, size: (u32, u32)) {
        // SAFETY: `Push` is `#[repr(C)]` plain data of four-byte fields (floats, words and ints), no padding.
        let bytes = std::slice::from_raw_parts((&self.push as *const Push).cast::<u8>(), size_of::<Push>());
        self.pass.dispatch(d, cb, set, bytes, size);
    }

    unsafe fn destroy(&mut self, d: &ash::Device) {
        self.pass.destroy(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{
        SOURCE_COLOUR_TARGET_GAME_BOY_ADVANCE, SOURCE_COLOUR_TARGET_GAME_BOY_COLOR,
        SOURCE_COLOUR_TARGET_SEGA_MEGA_DRIVE, SOURCE_COLOUR_TARGET_SUPER_GAME_BOY,
        SOURCE_COLOUR_TARGET_SUPER_NINTENDO_BLACK_BOOST,
    };

    /// The push block is 100 bytes in the shader's order.
    #[test]
    fn push_layout_matches_the_shader() {
        assert_eq!(size_of::<Push>(), 100);
        assert_eq!(std::mem::offset_of!(Push, out_scale), 56);
        assert_eq!(std::mem::offset_of!(Push, table), 60);
        assert_eq!(std::mem::offset_of!(Push, bits), 92);
        assert_eq!(std::mem::offset_of!(Push, kind), 96);
        assert!(size_of::<Push>() <= 128, "the guaranteed push constant size");
        let glsl = include_str!("../../../../../shaders/target.comp");
        let order = [
            "float k00;",
            "float k01;",
            "float k02;",
            "float k10;",
            "float k11;",
            "float k12;",
            "float k20;",
            "float k21;",
            "float k22;",
            "float input_range;",
            "float input_gamma;",
            "float clamp_sum;",
            "float divisor;",
            "float inv_out_gamma;",
            "float out_scale;",
            "uint table[8];",
            "int bits;",
            "int kind;",
        ];
        let pos: Vec<usize> = order
            .iter()
            .map(|m| glsl.find(m).unwrap_or_else(|| panic!("{m}")))
            .collect();
        assert!(pos.windows(2).all(|w| w[0] < w[1]), "push members out of order");
        assert!(glsl.contains("local_size_x = 8, local_size_y = 8"));
    }

    /// The colour table of a signal target (hardware colours, then the classification references from entry `SIGNAL_MAX`)
    /// is laid out as the shader reads it, and the frame uniform buffer with it stays within the 16384 bytes of
    /// `maxUniformBufferRange` that every Vulkan device guarantees (96 bytes of constants and 512 `vec4`: 8288 bytes).
    #[test]
    fn the_colour_table_layout_matches_the_shader_and_fits_the_guaranteed_uniform_range() {
        use crate::gpu::FRAME_TABLE_VEC4;
        use crt_model::target::SIGNAL_MAX;
        let glsl = include_str!("../../../../../shaders/target.comp");
        assert!(glsl.contains(&format!("const int TABLE_REFERENCES = {SIGNAL_MAX};")));
        assert!(glsl.contains("frame.target_table[TABLE_REFERENCES + i].rgb"));
        let common = include_str!("../../../../../shaders/include/common.glsl");
        assert!(common.contains(&format!("vec4 target_table[{FRAME_TABLE_VEC4}];")));
        assert_eq!(FRAME_TABLE_VEC4, 2 * SIGNAL_MAX);
        let bytes = size_of::<FrameConstants>() + 16 * FRAME_TABLE_VEC4;
        assert_eq!(bytes, 8288);
        assert!(bytes <= 16384, "{bytes}");
        // The table the CPU writes: the hardware colours at 0.., the references from SIGNAL_MAX, 16 bytes an entry.
        let p = Params {
            source_colour_target: crate::params::SOURCE_COLOUR_TARGET_ATARI_8BIT_GTIA,
            ..Params::default()
        };
        let table = crate::targets::frame_table(&p).expect("a signal target");
        assert_eq!(table.len(), SIGNAL_MAX + 256);
        assert!(table.len() <= FRAME_TABLE_VEC4);
        let Transform::Signal(s) = crate::targets::for_value(p.source_colour_target).unwrap().select(1) else {
            panic!("signal")
        };
        assert_eq!(table[0xA8][..3], s.colours[0xA8].map(|c| c as f32));
        assert_eq!(table[SIGNAL_MAX + 0xA8][..3], s.references[0xA8].map(|c| c as f32));
    }

    fn with(target: u32) -> Params {
        Params {
            source_colour_target: target,
            ..Params::default()
        }
    }

    /// The default is the identity; a target wakes the slot and pushes the constants of its file.
    #[test]
    fn the_slot_runs_only_with_a_target() {
        assert!(is_identity(&Params::default()));
        for t in [
            SOURCE_COLOUR_TARGET_GAME_BOY_COLOR,
            SOURCE_COLOUR_TARGET_GAME_BOY_ADVANCE,
            SOURCE_COLOUR_TARGET_SUPER_NINTENDO_BLACK_BOOST,
        ] {
            assert!(!is_identity(&with(t)));
        }
        let gbc = plan(&with(SOURCE_COLOUR_TARGET_GAME_BOY_COLOR));
        assert_eq!(
            (gbc.kind, gbc.bits, gbc.input_range, gbc.clamp_sum, gbc.divisor),
            (0, 5, 31.0, 960.0, 1020.0)
        );
        assert_eq!((gbc.input_gamma, gbc.inv_out_gamma, gbc.out_scale), (1.0, 1.0, 1.0));
        let gba = plan(&with(SOURCE_COLOUR_TARGET_GAME_BOY_ADVANCE));
        assert_eq!((gba.input_gamma, gba.divisor, gba.clamp_sum), (4.0, 255.0, f32::MAX));
        assert!((f64::from(gba.inv_out_gamma) - 1.0 / 2.2).abs() < 1e-7);
        assert!((f64::from(gba.out_scale) - 255.0 / 280.0).abs() < 1e-7);
    }

    /// The curve is packed four entries to a word, the first entry in the lowest byte.
    #[test]
    fn the_curve_is_packed_four_to_a_word() {
        let snes = plan(&with(SOURCE_COLOUR_TARGET_SUPER_NINTENDO_BLACK_BOOST));
        assert_eq!((snes.kind, snes.bits), (1, 5));
        // Entries 0 to 3 are 0x00, 0x01, 0x03, 0x06; the last word holds 0xe8, 0xf0, 0xf8, 0xff.
        assert_eq!(snes.table[0], 0x0603_0100);
        assert_eq!(snes.table[7], 0xfff8_f0e8);
        assert!((f64::from(snes.out_scale) - 1.0 / 255.0).abs() < 1e-9);
    }

    /// The Neo Geo curve: its ladder levels packed four to a word, the variant's scale in `out_scale` over `output_max`
    /// (1 / 255 for the normal variant, 0.557054 / 255 for the shadow), five-bit recovery.
    #[test]
    fn the_neo_geo_variant_scales_the_curve_output() {
        let neo = |variant: i32| Params {
            source_colour_variant: variant,
            ..with(crate::params::SOURCE_COLOUR_TARGET_SNK_NEO_GEO)
        };
        let normal = plan(&neo(1));
        assert_eq!((normal.kind, normal.bits), (1, 5));
        // Entries 0 to 3 are 0, 8, 14, 22; the last word holds 233, 241, 247, 255.
        assert_eq!(normal.table[0], 22 << 24 | 14 << 16 | 8 << 8);
        assert_eq!(normal.table[7], 255 << 24 | 247 << 16 | 241 << 8 | 233);
        assert!((f64::from(normal.out_scale) - 1.0 / 255.0).abs() < 1e-9);
        let shadow = plan(&neo(3));
        assert_eq!(shadow.table, normal.table, "the scale is not baked into the table");
        assert!((f64::from(shadow.out_scale) - 0.557_054 / 255.0).abs() < 1e-8);
        assert_eq!(plan(&neo(99)), normal, "past the last variant: the first");
    }

    /// A palette is packed as its twelve bytes, colour by colour, channel by channel, four to a word; the variant number
    /// picks the palette (1 = 1-A, 9 = 2-A) and a number past the last one gives the first. Hand derivation of 2-A:
    /// 0x533E is red 30, green 25, blue 20 and 0x2638 has red 24, so the first word is 30, 25, 20, 24 from the low byte.
    #[test]
    fn a_palette_is_packed_as_twelve_bytes_and_the_number_picks_it() {
        let with_variant = |n: i32| Params {
            source_colour_variant: n,
            ..with(SOURCE_COLOUR_TARGET_SUPER_GAME_BOY)
        };
        let a = plan(&with_variant(1));
        assert_eq!((a.kind, a.bits), (2, 0));
        // 1-A: colours (31, 29, 25), (27, 18, 9), (21, 5, 4), (6, 3, 10).
        assert_eq!(a.table[0], 31 | 29 << 8 | 25 << 16 | 27 << 24);
        assert_eq!(a.table[2], 4 | 6 << 8 | 3 << 16 | 10 << 24);
        assert_eq!(a.table[3..], [0; 5]);
        assert!((f64::from(a.out_scale) - 1.0 / 31.0).abs() < 1e-9);
        assert_eq!(plan(&with_variant(9)).table[0], 30 | 25 << 8 | 20 << 16 | 24 << 24);
        assert_eq!(
            plan(&with_variant(99)).table,
            a.table,
            "past the last palette: the first"
        );
        assert_eq!(plan(&with_variant(0)).table, a.table, "below 1: the first");
        assert_ne!(plan(&with_variant(2)).table, a.table);
    }

    /// An 8-bit palette (the Game Boy LCDs) is packed the same way and scaled by 1 / 255. Hand derivation of variant 1
    /// (BGB reality DMG, lightest 0x759833 = 117, 152, 51, then 0x588F51 = 88, 143, 81, 0x3B7560 = 59, 117, 96, and the
    /// darkest 0x2E615A = 46, 97, 90): the first word is 117, 152, 51, 88 from the low byte.
    #[test]
    fn an_eight_bit_palette_is_packed_and_scaled_by_255() {
        let p = Params {
            source_colour_variant: 1,
            ..with(crate::params::SOURCE_COLOUR_TARGET_GAME_BOY_LCD)
        };
        let a = plan(&p);
        assert_eq!((a.kind, a.bits), (2, 0));
        assert_eq!(a.table[0], 117 | 152 << 8 | 51 << 16 | 88 << 24);
        assert_eq!(a.table[1], 143 | 81 << 8 | 59 << 16 | 117 << 24);
        assert_eq!(a.table[2], 96 | 46 << 8 | 97 << 16 | 90 << 24);
        assert!((f64::from(a.out_scale) - 1.0 / 255.0).abs() < 1e-9);
    }

    /// The Mega Drive: variant 1 is Genesis Plus GX (the default, 17 per level on the 15 levels): kind 5, the 15 input
    /// levels then the 15 output codes, one byte each, the same on the three channels. Variant 2 Kega Fusion (steps of 33)
    /// and 3 Regen (32) keep the eight normal levels: kind 3, eight input levels, then eight output codes (three times); a
    /// number past the last gives the first.
    #[test]
    fn the_dac_is_packed_as_input_levels_then_output_codes() {
        let md = |variant: i32| Params {
            source_colour_variant: variant,
            ..with(SOURCE_COLOUR_TARGET_SEGA_MEGA_DRIVE)
        };
        let word = |a: [u32; 4]| a[0] | a[1] << 8 | a[2] << 16 | a[3] << 24;
        let gpgx = plan(&md(1));
        assert_eq!((gpgx.kind, gpgx.bits), (5, 0));
        // Bytes 0 to 14: the inputs 0, 17, ... 238; bytes 15 to 29: the outputs 0, 29, 52, 70, 87, 101, 116, 130, 144, 158,
        // 172, 187, 206, 228, 255; bytes 30 and 31 are unused.
        assert_eq!(
            gpgx.table,
            [
                word([0, 17, 34, 51]),
                word([68, 85, 102, 119]),
                word([136, 153, 170, 187]),
                word([204, 221, 238, 0]),
                word([29, 52, 70, 87]),
                word([101, 116, 130, 144]),
                word([158, 172, 187, 206]),
                word([228, 255, 0, 0]),
            ]
        );
        assert!((f64::from(gpgx.out_scale) - 1.0 / 255.0).abs() < 1e-9);
        let kega = plan(&md(2));
        assert_eq!((kega.kind, kega.bits), (3, 0));
        assert_eq!(kega.table[0], 0x6342_2100, "0, 33, 66, 99");
        // Outputs 0, 52, 87, 116 then 144, 172, 206, 255.
        assert_eq!(kega.table[2..4], [0x7457_3400, 0xFFCE_AC90]);
        // A DAC with no green or blue of its own repeats the red outputs for both: the same map on the three channels.
        assert_eq!(kega.table[4..6], kega.table[2..4]);
        assert_eq!(kega.table[6..], kega.table[2..4]);
        assert!((f64::from(kega.out_scale) - 1.0 / 255.0).abs() < 1e-9);
        let regen = plan(&md(3));
        assert_eq!((regen.kind, regen.table[0]), (3, 0x6040_2000), "0, 32, 64, 96");
        assert_eq!(regen.table[2..], kega.table[2..], "the same measured outputs");
        assert_eq!(plan(&md(20)).table, gpgx.table, "past the last emulator: the first");
    }

    /// The shader keeps its two dac layouts apart: kind 5 reads 15 inputs then 15 outputs.
    #[test]
    fn the_shader_reads_the_fifteen_point_dac_layout() {
        let glsl = include_str!("../../../../../shaders/target.comp");
        assert!(glsl.contains("else if (pc.kind == 5)"));
        assert!(glsl.contains("table_byte(15 + i), table_byte(16 + i)"));
        assert!(glsl.contains("clamp(x * 255.0, table_byte(0), table_byte(14))"));
        assert_eq!(super::KIND_DAC15, 5);
    }

    /// The lookup target (the PC Engine's 512 colours): the push constants carry only the kind (6, no device code recovery by the
    /// push), the colours are the frame table (entry `G x 64 + R x 8 + B`, no more entries than the table holds, so that the
    /// frame uniform buffer stays at 8288 bytes), and the shader recovers the level and reads that table.
    #[test]
    fn the_lookup_target_pushes_its_kind_and_reads_the_frame_table() {
        use crate::gpu::FRAME_TABLE_VEC4;
        use crt_model::target::LUT_LEN;
        let p = with(crate::params::SOURCE_COLOUR_TARGET_NEC_PC_ENGINE_COMPOSITE);
        assert!(!is_identity(&p));
        let push = plan(&p);
        assert_eq!((push.kind, push.bits, push.table), (6, 0, [0; CURVE_LEN / 4]));
        assert_eq!(push.k, plan(&Params::default()).k, "no matrix");
        assert_eq!(super::KIND_LUT, 6);
        const { assert!(LUT_LEN <= FRAME_TABLE_VEC4) };
        let table = crate::targets::frame_table(&p).expect("a lookup target");
        assert_eq!(table.len(), LUT_LEN);
        assert_eq!(table[511][..3], [1.0, 1.0, 1.0], "white is white");
        let glsl = include_str!("../../../../../shaders/target.comp");
        assert!(glsl.contains("else if (pc.kind == 6)"));
        assert!(glsl.contains("c = index >= 0 ? frame.target_table[index].rgb : x;"));
        assert!(glsl.contains("ivec3 n = (ci + 18) / 36;"));
        assert!(glsl.contains("equal(ci, (n << 5) | (n << 2) | (n >> 1))"));
        assert!(glsl.contains("return n.g * 64 + n.r * 8 + n.b;"));
    }

    /// The Master System: four codes with the last point repeated, and the blue of its own in the last eight bytes. Variant
    /// 1 (the first VDP with TmEE's levels): input 0, 85, 170, 255 (then 255 four times), red and green 0, 90, 173, 255,
    /// blue 0, 110, 173, 255; variant 3 (the second VDP): 0, 89, 174, 255 on the three channels.
    #[test]
    fn the_master_system_dac_packs_a_blue_of_its_own() {
        let sms = |variant: i32| Params {
            source_colour_variant: variant,
            ..with(crate::params::SOURCE_COLOUR_TARGET_SEGA_MASTER_SYSTEM)
        };
        let word = |a: [u32; 4]| a[0] | a[1] << 8 | a[2] << 16 | a[3] << 24;
        let first = plan(&sms(1));
        assert_eq!((first.kind, first.bits), (3, 0));
        assert_eq!(first.table[0], word([0, 85, 170, 255]));
        assert_eq!(first.table[1], word([255; 4]), "the padding repeats the last level");
        assert_eq!(first.table[2], word([0, 90, 173, 255]));
        assert_eq!(first.table[3], word([255; 4]));
        assert_eq!(first.table[4], first.table[2], "green is red's");
        assert_eq!(first.table[6], word([0, 110, 173, 255]), "blue's own");
        assert_eq!(first.table[7], word([255; 4]));
        let second = plan(&sms(3));
        assert_eq!(second.table[2], word([0, 89, 174, 255]));
        assert_eq!(second.table[4..6], second.table[2..4]);
        assert_eq!(second.table[6..], second.table[2..4], "no blue of its own");
    }
}

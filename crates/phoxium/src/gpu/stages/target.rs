//! Slot `target` (voltage domain, native -> native, the head of the chain): the source colour target, the colours
//! of the source as the device it came from would have shown them (`shaders/target.comp`, parameter
//! `source_colour_target` of `params/source.toml`, data files `presets/targets/`, oracle `crt_model::target`, tests
//! `tests/stage_target.rs`).
//!
//! The CPU turns the target's transform into the shader's constants ([`plan`], in f64) and pushes them. The slot is an
//! identity, and skipped, while the target is `none`.

use ash::vk;
use crt_model::target::{Transform, CURVE_LEN};

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

/// The push constants of `target.comp` (std430, 100 bytes): the matrix row by row and its scalars, the curve packed four
/// entries to a word, the bits per channel of the device code (0 = no recovery) and the kind.
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
    match t.transform {
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
                out_scale: (1.0 / c.output_max) as f32,
                bits: recovery_bits(t.transform.input_bits()),
                kind: KIND_CURVE,
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
}

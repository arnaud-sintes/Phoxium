//! Slot `tape` (voltage domain, amp -> amp, between the analogue impairments and the TV controls): a VHS tape (soft
//! luma, smeared and lagging colour, head-switching noise, dropouts) and the vertical roll (`shaders/tape.comp`,
//! parameters `params/tape.toml`, oracle `crt_model::tape`, tests `tests/stage_tape.rs`), optional, off by default.
//!
//! The CPU builds the oracle's [`Tape`] from the parameters ([`model`]) and pushes the per-frame constants
//! ([`plan`]: the step flags the oracle's conditions give, the low-pass half widths in samples, the roll offset of
//! the frame, the head band's first line). The slot is an identity, and skipped, while `tape_enable` is off or
//! every amount is 0.

use ash::vk;
use crt_model::tape::Tape;

use super::{ComputePass, Domain, Registration, Stage};
use crate::gpu::{Context, FrameConstants};
use crate::params::Params;
use crate::Res;

const SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tape.comp.spv"));

/// Registration of the slot (see [`super::CHAIN`]).
pub(in crate::gpu) const SLOT: Registration = Registration {
    name: "tape",
    input: Domain::Amp,
    output: Domain::Amp,
    new,
};

fn new() -> Box<dyn Stage> {
    Box::<TapeStage>::default()
}

const FLAG_FILTER: u32 = 1;
const FLAG_HEAD: u32 = 2;
const FLAG_DROPOUT: u32 = 4;
const FLAG_ROLL: u32 = 8;

/// The push constants of `tape.comp` (std430, 56 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Push {
    ry: i32,
    rc: i32,
    chroma_shift: f32,
    head_first: f32,
    head_lines: f32,
    head_shift_px: f32,
    head_noise: f32,
    dropout: f32,
    dropout_length_px: f32,
    roll_offset: i32,
    roll_bar: f32,
    flags: u32,
    width_px: f32,
    /// The source frame of this moment (`FrameConstants::source_frame` at `impair_field_rate_hz`): the frame counter of
    /// the hash draws (head tear, head noise, dropouts).
    source_frame: u32,
}

/// The oracle's settings for `params`.
pub(crate) fn model(p: &Params) -> Tape {
    Tape {
        field_rate_hz: f64::from(p.impair_field_rate_hz).max(1.0),
        luma_blur_px: f64::from(p.tape_luma_blur_px).max(0.0),
        chroma_blur_px: f64::from(p.tape_chroma_blur_px).max(0.0),
        chroma_shift_px: f64::from(p.tape_chroma_shift_px),
        head_lines: f64::from(p.tape_head_lines).max(0.0),
        head_shift_px: f64::from(p.tape_head_shift_px).max(0.0),
        head_noise: f64::from(p.tape_head_noise).max(0.0),
        dropout: f64::from(p.tape_dropout).clamp(0.0, 1.0),
        dropout_length_px: f64::from(p.tape_dropout_length_px).max(0.0),
        roll_hz: f64::from(p.tape_roll_hz).max(0.0),
        roll_bar_lines: f64::from(p.tape_roll_bar_lines).max(0.0),
    }
}

/// Whether the slot does nothing for `p`: off, or every amount at 0 (for `k` samples per source pixel).
pub(crate) fn is_identity(p: &Params, k: usize) -> bool {
    !p.tape_enable || model(p).is_neutral(k)
}

/// The push constants of the frame `fc` for `params`.
pub(super) fn plan(p: &Params, fc: &FrameConstants) -> Push {
    let m = model(p);
    let k = fc.amp_k.max(1) as usize;
    let lines = fc.native_size[1].max(1) as u32;
    // Real time: the source frames that have passed at the field rate, whatever the display rate.
    let frame = fc.source_frame(m.field_rate_hz);
    let mut flags = 0;
    if m.has_filter(k) {
        flags |= FLAG_FILTER;
    }
    if m.has_head() {
        flags |= FLAG_HEAD;
    }
    if m.dropout > 0.0 {
        flags |= FLAG_DROPOUT;
    }
    if m.roll_hz != 0.0 {
        flags |= FLAG_ROLL;
    }
    Push {
        ry: Tape::radius(m.luma_blur_px, k) as i32,
        rc: Tape::radius(m.chroma_blur_px, k) as i32,
        chroma_shift: (m.chroma_shift_px * k as f64) as f32,
        head_first: (f64::from(lines) - m.head_lines) as f32,
        head_lines: m.head_lines.max(1.0) as f32,
        head_shift_px: m.head_shift_px as f32,
        head_noise: m.head_noise as f32,
        dropout: m.dropout as f32,
        dropout_length_px: m.dropout_length_px as f32,
        roll_offset: m.roll_offset(frame, lines) as i32,
        roll_bar: m.roll_bar_lines as f32,
        flags,
        width_px: fc.native_size[0].max(1) as f32,
        source_frame: frame,
    }
}

/// The tape stage (see the module documentation).
#[derive(Default)]
struct TapeStage {
    pass: ComputePass,
    push: Push,
}

impl Stage for TapeStage {
    fn pass(&self) -> Option<&ComputePass> {
        Some(&self.pass)
    }

    unsafe fn create(&mut self, d: &ash::Device) -> Res<()> {
        self.pass.create(d, SPV, size_of::<Push>() as u32)
    }

    /// Off, or every amount at 0: the exact identity, the pipeline skips the slot.
    fn is_identity(&self, params: &Params, fc: &FrameConstants) -> bool {
        is_identity(params, fc.amp_k.max(1) as usize)
    }

    unsafe fn prepare(&mut self, _ctx: &Context, fc: &FrameConstants, params: &Params, runs: bool) -> Res<()> {
        if runs {
            self.push = plan(params, fc);
        }
        Ok(())
    }

    unsafe fn record(&self, d: &ash::Device, cb: vk::CommandBuffer, set: vk::DescriptorSet, size: (u32, u32)) {
        // SAFETY: `Push` is `#[repr(C)]` plain data of fourteen four-byte fields, no padding.
        let bytes = std::slice::from_raw_parts((&self.push as *const Push).cast::<u8>(), size_of::<Push>());
        self.pass.dispatch(d, cb, set, bytes, size);
    }

    unsafe fn destroy(&mut self, d: &ash::Device) {
        self.pass.destroy(d);
    }
}

#[cfg(test)]
mod tests {
    use super::super::AMP_K;
    use super::*;

    /// The push block is 56 bytes in the shader's order, and the seeds of the shader are the oracle's.
    #[test]
    fn push_layout_and_seeds_match_the_shader() {
        assert_eq!(size_of::<Push>(), 56);
        assert_eq!(std::mem::offset_of!(Push, flags), 44);
        assert_eq!(std::mem::offset_of!(Push, source_frame), 52);
        let glsl = include_str!("../../../../../shaders/tape.comp");
        let order = [
            "int ry;",
            "int rc;",
            "float chroma_shift;",
            "float head_first;",
            "float head_lines;",
            "float head_shift_px;",
            "float head_noise;",
            "float dropout;",
            "float dropout_length_px;",
            "int roll_offset;",
            "float roll_bar;",
            "uint flags;",
            "float width_px;",
            "uint source_frame;",
        ];
        let pos: Vec<usize> = order
            .iter()
            .map(|m| glsl.find(m).unwrap_or_else(|| panic!("{m}")))
            .collect();
        assert!(pos.windows(2).all(|w| w[0] < w[1]), "push members out of order");
        assert!(glsl.contains("local_size_x = 8, local_size_y = 8"));
        use crt_model::tape as t;
        for (name, value) in [
            ("SEED_HEAD", t::SEED_HEAD),
            ("SEED_HEAD_NOISE", t::SEED_HEAD_NOISE),
            ("SEED_DROPOUT", t::SEED_DROPOUT),
            ("MIX_LENGTH", t::MIX_LENGTH),
            ("MIX_POSITION", t::MIX_POSITION),
        ] {
            let line = format!("const uint {name} = 0x{value:08x}u;");
            assert!(glsl.contains(&line), "{line}");
        }
    }

    /// Off by default; on with the defaults the slot runs (a soft picture and lagging colour); on with every
    /// amount at 0, or off with amounts, it is an identity.
    #[test]
    fn the_slot_runs_only_with_the_module_on_and_an_amount() {
        let p = Params::default();
        let k = AMP_K as usize;
        assert!(!p.tape_enable && is_identity(&p, k));
        let mut on = p;
        on.tape_enable = true;
        assert!(!is_identity(&on, k), "the defaults are mild amounts, not zeros");
        let mut zero = on;
        zero.tape_luma_blur_px = 0.0;
        zero.tape_chroma_blur_px = 0.0;
        zero.tape_chroma_shift_px = 0.0;
        zero.tape_head_lines = 0.0;
        zero.tape_dropout = 0.0;
        zero.tape_roll_hz = 0.0;
        assert!(is_identity(&zero, k));
        let mut rolled = zero;
        rolled.tape_roll_hz = 0.2;
        assert!(!is_identity(&rolled, k));
    }

    /// The plan: the half widths in samples (K per pixel), the colour delay in samples, the roll offset of the frame
    /// from the f64 phase, the head band's first line and the source pixels per line.
    #[test]
    fn the_plan_is_in_amp_samples_and_per_frame() {
        let p = Params {
            tape_enable: true,
            tape_luma_blur_px: 0.5,
            tape_chroma_blur_px: 2.0,
            tape_chroma_shift_px: 0.75,
            tape_head_lines: 6.0,
            tape_roll_hz: 0.5,
            impair_field_rate_hz: 60.0,
            ..Params::default()
        };
        let fc = FrameConstants::new((320, 240), (1280, 960), false).at_source_frame(30, 60.0);
        let push = plan(&p, &fc);
        assert_eq!(push.source_frame, 30);
        let k = fc.amp_k;
        assert_eq!((push.ry, push.rc), ((0.5 * k as f32).round() as i32, 2 * k));
        assert_eq!(push.chroma_shift, 0.75 * k as f32);
        assert_eq!((push.head_first, push.width_px, push.roll_offset), (234.0, 320.0, 60));
        assert_eq!(push.flags, FLAG_FILTER | FLAG_HEAD | FLAG_DROPOUT | FLAG_ROLL);
    }
}

//! Slot `tv` (voltage domain, amp -> amp, between the analogue impairments and the gun): the knobs of a television
//! (sharpness, tint, colour, black and white level) and the refresh flicker (`shaders/tv.comp`, parameters
//! `params/tv.toml`, oracle `crt_model::tv`, tests `tests/stage_tv.rs`).
//!
//! The CPU builds the oracle's [`Tv`] from the parameters ([`model`]) and pushes the per-frame constants ([`plan`]:
//! the step flags the oracle's conditions give, the tint's cosine and sine, the flicker's gain at the frame index,
//! computed in f64 so that the frame counter never loses precision). The slot is an identity, and skipped, while
//! every knob is neutral.

use ash::vk;
use crt_model::tv::Tv;

use super::{ComputePass, Domain, Registration, Stage};
use crate::gpu::{Context, FrameConstants};
use crate::params::Params;
use crate::Res;

const SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tv.comp.spv"));

/// Registration of the slot (see [`super::CHAIN`]).
pub(in crate::gpu) const SLOT: Registration = Registration {
    name: "tv",
    input: Domain::Amp,
    output: Domain::Amp,
    new,
};

fn new() -> Box<dyn Stage> {
    Box::<TvStage>::default()
}

const FLAG_SHARPNESS: u32 = 1;
const FLAG_CHROMA: u32 = 2;
const FLAG_LEVELS: u32 = 4;
const FLAG_FLICKER: u32 = 8;

/// The push constants of `tv.comp` (std430, 36 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Push {
    sharpness: f32,
    radius: i32,
    cos_tint: f32,
    sin_tint: f32,
    colour: f32,
    black: f32,
    white: f32,
    gain: f32,
    flags: u32,
}

/// The oracle's settings for `params`.
pub(crate) fn model(p: &Params) -> Tv {
    Tv {
        sharpness: f64::from(p.tv_sharpness),
        sharpness_px: f64::from(p.tv_sharpness_px).max(0.0),
        tint_deg: f64::from(p.tv_tint_deg),
        colour: f64::from(p.tv_colour).max(0.0),
        black: f64::from(p.tv_black_level),
        white: f64::from(p.tv_white_level),
        flicker_amount: f64::from(p.tv_flicker_amount).clamp(0.0, 1.0),
        flicker_hz: f64::from(p.tv_flicker_hz).max(0.0),
        frame_rate_hz: f64::from(p.tv_frame_rate_hz).max(1.0),
    }
}

/// Whether the slot does nothing for `p`: every knob at its neutral value.
pub(crate) fn is_identity(p: &Params) -> bool {
    model(p).is_neutral()
}

/// The push constants of the frame `fc` for `params`.
pub(super) fn plan(p: &Params, fc: &FrameConstants) -> Push {
    let m = model(p);
    let radius = m.radius(fc.amp_k.max(1) as usize);
    let mut flags = 0;
    if m.has_sharpness() && radius > 0 {
        flags |= FLAG_SHARPNESS;
    }
    if m.has_chroma() {
        flags |= FLAG_CHROMA;
    }
    if m.black != 0.0 || m.white != 1.0 {
        flags |= FLAG_LEVELS;
    }
    if m.flicker_amount != 0.0 {
        flags |= FLAG_FLICKER;
    }
    let tint = m.tint_deg.to_radians();
    Push {
        sharpness: m.sharpness as f32,
        radius: radius as i32,
        cos_tint: tint.cos() as f32,
        sin_tint: tint.sin() as f32,
        colour: m.colour as f32,
        black: m.black as f32,
        white: m.white as f32,
        // Real time: the frames that have passed at the flicker's sampling rate (`tv_frame_rate_hz`).
        gain: m.flicker_gain(fc.source_frame(m.frame_rate_hz)) as f32,
        flags,
    }
}

/// The TV controls stage (see the module documentation).
#[derive(Default)]
struct TvStage {
    pass: ComputePass,
    push: Push,
}

impl Stage for TvStage {
    fn pass(&self) -> Option<&ComputePass> {
        Some(&self.pass)
    }

    unsafe fn create(&mut self, d: &ash::Device) -> Res<()> {
        self.pass.create(d, SPV, size_of::<Push>() as u32)
    }

    /// Every knob neutral: the exact identity, the pipeline skips the slot.
    fn is_identity(&self, params: &Params, _fc: &FrameConstants) -> bool {
        is_identity(params)
    }

    unsafe fn prepare(&mut self, _ctx: &Context, fc: &FrameConstants, params: &Params, runs: bool) -> Res<()> {
        if runs {
            self.push = plan(params, fc);
        }
        Ok(())
    }

    unsafe fn record(&self, d: &ash::Device, cb: vk::CommandBuffer, set: vk::DescriptorSet, size: (u32, u32)) {
        // SAFETY: `Push` is `#[repr(C)]` plain data of nine four-byte fields, no padding.
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

    /// The push block is 36 bytes in the shader's order.
    #[test]
    fn push_layout_matches_the_shader() {
        assert_eq!(size_of::<Push>(), 36);
        assert_eq!(std::mem::offset_of!(Push, flags), 32);
        let glsl = include_str!("../../../../../shaders/tv.comp");
        let order = [
            "float sharpness;",
            "int radius;",
            "float cos_tint;",
            "float sin_tint;",
            "float colour;",
            "float black;",
            "float white;",
            "float gain;",
            "uint flags;",
        ];
        let pos: Vec<usize> = order
            .iter()
            .map(|m| glsl.find(m).unwrap_or_else(|| panic!("{m}")))
            .collect();
        assert!(pos.windows(2).all(|w| w[0] < w[1]), "push members out of order");
        assert!(glsl.contains("local_size_x = 8, local_size_y = 8"));
    }

    /// Every knob neutral by default (the slot is skipped); each knob alone wakes it; the flags follow the
    /// oracle's conditions (a sharpness with no width, or less than half a sample, sets no flag).
    #[test]
    fn the_slot_runs_only_when_a_knob_moves() {
        let p = Params::default();
        assert!(is_identity(&p));
        let moved = |f: &dyn Fn(&mut Params)| {
            let mut q = p;
            f(&mut q);
            !is_identity(&q)
        };
        assert!(moved(&|q| q.tv_sharpness = 0.5));
        assert!(moved(&|q| q.tv_tint_deg = 5.0));
        assert!(moved(&|q| q.tv_colour = 1.2));
        assert!(moved(&|q| q.tv_black_level = 0.05));
        assert!(moved(&|q| q.tv_white_level = 0.9));
        assert!(moved(&|q| q.tv_flicker_amount = 0.1));
        let mut q = p;
        q.tv_sharpness = 1.0;
        q.tv_sharpness_px = 0.05;
        let fc = FrameConstants::new((320, 240), (1280, 960), false);
        assert_eq!(
            plan(&q, &fc).flags & FLAG_SHARPNESS,
            0,
            "0.05 px x K 4 rounds to no sample"
        );
        q.tv_sharpness_px = 1.0;
        let push = plan(&q, &fc);
        assert_eq!((push.flags, push.radius), (FLAG_SHARPNESS, 4));
    }

    /// The flicker's gain is of the frame, from the f64 phase: 10 Hz at 60 fps, depth 0.3, is 0.7 at frame 3.
    #[test]
    fn the_flicker_gain_follows_the_frame() {
        let p = Params {
            tv_flicker_amount: 0.3,
            ..Params::default()
        };
        let fc = FrameConstants::new((320, 240), (1280, 960), false).at_source_frame(3, 60.0);
        let push = plan(&p, &fc);
        assert!((f64::from(push.gain) - 0.7).abs() < 1e-6, "{}", push.gain);
        assert_eq!(push.flags, FLAG_FLICKER);
    }
}

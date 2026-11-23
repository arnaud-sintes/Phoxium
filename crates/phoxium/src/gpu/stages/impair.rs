//! Slot `impair` (voltage domain, amp -> amp, between the video amplifier and the gun): the optional analogue
//! impairments, off by default (`shaders/impair.comp`, parameters `params/impair.toml`, oracle
//! `crt_model::impair`, tests `tests/stage_impair.rs`): hum bar, line jitter and crawl, ghost, noise and snow, line
//! tracking.
//!
//! The CPU builds the oracle's [`Impair`] from the parameters ([`model`]) and pushes its per-frame constants
//! ([`plan`]: the hum and crawl phases of line 0 of this frame and their advance per line, the tracking band's
//! centre, the ghost delay in samples, computed in f64 so that the frame counter never loses precision). The
//! slot is an identity, and skipped, while `impair_enable` is off or every amount is 0.

use ash::vk;
use crt_model::impair::Impair;

use super::{ComputePass, Domain, Registration, Stage};
use crate::gpu::{Context, FrameConstants};
use crate::params::Params;
use crate::Res;

const SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/impair.comp.spv"));

/// Registration of the slot (see [`super::CHAIN`]).
pub(in crate::gpu) const SLOT: Registration = Registration {
    name: "impair",
    input: Domain::Amp,
    output: Domain::Amp,
    new,
};

fn new() -> Box<dyn Stage> {
    Box::<ImpairStage>::default()
}

/// The push constants of `impair.comp` (std430, 64 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Push {
    hum_amount: f32,
    hum_phase: f32,
    hum_per_line: f32,
    jitter_px: f32,
    crawl_px: f32,
    crawl_phase: f32,
    crawl_per_line: f32,
    ghost_gain: f32,
    ghost_delay: f32,
    noise: f32,
    snow: f32,
    snow_length: u32,
    tracking_px: f32,
    tracking_half: f32,
    tracking_centre: f32,
    tracking_noise: f32,
}

/// The oracle's settings for `params`.
pub(crate) fn model(p: &Params) -> Impair {
    Impair {
        field_rate_hz: f64::from(p.impair_field_rate_hz).max(1.0),
        hum_amount: f64::from(p.impair_hum_amount).max(0.0),
        hum_hz: f64::from(p.impair_hum_hz).max(0.0),
        jitter_px: f64::from(p.impair_jitter_px).max(0.0),
        crawl_px: f64::from(p.impair_crawl_px).max(0.0),
        crawl_period_lines: f64::from(p.impair_crawl_period_lines).max(1.0),
        crawl_speed_hz: f64::from(p.impair_crawl_speed_hz),
        ghost_gain: f64::from(p.impair_ghost_gain),
        ghost_delay_px: f64::from(p.impair_ghost_delay_px).max(0.0),
        noise: f64::from(p.impair_noise).max(0.0),
        snow: f64::from(p.impair_snow).clamp(0.0, 1.0),
        snow_length: p.impair_snow_length.max(1) as u32,
        tracking_px: f64::from(p.impair_tracking_px).max(0.0),
        tracking_lines: f64::from(p.impair_tracking_lines).max(1.0),
        tracking_position: f64::from(p.impair_tracking_position),
        tracking_roll_hz: f64::from(p.impair_tracking_roll_hz),
        tracking_noise: f64::from(p.impair_tracking_noise).max(0.0),
    }
}

/// Whether the slot does nothing for `p`: off, or every amount at 0.
pub(crate) fn is_identity(p: &Params) -> bool {
    !p.impair_enable || model(p).is_neutral()
}

/// The push constants of the frame `fc` for `params`.
pub(super) fn plan(p: &Params, fc: &FrameConstants) -> Push {
    let m = model(p);
    let lines = fc.native_size[1].max(1) as u32;
    let (hum_phase, hum_per_line) = m.hum_phase(fc.frame_index, lines);
    let (crawl_phase, crawl_per_line) = m.crawl_phase(fc.frame_index);
    Push {
        hum_amount: m.hum_amount as f32,
        hum_phase: hum_phase as f32,
        hum_per_line: hum_per_line as f32,
        jitter_px: m.jitter_px as f32,
        crawl_px: m.crawl_px as f32,
        crawl_phase: crawl_phase as f32,
        crawl_per_line: crawl_per_line as f32,
        ghost_gain: m.ghost_gain as f32,
        ghost_delay: (m.ghost_delay_px * f64::from(fc.amp_k.max(1))) as f32,
        noise: m.noise as f32,
        snow: m.snow as f32,
        snow_length: m.snow_length,
        tracking_px: m.tracking_px as f32,
        tracking_half: (0.5 * m.tracking_lines) as f32,
        tracking_centre: m.tracking_centre_lines(fc.frame_index, lines) as f32,
        tracking_noise: m.tracking_noise as f32,
    }
}

/// The impairments stage (see the module documentation).
#[derive(Default)]
struct ImpairStage {
    pass: ComputePass,
    push: Push,
}

impl Stage for ImpairStage {
    fn pass(&self) -> Option<&ComputePass> {
        Some(&self.pass)
    }

    unsafe fn create(&mut self, d: &ash::Device) -> Res<()> {
        self.pass.create(d, SPV, size_of::<Push>() as u32)
    }

    /// Off, or every amount at 0: the exact identity, the pipeline skips the slot.
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
        // SAFETY: `Push` is `#[repr(C)]` plain data of 16 four-byte fields, no padding.
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

    /// The push block is 64 bytes in the shader's order.
    #[test]
    fn push_layout_matches_the_shader() {
        assert_eq!(size_of::<Push>(), 64);
        assert_eq!(std::mem::offset_of!(Push, snow_length), 44);
        assert_eq!(std::mem::offset_of!(Push, tracking_noise), 60);
        let glsl = include_str!("../../../../../shaders/impair.comp");
        let order = [
            "float hum_amount;",
            "float hum_phase;",
            "float hum_per_line;",
            "float jitter_px;",
            "float crawl_px;",
            "float crawl_phase;",
            "float crawl_per_line;",
            "float ghost_gain;",
            "float ghost_delay;",
            "float noise;",
            "float snow;",
            "uint snow_length;",
            "float tracking_px;",
            "float tracking_half;",
            "float tracking_centre;",
            "float tracking_noise;",
        ];
        let pos: Vec<usize> = order
            .iter()
            .map(|m| glsl.find(m).unwrap_or_else(|| panic!("{m}")))
            .collect();
        assert!(pos.windows(2).all(|w| w[0] < w[1]), "push members out of order");
        assert!(glsl.contains("local_size_x = 8, local_size_y = 8"));
    }

    /// Off by default; on with the defaults the slot runs (the defaults are mild, not zero); on with every
    /// amount at 0, or off with amounts, it is an identity. The seeds of the shader are the oracle's.
    #[test]
    fn the_slot_runs_only_with_the_module_on_and_an_amount() {
        let p = Params::default();
        assert!(!p.impair_enable && is_identity(&p));
        let on = Params {
            impair_enable: true,
            ..p
        };
        assert!(!is_identity(&on), "the defaults are mild amounts, not zeros");
        let zero = Params {
            impair_hum_amount: 0.0,
            impair_jitter_px: 0.0,
            impair_crawl_px: 0.0,
            impair_ghost_gain: 0.0,
            impair_noise: 0.0,
            impair_snow: 0.0,
            impair_tracking_px: 0.0,
            ..on
        };
        assert!(is_identity(&zero));
        let glsl = include_str!("../../../../../shaders/impair.comp");
        for (name, value) in [
            ("SEED_JITTER", crt_model::impair::SEED_JITTER),
            ("SEED_TRACK", crt_model::impair::SEED_TRACK),
            ("SEED_NOISE", crt_model::impair::SEED_NOISE),
            ("SEED_SNOW", crt_model::impair::SEED_SNOW),
            ("SNOW_LEVEL_MIX", crt_model::impair::SNOW_LEVEL_MIX),
        ] {
            let line = format!("const uint {name} = 0x{value:08x}u;");
            assert!(glsl.contains(&line), "{line}");
        }
    }

    /// The plan of a frame: the ghost delay in amp samples (K per pixel), the phases of line 0 from the frame
    /// index in f64 (a counter far past f32 precision keeps its beat), the band half height and centre in lines.
    #[test]
    fn the_plan_is_in_amp_samples_and_per_frame_phases() {
        let p = Params {
            impair_enable: true,
            impair_ghost_delay_px: 5.0,
            impair_hum_hz: 50.0,
            impair_tracking_px: 4.0,
            impair_tracking_lines: 30.0,
            impair_tracking_position: 0.5,
            ..Params::default()
        };
        let mut fc = FrameConstants::new((320, 240), (1280, 960), false);
        fc.frame_index = 5;
        let push = plan(&p, &fc);
        assert_eq!(push.ghost_delay, 5.0 * fc.amp_k as f32);
        assert!((f64::from(push.hum_phase) - (50.0 * 5.0 / 60.0_f64).rem_euclid(1.0)).abs() < 1e-6);
        assert!((f64::from(push.hum_per_line) - 50.0 / (60.0 * 240.0)).abs() < 1e-9);
        assert_eq!((push.tracking_half, push.tracking_centre), (15.0, 120.0));
        fc.frame_index = 3_000_000_001;
        let far = plan(&p, &fc);
        let want = (50.0 * 3_000_000_001.0_f64 / 60.0).rem_euclid(1.0);
        assert!(
            (f64::from(far.hum_phase) - want).abs() < 1e-6,
            "{} {want}",
            far.hum_phase
        );
    }
}

//! Slot `scan` (linear light, tube -> tube, between `wear` and `phosphor`): motion clarity, the beam's sweep spread over
//! the output refreshes of one source frame (`shaders/scan.comp`, parameters `params/scan.toml`, oracle
//! `crt_model::scan`, tests `tests/stage_scan.rs`; QUESTIONS PHY-12, "Motion clarity").
//!
//! The pipeline decides the sweep ([`Sweep`]: the plan of [`crt_model::scan::plan`] from the parameters and the screen's
//! refresh rate, and the phase of this refresh in it) and hands it over with [`Stage::set_sweep`] before it routes the
//! frame. The slot is an exact identity, and skipped, while there is no sweep (the mode is off, or the screen
//! refreshes less than twice per source frame). Otherwise `prepare` turns the phase into the rows of the tube image that
//! the beam lit in this refresh ([`crt_model::scan::Plan::rows`]: the band of lines it crossed, with the spot's skirt
//! margin on the first and the last band, or the whole image for a lit strobe refresh, or nothing for a dark one) and
//! the gain of the lit rows, and the shader writes the lit rows at that gain and black elsewhere. Every stage before
//! this slot runs once per sweep (the pipeline holds their output between the refreshes, see `Pipeline`); the
//! phosphor stage after it sees the gated, boosted light of every refresh and decays it in real time.
//!
//! The band follows the raster's own axis (the tube image is the unturned raster, so a turned tube scans along its own
//! lines), and a boundary between two bands is a horizontal cut at the line boundary: on a tube with rotation or
//! trapezoid the lines at the cut are tilted, so a line at a boundary may be lit in two refreshes for a few pixels
//! (a known approximation, the geometry is drawn by the stages before this one).

use ash::vk;
use crt_model::scan::Plan;

use super::{ComputePass, Domain, Registration, Stage, Sweep};
use crate::gpu::{Context, FrameConstants};
use crate::params::Params;
use crate::Res;

const SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/scan.comp.spv"));

/// Registration of the slot (see [`super::CHAIN`]).
pub(in crate::gpu) const SLOT: Registration = Registration {
    name: "scan",
    input: Domain::Tube,
    output: Domain::Tube,
    new,
};

fn new() -> Box<dyn Stage> {
    Box::<ScanStage>::default()
}

/// The push constants of `scan.comp` (std430, 16 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(in crate::gpu) struct Push {
    /// First lit row of the tube image.
    pub row_lo: u32,
    /// One past the last lit row.
    pub row_hi: u32,
    /// Factor on the light of the lit rows.
    pub gain: f32,
    pub pad: u32,
}

/// The push constants of refresh `phase` of the sweep `plan` for a tube image of `fc`: the lit rows and the gain.
/// The raster's lines are the native rows, the margin is the spot's skirt around the raster (`raster_margin`).
pub(in crate::gpu) fn push_for(plan: &Plan, phase: u32, fc: &FrameConstants) -> Push {
    let lines = fc.native_size[1].max(0) as u32;
    let margin = fc.raster_margin[1].max(0) as u32;
    let height = fc.tube_size[1].max(0) as u32;
    let (row_lo, row_hi) = plan.rows(phase, lines, margin, height);
    Push {
        row_lo,
        row_hi,
        gain: plan.gain() as f32,
        pad: 0,
    }
}

/// The scan stage (see the module documentation).
#[derive(Default)]
struct ScanStage {
    pass: ComputePass,
    /// The sweep of the frame being planned (`None`: no scan).
    sweep: Option<Sweep>,
    push: Push,
}

impl Stage for ScanStage {
    fn pass(&self) -> Option<&ComputePass> {
        Some(&self.pass)
    }

    unsafe fn create(&mut self, d: &ash::Device) -> Res<()> {
        self.pass.create(d, SPV, size_of::<Push>() as u32)
    }

    /// No sweep: the exact identity, the pipeline skips the slot.
    fn is_identity(&self, _params: &Params, _fc: &FrameConstants) -> bool {
        self.sweep.is_none()
    }

    fn set_sweep(&mut self, sweep: Option<Sweep>) {
        self.sweep = sweep;
    }

    unsafe fn prepare(&mut self, _ctx: &Context, fc: &FrameConstants, _params: &Params, runs: bool) -> Res<()> {
        if let (true, Some(s)) = (runs, self.sweep) {
            self.push = push_for(&s.plan, s.phase, fc);
        }
        Ok(())
    }

    unsafe fn record(&self, d: &ash::Device, cb: vk::CommandBuffer, set: vk::DescriptorSet, size: (u32, u32)) {
        // SAFETY: `Push` is `#[repr(C)]` plain data of four four-byte fields, no padding.
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
    use crt_model::scan::{plan, Mode};

    /// The push block is 16 bytes, the shader declares the same fields.
    #[test]
    fn push_layout_matches_the_shader() {
        assert_eq!(size_of::<Push>(), 16);
        assert_eq!(std::mem::offset_of!(Push, row_hi), 4);
        assert_eq!(std::mem::offset_of!(Push, gain), 8);
        let glsl = include_str!("../../../../../shaders/scan.comp");
        for m in ["uint row_lo;", "uint row_hi;", "float gain;", "uint pad;"] {
            assert!(glsl.contains(m), "{m}");
        }
    }

    /// The rows and the gain of each refresh of a rolling sweep: the raster is the native rows, the margin the skirt.
    #[test]
    fn the_push_follows_the_oracle() {
        let mut fc = FrameConstants::new((320, 240), (1280, 960), false);
        fc.tube_size = [1280, 972];
        fc.raster_margin = [0, 6];
        let p = plan(Mode::Rolling, 300.0, 60.0, 0, 1, 1.0).unwrap();
        let rows: Vec<(u32, u32)> = (0..5)
            .map(|k| (push_for(&p, k, &fc).row_lo, push_for(&p, k, &fc).row_hi))
            .collect();
        assert_eq!(rows[0].0, 0);
        assert_eq!(rows[4].1, 972);
        for w in rows.windows(2) {
            assert_eq!(w[0].1, w[1].0, "the bands tile the image");
        }
        assert_eq!(push_for(&p, 2, &fc).gain, 5.0);
        let s = plan(Mode::Strobe, 240.0, 60.0, 0, 1, 0.0).unwrap();
        assert_eq!((push_for(&s, 0, &fc).row_lo, push_for(&s, 0, &fc).row_hi), (0, 972));
        assert_eq!((push_for(&s, 1, &fc).row_lo, push_for(&s, 1, &fc).row_hi), (0, 0));
        assert_eq!(push_for(&s, 0, &fc).gain, 1.0);
    }
}

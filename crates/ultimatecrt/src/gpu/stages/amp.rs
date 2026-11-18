//! Slot `amp` (voltage domain, native -> amp): the video amplifier (T-33, `shaders/amp.comp`).
//!
//! Each native row is a zero-order hold at the pixel clock, low-passed horizontally by a Gaussian
//! amplifier of bandwidth B before the gun law (`CRT.md` rule 1), with separate rise and fall times
//! (adjacent-pixel nonlinearity) and optional peaking, and sampled K times per native pixel
//! (K = `FrameConstants::amp_k`) so the beam stage sees a continuous signal. Parameters:
//! `params/amp.toml` (`amp_bandwidth_mhz`, `amp_pixel_clock_mhz`, `amp_fall_rise_ratio`,
//! `amp_peaking_gain`, `amp_peaking_mhz`). The defaults (PC-monitor bandwidth, no peaking) leave 240p
//! sources as an exact zero-order hold. When the slot is off, the pipeline's nearest resample gives the
//! zero-order hold, an exact identity.

use super::{ComputePass, Domain, Registration, Stage};
use crate::Res;

const SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/amp.comp.spv"));

/// Registration of the slot (see [`super::CHAIN`]).
pub(in crate::gpu) const SLOT: Registration = Registration {
    name: "amp",
    input: Domain::Native,
    output: Domain::Amp,
    new,
};

fn new() -> Box<dyn Stage> {
    Box::<Amp>::default()
}

#[derive(Default)]
struct Amp {
    pass: ComputePass,
}

impl Stage for Amp {
    fn pass(&self) -> Option<&ComputePass> {
        Some(&self.pass)
    }
    unsafe fn create(&mut self, d: &ash::Device) -> Res<()> {
        self.pass.create(d, SPV, 0)
    }
    unsafe fn destroy(&mut self, d: &ash::Device) {
        self.pass.destroy(d);
    }
}

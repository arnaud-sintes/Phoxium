//! Slot `gun` (amp -> amp): drive voltage to beam current per channel, the gun law (T-34, PHY-6 and COL-2,
//! `shaders/gun.comp`, parameters in `params/gun.toml`).
//!
//! Reads the drive voltage V in [0, 1] of the `amp` output (the `decode` contract) and writes the beam
//! current into its own image of the same size (no in-place update: every chain stage writes a new
//! image, see [`super`]). The law (power, GOG, BT.1886, Annex 1) and its controls come from `Params`; the
//! white balance gains come from `FrameConstants::white_gain` (solved on the CPU, `crate::white`).
//!
//! Moved here by T-34: the tube transfer that `decode` applied through `Settings::source` (default
//! identity now). A disabled `gun` slot is a bypass: the next slot reads the drive voltage V unchanged
//! (bit exact, same-domain slots are skipped), so a chain with `gun` off ends in V, not in beam current.

use super::{ComputePass, Domain, Registration, Stage};
use crate::Res;

const SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/gun.comp.spv"));

/// Registration of the slot (see [`super::CHAIN`]).
pub(in crate::gpu) const SLOT: Registration = Registration {
    name: "gun",
    input: Domain::Amp,
    output: Domain::Amp,
    new,
};

fn new() -> Box<dyn Stage> {
    Box::<Gun>::default()
}

#[derive(Default)]
struct Gun {
    pass: ComputePass,
}

impl Stage for Gun {
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

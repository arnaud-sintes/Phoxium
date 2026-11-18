//! Slot `gun` (amp -> amp): drive voltage to beam current per channel, the gun law (T-34,
//! `shaders/gun.comp`). Today the shader is an exact copy: the tube gamma is still applied by `decode`
//! (`Settings::source`) until T-30 makes `decode` output drive voltages and T-34 writes the gun law here.
//!
//! Reads the `amp` output and writes its own image of the same size (no in-place update: every chain
//! stage writes a new image, see [`super`]).

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

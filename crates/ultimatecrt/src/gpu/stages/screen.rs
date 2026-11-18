//! Slot `screen` (amp -> tube): light emitted by the phosphor screen at each host pixel of the tube
//! rectangle, product form L_c = mask_eval() x beam_eval() (`shaders/screen.comp`). The beam
//! (`shaders/include/beam.glsl`, T-36a, T-36b) and the mask (`shaders/include/mask.glsl`, T-39a, T-39b)
//! are stubs today: nearest amp sample and 1.0, an exact identity.

use super::{ComputePass, Domain, Registration, Stage};
use crate::Res;

const SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/screen.comp.spv"));

/// Registration of the slot (see [`super::CHAIN`]).
pub(in crate::gpu) const SLOT: Registration = Registration {
    name: "screen",
    input: Domain::Amp,
    output: Domain::Tube,
    new,
};

fn new() -> Box<dyn Stage> {
    Box::<Screen>::default()
}

#[derive(Default)]
struct Screen {
    pass: ComputePass,
}

impl Stage for Screen {
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

//! Slot `screen` (amp -> tube): light emitted by the phosphor screen at each host pixel of the tube
//! rectangle, product form L_c = mask_eval() x beam_eval() (`shaders/screen.comp`). The beam
//! (`shaders/include/beam.glsl`, T-36a, T-36b, T-36c) and the mask (`shaders/include/mask.glsl`, T-39a,
//! T-39b) are evaluated per host pixel. The shader's workgroup is one beam tile (`BEAM_TILE_W` x
//! `BEAM_TILE_H`, T-36c: the standard quality shares the line profiles of the tile in shared memory).

use ash::vk;

use super::{ComputePass, Domain, Registration, Stage};
use crate::Res;

const SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/screen.comp.spv"));

/// Workgroup size of `screen.comp`: `BEAM_TILE_W`, `BEAM_TILE_H` of `shaders/include/beam.glsl`.
const SCREEN_GROUP: (u32, u32) = (8, 32);

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
    unsafe fn record(&self, d: &ash::Device, cb: vk::CommandBuffer, set: vk::DescriptorSet, size: (u32, u32)) {
        self.pass.dispatch_group(d, cb, set, &[], size, SCREEN_GROUP);
    }
    unsafe fn destroy(&mut self, d: &ash::Device) {
        self.pass.destroy(d);
    }
}

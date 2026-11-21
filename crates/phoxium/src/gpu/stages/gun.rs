//! Slot `gun` (amp -> amp): drive voltage to beam current per channel, the gun law (T-34, PHY-6 and COL-2,
//! `shaders/gun.comp`, parameters in `params/gun.toml`).
//!
//! Reads the drive voltage V in [0, 1] of the `amp` output (the `decode` contract) and writes the beam
//! current into its own image of the same size (no in-place update: every chain stage writes a new
//! image, see [`super`]). The law (power, GOG, BT.1886, Annex 1) and its controls come from `Params`; the
//! white retune gains come from `FrameConstants::white_gain` (solved on the CPU, `crate::white`: relative
//! to the tube white, (1, 1, 1) when off or when the target is the tube white). The laws live in
//! `shaders/include/gun_law.glsl`, shared with `decode`, whose `srgb` source mode inverts the nominal curve
//! of the active law. `gun_bt1886_white` matters only through the ratio Lb / Lw. `gun_gamma` is clamped to
//! >= 0.1 in the shaders.
//!
//! Moved here by T-34: the tube transfer that `decode` applied through `Settings::source` (default
//! identity now). A disabled `gun` slot is a bypass: the next slot reads the drive voltage V unchanged
//! (bit exact, same-domain slots are skipped), so a chain with `gun` off ends in V, not in beam current,
//! and `encode` then treats V as linear light. The picture is LIGHTER than with the default gun (a pure
//! 2.4 power law), not darker: code 20 (V = 0.0784) shows as linear 0.0784 instead of 0.00222, which a
//! pure 2.2 host encodes as code 80 instead of 20 (79 on an sRGB host, 88 on a pure 2.4 host).

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

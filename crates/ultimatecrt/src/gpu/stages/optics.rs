//! Slot `optics` (linear light, tube -> tube): diffusion, halation, glare, vignette, tint (T-45).
//! Placeholder until then: bypassed, exact identity.

use super::{Domain, Placeholder, Registration, Stage};

/// Registration of the slot (see [`super::CHAIN`]).
pub(in crate::gpu) const SLOT: Registration = Registration {
    name: "optics",
    input: Domain::Tube,
    output: Domain::Tube,
    new,
};

fn new() -> Box<dyn Stage> {
    Box::<Placeholder>::default()
}

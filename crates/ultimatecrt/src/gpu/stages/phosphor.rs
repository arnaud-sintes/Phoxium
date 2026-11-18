//! Slot `phosphor` (linear light, tube -> tube): persistence, ageing and burn-in (T-41, T-42, T-43).
//! Placeholder until then: bypassed, exact identity.

use super::{Domain, Placeholder, Registration, Stage};

/// Registration of the slot (see [`super::CHAIN`]).
pub(in crate::gpu) const SLOT: Registration = Registration {
    name: "phosphor",
    input: Domain::Tube,
    output: Domain::Tube,
    new,
};

fn new() -> Box<dyn Stage> {
    Box::<Placeholder>::default()
}

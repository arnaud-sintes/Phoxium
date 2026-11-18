//! Slot `signal` (voltage domain, native -> native): composite or S-Video encode and decode (T-31,
//! optional, off by default). Placeholder until then: bypassed, exact identity.

use super::{Domain, Placeholder, Registration, Stage};

/// Registration of the slot (see [`super::CHAIN`]).
pub(in crate::gpu) const SLOT: Registration = Registration {
    name: "signal",
    input: Domain::Native,
    output: Domain::Native,
    new,
};

fn new() -> Box<dyn Stage> {
    Box::<Placeholder>::default()
}

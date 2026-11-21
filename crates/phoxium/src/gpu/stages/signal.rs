//! Slot `signal` (voltage domain, native -> native): the optional composite or S-Video encode and decode
//! (T-31, [`super::composite`]), off by default: skipped as an exact identity while `composite_enable` is
//! off.

use super::{Domain, Registration, Stage};

/// Registration of the slot (see [`super::CHAIN`]).
pub(in crate::gpu) const SLOT: Registration = Registration {
    name: "signal",
    input: Domain::Native,
    output: Domain::Native,
    new,
};

fn new() -> Box<dyn Stage> {
    Box::<super::composite::CompositeStage>::default()
}

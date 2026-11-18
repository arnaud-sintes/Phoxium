//! Slot `amp` (voltage domain, native -> amp): video amplifier, horizontal filter on native rows
//! supersampled by K (T-33). Placeholder until then: bypassed by the pipeline's nearest resample, which
//! repeats each native texel K times (zero-order hold), an exact identity.

use super::{Domain, Placeholder, Registration, Stage};

/// Registration of the slot (see [`super::CHAIN`]).
pub(in crate::gpu) const SLOT: Registration = Registration {
    name: "amp",
    input: Domain::Native,
    output: Domain::Amp,
    new,
};

fn new() -> Box<dyn Stage> {
    Box::<Placeholder>::default()
}

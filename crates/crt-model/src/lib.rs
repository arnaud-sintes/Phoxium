//! Pure f64 reference model of the CRT physics (T-14).
//!
//! No OS or GPU dependency. Lengths carry an `_mm` suffix, times `_ms` (or `_ns`
//! for the video amplifier), frequencies `_mhz`, currents `_ua`.
//! These functions are the oracle the GPU tests trust: keep them simple and
//! written straight from the formulas in `CRT.md`.
//!
//! Modules follow the physical chain: `units`, `geometry` (tube, raster, mask
//! layout), `colour` (gun laws, EOTFs, primaries), `beam` (spot, scanlines,
//! amplifier), `phosphor` (decay), `optics` (faceplate glass).
//!
//! Bad-input policy: a function whose formula would return NaN or an
//! infinity on out-of-domain input panics instead (`assert!`, also in
//! release builds), and documents the domain in a `# Panics` section. An
//! oracle that panics loudly is preferred over one that returns a silent NaN.

pub mod abl;
pub mod beam;
pub mod colour;
pub mod geometry;
pub mod gun;
pub mod mask;
pub mod optics;
pub mod output;
pub mod phosphor;
pub mod tube;
pub mod units;

/// Absolute-tolerance comparison used by the tests.
#[cfg(test)]
pub(crate) fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

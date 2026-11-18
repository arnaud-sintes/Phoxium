//! Test-only stage (T-20b acceptance): proves that a new stage needs only its own files (this module
//! and `shaders/test_dummy.comp`) plus one registration line in a chain. Tube -> tube, rgb = 1 - rgb.

use super::{ComputePass, Domain, Registration, Stage};
use crate::Res;

const SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/test_dummy.comp.spv"));

/// Registration of the slot.
pub(in crate::gpu) const SLOT: Registration = Registration {
    name: "dummy",
    input: Domain::Tube,
    output: Domain::Tube,
    new,
};

fn new() -> Box<dyn Stage> {
    Box::<Dummy>::default()
}

#[derive(Default)]
struct Dummy {
    pass: ComputePass,
}

impl Stage for Dummy {
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

#[cfg(test)]
mod tests {
    use super::super::{amp, gun, optics, phosphor, screen, signal};
    use super::*;
    use crate::gpu::{validation_layer_available, Headless, Settings, SourceFrame, TargetFormat};
    use crate::params::Params;
    use crate::scaling::View;

    /// The production chain plus the dummy: the one registration line is `SLOT` below.
    const TEST_CHAIN: &[Registration] = &[
        signal::SLOT,
        amp::SLOT,
        gun::SLOT,
        screen::SLOT,
        SLOT,
        phosphor::SLOT,
        optics::SLOT,
    ];

    /// A dummy stage registered by one line runs in the chain (output 255 - k for every code k), and
    /// turning it off restores the exact identity; validation silent.
    #[test]
    fn a_new_stage_needs_its_files_and_one_registration_line() {
        let required = std::env::var("UCRT_REQUIRE_GPU").as_deref() == Ok("1");
        let validation = validation_layer_available();
        assert!(!required || validation, "UCRT_REQUIRE_GPU=1 but no validation layer");
        let mut gpu = match Headless::with_chain(validation, TEST_CHAIN) {
            Ok(g) => g,
            Err(e) => {
                assert!(!required, "UCRT_REQUIRE_GPU=1 but no headless device: {e}");
                eprintln!("dummy stage test skipped: {e}");
                return;
            }
        };
        let counter = gpu.validation_counter();
        assert_eq!(
            gpu.stage_names(),
            ["signal", "amp", "gun", "screen", "dummy", "phosphor", "optics"]
        );
        let (w, h) = (256u32, 2u32);
        let mut bgra = Vec::new();
        for _ in 0..h {
            for x in 0..w {
                let k = x as u8;
                bgra.extend_from_slice(&[k, 255 - k, k, 255]);
            }
        }
        let render = |gpu: &mut Headless| {
            gpu.render(
                Some(SourceFrame {
                    width: w,
                    height: h,
                    bgra: &bgra,
                }),
                View::full(w, h),
                &Settings::identity(),
                &Params::identity_gun(),
                (w, h),
                TargetFormat::Bgra8Unorm,
            )
            .expect("headless render")
        };
        let on = render(&mut gpu);
        gpu.set_stage_enabled("dummy", false).unwrap();
        let off = render(&mut gpu);
        drop(gpu);
        for y in 0..h {
            for x in 0..w {
                let k = x as f32;
                let (a, b) = (on.output.pixel(x, y), off.output.pixel(x, y));
                assert_eq!((a[0] * 255.0).round(), 255.0 - k, "dummy on, red at {x}");
                assert_eq!((a[1] * 255.0).round(), k, "dummy on, green at {x}");
                assert_eq!((b[0] * 255.0).round(), k, "dummy off, red at {x}");
                assert_eq!((b[1] * 255.0).round(), 255.0 - k, "dummy off, green at {x}");
            }
        }
        assert!(on.stage("dummy").is_some());
        assert_eq!(counter.count(), 0, "validation messages (printed above)");
    }
}

//! Shared harness of the GPU tests (TEC-7).
//!
//! [`gpu_or_skip`] creates a headless renderer with the validation layer when it is installed.
//! Without a Vulkan 1.3 device the test prints "skipped" and passes, unless `UCRT_REQUIRE_GPU=1`,
//! which turns the skip into a failure and also requires the validation layer (a validation count
//! of 0 proves nothing without it). [`assert_no_validation_messages`] checks the count after the
//! renderer is dropped, so messages raised while destroying the device count too.

use ultimatecrt::gpu::{validation_layer_available, Headless, ValidationCounter};

/// True when `UCRT_REQUIRE_GPU=1`: a missing GPU or validation layer fails the test.
pub fn gpu_required() -> bool {
    std::env::var("UCRT_REQUIRE_GPU").as_deref() == Ok("1")
}

/// A headless renderer, validation on when the layer is installed, or `None` (after printing
/// "<test> skipped: <reason>") when no device can be created and the GPU is not required.
pub fn gpu_or_skip(test: &str) -> Option<Headless> {
    let validation = validation_layer_available();
    if !validation {
        assert!(
            !gpu_required(),
            "UCRT_REQUIRE_GPU=1 but VK_LAYER_KHRONOS_validation is not installed (install the Vulkan SDK)"
        );
        eprintln!("{test}: validation layer not installed, validation messages are not checked");
    }
    match Headless::new(validation) {
        Ok(gpu) => {
            assert_eq!(gpu.context().validation_enabled(), validation);
            Some(gpu)
        }
        Err(e) => {
            assert!(
                !gpu_required(),
                "UCRT_REQUIRE_GPU=1 but no headless Vulkan 1.3 device: {e}"
            );
            eprintln!("{test} skipped: no Vulkan 1.3 device ({e})");
            None
        }
    }
}

/// Fails when the validation layer reported any warning or error. Call it after dropping the
/// renderer the counter came from.
pub fn assert_no_validation_messages(counter: &ValidationCounter) {
    assert_eq!(
        counter.count(),
        0,
        "the validation layer reported messages (printed above on stderr)"
    );
}

//! Shared harness of the GPU tests (TEC-7).
//!
//! [`gpu_or_skip`] creates a headless renderer with the validation layer when it is installed.
//! Without a Vulkan 1.3 device the test prints "skipped" and passes, unless `UCRT_REQUIRE_GPU` is set (non-empty, not `0`),
//! which turns the skip into a failure and also requires the validation layer and synchronization validation (a validation
//! count of 0 proves nothing without them). [`assert_no_validation_messages`] checks the count after the
//! renderer is dropped, so messages raised while destroying the device count too.

use ultimatecrt::gpu::{gpu_required, validation_layer_available, Context, Headless, ValidationCounter};

/// A headless renderer, validation on when the layer is installed, or `None` (after printing
/// "<test> skipped: <reason>") when no device can be created and the GPU is not required.
///
/// Only a missing device or instance skips: once the [`Context`] exists, a failure to create the
/// renderer objects (shaders, pipeline layouts) panics, as it is a regression and not a missing GPU.
/// With `UCRT_REQUIRE_GPU` set, the validation layer and synchronization validation are required.
pub fn gpu_or_skip(test: &str) -> Option<Headless> {
    let validation = validation_layer_available();
    if !validation {
        assert!(
            !gpu_required(),
            "UCRT_REQUIRE_GPU is set but VK_LAYER_KHRONOS_validation is not installed (install the Vulkan SDK)"
        );
        eprintln!("{test}: validation layer not installed, validation messages are not checked");
    }
    let ctx = match Context::new_headless(validation) {
        Ok(ctx) => ctx,
        Err(e) => {
            assert!(
                !gpu_required(),
                "UCRT_REQUIRE_GPU is set but no headless Vulkan 1.3 device: {e}"
            );
            eprintln!("{test} skipped: no Vulkan 1.3 device ({e})");
            return None;
        }
    };
    assert_eq!(ctx.validation_enabled(), validation);
    if validation && !ctx.sync_validation_enabled() {
        assert!(
            !gpu_required(),
            "UCRT_REQUIRE_GPU is set but synchronization validation is off (the validation layer lacks \
             VK_EXT_validation_features or VK_EXT_layer_settings): update the Vulkan SDK"
        );
        eprintln!("{test}: WARNING synchronization validation is off, synchronization errors are not checked");
    }
    Some(Headless::from_context(ctx).expect("device exists but creating the headless renderer failed"))
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

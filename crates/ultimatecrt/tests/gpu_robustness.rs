//! GPU robustness (code review WP3, R-02 and R-01 GPU side), headless, with the validation layer (synchronization
//! validation on) and zero validation messages:
//! - a wide source with many amplifier samples renders with K limited to what the device allows for one image
//!   (it used to ask for an image wider than `maxImageDimension2D`), with a notice;
//! - a frame whose tube area is empty draws the surround only and is not an error.

mod common;

use ultimatecrt::gpu::{Headless, Settings, SourceFrame, TargetFormat};
use ultimatecrt::params::Params;
use ultimatecrt::quality::{clamp_amp_k, ImageLimits};
use ultimatecrt::scaling::View;

fn source(w: u32, h: u32) -> Vec<u8> {
    (0..w * h)
        .flat_map(|i| [(i % 251) as u8, (i / w) as u8, 200, 255])
        .collect()
}

fn render(
    gpu: &mut Headless,
    src: (u32, u32),
    bgra: &[u8],
    params: &Params,
    target: (u32, u32),
) -> ultimatecrt::gpu::Rendered {
    gpu.render(
        Some(SourceFrame {
            width: src.0,
            height: src.1,
            bgra,
        }),
        View::full(src.0, src.1),
        &Settings::default(),
        params,
        target,
        TargetFormat::Bgra8Unorm,
    )
    .expect("render")
}

/// R-02: a 2560-wide source with 16 amplifier samples (six shipped presets set 16; the default tier is
/// `standard`) asked for an amp image 40960 px wide, beyond every vendor's limit. K is now limited to the
/// device's largest image side (and the memory budget) in every tier: the device's own limit, Intel and AMD's
/// 16384, and the Vulkan minimum 4096.
#[test]
fn a_wide_source_renders_with_the_amplifier_samples_limited() {
    let Some(mut gpu) = common::gpu_or_skip("a_wide_source_renders_with_the_amplifier_samples_limited") else {
        return;
    };
    let counter = gpu.validation_counter();
    let device = gpu.image_limits();
    eprintln!("device image limits: {device:?}");
    let src = (2560u32, 48u32);
    let bgra = source(src.0, src.1);
    let params = Params {
        amp_samples: 16,
        ..Params::default()
    };
    let target = (1280, 720);
    let mut ks = Vec::new();
    for max_dim in [device.max_dim, 16384, 4096] {
        let limits = ImageLimits {
            max_dim: max_dim.min(device.max_dim),
            budget_bytes: device.budget_bytes,
        };
        gpu.set_image_limits(limits);
        let r = render(&mut gpu, src, &bgra, &params, target);
        let (k, clamped) = clamp_amp_k(16, src, limits);
        assert!(clamped, "{max_dim}: 2560 x 16 is over every limit");
        let amp = r.stage("amp").expect("amp image");
        assert_eq!(
            (amp.width, amp.height),
            (src.0 * k as u32, src.1),
            "{max_dim}: amp image"
        );
        assert!(amp.width <= limits.max_dim);
        for (name, img) in &r.stages {
            assert!(
                img.width <= limits.max_dim && img.height <= limits.max_dim,
                "{max_dim}: {name} is {} x {}",
                img.width,
                img.height
            );
        }
        let notice = gpu.gpu_notice().expect("a notice while K is limited");
        assert!(
            notice.contains(&format!("amplifier samples limited to {k} (asked 16)")),
            "{notice}"
        );
        assert!(r.output.rgba.iter().all(|v| v.is_finite()));
        if !ks.contains(&k) {
            ks.push(k);
        }
    }
    // Printed once per change, not per frame.
    let fresh = gpu.take_gpu_notices();
    assert_eq!(fresh.len(), ks.len(), "{fresh:?}");
    let again = render(&mut gpu, src, &bgra, &params, target);
    assert!(again.stage("amp").is_some());
    assert!(gpu.take_gpu_notices().is_empty(), "same limit: no new notice");
    // Within the limit (the device's again): K as asked, and the notice goes away.
    gpu.set_image_limits(device);
    let small = Params {
        amp_samples: 4,
        ..Params::default()
    };
    let r = render(&mut gpu, src, &bgra, &small, target);
    assert_eq!(r.stage("amp").map(|a| a.width), Some(src.0 * 4));
    assert_eq!(gpu.gpu_notice(), None);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// R-01 (GPU side): tube parameters that leave no tube area (0 typed in the menu, or a hand-edited file) used to
/// end the application with "empty Tube domain". The frame now draws the surround only, records no chain pass
/// and raises a notice; the next valid frame renders normally and clears it.
#[test]
fn an_empty_tube_area_draws_the_surround_only() {
    let Some(mut gpu) = common::gpu_or_skip("an_empty_tube_area_draws_the_surround_only") else {
        return;
    };
    let counter = gpu.validation_counter();
    let src = (64u32, 48u32);
    let bgra = source(src.0, src.1);
    let target = (320, 240);
    let normal = render(&mut gpu, src, &bgra, &Params::default(), target);
    assert!(gpu.last_chain_passes() > 0);
    assert!(normal.output.rgba.iter().any(|&v| v > 0.0 && v < 1.0), "a picture");
    let empties = [
        Params {
            tube_aspect: 0.0,
            ..Params::default()
        },
        Params {
            tube_visible_height_mm: 0.0,
            ..Params::default()
        },
        Params {
            tube_height_fraction: 0.0,
            ..Params::default()
        },
    ];
    for (i, p) in empties.iter().enumerate() {
        let r = render(&mut gpu, src, &bgra, p, target);
        assert_eq!(gpu.last_chain_passes(), 0, "case {i}: no chain pass");
        assert!(r.stages.is_empty(), "case {i}: nothing routed");
        // The surround (`source_surround_color`, black by default) everywhere, alpha 1.
        let surround = Params::default().source_surround_color;
        for px in r.output.rgba.chunks(4) {
            for c in 0..3 {
                assert!((px[c] - surround[c]).abs() < 1.0 / 255.0, "case {i}: {px:?}");
            }
        }
        let notice = gpu.gpu_notice().expect("a notice");
        assert!(notice.contains("tube area is empty"), "case {i}: {notice}");
    }
    // Several frames in a row with the empty area (frames in flight), then a valid one again.
    for _ in 0..3 {
        gpu.render(
            None,
            View::full(src.0, src.1),
            &Settings::default(),
            &empties[0],
            target,
            TargetFormat::Bgra8Unorm,
        )
        .expect("render without a new frame");
    }
    let back = render(&mut gpu, src, &bgra, &Params::default(), target);
    assert!(gpu.last_chain_passes() > 0);
    assert!(back.output.rgba.iter().any(|&v| v > 0.0 && v < 1.0), "a picture again");
    assert_eq!(gpu.gpu_notice(), None, "the notice is cleared");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

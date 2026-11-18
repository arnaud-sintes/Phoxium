//! GPU tests of the stage slots and the image contract (T-20b), headless, with the validation layer
//! (synchronization validation on) and zero validation messages.
//!
//! - Identity through the full chain: every slot at identity (placeholders, `gun` copy, `screen` with
//!   the beam and mask stubs), all slots on or all off, gives back every input code exactly (256 codes
//!   per channel), and every chain image holds the decoded values unchanged (FP16 copies, no
//!   arithmetic).
//! - Enable flags: each slot toggled alone leaves the output unchanged; unknown names are errors.
//! - Slot order and the image contract (domain sizes, K).
//! - The stage-test helper: an image injected after a slot, with injected `FrameConstants` (K, tube
//!   size), through a chosen stage subset, read back per slot.

mod common;

use ultimatecrt::gpu::{
    Domain, FrameConstants, Headless, ImageF32, Rendered, Settings, SourceFrame, StageInput, StageRun, TargetFormat,
    Transfer, AMP_K,
};
use ultimatecrt::params::Params;
use ultimatecrt::scaling::View;

const SLOTS: [&str; 6] = ["signal", "amp", "gun", "screen", "phosphor", "optics"];
const W: u32 = 256;
const H: u32 = 4;

/// Codes (r, g, b) of pixel (x, y): all 256 codes per channel on each row (as in `gpu_identity.rs`).
fn codes(x: u32, y: u32) -> [u8; 3] {
    [x as u8, (255 - x) as u8, ((x + 64 * y) % 256) as u8]
}

fn ramp() -> Vec<u8> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let [r, g, b] = codes(x, y);
            v.extend_from_slice(&[b, g, r, 255]);
        }
    }
    v
}

fn render(gpu: &mut Headless, bgra: &[u8], t: Transfer) -> Rendered {
    let settings = Settings {
        source: t,
        host: t,
        integer_scale: false,
    };
    gpu.render(
        Some(SourceFrame {
            width: W,
            height: H,
            bgra,
        }),
        View::full(W, H),
        &settings,
        &Params::default(),
        (W, H),
        TargetFormat::Bgra8Unorm,
    )
    .expect("headless render")
}

/// Every output code equals its input code.
fn assert_exact(r: &Rendered, what: &str) {
    for y in 0..H {
        for x in 0..W {
            let p = r.output.pixel(x, y);
            let got = [p[0], p[1], p[2]].map(|v| (v * 255.0).round() as u8);
            assert_eq!(got, codes(x, y), "{what}: pixel ({x}, {y})");
        }
    }
}

/// Every chain image holds the native raster's values, bit for bit: same-size images equal it, the
/// amp-domain images repeat each native texel K times.
fn assert_chain_images_carry_native(r: &Rendered, what: &str) {
    let names: Vec<&str> = r.stages.iter().map(|(n, _)| *n).collect();
    assert_eq!(names, SLOTS, "{what}: routed slots");
    for (name, img) in &r.stages {
        let k = img.width / W;
        assert_eq!(img.height, H, "{what}: {name} height");
        assert!(k == 1 || k == AMP_K, "{what}: {name} width {}", img.width);
        for y in 0..H {
            for x in 0..img.width {
                assert_eq!(
                    img.pixel(x, y)[..3],
                    r.native.pixel(x / k, y)[..3],
                    "{what}: {name} texel ({x}, {y})"
                );
            }
        }
    }
    assert_eq!(
        r.stage("amp").unwrap().width,
        W * AMP_K,
        "{what}: amp is K x native wide"
    );
    assert_eq!(
        r.stage("gun").unwrap().width,
        W * AMP_K,
        "{what}: gun keeps the amp size"
    );
    assert_eq!(r.stage("screen").unwrap().width, W, "{what}: screen is the tube size");
}

#[test]
fn slots_are_registered_in_physical_order() {
    let Some(gpu) = common::gpu_or_skip("gpu_stages order") else {
        return;
    };
    assert_eq!(gpu.stage_names(), SLOTS);
    for s in SLOTS {
        assert_eq!(gpu.stage_enabled(s), Some(true), "{s} on by default");
    }
    assert_eq!(gpu.stage_enabled("decode"), None);
}

/// Identity through the full chain, all slots on then all off, for the transfers of the T-20a identity
/// test: 256 codes per channel exact, chain images bit-exact copies of the native raster.
#[test]
fn full_chain_identity_all_slots_on_and_off() {
    let test = "gpu_stages full chain identity";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let bgra = ramp();
    for t in [Transfer::Power(2.4), Transfer::Srgb, Transfer::Identity] {
        gpu.enable_only(&SLOTS).unwrap();
        let on = render(&mut gpu, &bgra, t);
        assert_exact(&on, &format!("{test} {t:?} all on"));
        assert_chain_images_carry_native(&on, &format!("{test} {t:?} all on"));
        gpu.enable_only(&[]).unwrap();
        let off = render(&mut gpu, &bgra, t);
        assert_exact(&off, &format!("{test} {t:?} all off"));
        assert_chain_images_carry_native(&off, &format!("{test} {t:?} all off"));
        assert_eq!(on.output, off.output, "{test} {t:?}: on and off give the same output");
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Each slot toggled alone, frame after frame on one pipeline (descriptor rewrites, routing changes,
/// tracked barriers across frames): the output stays exact; validation silent.
#[test]
fn toggling_each_slot_keeps_the_identity() {
    let test = "gpu_stages toggling";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let bgra = ramp();
    let t = Transfer::Power(2.4);
    let reference = render(&mut gpu, &bgra, t);
    for s in SLOTS {
        gpu.set_stage_enabled(s, false).unwrap();
        assert_eq!(gpu.stage_enabled(s), Some(false));
        let r = render(&mut gpu, &bgra, t);
        assert_eq!(r.output, reference.output, "{test}: {s} off");
        gpu.set_stage_enabled(s, true).unwrap();
        let r = render(&mut gpu, &bgra, t);
        assert_eq!(r.output, reference.output, "{test}: {s} back on");
    }
    assert!(gpu.set_stage_enabled("nope", false).is_err());
    assert!(gpu.enable_only(&["gun", "nope"]).is_err());
    assert_eq!(
        gpu.stage_enabled("gun"),
        Some(true),
        "a refused enable_only changes nothing"
    );
    // Frames in flight with every slot off then on: overlapping frames share the chain images.
    gpu.enable_only(&[]).unwrap();
    let f = SourceFrame {
        width: W,
        height: H,
        bgra: &bgra,
    };
    let settings = Settings {
        source: t,
        host: t,
        integer_scale: false,
    };
    let frames = [
        (Some(f), View::full(W, H)),
        (None, View::full(W, H)),
        (Some(f), View::full(W, H)),
    ];
    let r = gpu
        .render_burst(&frames, &settings, &Params::default(), (W, H), TargetFormat::Bgra8Unorm)
        .unwrap();
    assert_eq!(r.output, reference.output, "{test}: burst, all off");
    gpu.enable_only(&SLOTS).unwrap();
    let r = gpu
        .render_burst(&frames, &settings, &Params::default(), (W, H), TargetFormat::Bgra8Unorm)
        .unwrap();
    assert_eq!(r.output, reference.output, "{test}: burst, all on");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// A float image with exact FP16 values (multiples of 1/64 below 4), distinct per texel and channel.
fn pattern(w: u32, h: u32) -> ImageF32 {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as f32;
            rgba.extend_from_slice(&[i / 64.0, 1.0 + i / 64.0, 2.0 - i / 128.0, 1.0]);
        }
    }
    ImageF32 {
        width: w,
        height: h,
        rgba,
    }
}

/// The stage-test helper: an amp-domain image injected after `gun` with injected constants (K = 2, a
/// tube twice the native size), only `screen` enabled: the screen output is the nearest amp sample per
/// host pixel (the beam fallback while the tube is not placed, px per mm and line pitch 0, T-36a),
/// exactly, and the slots before the injection point are absent.
#[test]
fn injected_image_through_a_stage_subset() {
    let test = "gpu_stages injection";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let (nw, nh) = (8u32, 4u32);
    let mut fc = FrameConstants::new((nw, nh), (2 * nw, 2 * nh), false);
    fc.amp_k = 2;
    assert_eq!((fc.px_per_mm, fc.line_pitch_mm), (0.0, 0.0), "tube not placed");
    assert_eq!(fc.domain_size(Domain::Amp), (16, 4));
    assert_eq!(fc.domain_size(Domain::Tube), (16, 8));
    let amp = pattern(16, 4);
    gpu.enable_only(&["screen"]).unwrap();
    let settings = Settings::identity();
    let params = Params::default();
    let run = StageRun {
        input: StageInput::Image {
            after: "gun",
            image: &amp,
        },
        frame: fc,
        settings: &settings,
        params: &params,
        format: TargetFormat::Rgba32F,
    };
    let r = gpu.render_stages(&run).expect("render_stages");
    let names: Vec<&str> = r.stages.iter().map(|(n, _)| *n).collect();
    assert_eq!(names, ["screen", "phosphor", "optics"]);
    assert_eq!(r.native.width, 0, "decode did not run");
    let screen = r.stage("screen").unwrap();
    assert_eq!((screen.width, screen.height), (16, 8));
    for y in 0..8 {
        for x in 0..16 {
            let want = amp.pixel(x, y / 2);
            assert_eq!(screen.pixel(x, y)[..3], want[..3], "{test}: screen ({x}, {y})");
        }
    }
    // Bypassed slots after `screen` pass its image on unchanged.
    assert_eq!(r.stage("optics"), Some(screen));
    // The target: the tube fills it (letterbox of 8x4 into 16x8), values clipped to [0, 1].
    for y in 0..8 {
        for x in 0..16 {
            let want = screen.pixel(x, y).map(|v| v.clamp(0.0, 1.0));
            let got = r.output.pixel(x, y);
            for c in 0..3 {
                assert!((got[c] - want[c]).abs() < 1e-6, "{test}: target ({x}, {y}) {c}");
            }
        }
    }

    // Injected as the decode output with every slot off: `amp` holds the zero-order hold (K = 2) and
    // `signal` passes the injected image on.
    gpu.enable_only(&[]).unwrap();
    let native = pattern(nw, nh);
    let run = StageRun {
        input: StageInput::Image {
            after: "decode",
            image: &native,
        },
        ..run
    };
    let r = gpu.render_stages(&run).expect("render_stages after decode");
    assert_eq!(r.stage("signal"), Some(&native));
    let amp_out = r.stage("amp").unwrap();
    assert_eq!((amp_out.width, amp_out.height), (16, 4));
    for y in 0..nh {
        for x in 0..16 {
            assert_eq!(amp_out.pixel(x, y), native.pixel(x / 2, y), "{test}: amp ({x}, {y})");
        }
    }
    // A wrong injected size is an error, not a GPU fault.
    let bad = pattern(3, 3);
    let run = StageRun {
        input: StageInput::Image {
            after: "gun",
            image: &bad,
        },
        ..run
    };
    assert!(gpu.render_stages(&run).is_err());
    let run = StageRun {
        input: StageInput::Image {
            after: "nope",
            image: &bad,
        },
        ..run
    };
    assert!(gpu.render_stages(&run).is_err());
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

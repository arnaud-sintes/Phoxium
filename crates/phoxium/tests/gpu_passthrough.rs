//! GPU test: the passthrough pass round-trips every 8-bit code through the headless renderer,
//! with zero validation messages (TEC-7 acceptance).

mod common;

use phoxium::gpu::TargetFormat;

const W: u32 = 256;
const H: u32 = 4;

/// Codes (r, g, b) of pixel (x, y): every channel takes all 256 codes on each row, in different
/// orders, and blue shifts by 64 per row so a row swap or a flip is detected.
fn codes(x: u32, y: u32) -> [u8; 3] {
    [x as u8, (255 - x) as u8, ((x + 64 * y) % 256) as u8]
}

/// A W x H BGRA8 ramp image, alpha 255.
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

/// Checks that every pixel of `format` output reads back as its code: `round(v * 255) == code`
/// (FP16 cannot hold k / 255 exactly), and `|v * 255 - code| <= tol`.
fn check(format: TargetFormat, tol: f32) {
    let test = format!("gpu_passthrough {format:?}");
    let Some(mut gpu) = common::gpu_or_skip(&test) else {
        return;
    };
    // Decode and encode only: the beam (T-36a) has its own tests.
    gpu.enable_only(&[]).expect("bypass every slot");
    let counter = gpu.validation_counter();
    eprintln!("{test}: on {}", gpu.context().gpu.name);
    let out = gpu.passthrough(W, H, &ramp(), format).expect("headless passthrough");
    // A second call checks that the command buffer, fence and descriptor set are reusable.
    let again = gpu
        .passthrough(W, H, &ramp(), format)
        .expect("second headless passthrough");
    drop(gpu);

    assert_eq!((out.width, out.height), (W, H));
    assert_eq!(out.rgba.len(), (W * H * 4) as usize);
    assert_eq!(out, again);
    for y in 0..H {
        for x in 0..W {
            let p = out.pixel(x, y);
            for (c, &code) in codes(x, y).iter().enumerate() {
                let v = p[c] * 255.0;
                assert_eq!(
                    v.round(),
                    f32::from(code),
                    "pixel ({x}, {y}) channel {c}: {} -> {v}",
                    code
                );
                assert!(
                    (v - f32::from(code)).abs() <= tol,
                    "pixel ({x}, {y}) channel {c}: {code} -> {v}"
                );
            }
            assert_eq!(p[3], 1.0, "pixel ({x}, {y}) alpha");
        }
    }
    common::assert_no_validation_messages(&counter);
}

#[test]
fn ramp_round_trips_through_rgba16f() {
    // FP16 has 11 significant bits, so one ULP in [0.5, 1) is 2^-11. Vulkan lets the conversion to
    // FP16 round to nearest or toward zero (the NVIDIA driver truncates: 254 reads 253.879), so the
    // error on v * 255 stays below one ULP, 255 * 2^-11 = 0.1245.
    check(TargetFormat::Rgba16F, 0.125);
}

#[test]
fn ramp_round_trips_through_rgba32f() {
    // The pipeline keeps the native raster in FP16 (A10, T-20a), so an FP32 target carries the same
    // FP16 error as above: below 255 * 2^-11 = 0.1245 on v * 255.
    check(TargetFormat::Rgba32F, 0.125);
}

#[test]
fn ramp_round_trips_through_bgra8_unorm() {
    // The swapchain format: the float to UNORM conversion rounds to the nearest code, so the FP16
    // error (< 0.125 of a code) disappears and every code comes back exactly.
    check(TargetFormat::Bgra8Unorm, 0.0);
}

/// Code of channel `c` of pixel `i` in the odd-size images: mixes the exact FP16 values 0 and 255
/// with other codes, and differs between neighbours so a stride or row-length mistake shows.
fn odd_code(i: u32, c: u32) -> u8 {
    match (i + c) % 5 {
        0 => 0,
        1 => 255,
        _ => ((i * 37 + c * 91 + 13) % 256) as u8,
    }
}

/// Odd sizes (rows not a multiple of 4 pixels or 16 bytes) through the RGBA16F target: every pixel
/// is the code it was given (0 and 255 exactly, others within the FP16 truncation).
fn check_odd_size(w: u32, h: u32) {
    let test = format!("gpu_passthrough odd {w}x{h}");
    let Some(mut gpu) = common::gpu_or_skip(&test) else {
        return;
    };
    // Decode and encode only: the beam (T-36a) has its own tests.
    gpu.enable_only(&[]).expect("bypass every slot");
    let counter = gpu.validation_counter();
    let mut bgra = Vec::new();
    for i in 0..w * h {
        bgra.extend_from_slice(&[odd_code(i, 2), odd_code(i, 1), odd_code(i, 0), 255]);
    }
    let out = gpu
        .passthrough(w, h, &bgra, TargetFormat::Rgba16F)
        .expect("headless passthrough");
    drop(gpu);

    assert_eq!((out.width, out.height), (w, h));
    assert_eq!(out.rgba.len(), (w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let p = out.pixel(x, y);
            for c in 0..3 {
                let code = odd_code(y * w + x, c);
                let v = p[c as usize] * 255.0;
                assert_eq!(
                    v.round(),
                    f32::from(code),
                    "pixel ({x}, {y}) channel {c}: {code} -> {v}"
                );
                assert!(
                    (v - f32::from(code)).abs() <= 0.125,
                    "pixel ({x}, {y}) channel {c}: {code} -> {v}"
                );
                if code == 0 || code == 255 {
                    assert_eq!(v, f32::from(code), "pixel ({x}, {y}) channel {c} must be exact");
                }
            }
            assert_eq!(p[3], 1.0, "pixel ({x}, {y}) alpha");
        }
    }
    common::assert_no_validation_messages(&counter);
}

#[test]
fn odd_size_3x5_round_trips_through_rgba16f() {
    check_odd_size(3, 5);
}

#[test]
fn single_pixel_round_trips_through_rgba16f() {
    check_odd_size(1, 1);
}

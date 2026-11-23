//! TATE, a tube turned a quarter turn (`source_rotation`, TODO "TATE (rotated tube)"), headless, with the
//! validation layer and zero validation messages.
//!
//! The chain works in the tube's own frame and only `decode` (the upright picture turned into the native raster)
//! and `encode` (the tube image turned into the output) know about the turn, so a turned render must equal, bit
//! for bit, the render of the turned picture on a plain tube whose image is then turned by hand:
//!
//! - the upright picture U (a portrait game, `w` x `h`) turned counter-clockwise reaches the chain as the native
//!   raster R of `h` x `w` with R(nx, ny) = U(ny, h - 1 - nx) (the first scanline is the rightmost column), turned
//!   clockwise as R(nx, ny) = U(w - 1 - ny, nx) (the first scanline is the leftmost column);
//! - the tube image T (the plain render, `A` x `B`) is shown counter-clockwise as the output pixel (x, y) =
//!   T(A - 1 - y, x) (the tube's top on the left), clockwise as (x, y) = T(y, B - 1 - x).
//!
//! The expectations below are written from those two rules, not from the shader code.

mod common;

use phoxium::gpu::{Headless, ImageF32, Settings, SourceFrame, TargetFormat};
use phoxium::params::{Params, SOURCE_ROTATION_CLOCKWISE, SOURCE_ROTATION_COUNTER_CLOCKWISE};
use phoxium::scaling::View;

/// The upright picture: `w` x `h` BGRA, every pixel different and none black.
fn upright(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            out.extend_from_slice(&[(40 + 9 * x + 3 * y) as u8, (60 + 5 * y) as u8, (200 - 7 * x) as u8, 255]);
        }
    }
    out
}

/// The native raster the chain must see for the upright picture (rules in the module documentation).
fn native_raster(u: &[u8], w: u32, h: u32, clockwise: bool) -> Vec<u8> {
    let (nw, nh) = (h, w);
    let mut out = Vec::new();
    for ny in 0..nh {
        for nx in 0..nw {
            let (sx, sy) = if clockwise { (w - 1 - ny, nx) } else { (ny, h - 1 - nx) };
            let i = ((sy * w + sx) * 4) as usize;
            out.extend_from_slice(&u[i..i + 4]);
        }
    }
    out
}

fn render(
    gpu: &mut Headless,
    src: (u32, u32),
    bgra: &[u8],
    view: View,
    params: &Params,
    target: (u32, u32),
) -> ImageF32 {
    gpu.render(
        Some(SourceFrame {
            width: src.0,
            height: src.1,
            bgra,
        }),
        view,
        &Settings::default(),
        params,
        target,
        TargetFormat::Rgba32F,
    )
    .expect("render")
    .output
}

fn turned_matches_plain(clockwise: bool, name: &str) {
    let Some(mut gpu) = common::gpu_or_skip(name) else {
        return;
    };
    let counter = gpu.validation_counter();
    let (w, h) = (12u32, 20u32);
    let u = upright(w, h);
    let r = native_raster(&u, w, h, clockwise);

    // The plain tube: the native raster `h` x `w`, an output of 100 x 60 (the tube's frame).
    let (a, b) = (100u32, 60u32);
    let plain = render(&mut gpu, (h, w), &r, View::full(h, w), &Params::default(), (a, b));

    // The turned tube: the upright picture, the output with its sides swapped, 60 x 100 (a portrait window).
    let params = Params {
        source_rotation: if clockwise {
            SOURCE_ROTATION_CLOCKWISE
        } else {
            SOURCE_ROTATION_COUNTER_CLOCKWISE
        },
        ..Params::default()
    };
    let turned = render(&mut gpu, (w, h), &u, View::full(w, h).turned(true), &params, (b, a));
    assert_eq!((turned.width, turned.height), (b, a));

    let mut lit = 0;
    for y in 0..a {
        for x in 0..b {
            let (tu, tv) = if clockwise { (y, b - 1 - x) } else { (a - 1 - y, x) };
            let want = plain.pixel(tu, tv);
            let got = turned.pixel(x, y);
            assert_eq!(
                got.map(f32::to_bits),
                want.map(f32::to_bits),
                "output ({x}, {y}) shows the tube pixel ({tu}, {tv})"
            );
            if want[0] + want[1] + want[2] > 0.05 {
                lit += 1;
            }
        }
    }
    assert!(lit > 200, "the picture is on screen ({lit} lit pixels)");
    common::assert_no_validation_messages(&counter);
}

#[test]
fn a_tube_turned_counter_clockwise_is_the_plain_tube_turned() {
    turned_matches_plain(false, "a_tube_turned_counter_clockwise_is_the_plain_tube_turned");
}

#[test]
fn a_tube_turned_clockwise_is_the_plain_tube_turned() {
    turned_matches_plain(true, "a_tube_turned_clockwise_is_the_plain_tube_turned");
}

/// The scanlines of a turned tube are the columns of the picture: on a flat white picture the dark gaps between the
/// scanlines show across the output's width when the tube is turned, and down its height when it is not. Counting
/// the local extremes of the brightness along a line of the output tells which way the scanlines run.
#[test]
fn the_scanlines_run_down_the_screen_when_the_tube_is_turned() {
    let Some(mut gpu) = common::gpu_or_skip("the_scanlines_run_down_the_screen_when_the_tube_is_turned") else {
        return;
    };
    let counter = gpu.validation_counter();
    // A flat white picture 16 x 24, so that only the scanline structure varies; about 10 px per line here.
    let (w, h) = (16u32, 24u32);
    let white: Vec<u8> = (0..w * h).flat_map(|_| [255u8, 255, 255, 255]).collect();
    let reversals = |img: &ImageF32, along_x: bool| -> usize {
        // The green channel along the middle line of the picture, in the direction that crosses the scanlines.
        let (len, mid) = if along_x {
            (img.width, img.height / 2)
        } else {
            (img.height, img.width / 2)
        };
        let v: Vec<f32> = (0..len)
            .map(|i| {
                if along_x {
                    img.pixel(i, mid)[1]
                } else {
                    img.pixel(mid, i)[1]
                }
            })
            .collect();
        v.windows(3)
            .filter(|t| (t[1] > t[0] && t[1] >= t[2]) || (t[1] < t[0] && t[1] <= t[2]))
            .count()
    };
    // The plain tube shows the 16 x 24 picture as 24 lines across the output's height.
    let plain = render(
        &mut gpu,
        (w, h),
        &white,
        View::full(w, h),
        &Params::default(),
        (240, 240),
    );
    let params = Params {
        source_rotation: SOURCE_ROTATION_COUNTER_CLOCKWISE,
        ..Params::default()
    };
    let turned = render(
        &mut gpu,
        (w, h),
        &white,
        View::full(w, h).turned(true),
        &params,
        (240, 240),
    );
    let plain_v = reversals(&plain, false);
    let plain_h = reversals(&plain, true);
    let turned_v = reversals(&turned, false);
    let turned_h = reversals(&turned, true);
    eprintln!("reversals: plain down {plain_v} across {plain_h}; turned down {turned_v} across {turned_h}");
    assert!(plain_v > plain_h + 4, "plain: the scanlines are rows");
    assert!(turned_h > turned_v + 4, "turned: the scanlines are columns");
    common::assert_no_validation_messages(&counter);
}

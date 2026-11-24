//! GPU tests of the source pre-filters and the flip (`shaders/decode.comp`, oracle `crt_model::prefilter`):
//! headless, validation on and zero validation messages. A random captured frame is decoded over the native raster
//! of a view with a divisor above 1 and compared with the oracle, pixel by pixel, for every kernel, then with the
//! flip alone (the point sample at the flipped block, bit for bit against the unflipped raster) and with a quarter
//! turn (the flip acts on the upright picture, the independent index formulas below).
//!
//! The decode stores FP16 (round to nearest: at most 2.4e-4 at 0.5 to 1) on top of the f32 arithmetic of the sums;
//! the budget is 5e-4 per channel.

mod common;

use crt_model::prefilter::{filter, Kind};
use phoxium::gpu::{Headless, ImageF32, Settings, SourceFrame, TargetFormat};
use phoxium::params::{
    ParamValue, Params, SOURCE_FLIP_BOTH, SOURCE_FLIP_HORIZONTAL, SOURCE_FLIP_VERTICAL, SOURCE_PREFILTER_BOX,
    SOURCE_PREFILTER_LANCZOS2, SOURCE_PREFILTER_LANCZOS3, SOURCE_PREFILTER_POINT, SOURCE_PREFILTER_SINC,
    SOURCE_PREFILTER_TRIANGLE, SOURCE_ROTATION_COUNTER_CLOCKWISE,
};
use phoxium::scaling::View;

const SRC: (u32, u32) = (40, 24);
const TOL: f64 = 5e-4;

/// A reproducible random BGRA frame, with hard edges and noise so that a wrong kernel shows.
fn frame() -> Vec<u8> {
    let mut s: u32 = 20_261_124;
    let mut v = Vec::new();
    for y in 0..SRC.1 {
        for x in 0..SRC.0 {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let n = (s >> 24) as u8;
            let edge = if (x / 3 + y / 2) % 2 == 0 { 255 } else { 0 };
            v.extend_from_slice(&[n, edge, (x * 6 % 256) as u8, 255]);
        }
    }
    v
}

fn code(bgra: &[u8], x: i64, y: i64) -> [f64; 3] {
    let x = x.clamp(0, i64::from(SRC.0) - 1) as usize;
    let y = y.clamp(0, i64::from(SRC.1) - 1) as usize;
    let i = (y * SRC.0 as usize + x) * 4;
    [bgra[i + 2], bgra[i + 1], bgra[i]].map(|c| f64::from(c) / 255.0)
}

fn decode(gpu: &mut Headless, bgra: &[u8], view: View, params: &Params) -> ImageF32 {
    gpu.render(
        Some(SourceFrame {
            width: SRC.0,
            height: SRC.1,
            bgra,
        }),
        view,
        &Settings::identity(),
        params,
        (view.native.0.max(1), view.native.1.max(1)),
        TargetFormat::Rgba32F,
    )
    .expect("render")
    .native
}

fn params(prefilter: u32, flip: u32, rotation: u32) -> Params {
    let mut p = Params::default();
    for (id, v) in [
        ("source_prefilter", ParamValue::Enum(prefilter)),
        ("source_flip", ParamValue::Enum(flip)),
        ("source_rotation", ParamValue::Enum(rotation)),
    ] {
        assert!(p.set(id, v), "{id}");
    }
    p
}

fn assert_close(got: [f32; 4], want: [f64; 3], what: &str) {
    for c in 0..3 {
        let e = (f64::from(got[c]) - want[c]).abs();
        assert!(
            e <= TOL,
            "{what}: channel {c}: gpu {} oracle {} (error {e})",
            got[c],
            want[c]
        );
    }
}

#[test]
fn every_prefilter_matches_the_oracle() {
    let Some(mut gpu) = common::gpu_or_skip("every_prefilter_matches_the_oracle") else {
        return;
    };
    let counter = gpu.validation_counter();
    let bgra = frame();
    // Two views: the block grid at the corner of the frame, so the windows of the first and last blocks leave the
    // frame (clamped), and a cropped one (the picture area inside bars: the taps must repeat the area's edge, not
    // read the bars around it).
    let views = [
        View {
            origin: (0, 0),
            div: (3, 2),
            native: (13, 12),
        },
        View {
            origin: (4, 2),
            div: (3, 2),
            native: (8, 8),
        },
    ];
    for view in views {
        // The picture area, inclusive, as the shader clamps to it.
        let (div, native) = (view.div, view.native);
        let area = (
            (i64::from(view.origin.0), i64::from(view.origin.1)),
            (
                i64::from(view.origin.0 + native.0 * div.0) - 1,
                i64::from(view.origin.1 + native.1 * div.1) - 1,
            ),
        );
        for (id, kind) in [
            (SOURCE_PREFILTER_POINT, Kind::Point),
            (SOURCE_PREFILTER_BOX, Kind::Box),
            (SOURCE_PREFILTER_TRIANGLE, Kind::Triangle),
            (SOURCE_PREFILTER_LANCZOS2, Kind::Lanczos2),
            (SOURCE_PREFILTER_LANCZOS3, Kind::Lanczos3),
            (SOURCE_PREFILTER_SINC, Kind::Sinc),
        ] {
            let out = decode(&mut gpu, &bgra, view, &params(id, 0, 0));
            assert_eq!((out.width, out.height), native);
            let mut distinct_from_point = 0;
            for ny in 0..i64::from(native.1) {
                for nx in 0..i64::from(native.0) {
                    let start = (
                        i64::from(view.origin.0) + nx * i64::from(div.0),
                        i64::from(view.origin.1) + ny * i64::from(div.1),
                    );
                    let fetch = |x: i64, y: i64| code(&bgra, x, y);
                    let d = (i64::from(div.0), i64::from(div.1));
                    let want = filter(kind, start, d, area, &fetch);
                    assert_close(
                        out.pixel(nx as u32, ny as u32),
                        want,
                        &format!("{kind:?} at ({nx}, {ny}), origin {:?}", view.origin),
                    );
                    let point = filter(Kind::Point, start, d, area, &fetch);
                    distinct_from_point += usize::from((0..3).any(|c| (want[c] - point[c]).abs() > 1e-3));
                }
            }
            // Every filter but the point sample changes a good share of this noisy picture.
            if kind != Kind::Point {
                assert!(
                    distinct_from_point > 30,
                    "{kind:?} changed only {distinct_from_point} pixels"
                );
            }
        }
    } // A divisor of 1 leaves every kernel the identity: the sinc of a 1:1 view is the point sample, bit for bit.
    let one = View::full(SRC.0, SRC.1);
    let plain = decode(&mut gpu, &bgra, one, &params(SOURCE_PREFILTER_POINT, 0, 0));
    let sinc = decode(&mut gpu, &bgra, one, &params(SOURCE_PREFILTER_SINC, 0, 0));
    assert_eq!(plain.rgba, sinc.rgba, "divisor 1: every filter is the identity");
    common::assert_no_validation_messages(&counter);
}

#[test]
fn the_flip_mirrors_the_picture_and_not_the_tube() {
    let Some(mut gpu) = common::gpu_or_skip("the_flip_mirrors_the_picture_and_not_the_tube") else {
        return;
    };
    let counter = gpu.validation_counter();
    let bgra = frame();
    let view = View::full(SRC.0, SRC.1);
    let plain = decode(&mut gpu, &bgra, view, &params(SOURCE_PREFILTER_POINT, 0, 0));
    for (flip, fx, fy) in [
        (SOURCE_FLIP_HORIZONTAL, true, false),
        (SOURCE_FLIP_VERTICAL, false, true),
        (SOURCE_FLIP_BOTH, true, true),
    ] {
        let flipped = decode(&mut gpu, &bgra, view, &params(SOURCE_PREFILTER_POINT, flip, 0));
        for y in 0..SRC.1 {
            for x in 0..SRC.0 {
                let (sx, sy) = (if fx { SRC.0 - 1 - x } else { x }, if fy { SRC.1 - 1 - y } else { y });
                assert_eq!(
                    flipped.pixel(x, y).map(f32::to_bits),
                    plain.pixel(sx, sy).map(f32::to_bits),
                    "flip {flip}: ({x}, {y}) shows ({sx}, {sy})"
                );
            }
        }
    }
    // The flip of the upright picture, with a quarter turn: counter-clockwise reads the upright block (p.y, nx - 1 -
    // p.x) of a raster of nx x ny; the horizontal flip mirrors the upright x, whose range is the native height.
    let turned_view = View::full(SRC.0, SRC.1).turned(true);
    let (nx, ny) = turned_view.native;
    let turned = decode(
        &mut gpu,
        &bgra,
        turned_view,
        &params(SOURCE_PREFILTER_POINT, 0, SOURCE_ROTATION_COUNTER_CLOCKWISE),
    );
    let both = decode(
        &mut gpu,
        &bgra,
        turned_view,
        &params(
            SOURCE_PREFILTER_POINT,
            SOURCE_FLIP_HORIZONTAL,
            SOURCE_ROTATION_COUNTER_CLOCKWISE,
        ),
    );
    for py in 0..ny {
        for px in 0..nx {
            let upright = (py, nx - 1 - px);
            let mirrored = (ny - 1 - upright.0, upright.1);
            assert_close(
                turned.pixel(px, py),
                code(&bgra, i64::from(upright.0), i64::from(upright.1)),
                "turned",
            );
            assert_close(
                both.pixel(px, py),
                code(&bgra, i64::from(mirrored.0), i64::from(mirrored.1)),
                "turned and flipped",
            );
        }
    }
    common::assert_no_validation_messages(&counter);
}

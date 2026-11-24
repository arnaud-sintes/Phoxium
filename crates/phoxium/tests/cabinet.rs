//! GPU test of the cabinet bezel and its reflection in the glass (`shaders/encode.frag`, `shaders/include/cabinet.glsl`,
//! oracle `crt_model::cabinet`): headless, validation on and zero validation messages. Only `encode` runs; the
//! tube image is a flat grey injected after `optics`, in a 200 x 100 target with the 160 x 60 picture centred at
//! (20, 20) (the rectangle set by hand), so the band of 20 px is exactly the surround on every side (`cabinet_bezel_width` 0.2 of the shorter
//! side, 100 px). The host transfer is the identity, so the output is the linear light and the oracle's numbers
//! compare directly: the bezel mixed over the surround colour by its coverage outside the rectangle, the
//! picture's light plus the reflection inside it.

mod common;

use crt_model::cabinet::{inset_px, Cabinet};
use phoxium::gpu::{FrameConstants, Headless, ImageF32, Settings, StageInput, StageRun, TargetFormat};
use phoxium::params::{ParamValue, Params};

const TARGET: (u32, u32) = (200, 100);
const PICTURE: (u32, u32) = (160, 60);
const RECT: [f64; 4] = [20.0, 20.0, 180.0, 80.0];
const GREY: f32 = 0.1;
const SURROUND: [f32; 3] = [0.2, 0.3, 0.4];
const TOL: f64 = 2e-5;

/// The tube image: the picture grown by the skirt `margin` on every side, a flat grey.
fn picture(margin: u32) -> ImageF32 {
    let (w, h) = (PICTURE.0 + 2 * margin, PICTURE.1 + 2 * margin);
    let mut rgba = Vec::new();
    for _ in 0..w * h {
        rgba.extend_from_slice(&[GREY, GREY, GREY, 1.0]);
    }
    ImageF32 {
        width: w,
        height: h,
        rgba,
    }
}

fn params(on: bool) -> Params {
    let mut p = Params::default();
    for (id, v) in [
        ("cabinet_enable", ParamValue::Bool(on)),
        ("cabinet_bezel_width", ParamValue::F32(0.2)),
        ("cabinet_bezel_color", ParamValue::Vec3([0.5, 0.45, 0.4])),
        ("cabinet_bevel", ParamValue::F32(0.5)),
        ("cabinet_corner_radius", ParamValue::F32(0.7)),
        ("cabinet_reflection", ParamValue::F32(0.2)),
        ("source_surround_color", ParamValue::Vec3(SURROUND)),
    ] {
        assert!(p.set(id, v), "{id}");
    }
    p
}

fn render(gpu: &mut Headless, p: &Params) -> phoxium::gpu::Rendered {
    render_with_margin(gpu, p, 0)
}

/// As the live pipeline does: the tube rectangle is the raster rectangle grown by the beam skirt `margin` (the
/// bezel must still be drawn from the raster, `FrameConstants::raster_margin`).
fn render_with_margin(gpu: &mut Headless, p: &Params, margin: u32) -> phoxium::gpu::Rendered {
    // The picture one texel per host pixel, centred: the letterbox of `new` would scale it to the target.
    let mut fc = FrameConstants::new(PICTURE, TARGET, false);
    let m = margin as f32;
    fc.tube_rect_px = [20.0 - m, 20.0 - m, 180.0 + m, 80.0 + m];
    fc.tube_size = [(PICTURE.0 + 2 * margin) as i32, (PICTURE.1 + 2 * margin) as i32];
    fc.raster_margin = [margin as i32, margin as i32];
    let tube = picture(margin);
    let settings = Settings::identity();
    gpu.render_stages(&StageRun {
        input: StageInput::Image {
            after: "optics",
            image: &tube,
        },
        frame: fc,
        settings: &settings,
        params: p,
        format: TargetFormat::Rgba32F,
    })
    .expect("render_stages")
}

fn oracle(p: &Params) -> Cabinet {
    Cabinet {
        band_px: inset_px(f64::from(p.cabinet_bezel_width), TARGET),
        colour: p.cabinet_bezel_color.map(f64::from),
        bevel: f64::from(p.cabinet_bevel),
        corner: f64::from(p.cabinet_corner_radius),
        reflection: f64::from(p.cabinet_reflection),
    }
}

#[test]
fn the_bezel_and_the_reflection_match_the_oracle() {
    let Some(mut gpu) = common::gpu_or_skip("the_bezel_and_the_reflection_match_the_oracle") else {
        return;
    };
    let counter = gpu.validation_counter();
    let on = params(true);
    let cab = oracle(&on);
    assert_eq!(cab.band_px, 20.0);
    let out = render(&mut gpu, &on).output;
    let (mut max, mut worst, mut bezel_px, mut reflect_px) = (0.0_f64, (0, 0), 0, 0);
    for y in 0..TARGET.1 {
        for x in 0..TARGET.0 {
            let p = [f64::from(x) + 0.5, f64::from(y) + 0.5];
            let inside = p[0] >= RECT[0] && p[0] < RECT[2] && p[1] >= RECT[1] && p[1] < RECT[3];
            let want: [f64; 3] = if inside {
                let r = cab.reflection(p, RECT);
                reflect_px += usize::from(r[0] > 0.0);
                [0, 1, 2].map(|c| f64::from(GREY) + r[c])
            } else {
                let b = cab.bezel(p, RECT);
                bezel_px += usize::from(b.coverage > 0.0);
                [0, 1, 2].map(|c| {
                    let lin = b.linear[c].min(1.0);
                    f64::from(SURROUND[c]) * (1.0 - b.coverage) + lin * b.coverage
                })
            };
            let got = out.pixel(x, y);
            for c in 0..3 {
                let e = (f64::from(got[c]) - want[c]).abs();
                if e > max {
                    max = e;
                    worst = (x, y);
                }
            }
        }
    }
    println!("cabinet: max {max:.2e} at {worst:?}, {bezel_px} bezel pixels, {reflect_px} reflecting pixels");
    if max > TOL {
        for (x, y) in [worst, (10, 50), (50, 10), (50, 90), (190, 50), (13, 12), (100, 5)] {
            let p = [f64::from(x) + 0.5, f64::from(y) + 0.5];
            println!("  {x},{y}: got {:?} oracle {:?}", out.pixel(x, y), cab.bezel(p, RECT));
        }
    }
    assert!(max <= TOL, "max error {max} at {worst:?}");
    // The numbers are not all trivial: a band of about 2 x (200 + 100) x 20 minus the cut corners, and a
    // reflection ring of the same width inside the picture.
    assert!(bezel_px > 10_000 && reflect_px > 4_000, "{bezel_px} {reflect_px}");
    // Named pixels: the face is not the surround, the cut corner is.
    let corner = out.pixel(0, 0);
    assert_eq!(
        [corner[0], corner[1], corner[2]],
        SURROUND,
        "the square corner is cut: the surround shows"
    );
    let face = out.pixel(10, 50);
    assert!(face[0] > 0.1 && (face[0] - SURROUND[0]).abs() > 0.01, "{face:?}");
    // Off: the surround is the clear colour everywhere outside the picture, and the picture is untouched.
    let off = render(&mut gpu, &params(false)).output;
    for (x, y) in [(0, 0), (10, 50), (190, 50), (100, 5), (100, 95)] {
        let v = off.pixel(x, y);
        assert_eq!([v[0], v[1], v[2]], SURROUND, "off, outside at {x},{y}");
    }
    for (x, y) in [(20, 20), (21, 50), (100, 22), (179, 79)] {
        let v = off.pixel(x, y);
        assert!(
            (f64::from(v[0]) - f64::from(GREY)).abs() < 1e-6,
            "off, picture at {x},{y}: {v:?}"
        );
    }
    common::assert_no_validation_messages(&counter);
}

/// With the beam's skirt margin (as the live pipeline places it: the tube rectangle is the raster grown by the
/// margin) the bezel's inner edge is the raster's edge, not the grown rectangle's: the skirt strip is covered by the
/// bezel and the reflection fades in over the picture. 8 px of margin on a 160 x 60 raster: the strip between
/// x = 12 and 20 (and 180 to 188, y likewise) shows the bezel, the oracle's, exactly as without a margin.
#[test]
fn the_bezel_is_drawn_from_the_raster_not_the_skirt() {
    let Some(mut gpu) = common::gpu_or_skip("the_bezel_is_drawn_from_the_raster_not_the_skirt") else {
        return;
    };
    let counter = gpu.validation_counter();
    let on = params(true);
    let cab = oracle(&on);
    let with = render_with_margin(&mut gpu, &on, 8).output;
    let without = render(&mut gpu, &on).output;
    let mut strip = 0;
    for y in 0..TARGET.1 {
        for x in 0..TARGET.0 {
            let p = [f64::from(x) + 0.5, f64::from(y) + 0.5];
            let inside = p[0] >= RECT[0] && p[0] < RECT[2] && p[1] >= RECT[1] && p[1] < RECT[3];
            let want: [f64; 3] = if inside {
                let r = cab.reflection(p, RECT);
                [0, 1, 2].map(|c| f64::from(GREY) + r[c])
            } else {
                let b = cab.bezel(p, RECT);
                strip += usize::from(b.coverage > 0.0 && p[0] >= 12.0 && p[0] < 188.0 && p[1] >= 12.0 && p[1] < 88.0);
                [0, 1, 2].map(|c| f64::from(SURROUND[c]) * (1.0 - b.coverage) + b.linear[c].min(1.0) * b.coverage)
            };
            let (a, b) = (with.pixel(x, y), without.pixel(x, y));
            for c in 0..3 {
                assert!(
                    (f64::from(a[c]) - want[c]).abs() <= TOL,
                    "with margin at ({x}, {y}) channel {c}: {} vs oracle {}",
                    a[c],
                    want[c]
                );
                assert_eq!(a[c], b[c], "the margin changes nothing at ({x}, {y})");
            }
        }
    }
    // The strip is a real part of the test: the bezel covers 8 px around the raster inside the grown rectangle.
    assert!(strip > 2_000, "{strip}");
    common::assert_no_validation_messages(&counter);
}

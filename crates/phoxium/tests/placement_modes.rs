//! Host scale modes of the placement (T-96, PLAN D-13 and D-18): `source_scale_mode` = native, integer,
//! fit, fit_free against the actual output size, with no assumed host resolution, plus the `tube` default.
//!
//! Pure CPU tests over a matrix of host sizes x native sizes x modes against the independent oracle
//! (`crt_model::tube::host_rows_scale`, a counting search, and `host_px_per_mm`), hand-derived cases, and
//! headless GPU tests (validation layer, zero messages) that render a pattern in each mode and check the
//! picture rectangle, the surround and the whole-pixel block structure on the output.
//!
//! # Hand derivations (PVM-20M4U class tube: 290 mm high, 4:3; PAR 1 unless stated)
//!
//! k is the number of host px per source row; the picture is round(k native_w) x k native_h (PAR 1); the
//! offsets are floor((target - picture) / 2); px per mm = k native_h / 290 (the raster fills the tube
//! height when the source is not wider than 4:3), so a 0.31 mm triad is 0.31 px per mm host px.
//! - 256 x 240 on 1920 x 1080: 1080 / 240 = 4.5, 1920 / 256 = 7.5.
//!   - native: 256 x 240 at (832, 420), 240 / 290 = 0.827586 px/mm;
//!   - integer auto and fit: k = 4, 1024 x 960 at (448, 60), 960 / 290 = 3.310345 px/mm, 1.026207 px per
//!     triad, scanline 4 px;
//!   - fit_free: k = 4.5, 1152 x 1080 at (384, 0), 1080 / 290 = 3.724138 px/mm, scanline 4.5 px;
//!   - tube (the default): 1080 / 290 = 3.724138 px/mm, 1152 x 1080 at (384, 0): the same picture as
//!     fit_free here, since the tube fills the output height;
//!   - integer x10: does not fit, clamped to 4 (reported);
//!   - integer auto, top-left: 1024 x 960 at (0, 0).
//! - 256 x 240 PAR 8:7 on 1920 x 1080, integer auto: aspect 256 x 8/7 / 240 = 1.219048, 1920 / (240 x
//!   1.219048) = 6.5625, so k = 4 (the height limits): 960 rows, round(4 x 292.571) = round(1170.29) = 1170
//!   columns at (375, 60). Rows are whole (4 px), columns are not (4.5714 px): not integer-aligned.
//! - 320 x 240 on 2560 x 1440: k = 6 (1440 / 240; 2560 / 320 = 8), 1920 x 1440 at (320, 0), 4.965517 px/mm.
//! - 640 x 480 on 3440 x 1440 (ultrawide): k = 3 (1440 / 480; 3440 / 640 = 5.375), 1920 x 1440 at (760, 0).
//! - 256 x 240 on 1080 x 1920 (portrait): 1080 / 256 = 4.219, 1920 / 240 = 8, so k = 4, 1024 x 960 at (28,
//!   480): the width limits.
//! - 256 x 240 on 3840 x 2160: 2160 / 240 = 9 exactly, k = 9, 2304 x 2160 at (768, 0).
//! - 1920 x 1080 on 1366 x 768: 1366 / 1920 = 0.7115, 768 / 1080 = 0.7111, no whole factor fits: integer
//!   and native report a downscale, k = 0.711111, round(1920 x 0.711111) = round(1365.33) = 1365 x 768 at
//!   (0, 0); fit falls back to the same free fit as documented (reported as fitting).
//! - 1280 x 720 on 1280 x 720, native: 1280 x 720 at (0, 0).

mod common;

use crt_model::tube::{self, HostScale};
use phoxium::gpu::{tube_rect_letterbox, Headless, Rendered, Settings, SourceFrame, TargetFormat};
use phoxium::params::{
    ParamValue, Params, SOURCE_SCALE_MODE_FIT, SOURCE_SCALE_MODE_FIT_FREE, SOURCE_SCALE_MODE_INTEGER,
    SOURCE_SCALE_MODE_NATIVE, SOURCE_SCALE_MODE_TUBE,
};
use phoxium::placement::{place, readout, Fit, PlacementInput, ScaleMode};
use phoxium::scaling::View;

/// The host sizes of PLAN D-18: 720p to 4K, 16:10, ultrawide and portrait.
const HOSTS: [(u32, u32); 10] = [
    (1280, 720),
    (1366, 768),
    (1600, 900),
    (1920, 1080),
    (1920, 1200),
    (2560, 1080),
    (2560, 1440),
    (3440, 1440),
    (3840, 2160),
    (1080, 1920),
];

const NATIVES: [(u32, u32); 5] = [(256, 240), (320, 240), (640, 480), (1280, 720), (1920, 1080)];

/// The host modes with their oracle counterpart.
const MODES: [(ScaleMode, HostScale); 6] = [
    (ScaleMode::Native, HostScale::Native),
    (ScaleMode::Integer(0), HostScale::Integer(0)),
    (ScaleMode::Integer(3), HostScale::Integer(3)),
    (ScaleMode::Integer(10), HostScale::Integer(10)),
    (ScaleMode::Fit, HostScale::Fit),
    (ScaleMode::FitFree, HostScale::FitFree),
];

fn params_for(mode: ScaleMode, center: bool) -> Params {
    let (e, n) = match mode {
        ScaleMode::Tube => (SOURCE_SCALE_MODE_TUBE, 0),
        ScaleMode::Native => (SOURCE_SCALE_MODE_NATIVE, 0),
        ScaleMode::Integer(n) => (SOURCE_SCALE_MODE_INTEGER, n as i32),
        ScaleMode::Fit => (SOURCE_SCALE_MODE_FIT, 0),
        ScaleMode::FitFree => (SOURCE_SCALE_MODE_FIT_FREE, 0),
        // Not an oracle mode (it has its own unit test in `placement`): the manual pixel size is set by its own
        // parameter, which this helper does not carry.
        ScaleMode::Manual(_) => unreachable!("the manual mode is tested in placement.rs"),
    };
    let mut p = Params::default();
    assert!(p.set("source_scale_mode", ParamValue::Enum(e)));
    assert!(p.set("source_scale", ParamValue::I32(n)));
    assert!(p.set("source_center", ParamValue::Bool(center)));
    p
}

/// x0, y0, w, h of a placement's rectangle.
fn rect(native: (u32, u32), target: (u32, u32), p: &Params) -> [f32; 4] {
    let r = place(&PlacementInput::from_params(p, native, target, false)).rect_px;
    [r[0], r[1], r[2] - r[0], r[3] - r[1]]
}

/// The hand-derived cases of the module documentation.
#[test]
fn hand_derived_cases() {
    let c = |m| params_for(m, true);
    let n = (256, 240);
    let fhd = (1920, 1080);
    assert_eq!(rect(n, fhd, &c(ScaleMode::Native)), [832.0, 420.0, 256.0, 240.0]);
    assert_eq!(rect(n, fhd, &c(ScaleMode::Integer(0))), [448.0, 60.0, 1024.0, 960.0]);
    assert_eq!(rect(n, fhd, &c(ScaleMode::Fit)), [448.0, 60.0, 1024.0, 960.0]);
    assert_eq!(rect(n, fhd, &c(ScaleMode::FitFree)), [384.0, 0.0, 1152.0, 1080.0]);
    assert_eq!(rect(n, fhd, &c(ScaleMode::Tube)), [384.0, 0.0, 1152.0, 1080.0]);
    assert_eq!(
        rect(n, fhd, &params_for(ScaleMode::Integer(0), false)),
        [0.0, 0.0, 1024.0, 960.0]
    );
    let r = readout(&c(ScaleMode::Integer(0)), n, fhd, false);
    assert!((r.px_per_mm - 3.310_345).abs() < 1e-6, "{}", r.px_per_mm);
    assert!((r.px_per_triad - 1.026_207).abs() < 1e-6, "{}", r.px_per_triad);
    assert!((r.line_px - 4.0).abs() < 1e-9);
    assert!(r.integer_aligned && r.fit == Fit::Fits);
    assert_eq!(
        (r.picture_px, r.offset_px, r.px_per_source_px),
        ((1024, 960), (448, 60), (4.0, 4.0))
    );
    let r = readout(&c(ScaleMode::Native), n, fhd, false);
    assert!((r.px_per_mm - 0.827_586).abs() < 1e-6);
    let r = readout(&c(ScaleMode::FitFree), n, fhd, false);
    assert!((r.px_per_mm - 3.724_138).abs() < 1e-6 && (r.line_px - 4.5).abs() < 1e-9 && !r.integer_aligned);
    let r = readout(&c(ScaleMode::Integer(10)), (320, 240), fhd, false);
    assert_eq!(r.fit, Fit::Clamped { requested: 10, used: 4 });
    assert_eq!((r.picture_px, r.offset_px), ((1280, 960), (320, 60)));
    // PAR 8:7: whole rows, not whole columns.
    let mut par = c(ScaleMode::Integer(0));
    assert!(par.set("source_par", ParamValue::F32(8.0 / 7.0)));
    let r = readout(&par, n, fhd, false);
    assert_eq!((r.picture_px, r.offset_px), ((1170, 960), (375, 60)));
    assert!(!r.integer_aligned && r.px_per_source_px.1 == 4.0);
    // The other hosts.
    assert_eq!(
        rect((320, 240), (2560, 1440), &c(ScaleMode::Integer(0))),
        [320.0, 0.0, 1920.0, 1440.0]
    );
    let r = readout(&c(ScaleMode::Integer(0)), (320, 240), (2560, 1440), false);
    assert!((r.px_per_mm - 1440.0 / 290.0).abs() < 1e-12);
    assert_eq!(
        rect((640, 480), (3440, 1440), &c(ScaleMode::Fit)),
        [760.0, 0.0, 1920.0, 1440.0]
    );
    assert_eq!(
        rect(n, (1080, 1920), &c(ScaleMode::Integer(0))),
        [28.0, 480.0, 1024.0, 960.0]
    );
    assert_eq!(rect(n, (3840, 2160), &c(ScaleMode::Fit)), [768.0, 0.0, 2304.0, 2160.0]);
    for m in [ScaleMode::Native, ScaleMode::Integer(0), ScaleMode::Integer(2)] {
        let r = readout(&c(m), (1920, 1080), (1366, 768), false);
        assert_eq!(r.fit, Fit::Downscaled, "{m}");
        assert_eq!((r.picture_px, r.offset_px), ((1365, 768), (0, 0)), "{m}");
    }
    let r = readout(&c(ScaleMode::Fit), (1920, 1080), (1366, 768), false);
    assert_eq!((r.fit, r.picture_px), (Fit::Fits, (1365, 768)));
    assert_eq!(
        rect((1280, 720), (1280, 720), &c(ScaleMode::Native)),
        [0.0, 0.0, 1280.0, 720.0]
    );
}

/// Every host x native x mode against the oracle: k (host px per source row), whether it fitted, the
/// rectangle (inside the output, centred), px per mm from the oracle's raster height, the scanline pitch
/// in host px = k, and in the whole-number modes exactly k x k host pixels per source pixel.
#[test]
fn host_matrix_against_the_oracle() {
    // The table's f32 aspect (1.3333334), as the application reads it.
    let visible = (290.0 * f64::from(Params::default().tube_aspect), 290.0);
    let mut cases = 0;
    for target in HOSTS {
        for native in NATIVES {
            let (nw, nh) = (f64::from(native.0), f64::from(native.1));
            let (tw, th) = (f64::from(target.0), f64::from(target.1));
            let aspect = nw / nh;
            let raster_h = tube::raster_size_mm(visible, aspect).1;
            for (mode, oracle_mode) in MODES {
                let what = format!("{mode} native {native:?} host {target:?}");
                let params = params_for(mode, true);
                let p = place(&PlacementInput::from_params(&params, native, target, false));
                let (k, fits) = tube::host_rows_scale(oracle_mode, nh, aspect, (tw, th));
                assert!(
                    (p.row_px - k).abs() <= 1e-9 * k.max(1.0),
                    "{what}: k {} vs {k}",
                    p.row_px
                );
                assert_eq!(p.fit == Fit::Fits, fits, "{what}: {:?}", p.fit);
                let [x0, y0, x1, y1] = p.rect_px.map(f64::from);
                let (w, h) = (x1 - x0, y1 - y0);
                assert_eq!((w, h), ((k * nw).round(), (k * nh).round()), "{what}");
                assert_eq!((x0, y0), (((tw - w) / 2.0).floor(), ((th - h) / 2.0).floor()), "{what}");
                assert!(
                    x0 >= 0.0 && y0 >= 0.0 && x1 <= tw && y1 <= th,
                    "{what}: inside the output"
                );
                let s = tube::host_px_per_mm(k, nh, raster_h);
                assert!(
                    (p.px_per_mm - s).abs() <= 1e-12 * s,
                    "{what}: px/mm {} vs {s}",
                    p.px_per_mm
                );
                assert!((p.line_pitch_mm * p.px_per_mm - k).abs() < 1e-9, "{what}: scanline px");
                let r = readout(&params, native, target, false);
                if k.fract() == 0.0 {
                    // Whole factor: every source pixel is exactly k x k host pixels.
                    assert_eq!((w, h), (k * nw, k * nh), "{what}");
                    assert!(r.integer_aligned, "{what}");
                    assert_eq!(r.px_per_source_px, (k, k), "{what}");
                } else {
                    assert!(!r.integer_aligned, "{what}");
                }
                if mode.is_whole() && k >= 1.0 {
                    // The free fit is only the fallback below 1.
                    assert!(k.fract() == 0.0, "{what}: whole modes give whole factors");
                }
                assert!((r.px_per_triad - f64::from(params.mask_pitch_mm) * p.px_per_mm).abs() < 1e-12);
                cases += 1;
            }
        }
    }
    assert_eq!(cases, 10 * 5 * 6);
}

/// The `tube` mode (the default) is unchanged by T-96 on every host: px per mm from the oracle's
/// `px_per_mm`, and with PAR 1 the rectangle of the T-20a letterbox, whole or fractional scale.
#[test]
fn tube_mode_is_unchanged_on_every_host() {
    let params = Params::default();
    assert_eq!(params.source_scale_mode, SOURCE_SCALE_MODE_TUBE, "tube is the default");
    // The table's f32 aspect (1.3333334), as the application reads it.
    let visible = (290.0 * f64::from(Params::default().tube_aspect), 290.0);
    for target in HOSTS {
        for native in NATIVES {
            for integer in [false, true] {
                let p = place(&PlacementInput::from_params(&params, native, target, integer));
                assert_eq!(
                    p.rect_px,
                    tube_rect_letterbox(native, target, integer),
                    "{native:?} {target:?}"
                );
                assert_eq!(p.fit, Fit::Fits);
                if !integer {
                    let aspect = f64::from(native.0) / f64::from(native.1);
                    let s = tube::px_per_mm(
                        tube::raster_size_mm(visible, aspect),
                        (f64::from(target.0), f64::from(target.1)),
                        1.0,
                    );
                    assert!((p.px_per_mm - s).abs() <= 1e-12 * s);
                }
            }
        }
    }
}

/// A `w` x `h` BGRA8 image whose neighbouring pixels differ in every channel.
fn pattern(w: u32, h: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = code(x, y);
            v.extend_from_slice(&[b, g, r, 255]);
        }
    }
    v
}

fn code(x: u32, y: u32) -> [u8; 3] {
    [
        (x * 17 + y * 3 + 1) as u8,
        (x * 5 + y * 29 + 40) as u8,
        (x * 11 + y * 7 + 100) as u8,
    ]
}

fn render(gpu: &mut Headless, native: (u32, u32), params: &Params, target: (u32, u32)) -> Rendered {
    let bgra = pattern(native.0, native.1);
    gpu.render(
        Some(SourceFrame {
            width: native.0,
            height: native.1,
            bgra: &bgra,
        }),
        View::full(native.0, native.1),
        &Settings::identity(),
        params,
        target,
        TargetFormat::Bgra8Unorm,
    )
    .expect("headless render")
}

/// Asserts the output: inside `[x0, y0, w, h]` source pixel (floor((x - x0) / k), floor((y - y0) / k)),
/// exactly (k whole); outside the surround code.
fn assert_blocks(r: &Rendered, rect: [u32; 4], k: u32, surround: [u8; 3], what: &str) {
    let out = &r.output;
    for y in 0..out.height {
        for x in 0..out.width {
            let p = out.pixel(x, y);
            let got = [p[0], p[1], p[2]].map(|v| (v * 255.0).round() as u8);
            let inside = x >= rect[0] && x < rect[0] + rect[2] && y >= rect[1] && y < rect[1] + rect[3];
            let want = if inside {
                code((x - rect[0]) / k, (y - rect[1]) / k)
            } else {
                surround
            };
            assert_eq!(got, want, "{what}: pixel ({x}, {y}), inside {inside}");
        }
    }
}

/// The modes through the real pipeline (every slot bypassed: an exact identity, as in `gpu_identity`), a
/// 32 x 24 pattern on a 200 x 150 output: 200 / 32 = 150 / 24 = 6.25.
/// - integer auto (and fit): k = 6, 192 x 144 at (4, 3), every source pixel a 6 x 6 block;
/// - native: 32 x 24 at (84, 63);
/// - integer x2 aligned top-left: 64 x 48 at (0, 0);
/// - native with the surround colour (0.2, 0.4, 0.6): codes 51, 102, 153 outside the picture;
/// - fit_free: k = 6.25, the whole 200 x 150 output is the picture (nearest resample, blocks of 6 or 7);
/// - tube: 150 / 290 px/mm, the 4:3 picture fills the 4:3 output (the T-20a letterbox).
#[test]
fn modes_on_the_headless_output() {
    let test = "placement_modes headless";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.enable_only(&[]).expect("bypass every slot");
    let native = (32, 24);
    let target = (200, 150);
    let with = |mode, center| {
        let m = params_for(mode, center);
        Params {
            source_scale_mode: m.source_scale_mode,
            source_scale: m.source_scale,
            source_center: m.source_center,
            ..Params::identity_gun()
        }
    };
    let r = render(&mut gpu, native, &with(ScaleMode::Integer(0), true), target);
    assert_blocks(&r, [4, 3, 192, 144], 6, [0; 3], "integer auto");
    let r = render(&mut gpu, native, &with(ScaleMode::Fit, true), target);
    assert_blocks(&r, [4, 3, 192, 144], 6, [0; 3], "fit");
    let r = render(&mut gpu, native, &with(ScaleMode::Native, true), target);
    assert_blocks(&r, [84, 63, 32, 24], 1, [0; 3], "native");
    let r = render(&mut gpu, native, &with(ScaleMode::Integer(2), false), target);
    assert_blocks(&r, [0, 0, 64, 48], 2, [0; 3], "integer x2 top-left");
    let mut grey = with(ScaleMode::Native, true);
    grey.source_surround_color = [0.2, 0.4, 0.6];
    let r = render(&mut gpu, native, &grey, target);
    assert_blocks(&r, [84, 63, 32, 24], 1, [51, 102, 153], "surround colour");
    // fit_free: the picture covers the output; column x shows source column floor(x / 6.25) up to the
    // nearest-resample rounding, so check the coverage and the source pixel at block centres.
    let r = render(&mut gpu, native, &with(ScaleMode::FitFree, true), target);
    for (sx, sy) in [(0, 0), (5, 7), (31, 23), (16, 12)] {
        let (x, y) = ((sx as f64 + 0.5) * 6.25, (sy as f64 + 0.5) * 6.25);
        let p = r.output.pixel(x as u32, y as u32);
        assert_eq!(
            [p[0], p[1], p[2]].map(|v| (v * 255.0).round() as u8),
            code(sx, sy),
            "fit_free ({sx}, {sy})"
        );
    }
    let corner = |x, y| r.output.pixel(x, y)[..3].iter().any(|&v| v > 0.0);
    assert!(corner(0, 0) && corner(199, 149), "fit_free covers the output");
    // tube (default): the 4:3 picture fills the 4:3 tube and the 4:3 output, the whole output too.
    let fc = phoxium::gpu::FrameConstants::placed(native, target, false, &with(ScaleMode::Tube, true));
    assert_eq!(fc.tube_rect_px, [0.0, 0.0, 200.0, 150.0]);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

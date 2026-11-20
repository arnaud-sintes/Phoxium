//! Chromatic aberration controls (QUESTIONS PHY-17, `params/geometry.toml` group "Chromatic aberration",
//! `shaders/include/geometry.glsl` `geometry_convergence_px`, oracle `crt_model::geometry::Chroma`), GPU tests,
//! headless, validation on, zero validation messages. The bit-exact check of the defaults against the build
//! before the controls is `tests/stage_chroma_reference.rs`.
//!
//! The tests inject a beam-current image after `gun` and run `screen` alone, mask off, small round spot
//! (FWHM 1.2 mm, no growth, no tail; sigma 1.02 px at 2 px per mm), as `tests/stage_geometry.rs`. Expected values
//! come from the `crt-model` oracle (`Chroma::offsets_mm`, `sphere_seen_mm`, `cylinder_seen_mm`); nothing repeats
//! the shader's formula. Hand derivations:
//!
//! C1. Picture 48 x 36 native at 6 px per native pixel, 2 px per mm: 288 x 216 px, 144 x 108 mm, half (72, 54)
//!     mm, half diagonal rc = 90 mm, centre (144, 108) px. A lit native pixel (k, l) has its box centre at
//!     ((k + 1/2) 6, (l + 1/2) 6) px; green lands there (flat) or where the faceplate shows it (curved).
//! C2. The offsets are evaluated where the light lands (the convergence hook's convention, stage_geometry G6b):
//!     red lands at p solving p = seen(s + red(p)), blue at p = seen(s - red(p)), s the raster point (mm on the
//!     phosphor surface), seen the faceplate's map to the view (identity when flat), red(p) = half the axis
//!     amount along the angle plus half the lateral amount times (|p| / rc)^e along p / |p|: the beams land
//!     apart on the phosphor, and a curved faceplate shows that separation foreshortened like the rest of the
//!     raster (about 3 % at the corner of the 300 mm sphere). Solved by fixed-point iteration in f64. Corner
//!     dot (3, 3): box centre (21, 21) px, g = (-61.5, -43.5) mm, |g| = 75.33 mm, (|g| / rc)^2 = 0.7006: with lateral 4 mm (e = 2) and axis 1 mm at 30 deg, red moves by about 1.40 mm
//!     outward plus (0.433, 0.25) mm, about 3.3 px in all; blue the opposite.
//! C3. No clipping at the hard bounds: axis 20 mm at 180 deg moves red 10 mm (20 px) left everywhere and blue
//!     20 px right. A square raster of 32 x 32 native at 6 px (192 px) on a 4:3 tube has its visible opening
//!     192 x 4 / 3 = 256 px wide, 32 px past each side of the raster; the skirt margin (4 sigma at twice the
//!     peak plus the largest misconvergence, 10 mm) is ceil((4 x 0.5096 + 10) x 2) = 25 px. A dot on the
//!     raster's first column then lands its red light 20 px left of the raster, inside the margin and the
//!     opening: the red energy equals the green energy. With the former margin (no chromatic term:
//!     ceil(4 x 0.5096 x 2) = 5 px) the red light is cut.

mod common;

use crt_model::geometry::{cylinder_seen_mm, sphere_seen_mm, Chroma};
use ultimatecrt::gpu::{Domain, FrameConstants, Headless, ImageF32, Settings, StageInput, StageRun, TargetFormat};
use ultimatecrt::params::{
    Params, BEAM_QUALITY_REFERENCE, GEOMETRY_CURVATURE_CYLINDRICAL, GEOMETRY_CURVATURE_SPHERICAL, QUALITY_TIER_EXACT,
    QUALITY_TIER_FAST, QUALITY_TIER_LOW, QUALITY_TIER_STANDARD,
};
use ultimatecrt::placement::chroma;

const PPM: f64 = 2.0;
const N: u32 = 6;
const WHITE: [f32; 3] = [1.0; 3];
/// Curvature radius of the curved cases (mm): a strong 300 mm on the 144 x 108 mm picture of C1.
const R: f64 = 300.0;

/// A tube of `cols` x `rows` native pixels at N host px per native pixel, PPM px per mm.
fn tube(cols: u32, rows: u32) -> FrameConstants {
    let mut fc = FrameConstants::new((cols, rows), (cols * N, rows * N), false);
    fc.px_per_mm = PPM as f32;
    fc.line_pitch_mm = (f64::from(N) / PPM) as f32;
    fc
}

/// Small round spot (FWHM 1.2 mm, no growth, no tail), mask off.
fn base() -> Params {
    Params {
        beam_fwhm_mid_mm: 1.2,
        beam_fwhm_peak_mm: 1.2,
        beam_tail_weight: 0.0,
        mask_enabled: false,
        ..Params::default()
    }
}

/// The amp-domain current image of `fc`: white on the native pixels `lit`, black elsewhere.
fn dots(fc: &FrameConstants, lit: &[(u32, u32)]) -> ImageF32 {
    let (w, h) = fc.domain_size(Domain::Amp);
    let k = fc.amp_k as u32;
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = if lit.contains(&(x / k, y)) { WHITE } else { [0.0; 3] };
            rgba.extend_from_slice(&[r, g, b, 1.0]);
        }
    }
    ImageF32 {
        width: w,
        height: h,
        rgba,
    }
}

/// Runs `screen` alone on `amp` injected after `gun`; returns its tube image.
fn run(gpu: &mut Headless, fc: FrameConstants, params: &Params, amp: &ImageF32) -> ImageF32 {
    gpu.enable_only(&["screen"]).unwrap();
    let settings = Settings::identity();
    let run = StageRun {
        input: StageInput::Image {
            after: "gun",
            image: amp,
        },
        frame: fc,
        settings: &settings,
        params,
        format: TargetFormat::Rgba32F,
    };
    gpu.render_stages(&run)
        .expect("render_stages")
        .stage("screen")
        .expect("screen ran")
        .clone()
}

/// Light centroid and energy of channel `c` within `half` px of `centre` (pixel centres at index + 0.5).
fn centroid(img: &ImageF32, c: usize, centre: (f64, f64), half: i64) -> ((f64, f64), f64) {
    let (mut e, mut sx, mut sy) = (0.0, 0.0, 0.0);
    let clamp = |v: i64, len: u32| v.clamp(0, i64::from(len)) as u32;
    let (cx, cy) = (centre.0.round() as i64, centre.1.round() as i64);
    for y in clamp(cy - half, img.height)..clamp(cy + half, img.height) {
        for x in clamp(cx - half, img.width)..clamp(cx + half, img.width) {
            let v = f64::from(img.pixel(x, y)[c]);
            e += v;
            sx += v * (f64::from(x) + 0.5);
            sy += v * (f64::from(y) + 0.5);
        }
    }
    assert!(e > 0.0, "no light in the window around {centre:?}");
    ((sx / e, sy / e), e)
}

/// C2: where beam `c` lands and is seen (mm from the centre) for the raster point `s` (mm, on the phosphor
/// surface): the beam lands `offset_c` (mm on the surface) away from `s`, the offset taken at the seen landing
/// point, so p = seen(s + offset_c(p)); `seen` maps the surface to the view (identity on a flat tube).
fn landing(
    ch: &Chroma,
    c: usize,
    s: (f64, f64),
    half: (f64, f64),
    seen: &dyn Fn((f64, f64)) -> (f64, f64),
) -> (f64, f64) {
    let mut p = seen(s);
    for _ in 0..100 {
        let o = ch.offsets_mm(p, half)[c];
        p = seen((s.0 + o.0, s.1 + o.1));
    }
    p
}

/// The beam paths under test: name and the parameters that select it on top of `p`.
fn paths(p: &Params) -> Vec<(&'static str, Params, f64)> {
    let tier = |t: u32| Params { quality_tier: t, ..*p };
    let curved = |c: u32, t: u32| Params {
        geometry_curvature: c,
        geometry_radius_mm: R as f32,
        quality_tier: t,
        ..*p
    };
    vec![
        ("flat, exact (tile path)", tier(QUALITY_TIER_EXACT), 0.05),
        ("flat, standard (line images)", tier(QUALITY_TIER_STANDARD), 0.05),
        ("flat, fast", tier(QUALITY_TIER_FAST), 0.05),
        ("flat, low", tier(QUALITY_TIER_LOW), 0.05),
        (
            "flat, reference beam",
            Params {
                beam_quality: BEAM_QUALITY_REFERENCE,
                ..*p
            },
            0.05,
        ),
        (
            "spherical, standard (warp path)",
            curved(GEOMETRY_CURVATURE_SPHERICAL, QUALITY_TIER_STANDARD),
            0.05,
        ),
        (
            "spherical, low (warp path)",
            curved(GEOMETRY_CURVATURE_SPHERICAL, QUALITY_TIER_LOW),
            0.05,
        ),
        (
            "cylindrical, standard (warp path)",
            curved(GEOMETRY_CURVATURE_CYLINDRICAL, QUALITY_TIER_STANDARD),
            0.05,
        ),
        (
            "spherical, reference beam",
            Params {
                beam_quality: BEAM_QUALITY_REFERENCE,
                ..curved(GEOMETRY_CURVATURE_SPHERICAL, QUALITY_TIER_STANDARD)
            },
            0.05,
        ),
    ]
}

/// C1, C2: a white-on-black grid of dots (the centre and the four corners); red and blue land where the oracle
/// puts them, relative to green, in every beam path (tile, line images, every tier, reference, curved warp),
/// for a lateral term of each edge exponent plus a constant term along an angle.
#[test]
fn red_and_blue_land_where_the_oracle_puts_them_in_every_path() {
    let test = "stage_chroma landing";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let fc = tube(48, 36);
    let centre = (144.0, 108.0);
    let half = (72.0, 54.0);
    let grid = [(3_u32, 3_u32), (44, 3), (3, 32), (44, 32), (23, 17)];
    let amp = dots(&fc, &grid);
    let mut worst = 0.0_f64;
    let mut cases = 0;
    for (lateral, axis, angle, e) in [
        (4.0_f32, 1.0_f32, 30.0_f32, 2.0_f32),
        (-3.0, 0.0, 0.0, 1.0),
        (5.0, 0.6, -120.0, 3.0),
    ] {
        let p = Params {
            geometry_chroma_lateral_mm: lateral,
            geometry_chroma_axis_mm: axis,
            geometry_chroma_angle_deg: angle,
            geometry_chroma_edge_exponent: e,
            ..base()
        };
        let ch = chroma(&p);
        for (name, q, tol) in paths(&p) {
            let out = run(&mut gpu, fc, &q, &amp);
            for &(k, l) in &grid {
                let s = (
                    ((f64::from(k) + 0.5) * f64::from(N) - centre.0) / PPM,
                    ((f64::from(l) + 0.5) * f64::from(N) - centre.1) / PPM,
                );
                let curvature = q.geometry_curvature;
                let seen = move |m: (f64, f64)| match curvature {
                    GEOMETRY_CURVATURE_SPHERICAL => sphere_seen_mm(R, m, 0.0),
                    GEOMETRY_CURVATURE_CYLINDRICAL => cylinder_seen_mm(R, m),
                    _ => m,
                };
                let g = seen(s);
                let px = |m: (f64, f64)| (centre.0 + m.0 * PPM, centre.1 + m.1 * PPM);
                let (gc, _) = centroid(&out, 1, px(g), 16);
                for (c, cname) in [(0, "red"), (2, "blue")] {
                    let want = px(landing(&ch, c, s, half, &seen));
                    let (got, _) = centroid(&out, c, want, 16);
                    // Displacement from green, measured and expected (px).
                    let d = (got.0 - gc.0, got.1 - gc.1);
                    let dw = (want.0 - px(g).0, want.1 - px(g).1);
                    let err = (d.0 - dw.0).abs().max((d.1 - dw.1).abs());
                    worst = worst.max(err);
                    cases += 1;
                    if (k, l) == (3, 3) || (k, l) == (23, 17) {
                        println!(
                            "{test}: lateral {lateral} axis {axis} at {angle} deg e {e}, {name}, dot ({k}, {l}): {cname} - green ({:.3}, {:.3}) px, oracle ({:.3}, {:.3}) px",
                            d.0, d.1, dw.0, dw.1
                        );
                    }
                    assert!(
                        err <= tol,
                        "{test}: lateral {lateral} axis {axis} e {e}, {name}, dot ({k}, {l}) {cname}: {d:?} vs {dw:?}"
                    );
                }
                // Green does not move: its own landing point (the geometry's), as without the controls.
                let gw = px(g);
                assert!(
                    (gc.0 - gw.0).abs() <= 0.05 && (gc.1 - gw.1).abs() <= 0.05,
                    "{test}: {name}, dot ({k}, {l}) green {gc:?} vs {gw:?}"
                );
            }
        }
    }
    println!("{test}: {cases} red and blue displacements, worst error {worst:.4} px");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Identity: with both amounts 0 the angle and the edge exponent are inert, bit for bit, in every path; and
/// the controls reuse the convergence maths: an axis amount at 0 deg is red dx = +a / 2 and blue dx = -a / 2,
/// a lateral amount at exponent 2 is red radial = +l / 2 and blue radial = -l / 2 (same image within 1e-3 of
/// white: the f32 forms differ, pow against |u| / rc^2, by up to one FP16 step of the stored light).
#[test]
fn zero_is_bit_exact_and_matches_the_convergence_controls() {
    let test = "stage_chroma identity";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let fc = tube(48, 36);
    // A varied picture: a pattern of FP16-exact currents (sixteenths).
    let (w, h) = fc.domain_size(Domain::Amp);
    let k = fc.amp_k as u32;
    let value = |x: u32, y: u32| (1 + (x * 5 + y * 3) % 11) as f32 / 16.0;
    let mut rgba = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let n = x / k;
            rgba.extend_from_slice(&[value(n, y), value(y, n), value(n + 1, y), 1.0]);
        }
    }
    let amp = ImageF32 {
        width: w,
        height: h,
        rgba,
    };
    let max_diff = |a: &ImageF32, b: &ImageF32| {
        a.rgba
            .iter()
            .zip(&b.rgba)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0_f32, f32::max)
    };
    for (name, q, _) in paths(&base()) {
        let reference = run(&mut gpu, fc, &q, &amp);
        let inert = Params {
            geometry_chroma_angle_deg: 77.0,
            geometry_chroma_edge_exponent: 3.5,
            ..q
        };
        assert_eq!(
            run(&mut gpu, fc, &inert, &amp).rgba,
            reference.rgba,
            "{test}: {name}: inert"
        );
        let axis = Params {
            geometry_chroma_axis_mm: 0.6,
            ..q
        };
        let conv = Params {
            geometry_conv_red_dx_mm: 0.3,
            geometry_conv_blue_dx_mm: -0.3,
            ..q
        };
        let d_axis = max_diff(&run(&mut gpu, fc, &axis, &amp), &run(&mut gpu, fc, &conv, &amp));
        let lateral = Params {
            geometry_chroma_lateral_mm: 2.0,
            ..q
        };
        let conv = Params {
            geometry_conv_red_radial_mm: 1.0,
            geometry_conv_blue_radial_mm: -1.0,
            ..q
        };
        let d_lat = max_diff(&run(&mut gpu, fc, &lateral, &amp), &run(&mut gpu, fc, &conv, &amp));
        println!("{test}: {name}: inert bit-exact; axis vs convergence dx {d_axis:e}, lateral vs radial {d_lat:e}");
        assert!(d_axis <= 1e-3 && d_lat <= 1e-3, "{test}: {name}: {d_axis} {d_lat}");
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// C3: at the hard bound the shifted colour is not clipped at the picture's edge: the skirt margin (and the
/// warp path's line-image pad) include the chromatic term, so the red light of a dot on the raster's first
/// column, moved 20 px left past the raster, keeps all its energy; the former margin would cut it.
#[test]
fn hard_bound_shift_is_not_clipped_at_the_edges() {
    let test = "stage_chroma no clipping";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let p = Params {
        geometry_chroma_axis_mm: 20.0,
        geometry_chroma_angle_deg: 180.0,
        tube_aspect: 4.0 / 3.0,
        ..base()
    };
    let place = |params: &Params| {
        let mut fc = tube(32, 32);
        fc.target_size = [400, 400];
        fc.tube_rect_px = [104.0, 104.0, 296.0, 296.0];
        fc.with_skirt_margin(params)
    };
    let fc = place(&p);
    println!("{test}: margin {:?} px", fc.raster_margin);
    assert_eq!(fc.raster_margin, [25, 25], "{test}: C3 margin");
    let m = f64::from(fc.raster_margin[0]);
    let lit = [(0_u32, 16_u32)];
    for (name, q, _) in paths(&p).into_iter().filter(|(n, _, _)| n.starts_with("flat")) {
        let amp = dots(&fc, &lit);
        let out = run(&mut gpu, fc, &q, &amp);
        // Green at the box centre (3, 99) px of the raster, red 20 px left, blue 20 px right.
        let g = (m + 3.0, m + 99.0);
        let (gc, ge) = centroid(&out, 1, g, 12);
        let (rc, re) = centroid(&out, 0, (g.0 - 20.0, g.1), 12);
        let (bc, be) = centroid(&out, 2, (g.0 + 20.0, g.1), 12);
        println!("{test}: {name}: green {gc:?} energy {ge:.4}, red {rc:?} {re:.4}, blue {bc:?} {be:.4}");
        assert!(
            (re / ge - 1.0).abs() <= 1e-3,
            "{test}: {name}: red energy {re} vs green {ge}"
        );
        assert!(
            (be / ge - 1.0).abs() <= 1e-3,
            "{test}: {name}: blue energy {be} vs green {ge}"
        );
        assert!((rc.0 - (gc.0 - 20.0)).abs() <= 0.05, "{test}: {name}: red at {rc:?}");
    }
    // The former margin (no chromatic term) cuts the red light.
    let old = place(&Params {
        geometry_chroma_axis_mm: 0.0,
        ..p
    });
    assert_eq!(old.raster_margin, [5, 5]);
    let q = Params {
        quality_tier: QUALITY_TIER_EXACT,
        ..p
    };
    let out = run(&mut gpu, old, &q, &dots(&old, &lit));
    let (_, ge) = centroid(&out, 1, (8.0, 104.0), 12);
    let red: f64 = (0..out.height)
        .flat_map(|y| (0..out.width).map(move |x| (x, y)))
        .map(|(x, y)| f64::from(out.pixel(x, y)[0]))
        .sum();
    println!("{test}: former margin: red energy {red:.4} vs green {ge:.4}");
    assert!(
        red < 0.5 * ge,
        "{test}: the former margin should have cut the red light"
    );
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

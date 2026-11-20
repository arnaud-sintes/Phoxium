//! GPU tests of the geometry and convergence of the `screen` slot (T-37, `shaders/include/geometry.glsl`,
//! hooks in `shaders/include/beam.glsl`, parameters `params/geometry.toml`), headless, validation on, zero
//! validation messages.
//!
//! Every test injects its own `FrameConstants` and a beam-current image after `gun` (the raster-scale test
//! also runs `abl`), with the mask off, and measures centroids or edges of the tube image. Expected values
//! come from the `crt-model` f64 oracle (`crt_model::geometry`, which maps raster -> seen, the opposite
//! direction of the shader, and inverts by bisection) and from the hand derivations below. Nothing here
//! repeats the shader's formula.
//!
//! Set-up. The tube rectangle is the picture, its centre the tube centre. A native pixel k of a raster of n
//! host px per native pixel covers the raster box [k n, (k + 1) n) host px; its centre is (k + 1/2) n. At px
//! per mm m, raster position s (mm from the centre) = ((k + 1/2) n - centre) / m. The spot is small (FWHM
//! 0.6 mm, no growth, no tail) and symmetric, so a lit box's light centroid is the seen position of the box
//! centre, to second order in the box size (below 0.01 px here).
//!
//! Hand derivations (CRT.md sections in brackets; an independent model re-derives them):
//!
//! G1. 1R sagitta [Faceplate curvature]. 19 in viewable = 482.6 mm, R = 1.767 x 482.6 = 852.75 mm, half
//!     diagonal 241.3 mm: s = R - sqrt(R^2 - 241.3^2) = 852.75 - 817.90 = 34.85 mm (crt-model
//!     `sagitta_19in_1r`, `projection_of_a_1r_faceplate`). The test tube uses R = 852.8 mm.
//! G2. Spherical, horizontal axis, eye at infinity [Raster distortions: position = R theta]. The raster is
//!     uniform in arc length, so a raster point at arc s is seen at R sin(s / R). Tube 386 x 290 mm (the
//!     19 in visible area) at 1 px per mm, 193 x 145 native pixels at n = 2. Column 180: s = 361 - 193 = 168
//!     mm, s / R = 0.196998, sin = 0.196998 - 0.001274 + 0.0000025 = 0.195726, seen at 166.915 mm: the
//!     centroid moves inward by 1.085 px, to x = 359.915. Column 96 (s = 0) does not move; column 12 (s =
//!     -168) moves by +1.085 px (symmetry).
//! G3. Spherical, corner [same]. Native pixel (180, 135): s = (168, 126) mm, |s| = 210 mm, 210 / 852.8 =
//!     0.246248, sin = 0.243767, seen radius 207.885 mm along the same direction (factor 0.989929): (166.308,
//!     124.731) mm, i.e. host (359.308, 269.731); the radial displacement at that point is 2.115 mm.
//! G4. Eye at 600 mm [Faceplate curvature: viewing distance 500 to 650 mm, 1.0 to 1.3 diagonals]. The seen
//!     position is lateral x eye / (eye + depth): column 180 at arc 168 mm has depth R (1 - cos(0.196998)) =
//!     852.8 x 0.019343 = 16.496 mm, seen at 166.915 x 600 / 616.496 = 162.449 mm (centroid at 355.449).
//! G5. Cylindrical [Faceplate curvature: Trinitron curved horizontally, flat vertically]. x maps as G2 and
//!     y is unchanged: a lit column is seen at x = 359.915 on every row (vertical lines stay straight, x
//!     independent of y within 0.05 px).
//! G6. Convergence [Convergence: static offset plus quadratic per channel]. Tube 128 x 96 px at 4 px per mm
//!     (32 x 24 mm, half 16 x 12, half diagonal 20 mm). Red +0.3 mm horizontally: the red centroid moves by
//!     0.3 x 4 = 1.2 px; blue -0.2 mm vertically: -0.8 px; green stays. Radial 0.5 mm on red at native pixel
//!     (14, 10) of 16 x 12 (n = 8): box centre (116, 84) px, u = (13, 9) mm, (r / rc)^2 = 250 / 400 =
//!     0.625, 0.3125 mm outward along (13, 9) / 15.8114: (0.25694, 0.17788) mm = (1.0278, 0.7115) px.
//! G7. Raster scale [ABL, blooming and EHT regulation; raster size ~ 1 / sqrt(V)]. raster_scale = 1 + b
//!     (APL - ref). A white box of 32 x 24 native pixels centred in 64 x 48 has APL 768 / 3072 = 0.25; b =
//!     0.03, ref 0, tau 0: scale 1.0075. Tube 256 x 192 px (n = 4, 2 px per mm): the box spans [64, 192) x
//!     [48, 144) px, 64 px either side of the centre 128; scaled about the centre its edges go to 128 -/+
//!     64 x 1.0075 = 63.52 and 192.48 (0.48 px out each side), the width to 128.96 px.
//! G8. Identity [PLAN T-37 accept: flat with zero residuals is bit-identical to the T-36b render]. With the
//!     curvature flat, the radius and the eye distance are inert, so changing them is bit-identical; every
//!     T-36 and T-39 test keeps passing unchanged (the hooks return the old values exactly).
//! G9. Overscan [Visible area, overscan: 5 to 10 % per axis]. Overscan 0.05 makes the raster 1.05 times
//!     wider about the centre: the G7 box edges go to 128 -/+ 64 x 1.05 = 60.8 and 195.2; a full-width
//!     source has its edges 0.05 x 128 = 6.4 px outside the tube on each side (2.5 % of the width).
//! G10. Pincushion [Raster distortions: residual 0.5 to 3 %]. Forward residual s (1 + k |s|^2 / rc^2), rc
//!     the half diagonal (64 x 48 mm half, rc^2 = 6400). k = 0.02, box centre at (49, 37) mm: |s|^2 = 3770,
//!     factor 1.0117813, seen at (49.5773, 37.4359) mm = host (227.155, 170.872) at 2 px per mm. The mask
//!     (16 px per triad) is not moved: a flat white field gives the same mask x beam image with and
//!     without the pincushion.
//! G11. Skirt [Spot profile; review of T-36b]. A flat field of current I ends at the raster edge; the
//!     spot's x-profile spills past it. Per unit length of line the light beyond the edge is
//!     I integral_0^inf Q(x / sigma) dx = I sigma / sqrt(2 pi) = 0.3989 I sigma; per row, averaged over whole
//!     line periods (the line deposits I P per unit length per pitch P), the same. PVM-20M4U peak spot:
//!     FWHM 0.8 mm, sigma 0.8 / 2.3548 x (1440 / 290) = 1.6870 px, skirt 0.6730 px of light per row and per
//!     bright edge. The tube image is grown by at least 4 sigma at I = 2 per side, but since the T-37 review
//!     (F1) the margin is only the evaluated area: it lies outside the visible opening (G12), so its light is
//!     exactly 0; the skirt that falls inside the opening (the raster's own edge pixels) stays and matches the
//!     oracle.
//! G6b. Radial convergence, landing point [T-37 review F4]. The shader evaluates the offset where the light
//!     lands: p = s + 0.5 p |p| / 400 mm (s = (13, 9) mm). Fixed-point iteration in f64 (50 steps): p - s =
//!     (0.267623, 0.185278) mm = (1.07049, 0.74111) px; the oracle's source-point value (1.0278, 0.7115) px
//!     is its first-order inverse.
//! G12. Visible opening [Visible area, overscan, corners: rounded rectangle, corner radius 15 to 40 mm]. The
//!     opening is the tube's visible area (the raster rectangle at zero overscan when the raster has the tube's
//!     aspect, 4:3 here; a narrower par raster gets the tube's full width) with corners of radius r. Tube
//!     64 x 48 native at n = 4, 2 px per mm (256 x 192 px, 128 x 96 mm), r = 20 mm = 40 px: the corner arc is
//!     centred at (40, 40) px from the top-left; the pixel (5, 5) (centre (5.5, 5.5)) is at 40 sqrt(2) -
//!     34.5 sqrt(2) = 7.78 px outside the arc, so exactly 0; (40, 2) is on the straight edge, inside by 2.5
//!     px: as without the opening. With overscan 5 % the full-white picture is cut at the opening (the raster
//!     edge at zero overscan, x = 0 and 256), whatever the spot size.
//! G13. Trapezoid [Raster distortions: trapezoid residual under 1 %; forward x (1 + t y / half height)]. t =
//!     0.03, box centre s = (49, 37) mm on the G10 tube: x = 49 (1 + 0.03 x 37 / 48) = 50.13313 mm, host
//!     128 + 100.2663 = 228.2663; y unchanged, 170.
//! G14. Rotation [rotation up to 1 to 2 deg]. +1.5 deg (clockwise on the screen, y down): (cos 49 - sin 37,
//!     sin 49 + cos 37) = (48.01466, 38.26999) mm, host (224.0293, 172.5400).
//! G15. Barrel (negative pincushion) [same table]. k = -0.02 at s = (49, 37): factor 1 - 0.02 x 3770 / 6400
//!     = 0.98821875, host (128 + 98 f, 96 + 74 f) = (224.8454, 169.1282).
//! G16. Spherical corner with the eye at 600 mm [Faceplate curvature]. Native (180, 135), |s| = 210 mm,
//!     phi = 0.246248: lateral R sin(phi) = 207.884 mm, depth R (1 - cos(phi)) = 25.726 mm, seen radius
//!     207.884 x 600 / 625.726 = 199.337 mm along (168, 126) / 210: host (352.4698, 264.6024).
//! G17. Cylinder with the eye at 600 mm. Column 180 (arc 168 mm): lateral 166.9155, depth 16.4944, seen x
//!     = 166.9155 x 600 / 616.4944 = 162.4496 mm, host 355.4496 on every row (the depth depends on x only,
//!     so vertical lines stay straight); y is scaled by the same 0.973245: native row 135 (y 126 mm) is seen
//!     at host y 145 + 122.6289 = 267.6289.
//! G18. ABL state [T-37 review F2]. abl disabled while abl_breathing != 0: raster_scale 1, bit-identical to
//!     breathing 0. Cold start: the state is seeded with apl_f = abl_breathing_ref, so with frame_time_ms 0
//!     (alpha 0, apl_f kept) the first frame has scale 1 + b (ref - ref) = 1 exactly: bit-identical.

mod common;

use crt_model::beam::{AmpRaster, BeamOracle, Spot, SpotGrowth};
use crt_model::geometry::{
    arc_from_projected_mm, convergence_offset_mm, cylinder_seen_eye_mm, cylinder_seen_mm, projected_from_arc_mm,
    sphere_seen_mm,
};
use crt_model::units::sigma_from_fwhm;
use ultimatecrt::gpu::{Domain, FrameConstants, Headless, ImageF32, Settings, StageInput, StageRun, TargetFormat};
use ultimatecrt::params::{Params, GEOMETRY_CURVATURE_CYLINDRICAL, GEOMETRY_CURVATURE_SPHERICAL};

/// 19 in 1R radius used by the curvature tests (mm).
const R_1R: f64 = 852.8;

/// A tube of `cols` x `rows` native pixels at `n` host px per native pixel, `ppm` px per mm, line pitch
/// `n / ppm` mm, K = 4.
fn tube(cols: u32, rows: u32, n: u32, ppm: f32) -> FrameConstants {
    let mut fc = FrameConstants::new((cols, rows), (cols * n, rows * n), false);
    assert_eq!(fc.tube_size, [(cols * n) as i32, (rows * n) as i32]);
    fc.px_per_mm = ppm;
    fc.line_pitch_mm = n as f32 / ppm;
    fc
}

/// Small round spot (FWHM 0.6 mm, no growth, no tail), mask off.
fn base() -> Params {
    Params {
        beam_fwhm_mid_mm: 0.6,
        beam_fwhm_peak_mm: 0.6,
        beam_tail_weight: 0.0,
        mask_enabled: false,
        ..Params::default()
    }
}

/// The curvature tests run at 1 px per mm: a 2 mm spot (sigma 0.85 px) keeps the pixel-centre centroid of
/// the area-integrated light unbiased (a spot narrower than a pixel biases it toward the pixel centre by up
/// to 0.1 px).
fn wide() -> Params {
    Params {
        beam_fwhm_mid_mm: 2.0,
        beam_fwhm_peak_mm: 2.0,
        ..base()
    }
}

/// The amp-domain current image of `fc`: `f(native column, native row)` per channel.
fn currents(fc: &FrameConstants, f: impl Fn(u32, u32) -> [f32; 3]) -> ImageF32 {
    let (w, h) = fc.domain_size(Domain::Amp);
    let k = fc.amp_k as u32;
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = f(x / k, y);
            rgba.extend_from_slice(&[r, g, b, 1.0]);
        }
    }
    ImageF32 {
        width: w,
        height: h,
        rgba,
    }
}

/// Runs the `slots` on `amp` injected after `gun`; returns the tube image of `screen`.
fn run(gpu: &mut Headless, slots: &[&str], fc: FrameConstants, params: &Params, amp: &ImageF32) -> ImageF32 {
    gpu.enable_only(slots).unwrap();
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
    let r = gpu.render_stages(&run).expect("render_stages");
    r.stage("screen").expect("screen ran").clone()
}

/// Light centroid (x, y) of channel `c` over the window `xs` x `ys` (pixel centres at index + 0.5).
fn centroid(img: &ImageF32, c: usize, xs: std::ops::Range<u32>, ys: std::ops::Range<u32>) -> (f64, f64) {
    let (mut e, mut sx, mut sy) = (0.0, 0.0, 0.0);
    for y in ys {
        for x in xs.clone() {
            let v = f64::from(img.pixel(x, y)[c]);
            e += v;
            sx += v * (f64::from(x) + 0.5);
            sy += v * (f64::from(y) + 0.5);
        }
    }
    assert!(e > 0.0, "no light in the window");
    (sx / e, sy / e)
}

fn window(centre: f64, half: u32, len: u32) -> std::ops::Range<u32> {
    let c = centre.round() as i64;
    (c - i64::from(half)).max(0) as u32..((c + i64::from(half)) as u32).min(len)
}

const WHITE: [f32; 3] = [1.0; 3];

/// G2, G3, G4: spherical faceplate, eye at infinity and at 600 mm, against the oracle.
#[test]
fn spherical_curvature_matches_the_oracle() {
    let test = "stage_geometry spherical";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let fc = tube(193, 145, 2, 1.0);
    let centre = (193.0, 145.0);
    for eye in [0.0, 600.0] {
        let p = Params {
            geometry_curvature: GEOMETRY_CURVATURE_SPHERICAL,
            geometry_radius_mm: R_1R as f32,
            geometry_eye_distance_mm: eye as f32,
            ..wide()
        };
        // Lit columns: native rows 68 to 77 around the centre (host rows 136 to 154), measured on the line period at the centre, host rows 144 and 145.
        for col in [12_u32, 96, 180] {
            let amp = currents(&fc, |x, y| {
                if x == col && (68..78).contains(&y) {
                    WHITE
                } else {
                    [0.0; 3]
                }
            });
            let out = run(&mut gpu, &["screen"], fc, &p, &amp);
            let s = (f64::from(col) + 0.5) * 2.0 - centre.0;
            let want = centre.0 + projected_from_arc_mm(R_1R, s, eye);
            let (cx, _) = centroid(&out, 1, window(want, 10, 386), 144..146);
            println!("{test} eye {eye}: column {col} arc {s} mm -> centroid {cx:.4}, oracle {want:.4}");
            assert!(
                (cx - want).abs() <= 0.05,
                "{test} eye {eye} column {col}: {cx} vs {want}"
            );
        }
    }
    // G2 by hand: column 180 at 359.915.
    assert!((centre.0 + projected_from_arc_mm(R_1R, 168.0, 0.0) - 359.915).abs() < 0.002);
    // G4 by hand: 355.449 with the eye at 600 mm.
    assert!((centre.0 + projected_from_arc_mm(R_1R, 168.0, 600.0) - 355.449).abs() < 0.005);
    // The oracle's inverse agrees with the forward map at the corner.
    assert!((arc_from_projected_mm(R_1R, projected_from_arc_mm(R_1R, 168.0, 600.0), 600.0) - 168.0).abs() < 1e-9);

    // G3: corner dot (180, 135), eye at infinity.
    let p = Params {
        geometry_curvature: GEOMETRY_CURVATURE_SPHERICAL,
        geometry_radius_mm: R_1R as f32,
        ..wide()
    };
    let amp = currents(&fc, |x, y| if x == 180 && y == 135 { WHITE } else { [0.0; 3] });
    let out = run(&mut gpu, &["screen"], fc, &p, &amp);
    let (sx, sy) = sphere_seen_mm(R_1R, (168.0, 126.0), 0.0);
    let want = (centre.0 + sx, centre.1 + sy);
    assert!(
        (want.0 - 359.308).abs() < 0.002 && (want.1 - 269.731).abs() < 0.002,
        "{want:?}"
    );
    let got = centroid(&out, 1, window(want.0, 8, 386), window(want.1, 8, 290));
    println!("{test}: corner dot centroid {got:?}, oracle {want:?}");
    assert!(
        (got.0 - want.0).abs() <= 0.05 && (got.1 - want.1).abs() <= 0.05,
        "{test} corner: {got:?} vs {want:?}"
    );
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// G5: cylindrical faceplate: a vertical source line stays straight and is seen where the oracle puts it.
#[test]
fn cylindrical_keeps_vertical_lines_straight() {
    let test = "stage_geometry cylindrical";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let fc = tube(193, 145, 2, 1.0);
    let p = Params {
        geometry_curvature: GEOMETRY_CURVATURE_CYLINDRICAL,
        geometry_radius_mm: R_1R as f32,
        ..wide()
    };
    let amp = currents(&fc, |x, _| if x == 180 { WHITE } else { [0.0; 3] });
    let out = run(&mut gpu, &["screen"], fc, &p, &amp);
    let want = 193.0 + cylinder_seen_mm(R_1R, (168.0, 0.0)).0;
    assert!((want - 359.915).abs() < 0.002);
    // Rows over the whole height (whole line periods of 2 rows each, centred on line centres).
    for y0 in [4_u32, 60, 144, 230, 284] {
        let (cx, _) = centroid(&out, 1, window(want, 10, 386), y0..y0 + 2);
        println!("{test}: rows {y0}..{} centroid {cx:.4}, oracle {want:.4}", y0 + 2);
        assert!((cx - want).abs() <= 0.05, "{test} rows {y0}: {cx} vs {want}");
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// G6: static and radial convergence offsets move each channel's centroid by the configured mm.
#[test]
fn convergence_shifts_each_channel() {
    let test = "stage_geometry convergence";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let ppm = 4.0;
    let fc = tube(16, 12, 8, ppm);
    let p = Params {
        geometry_conv_red_dx_mm: 0.3,
        geometry_conv_blue_dy_mm: -0.2,
        ..base()
    };
    let amp = currents(&fc, |x, y| if x == 8 && y == 6 { WHITE } else { [0.0; 3] });
    let out = run(&mut gpu, &["screen"], fc, &p, &amp);
    let (xs, ys) = (56..80, 40..64);
    let g = centroid(&out, 1, xs.clone(), ys.clone());
    let r = centroid(&out, 0, xs.clone(), ys.clone());
    let b = centroid(&out, 2, xs, ys);
    println!("{test}: green {g:?}, red {r:?}, blue {b:?}");
    assert!(
        (g.0 - 68.0).abs() <= 0.05 && (g.1 - 52.0).abs() <= 0.05,
        "{test}: green {g:?}"
    );
    assert!(
        (r.0 - g.0 - 0.3 * f64::from(ppm)).abs() <= 0.1 && (r.1 - g.1).abs() <= 0.05,
        "{test}: red {r:?}"
    );
    assert!(
        (b.1 - g.1 + 0.2 * f64::from(ppm)).abs() <= 0.1 && (b.0 - g.0).abs() <= 0.05,
        "{test}: blue {b:?}"
    );

    // Radial term on red at the box (14, 10).
    let p = Params {
        geometry_conv_red_radial_mm: 0.5,
        ..base()
    };
    let amp = currents(&fc, |x, y| if x == 14 && y == 10 { WHITE } else { [0.0; 3] });
    let out = run(&mut gpu, &["screen"], fc, &p, &amp);
    let (xs, ys) = (104..128, 72..96);
    let g = centroid(&out, 1, xs.clone(), ys.clone());
    let r = centroid(&out, 0, xs, ys);
    let (dx, dy) = convergence_offset_mm((0.0, 0.0), 0.5, (13.0, 9.0), (16.0, 12.0));
    let want = (dx * f64::from(ppm), dy * f64::from(ppm));
    assert!(
        (want.0 - 1.0278).abs() < 1e-3 && (want.1 - 0.7115).abs() < 1e-3,
        "{want:?}"
    );
    println!("{test} radial: green {g:?}, red {r:?}, expected red - green {want:?}");
    assert!(
        (g.0 - 116.0).abs() <= 0.05 && (g.1 - 84.0).abs() <= 0.05,
        "{test} radial: green {g:?}"
    );
    // G6b: the shader takes the offset at the landing point; the implied value (1.07049, 0.74111) px.
    let implied = (1.07049, 0.74111);
    assert!(
        (r.0 - g.0 - implied.0).abs() <= 0.02 && (r.1 - g.1 - implied.1).abs() <= 0.02,
        "{test} radial: red {r:?} vs green {g:?}, landing-point offset {implied:?}"
    );
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Column sums of channel 1 over `ys`, per column.
fn column_sums(img: &ImageF32, ys: std::ops::Range<u32>) -> Vec<f64> {
    (0..img.width)
        .map(|x| ys.clone().map(|y| f64::from(img.pixel(x, y)[1])).sum())
        .collect()
}

/// Left and right edges of a lit box from column sums: plateau from the box interior, each edge = the end
/// of a window that starts in the dark minus the lit fraction summed over it.
fn edges(
    sums: &[f64],
    interior: std::ops::Range<usize>,
    left: std::ops::Range<usize>,
    right: std::ops::Range<usize>,
) -> (f64, f64) {
    let plateau = sums[interior.clone()].iter().sum::<f64>() / interior.len() as f64;
    let l = left.end as f64 - sums[left].iter().map(|v| v / plateau).sum::<f64>();
    let r = right.start as f64 + sums[right].iter().map(|v| v / plateau).sum::<f64>();
    (l, r)
}

/// G7: raster_scale from the ABL state enlarges the picture about the tube centre.
#[test]
fn raster_scale_enlarges_about_the_centre() {
    let test = "stage_geometry raster scale";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let fc = tube(64, 48, 4, 2.0);
    let amp = currents(&fc, |x, y| {
        if (16..48).contains(&x) && (12..36).contains(&y) {
            WHITE
        } else {
            [0.0; 3]
        }
    });
    let ys = 64..128;
    let mut results = Vec::new();
    for b in [0.0_f32, 0.03] {
        let p = Params {
            abl_enable: false,
            abl_breathing: b,
            abl_breathing_ref: 0.0,
            abl_tau_s: 0.0,
            ..base()
        };
        let out = run(&mut gpu, &["abl", "screen"], fc, &p, &amp);
        let sums = column_sums(&out, ys.clone());
        let (l, r) = edges(&sums, 100..156, 40..100, 156..216);
        println!("{test}: b {b}: edges {l:.4} {r:.4}, width {:.4}", r - l);
        results.push((l, r));
    }
    let scale = 1.0 + 0.03 * 0.25;
    let (l0, r0) = results[0];
    let (l1, r1) = results[1];
    assert!(
        (l0 - 64.0).abs() <= 0.05 && (r0 - 192.0).abs() <= 0.05,
        "{test}: b = 0 edges {l0} {r0}"
    );
    assert!((l1 - (128.0 - 64.0 * scale)).abs() <= 0.05, "{test}: left edge {l1}");
    assert!((r1 - (128.0 + 64.0 * scale)).abs() <= 0.05, "{test}: right edge {r1}");

    // G9: horizontal overscan 5 % scales the raster width by 1.05 about the centre: edges at 128 -/+ 67.2.
    let p = Params {
        geometry_overscan_x: 0.05,
        ..base()
    };
    let out = run(&mut gpu, &["screen"], fc, &p, &amp);
    let (l, r) = edges(&column_sums(&out, ys.clone()), 100..156, 40..100, 156..216);
    println!("{test}: overscan 5 %: edges {l:.4} {r:.4}");
    assert!(
        (l - 60.8).abs() <= 0.05 && (r - 195.2).abs() <= 0.05,
        "{test}: overscan edges {l} {r}"
    );
    // Full-width source: its edges (raster 0 and 256) land 5 % of the half width outside the tube.
    let full = currents(&fc, |_, _| WHITE);
    let out = run(&mut gpu, &["screen"], fc, &p, &full);
    let sums = column_sums(&out, ys);
    let plateau = sums[128];
    // The first column holds raster [-0.05 x 128 + ..]: fully lit, as bright as the centre (within the blur
    // tail of the far edge at -6.4 px, 12 sigma away).
    assert!(
        (sums[0] / plateau - 1.0).abs() <= 1e-3,
        "{test}: overscan first column {}",
        sums[0] / plateau
    );
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// G10: pincushion moves the source but not the mask (the mask stays in tube coordinates).
#[test]
fn pincushion_moves_the_beam_not_the_mask() {
    let test = "stage_geometry pincushion";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let fc = tube(64, 48, 4, 2.0);
    let k = 0.02;
    let p = Params {
        geometry_pincushion: k,
        ..base()
    };
    let amp = currents(&fc, |x, y| if x == 56 && y == 42 { WHITE } else { [0.0; 3] });
    let out = run(&mut gpu, &["screen"], fc, &p, &amp);
    // Box centre (226, 170) px = (49, 37) mm from the centre; rc^2 = 64^2 + 48^2 = 6400 mm^2; forward
    // residual s (1 + k |s|^2 / rc^2) = s x 1.0117813: (49.5773, 37.4359) mm = (227.155, 170.872) px.
    let f = 1.0 + f64::from(k) * (49.0 * 49.0 + 37.0 * 37.0) / 6400.0;
    let want = (128.0 + 98.0 * f, 96.0 + 74.0 * f);
    assert!(
        (want.0 - 227.155).abs() < 0.001 && (want.1 - 170.872).abs() < 0.001,
        "{want:?}"
    );
    let got = centroid(&out, 1, 214..240, 160..182);
    println!("{test}: dot centroid {got:?}, expected {want:?}");
    assert!(
        (got.0 - want.0).abs() <= 0.05 && (got.1 - want.1).abs() <= 0.05,
        "{test}: {got:?} vs {want:?}"
    );

    // Grille at 16 px per triad (8 mm at 2 px per mm), flat white: inside the picture the beam is flat, so
    // with and without pincushion every pixel (mask x beam) is the same: the stripes did not move. A 6 mm
    // spot (sigma 5.1 px) on 4 px lines leaves no scanline ripple (exp(-2 pi^2 1.63) ~ 1e-14), so the beam
    // is flat however the lines are bent.
    let masked = |k: f32| Params {
        beam_fwhm_mid_mm: 6.0,
        beam_fwhm_peak_mm: 6.0,
        mask_enabled: true,
        mask_pitch_mm: 8.0,
        mask_damper_count: 0,
        geometry_pincushion: k,
        ..base()
    };
    let full = currents(&fc, |_, _| WHITE);
    let a = run(&mut gpu, &["screen"], fc, &masked(0.0), &full);
    let b = run(&mut gpu, &["screen"], fc, &masked(k), &full);
    for y in 20..172 {
        for x in 20..236 {
            let (va, vb) = (a.pixel(x, y), b.pixel(x, y));
            for c in 0..3 {
                let (va, vb) = (f64::from(va[c]), f64::from(vb[c]));
                assert!(
                    (va - vb).abs() <= 2e-3 * va.max(1e-3) + 1e-4,
                    "{test}: mask moved at ({x}, {y}) {va} vs {vb}"
                );
            }
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// G11: the tube image's margin (review of T-36b) is evaluated but lies outside the visible opening (T-37
/// review F1): exactly 0 there; inside, the raster's edge matches the oracle; the clipped skirt energy per row
/// at a flat-field edge is I sigma / sqrt(2 pi).
#[test]
fn skirt_past_the_raster_ends_matches_the_oracle() {
    let test = "stage_geometry skirt";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let ppm = 1440.0_f32 / 290.0;
    let p = Params {
        beam_fwhm_mid_mm: 0.45,
        beam_fwhm_peak_mm: 0.8,
        beam_growth_exponent: 0.5,
        beam_tail_weight: 0.0,
        mask_enabled: false,
        ..Params::default()
    };
    // Raster 96 x 48 px (16 x 8 native, 6 px per line) centred in a 160 x 100 target.
    let mut fc = tube(16, 8, 6, ppm);
    fc.target_size = [160, 100];
    fc.tube_rect_px = [32.0, 26.0, 128.0, 74.0];
    let fc = fc.with_skirt_margin(&p);
    let growth = SpotGrowth {
        fwhm_mid: 0.45,
        fwhm_peak: 0.8,
        n: 0.5,
    };
    let ppm64 = f64::from(ppm);
    let reach = 4.0 * sigma_from_fwhm(growth.fwhm(2.0)) * ppm64;
    let m = fc.raster_margin;
    println!("{test}: margin {m:?} px, 4 sigma at I = 2: {reach:.3} px");
    assert!(
        f64::from(m[0]) >= reach && f64::from(m[1]) >= reach,
        "{test}: margin {m:?} < {reach}"
    );
    assert_eq!(fc.tube_size, [96 + 2 * m[0], 48 + 2 * m[1]]);

    let amp = currents(&fc, |_, _| WHITE);
    let out = run(&mut gpu, &["screen"], fc, &p, &amp);
    let ones = vec![1.0; amp.rgba.len() / 4];
    let sigma = |i: f64| sigma_from_fwhm(growth.fwhm(i)) * ppm64;
    let spot = Spot {
        sigma_px: &sigma,
        tail_weight: 0.0,
        tail_ratio: 1.0,
    };
    let oracle = BeamOracle::new(
        AmpRaster {
            currents: &ones,
            samples: amp.width as usize,
            sample_width: 96.0 / f64::from(amp.width),
            first: 3.0,
            pitch: 6.0,
            mult: 1,
        },
        &spot,
    );
    // Rows m + 12 .. m + 36: four whole line periods inside the raster. Left margin columns: outside the
    // visible opening (G12), exactly 0 (the oracle's skirt there is the light the opening clips). The first 6
    // columns of the raster (inside the opening, where the edge's own skirt deficit shows) match the oracle.
    let (mx, my) = (m[0] as u32, m[1] as u32);
    let rows = my + 12..my + 36;
    let mut clipped = 0.0;
    for y in rows.clone() {
        let ry = f64::from(y) - f64::from(my);
        for x in 0..mx + 6 {
            let rx = f64::from(x) - f64::from(mx);
            let w = oracle.pixel_mean((0.0, 0.0), (rx, rx + 1.0), (ry, ry + 1.0));
            let g = f64::from(out.pixel(x, y)[1]);
            if x < mx {
                assert_eq!(g, 0.0, "{test}: ({x}, {y}) outside the opening");
                clipped += w;
            } else {
                assert!(
                    (g - w).abs() <= 1e-3 * w + 3e-4,
                    "{test}: ({x}, {y}) GPU {g} vs oracle {w}"
                );
            }
        }
    }
    let n_rows = f64::from(rows.end - rows.start);
    let hand = sigma(1.0) / (2.0 * std::f64::consts::PI).sqrt();
    println!(
        "{test}: skirt energy per row clipped by the opening (oracle) {:.4}, hand sigma / sqrt(2 pi) = {hand:.4} px",
        clipped / n_rows
    );
    assert!(
        (clipped / n_rows / hand - 1.0).abs() <= 0.01,
        "{test}: oracle skirt vs hand"
    );
    assert!((hand - 0.673).abs() <= 0.002, "{test}: hand {hand}");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// G8: identity. The radius and the eye distance are inert while the faceplate is flat (bit-identical), and
/// breathing 0 with the abl slot running is bit-identical to `screen` alone.
#[test]
fn identity_is_bit_exact() {
    let test = "stage_geometry identity";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let fc = tube(12, 10, 6, 1440.0 / 290.0);
    // FP16-exact currents (sixteenths): the abl slot stores its copy in an FP16 chain image, an injected
    // image goes in unrounded.
    let value = |x: u32, y: u32| (1 + (x * 5 + y * 3) % 11) as f32 / 16.0;
    let amp = currents(&fc, |x, y| [value(x, y), value(y, x), value(x + 1, y)]);
    let reference = run(&mut gpu, &["screen"], fc, &base(), &amp);
    let inert = Params {
        geometry_radius_mm: 1000.0,
        geometry_eye_distance_mm: 600.0,
        ..base()
    };
    assert_eq!(
        run(&mut gpu, &["screen"], fc, &inert, &amp).rgba,
        reference.rgba,
        "{test}: inert"
    );
    let with_abl = run(&mut gpu, &["abl", "screen"], fc, &base(), &amp);
    assert_eq!(with_abl.rgba, reference.rgba, "{test}: abl slot, breathing 0");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// G12: the visible opening. Rounded corners clip to exactly 0; the straight edge and the inside are
/// unchanged (bit-exact); with overscan the picture is cut at the opening whatever the spot size.
#[test]
fn opening_clips_the_light() {
    let test = "stage_geometry opening";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let fc = tube(64, 48, 4, 2.0);
    let full = currents(&fc, |_, _| WHITE);
    let square = run(&mut gpu, &["screen"], fc, &base(), &full);
    let p = Params {
        geometry_opening_corner_radius_mm: 20.0,
        ..base()
    };
    let round = run(&mut gpu, &["screen"], fc, &p, &full);
    // Corner arcs of radius 40 px centred 40 px in from each corner of the 256 x 192 opening.
    let (w, h, r) = (256.0_f64, 192.0_f64, 40.0_f64);
    let (mut zeros, mut same) = (0, 0);
    for y in 0..192_u32 {
        for x in 0..256_u32 {
            let (cx, cy) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
            let ax = if cx < r {
                r - cx
            } else if cx > w - r {
                cx - (w - r)
            } else {
                0.0
            };
            let ay = if cy < r {
                r - cy
            } else if cy > h - r {
                cy - (h - r)
            } else {
                0.0
            };
            let d = ax.hypot(ay) - r; // distance outside the arc (corner regions; <= 0 elsewhere)
            let (a, b) = (square.pixel(x, y), round.pixel(x, y));
            if d >= 0.5 {
                assert!(b[..3].iter().all(|&v| v == 0.0), "{test}: ({x}, {y}) outside: {b:?}");
                zeros += 1;
            } else if d <= -0.5 {
                assert_eq!(a[..3], b[..3], "{test}: ({x}, {y}) inside");
                same += 1;
            }
        }
    }
    println!("{test}: {zeros} pixels outside (exactly 0), {same} inside (unchanged)");
    // G12 by hand: (5, 5) is 7.78 px outside the arc; (40, 2) is on the straight edge.
    assert!(round.pixel(5, 5)[..3].iter().all(|&v| v == 0.0));
    assert_eq!(round.pixel(40, 2)[..3], square.pixel(40, 2)[..3]);
    assert!(square.pixel(5, 5)[1] > 0.5);

    // Overscan 5 % on a tube with a skirt margin: the margin (outside the opening) is exactly 0 for a small
    // and a large spot, the first column inside is lit.
    for fwhm in [0.5_f32, 1.6] {
        let p = Params {
            beam_fwhm_mid_mm: fwhm,
            beam_fwhm_peak_mm: fwhm,
            geometry_overscan_x: 0.05,
            ..base()
        };
        let mut fc = tube(16, 8, 6, 1440.0 / 290.0);
        fc.target_size = [160, 100];
        fc.tube_rect_px = [32.0, 26.0, 128.0, 74.0];
        let fc = fc.with_skirt_margin(&p);
        let (mx, my) = (fc.raster_margin[0] as u32, fc.raster_margin[1] as u32);
        assert!(mx > 0, "{test}: no margin");
        let out = run(&mut gpu, &["screen"], fc, &p, &currents(&fc, |_, _| WHITE));
        let tw = fc.tube_size[0] as u32;
        for y in my + 12..my + 36 {
            for x in (0..mx).chain(tw - mx..tw) {
                assert!(
                    out.pixel(x, y)[..3].iter().all(|&v| v == 0.0),
                    "{test}: FWHM {fwhm}: ({x}, {y}) outside the opening"
                );
            }
            assert!(out.pixel(mx, y)[1] > 0.0 && out.pixel(tw - mx - 1, y)[1] > 0.0);
        }
    }

    // A par raster narrower than the tube (48 x 48 px, square, in a 4:3 tube): the opening is the tube's full
    // visible width, 48 x 4 / 3 = 64 px, so the skirt in the side margins (inside the opening) is drawn; the
    // opening's height is the raster's, so the top and bottom margins are exactly 0.
    let mut fc = tube(12, 12, 4, 2.0);
    fc.target_size = [100, 60];
    fc.tube_rect_px = [26.0, 6.0, 74.0, 54.0];
    let fc = fc.with_skirt_margin(&base());
    let (mx, my) = (fc.raster_margin[0] as u32, fc.raster_margin[1] as u32);
    assert!(mx > 0 && my > 0, "{test}: margin {mx} {my}");
    let out = run(&mut gpu, &["screen"], fc, &base(), &currents(&fc, |_, _| WHITE));
    let (tw, th) = (fc.tube_size[0] as u32, fc.tube_size[1] as u32);
    let side = (my + 8..my + 40)
        .map(|y| f64::from(out.pixel(mx - 1, y)[1]))
        .sum::<f64>();
    println!("{test}: par narrower raster: side margin column light {side:.4}");
    assert!(side > 0.0, "{test}: no skirt in the side margin inside the opening");
    for x in 0..tw {
        for y in (0..my).chain(th - my..th) {
            assert!(
                out.pixel(x, y)[..3].iter().all(|&v| v == 0.0),
                "{test}: ({x}, {y}) above or below the opening"
            );
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Centroid of channel 1 of a single lit native pixel `dot` on the G10 tube (64 x 48, n = 4, 2 px per mm).
fn dot_centroid(gpu: &mut Headless, p: &Params, dot: (u32, u32), want: (f64, f64)) -> (f64, f64) {
    let fc = tube(64, 48, 4, 2.0);
    let amp = currents(&fc, |x, y| if (x, y) == dot { WHITE } else { [0.0; 3] });
    let out = run(gpu, &["screen"], fc, p, &amp);
    centroid(&out, 1, window(want.0, 12, 256), window(want.1, 12, 192))
}

/// G13, G14, G15: trapezoid, rotation and barrel move a dot where the forward maps put it.
#[test]
fn trapezoid_rotation_barrel_match_the_oracle() {
    let test = "stage_geometry distortions";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let s = (49.0_f64, 37.0_f64); // box centre of native (56, 42), mm from the centre
    let host = |m: (f64, f64)| (128.0 + 2.0 * m.0, 96.0 + 2.0 * m.1);
    // G13: forward trapezoid x (1 + t y / half height), half height 48 mm.
    let t = 0.03_f64;
    let trap = host((s.0 * (1.0 + t * s.1 / 48.0), s.1));
    assert!((trap.0 - 228.2663).abs() < 1e-3 && trap.1 == 170.0, "{trap:?}");
    // G14: forward rotation by +1.5 deg (clockwise on the screen, y down).
    let a = 1.5_f64.to_radians();
    let rot = host((a.cos() * s.0 - a.sin() * s.1, a.sin() * s.0 + a.cos() * s.1));
    assert!(
        (rot.0 - 224.0293).abs() < 1e-3 && (rot.1 - 172.5400).abs() < 1e-3,
        "{rot:?}"
    );
    // G15: forward barrel s (1 + k |s|^2 / rc^2), k = -0.02, rc^2 = 6400.
    let k = -0.02_f64;
    let f = 1.0 + k * (s.0 * s.0 + s.1 * s.1) / 6400.0;
    let barrel = host((s.0 * f, s.1 * f));
    assert!(
        (barrel.0 - 224.8454).abs() < 1e-3 && (barrel.1 - 169.1282).abs() < 1e-3,
        "{barrel:?}"
    );
    let cases = [
        (
            "trapezoid",
            Params {
                geometry_trapezoid: t as f32,
                ..base()
            },
            trap,
        ),
        (
            "rotation",
            Params {
                geometry_rotation_deg: 1.5,
                ..base()
            },
            rot,
        ),
        (
            "barrel",
            Params {
                geometry_pincushion: k as f32,
                ..base()
            },
            barrel,
        ),
    ];
    for (name, p, want) in cases {
        let got = dot_centroid(&mut gpu, &p, (56, 42), want);
        println!("{test} {name}: centroid {got:?}, expected {want:?}");
        assert!(
            (got.0 - want.0).abs() <= 0.05 && (got.1 - want.1).abs() <= 0.05,
            "{test} {name}: {got:?} vs {want:?}"
        );
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Review D-08: pincushion and trapezoid over their whole hard range (|k| <= 0.3, |t| <= 0.5, and both at
/// once), dots near the corner (one native pixel in from the raster's last row and column, whose spot is cut
/// at the raster end) and elsewhere on the G10 tube, against the forward maps (pincushion
/// s (1 + k |s|^2 / 6400), then trapezoid x (1 + t y / 48)). The former inverse (four fixed-point steps) left
/// tens of px at k = -0.3 in the corner (about 10 px estimated at the dot (62, 46)). Tolerance 0.1 px, divided by the
/// local radial scale f' = 1 + 3 k (r / rc)^2 where it is under 0.5: the host pixel's footprint is not resized
/// for the local compression (geometry.glsl), so a barrel's corner (f' about 0.19 at k = -0.3) is sampled every
/// 5 raster px and the dot's centroid moves by up to about 0.1 px there (0.11 px measured).
#[test]
fn pincushion_and_trapezoid_hard_range_match_the_forward_map() {
    let test = "stage_geometry hard range";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let forward = |k: f64, t: f64, dot: (u32, u32)| {
        let s = (
            (f64::from(dot.0) + 0.5) * 2.0 - 64.0,
            (f64::from(dot.1) + 0.5) * 2.0 - 48.0,
        );
        let f = 1.0 + k * (s.0 * s.0 + s.1 * s.1) / 6400.0;
        let (x, y) = (s.0 * f, s.1 * f);
        (128.0 + 2.0 * x * (1.0 + t * y / 48.0), 96.0 + 2.0 * y)
    };
    // (k, t, dots): the dots stay inside the visible opening after the distortion.
    type Case = (f64, f64, &'static [(u32, u32)]);
    let cases: [Case; 8] = [
        (-0.3, 0.0, &[(62, 46), (56, 42), (10, 5), (1, 1)]),
        (-0.15, 0.0, &[(62, 46), (10, 5)]),
        (0.15, 0.0, &[(56, 42), (10, 5)]),
        (0.3, 0.0, &[(56, 42), (50, 38), (10, 5)]),
        (0.0, 0.5, &[(50, 38), (10, 5)]),
        (0.0, -0.5, &[(50, 38), (10, 5)]),
        (-0.3, 0.5, &[(50, 38), (62, 46)]),
        (0.3, -0.5, &[(50, 38), (20, 30)]),
    ];
    for (k, t, dots) in cases {
        let p = Params {
            geometry_pincushion: k as f32,
            geometry_trapezoid: t as f32,
            ..base()
        };
        for &dot in dots {
            let want = forward(k, t, dot);
            assert!(
                (0.0..256.0).contains(&want.0) && (0.0..192.0).contains(&want.1),
                "{want:?}"
            );
            let got = dot_centroid(&mut gpu, &p, dot, want);
            let r2 = ((f64::from(dot.0) + 0.5) * 2.0 - 64.0).powi(2) + ((f64::from(dot.1) + 0.5) * 2.0 - 48.0).powi(2);
            let radial_scale = 1.0 + 3.0 * k * r2 / 6400.0;
            let tol = 0.1 * (0.5 / radial_scale).max(1.0);
            println!("{test} k {k} t {t} dot {dot:?}: centroid {got:?}, expected {want:?}");
            assert!(
                (got.0 - want.0).abs() <= tol && (got.1 - want.1).abs() <= tol,
                "{test} k {k} t {t} dot {dot:?}: {got:?} vs {want:?} (tolerance {tol})"
            );
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// G16, G17: the eye at 600 mm, spherical corner dot and cylindrical column, against the oracle.
#[test]
fn finite_eye_matches_the_oracle() {
    let test = "stage_geometry finite eye";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let fc = tube(193, 145, 2, 1.0);
    // G16: spherical, corner dot (180, 135).
    let p = Params {
        geometry_curvature: GEOMETRY_CURVATURE_SPHERICAL,
        geometry_radius_mm: R_1R as f32,
        geometry_eye_distance_mm: 600.0,
        ..wide()
    };
    let (sx, sy) = sphere_seen_mm(R_1R, (168.0, 126.0), 600.0);
    let want = (193.0 + sx, 145.0 + sy);
    assert!(
        (want.0 - 352.4698).abs() < 2e-3 && (want.1 - 264.6024).abs() < 2e-3,
        "{want:?}"
    );
    let amp = currents(&fc, |x, y| if x == 180 && y == 135 { WHITE } else { [0.0; 3] });
    let out = run(&mut gpu, &["screen"], fc, &p, &amp);
    let got = centroid(&out, 1, window(want.0, 8, 386), window(want.1, 8, 290));
    println!("{test} sphere corner: centroid {got:?}, oracle {want:?}");
    assert!(
        (got.0 - want.0).abs() <= 0.05 && (got.1 - want.1).abs() <= 0.05,
        "{test} sphere corner: {got:?} vs {want:?}"
    );

    // G17: cylindrical, column 180 straight at x = 355.4496, and the dot (180, 135) at y 267.6289.
    let p = Params {
        geometry_curvature: GEOMETRY_CURVATURE_CYLINDRICAL,
        ..p
    };
    let (cx, cy) = cylinder_seen_eye_mm(R_1R, (168.0, 126.0), 600.0);
    let want = (193.0 + cx, 145.0 + cy);
    assert!(
        (want.0 - 355.4496).abs() < 2e-3 && (want.1 - 267.6289).abs() < 2e-3,
        "{want:?}"
    );
    let amp = currents(&fc, |x, _| if x == 180 { WHITE } else { [0.0; 3] });
    let out = run(&mut gpu, &["screen"], fc, &p, &amp);
    for y0 in [10_u32, 60, 144, 230, 276] {
        let (gx, _) = centroid(&out, 1, window(want.0, 10, 386), y0..y0 + 2);
        println!(
            "{test} cylinder rows {y0}..{}: centroid {gx:.4}, oracle {:.4}",
            y0 + 2,
            want.0
        );
        assert!((gx - want.0).abs() <= 0.05, "{test} cylinder rows {y0}: {gx}");
    }
    let amp = currents(&fc, |x, y| if x == 180 && y == 135 { WHITE } else { [0.0; 3] });
    let out = run(&mut gpu, &["screen"], fc, &p, &amp);
    let got = centroid(&out, 1, window(want.0, 8, 386), window(want.1, 8, 290));
    println!("{test} cylinder dot: centroid {got:?}, oracle {want:?}");
    assert!(
        (got.0 - want.0).abs() <= 0.05 && (got.1 - want.1).abs() <= 0.05,
        "{test} cylinder dot: {got:?} vs {want:?}"
    );
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// G18: raster_scale is 1 when the abl slot does not run, and exactly 1 on the first frame of a cold start.
#[test]
fn abl_state_is_well_defined() {
    let test = "stage_geometry abl state";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let mut fc = tube(12, 10, 6, 1440.0 / 290.0);
    fc.frame_time_ms = 0.0;
    let value = |x: u32, y: u32| (1 + (x * 5 + y * 3) % 11) as f32 / 16.0;
    let amp = currents(&fc, |x, y| [value(x, y), value(y, x), value(x + 1, y)]);
    // The exact tier: one beam path for both (T-48: the standard tier takes the line images for an unscaled
    // raster and the tile path once the breathing may scale it, which differ by up to one FP16 step).
    let exact = Params {
        quality_tier: ultimatecrt::params::QUALITY_TIER_EXACT,
        ..base()
    };
    let reference = run(&mut gpu, &["screen"], fc, &exact, &amp);
    let breathing = Params {
        abl_enable: false,
        abl_breathing: 0.03,
        abl_breathing_ref: 0.5,
        abl_tau_s: 0.1,
        ..exact
    };
    // abl disabled: its state is not current, raster_scale 1.
    let off = run(&mut gpu, &["screen"], fc, &breathing, &amp);
    assert_eq!(off.rgba, reference.rgba, "{test}: abl disabled, breathing on");
    // abl runs again (cold start, after a frame without it): apl_f seeded with the reference, alpha 0.
    let cold = run(&mut gpu, &["abl", "screen"], fc, &breathing, &amp);
    assert_eq!(cold.rgba, reference.rgba, "{test}: cold start, first frame");
    // The next frame with a real frame time follows the APL (not 0.5): the picture changes.
    fc.frame_time_ms = 16.7;
    let next = run(&mut gpu, &["abl", "screen"], fc, &breathing, &amp);
    assert_ne!(next.rgba, reference.rgba, "{test}: the state follows the APL");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// G19: overscan spreads the beam over a larger raster: the spot keeps its size in mm and the picture is
/// dimmer by 1 / (1 + overscan) (T-37 review F3).
#[test]
fn overscan_keeps_the_spot_size_and_dims() {
    let test = "stage_geometry overscan spot";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let fc = tube(64, 48, 4, 2.0);
    let ys = 64..128; // 16 whole line periods (pitch 4 px, unchanged by a horizontal overscan)
    let full = currents(&fc, |_, _| WHITE);
    let mean = |img: &ImageF32| {
        let s = column_sums(img, ys.clone());
        s[120..136].iter().sum::<f64>() / (16.0 * 64.0)
    };
    let p0 = base();
    let p1 = Params {
        geometry_overscan_x: 0.05,
        ..base()
    };
    let m0 = mean(&run(&mut gpu, &["screen"], fc, &p0, &full));
    let m1 = mean(&run(&mut gpu, &["screen"], fc, &p1, &full));
    println!(
        "{test}: flat white mean {m0:.6} -> {m1:.6} with 5 % overscan, ratio {:.6}",
        m1 / m0
    );
    assert!((m0 - 1.0).abs() <= 1e-3, "{test}: flat white {m0}");
    assert!((m1 / m0 - 1.0 / 1.05).abs() <= 1e-3, "{test}: dimming {}", m1 / m0);

    // A lit native column 32 with a 2 mm spot: seen box width 4 x 1.05 = 4.2 px, centre 128 + 2 x 1.05 =
    // 130.1; profile variance (box, spot, pixel) 4.2^2 / 12 + sigma^2 + 1 / 12 with sigma = 2 / 2.35482 x 2
    // = 1.698644 px (kept in mm): 1.47 + 2.885391 + 0.083333 = 4.438724 (the spot magnified with the raster
    // would give 4.734563).
    let p = Params {
        geometry_overscan_x: 0.05,
        ..wide()
    };
    let col = currents(&fc, |x, _| if x == 32 { WHITE } else { [0.0; 3] });
    let out = run(&mut gpu, &["screen"], fc, &p, &col);
    let s = column_sums(&out, ys.clone());
    let (mut e, mut m1x, mut m2x) = (0.0, 0.0, 0.0);
    for (x, v) in s.iter().enumerate().take(150).skip(110) {
        let c = x as f64 + 0.5;
        e += v;
        m1x += v * c;
        m2x += v * c * c;
    }
    let c = m1x / e;
    let var = m2x / e - c * c;
    let sigma: f64 = 2.0 / 2.354_82 * 2.0;
    let want = 4.2 * 4.2 / 12.0 + sigma * sigma + 1.0 / 12.0;
    assert!((want - 4.438_724).abs() < 1e-4, "{want}");
    println!("{test}: column centroid {c:.4} (130.1), variance {var:.5} (expected {want:.5})");
    assert!((c - 130.1).abs() <= 0.02, "{test}: centroid {c}");
    assert!((var - want).abs() <= 0.02, "{test}: variance {var} vs {want}");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

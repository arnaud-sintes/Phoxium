//! GPU tests of the interlace module (T-32, manual field mode; `params/interlace.toml`, `beam_raster` in
//! `shaders/include/beam.glsl`, `shaders/include/interlace.glsl`), headless, validation on, zero validation
//! messages, one shared device for the whole program.
//!
//! Every check injects its own `FrameConstants` (the frame index picks the field) and a beam-current image after
//! `gun` (amp domain, linear values), runs `screen` (and `phosphor` for the decay check) and compares the tube image
//! with the `crt-model` oracle: the raster of the field ([`Interlace::frame_raster`]) with the field's rows
//! gathered ([`Raster::gather`]) is a progressive raster, drawn by the 2D [`BeamOracle`].
//!
//! Set-up: 12 x 16 native pixels at 8 host px per line (tube 96 x 128 px, K = 4), 4 px per mm, line pitch 2 mm,
//! spot sigma 1.6 px (0.2 progressive pitches, 0.1 field pitches of a split woven frame), spot growth off, mask
//! off. Progressive line k is centred at (k + 0.5) 8 px. Hand-derived expectations:
//! - `split_woven_frame`, field f: lines of pitch 16 px from 4 + 8 f px, line n drawing row 2 n + f; each line
//!   deposits current x 16 px, so one field of a flat field keeps the mean and the mean of two consecutive
//!   fields is the progressive picture (each progressive line gets 16 / 2 = 8 px of deposit on average).
//!   At an odd progressive line centre, field 0's nearest lines are 8 px = 5 sigma away: below 4e-6 of white.
//! - `alternate_frames_are_fields`: field 1 is the progressive raster moved down by 4 px (half a pitch); the
//!   centroid of an isolated line moves by 4 px (PLAN T-32: P / 2 +/- 0.1 px).
//! - Off, and the top field of `alternate_frames_are_fields` (240p double strike), add nothing to the raster's
//!   arithmetic: bit-identical to the progressive render.

mod common;

use crt_model::beam::{AmpRaster, BeamOracle, Spot};
use crt_model::interlace::Raster;
use crt_model::units::fwhm_from_sigma;
use ultimatecrt::gpu::{
    warp_plan, Domain, FrameConstants, Headless, ImageF32, Settings, StageInput, StageRun, TargetFormat,
};
use ultimatecrt::params::{
    Params, BEAM_QUALITY_REFERENCE, BEAM_QUALITY_STANDARD, GEOMETRY_CURVATURE_SPHERICAL,
    INTERLACE_FIELD_ORDER_BOTTOM_FIRST, INTERLACE_FIELD_ORDER_TOP_FIRST, INTERLACE_FIELD_SELECT_ALTERNATE,
    INTERLACE_FIELD_SELECT_BOTTOM, INTERLACE_FIELD_SELECT_TOP, INTERLACE_MODE_ALTERNATE_FRAMES_ARE_FIELDS,
    INTERLACE_MODE_OFF, INTERLACE_MODE_SPLIT_WOVEN_FRAME,
};

const COLS: u32 = 12;
const ROWS: u32 = 16;
/// Host px per progressive line.
const N: u32 = 8;
const PPM: f32 = 4.0;
/// Spot core sigma, host px.
const SIGMA_PX: f64 = 1.6;

fn tube(frame_index: u32) -> FrameConstants {
    let mut fc = FrameConstants::new((COLS, ROWS), (COLS * N, ROWS * N), false);
    assert_eq!(fc.tube_size, [(COLS * N) as i32, (ROWS * N) as i32]);
    fc.px_per_mm = PPM;
    fc.line_pitch_mm = N as f32 / PPM;
    fc.frame_index = frame_index;
    fc
}

fn params(mode: u32, order: u32, select: u32, quality: u32) -> Params {
    let fwhm = fwhm_from_sigma(SIGMA_PX / f64::from(PPM)) as f32;
    Params {
        beam_fwhm_mid_mm: fwhm,
        beam_fwhm_peak_mm: fwhm,
        beam_quality: quality,
        mask_enabled: false,
        interlace_mode: mode,
        interlace_field_order: order,
        interlace_field_select: select,
        ..Params::default()
    }
}

/// The amp-domain beam-current image with value `f(amp column, row)` per channel.
fn currents(fc: &FrameConstants, f: impl Fn(u32, u32) -> [f32; 3]) -> ImageF32 {
    let (w, h) = fc.domain_size(Domain::Amp);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = f(x, y);
            rgba.extend_from_slice(&[r, g, b, 1.0]);
        }
    }
    ImageF32 {
        width: w,
        height: h,
        rgba,
    }
}

/// A varied picture: every row and amp column different, per channel.
fn varied(fc: &FrameConstants) -> ImageF32 {
    currents(fc, |x, y| {
        let h = x.wrapping_mul(2_654_435_761).wrapping_add(y.wrapping_mul(40_503)) >> 7;
        [
            (h % 97) as f32 / 96.0,
            ((h / 97) % 89) as f32 / 88.0,
            ((y * 7 + x / 4) % 5) as f32 / 4.0,
        ]
    })
}

/// Rows `lit` white, the others black.
fn rows_lit(fc: &FrameConstants, lit: &[u32]) -> ImageF32 {
    currents(fc, |_, y| if lit.contains(&y) { [1.0; 3] } else { [0.0; 3] })
}

/// Runs the slots `slots` on `amp` injected after `gun`; returns the image the last slot left.
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
    r.stage(slots[slots.len() - 1]).expect("slot ran").clone()
}

fn screen(gpu: &mut Headless, fc: FrameConstants, params: &Params, amp: &ImageF32) -> ImageF32 {
    run(gpu, &["screen"], fc, params, amp)
}

/// Mean of channel 1 over the host rows [y0, y1) and the middle columns (away from the raster's ends).
fn band(img: &ImageF32, y0: u32, y1: u32) -> f64 {
    let (x0, x1) = (img.width / 4, 3 * img.width / 4);
    let mut s = 0.0;
    for y in y0..y1 {
        for x in x0..x1 {
            s += f64::from(img.pixel(x, y)[1]);
        }
    }
    s / f64::from((y1 - y0) * (x1 - x0))
}

/// The oracle's interlace settings for the parameters.
fn model(p: &Params) -> crt_model::interlace::Interlace {
    ultimatecrt::interlace::model(p)
}

/// Every row of the checked columns equals the oracle of the frame's field raster. Tolerance as
/// tests/stage_beam_vertical.rs (FP16 storage, erf, the shader's 4-sigma truncation of lines and samples, with
/// the field's pitch).
fn assert_matches_oracle(what: &str, fc: &FrameConstants, p: &Params, amp: &ImageF32, out: &ImageF32) {
    let progressive = Raster::progressive(ROWS as usize, 1, f64::from(N), f64::from(ROWS * N));
    let field = model(p).frame_raster(&progressive, fc.frame_index);
    let samples = amp.width as usize;
    let a = f64::from(fc.tube_size[0]) / f64::from(amp.width);
    let sigma = |_: f64| SIGMA_PX;
    let spot = Spot {
        sigma_px: &sigma,
        tail_weight: 0.0,
        tail_ratio: 1.0,
    };
    let checked = [0, 1, out.width / 2, out.width / 2 + 3, out.width - 1];
    for c in 0..3 {
        let all: Vec<f64> = amp.rgba.chunks(4).map(|q| f64::from(q[c])).collect();
        let gathered = field.gather(&all, samples);
        let i_max = all.iter().cloned().fold(0.0, f64::max);
        let raster = AmpRaster {
            currents: &gathered,
            samples,
            sample_width: a,
            first: field.first,
            pitch: field.pitch,
            mult: field.mult,
        };
        let oracle = BeamOracle::new(raster, &spot);
        let truncation = 2.0 * i_max * (field.pitch * 1.338e-4 / SIGMA_PX + 3.17e-5) + 4.0 * 3.17e-5 * i_max;
        for &x in &checked {
            for y in 0..out.height {
                let want = oracle.pixel_mean(
                    (0.0, 0.0),
                    (f64::from(x), f64::from(x + 1)),
                    (f64::from(y), f64::from(y + 1)),
                );
                let got = f64::from(out.pixel(x, y)[c]);
                assert!(
                    (got - want).abs() <= 1e-3 * want.abs() + 1e-4 + truncation,
                    "{what}: pixel ({x}, {y}) channel {c}: GPU {got} vs oracle {want}"
                );
            }
        }
    }
}

const QUALITIES: [(&str, u32); 2] = [
    ("standard", BEAM_QUALITY_STANDARD),
    ("reference", BEAM_QUALITY_REFERENCE),
];

/// Off is bit-identical whatever the other interlace settings and the frame index; the top field of
/// `alternate_frames_are_fields` (240p double strike) too.
fn off_is_bit_exact(gpu: &mut Headless) {
    let amp = varied(&tube(0));
    for (qn, q) in QUALITIES {
        assert_eq!(
            Params::default().interlace_mode,
            INTERLACE_MODE_OFF,
            "off is the default"
        );
        let base = screen(gpu, tube(0), &params(INTERLACE_MODE_OFF, 0, 0, q), &amp);
        for (order, select, k) in [
            (INTERLACE_FIELD_ORDER_TOP_FIRST, INTERLACE_FIELD_SELECT_ALTERNATE, 1),
            (INTERLACE_FIELD_ORDER_BOTTOM_FIRST, INTERLACE_FIELD_SELECT_ALTERNATE, 0),
            (INTERLACE_FIELD_ORDER_TOP_FIRST, INTERLACE_FIELD_SELECT_BOTTOM, 3),
        ] {
            let off = screen(gpu, tube(k), &params(INTERLACE_MODE_OFF, order, select, q), &amp);
            assert!(
                off.rgba == base.rgba,
                "{qn}: off with order {order}, select {select}, frame {k} differs"
            );
        }
        for k in [0, 1] {
            let top = params(
                INTERLACE_MODE_ALTERNATE_FRAMES_ARE_FIELDS,
                0,
                INTERLACE_FIELD_SELECT_TOP,
                q,
            );
            let img = screen(gpu, tube(k), &top, &amp);
            assert!(
                img.rgba == base.rgba,
                "{qn}: locked top field (double strike) differs from progressive"
            );
        }
        assert_matches_oracle(
            &format!("off, {qn}"),
            &tube(0),
            &params(INTERLACE_MODE_OFF, 0, 0, q),
            &amp,
            &base,
        );
    }
}

/// Both modes, both fields, both qualities against the oracle.
fn fields_match_the_oracle(gpu: &mut Headless) {
    let amp = varied(&tube(0));
    for (qn, q) in QUALITIES {
        for (mn, mode) in [
            ("alternate frames", INTERLACE_MODE_ALTERNATE_FRAMES_ARE_FIELDS),
            ("split woven", INTERLACE_MODE_SPLIT_WOVEN_FRAME),
        ] {
            for (order, k) in [
                (INTERLACE_FIELD_ORDER_TOP_FIRST, 4),
                (INTERLACE_FIELD_ORDER_TOP_FIRST, 5),
                (INTERLACE_FIELD_ORDER_BOTTOM_FIRST, 4),
            ] {
                let p = params(mode, order, INTERLACE_FIELD_SELECT_ALTERNATE, q);
                let fc = tube(k);
                let img = screen(gpu, fc, &p, &amp);
                assert_matches_oracle(&format!("{mn}, {qn}, order {order}, frame {k}"), &fc, &p, &amp, &img);
            }
        }
    }
}

/// Split woven frame, fields alternating: two consecutive frames light complementary line sets, and their mean
/// is the progressive picture.
fn consecutive_frames_are_complementary(gpu: &mut Headless) {
    let white = currents(&tube(0), |_, _| [1.0; 3]);
    for (qn, q) in QUALITIES {
        let p = params(INTERLACE_MODE_SPLIT_WOVEN_FRAME, 0, INTERLACE_FIELD_SELECT_ALTERNATE, q);
        let a = screen(gpu, tube(6), &p, &white);
        let b = screen(gpu, tube(7), &p, &white);
        let prog = screen(gpu, tube(6), &params(INTERLACE_MODE_OFF, 0, 0, q), &white);
        let x = a.width / 2;
        for k in 1..ROWS - 1 {
            // Host row through progressive line k's centre ((k + 0.5) 8 = 8 k + 4: pixel 8 k + 3 or 8 k + 4).
            let y = k * N + N / 2;
            let (va, vb) = (f64::from(a.pixel(x, y)[1]), f64::from(b.pixel(x, y)[1]));
            let (on, off) = if k % 2 == 0 { (va, vb) } else { (vb, va) };
            assert!(
                on > 1.0 && off < 1e-4,
                "{qn}: line {k}: field on {on}, other field {off}"
            );
        }
        for y in 0..a.height {
            for x in [a.width / 2, a.width / 3] {
                let mean = 0.5 * (f64::from(a.pixel(x, y)[1]) + f64::from(b.pixel(x, y)[1]));
                let want = f64::from(prog.pixel(x, y)[1]);
                assert!(
                    (mean - want).abs() <= 2e-3 * want + 2e-4,
                    "{qn}: ({x}, {y}): mean of the fields {mean}, progressive {want}"
                );
            }
        }
        // One field of a flat field keeps the mean (whole field pitches in the middle).
        let (ma, mp) = (band(&a, 2 * N, 14 * N), band(&prog, 2 * N, 14 * N));
        assert!(
            (ma - mp).abs() <= 2e-3 * mp,
            "{qn}: field mean {ma}, progressive mean {mp}"
        );
    }
}

/// PLAN T-32 accept: a 1-line horizontal detail alternates on and off between successive outputs, a 2-line detail
/// stays lit in both.
fn one_line_detail_twitters(gpu: &mut Headless) {
    let p = params(
        INTERLACE_MODE_SPLIT_WOVEN_FRAME,
        0,
        INTERLACE_FIELD_SELECT_ALTERNATE,
        BEAM_QUALITY_STANDARD,
    );
    let one = rows_lit(&tube(0), &[9]);
    let two = rows_lit(&tube(0), &[4, 5]);
    let mut two_seen = Vec::new();
    for k in 0..4 {
        let l1 = band(&screen(gpu, tube(k), &p, &one), 9 * N, 10 * N);
        let l2 = band(&screen(gpu, tube(k), &p, &two), 4 * N, 6 * N);
        // Row 9 is odd: bottom field, odd frames (top field first).
        if k % 2 == 1 {
            assert!(l1 > 0.9, "frame {k}: 1-line detail should be lit: {l1}");
        } else {
            assert!(l1 < 1e-4, "frame {k}: 1-line detail should be dark: {l1}");
        }
        assert!(l2 > 0.4, "frame {k}: 2-line detail should be lit: {l2}");
        two_seen.push(l2);
    }
    let (lo, hi) = two_seen
        .iter()
        .fold((f64::MAX, 0.0_f64), |(l, h), &v| (l.min(v), h.max(v)));
    assert!(hi - lo <= 0.01 * hi, "2-line detail steady: {two_seen:?}");
}

/// PLAN T-32 accept: field 1 scanline centres shifted by P / 2 +/- 0.1 px (alternate frames are fields).
fn bottom_field_moves_half_a_pitch(gpu: &mut Headless) {
    let p = params(
        INTERLACE_MODE_ALTERNATE_FRAMES_ARE_FIELDS,
        0,
        INTERLACE_FIELD_SELECT_ALTERNATE,
        BEAM_QUALITY_STANDARD,
    );
    let amp = rows_lit(&tube(0), &[7]);
    let centroid = |img: &ImageF32| {
        let x = img.width / 2;
        let (mut s, mut sy) = (0.0, 0.0);
        for y in 0..img.height {
            let v = f64::from(img.pixel(x, y)[1]);
            s += v;
            sy += v * (f64::from(y) + 0.5);
        }
        sy / s
    };
    let c0 = centroid(&screen(gpu, tube(0), &p, &amp));
    let c1 = centroid(&screen(gpu, tube(1), &p, &amp));
    assert!((c0 - 7.5 * f64::from(N)).abs() <= 0.1, "top field centroid {c0}");
    assert!(
        (c1 - c0 - 0.5 * f64::from(N)).abs() <= 0.1,
        "shift {} px, want {}",
        c1 - c0,
        0.5 * f64::from(N)
    );
}

/// A locked field gives the same picture at every frame; the two locked fields differ.
fn locked_field_is_stable(gpu: &mut Headless) {
    let amp = varied(&tube(0));
    for mode in [
        INTERLACE_MODE_SPLIT_WOVEN_FRAME,
        INTERLACE_MODE_ALTERNATE_FRAMES_ARE_FIELDS,
    ] {
        let mut firsts = Vec::new();
        for select in [INTERLACE_FIELD_SELECT_TOP, INTERLACE_FIELD_SELECT_BOTTOM] {
            let p = params(mode, INTERLACE_FIELD_ORDER_BOTTOM_FIRST, select, BEAM_QUALITY_STANDARD);
            let first = screen(gpu, tube(0), &p, &amp);
            for k in 1..4 {
                let img = screen(gpu, tube(k), &p, &amp);
                assert!(
                    img.rgba == first.rgba,
                    "mode {mode}, select {select}: frame {k} differs from frame 0"
                );
            }
            firsts.push(first);
        }
        assert!(
            firsts[0].rgba != firsts[1].rgba,
            "mode {mode}: the two locked fields are the same picture"
        );
    }
}

/// With persistence on (T-41), the previous field's lines still glow, dimmer than the current field's.
fn previous_field_decays(gpu: &mut Headless) {
    let p = Params {
        phosphor_persistence: true,
        phosphor_frame_rate_hz: 60.0,
        ..params(
            INTERLACE_MODE_SPLIT_WOVEN_FRAME,
            0,
            INTERLACE_FIELD_SELECT_ALTERNATE,
            BEAM_QUALITY_STANDARD,
        )
    };
    let white = currents(&tube(0), |_, _| [1.0; 3]);
    gpu.clear_history();
    let slots = ["screen", "phosphor"];
    let mut fc = tube(0);
    fc.frame_time_ms = 1000.0 / 60.0;
    let _top = run(gpu, &slots, fc, &p, &white);
    fc.frame_index = 1;
    let bottom = run(gpu, &slots, fc, &p, &white);
    let x = bottom.width / 2;
    for k in 2..ROWS - 2 {
        let v = f64::from(bottom.pixel(x, k * N + N / 2)[1]);
        let w = f64::from(bottom.pixel(x, (k + 1) * N + N / 2)[1]);
        let (prev, cur) = if k % 2 == 0 { (v, w) } else { (w, v) };
        assert!(
            prev > 1e-4 && prev < 0.5 * cur,
            "line {k}: previous field {prev}, current field {cur}"
        );
    }
    let y = 8 * N + N / 2;
    let (cur, prev) = (bottom.pixel(x, y + N), bottom.pixel(x, y));
    println!(
        "stage_interlace: previous field / current field at the line centres, R G B: {:.3} {:.3} {:.3}",
        prev[0] / cur[0],
        prev[1] / cur[1],
        prev[2] / cur[2]
    );
    gpu.clear_history();
}

/// The warp path (curved faceplate, `beam_warp.glsl`) draws the field's rows too: a 1-line detail of an odd row
/// is dark in the top field and lit in the bottom one, and the warp path stays within the standard-vs-reference
/// contract (MAE 0.5 %, max 4 % of white, tests/stage_beam_warp.rs).
fn warp_path_draws_the_field(gpu: &mut Headless) {
    let fc = tube(0);
    let curved = |select: u32, q: u32| Params {
        geometry_curvature: GEOMETRY_CURVATURE_SPHERICAL,
        geometry_radius_mm: 60.0,
        ..params(INTERLACE_MODE_SPLIT_WOVEN_FRAME, 0, select, q)
    };
    assert!(
        warp_plan(&fc, &curved(INTERLACE_FIELD_SELECT_TOP, BEAM_QUALITY_STANDARD)).is_some(),
        "the curved case takes the warp path"
    );
    let one = rows_lit(&fc, &[9]);
    for (select, lit) in [
        (INTERLACE_FIELD_SELECT_TOP, false),
        (INTERLACE_FIELD_SELECT_BOTTOM, true),
    ] {
        let s = screen(gpu, fc, &curved(select, BEAM_QUALITY_STANDARD), &one);
        let r = screen(gpu, fc, &curved(select, BEAM_QUALITY_REFERENCE), &one);
        let total: f64 = s.rgba.chunks(4).map(|v| f64::from(v[1])).sum();
        if lit {
            assert!(total > 100.0, "bottom field: detail lit on the warp path ({total})");
        } else {
            assert!(total < 1e-2, "top field: detail dark on the warp path ({total})");
        }
        let (mut sum, mut max) = (0.0, 0.0_f64);
        for (a, b) in s.rgba.chunks(4).zip(r.rgba.chunks(4)) {
            for c in 0..3 {
                let e = (f64::from(a[c]) - f64::from(b[c])).abs();
                sum += e;
                max = max.max(e);
            }
        }
        let mae = sum / (s.rgba.len() / 4 * 3) as f64;
        assert!(
            mae <= 0.005 && max <= 0.04,
            "select {select}: warp vs reference MAE {mae}, max {max}"
        );
    }
}

#[test]
fn interlace_field_mode() {
    let test = "stage_interlace";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    type Check = fn(&mut Headless);
    let checks: [(&str, Check); 8] = [
        ("off is bit-exact", off_is_bit_exact),
        ("fields match the oracle", fields_match_the_oracle),
        (
            "consecutive frames are complementary",
            consecutive_frames_are_complementary,
        ),
        ("1-line detail twitters", one_line_detail_twitters),
        ("bottom field moves half a pitch", bottom_field_moves_half_a_pitch),
        ("locked field is stable", locked_field_is_stable),
        ("previous field decays", previous_field_decays),
        ("warp path draws the field", warp_path_draws_the_field),
    ];
    for (name, check) in checks {
        let t = std::time::Instant::now();
        check(&mut gpu);
        println!("{test}: {name}: ok ({:.2} s)", t.elapsed().as_secs_f64());
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

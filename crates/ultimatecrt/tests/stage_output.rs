//! GPU tests of the output transform (T-46): the `encode` stage, headless, with the validation layer
//! (synchronization validation on) and zero validation messages. The expected numbers come from the
//! `crt-model` f64 oracle (`output`, `colour`) and from the hand derivations below, not from the shader.
//!
//! The tube image is injected as the output of the last chain slot (`optics`), as linear light, so that
//! only `encode` runs: exposure, colour matrix, clip counters, shoulder, host EOTF inverse, dither.
//! Identity transfer on the host side (`Settings::identity`) isolates the linear part; a power or sRGB
//! host checks the encode.
//!
//! Hand derivations (CRT.md, Electro-optical transfer and Chromaticity sections):
//! - Dark tone 20/255: pure 2.4 gives (20/255)^2.4 = exp(2.4 ln 0.078431) = exp(2.4 x -2.54553) = exp(-6.1093)
//!   = 0.00222; pure 2.2 gives exp(-5.6002) = 0.00370. A pure 2.2 host shows 0.00370 as 20/255 (code 20), but
//!   0.00222 as 255 x 0.00222^(1/2.2) = 255 x exp(-6.1093 / 2.2) = 255 x exp(-2.7770) = 255 x 0.06217 = 15.85,
//!   code 16. A piecewise sRGB host has the linear toe 12.92 L below 0.0031308: 255 x 12.92 x 0.00222 = 7.31,
//!   code 7, and for 0.00370 (above the toe) 255 x (1.055 x 0.00370^(1/2.4) - 0.055) = 255 x (1.055 x 0.09697
//!   - 0.055) = 255 x 0.04730 = 12.06, code 12.
//! - SMPTE-C primaries with D65 to sRGB (matrix of `crt_model::output`, hand-checked there): the first
//!   column (the red primary) is (0.9396, 0.0178, -0.0016), the second (green) (0.0502, 0.9658, -0.0044),
//!   and the third (blue) (0.0103, 0.0164, 1.0060). Full red gives a negative blue channel (-0.0016) and full
//!   green too (-0.0044): two gamut-clipped pixels; full blue gives 1.0060 in blue: one pixel over panel white;
//!   rows sum to 1.
//! - Exposure: linear in E, so 0.5 halves every value.
//! - Shoulder with knee 0.8 at x = 1: 0.8 + 0.2 (1 - exp(-1)) = 0.926424; at x = 2: 0.8 + 0.2 (1 - exp(-6))
//!   = 0.999505 (T-46 asymptotic shoulder = T-84 shoulder of rate 1, a white point far above 1).
//! - T-84 headroom modes: with only `encode` running the readout is exposure x white matrix maximum; the default
//!   shoulder mode with its automatic white point leaves a readout <= 1 untouched (hard clip at 1 = the T-46
//!   identity chain), physical and mask depth divide the exposure by readout x (1 + margin).
//! - Ordered dither of the flat value 127.5 codes: thresholds (b + 0.5) / 64, b = 0..63; floor(127.5 + t) is 128
//!   when t >= 0.5, i.e. for b >= 32: half of the pixels, mean 127.5 exactly, each pixel within 0.5 code.

//! - Ordered dither at 127.25 and 127.75 codes (the fractional part f is the share of pixels that must round
//!   up): thresholds t_b = (b + 0.5) / 64 and floor(c + t_b) = 128 when t_b >= 1 - f. For f = 0.25 that is
//!   t_b >= 0.75, i.e. b + 0.5 >= 48, b >= 48 (b = 47 gives 0.7422 < 0.75): 16 of 64 thresholds, share 0.25,
//!   mean exactly 127.25. For f = 0.75: t_b >= 0.25, b + 0.5 >= 16, b >= 16 (b = 15 gives 0.2422): 48 of 64,
//!   share 0.75, mean exactly 127.75. A 256 x 256 field holds whole 8 x 8 tiles, so the ordered mean is exact
//!   (tolerance 1e-3 for the f32 rounding of the input). Noise: each pixel is 127 or 128, a Bernoulli of
//!   probability f, variance f (1 - f) <= 0.1875; the mean of N = 65536 pixels has a standard deviation of
//!   sqrt(0.1875 / N) = 0.0017, so the tolerance is 5 sigma (0.0085) plus 0.01 for the hardware rounding fuzz.

mod common;

use crt_model::colour::{gun_power, srgb_eotf};
use crt_model::output::{
    apply, dithered_code, encode_power, encode_srgb, ordered_threshold, shoulder, shoulder_rate, shoulder_with_rate,
    tube_to_display, D65, IDENTITY,
};
use ultimatecrt::gpu::{
    output_matrix, ClipCounts, FrameConstants, Headless, ImageF32, Rendered, Settings, StageInput, StageRun,
    TargetFormat, Transfer,
};
use ultimatecrt::params::{
    meta, ParamValue, Params, OUTPUT_HEADROOM_MODE_CLIP, OUTPUT_HEADROOM_MODE_MASK_DEPTH, OUTPUT_HEADROOM_MODE_PHYSICAL,
};

/// Renders `tube` (linear light, one texel per host pixel) through `encode` only, into a target of the
/// same size, as `format`. `frame_index` is injected into the frame constants.
fn encode(
    gpu: &mut Headless,
    tube: &ImageF32,
    params: &Params,
    host: Transfer,
    format: TargetFormat,
    frame_index: u32,
) -> Rendered {
    let mut fc = FrameConstants::new((tube.width, tube.height), (tube.width, tube.height), false);
    fc.frame_index = frame_index;
    let settings = Settings {
        host,
        ..Settings::identity()
    };
    let run = StageRun {
        input: StageInput::Image {
            after: "optics",
            image: tube,
        },
        frame: fc,
        settings: &settings,
        params,
        format,
    };
    gpu.render_stages(&run).expect("render_stages")
}

fn image(width: u32, height: u32, f: impl Fn(u32, u32) -> [f32; 3]) -> ImageF32 {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let [r, g, b] = f(x, y);
            rgba.extend_from_slice(&[r, g, b, 1.0]);
        }
    }
    ImageF32 { width, height, rgba }
}

fn set(p: &mut Params, id: &str, v: ParamValue) {
    assert!(p.set(id, v), "parameter {id}");
}

fn dither_mode(p: &mut Params, name: &str) {
    let i = meta("output_dither")
        .unwrap()
        .values
        .iter()
        .position(|v| *v == name)
        .expect("dither mode");
    set(p, "output_dither", ParamValue::Enum(i as u32));
}

fn tube_faithful() -> Params {
    let mut p = Params::default();
    set(&mut p, "output_match_srgb", ParamValue::Bool(false));
    p
}

/// Code (0..=255) of a Bgra8 output channel.
fn code(v: f32) -> i32 {
    (v * 255.0).round() as i32
}

fn gpu(name: &str) -> Option<Headless> {
    common::gpu_or_skip(name)
}

#[test]
fn matrix_srgb_primaries_with_d65_is_identity() {
    // No GPU needed: the matrix the stage pushes.
    let mut p = tube_faithful();
    set(&mut p, "tube_red_xy", ParamValue::Vec2([0.64, 0.33]));
    set(&mut p, "tube_green_xy", ParamValue::Vec2([0.30, 0.60]));
    set(&mut p, "tube_blue_xy", ParamValue::Vec2([0.15, 0.06]));
    set(&mut p, "tube_white_xy", ParamValue::Vec2([0.3127, 0.3290]));
    let m = output_matrix(&p);
    for (i, row) in m.iter().enumerate() {
        for (j, v) in row.iter().enumerate() {
            let want = if i == j { 1.0 } else { 0.0 };
            assert!((v - want).abs() < 1e-5, "[{i}][{j}] = {v}");
        }
    }
}

#[test]
fn matrix_smpte_c_rows_sum_to_one_and_follow_the_oracle() {
    let p = tube_faithful();
    let m = output_matrix(&p);
    let want = tube_to_display((0.630, 0.340), (0.310, 0.595), (0.155, 0.070), D65);
    // The parameters are f32 (0.3127 is not exact), the oracle here takes the f64 literals: 1e-6 apart.
    for i in 0..3 {
        for j in 0..3 {
            assert!(
                (m[i][j] - want[i][j]).abs() < 1e-6,
                "[{i}][{j}]: {} vs {}",
                m[i][j],
                want[i][j]
            );
        }
    }
    for (i, row) in m.iter().enumerate() {
        assert!((row.iter().sum::<f64>() - 1.0).abs() < 1e-4, "row {i}: {row:?}");
    }
    // Match sRGB colours: identity, whatever the tube xy.
    assert_eq!(output_matrix(&Params::default()), IDENTITY);
    // A degenerate tube (collinear primaries, y = 0) falls back to the identity instead of panicking.
    let mut bad = tube_faithful();
    set(&mut bad, "tube_green_xy", ParamValue::Vec2([0.5, 0.5]));
    set(&mut bad, "tube_red_xy", ParamValue::Vec2([0.4, 0.4]));
    set(&mut bad, "tube_blue_xy", ParamValue::Vec2([0.6, 0.6]));
    assert_eq!(output_matrix(&bad), IDENTITY);
    let mut bad = tube_faithful();
    set(&mut bad, "tube_white_xy", ParamValue::Vec2([0.3, 0.0]));
    assert_eq!(output_matrix(&bad), IDENTITY);
}

#[test]
fn colour_matrix_on_the_gpu_matches_the_oracle() {
    let Some(mut gpu) = gpu("colour_matrix_on_the_gpu_matches_the_oracle") else {
        return;
    };
    let colours: [[f32; 3]; 8] = [
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
        [1.0, 1.0, 1.0],
        [0.5, 0.5, 0.5],
        [0.2, 0.6, 0.9],
        [0.9, 0.3, 0.1],
        [0.0, 0.0, 0.0],
    ];
    let tube = image(8, 1, |x, _| colours[x as usize]);
    let p = tube_faithful();
    let m = output_matrix(&p);
    let r = encode(&mut gpu, &tube, &p, Transfer::Identity, TargetFormat::Rgba32F, 0);
    for (x, c) in colours.iter().enumerate() {
        let want = apply(&m, c.map(f64::from)).map(|v| shoulder(v, 1.0));
        let got = r.output.pixel(x as u32, 0);
        for k in 0..3 {
            assert!(
                (f64::from(got[k]) - want[k]).abs() < 1e-5,
                "pixel {x} channel {k}: {} vs {}",
                got[k],
                want[k]
            );
        }
    }
    // Full green: blue = -0.0044 clips to 0 (gamut); full blue: 1.0060 clips to 1; white stays white.
    assert_eq!(r.output.pixel(1, 0)[2], 0.0);
    assert_eq!(r.output.pixel(2, 0)[2], 1.0);
    let w = r.output.pixel(3, 0);
    assert!(w[..3].iter().all(|v| (v - 1.0).abs() < 1e-5), "{w:?}");
    // Hand count (see the module doc): red and green leave the panel gamut (negative blue channel), blue is
    // 1.0060 in blue, over panel white; white and the greys are neither.
    assert_eq!(
        r.clip,
        ClipCounts {
            over_white: 1,
            out_of_gamut: 2
        }
    );
    drop(gpu);
}

#[test]
fn transform_off_is_an_exact_identity() {
    let Some(mut gpu) = gpu("transform_off_is_an_exact_identity") else {
        return;
    };
    let counter = gpu.validation_counter();
    // Values up to 1 pass bit-exactly: default parameters (match sRGB, knee 1, exposure 1, no dither),
    // identity host transfer.
    let tube = image(256, 2, |x, y| {
        let v = x as f32 / 255.0;
        [v, 1.0 - v, if y == 0 { v * v } else { 1.0 }]
    });
    let r = encode(
        &mut gpu,
        &tube,
        &Params::default(),
        Transfer::Identity,
        TargetFormat::Rgba32F,
        3,
    );
    assert_eq!(r.output, tube);
    assert_eq!(r.clip, ClipCounts::default());
    // The 8-bit target gives back every code, with the dither modes on too (exact codes are fixed points
    // of floor(v 255 + t), t in [0, 1)).
    let ramp = image(256, 1, |x, _| {
        let v = x as f32 / 255.0;
        [v, v, v]
    });
    for mode in ["off", "ordered", "noise"] {
        let mut p = Params::default();
        dither_mode(&mut p, mode);
        for frame in [0, 1, 17] {
            let r = encode(&mut gpu, &ramp, &p, Transfer::Identity, TargetFormat::Bgra8Unorm, frame);
            for x in 0..256u32 {
                let got = r.output.pixel(x, 0);
                for c in &got[..3] {
                    assert_eq!(code(*c), x as i32, "mode {mode} frame {frame} code {x}");
                }
            }
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

#[test]
fn host_eotf_round_trips_all_256_codes() {
    let Some(mut gpu) = gpu("host_eotf_round_trips_all_256_codes") else {
        return;
    };
    // Linear light of each code through the host EOTF, encoded back by the stage, dither off.
    for (name, host, eotf) in [
        (
            "pure 2.2",
            Transfer::Power(2.2),
            (|v| gun_power(v, 2.2)) as fn(f64) -> f64,
        ),
        ("pure 2.4", Transfer::Power(2.4), |v| gun_power(v, 2.4)),
        ("sRGB", Transfer::Srgb, srgb_eotf),
    ] {
        let tube = image(256, 1, |x, _| {
            let l = eotf(f64::from(x) / 255.0) as f32;
            [l, l, l]
        });
        let r = encode(&mut gpu, &tube, &Params::default(), host, TargetFormat::Bgra8Unorm, 0);
        for x in 0..256u32 {
            let p = r.output.pixel(x, 0);
            assert!(
                (0..3).all(|c| code(p[c]) == x as i32),
                "{name}: code {x} came back as {:?}",
                &p[..3]
            );
        }
    }
    drop(gpu);
}

#[test]
fn host_eotf_encode_matches_the_oracle_in_float() {
    let Some(mut gpu) = gpu("host_eotf_encode_matches_the_oracle_in_float") else {
        return;
    };
    let tube = image(64, 1, |x, _| {
        let l = (f64::from(x) / 63.0).powi(3) as f32;
        [l, l * 0.5, 1.0 - l]
    });
    for (host, f) in [
        (Transfer::Power(2.2), (|l| encode_power(l, 2.2)) as fn(f64) -> f64),
        (Transfer::Srgb, encode_srgb),
    ] {
        let r = encode(&mut gpu, &tube, &Params::default(), host, TargetFormat::Rgba32F, 0);
        for x in 0..64u32 {
            let t = tube.pixel(x, 0);
            let g = r.output.pixel(x, 0);
            for c in 0..3 {
                let want = f(f64::from(t[c]));
                assert!(
                    (f64::from(g[c]) - want).abs() < 2e-5,
                    "{host:?} x {x} c {c}: {} vs {want}",
                    g[c]
                );
            }
        }
    }
    drop(gpu);
}

#[test]
fn dark_tone_20_of_255() {
    let Some(mut gpu) = gpu("dark_tone_20_of_255") else {
        return;
    };
    let l24 = gun_power(20.0 / 255.0, 2.4) as f32; // 0.00222
    let l22 = gun_power(20.0 / 255.0, 2.2) as f32; // 0.00370
    assert!((l24 - 0.00222).abs() < 1e-5 && (l22 - 0.00370).abs() < 1e-5);
    let tube = image(2, 1, |x, _| if x == 0 { [l24; 3] } else { [l22; 3] });
    let render = |gpu: &mut Headless, host| {
        let r = encode(gpu, &tube, &Params::default(), host, TargetFormat::Bgra8Unorm, 0);
        (code(r.output.pixel(0, 0)[0]), code(r.output.pixel(1, 0)[0]))
    };
    // Pure 2.2 host (see the derivations in the module doc): 15.85 -> 16 and 20.
    assert_eq!(render(&mut gpu, Transfer::Power(2.2)), (16, 20));
    // Piecewise sRGB host: toe 7.31 -> 7, and 12.06 -> 12.
    assert_eq!(render(&mut gpu, Transfer::Srgb), (7, 12));
    drop(gpu);
}

#[test]
fn exposure_scales_linear_light() {
    let Some(mut gpu) = gpu("exposure_scales_linear_light") else {
        return;
    };
    let tube = image(4, 1, |x, _| {
        let v = [0.05, 0.2, 0.6, 0.9][x as usize];
        [v, v * 0.5, 1.0 - v]
    });
    let mut p = Params::default();
    set(&mut p, "exposure", ParamValue::F32(0.5));
    let r = encode(&mut gpu, &tube, &p, Transfer::Identity, TargetFormat::Rgba32F, 0);
    for x in 0..4 {
        let (t, g) = (tube.pixel(x, 0), r.output.pixel(x, 0));
        for c in 0..3 {
            let want = f64::from(t[c]) * 0.5;
            assert!(
                (f64::from(g[c]) - want).abs() <= 1e-3 * want.max(1e-6),
                "x {x} c {c}: {} vs {want}",
                g[c]
            );
        }
    }
    drop(gpu);
}

#[test]
fn shoulder_is_identity_below_the_knee_monotonic_above_and_hard_at_knee_1() {
    let Some(mut gpu) = gpu("shoulder_is_identity_below_the_knee") else {
        return;
    };
    // 0 to 2.5 in 250 steps, repeated in the three channels.
    let tube = image(251, 1, |x, _| {
        let v = x as f32 * 0.01;
        [v, v, v]
    });
    // T-84: shoulder mode (the default) with a far white point: T = (64 - 0.8) / 0.2 = 316, rate
    // 1 - exp(-316) = 1 in f64, which is the T-46 asymptotic shoulder exactly.
    let mut soft = Params::default();
    set(&mut soft, "output_knee", ParamValue::F32(0.8));
    set(&mut soft, "output_shoulder_white", ParamValue::F32(64.0));
    assert_eq!(shoulder_rate(0.8, 64.0), Some(1.0));
    let r = encode(&mut gpu, &tube, &soft, Transfer::Identity, TargetFormat::Rgba32F, 0);
    let mut last = -1.0f32;
    for x in 0..251u32 {
        let v = f64::from(tube.pixel(x, 0)[0]);
        let g = r.output.pixel(x, 0)[0];
        assert!(
            (f64::from(g) - shoulder(v, 0.8)).abs() < 2e-6,
            "x {x}: {g} vs {}",
            shoulder(v, 0.8)
        );
        if v <= 0.8 {
            assert_eq!(f64::from(g), v, "identity below the knee, x {x}");
        }
        assert!(g >= last && g < 1.0 + 1e-6, "x {x}: {g}");
        if v > 0.8 && v < 2.0 {
            assert!(g > last, "strictly increasing above the knee, x {x}");
        }
        last = g;
    }
    // Hand values: 0.926424 at 1, 0.999505 at 2.
    assert!((f64::from(r.output.pixel(100, 0)[0]) - 0.926_424).abs() < 1e-5);
    assert!((f64::from(r.output.pixel(200, 0)[0]) - 0.999_505).abs() < 1e-5);
    // Hard clip at panel white: knee 1, the clip mode, and the default shoulder mode when the headroom readout
    // is 1 (only `encode` runs, identity matrix, exposure 1: nothing above panel white to roll off).
    let mut knee1 = Params::default();
    set(&mut knee1, "output_knee", ParamValue::F32(1.0));
    set(&mut knee1, "output_shoulder_white", ParamValue::F32(4.0));
    let mut clip = Params::default();
    set(
        &mut clip,
        "output_headroom_mode",
        ParamValue::Enum(OUTPUT_HEADROOM_MODE_CLIP),
    );
    set(&mut clip, "output_shoulder_white", ParamValue::F32(4.0));
    for p in [knee1, clip, Params::default()] {
        let r = encode(&mut gpu, &tube, &p, Transfer::Identity, TargetFormat::Rgba32F, 0);
        for x in 0..251u32 {
            let v = tube.pixel(x, 0)[0];
            assert_eq!(r.output.pixel(x, 0)[0], v.min(1.0), "x {x}");
        }
    }
    drop(gpu);
}

/// T-84 shoulder to a white point: knee 0.6 (the default), white point 2: T = 1.4 / 0.4 = 3.5, the rate solves
/// 1 - exp(-3.5 a) = a (crt_model::output::shoulder_rate, about 0.96597: exp(-3.3809) = 0.034017). The GPU, given the f32 rate, matches
/// the f64 oracle `shoulder_with_rate`, reaches panel white at x = 2 (within the f32 exp) and clips above;
/// identity below the knee.
#[test]
fn shoulder_reaches_panel_white_at_the_white_point() {
    let Some(mut gpu) = gpu("shoulder_reaches_panel_white_at_the_white_point") else {
        return;
    };
    let tube = image(251, 1, |x, _| {
        let v = x as f32 * 0.01;
        [v, v, v]
    });
    let mut p = Params::default();
    set(&mut p, "output_shoulder_white", ParamValue::F32(2.0));
    let rate = f64::from(shoulder_rate(0.6, 2.0).unwrap() as f32);
    assert!((rate - 0.965_97).abs() < 1e-4, "{rate}");
    let r = encode(&mut gpu, &tube, &p, Transfer::Identity, TargetFormat::Rgba32F, 0);
    let mut last = -1.0f32;
    for x in 0..251u32 {
        let v = f64::from(tube.pixel(x, 0)[0]);
        let g = r.output.pixel(x, 0)[0];
        let want = shoulder_with_rate(v, 0.6, Some(rate));
        assert!((f64::from(g) - want).abs() < 2e-6, "x {x}: {g} vs {want}");
        if v <= 0.6 {
            assert_eq!(f64::from(g), v, "identity below the knee, x {x}");
        }
        assert!(g >= last && g <= 1.0, "x {x}: {g}");
        last = g;
    }
    assert!((f64::from(r.output.pixel(200, 0)[0]) - 1.0).abs() < 1e-5);
    assert_eq!(r.output.pixel(250, 0)[0], 1.0);
    drop(gpu);
}

/// T-84 physical and mask depth modes on `encode` alone: the headroom readout is the exposure times the white
/// matrix maximum (no mask, no scanlines), here 2 x 1 = 2, so the pushed exposure is 2 / (2 x 1.02) = 0.980392
/// and the brightest input (1.0) lands at 1 / 1.02 = 0.980392: nothing clips, the clip counter stays 0. Mask
/// depth has no mask to reduce here and derives the same exposure. The readout is reported by the renderer.
#[test]
fn physical_mode_derives_the_exposure_from_the_readout() {
    let Some(mut gpu) = gpu("physical_mode_derives_the_exposure") else {
        return;
    };
    let tube = image(5, 1, |x, _| {
        let v = [0.1, 0.3, 0.5, 0.8, 1.0][x as usize];
        [v, v, v]
    });
    for mode in [OUTPUT_HEADROOM_MODE_PHYSICAL, OUTPUT_HEADROOM_MODE_MASK_DEPTH] {
        let mut p = Params::default();
        set(&mut p, "exposure", ParamValue::F32(2.0));
        set(&mut p, "output_headroom_mode", ParamValue::Enum(mode));
        let r = encode(&mut gpu, &tube, &p, Transfer::Identity, TargetFormat::Rgba32F, 0);
        let h = gpu.last_headroom().expect("readout");
        assert_eq!(
            (h.mask_peak, h.scanline_peak, h.matrix_max, h.exposure),
            (1.0, 1.0, 1.0, 2.0)
        );
        let e = 2.0 / (2.0 * 1.02);
        for x in 0..5 {
            let want = f64::from(tube.pixel(x, 0)[0]) * e;
            let got = f64::from(r.output.pixel(x, 0)[0]);
            assert!((got - want).abs() < 1e-6, "mode {mode} x {x}: {got} vs {want}");
        }
        assert_eq!(r.clip.over_white, 0, "mode {mode}");
        // Clip mode with the same exposure: 1.6 and 2.0 clip (0.5 x 2 = 1.0 is panel white, not over).
        set(
            &mut p,
            "output_headroom_mode",
            ParamValue::Enum(OUTPUT_HEADROOM_MODE_CLIP),
        );
        let r = encode(&mut gpu, &tube, &p, Transfer::Identity, TargetFormat::Rgba32F, 0);
        assert_eq!(r.clip.over_white, 2);
        assert_eq!(r.output.pixel(4, 0)[0], 1.0);
    }
    drop(gpu);
}

#[test]
fn clip_counters_count_the_saturated_pixels_of_a_known_pattern() {
    let Some(mut gpu) = gpu("clip_counters_count_the_saturated_pixels") else {
        return;
    };
    let counter = gpu.validation_counter();
    // A 32 x 16 tube inside a 40 x 16 target: 4 black surround pixels on each side, never counted.
    let (w, h) = (32u32, 16u32);
    // Pixel kinds by column x % 8: 0 exactly white (not clipped), 1 over white in red (1.5), 2 over white
    // in blue only (1.0001 + margin: 1.01), 3 below white (0.99), 4 black, 5 to 7 mid grey.
    let tube = image(w, h, |x, _| match x % 8 {
        0 => [1.0, 1.0, 1.0],
        1 => [1.5, 0.2, 0.2],
        2 => [0.2, 0.2, 1.01],
        3 => [0.99, 0.99, 0.99],
        4 => [0.0, 0.0, 0.0],
        _ => [0.4, 0.4, 0.4],
    });
    let per_row_over = 2; // columns 1 and 2 of every 8
    let want_over = per_row_over * (w / 8) * h;
    let mut fc = FrameConstants::new((w, h), (w + 8, h), false);
    assert_eq!(fc.tube_size, [w as i32, h as i32], "tube {fc:?}");
    fc.frame_index = 0;
    let settings = Settings::identity();
    let render = |gpu: &mut Headless, p: &Params| {
        let run = StageRun {
            input: StageInput::Image {
                after: "optics",
                image: &tube,
            },
            frame: fc,
            settings: &settings,
            params: p,
            format: TargetFormat::Rgba32F,
        };
        gpu.render_stages(&run).expect("render_stages")
    };
    let r = render(&mut gpu, &Params::default());
    assert_eq!(r.clip.over_white, want_over);
    assert_eq!(r.clip.out_of_gamut, 0);
    // Black surround: the tube rectangle is centred, 4 pixels each side.
    assert_eq!(r.output.pixel(0, 0), [0.0, 0.0, 0.0, 1.0]);
    // The counters are cleared every frame: a second frame gives the same count, not double.
    assert_eq!(render(&mut gpu, &Params::default()).clip.over_white, want_over);
    // The soft shoulder does not change what is counted.
    let mut soft = Params::default();
    set(&mut soft, "output_knee", ParamValue::F32(0.7));
    assert_eq!(render(&mut gpu, &soft).clip.over_white, want_over);
    // Exposure 2: 0.99 -> 1.98 and the mid greys 0.4 -> 0.8 (not over); columns 0, 1, 2, 3 are over now.
    let mut bright = Params::default();
    set(&mut bright, "exposure", ParamValue::F32(2.0));
    assert_eq!(render(&mut gpu, &bright).clip.over_white, 4 * (w / 8) * h);
    // Tube-faithful colours: only the full-green style pixels leave the gamut; this pattern has none
    // with a negative channel beyond -1e-4 except the saturated column 1 (red 1.5, small g and b).
    let faithful = tube_faithful();
    let m = output_matrix(&faithful);
    let negative = |c: [f32; 3]| apply(&m, c.map(f64::from)).iter().any(|v| *v < -1e-4);
    let columns: [[f32; 3]; 8] = [
        [1.0; 3],
        [1.5, 0.2, 0.2],
        [0.2, 0.2, 1.01],
        [0.99; 3],
        [0.0; 3],
        [0.4; 3],
        [0.4; 3],
        [0.4; 3],
    ];
    let want_gamut = columns.iter().filter(|c| negative(**c)).count() as u32 * (w / 8) * h;
    assert_eq!(render(&mut gpu, &faithful).clip.out_of_gamut, want_gamut);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Flat field of `v` (linear = code value with the identity host transfer) rendered as 8-bit codes.
fn codes_of(r: &Rendered) -> Vec<i32> {
    r.output.rgba.chunks(4).map(|p| code(p[0])).collect()
}

#[test]
fn dither_statistics_determinism_and_off() {
    let Some(mut gpu) = gpu("dither_statistics_determinism_and_off") else {
        return;
    };
    let counter = gpu.validation_counter();
    // A flat field at half a code: 127.5 / 255.
    let v = 127.5f32 / 255.0;
    let (w, h) = (256u32, 256u32);
    let flat = image(w, h, |_, _| [v, v, v]);
    let n = f64::from(w * h);
    let mean = |codes: &[i32]| codes.iter().map(|c| f64::from(*c)).sum::<f64>() / n;

    // Off: a single value (the hardware rounds), no spread.
    let off = codes_of(&encode(
        &mut gpu,
        &flat,
        &Params::default(),
        Transfer::Identity,
        TargetFormat::Bgra8Unorm,
        0,
    ));
    assert!(off.iter().all(|c| *c == off[0]));

    // Ordered: exactly the oracle, pixel by pixel; mean 127.5; static (independent of the frame index).
    let mut p = Params::default();
    dither_mode(&mut p, "ordered");
    let a = codes_of(&encode(
        &mut gpu,
        &flat,
        &p,
        Transfer::Identity,
        TargetFormat::Bgra8Unorm,
        0,
    ));
    let b = codes_of(&encode(
        &mut gpu,
        &flat,
        &p,
        Transfer::Identity,
        TargetFormat::Bgra8Unorm,
        5,
    ));
    assert_eq!(a, b, "ordered dither is static");
    for y in 0..h {
        for x in 0..w {
            let want = dithered_code(f64::from(v), 255.0, ordered_threshold(x, y));
            assert_eq!(f64::from(a[(y * w + x) as usize]), want, "pixel ({x}, {y})");
        }
    }
    assert!((mean(&a) - 127.5).abs() < 1e-9, "ordered mean {}", mean(&a));

    // Noise: mean within 0.1 LSB, every pixel within 1 LSB of the exact value, deterministic for a
    // frame index, different from another frame index.
    dither_mode(&mut p, "noise");
    let n0 = codes_of(&encode(
        &mut gpu,
        &flat,
        &p,
        Transfer::Identity,
        TargetFormat::Bgra8Unorm,
        0,
    ));
    let n0_again = codes_of(&encode(
        &mut gpu,
        &flat,
        &p,
        Transfer::Identity,
        TargetFormat::Bgra8Unorm,
        0,
    ));
    let n1 = codes_of(&encode(
        &mut gpu,
        &flat,
        &p,
        Transfer::Identity,
        TargetFormat::Bgra8Unorm,
        1,
    ));
    assert_eq!(n0, n0_again, "same frame index, same pattern");
    assert_ne!(n0, n1, "the noise moves with the frame index");
    for (name, c) in [("noise 0", &n0), ("noise 1", &n1)] {
        assert!((mean(c) - 127.5).abs() < 0.1, "{name}: mean {}", mean(c));
        assert!(
            c.iter().all(|k| (f64::from(*k) - 127.5).abs() <= 1.0),
            "{name}: max error above 1 LSB"
        );
    }
    println!(
        "flat 127.5: ordered mean {}, noise means {} and {}",
        mean(&a),
        mean(&n0),
        mean(&n1)
    );

    // A float target is never dithered.
    let r = encode(&mut gpu, &flat, &p, Transfer::Identity, TargetFormat::Rgba32F, 0);
    assert!(r.output.rgba.chunks(4).all(|px| px[0] == v));
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Flat fields at 127.25 and 127.75 codes (fractional part other than one half), ordered and noise modes:
/// means from the hand derivation in the module doc, every code within 1 of the exact value.
#[test]
fn dither_fractional_codes_other_than_one_half() {
    let Some(mut gpu) = gpu("dither_fractional_codes_other_than_one_half") else {
        return;
    };
    let counter = gpu.validation_counter();
    let (w, h) = (256u32, 256u32);
    let n = f64::from(w * h);
    let mean = |codes: &[i32]| codes.iter().map(|c| f64::from(*c)).sum::<f64>() / n;
    let noise_tol = 5.0 * (0.1875 / n).sqrt() + 0.01;
    for exact in [127.25f64, 127.75] {
        let v = (exact / 255.0) as f32;
        let flat = image(w, h, |_, _| [v, v, v]);
        let mut p = Params::default();
        dither_mode(&mut p, "ordered");
        let o = codes_of(&encode(
            &mut gpu,
            &flat,
            &p,
            Transfer::Identity,
            TargetFormat::Bgra8Unorm,
            0,
        ));
        assert!((mean(&o) - exact).abs() < 1e-3, "ordered {exact}: mean {}", mean(&o));
        assert!(
            o.iter().all(|k| (f64::from(*k) - exact).abs() < 1.0),
            "ordered {exact}: error above 1"
        );
        for y in 0..h {
            for x in 0..w {
                let want = dithered_code(f64::from(v), 255.0, ordered_threshold(x, y));
                assert_eq!(
                    f64::from(o[(y * w + x) as usize]),
                    want,
                    "ordered {exact} pixel ({x}, {y})"
                );
            }
        }
        dither_mode(&mut p, "noise");
        for frame in [0u32, 1] {
            let c = codes_of(&encode(
                &mut gpu,
                &flat,
                &p,
                Transfer::Identity,
                TargetFormat::Bgra8Unorm,
                frame,
            ));
            assert!(
                (mean(&c) - exact).abs() < noise_tol,
                "noise {exact} frame {frame}: mean {} (tolerance {noise_tol})",
                mean(&c)
            );
            assert!(
                c.iter().all(|k| (f64::from(*k) - exact).abs() < 1.0),
                "noise {exact} frame {frame}: error above 1"
            );
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Dark-tone banding check (CAP-9, replaces SP-4's banding test): an 8-bit dark ramp, source codes 0 to 31
/// in bands of 8 columns, through the tube gamma 2.4 and an exposure of 0.5 (mask loss), shown by a pure
/// 2.2 host. Reports, with and without dither, the error of each band against the exact (unquantised) host
/// code; with dither the band mean follows the exact value and no pixel is more than 1 LSB away.
#[test]
fn dark_ramp_banding_with_and_without_dither() {
    let Some(mut gpu) = gpu("dark_ramp_banding_with_and_without_dither") else {
        return;
    };
    let counter = gpu.validation_counter();
    let (w, h) = (256u32, 32u32);
    let exposure = 0.5;
    let linear = |k: u32| gun_power(f64::from(k) / 255.0, 2.4) * exposure;
    let exact = |k: u32| encode_power(linear(k), 2.2) * 255.0;
    let tube = image(w, h, |x, _| {
        let l = (gun_power(f64::from(x / 8) / 255.0, 2.4)) as f32;
        [l, l, l]
    });
    let mut base = Params::default();
    set(&mut base, "exposure", ParamValue::F32(exposure as f32));

    let mut report = |name: &str, p: &Params| -> (f64, f64, f64) {
        let r = encode(&mut gpu, &tube, p, Transfer::Power(2.2), TargetFormat::Bgra8Unorm, 0);
        let codes = codes_of(&r);
        let (mut worst_mean, mut worst_pixel, mut worst_step) = (0.0f64, 0.0f64, 0.0f64);
        let mut last_mean = 0.0;
        println!("{name}: band, exact code, mean code");
        for k in 0..32u32 {
            let mut sum = 0.0;
            for y in 0..h {
                for x in 8 * k..8 * k + 8 {
                    let c = f64::from(codes[(y * w + x) as usize]);
                    sum += c;
                    worst_pixel = worst_pixel.max((c - exact(k)).abs());
                }
            }
            let mean = sum / f64::from(8 * h);
            println!("  {k:2}  {:7.3}  {mean:7.3}", exact(k));
            worst_mean = worst_mean.max((mean - exact(k)).abs());
            if k > 0 {
                worst_step = worst_step.max((mean - last_mean).abs());
            }
            last_mean = mean;
        }
        println!("{name}: worst band mean error {worst_mean:.3}, worst pixel error {worst_pixel:.3}, largest step between bands {worst_step:.3}");
        (worst_mean, worst_pixel, worst_step)
    };
    let (m_off, p_off, _) = report("no dither", &base);
    let mut ordered = base;
    dither_mode(&mut ordered, "ordered");
    let (m_ord, p_ord, s_ord) = report("ordered", &ordered);
    let mut noise = base;
    dither_mode(&mut noise, "noise");
    let (m_noise, p_noise, s_noise) = report("noise", &noise);

    // Without dither the error is the rounding error: half a code, plus the tolerance of the hardware
    // quantiser (the RTX A2000 turns k + 0.55 into k for k < 3, a 0.05 code fuzz in the ROP conversion).
    assert!(p_off <= 0.6 && m_off <= 0.6, "undithered errors {p_off} {m_off}");
    // With dither: no pixel more than 1 LSB from the exact value, and the band means follow it.
    assert!(
        p_ord < 1.0 + 1e-3 && p_noise < 1.0 + 1e-3,
        "pixel errors {p_ord} {p_noise}"
    );
    assert!(m_ord < 0.02, "ordered band mean error {m_ord}");
    assert!(m_noise < 0.1, "noise band mean error {m_noise}");
    assert!(m_ord < m_off && m_noise < m_off);
    // No step between neighbouring bands above 1 LSB (the exact ramp rises by less than 1 code per band
    // in this range, and the dithered means follow it).
    assert!(s_ord <= 1.0 + 0.05 && s_noise <= 1.0 + 0.15, "steps {s_ord} {s_noise}");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

//! Optics stage (T-45): `shaders/optics*.comp`, `src/gpu/stages/optics.rs`, `params/optics.toml`, against
//! the f64 oracle `crt_model::optics` (model and derivation in its module doc; CRT.md rule 7).
//!
//! # What the oracle computes (independent of the shader)
//!
//! - Diffusion: separable, per axis the tent-weighted Gaussian integral (Simpson), out to 6 sigma + 2 px.
//! - Halo: the pixel kernel (density integrated over every host pixel, 4 x 4 or 12 x 12 midpoints), full
//!   resolution, convolved by FFT with zero padding. The GPU instead evaluates cell masses on one pyramid level
//!   (box means) and interpolates bilinearly: the difference is the method's error, measured here.
//! - Glare: the exact image mean of the diffused light (the GPU uses the undiffused mean through an FP16 box
//!   pyramid).
//! - Vignette v^(rho^2), tint and ambient E (T tint)^2 r_s / (pi L_w) in closed form.
//!
//! # Tolerances
//!
//! The output is FP16 (round to nearest, `round_f16`): half a step relative, 2^-11. The method's error budget
//! (QUESTIONS PHY-10, the T-48 standard-quality contract): mean absolute error <= 0.5 % of white, max <= 4 %
//! of white, on frames that are not impulses. Energy: the halo holds the configured weight +/- 0.1 point
//! absolute, the diffusion conserves the light +/- 0.5 %.

mod common;

use crt_model::optics::{Glass, Halo, Optics, TubeImage};
use ultimatecrt::gpu::{FrameConstants, Headless, ImageF32, Settings, SourceFrame, StageInput, StageRun, TargetFormat};
use ultimatecrt::params::{Params, OPTICS_HALO_METHOD_GAUSSIANS, OPTICS_HALO_METHOD_PYRAMID};
use ultimatecrt::scaling::View;

fn frame(w: u32, h: u32, ppm: f32) -> FrameConstants {
    let mut fc = FrameConstants::new((w, h), (w, h), false);
    assert_eq!(fc.tube_size, [w as i32, h as i32]);
    fc.px_per_mm = ppm;
    fc.line_pitch_mm = 1.0;
    fc
}

fn image(w: u32, h: u32, px: impl Fn(u32, u32) -> [f32; 3]) -> ImageF32 {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = px(x, y);
            rgba.extend_from_slice(&[r, g, b, 1.0]);
        }
    }
    ImageF32 {
        width: w,
        height: h,
        rgba,
    }
}

/// A game-like frame: a sky gradient, 8 x 8 px tiles of pseudo-random colours over the lower part, a white
/// HUD bar, a few small highlights above white (mask peaks), black borders.
pub fn game_frame(w: u32, h: u32) -> ImageF32 {
    let hash = |x: u32, y: u32, k: u32| {
        let mut s = x.wrapping_mul(73_856_093) ^ y.wrapping_mul(19_349_663) ^ k.wrapping_mul(83_492_791);
        s ^= s >> 13;
        s = s.wrapping_mul(0x5bd1_e995);
        s ^= s >> 15;
        (s & 0xFFFF) as f32 / 65535.0
    };
    image(w, h, |x, y| {
        if x < w / 40 || x >= w - w / 40 {
            return [0.0; 3];
        }
        if y < h / 12 && x > w / 4 && x < 3 * w / 4 {
            return [1.0; 3];
        }
        if y < h / 2 {
            let t = y as f32 / (h / 2) as f32;
            return [0.1 + 0.2 * t, 0.2 + 0.3 * t, 0.6 + 0.3 * t];
        }
        let (tx, ty) = (x / 8, y / 8);
        if hash(tx, ty, 7) > 0.97 {
            return [2.5, 2.2, 1.8];
        }
        let v = hash(tx, ty, 1).powf(2.2);
        [v * hash(tx, ty, 2), v * hash(tx, ty, 3), v * hash(tx, ty, 4)]
    })
}

/// Every effect on, at the given halo weight.
fn all_on(halo: f32) -> Params {
    Params {
        optics_enable: true,
        optics_halation_weight: halo,
        optics_glare_weight: 0.01,
        optics_diffusion_weight: 0.2,
        optics_diffusion_mm: 0.4,
        optics_vignette_corner: 0.8,
        optics_tint_rgb: [1.0, 0.97, 0.94],
        optics_ambient_lux: 20.0,
        ..Params::default()
    }
}

/// Only the halo.
fn halo_only(weight: f32, transmission: f32) -> Params {
    Params {
        optics_enable: true,
        optics_halation_weight: weight,
        optics_glass_transmission: transmission,
        optics_glare_weight: 0.0,
        optics_diffusion_weight: 0.0,
        optics_vignette_corner: 1.0,
        ..Params::default()
    }
}

/// The oracle model of the parameters.
pub fn model(p: &Params) -> Optics {
    Optics {
        glass: Glass {
            thickness_mm: f64::from(p.optics_glass_thickness_mm),
            index: f64::from(p.optics_glass_index),
            transmission: f64::from(p.optics_glass_transmission),
        },
        halation_weight: f64::from(p.optics_halation_weight),
        diffusion_weight: f64::from(p.optics_diffusion_weight),
        diffusion_sigma_mm: f64::from(p.optics_diffusion_mm),
        glare_weight: f64::from(p.optics_glare_weight),
        vignette_corner: f64::from(p.optics_vignette_corner),
        tint: p.optics_tint_rgb.map(f64::from),
        ambient_lux: f64::from(p.optics_ambient_lux),
        screen_reflectance: f64::from(p.optics_screen_reflectance),
        white_nits: f64::from(p.optics_white_nits),
    }
}

pub fn to_tube(img: &ImageF32) -> TubeImage {
    let mut t = TubeImage::new(img.width as usize, img.height as usize);
    for (i, p) in t.rgb.iter_mut().enumerate() {
        for (c, v) in p.iter_mut().enumerate() {
            *v = f64::from(img.rgba[4 * i + c]);
        }
    }
    t
}

/// The oracle output for `input` (no raster margin).
pub fn oracle(p: &Params, input: &ImageF32, ppm: f32) -> TubeImage {
    model(p).apply(&to_tube(input), f64::from(ppm), (0, 0))
}

/// One frame of `optics` alone on `input` (injected after `phosphor`); its output image.
pub fn run(gpu: &mut Headless, params: &Params, input: &ImageF32, ppm: f32) -> ImageF32 {
    gpu.enable_only(&["optics"]).unwrap();
    let settings = Settings::identity();
    let r = gpu
        .render_stages(&StageRun {
            input: StageInput::Image {
                after: "phosphor",
                image: input,
            },
            frame: frame(input.width, input.height, ppm),
            settings: &settings,
            params,
            format: TargetFormat::Rgba32F,
        })
        .expect("render_stages");
    r.stage("optics").expect("optics output").clone()
}

/// Mean and max absolute difference over the rgb channels.
pub fn errors(gpu: &ImageF32, want: &TubeImage) -> (f64, f64) {
    let (mut sum, mut max) = (0.0, 0.0_f64);
    for (i, w) in want.rgb.iter().enumerate() {
        for (c, wc) in w.iter().enumerate() {
            let e = (f64::from(gpu.rgba[4 * i + c]) - wc).abs();
            sum += e;
            max = max.max(e);
        }
    }
    (sum / (3 * want.rgb.len()) as f64, max)
}

fn total(img: &ImageF32, c: usize) -> f64 {
    img.rgba.chunks(4).map(|p| f64::from(p[c])).sum()
}

/// Off (the default), neutral, or an unplaced tube: the slot is skipped and the output is the input bit
/// for bit; on again it runs.
#[test]
fn off_is_bit_exact_identity() {
    let Some(mut gpu) = common::gpu_or_skip("off_is_bit_exact_identity") else {
        return;
    };
    let counter = gpu.validation_counter();
    assert!(!Params::default().optics_enable, "optics off by default");
    let pic = game_frame(96, 64);
    let off = Params::default();
    assert_eq!(run(&mut gpu, &off, &pic, 2.0), pic, "off");
    assert!(gpu.last_skipped_slots().contains(&"optics"));
    let neutral = Params {
        optics_enable: true,
        optics_halation_weight: 0.0,
        optics_glare_weight: 0.0,
        optics_diffusion_weight: 0.0,
        optics_vignette_corner: 1.0,
        ..off
    };
    assert_eq!(run(&mut gpu, &neutral, &pic, 2.0), pic, "neutral");
    assert!(gpu.last_skipped_slots().contains(&"optics"));
    assert_eq!(run(&mut gpu, &all_on(0.03), &pic, 0.0), pic, "unplaced");
    let on = run(&mut gpu, &all_on(0.03), &pic, 2.0);
    assert!(!gpu.last_skipped_slots().contains(&"optics"));
    assert_ne!(on, pic, "on");
    assert_eq!(run(&mut gpu, &off, &pic, 2.0), pic, "off after on");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Azimuthal mean of channel 0 around (cx, cy) in 1 px bins, out to `rmax`.
fn radial(get: impl Fn(usize, usize) -> f64, w: usize, h: usize, cx: usize, cy: usize, rmax: usize) -> Vec<f64> {
    let mut s = vec![0.0; rmax + 1];
    let mut n = vec![0usize; rmax + 1];
    for y in 0..h {
        for x in 0..w {
            let r = ((x as f64 - cx as f64).powi(2) + (y as f64 - cy as f64).powi(2))
                .sqrt()
                .round() as usize;
            if r <= rmax {
                s[r] += get(x, y);
                n[r] += 1;
            }
        }
    }
    s.iter().zip(&n).map(|(a, &k)| a / k.max(1) as f64).collect()
}

/// A point source (the impulse response): the halo holds the configured weight (+/- 0.1 point), the ring's
/// inner edge is where the physics puts it (t = 12 mm, n 1.52: 1.747 t = 20.97 mm = 52.4 px at 2.5 px/mm,
/// within 1 px in the oracle; the GPU's kernel level smears it over its cell size, recorded), and the radial
/// profile follows the oracle (error relative to the ring peak recorded and bounded).
#[test]
fn impulse_ring_and_energy() {
    let Some(mut gpu) = common::gpu_or_skip("impulse_ring_and_energy") else {
        return;
    };
    let counter = gpu.validation_counter();
    let (w, h, ppm) = (640u32, 640u32, 2.5f32);
    let (cx, cy) = (320usize, 320usize);
    let v = 1000.0f32;
    let pic = image(w, h, |x, y| {
        if (x as usize, y as usize) == (cx, cy) {
            [v; 3]
        } else {
            [0.0; 3]
        }
    });
    let weight = 0.03;
    let p = halo_only(weight, 0.5);
    let halo = Halo::new(model(&p).glass);
    let support_px = halo.support_mm * f64::from(ppm);
    assert!(support_px < 310.0, "the halo fits: {support_px}");
    let want = oracle(&p, &pic, ppm);
    let edge = halo.ring_radius_mm() * f64::from(ppm);
    let rmax = support_px as usize;
    let ro = radial(
        |x, y| want.rgb[y * w as usize + x][0],
        w as usize,
        h as usize,
        cx,
        cy,
        rmax,
    );
    let peak_o = ro[3..].iter().cloned().fold(0.0, f64::max);
    // Oracle: the steepest rise of the profile is at the edge.
    let rise = |prof: &[f64]| {
        (3..prof.len() - 1)
            .max_by(|&a, &b| (prof[a + 1] - prof[a]).total_cmp(&(prof[b + 1] - prof[b])))
            .unwrap()
    };
    let half = |prof: &[f64], peak: f64| (3..prof.len()).find(|&r| prof[r] >= 0.5 * peak).unwrap();
    let edge_o = rise(&ro);
    println!(
        "oracle: edge {edge:.2} px, steepest rise between {edge_o} and {} px, half-peak at {} px",
        edge_o + 1,
        half(&ro, peak_o)
    );
    assert!((edge_o as f64 + 0.5 - edge).abs() <= 1.0);
    let mut kernel = None;
    for (name, method, taps) in [
        ("kernel", 0, 24),
        ("kernel 12 taps", 0, 12),
        ("kernel 32 taps", 0, 32),
        ("kernel 48 taps", 0, 48),
        ("pyramid", OPTICS_HALO_METHOD_PYRAMID, 24),
        ("gaussians", OPTICS_HALO_METHOD_GAUSSIANS, 24),
    ] {
        let pm = Params {
            optics_halo_method: method,
            optics_halo_taps: taps,
            ..p
        };
        let out = run(&mut gpu, &pm, &pic, ppm);
        let direct = f64::from(v) * (1.0 - f64::from(weight));
        let halo_energy = (total(&out, 0) - direct) / f64::from(v);
        let rg = radial(
            |x, y| f64::from(out.rgba[4 * (y * w as usize + x)]),
            w as usize,
            h as usize,
            cx,
            cy,
            rmax,
        );
        let peak_g = rg[3..].iter().cloned().fold(0.0, f64::max);
        let err = (3..=rmax).map(|r| (rg[r] - ro[r]).abs()).fold(0.0, f64::max) / peak_o;
        let mean_err = (3..=rmax).map(|r| (rg[r] - ro[r]).abs()).sum::<f64>() / (rmax - 2) as f64 / peak_o;
        let r_peak_o = (3..=rmax).max_by(|&a, &b| ro[a].total_cmp(&ro[b])).unwrap();
        let r_peak_g = (3..=rmax).max_by(|&a, &b| rg[a].total_cmp(&rg[b])).unwrap();
        println!(
            "{name}: halo energy {:.4} (configured {weight}), ring peak at {r_peak_g} px (oracle {r_peak_o}), \
             half-peak at {} px (oracle {}), peak {:.3e} (oracle {:.3e}), profile error max {:.1} % mean {:.2} % of \
             the oracle ring peak",
            halo_energy,
            half(&rg, peak_g),
            half(&ro, peak_o),
            peak_g,
            peak_o,
            100.0 * err,
            100.0 * mean_err
        );
        if name.starts_with("kernel") || name == "gaussians" {
            assert!(
                (halo_energy - f64::from(weight)).abs() <= 0.001,
                "{name}: halo energy {halo_energy}"
            );
        }
        if name == "kernel" {
            kernel = Some((err, mean_err, half(&rg, peak_g)));
        }
    }
    // The level's cells (16 px here) smear the sharp inner edge of a single-pixel source: the largest error
    // sits on the edge; on average the profile follows the oracle within a few percent of the ring peak.
    let (err, mean_err, half_g) = kernel.unwrap();
    assert!(
        err < 0.45 && mean_err < 0.03,
        "kernel: profile error {err}, mean {mean_err}"
    );
    assert!((half_g as f64 - edge).abs() <= 8.0, "kernel: edge {half_g}");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Review R-49: halation 0.8 with glare 0.8 (and a diffusion weight of 1.5) can no longer give negative light
/// or create light. The weights are normalised to 0.5 / 0.5 (`crt_model::optics::light_weights`), so the
/// direct image weighs 0, not -0.6. An impulse far from the edges, a neutral glass, the `kernel` halo (the
/// default, a non-negative kernel like the oracle's): no channel is negative, and the total light equals the
/// input and the oracle's total within 0.5 % (the halo energy budget, FP16 included). The signed `pyramid`
/// and `gaussians` methods are printed only: their undershoot far from the light is their method error
/// (not a weight effect; the direct weight is 0 everywhere here). Negative weights act as 0 (the stage
/// output is then the input, within half an FP16 step).
#[test]
fn extreme_weights_give_no_negative_light() {
    let Some(mut gpu) = common::gpu_or_skip("extreme_weights_give_no_negative_light") else {
        return;
    };
    let counter = gpu.validation_counter();
    let (w, h, ppm) = (640u32, 640u32, 2.5f32);
    let v = 1000.0f32;
    let pic = image(w, h, |x, y| {
        if (x, y) == (320, 320) {
            [v, 0.5 * v, 2.0 * v]
        } else {
            [0.0; 3]
        }
    });
    let p = Params {
        optics_halation_weight: 0.8,
        optics_glare_weight: 0.8,
        optics_diffusion_weight: 1.5,
        optics_diffusion_mm: 0.4,
        ..halo_only(0.8, 0.5)
    };
    let want = oracle(&p, &pic, ppm);
    for (name, method) in [
        ("kernel", 0),
        ("pyramid", OPTICS_HALO_METHOD_PYRAMID),
        ("gaussians", OPTICS_HALO_METHOD_GAUSSIANS),
    ] {
        let pm = Params {
            optics_halo_method: method,
            ..p
        };
        let out = run(&mut gpu, &pm, &pic, ppm);
        assert!(out.rgba.iter().all(|x| x.is_finite()), "{name}: non-finite values");
        let min = out.rgba.iter().fold(f32::INFINITY, |m, &x| m.min(x));
        for c in 0..3 {
            let tin = total(&pic, c);
            let tout = total(&out, c);
            let tor: f64 = want.rgb.iter().map(|q| q[c]).sum();
            println!("{name} channel {c}: total out {tout:.3}, in {tin:.3}, oracle {tor:.3}; minimum value {min:.3e}");
            assert!((tor / tin - 1.0).abs() < 1e-3, "oracle {c}: {tor} vs {tin}");
            assert!(want.rgb.iter().all(|q| q[c] >= 0.0), "oracle {c}: negative light");
            if method == 0 {
                assert!(min >= 0.0, "{name}: negative light {min}");
                assert!((tout / tin - 1.0).abs() < 5e-3, "{name} channel {c}: {tout} vs {tin}");
            }
        }
    }
    // Negative weights act as 0: the light passes unchanged (vignette 1, tint 1, no ambient).
    let neg = Params {
        optics_halation_weight: -0.5,
        optics_glare_weight: -0.5,
        optics_diffusion_weight: -1.0,
        ..p
    };
    let small = game_frame(96, 64);
    let out = run(&mut gpu, &neg, &small, ppm);
    for (i, (&got, &x)) in out.rgba.iter().zip(&small.rgba).enumerate() {
        assert!(
            (f64::from(got) - f64::from(x)).abs() <= f64::from(x).abs() / 2048.0 + 1e-7,
            "negative weights, texel {} channel {}: {got} vs {x}",
            i / 4,
            i % 4
        );
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// The edge of a white half plane, a flat white field and a game-like frame with every effect on: the GPU
/// (kernel method, default taps) against the oracle within the error budget, and the flat field keeps its
/// level in the middle (redistribution only).
#[test]
fn frames_match_the_oracle() {
    let Some(mut gpu) = common::gpu_or_skip("frames_match_the_oracle") else {
        return;
    };
    let counter = gpu.validation_counter();
    let (w, h, ppm) = (480u32, 360u32, 1.24f32);
    let edge = image(w, h, |x, _| if x >= w / 2 { [1.0; 3] } else { [0.0; 3] });
    let flat = image(w, h, |_, _| [1.0; 3]);
    let game = game_frame(w, h);
    for (name, pic) in [("edge", &edge), ("flat white", &flat), ("game", &game)] {
        for (pname, p) in [("halo 5 %", halo_only(0.05, 0.85)), ("all on", all_on(0.03))] {
            let want = oracle(&p, pic, ppm);
            let got = run(&mut gpu, &p, pic, ppm);
            let (mae, max) = errors(&got, &want);
            println!(
                "{name}, {pname}: MAE {:.4} %, max {:.3} % of white",
                100.0 * mae,
                100.0 * max
            );
            assert!(mae <= 0.005 && max <= 0.04, "{name}, {pname}: {mae} {max}");
        }
    }
    // Flat white, halo only: the centre keeps the white minus the light the halo carries past the edges.
    let p = halo_only(0.05, 0.85);
    let got = run(&mut gpu, &p, &flat, ppm);
    let want = oracle(&p, &flat, ppm);
    let i = (h / 2 * w + w / 2) as usize;
    println!(
        "flat white centre: GPU {:.5}, oracle {:.5}",
        got.rgba[4 * i],
        want.rgb[i][0]
    );
    assert!((f64::from(got.rgba[4 * i]) - want.rgb[i][0]).abs() < 0.002);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Diffusion alone conserves a point's light (+/- 0.5 %) and matches the oracle; vignette alone gives the
/// configured corner / centre ratio (+/- 1 %); glare alone puts w_g x APL into a dark patch, linear in the APL
/// (+/- 2 %); tint and ambient are the closed forms.
#[test]
fn diffusion_vignette_glare_tint_ambient() {
    let Some(mut gpu) = common::gpu_or_skip("diffusion_vignette_glare_tint_ambient") else {
        return;
    };
    let counter = gpu.validation_counter();
    let none = Params {
        optics_enable: true,
        optics_halation_weight: 0.0,
        optics_glare_weight: 0.0,
        optics_diffusion_weight: 0.0,
        optics_vignette_corner: 1.0,
        ..Params::default()
    };
    // Diffusion: sigma 0.5 mm at 4 px/mm = 2 px (radius 9 px).
    let (w, h, ppm) = (64u32, 48u32, 4.0f32);
    let dot = image(w, h, |x, y| {
        if (x, y) == (31, 23) {
            [100.0, 50.0, 25.0]
        } else {
            [0.0; 3]
        }
    });
    let pd = Params {
        optics_diffusion_weight: 0.5,
        optics_diffusion_mm: 0.5,
        ..none
    };
    let out = run(&mut gpu, &pd, &dot, ppm);
    for c in 0..3 {
        let ratio = total(&out, c) / total(&dot, c);
        println!("diffusion channel {c}: energy ratio {ratio:.5}");
        assert!((ratio - 1.0).abs() <= 0.005);
    }
    let (mae, max) = errors(&out, &oracle(&pd, &dot, ppm));
    println!("diffusion vs oracle: MAE {mae:.2e}, max {max:.2e} (point of 100)");
    assert!(max < 0.05, "{max}");
    // Vignette 0.8: corner pixel / centre pixel = 0.8^(rho^2) for the corner pixel centre.
    let flat = image(w, h, |_, _| [1.0; 3]);
    let pv = Params {
        optics_vignette_corner: 0.8,
        ..none
    };
    let out = run(&mut gpu, &pv, &flat, ppm);
    let centre = out.pixel(w / 2, h / 2)[1];
    let corner = out.pixel(0, 0)[1];
    let hd2 = (f64::from(w * w + h * h)) / 4.0;
    let rho2 = ((f64::from(w) / 2.0 - 0.5).powi(2) + (f64::from(h) / 2.0 - 0.5).powi(2)) / hd2;
    let want = 0.8f64.powf(rho2);
    println!(
        "vignette: corner / centre {:.4}, want {want:.4} (corner of the raster: 0.8)",
        corner / centre
    );
    assert!((f64::from(corner / centre) / want - 1.0).abs() <= 0.01);
    // Glare: dark 1-px patch in a surround of level s.
    let pg = Params {
        optics_glare_weight: 0.02,
        ..none
    };
    for s in [0.25f32, 0.5, 1.0, 2.0] {
        let pic = image(w, h, |x, y| if (x, y) == (20, 20) { [0.0; 3] } else { [s; 3] });
        let out = run(&mut gpu, &pg, &pic, ppm);
        let apl = f64::from(s) * f64::from(w * h - 1) / f64::from(w * h);
        let got = f64::from(out.pixel(20, 20)[0]);
        println!("glare: surround {s}, patch {got:.6}, w_g APL {:.6}", 0.02 * apl);
        assert!((got / (0.02 * apl) - 1.0).abs() <= 0.02);
    }
    // Tint and ambient on black and on white.
    let pt = Params {
        optics_tint_rgb: [1.0, 0.9, 0.8],
        optics_ambient_lux: 100.0,
        ..none
    };
    let out = run(&mut gpu, &pt, &flat, ppm);
    let want = oracle(&pt, &flat, ppm);
    let (mae, max) = errors(&out, &want);
    println!(
        "tint + ambient: MAE {mae:.2e}, max {max:.2e}; green {:.5}",
        out.pixel(3, 3)[1]
    );
    assert!(max <= 1.0 / 1024.0);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// The f32 -> FP16 rounding to nearest (ties to even) of a normal value, as `round_f16` of `common.glsl`.
fn f16_round(x: f32) -> f32 {
    let b = x.to_bits();
    let r = (b + 0x0FFF + ((b >> 13) & 1)) & 0xFFFF_E000;
    f32::from_bits(r)
}

/// FP16 stores round to nearest: a tint of 0.9 on values whose product falls between two FP16 values gives
/// the nearest one (truncation would give the lower one for half of them).
#[test]
fn fp16_store_rounds_to_nearest() {
    let Some(mut gpu) = common::gpu_or_skip("fp16_store_rounds_to_nearest") else {
        return;
    };
    let counter = gpu.validation_counter();
    let (w, h) = (32u32, 8u32);
    // FP16-exact inputs 1 + k / 1024.
    let pic = image(w, h, |x, y| [1.0 + (x + w * y) as f32 / 1024.0; 3]);
    let p = Params {
        optics_enable: true,
        optics_halation_weight: 0.0,
        optics_glare_weight: 0.0,
        optics_diffusion_weight: 0.0,
        optics_vignette_corner: 1.0,
        optics_tint_rgb: [0.9, 0.9, 0.9],
        ..Params::default()
    };
    let out = run(&mut gpu, &p, &pic, 2.0);
    let mut differs = 0;
    for i in 0..(w * h) as usize {
        let x = pic.rgba[4 * i];
        // The shader computes v tint g + amb with v = exp2(rho2 log2 1) = 1, amb = 0.
        let want = f16_round(0.9f32 * x);
        assert_eq!(out.rgba[4 * i], want, "texel {i}: {x} x 0.9");
        let trunc = f32::from_bits((0.9f32 * x).to_bits() & 0xFFFF_E000);
        differs += usize::from(trunc != want);
    }
    println!("{differs} of {} texels differ from a truncating store", w * h);
    assert!(differs > 0);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Cost study of the halo methods (QUESTIONS PHY-10, T-44), headless, run by hand:
/// `cargo test -p ultimatecrt --test stage_optics optics_cost_study -- --ignored --nocapture`.
/// Tube images 1920 x 1080 and 1920 x 1200 with the PVM-20M4U tube (386.7 x 290 mm visible) across the image
/// width: 4.966 px/mm (glass 12 mm, n 1.52, T 0.85: ring edge at 104.1 px, halo support 619 px), every effect on
/// with a 5 % halo (the top of the CRT.md range, the worst case for the error) and a 0.4 mm (2 px) diffusion,
/// the game-like frame. Per configuration: GPU time of the slot in bursts of 40 back-to-back frames, each
/// burst paired with a burst of a calibration pass (the phosphor saturation pass, one full-resolution read and
/// write) to see the clock drift of the shared laptop GPU; error against the oracle on the frame (FFT, f64); and
/// the radial profile of a point source against the exact pixel kernel (error relative to the ring peak). The
/// table and the choice are in QUESTIONS PHY-10. `UCRT_OPTICS_TIMING_ONLY` skips the oracle (error columns then
/// meaningless).
#[test]
#[ignore]
fn optics_cost_study() {
    let Some(mut gpu) = common::gpu_or_skip("optics_cost_study") else {
        return;
    };
    let counter = gpu.validation_counter();
    let methods: [(&str, u32, i32); 6] = [
        ("kernel, 16 taps", 0, 16),
        ("kernel, 24 taps", 0, 24),
        ("kernel, 32 taps", 0, 32),
        ("kernel, 48 taps", 0, 48),
        ("pyramid", OPTICS_HALO_METHOD_PYRAMID, 24),
        ("gaussians (4, full res)", OPTICS_HALO_METHOD_GAUSSIANS, 24),
    ];
    for (w, h) in [(1920u32, 1080u32), (1920, 1200)] {
        // The PVM-20M4U tube (386.7 x 290 mm visible) across the image width: 4.966 px/mm.
        let ppm = w as f32 / (290.0 * 4.0 / 3.0);
        let base = all_on(0.05);
        let game = game_frame(w, h);
        // UCRT_OPTICS_TIMING_ONLY set: no oracle (the error columns then compare with the input frame).
        let timing_only = std::env::var_os("UCRT_OPTICS_TIMING_ONLY").is_some();
        let t0 = std::time::Instant::now();
        let want = if timing_only {
            to_tube(&game)
        } else {
            oracle(&base, &game, ppm)
        };
        let without_halo = Params {
            optics_halation_weight: 0.0,
            ..base
        };
        let want_nohalo = if timing_only {
            to_tube(&game)
        } else {
            oracle(&without_halo, &game, ppm)
        };
        println!("{w}x{h}: oracle {:.1} s", t0.elapsed().as_secs_f64());
        // Point source: the oracle is the pixel kernel itself.
        let (cx, cy) = (w as usize / 2, h as usize / 2);
        let v = 1000.0f32;
        let dot = image(w, h, |x, y| {
            if (x as usize, y as usize) == (cx, cy) {
                [v; 3]
            } else {
                [0.0; 3]
            }
        });
        let halo = Halo::new(model(&base).glass);
        let (rad, k) = crt_model::optics::halo_kernel_px(&halo, f64::from(ppm));
        let n = 2 * rad + 1;
        let rmax = rad.min(cy - 1);
        let ro = radial(
            |x, y| {
                let (dx, dy) = (x as i64 - cx as i64 + rad as i64, y as i64 - cy as i64 + rad as i64);
                if dx < 0 || dy < 0 || dx >= n as i64 || dy >= n as i64 {
                    0.0
                } else {
                    f64::from(v) * 0.05 * k[dy as usize * n + dx as usize]
                }
            },
            w as usize,
            h as usize,
            cx,
            cy,
            rmax,
        );
        let peak_o = ro[3..].iter().cloned().fold(0.0, f64::max);
        let dot_params = halo_only(0.05, 0.85);
        println!(
            "{w}x{h} at {ppm:.3} px/mm: ring edge {:.1} px, support {:.0} px",
            halo.ring_radius_mm() * f64::from(ppm),
            halo.support_mm * f64::from(ppm)
        );
        // Configurations: (name, slot timed, parameters, oracle). Timed in interleaved rounds so that every
        // configuration sees the same clock states; the phosphor saturation pass (one full-resolution read and
        // write, about 1 ms live at 1920 x 1440, ARCHITECTURE 3.7 T-47) calibrates the harness.
        let mut configs: Vec<(String, &str, Params, Option<&TubeImage>, bool)> = Vec::new();
        for (name, method, taps) in methods {
            let p = Params {
                optics_halo_method: method,
                optics_halo_taps: taps,
                ..base
            };
            configs.push((name.to_string(), "optics", p, Some(&want), true));
        }
        configs.push((
            "no halo (diffusion, glare, vignette, tint, ambient)".into(),
            "optics",
            without_halo,
            Some(&want_nohalo),
            false,
        ));
        let vignette_only = Params {
            optics_halation_weight: 0.0,
            optics_glare_weight: 0.0,
            optics_diffusion_weight: 0.0,
            ..base
        };
        configs.push((
            "composite alone (vignette, tint, ambient)".into(),
            "optics",
            vignette_only,
            None,
            false,
        ));
        let diffusion_only = Params {
            optics_diffusion_weight: base.optics_diffusion_weight,
            ..vignette_only
        };
        configs.push((
            "composite with diffusion (no pyramid)".into(),
            "optics",
            diffusion_only,
            None,
            false,
        ));
        let glare_only = Params {
            optics_glare_weight: base.optics_glare_weight,
            ..vignette_only
        };
        configs.push((
            "pyramid and composite (glare, no diffusion)".into(),
            "optics",
            glare_only,
            None,
            false,
        ));
        let saturation = Params {
            phosphor_saturation: true,
            ..Params::default()
        };
        configs.push((
            "calibration: phosphor saturation pass".into(),
            "phosphor",
            saturation,
            None,
            false,
        ));
        // Accuracy: one injected frame per configuration. Timing: bursts of back-to-back frames of the full
        // pipeline from a captured-like source (only the configuration's slot enabled; the domain changes are
        // the resample copies), so the GPU stays busy and its clocks up (single frames with readbacks leave it
        // idle between frames and measured 2 to 10 times the burst time); 4 bursts of 40 frames after a
        // warm-up burst, the mean of each burst (`take_timing_average`); reported: median and minimum of the
        // 4 means. The source fills the 4:3 tube's width, so the burst runs at the same px per mm.
        let bgra: Vec<u8> = game
            .rgba
            .chunks(4)
            .flat_map(|p| {
                let code = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                [code(p[2]), code(p[1]), code(p[0]), 255]
            })
            .collect();
        let mut ms = vec![Vec::new(); configs.len()];
        let mut ratio: Vec<Vec<f64>> = vec![Vec::new(); configs.len()];
        let mut calib: Vec<f64> = Vec::new();
        let mut outs: Vec<Option<ImageF32>> = vec![None; configs.len()];
        for (i, (_, slot, p, reference, _)) in configs.iter().enumerate() {
            gpu.enable_only(&[slot]).unwrap();
            let settings = Settings::identity();
            if reference.is_some() {
                let r = gpu
                    .render_stages(&StageRun {
                        input: StageInput::Image {
                            after: "phosphor",
                            image: &game,
                        },
                        frame: frame(w, h, ppm),
                        settings: &settings,
                        params: p,
                        format: TargetFormat::Rgba32F,
                    })
                    .expect("render_stages");
                outs[i] = r.stage(slot).cloned();
            }
            let frames: Vec<(Option<SourceFrame<'_>>, View)> = (0..40)
                .map(|_| {
                    (
                        Some(SourceFrame {
                            width: w,
                            height: h,
                            bgra: &bgra,
                        }),
                        View::full(w, h),
                    )
                })
                .collect();
            // Each burst of the configuration follows a burst of the calibration pass; the ratio of the two
            // means cancels most of the clock drift of a shared laptop GPU.
            for burst in 0..5 {
                let mut burst_mean = |slot: &str, p: &Params| {
                    gpu.enable_only(&[slot]).unwrap();
                    let _ = gpu.take_timing_average();
                    gpu.render_burst(&frames, &settings, p, (w, h), TargetFormat::Bgra8Unorm)
                        .expect("burst");
                    gpu.take_timing_average().and_then(|t| t.stage_ms(slot))
                };
                let c = burst_mean("phosphor", &saturation);
                let m = burst_mean(slot, p);
                if burst > 0 {
                    if let (Some(c), Some(m)) = (c, m) {
                        ms[i].push(m);
                        ratio[i].push(m / c);
                        calib.push(c);
                    }
                }
            }
        }
        calib.sort_by(f64::total_cmp);
        let best = calib[0];
        println!(
            "calibration pass (one full-resolution read and write): best {best:.3} ms, median {:.3} ms over {} bursts",
            calib[calib.len() / 2],
            calib.len()
        );
        println!("| configuration | median ms | min ms | ratio to the calibration pass (median) | at the best calibration, ms | MAE % white | max % white | ring profile max / mean % of peak |");
        for (i, (name, _, p, reference, ring)) in configs.iter().enumerate() {
            ms[i].sort_by(f64::total_cmp);
            let errs = match (reference, &outs[i]) {
                (Some(want), Some(out)) => {
                    let (mae, max) = errors(out, want);
                    format!("{:.4} | {:.3}", 100.0 * mae, 100.0 * max)
                }
                _ => String::from("- | -"),
            };
            let ring = if *ring {
                let pd = Params {
                    optics_halo_method: p.optics_halo_method,
                    optics_halo_taps: p.optics_halo_taps,
                    ..dot_params
                };
                let o = run(&mut gpu, &pd, &dot, ppm);
                let rg = radial(
                    |x, y| {
                        let val = f64::from(o.rgba[4 * (y * w as usize + x)]);
                        if (x, y) == (cx, cy) {
                            val - f64::from(v) * 0.95
                        } else {
                            val
                        }
                    },
                    w as usize,
                    h as usize,
                    cx,
                    cy,
                    rmax,
                );
                let emax = (3..=rmax).map(|r| (rg[r] - ro[r]).abs()).fold(0.0, f64::max) / peak_o;
                let emean = (3..=rmax).map(|r| (rg[r] - ro[r]).abs()).sum::<f64>() / (rmax - 2) as f64 / peak_o;
                let half = (3..=rmax).find(|&r| rg[r] >= 0.5 * rg[3..].iter().cloned().fold(0.0, f64::max));
                format!("{:.1} / {:.2} (half-peak {half:?} px)", 100.0 * emax, 100.0 * emean)
            } else {
                String::from("-")
            };
            ratio[i].sort_by(f64::total_cmp);
            let rm = ratio[i][ratio[i].len() / 2];
            println!(
                "| {name} | {:.3} | {:.3} | {rm:.2} | {:.3} | {errs} | {ring} |",
                ms[i][ms[i].len() / 2],
                ms[i][0],
                rm * best
            );
        }
        let half_o = (3..=rmax).find(|&r| ro[r] >= 0.5 * peak_o);
        println!("oracle ring half-peak at {half_o:?} px");
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

//! Ageing and burn-in (T-42, T-43): the `wear` slot (`shaders/wear.comp`, `shaders/wear_accumulate.comp`,
//! `src/gpu/stages/wear.rs`), the cathode wear in `gun` (`shaders/gun.comp`), the spot growth of a worn gun
//! (`src/ageing.rs`), and the persistence of the wear map (`src/tube_state.rs`), against the f64 oracles
//! `crt_model::ageing` and `crt_model::burnin`.
//!
//! One GPU context for the whole program (one test function, sections in order), small frames (128 x 96 tube
//! images), so it stays within a few seconds.
//!
//! # Derivations
//!
//! - Wear law (crt-model `ageing`): efficiency 1 / (1 + c_ch Q), c = rates x k0 with k0 solved so that the
//!   tube white (SMPTE-C primaries, D65, luminance shares s(D65) = 0.2124, 0.7011, 0.0866) keeps 0.85 after
//!   20,000 h_ref. A channel loses 15 % at Q = (1 / 0.85 - 1) / c_ch (`WearLaw::charge_for_loss`).
//! - Ageing: Q_age = hours x level x L_w / 350. With level 1 and L_w = 350 cd/m2, Q_age = hours.
//! - Burn-in: one frame adds scale x (texel area mean of the light), scale = (frame hours x acceleration +
//!   'burn in now' hours) x L_w / 350; with L_w = 350, scale = hours. The map is 640 x 480 texels over the
//!   visible area (here 386.67 x 290 mm at 0.3 px per mm: 116 x 87 px centred in the 128 x 96 image, corner
//!   (6, 4.5), 0.18125 px per texel, so the texels straddle the pixel edges).
//! - Spot growth 1.2: both FWHM scaled, so the measured vertical sigma of one scanline is 1.2 times larger
//!   (variance of the box-sampled profile minus 1/12 px^2 for the pixel box).
//!
//! # Tolerances
//!
//! The chain images are FP16 rounded to nearest: a value is within 2^-11 relative of the f32 result (plus
//! 6e-8 absolute in the subnormal range); the map is FP32 (Kahan sums), compared at 2e-6 relative.

mod common;

use std::time::Instant;

use crt_model::ageing::{Cathode, WearLaw, REF_HOURS};
use crt_model::burnin::{placement, Placement, WearMap as OracleMap};
use crt_model::gun::{white_weights, white_xy, Gun};
use phoxium::gpu::{
    Domain, FrameConstants, Headless, ImageF32, Settings, SourceFrame, StageInput, StageRun, TargetFormat,
    WEAR_MAP_SIZE,
};
use phoxium::params::{Params, BEAM_QUALITY_REFERENCE};
use phoxium::scaling::View;
use phoxium::tube_state::{self, TubeState, WearHost, WearMap};

const W: u32 = 128;
const H: u32 = 96;
const HALF_FP16: f64 = 1.0 / 2048.0;

fn fp16_close(got: f32, want: f64) -> bool {
    (f64::from(got) - want).abs() <= want.abs() * HALF_FP16 + 6e-8
}

/// Tube-domain light: (r, g, b) of `px(x, y)`, alpha 1.
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

/// A burn-in picture: black, a white bar at the bottom (a HUD), a red block, a colour patch (FP16-exact).
fn hud(x: u32, y: u32) -> [f32; 3] {
    if y >= 80 {
        [1.0; 3]
    } else if (10..30).contains(&x) && (10..30).contains(&y) {
        [1.0, 0.0, 0.0]
    } else if (60..90).contains(&x) && (20..50).contains(&y) {
        [0.5, 0.75, 0.25]
    } else {
        [0.0; 3]
    }
}

/// The tube constants: 128 x 96 image, placed at `ppm` px per mm (0: unplaced), `frame_ms` since the last frame.
fn tube(ppm: f32, frame_ms: f32) -> FrameConstants {
    let mut fc = FrameConstants::new((16, 12), (W, H), false);
    assert_eq!(fc.tube_size, [W as i32, H as i32]);
    fc.px_per_mm = ppm;
    fc.line_pitch_mm = if ppm > 0.0 { 8.0 / ppm } else { 0.0 };
    fc.frame_time_ms = frame_ms;
    fc
}

/// One frame of `wear` alone on `input` (injected after `screen`); its output image and whether it was skipped.
fn wear(gpu: &mut Headless, params: &Params, fc: FrameConstants, input: &ImageF32) -> (ImageF32, bool) {
    gpu.enable_only(&["wear"]).unwrap();
    let settings = Settings::identity();
    let r = gpu
        .render_stages(&StageRun {
            input: StageInput::Image {
                after: "screen",
                image: input,
            },
            frame: fc,
            settings: &settings,
            params,
            format: TargetFormat::Rgba32F,
        })
        .expect("render_stages");
    let skipped = gpu.last_skipped_slots().contains(&"wear");
    (r.stage("wear").expect("wear image").clone(), skipped)
}

fn burn_params() -> Params {
    Params {
        burnin_enable: true,
        burnin_acceleration: 0.0,
        ageing_white_cd_m2: 350.0,
        ..Params::default()
    }
}

/// The oracle wear law of `p`, from crt-model only (luminance shares of the tube primaries by
/// `gun::white_weights`, a different code path from `src/white.rs`).
fn oracle_law(p: &Params) -> (WearLaw, [f64; 3]) {
    let xy = |v: [f32; 2]| (f64::from(v[0]), f64::from(v[1]));
    let shares = white_weights(
        xy(p.tube_red_xy),
        xy(p.tube_green_xy),
        xy(p.tube_blue_xy),
        xy(p.tube_white_xy),
    );
    let law = WearLaw::calibrated(
        p.ageing_rates_rgb.map(f64::from),
        shares,
        f64::from(p.ageing_ref_loss),
        f64::from(p.ageing_ref_hours),
    );
    (law, shares)
}

fn oracle_place(p: &Params, ppm: f32) -> Placement {
    let vis_h = f64::from(p.tube_visible_height_mm);
    placement(
        [f64::from(W), f64::from(H)],
        f64::from(ppm),
        [vis_h * f64::from(p.tube_aspect), vis_h],
        [WEAR_MAP_SIZE.0 as usize, WEAR_MAP_SIZE.1 as usize],
    )
}

fn light_of(img: &ImageF32) -> Vec<[f64; 3]> {
    img.rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| [f64::from(c[0]), f64::from(c[1]), f64::from(c[2])])
        .collect()
}

fn to_oracle(m: &WearMap) -> OracleMap {
    OracleMap {
        width: m.width as usize,
        height: m.height as usize,
        q: m.q
            .as_chunks::<3>()
            .0
            .iter()
            .map(|c| [f64::from(c[0]), f64::from(c[1]), f64::from(c[2])])
            .collect(),
    }
}

/// Compares the GPU map with the oracle map: relative 2e-6 (FP32 sums of FP32 products) plus 1e-4 of the
/// frame's charge `scale` for the texel edges, which the GPU places in f32 (an edge near x = 100 px is off by up
/// to one f32 ulp, 8e-6 px, against texels of 0.18 px: 4e-5 of the texel per edge).
fn assert_map(what: &str, gpu: &WearMap, oracle: &OracleMap, scale: f64) {
    assert_eq!((gpu.width as usize, gpu.height as usize), (oracle.width, oracle.height));
    let mut worst = 0.0f64;
    for (i, (g, o)) in gpu.q.as_chunks::<3>().0.iter().zip(&oracle.q).enumerate() {
        for c in 0..3 {
            let d = (f64::from(g[c]) - o[c]).abs();
            assert!(
                d <= 2e-6 * o[c].abs() + 1e-4 * scale,
                "{what}: texel {i} channel {c}: gpu {} oracle {}",
                g[c],
                o[c]
            );
            worst = worst.max(d / scale);
        }
    }
    eprintln!("{what}: worst map difference {worst:.2e} of the charge");
}

#[test]
fn ageing_and_burnin_on_the_gpu() {
    let Some(mut gpu) = common::gpu_or_skip("ageing_and_burnin_on_the_gpu") else {
        return;
    };
    let counter = gpu.validation_counter();
    let t0 = Instant::now();
    identity_when_off(&mut gpu);
    eprintln!("identity {:?}", t0.elapsed());
    ageing_efficiency(&mut gpu);
    cathode_wear(&mut gpu);
    spot_growth(&mut gpu);
    eprintln!("ageing {:?}", t0.elapsed());
    burnin_accumulation_and_ghost(&mut gpu);
    calibration_point(&mut gpu);
    long_frame_gap_does_not_wear_by_hours(&mut gpu);
    real_time_increments_are_not_lost(&mut gpu);
    eprintln!("burn-in {:?}", t0.elapsed());
    frozen_map_and_persistence(&mut gpu);
    eprintln!("persistence {:?}", t0.elapsed());
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Off (the defaults, and ageing on with neutral values): the slot is skipped and the chain output is the
/// same bit for bit as with the slot disabled.
fn identity_when_off(gpu: &mut Headless) {
    let pic = image(W, H, hud);
    let (out, skipped) = wear(gpu, &Params::default(), tube(0.3, 16.0), &pic);
    assert!(skipped, "defaults: the wear slot is skipped");
    assert_eq!(out, pic, "defaults: exact");
    // The whole chain from a captured frame: defaults, ageing on with neutral values, and the slot off.
    let (sw, sh) = (32u32, 24u32);
    let bgra: Vec<u8> = (0..sw * sh)
        .flat_map(|i| {
            let k = (i * 7 % 256) as u8;
            [k, 255 - k, k / 2, 255]
        })
        .collect();
    let frame = SourceFrame {
        width: sw,
        height: sh,
        bgra: &bgra,
    };
    let settings = Settings::default();
    let render = |gpu: &mut Headless, p: &Params| {
        gpu.render(
            Some(frame),
            View::full(sw, sh),
            &settings,
            p,
            (128, 96),
            TargetFormat::Rgba16F,
        )
        .expect("render")
        .output
    };
    for n in gpu.stage_names() {
        gpu.set_stage_enabled(n, true).unwrap();
    }
    let base = render(gpu, &Params::default());
    let neutral = Params {
        ageing_enable: true,
        ..Params::default()
    };
    assert_eq!(render(gpu, &neutral), base, "ageing on, neutral values: bit exact");
    assert!(gpu.last_skipped_slots().contains(&"wear"));
    gpu.set_stage_enabled("wear", false).unwrap();
    assert_eq!(render(gpu, &Params::default()), base, "slot off: same");
}

/// 15 % blue loss -> blue x 0.85 within FP16, every pixel equals the oracle, and the white drifts warm (CIE x up).
fn ageing_efficiency(gpu: &mut Headless) {
    let mut p = Params {
        ageing_enable: true,
        ageing_mean_level: 1.0,
        ageing_white_cd_m2: 350.0,
        ..Params::default()
    };
    let (law, _) = oracle_law(&p);
    let q = law.charge_for_loss(2, 0.15);
    p.ageing_hours = q as f32;
    let q = f64::from(p.ageing_hours); // the hours as the GPU gets them
    let e = law.efficiency([q; 3]);
    assert!((e[2] - 0.85).abs() < 1e-6);
    let pic = image(W, H, |x, y| [1.0, [0.5, 1.0, 2.0][((x + y) % 3) as usize], 0.75]);
    let (out, skipped) = wear(gpu, &p, tube(0.3, 16.0), &pic);
    assert!(!skipped);
    for y in 0..H {
        for x in 0..W {
            let (i, o) = (pic.pixel(x, y), out.pixel(x, y));
            for c in 0..3 {
                let want = f64::from(i[c]) * e[c];
                assert!(fp16_close(o[c], want), "ageing ({x}, {y}) {c}: {} against {want}", o[c]);
            }
            assert!((f64::from(o[2] / i[2]) - 0.85).abs() < 0.005, "blue x 0.85 +/- 0.5 %");
        }
    }
    let xy = |v: [f32; 2]| (f64::from(v[0]), f64::from(v[1]));
    let (r, g, b, w) = (
        xy(p.tube_red_xy),
        xy(p.tube_green_xy),
        xy(p.tube_blue_xy),
        xy(p.tube_white_xy),
    );
    let new = white_xy([1.0; 3], r, g, b, w);
    let old = white_xy(e, r, g, b, w);
    assert!(old.0 > new.0, "white x increases with age: {} -> {}", new.0, old.0);
}

/// Cathode wear in `gun`: (1 - loss) x V^2.4 at V - drift, per gun, against `crt_model::ageing::Cathode`.
fn cathode_wear(gpu: &mut Headless) {
    let p = Params {
        ageing_enable: true,
        ageing_cathode_loss_rgb: [0.1, 0.2, 0.3],
        ageing_cutoff_drift_rgb: [0.05, 0.0, -0.02],
        ..Params::default()
    };
    let fc = FrameConstants::new((32, 8), (32, 8), false);
    let (aw, ah) = fc.domain_size(Domain::Amp);
    let drive = image(aw, ah, |x, _| [x as f32 / (aw - 1) as f32; 3]);
    gpu.enable_only(&["gun"]).unwrap();
    let settings = Settings::identity();
    let r = gpu
        .render_stages(&StageRun {
            input: StageInput::Image {
                after: "amp",
                image: &drive,
            },
            frame: fc,
            settings: &settings,
            params: &p,
            format: TargetFormat::Rgba32F,
        })
        .expect("render_stages");
    let out = r.stage("gun").unwrap();
    let cathode = Cathode {
        loss: p.ageing_cathode_loss_rgb.map(f64::from),
        cutoff_drift: p.ageing_cutoff_drift_rgb.map(f64::from),
    };
    let gun = Gun::default();
    for x in 0..aw {
        let v = f64::from(drive.pixel(x, 0)[0]);
        let o = out.pixel(x, 0);
        for (c, got) in o.iter().take(3).enumerate() {
            // The shader subtracts the f32 drift from the f32 drive.
            let vd = f64::from(drive.pixel(x, 0)[0] - p.ageing_cutoff_drift_rgb[c]);
            let want = (1.0 - f64::from(p.ageing_cathode_loss_rgb[c])) * gun.beam_current(c, vd);
            assert!(
                (f64::from(*got) - want).abs() <= want * (HALF_FP16 + 2e-4) + 1e-7,
                "gun x {x} channel {c}: {got} against {want}"
            );
            let model = cathode.beam_current(&gun, c, v);
            assert!((model - want).abs() <= 1e-6 * want.max(1e-3), "oracle cross-check");
        }
    }
    assert_eq!(out.pixel(1, 0)[0], 0.0, "red below its drifted cutoff: black");
}

/// Spot growth 1.2: the measured vertical sigma of one scanline is 1.2 times the new tube's (+/- 2 %).
fn spot_growth(gpu: &mut Headless) {
    let (cols, rows, n, ppm) = (16u32, 8u32, 12u32, 4.0f32);
    let mut fc = FrameConstants::new((cols, rows), (cols * n, rows * n), false);
    fc.px_per_mm = ppm;
    fc.line_pitch_mm = n as f32 / ppm;
    let (aw, ah) = fc.domain_size(Domain::Amp);
    let amp = image(aw, ah, |_, y| if y == 4 { [1.0; 3] } else { [0.0; 3] });
    let base = Params {
        beam_fwhm_mid_mm: 2.0,
        beam_fwhm_peak_mm: 2.0,
        beam_tail_weight: 0.0,
        mask_enabled: false,
        beam_quality: BEAM_QUALITY_REFERENCE,
        ..Params::default()
    };
    let sigma = |gpu: &mut Headless, p: &Params| -> f64 {
        gpu.enable_only(&["screen"]).unwrap();
        let settings = Settings::identity();
        let r = gpu
            .render_stages(&StageRun {
                input: StageInput::Image {
                    after: "gun",
                    image: &amp,
                },
                frame: fc,
                settings: &settings,
                params: p,
                format: TargetFormat::Rgba32F,
            })
            .expect("render_stages");
        let img = r.stage("screen").unwrap();
        let x = img.width / 2;
        let (mut s0, mut s1, mut s2) = (0.0, 0.0, 0.0);
        for y in 0..img.height {
            let l = f64::from(img.pixel(x, y)[1]);
            let c = f64::from(y) + 0.5;
            (s0, s1, s2) = (s0 + l, s1 + l * c, s2 + l * c * c);
        }
        let mean = s1 / s0;
        (s2 / s0 - mean * mean - 1.0 / 12.0).sqrt()
    };
    let s_new = sigma(gpu, &base);
    let worn = Params {
        ageing_enable: true,
        ageing_spot_growth: 1.2,
        ..base
    };
    let s_worn = sigma(gpu, &worn);
    let want = 2.0 / 2.354_820_045 * f64::from(ppm);
    assert!(
        (s_new / want - 1.0).abs() < 0.02,
        "new tube sigma {s_new} px, {want} expected"
    );
    assert!(
        (s_worn / s_new / 1.2 - 1.0).abs() < 0.02,
        "worn sigma {s_worn} / new {s_new} = {}, 1.2 expected",
        s_worn / s_new
    );
}

/// Burn-in: one 'burn in now' frame of a HUD picture, map against the oracle; the ghost on the next frame
/// against the oracle; exactly 1 where no texel near the pixel wore; red-only wear stays red; monotonic.
fn burnin_accumulation_and_ghost(gpu: &mut Headless) {
    let p = burn_params();
    let ppm = 0.3;
    gpu.reset_wear();
    let pic = image(W, H, hud);
    gpu.burn_in_now(1000.0);
    // The frame time is 0 and the acceleration 0: only the 1000 h of 'burn in now' are added.
    let (first, _) = wear(gpu, &p, tube(ppm, 0.0), &pic);
    assert_eq!(first, pic, "the wear of a frame shows from the next frame on");
    let map = gpu.read_wear_map().unwrap().expect("map");
    assert_eq!((map.width, map.height), WEAR_MAP_SIZE);
    let place = oracle_place(&p, ppm);
    assert!((place.origin_px[0] - 6.0).abs() < 1e-3 && (place.texel_px[0] - 0.18125).abs() < 1e-5);
    let mut oracle = OracleMap::new(WEAR_MAP_SIZE.0 as usize, WEAR_MAP_SIZE.1 as usize);
    oracle.accumulate(&light_of(&pic), W as usize, H as usize, &place, 1000.0);
    assert_map("burn in now 1000 h", &map, &oracle, 1000.0);
    // Per colour: texels inside the red block wore red only (exactly 0 green and blue).
    let texel = |x: f64, y: f64| {
        let i = ((x - place.origin_px[0]) / place.texel_px[0]) as usize;
        let j = ((y - place.origin_px[1]) / place.texel_px[1]) as usize;
        (j * WEAR_MAP_SIZE.0 as usize + i) * 3
    };
    let t = texel(20.0, 20.0);
    assert!(
        map.q[t] > 999.0 && map.q[t + 1] == 0.0 && map.q[t + 2] == 0.0,
        "red only"
    );
    // The ghost: a flat grey shows the wear, the oracle applies the GPU map; away from wear exactly 1.
    let grey = image(W, H, |_, _| [0.5; 3]);
    let (out, _) = wear(gpu, &p, tube(ppm, 0.0), &grey);
    let (law, _) = oracle_law(&p);
    let gm = to_oracle(&map);
    for y in 0..H {
        for x in 0..W {
            let want = gm.apply(&law, 0.0, &place, f64::from(x), f64::from(y), [0.5; 3]);
            let o = out.pixel(x, y);
            for c in 0..3 {
                assert!(
                    fp16_close(o[c], want[c]),
                    "ghost ({x}, {y}) {c}: {} against {}",
                    o[c],
                    want[c]
                );
            }
        }
    }
    // Clear of every worn region by more than one texel plus a pixel: exactly unchanged.
    for (x, y) in [(45, 5), (100, 10), (5, 60), (120, 70), (45, 40)] {
        assert_eq!(out.pixel(x, y)[..3], [0.5; 3], "no wear at ({x}, {y})");
    }
    // Inside the bar the ghost is tinted: blue (fastest) darkest.
    let b = out.pixel(64, 90);
    assert!(b[2] < b[1] && b[1] < b[0] && b[0] < 0.5, "tinted ghost {b:?}");
    // Monotonic: a second burn adds, never removes.
    gpu.burn_in_now(500.0);
    let _ = wear(gpu, &p, tube(ppm, 0.0), &pic);
    let again = gpu.read_wear_map().unwrap().unwrap();
    assert!(again.q.iter().zip(&map.q).all(|(a, b)| a >= b));
    oracle.accumulate(&light_of(&pic), W as usize, H as usize, &place, 500.0);
    assert_map("second burn 500 h", &again, &oracle, 1500.0);
}

/// The calibration point on the GPU: a full-white field for 20,000 h at 350 cd/m2 keeps 0.85 of its
/// luminance (+/- 0.01), blue the most worn; the acceleration path (one frame of the longest step, 250 ms =
/// 1/14400 h, at x 288,000,000).
fn calibration_point(gpu: &mut Headless) {
    let mut p = burn_params();
    p.burnin_acceleration = (REF_HOURS * 14_400.0) as f32;
    gpu.reset_wear();
    let white = image(W, H, |_, _| [1.0; 3]);
    // One frame of 250 ms (the clamp of the frame step, review R-10) at acceleration 2.88e8: 20,000 h
    // (unplaced: the map covers the whole image).
    let _ = wear(gpu, &p, tube(0.0, 250.0), &white);
    let map = gpu.read_wear_map().unwrap().unwrap();
    // Uniform within the f32 placement of the texel edges (see `assert_map`).
    let worst = map
        .q
        .iter()
        .map(|q| (f64::from(*q) / REF_HOURS - 1.0).abs())
        .fold(0.0, f64::max);
    assert!(worst < 2e-4, "uniform 20,000 h: worst {worst:.2e}");
    p.burnin_acceleration = 0.0;
    let (out, _) = wear(gpu, &p, tube(0.0, 16.0), &white);
    let (law, shares) = oracle_law(&p);
    let o = out.pixel(W / 2, H / 2);
    let y: f64 = (0..3).map(|c| shares[c] * f64::from(o[c])).sum();
    assert!((y - 0.85).abs() < 0.01, "white luminance {y}");
    assert!((y - law.white_efficiency(shares, REF_HOURS)).abs() < 1e-3);
    assert!(o[2] < o[1] && o[1] < o[0], "blue wears faster than red: {o:?}");
    let e = law.efficiency([REF_HOURS; 3]);
    let ratio = (1.0 / f64::from(o[2]) - 1.0) / (1.0 / f64::from(o[0]) - 1.0);
    let want = (1.0 / e[2] - 1.0) / (1.0 / e[0] - 1.0);
    assert!(
        (ratio / want - 1.0).abs() < 0.01,
        "blue to red wear ratio {ratio}, rates give {want}"
    );
}

/// Review R-10: the first frame after a 10 minute minimise (frame time 600,000 ms) at acceleration 3600 wears
/// like a 250 ms frame (0.25 h of a white field, 350 cd/m2: 0.25 h_ref), not 600 h; the next normal frame adds
/// its own 16 ms.
fn long_frame_gap_does_not_wear_by_hours(gpu: &mut Headless) {
    let p = Params {
        burnin_acceleration: 3600.0,
        ..burn_params()
    };
    gpu.reset_wear();
    let white = image(W, H, |_, _| [1.0; 3]);
    let _ = wear(gpu, &p, tube(0.0, 600_000.0), &white);
    let map = gpu.read_wear_map().unwrap().unwrap();
    let worst = map
        .q
        .iter()
        .map(|q| (f64::from(*q) / 0.25 - 1.0).abs())
        .fold(0.0, f64::max);
    assert!(
        worst < 2e-4,
        "a 10 minute gap wears 0.25 h_ref, worst {worst:.2e} off ({})",
        map.q[0]
    );
    let _ = wear(gpu, &p, tube(0.0, 16.0), &white);
    let map = gpu.read_wear_map().unwrap().unwrap();
    let want = 0.25 + 16.0 / 3_600_000.0 * 3600.0;
    let worst = map
        .q
        .iter()
        .map(|q| (f64::from(*q) / want - 1.0).abs())
        .fold(0.0, f64::max);
    assert!(worst < 2e-4, "then a 16 ms frame: {want}, worst {worst:.2e} off");
}

/// Real time on an old tube: 1000 h already worn, frames of 3.6 ms at acceleration 1 and 350 cd/m2 add 1e-6 h
/// each, 1/61 of an f32 ulp at 1000 (6.1e-5). A plain FP32 sum would stay at 1000 for ever; the compensated
/// sum reaches 1000 + 200e-6 after 200 frames (within one ulp).
fn real_time_increments_are_not_lost(gpu: &mut Headless) {
    let p = Params {
        burnin_acceleration: 1.0,
        ..burn_params()
    };
    let (mw, mh) = WEAR_MAP_SIZE;
    gpu.set_wear_map(&WearMap {
        q: vec![1000.0; (mw * mh * 3) as usize],
        ..WearMap::new(mw, mh)
    })
    .unwrap();
    let white = image(W, H, |_, _| [1.0; 3]);
    for _ in 0..200 {
        let _ = wear(gpu, &p, tube(0.0, 3.6), &white);
    }
    let map = gpu.read_wear_map().unwrap().unwrap();
    let ulp = 1000.0_f32.next_up() - 1000.0;
    for (i, q) in map.q.iter().enumerate().step_by(997) {
        assert!(
            (f64::from(*q) - 1000.0002).abs() <= f64::from(ulp),
            "texel value {i}: {q}, 1000.0002 expected (increments lost?)"
        );
    }
}

/// Acceleration 0: the map stays bit-identical through 1000 live frames (frame times measured); the
/// asynchronous snapshot equals the blocking read; save and load through the state file are bit exact,
/// and a corrupt file gives a new tube with a warning (the controller on the headless renderer).
fn frozen_map_and_persistence(gpu: &mut Headless) {
    let mut p = burn_params();
    let before = gpu.read_wear_map().unwrap().unwrap();
    let (sw, sh) = (16u32, 12u32);
    let bgra = vec![255u8; (sw * sh * 4) as usize];
    let frame = SourceFrame {
        width: sw,
        height: sh,
        bgra: &bgra,
    };
    for n in gpu.stage_names() {
        gpu.set_stage_enabled(n, true).unwrap();
    }
    let frames: Vec<_> = (0..1000)
        .map(|i| (if i == 0 { Some(frame) } else { None }, View::full(sw, sh)))
        .collect();
    gpu.render_burst(&frames, &Settings::default(), &p, (64, 48), TargetFormat::Rgba16F)
        .expect("burst");
    let after = gpu.read_wear_map().unwrap().unwrap();
    assert!(
        after.q.iter().zip(&before.q).all(|(a, b)| a.to_bits() == b.to_bits()),
        "acceleration 0: bit-identical after 1000 frames"
    );
    // Asynchronous snapshot (event polled, no wait) equals the blocking read.
    gpu.request_wear_snapshot();
    assert!(
        gpu.take_wear_snapshot().unwrap().is_none(),
        "nothing before a frame ran"
    );
    gpu.render(
        Some(frame),
        View::full(sw, sh),
        &Settings::default(),
        &p,
        (64, 48),
        TargetFormat::Rgba16F,
    )
    .unwrap();
    let snap = gpu.take_wear_snapshot().unwrap().expect("snapshot after the frame");
    assert_eq!(snap, after);
    // File round trip through the controller.
    let dir = std::env::temp_dir().join(format!("phoxium-stage-ageing-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let t0 = Instant::now();
    let mut ts = TubeState::new(Some(dir.clone()), false, t0);
    // A stored map of another size is resampled on load (a uniform 3 h map stays 3 h).
    let path = tube_state::state_path(&dir, Some("p1"));
    tube_state::save(
        &path,
        &WearMap {
            q: vec![3.0; 64 * 48 * 3],
            ..WearMap::new(64, 48)
        },
    )
    .unwrap();
    ts.tick(gpu, &p, Some("p1"), t0);
    let loaded = gpu.read_wear_map().unwrap().unwrap();
    assert!(loaded.q.iter().all(|v| *v == 3.0), "resampled uniform map");
    gpu.set_wear_map(&after).unwrap();
    ts.finish(gpu);
    match tube_state::load(&path, WEAR_MAP_SIZE) {
        tube_state::Loaded::Map { map, resampled_from } => {
            assert_eq!(resampled_from, None);
            assert!(
                map.q.iter().zip(&after.q).all(|(a, b)| a.to_bits() == b.to_bits()),
                "saved bit exact"
            );
        }
        other => panic!("{other:?}"),
    }
    // Load into a new session: bit exact on the GPU.
    gpu.reset_wear();
    let mut ts = TubeState::new(Some(dir.clone()), false, t0);
    ts.tick(gpu, &p, Some("p1"), t0);
    // One frame uploads it; the read back then comes from the GPU images.
    let _ = wear(gpu, &p, tube(0.3, 0.0), &image(W, H, hud));
    let back = gpu.read_wear_map().unwrap().unwrap();
    assert!(
        back.q.iter().zip(&after.q).all(|(a, b)| a.to_bits() == b.to_bits()),
        "loaded bit exact"
    );
    // Corrupt file: new tube plus a warning, never a panic.
    std::fs::write(tube_state::state_path(&dir, Some("p2")), b"not a wear map").unwrap();
    ts.tick(gpu, &p, Some("p2"), t0);
    assert!(ts
        .take_messages()
        .iter()
        .any(|m| m.contains("warning") && m.contains(".bak")));
    let _ = wear(gpu, &p, tube(0.3, 0.0), &image(W, H, hud));
    let fresh = gpu.read_wear_map().unwrap().unwrap();
    assert_eq!(fresh.max_charge(), 0.0, "corrupt file: new tube");
    // Burn-in off: the controller does nothing, the map stays on the GPU.
    p.burnin_enable = false;
    ts.tick(gpu, &p, Some("p1"), t0);
    assert_eq!(
        ts.loaded_path(),
        Some(tube_state::state_path(&dir, Some("p2")).as_path())
    );
    let _ = std::fs::remove_dir_all(&dir);
}

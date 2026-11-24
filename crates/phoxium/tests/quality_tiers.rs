//! Quality tiers (PLAN T-48, QUESTIONS PRF-2, `params/quality.toml`, `src/quality.rs`): selecting a tier changes
//! the evaluation's cost parameters only, and each tier stays within its documented error against `exact`.
//!
//! The chain `amp` -> `gun` -> `screen` runs from a drive-voltage image injected after `decode`, with the
//! constants of the live path (`FrameConstants::placed`, whose `amp_k` follows the tier, plus the skirt margin), on
//! a 1920 x 1080 output, the mask off (the mask is the same multiplier in every tier; without it the error is in
//! units of white: current 1 gives light 1 on a flat field). Configurations: the table defaults, PVM-20M4U flat
//! (amp_samples 16), PVM-20M4U flat with a 5 % beam tail, PVM-20M4U flat with chromatic aberration (lateral 3 mm,
//! axis 0.5 mm at 30 deg, QUESTIONS PHY-17), PVM-20M4U as shipped (cylindrical) and the 26 in slot TV (spherical).
//! Contents: game-like pixel art, a 1-px checker, a ramp, per-pixel noise.
//!
//! Contracts (errors of the `screen` image against `exact`, mean absolute and max over every pixel and channel):
//! - `standard`: the T-48 accuracy contract, MAE <= 0.5 % and max <= 4 % of white; a curved tube is bit-identical
//!   (the tiers differ on separable geometries only);
//! - `fast`: MAE <= 1 % of white (PLAN T-48 tier contract), max <= [`FAST_MAX`];
//! - `low`: MAE <= 1 % of white, max <= [`LOW_MAX`].
//!
//! The measured values are printed (the tier table of ARCHITECTURE 3.7 comes from this output).

mod common;

use phoxium::gpu::{FrameConstants, Headless, ImageF32, Settings, StageInput, StageRun, TargetFormat};
use phoxium::params::{
    Params, GEOMETRY_CURVATURE_FLAT, QUALITY_TIER_EXACT, QUALITY_TIER_FAST, QUALITY_TIER_LOW, QUALITY_TIER_STANDARD,
};
use phoxium::patterns::{generate, Pattern};
use phoxium::presets::Library;

const DEFAULT_NATIVE: (u32, u32) = (320, 240);
const DEFAULT_TARGET: (u32, u32) = (1920, 1080);

/// `WxH` from an environment variable, else `default`.
fn size_from_env(name: &str, default: (u32, u32)) -> (u32, u32) {
    std::env::var(name)
        .ok()
        .and_then(|v| {
            let (w, h) = v.split_once('x')?;
            Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
        })
        .unwrap_or(default)
}

/// The source size: 320 x 240 (the contract), or `PHOXIUM_TIER_NATIVE` (`WxH`) to measure other sizes.
fn native_size() -> (u32, u32) {
    size_from_env("PHOXIUM_TIER_NATIVE", DEFAULT_NATIVE)
}

/// The output size: 1920 x 1080 (the contract), or `PHOXIUM_TIER_TARGET`.
fn target_size() -> (u32, u32) {
    size_from_env("PHOXIUM_TIER_TARGET", DEFAULT_TARGET)
}

/// The contracts are enforced for the default sizes only; other sizes are measured and printed.
fn enforcing() -> bool {
    native_size() == DEFAULT_NATIVE && target_size() == DEFAULT_TARGET
}

/// Contracts (module header), in units of white.
const STANDARD_MAE: f64 = 0.005;
const STANDARD_MAX: f64 = 0.04;
const TIER_MAE: f64 = 0.01;
/// Documented max of `fast` and `low` (largest measured, rounded up; ARCHITECTURE 3.7 tier table).
const FAST_MAX: f64 = 0.03;
const LOW_MAX: f64 = 0.03;

fn hash(a: u32, b: u32) -> u32 {
    let mut v = a.wrapping_mul(0x9E37_79B1) ^ b.wrapping_mul(0x85EB_CA77) ^ 0x1234_5678;
    v ^= v >> 15;
    v = v.wrapping_mul(0x2C1B_3C6D);
    v ^= v >> 12;
    v = v.wrapping_mul(0x297A_2D39);
    v ^ (v >> 15)
}

/// Codes (R, G, B) of the contents.
fn contents() -> Vec<(&'static str, Vec<[u8; 3]>)> {
    let (w, h) = native_size();
    let bgra = |p: &Pattern| -> Vec<[u8; 3]> { generate(p, w, h, 0).chunks(4).map(|b| [b[2], b[1], b[0]]).collect() };
    const PAL: [[u8; 3]; 8] = [
        [0, 0, 0],
        [29, 43, 83],
        [126, 37, 83],
        [0, 135, 81],
        [171, 82, 54],
        [255, 241, 232],
        [255, 0, 77],
        [41, 173, 255],
    ];
    let game: Vec<[u8; 3]> = (0..h)
        .flat_map(|y| {
            (0..w).map(move |x| {
                let t = hash(x / 8, y / 8);
                if t.is_multiple_of(3) {
                    PAL[(hash(x, y) % 8) as usize]
                } else {
                    PAL[(t >> 4) as usize % 8]
                }
            })
        })
        .collect();
    let noise: Vec<[u8; 3]> = (0..h)
        .flat_map(|y| {
            (0..w)
                .map(move |x| hash(x, y))
                .map(|v| [v as u8, (v >> 8) as u8, (v >> 16) as u8])
        })
        .collect();
    vec![
        ("game", game),
        ("checker 1 px", bgra(&Pattern::Checker(1))),
        ("ramp", bgra(&Pattern::Ramp)),
        ("noise", noise),
    ]
}

/// The configurations (mask off).
fn configs() -> Vec<(&'static str, Params)> {
    let lib = Library::embedded();
    let preset = |id: &str| Params {
        mask_enabled: false,
        ..lib.find(id).expect("shipped preset").params()
    };
    let flat01 = Params {
        geometry_curvature: GEOMETRY_CURVATURE_FLAT,
        ..preset("pvm-20m4u")
    };
    vec![
        (
            "defaults",
            Params {
                mask_enabled: false,
                ..Params::default()
            },
        ),
        ("01 flat", flat01),
        (
            "01 flat, tail 5 %",
            Params {
                beam_tail_weight: 0.05,
                ..flat01
            },
        ),
        (
            "01 flat, chromatic aberration",
            Params {
                geometry_chroma_lateral_mm: 3.0,
                geometry_chroma_axis_mm: 0.5,
                geometry_chroma_angle_deg: 30.0,
                ..flat01
            },
        ),
        ("01 (cylindrical)", preset("pvm-20m4u")),
        ("05 (spherical)", preset("slot-tv-26in")),
    ]
}

/// The `screen` image of `native` (drive voltages) through amp, gun and screen with the live constants.
fn screen(gpu: &mut Headless, p: &Params, native: &ImageF32) -> ImageF32 {
    let fc = FrameConstants::placed(native_size(), target_size(), false, p).with_skirt_margin(p);
    gpu.enable_only(&["amp", "gun", "screen"]).unwrap();
    let settings = Settings::identity();
    let run = StageRun {
        input: StageInput::Image {
            after: "decode",
            image: native,
        },
        frame: fc,
        settings: &settings,
        params: p,
        format: TargetFormat::Rgba32F,
    };
    gpu.render_stages(&run)
        .expect("render_stages")
        .stage("screen")
        .expect("screen ran")
        .clone()
}

/// Mean and max absolute difference over every pixel and channel.
fn errors(a: &ImageF32, b: &ImageF32) -> (f64, f64) {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let (mut sum, mut max, mut n) = (0.0, 0.0_f64, 0usize);
    for (pa, pb) in a.rgba.chunks(4).zip(b.rgba.chunks(4)) {
        for c in 0..3 {
            let d = (f64::from(pa[c]) - f64::from(pb[c])).abs();
            sum += d;
            max = max.max(d);
            n += 1;
        }
    }
    (sum / n as f64, max)
}

#[test]
fn each_tier_is_within_its_contract_of_exact() {
    let test = "quality_tiers contract";
    let Some(mut gpu) = common::gpu_or_skip(test) else {
        return;
    };
    let counter = gpu.validation_counter();
    let tier = |p: &Params, t: u32| Params { quality_tier: t, ..*p };
    let mut worst = [(0.0_f64, 0.0_f64); 3];
    for (cname, p) in configs() {
        let curved = p.geometry_curvature != GEOMETRY_CURVATURE_FLAT;
        for (kname, codes) in contents() {
            let mut rgba = Vec::with_capacity(codes.len() * 4);
            for c in &codes {
                rgba.extend(c.iter().map(|&v| f32::from(v) / 255.0));
                rgba.push(1.0);
            }
            let native = ImageF32 {
                width: native_size().0,
                height: native_size().1,
                rgba,
            };
            let exact = screen(&mut gpu, &tier(&p, QUALITY_TIER_EXACT), &native);
            let mut line = format!("{test}: {cname} | {kname}");
            for (k, (name, t, mae_max, max_max)) in [
                ("standard", QUALITY_TIER_STANDARD, STANDARD_MAE, STANDARD_MAX),
                ("fast", QUALITY_TIER_FAST, TIER_MAE, FAST_MAX),
                ("low", QUALITY_TIER_LOW, TIER_MAE, LOW_MAX),
            ]
            .into_iter()
            .enumerate()
            {
                let img = screen(&mut gpu, &tier(&p, t), &native);
                let (mae, max) = errors(&exact, &img);
                line.push_str(&format!(" | {name} MAE {:.4} % max {:.3} %", mae * 100.0, max * 100.0));
                worst[k] = (worst[k].0.max(mae), worst[k].1.max(max));
                assert!(
                    !enforcing() || (mae <= mae_max && max <= max_max),
                    "{test}: {cname}, {kname}, {name}: MAE {mae}, max {max}"
                );
                if enforcing() && curved && t == QUALITY_TIER_STANDARD {
                    assert_eq!(
                        img, exact,
                        "{test}: {cname}, {kname}: a curved tube is the same in standard"
                    );
                }
            }
            println!("{line}");
        }
    }
    for (name, (mae, max)) in ["standard", "fast", "low"].iter().zip(worst) {
        println!(
            "{test}: worst {name}: MAE {:.4} %, max {:.3} % of white",
            mae * 100.0,
            max * 100.0
        );
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

//! Bit-exact check of the chromatic aberration controls at their defaults (QUESTIONS PHY-17): with both amounts
//! at 0 the picture is the one of the build before the controls, bit for bit.
//!
//! [`defaults_match_the_recorded_reference`] renders a fixed set of scenes (the table defaults in every quality
//! tier and in the reference beam quality, the ten shipped presets as shipped, the PVM-20M4U preset with the
//! convergence controls set, flat and cylindrical) through the full chain (`render_cmd::render_source`, a
//! crosshatch source, 768 x 576) and hashes each output bit for bit (FNV-1a 64 of the f32 bits). Each render
//! uses a new headless renderer: a renderer keeps the temporal state of its stages (ABL low-pass, phosphor
//! persistence) and measures the frame time on the wall clock, so a second render on the same renderer depends
//! on the first and on timing; the first frame of a new renderer has frame time 0 and no history. Each scene is
//! rendered twice and must give the same hash both times.
//!
//! With `UCRT_CHROMA_REFERENCE` naming a file that does not exist, the hashes are written there; with an
//! existing file, every hash must be unchanged. The reference was recorded with the sources of the build
//! before the controls (this file only uses APIs that existed then), on the same device and driver, then
//! checked with the controls. Without the variable the test only prints the hashes (they depend on the device
//! and driver, so no hash is committed).

mod common;

use ultimatecrt::gpu::TargetFormat;
use ultimatecrt::params::{
    Params, BEAM_QUALITY_REFERENCE, GEOMETRY_CURVATURE_FLAT, QUALITY_TIER_EXACT, QUALITY_TIER_FAST, QUALITY_TIER_LOW,
};
use ultimatecrt::presets::Library;
use ultimatecrt::render_cmd::render_source;
use ultimatecrt::source::Source;

/// FNV-1a 64 of the f32 bits of an image.
fn fnv(rgba: &[f32]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for v in rgba {
        for b in v.to_bits().to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

/// The scenes of the recorded reference.
fn reference_scenes() -> Vec<(String, Params)> {
    let d = Params::default();
    let mut v = vec![
        ("defaults".to_string(), d),
        (
            "defaults exact".to_string(),
            Params {
                quality_tier: QUALITY_TIER_EXACT,
                ..d
            },
        ),
        (
            "defaults fast".to_string(),
            Params {
                quality_tier: QUALITY_TIER_FAST,
                ..d
            },
        ),
        (
            "defaults low".to_string(),
            Params {
                quality_tier: QUALITY_TIER_LOW,
                ..d
            },
        ),
        (
            "defaults reference beam".to_string(),
            Params {
                beam_quality: BEAM_QUALITY_REFERENCE,
                ..d
            },
        ),
    ];
    let lib = Library::embedded();
    for p in lib.list() {
        v.push((format!("preset {}", p.id), p.params()));
    }
    let p01 = lib.find("pvm-20m4u").expect("shipped preset").params();
    v.push((
        "01 flat, convergence".to_string(),
        Params {
            geometry_curvature: GEOMETRY_CURVATURE_FLAT,
            geometry_conv_red_dx_mm: 0.3,
            geometry_conv_blue_dy_mm: -0.2,
            geometry_conv_blue_radial_mm: -0.5,
            quality_tier: QUALITY_TIER_EXACT,
            ..p01
        },
    ));
    v.push((
        "01 cylindrical, convergence".to_string(),
        Params {
            geometry_conv_red_radial_mm: 0.6,
            geometry_conv_green_dx_mm: -0.2,
            ..p01
        },
    ));
    v
}

/// The hash of one render of `p` on a new renderer (`None`: no GPU, the test is skipped).
fn render_hash(test: &str, source: &Source, p: &Params) -> Option<u64> {
    let mut gpu = common::gpu_or_skip(test)?;
    let counter = gpu.validation_counter();
    let out = render_source(&mut gpu, source, p, Some((768, 576)), TargetFormat::Rgba32F).expect("render");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
    Some(fnv(&out.rgba))
}

/// Defaults reproduce the build before the controls, bit for bit (module header).
#[test]
fn defaults_match_the_recorded_reference() {
    let test = "stage_chroma_reference";
    let source = Source::choose(Some("crosshatch:16"), Some((256, 224)), None).unwrap();
    let mut lines = Vec::new();
    for (name, p) in reference_scenes() {
        let Some(a) = render_hash(test, &source, &p) else {
            return;
        };
        let b = render_hash(test, &source, &p).unwrap();
        assert_eq!(a, b, "{test}: {name}: two renders on new renderers differ");
        lines.push(format!("{name}\t{a:016x}"));
    }
    for l in &lines {
        println!("{test}: {l}");
    }
    let Some(path) = std::env::var_os("UCRT_CHROMA_REFERENCE") else {
        eprintln!("{test}: UCRT_CHROMA_REFERENCE not set, hashes printed only");
        return;
    };
    let path = std::path::PathBuf::from(path);
    if !path.exists() {
        std::fs::write(&path, lines.join("\n") + "\n").expect("write the reference");
        println!("{test}: reference recorded to {}", path.display());
        return;
    }
    let recorded = std::fs::read_to_string(&path).expect("read the reference");
    let recorded: Vec<&str> = recorded.lines().collect();
    assert_eq!(recorded.len(), lines.len(), "{test}: scene count");
    let differ: Vec<(&str, &str)> = recorded
        .iter()
        .zip(&lines)
        .filter(|(r, l)| **r != l.as_str())
        .map(|(r, l)| (*r, l.as_str()))
        .collect();
    assert!(
        differ.is_empty(),
        "{test}: differs from the recorded reference (recorded, now): {differ:?}"
    );
    println!("{test}: {} scenes bit-identical to {}", lines.len(), path.display());
}

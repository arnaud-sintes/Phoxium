//! Parameter validation (WP4, R-05): every input path goes through `Params::set_checked`, so no value outside
//! the hard range of the table (`params/*.toml`) reaches the GPU.
//!
//! - The owner's failure (B-02): 0 typed into `tube_aspect` (or a tube size from a file) used to give an empty
//!   tube domain, which the renderer turned into an error that ended the app. Typed values are now clamped to
//!   the slider range and file values to the hard range: the tube is always placed.
//! - The shipped presets are inside the hard ranges and round-trip through their files without a clamp.
//! - Headless sweep: every numeric parameter at its hard minimum and at its hard maximum renders a finite
//!   output, with no error and (when the layer is installed) no validation message.

mod common;

use ultimatecrt::gpu::{FrameConstants, TargetFormat};
use ultimatecrt::params::{self, Applied, ParamMeta, ParamType, ParamValue, Params, PARAMS_META};
use ultimatecrt::presets::{Library, Origin, Preset};
use ultimatecrt::render_cmd::render_source;
use ultimatecrt::source::Source;
use ultimatecrt::ui::panels::clamp_to_slider;

/// The tube is placed with a non-empty domain on `target`.
fn placed(p: &Params, target: (u32, u32)) -> bool {
    let fc = FrameConstants::placed((256, 240), target, false, p);
    fc.tube_size[0] > 0 && fc.tube_size[1] > 0 && fc.px_per_mm > 0.0 && fc.px_per_mm.is_finite()
}

/// `x` in every component of the type of `m` (numbers only).
fn value(m: &ParamMeta, x: f64) -> Option<ParamValue> {
    let f = x as f32;
    Some(match m.ty {
        ParamType::F32 => ParamValue::F32(f),
        ParamType::I32 => ParamValue::I32(x as i32),
        ParamType::Vec2 => ParamValue::Vec2([f; 2]),
        ParamType::Vec3 => ParamValue::Vec3([f; 3]),
        ParamType::Vec4 => ParamValue::Vec4([f; 4]),
        ParamType::Bool | ParamType::Enum => return None,
    })
}

const TUBE_IDS: [&str; 5] = [
    "tube_aspect",
    "tube_visible_height_mm",
    "tube_height_fraction",
    "source_par",
    "source_display_aspect",
];

/// Regression of the owner's failure: 0 (or a negative or tiny value) typed in the menu or read from a file
/// for a tube size never gives an empty tube domain.
#[test]
fn typed_or_loaded_zero_tube_sizes_never_give_an_empty_tube_domain() {
    for id in TUBE_IDS {
        let m = params::meta(id).unwrap();
        for x in [0.0, -0.0, -1.0, 1e-30, -1e30] {
            let v = value(m, x).unwrap();
            // Typed: clamped to the slider, then set.
            let mut typed = Params::default();
            assert!(typed.set(id, clamp_to_slider(m, v)), "{id} typed {x}");
            assert!(placed(&typed, (2560, 1440)), "{id} typed {x}: empty tube domain");
            // From a preset, the settings or --set: clamped to the hard range.
            let mut file = Params::default();
            assert!(file.set(id, v), "{id} = {x}");
            assert!(placed(&file, (2560, 1440)), "{id} = {x}: empty tube domain");
        }
    }
    // Every tube parameter at its hard minimum, then at its hard maximum, at once, from a 320 x 240 output up.
    // (Any positive minimum is under a pixel on a small enough window: there, the renderer's own empty-domain
    // guard applies, WP3.)
    for pick_max in [false, true] {
        let mut p = Params::default();
        for id in TUBE_IDS {
            let m = params::meta(id).unwrap();
            let (lo, hi) = m.hard_range.unwrap();
            assert!(p.set(id, value(m, if pick_max { hi } else { lo }).unwrap()));
        }
        for target in [(2560, 1440), (640, 480), (320, 240)] {
            assert!(
                placed(&p, target),
                "hard {} on {target:?}",
                if pick_max { "maxima" } else { "minima" }
            );
        }
    }
    params::clear_notices();
}

/// The shipped presets: inside the hard ranges, no warning, and the same parameters after a round trip
/// through their file text.
#[test]
fn shipped_presets_are_inside_the_hard_ranges_and_round_trip() {
    let lib = Library::embedded();
    assert!(lib.warnings.is_empty(), "{:?}", lib.warnings);
    assert_eq!(lib.list().len(), 10);
    for p in lib.list() {
        assert!(p.warnings.is_empty(), "{}: {:?}", p.id, p.warnings);
        for (id, v) in &p.values {
            assert!(params::in_hard_range(params::meta(id).unwrap(), *v), "{}: {id}", p.id);
        }
        let q = p.params();
        for m in PARAMS_META {
            assert!(params::in_hard_range(m, q.get(m.id).unwrap()), "{}: {}", p.id, m.id);
        }
        let back = Preset::parse("back.toml", &p.to_toml(), Origin::Embedded).unwrap();
        assert!(back.warnings.is_empty(), "{}: {:?}", p.id, back.warnings);
        assert_eq!(back.params(), q, "{}", p.id);
    }
}

/// A user preset saved from out-of-range live values (set through a path that clamps) loads back with the
/// same, valid, values.
#[test]
fn a_preset_saved_after_clamps_round_trips() {
    let mut p = Params::default();
    assert_eq!(
        p.set_checked("optics_halation_weight", ParamValue::F32(0.9)),
        Ok(Applied::Exact)
    );
    for (id, v) in [
        ("tube_aspect", ParamValue::F32(0.0)),
        ("amp_samples", ParamValue::I32(100_000)),
        ("optics_glare_weight", ParamValue::F32(0.9)),
        ("gun_drive_rgb", ParamValue::Vec3([-1.0, 1.0, 50.0])),
    ] {
        assert!(matches!(p.set_checked(id, v), Ok(Applied::Clamped(_))), "{id}");
    }
    assert!(p.optics_halation_weight + p.optics_glare_weight <= 1.0);
    assert_eq!(
        (p.tube_aspect, p.amp_samples, p.gun_drive_rgb),
        (0.25, 64, [0.0, 1.0, 10.0])
    );
    let saved = Preset::from_params("clamped", "Clamped", &p).unwrap();
    let back = Preset::parse("clamped.toml", &saved.to_toml(), Origin::Embedded).unwrap();
    assert!(
        back.warnings.iter().all(|w| !w.contains("valid range")),
        "{:?}",
        back.warnings
    );
    assert_eq!(back.params(), p);
}

/// Every numeric parameter at its hard minimum and maximum (all components), on top of a base with the
/// optional stages on, renders a finite output without an error.
#[test]
fn every_parameter_at_its_hard_bounds_renders_a_finite_output() {
    let Some(mut gpu) = common::gpu_or_skip("param_validation hard bounds") else {
        return;
    };
    let counter = gpu.validation_counter();
    let source = Source::choose(Some("ramp"), Some((64, 48)), None).unwrap();
    let mut base = Params::default();
    for (id, v) in [
        ("optics_enable", ParamValue::Bool(true)),
        ("composite_enable", ParamValue::Bool(true)),
        ("ageing_enable", ParamValue::Bool(true)),
        ("abl_enable", ParamValue::Bool(true)),
        ("phosphor_saturation", ParamValue::Bool(true)),
        ("phosphor_persistence", ParamValue::Bool(true)),
        ("output_match_srgb", ParamValue::Bool(false)),
        (
            "geometry_curvature",
            ParamValue::Enum(ultimatecrt::params::GEOMETRY_CURVATURE_SPHERICAL),
        ),
        ("beam_tail_weight", ParamValue::F32(0.05)),
    ] {
        assert!(base.set(id, v), "{id}");
    }
    let mut runs = 0;
    for m in PARAMS_META.iter().filter(|m| !m.file.starts_with("params/_")) {
        let Some((lo, hi)) = m.hard_range else {
            continue;
        };
        for x in [lo, hi] {
            let mut p = base;
            assert!(p.set(m.id, value(m, x).unwrap()), "{} = {x}", m.id);
            let out = render_source(&mut gpu, &source, &p, Some((192, 144)), TargetFormat::Rgba32F)
                .unwrap_or_else(|e| panic!("{} = {x}: render failed: {e}", m.id));
            assert!(
                out.rgba.iter().all(|c| c.is_finite()),
                "{} = {x}: non-finite output",
                m.id
            );
            runs += 1;
        }
    }
    eprintln!("param_validation: {runs} renders at the hard bounds");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
    params::clear_notices();
}

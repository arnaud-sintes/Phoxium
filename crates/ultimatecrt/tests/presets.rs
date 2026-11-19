//! GPU test of the tube presets (T-50): each shipped preset, applied onto the parameter defaults, renders a
//! test pattern through the headless `render` path at 2560x1440 into a finite image, with the validation
//! layer silent. The debug build renders three presets (a grille monitor, a TV with ABL, a slot TV); the
//! ignored test renders all ten (`cargo test --test presets -- --ignored`). Every shipped preset has persistence on (T-64),\n//! so these renders include the phosphor trail, the faceplate geometry, convergence and, on the TV presets, ABL.\n//! The persistence test below checks the trail of each preset against the crt_model::phosphor oracle.

mod common;

use crt_model::phosphor::{DecayFit, KUHN_BLUE, KUHN_GREEN, KUHN_RED};
use crt_model::units::frame_period_ms;
use ultimatecrt::gpu::{FrameConstants, Headless, ImageF32, Settings, StageInput, StageRun, TargetFormat};
use ultimatecrt::params::Params;
use ultimatecrt::presets::Library;
use ultimatecrt::render_cmd::render_source;
use ultimatecrt::source::Source;

const SIZE: (u32, u32) = (2560, 1440);

fn render_presets(name: &str, indices: &[usize]) {
    let Some(mut gpu) = common::gpu_or_skip(name) else {
        return;
    };
    let counter = gpu.validation_counter();
    let lib = Library::embedded();
    assert!(lib.warnings.is_empty(), "{:?}", lib.warnings);
    let source = Source::choose(Some("crosshatch:16"), Some((320, 240)), None).unwrap();
    for &i in indices {
        let preset = &lib.list()[i];
        let img = render_source(&mut gpu, &source, &preset.params(), Some(SIZE), TargetFormat::Rgba32F)
            .unwrap_or_else(|e| panic!("{}: {e}", preset.id));
        assert_eq!((img.width, img.height), SIZE, "{}", preset.id);
        let mut sum = 0.0_f64;
        let mut max = 0.0_f32;
        // Every value, alpha included, must be finite; the colour channels also feed the statistics.
        for (i, &c) in img.rgba.iter().enumerate() {
            assert!(c.is_finite(), "{}: non-finite output value {c}", preset.id);
            if i % 4 != 3 {
                sum += f64::from(c);
                max = max.max(c);
            }
        }
        let mean = sum / (f64::from(SIZE.0) * f64::from(SIZE.1) * 3.0);
        assert!(
            mean > 0.001 && max > 0.1,
            "{}: picture is black (mean {mean}, max {max})",
            preset.id
        );
        assert!(max <= 1.0 + 1e-4, "{}: encoded value above 1: {max}", preset.id);
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

#[test]
fn three_presets_render_finite_images() {
    render_presets("presets three", &[0, 2, 4]);
}

#[test]
#[ignore = "renders all ten presets at 2560x1440 (slow in the debug build)"]
fn all_ten_presets_render_finite_images() {
    render_presets("presets all", &(0..10).collect::<Vec<_>>());
}

/// A texel image of one colour.
fn flat(w: u32, h: u32, c: f32) -> ImageF32 {
    let mut rgba = Vec::new();
    for _ in 0..w * h {
        rgba.extend_from_slice(&[c, c, c, 1.0]);
    }
    ImageF32 {
        width: w,
        height: h,
        rgba,
    }
}

/// One frame of the `phosphor` stage alone on `input` (injected after `screen`), with `params`.
fn run_phosphor(gpu: &mut Headless, params: &Params, input: &ImageF32) -> ImageF32 {
    gpu.enable_only(&["phosphor"]).unwrap();
    let settings = Settings::identity();
    let (w, h) = (input.width, input.height);
    let r = gpu
        .render_stages(&StageRun {
            input: StageInput::Image {
                after: "screen",
                image: input,
            },
            frame: FrameConstants::new((w, h), (w, h), false),
            settings: &settings,
            params,
            format: TargetFormat::Rgba32F,
        })
        .expect("render_stages");
    r.stage("phosphor").expect("phosphor output").clone()
}

/// T-64: with the parameters of each shipped preset (persistence on, tail strength 1, 60 Hz), a single white frame
/// followed by black gives the CRT.md frame-energy ratio of the Kuhn P22 fit in the next frame: green E1/E0 = 5.36 %
/// (blue 4.72 %, red none), equal to the `crt_model::phosphor` oracle within one FP16 step.
#[test]
fn every_preset_persistence_trail_equals_the_oracle() {
    let Some(mut gpu) = common::gpu_or_skip("presets persistence trail") else {
        return;
    };
    let counter = gpu.validation_counter();
    let lib = Library::embedded();
    assert!(lib.warnings.is_empty(), "{:?}", lib.warnings);
    let t = frame_period_ms(60.0);
    let (w, h) = (8, 4);
    let (black, white) = (flat(w, h, 0.0), flat(w, h, 1.0));
    let fp16 = 1.0 / 1024.0 + 1e-5;
    for preset in lib.list() {
        let p = preset.params();
        assert!(p.phosphor_persistence, "{}: persistence on", preset.id);
        assert_eq!(p.phosphor_tail_strength_rgb, [1.0; 3], "{}", preset.id);
        assert_eq!(p.phosphor_frame_rate_hz, 60.0, "{}", preset.id);
        // Flush the trail of the previous preset, then a black frame primes the history to 0.
        let mut last = run_phosphor(&mut gpu, &p, &black);
        for _ in 0..3000 {
            if last == black {
                break;
            }
            last = run_phosphor(&mut gpu, &p, &black);
        }
        assert_eq!(last, black, "{}: trail flushed", preset.id);
        let y0 = run_phosphor(&mut gpu, &p, &white);
        let y1 = run_phosphor(&mut gpu, &p, &black);
        let fits: [(&str, DecayFit<'static>, f64); 3] = [
            ("red", KUHN_RED, 0.0),
            ("green", KUHN_GREEN, 5.36),
            ("blue", KUHN_BLUE, 4.72),
        ];
        for (ch, (name, fit, table)) in fits.into_iter().enumerate() {
            let (a, b) = (f64::from(y0.rgba[ch]), f64::from(y1.rgba[ch]));
            if ch == 0 {
                assert_eq!(b, 0.0, "{}: red has no trail", preset.id);
                continue;
            }
            let ratio = b / a;
            let oracle = fit.frame_energy_ratios(t)[0];
            assert!(
                ((ratio - oracle) / oracle).abs() < fp16,
                "{}: {name} E1/E0 {ratio} against oracle {oracle}",
                preset.id
            );
            assert!(
                (ratio * 100.0 - table).abs() < 0.02,
                "{}: {name} E1/E0 {} % against CRT.md {table} %",
                preset.id,
                ratio * 100.0
            );
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

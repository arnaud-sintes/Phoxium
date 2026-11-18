//! GPU test of the tube presets (T-50): each shipped preset, applied onto the parameter defaults, renders a
//! test pattern through the headless `render` path at 2560x1440 into a finite image, with the validation
//! layer silent. The debug build renders three presets (a grille monitor, a TV with ABL, a slot TV); the
//! ignored test renders all ten (`cargo test --test presets -- --ignored`).

mod common;

use ultimatecrt::gpu::TargetFormat;
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

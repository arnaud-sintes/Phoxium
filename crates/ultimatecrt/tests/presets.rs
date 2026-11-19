//! GPU test of the tube presets (T-50): each shipped preset, applied onto the parameter defaults, renders a
//! test pattern through the headless `render` path at 2560x1440 into a finite image, with the validation
//! layer silent. The debug build renders three presets (a grille monitor, a TV with ABL, a slot TV); the
//! ignored test renders all ten (`cargo test --test presets -- --ignored`). Every shipped preset has persistence on (T-64),\n//! so these renders include the phosphor trail, the faceplate geometry, convergence and, on the TV presets, ABL.\n//! The persistence test below checks the trail of each preset against the crt_model::phosphor oracle.

mod common;

use crt_model::output::{apply, shoulder_with_rate};
use crt_model::phosphor::{DecayFit, KUHN_BLUE, KUHN_GREEN, KUHN_RED};
use crt_model::units::frame_period_ms;
use ultimatecrt::gpu::{
    output_matrix, FrameConstants, Headless, ImageF32, Settings, SourceFrame, StageInput, StageRun, TargetFormat,
};
use ultimatecrt::headroom::OutputTransform;
use ultimatecrt::params::{
    ParamValue, Params, OUTPUT_HEADROOM_MODE_CLIP, OUTPUT_HEADROOM_MODE_MASK_DEPTH, OUTPUT_HEADROOM_MODE_PHYSICAL,
    OUTPUT_HEADROOM_MODE_SHOULDER,
};
use ultimatecrt::presets::Library;
use ultimatecrt::render_cmd::render_source;
use ultimatecrt::scaling::View;
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

/// T-84 (audit B1/M1, D-16): a full-white field through the whole chain of every preset, in each headroom
/// mode, at the reference host 2560 x 1440 (the owner's monitors; 320 x 240 source, the raster at full height).
///
/// The chain runs as live (beam, mask, saturation, persistence, geometry, encode; `Settings::default`, host pure
/// 2.2, float target so no dither), three frames after a history clear (persistence primed). ABL is turned off:
/// on a full-white field it only lowers the light, and its loop integrates the real time between headless
/// frames, which would make the modes differ by timing alone.
///
/// Per texel of the encode input L (the last chain image), the display value is c = M (e L) (M the colour
/// matrix, e the exposure the mode pushes, `OutputTransform`), and the output must equal the oracle
/// `shoulder_with_rate(c, knee, rate)` (hard clip for knee 1 or rate 0) decoded through 2.2: checked as the
/// sums over the field (relative 2e-3: FP16 storage and the f32 pow round trip). Hand derivations of the
/// expectations:
/// - The readout (`crate::headroom`) is an upper bound: max c <= readout (x e / exposure in physical modes).
/// - physical, mask_depth: e = exposure / max(1, H (1 + 0.02)), so max c <= 1 / 1.02 < 1: no clip counted, and
///   the output mean equals the unclipped mean (no loss, within 2e-3).
/// - mask_depth (depth 0.5): the mask weight 1 + d (m - 1) has the same field mean as m (linear, mean of m is
///   1), so the encode input mean equals the one of physical mode within 0.5 % (edges of the curved raster),
///   and the mask peak is 1 + 0.5 (peak - 1).
/// - shoulder (default): exposure unchanged, the white point W = H x 1.02 >= max c, so no value is hard-clipped;
///   values above the knee 0.6 are compressed, and the field mean loses at most [`SHOULDER_MAX_LOSS`].
/// - clip (T-46): the loss to clipping is reported (the audit's 12 to 60 %), not asserted.
#[test]
fn flat_white_through_the_chain_in_every_headroom_mode() {
    let Some(mut gpu) = common::gpu_or_skip("presets flat white") else {
        return;
    };
    let counter = gpu.validation_counter();
    let lib = Library::embedded();
    let (w, h) = (320u32, 240u32);
    let white = vec![255u8; (w * h * 4) as usize];
    let frame = SourceFrame {
        width: w,
        height: h,
        bgra: &white,
    };
    let view = View::full(w, h);
    let modes = [
        ("clip", OUTPUT_HEADROOM_MODE_CLIP),
        ("shoulder", OUTPUT_HEADROOM_MODE_SHOULDER),
        ("physical", OUTPUT_HEADROOM_MODE_PHYSICAL),
        ("mask_depth", OUTPUT_HEADROOM_MODE_MASK_DEPTH),
    ];
    eprintln!(
        "preset            mask(phys)    scan   matrix  exp    H      | loss clip  shoulder | physical e   white"
    );
    for preset in lib.list() {
        let base = preset.params();
        assert_eq!(
            base.output_headroom_mode, OUTPUT_HEADROOM_MODE_SHOULDER,
            "{}: D-16 default",
            preset.id
        );
        let mut input_sum = [0.0_f64; 4];
        let mut losses = [0.0_f64; 4];
        let mut physical_e = 0.0;
        let mut readout = None;
        for (k, (name, mode)) in modes.iter().enumerate() {
            let mut p = base;
            assert!(p.set("abl_enable", ParamValue::Bool(false)));
            assert!(p.set("output_headroom_mode", ParamValue::Enum(*mode)));
            assert!(p.set("mask_depth", ParamValue::F32(0.5)));
            gpu.clear_history();
            let frames = [(Some(frame), view); 3];
            let r = gpu
                .render_burst(&frames, &Settings::default(), &p, SIZE, TargetFormat::Rgba32F)
                .unwrap_or_else(|e| panic!("{} {name}: {e}", preset.id));
            let hr = gpu.last_headroom().expect("readout");
            let xf = OutputTransform::new(&p, &hr);
            let m = output_matrix(&p);
            let (_, input) = r.stages.last().expect("chain image");
            let e = f64::from(xf.exposure);
            let rate = (xf.knee < 1.0 && xf.rate > 0.0).then_some(f64::from(xf.rate));
            let (mut unclipped, mut expected, mut max_c) = (0.0_f64, 0.0_f64, 0.0_f64);
            for t in input.rgba.chunks(4) {
                let c = apply(&m, [t[0], t[1], t[2]].map(|v| f64::from(v) * e));
                for v in c {
                    unclipped += v.max(0.0);
                    expected += shoulder_with_rate(v, f64::from(xf.knee), rate);
                    max_c = max_c.max(v);
                }
                input_sum[k] += f64::from(t[0] + t[1] + t[2]);
            }
            let got: f64 = r
                .output
                .rgba
                .chunks(4)
                .map(|o| (0..3).map(|i| f64::from(o[i].max(0.0)).powf(2.2)).sum::<f64>())
                .sum();
            let id = &preset.id;
            assert!(
                ((got - expected) / expected).abs() < 2e-3,
                "{id} {name}: output sum {got} against the oracle {expected}"
            );
            let bound = hr.total() * e / f64::from(p.exposure);
            assert!(
                max_c <= bound * (1.0 + 1e-3),
                "{id} {name}: peak {max_c} above the readout {bound} ({hr:?})"
            );
            losses[k] = 1.0 - got / unclipped;
            match *mode {
                OUTPUT_HEADROOM_MODE_PHYSICAL | OUTPUT_HEADROOM_MODE_MASK_DEPTH => {
                    assert_eq!(r.clip.over_white, 0, "{id} {name}: clipped pixels");
                    assert!(max_c <= 1.0 / 1.02 + 1e-3, "{id} {name}: peak {max_c}");
                    assert!(losses[k].abs() < 2e-3, "{id} {name}: loss {}", losses[k]);
                    if *mode == OUTPUT_HEADROOM_MODE_PHYSICAL {
                        physical_e = e;
                        readout = Some(hr);
                    } else {
                        let want = 1.0 + 0.5 * (hr.mask_peak_physical - 1.0);
                        assert!((hr.mask_peak - want).abs() < 1e-9, "{id}: {hr:?}");
                    }
                }
                OUTPUT_HEADROOM_MODE_SHOULDER => {
                    assert_eq!(f64::from(xf.exposure), f64::from(p.exposure), "{id}");
                    if let Some(a) = rate {
                        // No hard clip: the white point is at or above every value.
                        let white = hr.total() * 1.02;
                        assert!(max_c <= white * (1.0 + 1e-3), "{id}: {max_c} above W {white}");
                        // The f32 rate reaches panel white at W within its rounding.
                        assert!(shoulder_with_rate(white, f64::from(xf.knee), Some(a)) > 1.0 - 1e-5);
                    }
                    assert!(losses[k] < SHOULDER_MAX_LOSS, "{id} shoulder: loss {}", losses[k]);
                }
                _ => {}
            }
        }
        // Mask depth keeps the field mean of the encode input (physical: index 2, mask depth: index 3).
        let ratio = input_sum[3] / input_sum[2];
        assert!(
            (ratio - 1.0).abs() < 5e-3,
            "{}: mask depth changes the mean by {ratio}",
            preset.id
        );
        let hr = readout.expect("physical readout");
        eprintln!(
            "{:<17} {:.3}         {:.3}  {:.3}   {:.2}   {:.3}  | {:5.1} %   {:5.1} % | {:.3}  {:.3}",
            preset.id,
            hr.mask_peak_physical,
            hr.scanline_peak,
            hr.matrix_max,
            hr.exposure,
            hr.total(),
            100.0 * losses[0],
            100.0 * losses[1],
            physical_e,
            physical_e / hr.exposure,
        );
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Largest share of the full-white field mean the default shoulder (knee 0.6, automatic white point) may lose
/// on a shipped preset at 2560 x 1440 (T-84). Measured worst: preset 06 (slot mask, readout 3.87) 55.1 %, with 5
/// points of room. The shoulder loses 1 to 4 points MORE than the T-46 hard clip (54.1 % there): the knee 0.6
/// also compresses the field mean between 0.6 and 1; what it buys is no hard clip (no bands or tint), not mean
/// brightness. The measured table is in the presets' notes.
const SHOULDER_MAX_LOSS: f64 = 0.6;

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

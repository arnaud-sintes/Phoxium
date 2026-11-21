//! GPU test of the `render` command (T-23): the files written by `phoxium render --pattern ramp`
//! equal the in-process headless result exactly, and the PNG matches the pipeline formula.
//!
//! Pipeline with the defaults of the live path (T-20a, `Settings::default`, `Params::default`): the
//! source codes c are decoded to drive voltages V = c / 255 (FP16), the `gun` slot (T-34, default pure
//! power 2.4) makes the linear light L = (c / 255)^2.4, stored in FP16, and
//! `encode` writes V = L^(1 / 2.2) (host pure 2.2), so the expected code is
//! round(255 * (c / 255)^(2.4 / 2.2)). The FP16 intermediate shifts it by well under one code; the
//! test allows 1.

mod common;

use std::path::PathBuf;
use std::process::Command;

use phoxium::gpu::TargetFormat;
use phoxium::params::Params;
use phoxium::patterns;
use phoxium::render_cmd::{decode_pfm, render_source};
use phoxium::source::Source;

const W: u32 = 256;
const H: u32 = 8;

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("phoxium-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn run_cli(out: &std::path::Path) {
    let status = Command::new(env!("CARGO_BIN_EXE_phoxium"))
        .args([
            "render",
            "--pattern",
            "ramp",
            "--pattern-size",
            &format!("{W}x{H}"),
            "--out",
        ])
        .arg(out)
        .status()
        .expect("spawn phoxium");
    assert!(status.success(), "phoxium render failed");
}

fn ramp_source() -> Source {
    Source::choose(Some("ramp"), Some((W, H)), None).unwrap()
}

#[test]
fn pfm_equals_the_in_process_render() {
    let Some(mut gpu) = common::gpu_or_skip("render_cmd pfm") else {
        return;
    };
    let counter = gpu.validation_counter();
    let expected = render_source(
        &mut gpu,
        &ramp_source(),
        &Params::default(),
        None,
        TargetFormat::Rgba32F,
    )
    .unwrap();
    drop(gpu);
    common::assert_no_validation_messages(&counter);

    let dir = temp_dir("pfm");
    let out = dir.join("ramp.pfm");
    run_cli(&out);
    let got = decode_pfm(&std::fs::read(&out).unwrap()).unwrap();
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!((got.width, got.height), (W, H));
    for y in 0..H {
        for x in 0..W {
            assert_eq!(got.pixel(x, y)[..3], expected.pixel(x, y)[..3], "pixel ({x}, {y})");
        }
    }
}

#[test]
fn png_matches_the_pipeline_formula() {
    let Some(mut gpu) = common::gpu_or_skip("render_cmd png") else {
        return;
    };
    let counter = gpu.validation_counter();
    let expected = render_source(
        &mut gpu,
        &ramp_source(),
        &Params::default(),
        None,
        TargetFormat::Bgra8Unorm,
    )
    .unwrap();
    // The formula is the decode and encode path: since T-36b the beam's spot (horizontal blur, spot
    // growth) changes pixel values even at 1 px per source pixel, so it is checked with every slot
    // bypassed (an exact identity, A11); the PNG is checked against the full render. The gun stays on
    // (it carries the tube gamma since T-34), only the beam/mask `screen` slot is bypassed.
    gpu.set_stage_enabled("screen", false).expect("bypass the beam");
    let plain = render_source(
        &mut gpu,
        &ramp_source(),
        &Params::default(),
        None,
        TargetFormat::Bgra8Unorm,
    )
    .unwrap();
    drop(gpu);
    common::assert_no_validation_messages(&counter);

    let dir = temp_dir("png");
    let out = dir.join("ramp.png");
    run_cli(&out);
    let mut dec = png::Decoder::new(std::io::Cursor::new(std::fs::read(&out).unwrap()));
    dec.set_transformations(png::Transformations::EXPAND);
    let mut r = dec.read_info().unwrap();
    let mut buf = vec![0; r.output_buffer_size().unwrap()];
    let info = r.next_frame(&mut buf).unwrap();
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!((info.width, info.height), (W, H));
    assert_eq!(info.color_type, png::ColorType::Rgb);
    assert_eq!(info.bit_depth, png::BitDepth::Eight);

    // The source codes, from the pattern itself (BGRA).
    let src = patterns::generate(&"ramp".parse().unwrap(), W, H, 0);
    for y in 0..H {
        for x in 0..W {
            let i = (y * W + x) as usize;
            for c in 0..3 {
                let got = buf[i * 3 + c];
                // Exactly the in-process Bgra8 render.
                assert_eq!(
                    f32::from(got),
                    (expected.pixel(x, y)[c] * 255.0).round(),
                    "pixel ({x}, {y}) ch {c}"
                );
                // And the hand-derived formula (src is BGRA: channel c of RGB is byte 2 - c), beam bypassed.
                let code = f64::from(src[i * 4 + 2 - c]);
                let want = (255.0 * (code / 255.0).powf(2.4 / 2.2)).round();
                let plain = f64::from((plain.pixel(x, y)[c] * 255.0).round());
                assert!(
                    (plain - want).abs() <= 1.0,
                    "pixel ({x}, {y}) ch {c}: code {code} -> {plain}, formula {want}"
                );
            }
        }
    }
}

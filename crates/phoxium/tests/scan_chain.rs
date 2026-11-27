//! Motion clarity through the whole live chain (`scan_mode`, slot `scan`, QUESTIONS PHY-12 "Motion clarity"; oracle
//! `crt_model::scan`): a captured BGRA frame goes through every slot the window loop runs, at a screen refresh of 240 or
//! 300 Hz given to the pipeline (`Settings::refresh_hz`), one `render` per refresh, the frame period fixed
//! (`Headless::set_frame_time_ms`). The images read back are the chain slots' (`Rendered::stage`).
//!
//! What is checked, by hand-derived expectations:
//! - **A rolling sweep** of `n` refreshes lights, at refresh `k`, the band of lines the beam crosses and nothing else,
//!   `n` times brighter with the full boost (a line is lit for 1 / `n` of the sweep): every row that holds light is lit in
//!   exactly one refresh, the bands tile the image in order, and their mean over the sweep is the frame-integrated
//!   picture. The stages before `scan` ran once: their image is the same at every refresh.
//! - **A new source frame** arriving in the middle of a sweep is decoded (the native raster is the new frame) but only
//!   drawn by the next sweep: no refresh shows half of each.
//! - **A strobe** shows the whole frame, at the gain `n / lit`, for its first `lit` refreshes and black after.
//! - **No sweep** (the screen refreshes less than twice per source frame) is exactly off.
//! - **With persistence** the phosphor recurrence keeps the mean light of a still picture over the sweep (its steady
//!   state gain is 1), whatever the bands do.

mod common;

use phoxium::gpu::{Headless, ImageF32, Rendered, Settings, SourceFrame, StageReadback, TargetFormat};
use phoxium::params::{ParamValue, Params};
use phoxium::render_cmd::set_param;
use phoxium::scaling::View;

const SRC: (u32, u32) = (160, 120);
const TARGET: (u32, u32) = (480, 360);

/// A flat BGRA source of the code `level`.
fn flat(level: u8) -> Vec<u8> {
    [level, level, level, 255].repeat((SRC.0 * SRC.1) as usize)
}

/// The defaults with the ABL off (its low-pass would move the drive between sweeps) and the given scan settings.
fn params(sets: &[(&str, &str)]) -> Params {
    let mut p = Params::default();
    assert!(p.set("abl_enable", ParamValue::Bool(false)));
    for (id, v) in sets {
        set_param(&mut p, id, v).unwrap_or_else(|e| panic!("{id}={v}: {e}"));
    }
    p
}

fn settings(refresh_hz: f32) -> Settings {
    Settings {
        refresh_hz,
        ..Settings::default()
    }
}

/// One refresh: `src` is a newly captured frame, if any.
fn refresh(gpu: &mut Headless, src: Option<&[u8]>, s: &Settings, p: &Params) -> Rendered {
    let frame = src.map(|bgra| SourceFrame {
        width: SRC.0,
        height: SRC.1,
        bgra,
    });
    gpu.render(frame, View::full(SRC.0, SRC.1), s, p, TARGET, TargetFormat::Rgba32F)
        .expect("render")
}

/// The sum of the light of each row of an image (rgb).
fn row_sums(img: &ImageF32) -> Vec<f64> {
    (0..img.height)
        .map(|y| {
            (0..img.width)
                .map(|x| {
                    let p = img.pixel(x, y);
                    f64::from(p[0]) + f64::from(p[1]) + f64::from(p[2])
                })
                .sum()
        })
        .collect()
}

fn total(img: &ImageF32) -> f64 {
    row_sums(img).iter().sum()
}

/// A headless renderer with a fixed frame period of one refresh at `hz`.
fn rig(test: &str, hz: f32) -> Option<Headless> {
    let mut gpu = common::gpu_or_skip(test)?;
    gpu.set_frame_time_ms(Some(1000.0 / hz));
    Some(gpu)
}

/// Rolling scan, 300 Hz for 60 Hz: five refreshes, one band each, five times the light, the same held image under all.
#[test]
fn a_rolling_sweep_lights_one_band_per_refresh() {
    let Some(mut gpu) = rig("a_rolling_sweep_lights_one_band_per_refresh", 300.0) else {
        return;
    };
    let counter = gpu.validation_counter();
    let (s, p) = (settings(300.0), params(&[("scan_mode", "rolling")]));
    let src = flat(128);
    let frames: Vec<Rendered> = (0..5)
        .map(|k| refresh(&mut gpu, (k == 0).then_some(&src[..]), &s, &p))
        .collect();
    let base = frames[0].stage("screen").expect("screen slot").clone();
    let base_rows = row_sums(&base);
    assert!(
        base_rows.iter().filter(|&&r| r > 0.0).count() > 100,
        "the picture has light"
    );
    let mut first_lit: Vec<Option<usize>> = vec![None; base.height as usize];
    for (k, r) in frames.iter().enumerate() {
        let held = r.stage("screen").unwrap();
        assert_eq!(
            held.rgba, base.rgba,
            "refresh {k}: the stages before scan ran once per sweep"
        );
        let scan = r.stage("scan").expect("scan slot ran");
        assert_eq!((scan.width, scan.height), (base.width, base.height));
        let rows = row_sums(scan);
        for (y, &lit) in rows.iter().enumerate() {
            if lit > 0.0 {
                assert!(first_lit[y].is_none(), "row {y} lit in refreshes {first_lit:?} and {k}");
                first_lit[y] = Some(k);
            }
        }
        // Where lit, five times the held light (FP16 store: one step in 1000).
        for y in 0..scan.height {
            for x in (0..scan.width).step_by(7) {
                let (a, b) = (scan.pixel(x, y), base.pixel(x, y));
                for c in 0..3 {
                    let want = if rows[y as usize] > 0.0 { 5.0 * b[c] } else { 0.0 };
                    assert!(
                        (a[c] - want).abs() <= 2e-3 * want.max(1.0),
                        "refresh {k} ({x}, {y}) {c}: {} vs {want}",
                        a[c]
                    );
                }
            }
        }
    }
    // Every row that holds light is lit once, and the bands follow each other down the picture.
    let mut last = 0;
    let mut counts = [0usize; 5];
    for (y, &r) in base_rows.iter().enumerate() {
        match (r > 0.0, first_lit[y]) {
            (true, Some(k)) => {
                assert!(k >= last, "row {y} lit at {k} after a band at {last}");
                last = k;
                counts[k] += 1;
            }
            (true, None) => panic!("row {y} holds light and is never lit"),
            (false, Some(k)) => panic!("row {y} holds no light and is lit at {k}"),
            (false, None) => {}
        }
    }
    let lit_rows: usize = counts.iter().sum();
    for (k, c) in counts.iter().enumerate() {
        assert!(
            (*c as f64 - lit_rows as f64 / 5.0).abs() <= 0.1 * lit_rows as f64 / 5.0 + 3.0,
            "band {k} has {c} of {lit_rows} lit rows: {counts:?}"
        );
    }
    // The mean of the five refreshes is the frame-integrated picture.
    let mean: f64 = frames.iter().map(|r| total(r.stage("scan").unwrap())).sum::<f64>() / 5.0;
    assert!(
        (mean - total(&base)).abs() <= 2e-3 * total(&base),
        "mean {mean} vs {}",
        total(&base)
    );
    drop(frames);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// A frame that arrives in the middle of a sweep is decoded at once and drawn by the next sweep only.
#[test]
fn a_new_frame_waits_for_the_next_sweep() {
    let Some(mut gpu) = rig("a_new_frame_waits_for_the_next_sweep", 240.0) else {
        return;
    };
    let counter = gpu.validation_counter();
    let (s, p) = (settings(240.0), params(&[("scan_mode", "rolling")]));
    let (dim, bright) = (flat(64), flat(192));
    let mut frames = Vec::new();
    for k in 0..8 {
        let src = match k {
            0 => Some(&dim[..]),
            2 => Some(&bright[..]),
            _ => None,
        };
        frames.push(refresh(&mut gpu, src, &s, &p));
    }
    let first = frames[0].stage("screen").unwrap().clone();
    for (k, f) in frames.iter().enumerate().take(4).skip(1) {
        assert_eq!(
            f.stage("screen").unwrap().rgba,
            first.rgba,
            "refresh {k} still draws the first frame"
        );
    }
    let native = |k: usize| frames[k].native.rgba.iter().map(|&v| f64::from(v)).sum::<f64>();
    assert!(native(1) == native(0), "refresh 1 decoded nothing new");
    assert!(
        native(2) > 1.5 * native(0),
        "the new frame is decoded as soon as it arrives"
    );
    let next = frames[4].stage("screen").unwrap();
    assert!(
        total(next) > 2.0 * total(&first),
        "the next sweep draws the new frame: {} vs {}",
        total(next),
        total(&first)
    );
    for (k, f) in frames.iter().enumerate().take(8).skip(5) {
        assert_eq!(
            f.stage("screen").unwrap().rgba,
            next.rgba,
            "refresh {k} holds the second frame"
        );
    }
    drop(frames);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// The strobe: the whole frame at the gain n / lit for the first `lit` refreshes, black for the others.
#[test]
fn a_strobe_shows_the_whole_frame_then_black() {
    let Some(mut gpu) = rig("a_strobe_shows_the_whole_frame_then_black", 240.0) else {
        return;
    };
    let counter = gpu.validation_counter();
    let (s, p) = (
        settings(240.0),
        params(&[("scan_mode", "strobe"), ("scan_lit_refreshes", "2")]),
    );
    let src = flat(100);
    let frames: Vec<Rendered> = (0..8)
        .map(|k| refresh(&mut gpu, (k == 0).then_some(&src[..]), &s, &p))
        .collect();
    let base = total(frames[0].stage("screen").unwrap());
    for (k, r) in frames.iter().enumerate() {
        let t = total(r.stage("scan").unwrap());
        if k % 4 < 2 {
            // lit: 4 refreshes per sweep, 2 lit: gain 2.
            assert!(
                (t - 2.0 * base).abs() <= 2e-3 * base * 2.0,
                "refresh {k}: {t} vs {}",
                2.0 * base
            );
        } else {
            assert_eq!(t, 0.0, "refresh {k} is dark");
        }
    }
    drop(frames);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Interlaced fields alternate per sweep (per source frame), not per refresh: with four refreshes per frame the field
/// of the held beam stages still flips from one sweep to the next, and stays the same inside a sweep.
#[test]
fn the_field_alternates_per_sweep() {
    let Some(mut gpu) = rig("the_field_alternates_per_sweep", 240.0) else {
        return;
    };
    let counter = gpu.validation_counter();
    let (s, p) = (
        settings(240.0),
        params(&[
            ("scan_mode", "rolling"),
            ("interlace_mode", "alternate_frames_are_fields"),
        ]),
    );
    let src = flat(128);
    let frames: Vec<Rendered> = (0..12)
        .map(|k| refresh(&mut gpu, (k == 0).then_some(&src[..]), &s, &p))
        .collect();
    let screen = |k: usize| &frames[k].stage("screen").unwrap().rgba;
    for k in 1..4 {
        assert_eq!(screen(k), screen(0), "refresh {k} is inside the first sweep");
    }
    assert_ne!(screen(4), screen(0), "the second sweep draws the other field");
    assert_eq!(screen(8), screen(0), "and the third the first again");
    for k in 5..8 {
        assert_eq!(screen(k), screen(4), "refresh {k} is inside the second sweep");
    }
    drop(frames);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// A screen that refreshes at the source rate has no sweep: the output is exactly the scan-off one.
#[test]
fn no_sweep_is_exactly_off() {
    let Some(mut gpu) = rig("no_sweep_is_exactly_off", 60.0) else {
        return;
    };
    let src = flat(90);
    let off = refresh(&mut gpu, Some(&src), &settings(60.0), &params(&[]));
    let on = refresh(
        &mut gpu,
        Some(&src),
        &settings(60.0),
        &params(&[("scan_mode", "rolling")]),
    );
    assert_eq!(
        on.output.rgba, off.output.rgba,
        "a rolling scan at 60 Hz for 60 Hz is not a scan"
    );
    assert_eq!(on.stage("scan").unwrap().rgba, off.stage("scan").unwrap().rgba);
    // And a sweep that exists changes the picture (so the comparison above can fail).
    let sweep = refresh(
        &mut gpu,
        Some(&src),
        &settings(240.0),
        &params(&[("scan_mode", "rolling")]),
    );
    assert_ne!(sweep.output.rgba, off.output.rgba);
}

/// With persistence the recurrence keeps the mean light of a still picture over the sweep: at its steady state the
/// mean over five refreshes of the phosphor output is the frame-integrated light.
#[test]
fn rolling_with_persistence_keeps_the_mean_light() {
    let Some(mut gpu) = rig("rolling_with_persistence_keeps_the_mean_light", 300.0) else {
        return;
    };
    let counter = gpu.validation_counter();
    let s = settings(300.0);
    let persist = [
        ("scan_mode", "rolling"),
        ("phosphor_persistence", "true"),
        ("phosphor_persistence_blend", "1"),
        ("phosphor_frame_rate_hz", "300"),
    ];
    let p = params(&persist);
    let src = flat(128);
    // The slow terms take a few seconds to settle (rho about 0.996 a refresh): 1500 refreshes, only the last five read.
    gpu.set_read_stages(StageReadback::None);
    for k in 0..1495 {
        refresh(&mut gpu, (k == 0).then_some(&src[..]), &s, &p);
    }
    gpu.set_read_stages(StageReadback::All);
    let tail: Vec<Rendered> = (0..5).map(|_| refresh(&mut gpu, None, &s, &p)).collect();
    let base = total(tail[0].stage("screen").unwrap());
    let mean = tail.iter().map(|r| total(r.stage("phosphor").unwrap())).sum::<f64>() / 5.0;
    // The recurrence's gain is 1 for a constant input and linear: the mean of the sweep is the picture's light, up to
    // the history not being fully settled and the FP16 stores.
    assert!(
        (mean - base).abs() <= 0.03 * base,
        "mean light over the sweep {mean} against the frame's {base}"
    );
    // The sweep shows in space: the total light of a refresh is the same (a fifth of the rows at five times), but red
    // has no afterglow to speak of (tau 0.44 ms against a 3.3 ms refresh), so at the first refresh of the sweep the
    // top fifth of the picture is lit and the bottom fifth, lit one refresh before, is already dark.
    let first = tail[0].stage("phosphor").unwrap();
    let red_rows = |y0: u32, y1: u32| -> f64 {
        (y0..y1)
            .map(|y| (0..first.width).map(|x| f64::from(first.pixel(x, y)[0])).sum::<f64>())
            .sum()
    };
    let fifth = first.height / 5;
    let (top, bottom) = (red_rows(0, fifth), red_rows(first.height - fifth, first.height));
    assert!(
        top > 5.0 * bottom,
        "red of the lit band {top} against the band lit one refresh earlier {bottom}"
    );
    drop(tail);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

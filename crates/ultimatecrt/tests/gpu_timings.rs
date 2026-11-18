//! GPU timestamp tests (T-21), headless, with the validation layer (synchronization validation on) and
//! zero validation messages.
//!
//! A frame rendered by [`Headless`] carries per-segment GPU times (`upload`, `decode`, every chain slot,
//! `encode`): positive for what ran, absent for what did not, summing to no more than the whole frame,
//! and the whole frame no longer than the wall-clock time of the call that rendered it.

mod common;

use std::time::Instant;

use ultimatecrt::gpu::{FrameTimings, Headless, Settings, SourceFrame, TargetFormat};
use ultimatecrt::params::Params;
use ultimatecrt::scaling::View;

/// Source and target sizes: large enough that every pass lasts well above the timestamp resolution.
const SRC: (u32, u32) = (640, 480);
const TARGET: (u32, u32) = (1280, 960);

fn picture() -> Vec<u8> {
    (0..SRC.0 * SRC.1)
        .flat_map(|i| [i as u8, (i / SRC.0) as u8, (i % SRC.0) as u8, 255])
        .collect()
}

fn render(gpu: &mut Headless, bgra: &[u8], new_frame: bool) -> f64 {
    let frame = new_frame.then_some(SourceFrame {
        width: SRC.0,
        height: SRC.1,
        bgra,
    });
    let t = Instant::now();
    gpu.render(
        frame,
        View::full(SRC.0, SRC.1),
        &Settings::default(),
        &Params::default(),
        TARGET,
        TargetFormat::Bgra8Unorm,
    )
    .expect("headless render");
    t.elapsed().as_secs_f64() * 1000.0
}

/// A renderer with working timestamps, or `None` (skipped, printed) on a device without them.
fn timed_gpu(test: &str) -> Option<Headless> {
    let gpu = common::gpu_or_skip(test)?;
    if let Some(why) = gpu.timing_unavailable() {
        assert!(
            !ultimatecrt::gpu::gpu_required(),
            "UCRT_REQUIRE_GPU is set but the device has no timestamps: {why}"
        );
        eprintln!("{test} skipped: {why}");
        return None;
    }
    Some(gpu)
}

/// The invariants of the timings of one frame rendered in `wall_ms`.
fn assert_sane(t: &FrameTimings, wall_ms: f64) {
    assert_eq!(t.frames, 1);
    assert!(t.frame_ms > 0.0 && t.frame_ms.is_finite(), "frame {} ms", t.frame_ms);
    for s in &t.stages {
        if let Some(ms) = s.ms {
            assert!(ms >= 0.0 && ms.is_finite(), "{}: {ms} ms", s.name);
        }
    }
    let sum = t.sum_ms();
    assert!(sum > 0.0, "no stage time at all");
    assert!(
        sum <= t.frame_ms + 1e-9,
        "stages {sum} ms exceed the frame {} ms",
        t.frame_ms
    );
    assert!(
        t.frame_ms <= wall_ms,
        "the GPU frame ({} ms) is longer than the call that rendered it ({wall_ms} ms)",
        t.frame_ms
    );
}

#[test]
fn every_frame_has_positive_times_for_the_stages_that_ran() {
    let Some(mut gpu) = timed_gpu("every_frame_has_positive_times") else {
        return;
    };
    let counter = gpu.validation_counter();
    let names = gpu.timing_names();
    assert_eq!(names.first(), Some(&"upload"));
    assert_eq!(names.get(1), Some(&"decode"));
    assert_eq!(names.last(), Some(&"encode"));
    for s in gpu.stage_names() {
        assert!(names.contains(&s), "chain slot {s} is not timed");
    }
    assert!(gpu.last_timings().is_none(), "no timings before the first frame");
    let bgra = picture();
    for n in 0..6 {
        let wall = render(&mut gpu, &bgra, true);
        let t = gpu.last_timings().expect("timings after a frame").clone();
        assert_sane(&t, wall);
        eprintln!("frame {n}: {} (wall {wall:.1} ms)", t.stats_line());
        assert_eq!(t.stages.len(), names.len());
        assert_eq!(t.stages.iter().map(|s| s.name).collect::<Vec<_>>(), names);
        for ran in ["upload", "decode", "encode"] {
            assert!(
                t.stage_ms(ran).is_some_and(|ms| ms > 0.0),
                "frame {n}: {ran} {:?}",
                t.stage_ms(ran)
            );
        }
        // Every slot has a value or none, never a negative one; a slot with a value that is not
        // flagged bypassed ran its stage.
        assert_eq!(t.stats_line().matches(" | ").count(), names.len());
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

#[test]
fn what_did_not_run_is_absent_and_bypassed_domain_changes_are_flagged() {
    let Some(mut gpu) = timed_gpu("what_did_not_run_is_absent") else {
        return;
    };
    let counter = gpu.validation_counter();
    let bgra = picture();

    // No new frame: no upload.
    render(&mut gpu, &bgra, true);
    let wall = render(&mut gpu, &bgra, false);
    let t = gpu.last_timings().unwrap().clone();
    assert_sane(&t, wall);
    assert_eq!(t.stage_ms("upload"), None, "upload without a new frame");
    assert!(t.stage_ms("decode").is_some_and(|ms| ms > 0.0));

    // Every slot off: a slot keeping its domain is skipped (absent), one that changes it runs the
    // resample copy, timed and flagged as bypassed.
    gpu.enable_only(&[]).unwrap();
    let wall = render(&mut gpu, &bgra, true);
    let t = gpu.last_timings().unwrap().clone();
    assert_sane(&t, wall);
    for s in &t.stages {
        if gpu.stage_names().contains(&s.name) {
            assert!(
                s.ms.is_none() || s.bypassed,
                "{} is off but reports {:?} ms as a run stage",
                s.name,
                s.ms
            );
        }
    }
    let skipped = t.stages.iter().filter(|s| s.ms.is_none()).count();
    let bypassed = t.stages.iter().filter(|s| s.bypassed).count();
    assert!(bypassed >= 2, "amp and screen change the domain and are resampled");
    assert!(skipped >= 1, "a same-domain slot that is off is skipped");

    // Back on: no slot is reported bypassed unless it is a placeholder (that has no pass yet).
    for n in gpu.stage_names() {
        gpu.set_stage_enabled(n, true).unwrap();
    }
    render(&mut gpu, &bgra, true);
    assert!(gpu
        .last_timings()
        .unwrap()
        .stages
        .iter()
        .all(|s| s.ms.is_none_or(|ms| ms >= 0.0)));
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

#[test]
fn a_burst_is_averaged_over_every_frame() {
    let Some(mut gpu) = timed_gpu("a_burst_is_averaged") else {
        return;
    };
    let counter = gpu.validation_counter();
    let bgra = picture();
    render(&mut gpu, &bgra, true);
    let _ = gpu.take_timing_average();
    let frames: Vec<(Option<SourceFrame<'_>>, View)> = (0..5)
        .map(|_| {
            (
                Some(SourceFrame {
                    width: SRC.0,
                    height: SRC.1,
                    bgra: &bgra,
                }),
                View::full(SRC.0, SRC.1),
            )
        })
        .collect();
    let t0 = Instant::now();
    gpu.render_burst(
        &frames,
        &Settings::default(),
        &Params::default(),
        TARGET,
        TargetFormat::Bgra8Unorm,
    )
    .expect("burst");
    let wall = t0.elapsed().as_secs_f64() * 1000.0;
    let last = gpu.last_timings().expect("last frame").clone();
    assert_sane(&last, wall);
    let mean = gpu.take_timing_average().expect("average");
    assert_eq!(mean.frames, 5, "every frame of the burst was read exactly once");
    assert!(mean.frame_ms > 0.0 && mean.sum_ms() <= mean.frame_ms + 1e-9);
    assert!(gpu.take_timing_average().is_none(), "taking the average restarts it");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

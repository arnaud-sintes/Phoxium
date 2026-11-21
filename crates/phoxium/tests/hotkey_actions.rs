//! Hotkey actions on the headless pipeline (T-54), with the validation layer and zero validation messages:
//! the A/B bypass disables every chain slot and restores them exactly, and preset next and previous give
//! renders that differ. The decisions themselves are unit tested in `controls.rs`; this checks them against
//! the real chain.

mod common;

use phoxium::{
    controls::{bypass_plan, step_preset, Step},
    gpu::{Headless, Rendered, Settings, SourceFrame, TargetFormat},
    params::Params,
    presets::Library,
    scaling::View,
};

const SRC: (u32, u32) = (96, 64);
const TARGET: (u32, u32) = (288, 192);

fn picture() -> Vec<u8> {
    (0..SRC.0 * SRC.1)
        .flat_map(|i| {
            let (x, y) = (i % SRC.0, i / SRC.0);
            [
                (140 + (x * 3 + y) % 116) as u8,
                (120 + (x + y * 5) % 136) as u8,
                (100 + (x * 7) % 156) as u8,
                255,
            ]
        })
        .collect()
}

fn run(gpu: &mut Headless, bgra: &[u8], p: &Params, upload: bool) -> Rendered {
    let mut last = None;
    for i in 0..3 {
        last = Some(
            gpu.render(
                (upload && i == 0).then_some(SourceFrame {
                    width: SRC.0,
                    height: SRC.1,
                    bgra,
                }),
                View::full(SRC.0, SRC.1),
                &Settings::default(),
                p,
                TARGET,
                TargetFormat::Rgba32F,
            )
            .expect("render"),
        );
    }
    last.expect("frames")
}

fn apply(gpu: &mut Headless, flags: &[(&'static str, bool)]) {
    for &(name, on) in flags {
        gpu.set_stage_enabled(name, on).expect("a chain slot");
    }
}

#[test]
fn the_bypass_turns_every_chain_slot_off_and_back_to_the_same_picture() {
    let Some(mut gpu) = common::gpu_or_skip("the_bypass_turns_every_chain_slot_off") else {
        return;
    };
    let counter = gpu.validation_counter();
    let bgra = picture();
    let params = Params::default();
    let full = run(&mut gpu, &bgra, &params, true);
    let full_passes = gpu.last_chain_passes();

    let current: Vec<(&'static str, bool)> = gpu.stage_names().into_iter().map(|n| (n, true)).collect();
    assert!(current.len() >= 6, "the chain has its slots: {current:?}");
    let mut saved = None;
    let off = bypass_plan(true, &current, &mut saved);
    assert!(off.iter().all(|&(_, on)| !on), "all chain slots disabled: {off:?}");
    apply(&mut gpu, &off);
    let bypassed = run(&mut gpu, &bgra, &params, false);
    assert_ne!(
        full.output.rgba, bypassed.output.rgba,
        "the bypassed picture is not the CRT picture"
    );
    // Only the resample copies between image domains remain (never a stage pass): fewer than the CRT chain.
    assert!(
        gpu.last_chain_passes() < full_passes,
        "bypassed {} passes, CRT chain {full_passes}",
        gpu.last_chain_passes()
    );

    let on = bypass_plan(false, &off, &mut saved);
    assert_eq!(on, current, "everything was on before, everything is on again");
    apply(&mut gpu, &on);
    let restored = run(&mut gpu, &bgra, &params, false);
    assert_eq!(
        full.output.rgba, restored.output.rgba,
        "bypass off gives back exactly the CRT picture"
    );
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

#[test]
fn preset_next_and_previous_render_differently_and_undo_each_other() {
    let Some(mut gpu) = common::gpu_or_skip("preset_next_and_previous_render_differently") else {
        return;
    };
    let counter = gpu.validation_counter();
    let bgra = picture();
    let lib = Library::embedded();
    let a = step_preset(&lib, None, Step::Next, &Params::default()).expect("first preset");
    let b = step_preset(&lib, Some(&a.id), Step::Next, &a.params).expect("second preset");
    assert_ne!(a.id, b.id);
    assert_ne!(a.params, b.params);
    let ra = run(&mut gpu, &bgra, &a.params, true);
    let rb = run(&mut gpu, &bgra, &b.params, false);
    assert_ne!(ra.output.rgba, rb.output.rgba, "two presets give two pictures");
    // Previous from the second is the first again, with the same parameters.
    let back = step_preset(&lib, Some(&b.id), Step::Previous, &b.params).expect("previous");
    assert_eq!((back.id.as_str(), back.params), (a.id.as_str(), a.params));
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

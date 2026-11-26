//! The phosphor-type selector through the GPU chain (`phosphor_type`, QUESTIONS PHY-19, CRT.md "Phosphor types").
//!
//! - P22, the default, renders exactly what it rendered before the selector: [`p22_sequence_hash`] prints the hash
//!   of a 40-frame moving-bar sequence through the whole chain (persistence on, 60 Hz), compared by hand with the
//!   same sequence on the branch before the selector (recorded in QUESTIONS PHY-19).
//! - A monochrome type drives the three channels with the BT.601 luma of the source ([`mono_drive_is_the_luma`]),
//!   shows the type's chromaticity (the colour matrix of `phosphor_type::mono_output_matrix`, with the existing gamut
//!   clip of `encode`: [`mono_light_has_the_type_colour`]) and its trail follows the oracle fit of its data row
//!   ([`mono_trail_follows_the_oracle`], the single-excitation method of `tests/persistence_chain.rs`).
//! - With a source colour target on, `decode` keeps the colour and the `target` slot takes the luma of its own output
//!   ([`mono_cpc_green_target_acts_on_the_colour`], [`mono_c64_target_keeps_five_colours_apart`]); without a target nothing
//!   changed ([`mono_without_a_target_is_unchanged`]).

mod common;

use crt_model::colour::mul3;
use crt_model::phosphor::{blended_trail_ratio, DecayControls, TRAIL_HORIZON_MS};
use crt_model::phosphor_types::{mono_drive, MONO_TYPES};
use crt_model::target::{Matrix, Transform};
use phoxium::gpu::{Headless, ImageF32, Settings, SourceFrame, StageReadback, TargetFormat};
use phoxium::params::{
    Params, SOURCE_COLOUR_TARGET_AMSTRAD_CPC_GREEN_SCREEN, SOURCE_COLOUR_TARGET_COMMODORE_64, SOURCE_COLOUR_TARGET_NONE,
};
use phoxium::phosphor_type::mono_output_matrix;
use phoxium::scaling::View;

const SRC: (u32, u32) = (320, 240);
const TARGET: (u32, u32) = (960, 720);
const BAR: u32 = 12;

/// The `phosphor_type` index of a monochrome type (its place after p22).
fn index(i: usize) -> u32 {
    i as u32 + 1
}

/// A black BGRA source with a white bar of [`BAR`] columns starting at column `x` (`None`: all black).
fn bar(x: Option<u32>) -> Vec<u8> {
    let mut v = vec![0u8; (SRC.0 * SRC.1 * 4) as usize];
    for row in 0..SRC.1 {
        for col in 0..SRC.0 {
            let i = ((row * SRC.0 + col) * 4) as usize;
            let lit = x.is_some_and(|x| col >= x && col < x + BAR);
            v[i..i + 4].copy_from_slice(&if lit { [255; 4] } else { [0, 0, 0, 255] });
        }
    }
    v
}

fn render(gpu: &mut Headless, p: &Params, settings: &Settings, data: &[u8]) -> phoxium::gpu::Rendered {
    gpu.render(
        Some(SourceFrame {
            width: SRC.0,
            height: SRC.1,
            bgra: data,
        }),
        View::full(SRC.0, SRC.1),
        settings,
        p,
        TARGET,
        TargetFormat::Rgba32F,
    )
    .expect("render")
}

fn sums(img: &ImageF32) -> [f64; 3] {
    let mut s = [0.0; 3];
    for t in img.rgba.as_chunks::<4>().0 {
        for c in 0..3 {
            s[c] += f64::from(t[c]);
        }
    }
    s
}

/// FNV-1a over the bits of every value of `img`.
fn hash(h: &mut u64, img: &ImageF32) {
    for v in &img.rgba {
        for b in v.to_bits().to_le_bytes() {
            *h ^= u64::from(b);
            *h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
}

/// The P22 default through the whole chain: the hash of 40 output frames of a moving bar (persistence on, real
/// time at 60 Hz), for the defaults and with `phosphor_type` set explicitly to p22 (identical). The value is
/// compared by hand with the branch before the selector (QUESTIONS PHY-19); here only the two runs must agree.
#[test]
fn p22_sequence_hash() {
    let Some(mut gpu) = common::gpu_or_skip("phosphor types p22 hash") else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.set_read_stages(StageReadback::Last);
    let mut p = Params {
        phosphor_persistence: true,
        ..Params::default()
    };
    let run = |gpu: &mut Headless, p: &Params| {
        gpu.set_frame_time_ms(Some(1000.0 / 60.0));
        gpu.clear_history();
        let mut h = 0xcbf2_9ce4_8422_2325_u64;
        for k in 0..40 {
            let r = render(gpu, p, &Settings::default(), &bar(Some((k * 7) % (SRC.0 - BAR))));
            hash(&mut h, &r.output);
        }
        h
    };
    let a = run(&mut gpu, &p);
    p.phosphor_type = phoxium::params::PHOSPHOR_TYPE_P22;
    let b = run(&mut gpu, &p);
    println!("p22 40-frame output hash: {a:016x}");
    assert_eq!(a, b);
    gpu.set_frame_time_ms(None);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Patches of pure red, green, blue, white and two greys (8-bit codes), one per column band.
const PATCHES: [[u8; 3]; 6] = [
    [255, 0, 0],
    [0, 255, 0],
    [0, 0, 255],
    [255, 255, 255],
    [128, 128, 128],
    [64, 160, 32],
];

fn patches() -> Vec<u8> {
    let mut v = vec![0u8; (SRC.0 * SRC.1 * 4) as usize];
    let band = SRC.0 / PATCHES.len() as u32;
    for row in 0..SRC.1 {
        for col in 0..SRC.0 {
            let c = PATCHES[((col / band) as usize).min(PATCHES.len() - 1)];
            let i = ((row * SRC.0 + col) * 4) as usize;
            v[i..i + 4].copy_from_slice(&[c[2], c[1], c[0], 255]);
        }
    }
    v
}

/// The decode stage of a monochrome type writes the BT.601 luma of the code values to all three channels (within
/// the FP16 rounding of the native raster); P22 keeps the channels.
#[test]
fn mono_drive_is_the_luma() {
    let Some(mut gpu) = common::gpu_or_skip("phosphor types luma") else {
        return;
    };
    let counter = gpu.validation_counter();
    let src = patches();
    let band = SRC.0 / PATCHES.len() as u32;
    for (i, t) in MONO_TYPES.iter().enumerate() {
        let mut p = Params::identity_gun();
        p.phosphor_type = index(i);
        let r = render(&mut gpu, &p, &Settings::identity(), &src);
        for (k, c) in PATCHES.iter().enumerate() {
            let x = k as u32 * band + band / 2;
            let o = ((SRC.1 / 2 * SRC.0 + x) * 4) as usize;
            let px = &r.native.rgba[o..o + 3];
            let want = mono_drive(c.map(|v| f64::from(v) / 255.0));
            for &v in px {
                assert!(
                    (f64::from(v) - want).abs() <= want * 1e-3 + 1e-6,
                    "{} patch {c:?}: {px:?} vs {want}",
                    t.id
                );
            }
        }
    }
    let r = render(&mut gpu, &Params::identity_gun(), &Settings::identity(), &src);
    let o = ((SRC.1 / 2 * SRC.0 + band / 2) * 4) as usize;
    assert_eq!(&r.native.rgba[o..o + 3], &[1.0, 0.0, 0.0], "p22 keeps red");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// The display light of a monochrome type: the last chain image (tube domain) has three equal channels, and the
/// output is the colour matrix of the type applied to it, a negative display channel clipped to 0 (the existing
/// gamut handling, counted as out of gamut): the positive channels keep the matrix's ratios. The chromaticity before
/// the clip is checked exactly in `phosphor_type::tests::mono_white_has_the_type_chromaticity`.
#[test]
fn mono_light_has_the_type_colour() {
    let Some(mut gpu) = common::gpu_or_skip("phosphor types colour") else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.set_read_stages(StageReadback::Last);
    // A dark grey keeps every channel under the output shoulder's knee.
    let grey = vec![40u8; (SRC.0 * SRC.1 * 4) as usize];
    for (i, t) in MONO_TYPES.iter().enumerate() {
        let p = Params {
            phosphor_type: index(i),
            ..Params::default()
        };
        let r = render(&mut gpu, &p, &Settings::default(), &grey);
        let (_, last) = r.stages.last().expect("chain image");
        let s = sums(last);
        assert!(s[0] > 0.0, "{}: lit", t.id);
        for c in 1..3 {
            assert!((s[c] / s[0] - 1.0).abs() < 1e-6, "{}: tube channels equal {s:?}", t.id);
        }
        let m = mono_output_matrix(t.xy).unwrap();
        let want = mul3(&m, [1.0; 3]);
        let negative = want.iter().any(|v| *v < 0.0);
        assert_eq!(
            r.clip.out_of_gamut > 0,
            negative,
            "{}: out of gamut {:?} {want:?}",
            t.id,
            r.clip
        );
        // Centre pixel of the output, linear display light (the default host transfer, a pure 2.2 power, undone).
        let o = ((TARGET.1 / 2 * TARGET.0 + TARGET.0 / 2) * 4) as usize;
        let lin: Vec<f64> = r.output.rgba[o..o + 3]
            .iter()
            .map(|&v| f64::from(v).max(0.0).powf(2.2))
            .collect();
        let (big, _) = want
            .iter()
            .enumerate()
            .fold((0, f64::MIN), |a, (k, &v)| if v > a.1 { (k, v) } else { a });
        for c in 0..3 {
            if want[c] <= 0.0 {
                assert!(lin[c] < 1e-3, "{}: clipped channel {c} = {}", t.id, lin[c]);
            } else {
                let (got, exp) = (lin[c] / lin[big], want[c] / want[big]);
                assert!(
                    (got - exp).abs() < 0.02,
                    "{}: channel {c} ratio {got} vs {exp} ({lin:?})",
                    t.id
                );
            }
        }
        println!("{}: tube->display {want:?}, output linear {lin:?}", t.id);
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Single excitation through the whole chain (method of `tests/persistence_chain.rs`): for every monochrome type,
/// the summed trail of frames 1 to 3 after a bar, per channel, equals the oracle of the type's fit (all three
/// channels the same) at 60 Hz, at the full trail (blend 1) and at the default blend 0.5.
#[test]
fn mono_trail_follows_the_oracle() {
    let Some(mut gpu) = common::gpu_or_skip("phosphor types trail") else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.set_read_stages(StageReadback::Last);
    let (black, lit) = (bar(None), bar(Some(SRC.0 / 2)));
    let t = 1000.0 / 60.0;
    let mut bad = Vec::new();
    for (i, ty) in MONO_TYPES.iter().enumerate() {
        for blend in [1.0_f32, 0.5] {
            let p = Params {
                phosphor_type: index(i),
                phosphor_persistence: true,
                phosphor_persistence_blend: blend,
                phosphor_frame_rate_hz: 60.0,
                ..Params::default()
            };
            gpu.set_frame_time_ms(Some(t as f32));
            gpu.clear_history();
            let s = Settings::default();
            let _ = render(&mut gpu, &p, &s, &black);
            let lb = sums(&r_last(&render(&mut gpu, &p, &s, &black)));
            let l0 = sums(&r_last(&render(&mut gpu, &p, &s, &lit)));
            let lk: Vec<[f64; 3]> = (0..3)
                .map(|_| sums(&r_last(&render(&mut gpu, &p, &s, &black))))
                .collect();
            let fb = ty.fit.controlled_feedback(t, TRAIL_HORIZON_MS, DecayControls::PHYSICAL);
            let oracle: Vec<f64> = ty
                .fit
                .controlled_trail_ratios(t, TRAIL_HORIZON_MS, DecayControls::PHYSICAL)
                .into_iter()
                .take(3)
                .map(|r| blended_trail_ratio(fb.w0, r, f64::from(blend)))
                .collect();
            for ch in 0..3 {
                let got: Vec<f64> = lk.iter().map(|l| (l[ch] - lb[ch]) / (l0[ch] - lb[ch])).collect();
                for k in 0..3 {
                    let ok = if oracle[k] > 1e-4 {
                        ((got[k] - oracle[k]) / oracle[k]).abs() < 0.01
                    } else {
                        got[k].abs() < 1e-4
                    };
                    if !ok {
                        bad.push(format!(
                            "{} blend {blend} ch {ch} k {}: {} vs {}",
                            ty.id,
                            k + 1,
                            got[k],
                            oracle[k]
                        ));
                    }
                }
                if ch == 1 {
                    println!("{} blend {blend}: trail {got:.4?} (oracle {oracle:.4?})", ty.id);
                }
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
    gpu.set_frame_time_ms(None);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

fn r_last(r: &phoxium::gpu::Rendered) -> ImageF32 {
    r.stages.last().expect("chain image").1.clone()
}

// ---------------------------------------------------------------------------------------------------------------
// A monochrome phosphor type with a source colour target (`decode` keeps the colour while a target is on, the `target`
// slot takes the BT.601 luma of its own output; `shaders/decode.comp`, `shaders/target.comp`, oracle
// `crt_model::target::Transform::apply_mono`).
// ---------------------------------------------------------------------------------------------------------------

/// The Amstrad CPC green monitor (variant 1, 27 linear steps) written here independently of the data file: the grey
/// (9 R + 3 G + B) / 13 on all three channels.
fn cpc_green() -> Transform {
    Transform::Matrix(Matrix {
        rows: [[9.0, 3.0, 1.0]; 3],
        input_bits: 8,
        input_range: 1.0,
        input_gamma: 1.0,
        clamp: f64::INFINITY,
        divisor: 13.0,
        output_gamma: 1.0,
        output_scale: 1.0,
    })
}

/// The pixel at the centre of band `k` of `n` bands, mid height.
fn band_centre(k: usize, n: usize) -> (u32, u32) {
    let band = SRC.0 / n as u32;
    (k as u32 * band + band / 2, SRC.1 / 2)
}

/// A picture of side-by-side bands, one per colour (8-bit codes).
fn bands(colours: &[[u8; 3]]) -> Vec<u8> {
    let mut v = vec![0u8; (SRC.0 * SRC.1 * 4) as usize];
    let band = SRC.0 / colours.len() as u32;
    for row in 0..SRC.1 {
        for col in 0..SRC.0 {
            let c = colours[((col / band) as usize).min(colours.len() - 1)];
            let i = ((row * SRC.0 + col) * 4) as usize;
            v[i..i + 4].copy_from_slice(&[c[2], c[1], c[0], 255]);
        }
    }
    v
}

/// With a monochrome phosphor and no target the decode raster is the luma exactly as before (`mono_drive_is_the_luma`),
/// the target slot is skipped, and the whole chain is deterministic: the hash of the output and native raster of every
/// monochrome type is printed to be compared by hand with the same run on the commit before the change (recorded in
/// QUESTIONS, "A colour target on a monochrome phosphor").
#[test]
fn mono_without_a_target_is_unchanged() {
    let Some(mut gpu) = common::gpu_or_skip("phosphor types mono no target") else {
        return;
    };
    let counter = gpu.validation_counter();
    let src = patches();
    let band = SRC.0 / PATCHES.len() as u32;
    let mut total = 0xcbf2_9ce4_8422_2325_u64;
    for (i, t) in MONO_TYPES.iter().enumerate() {
        let p = Params {
            phosphor_type: index(i),
            ..Params::default()
        };
        let a = render(&mut gpu, &p, &Settings::default(), &src);
        assert!(
            gpu.last_skipped_slots().contains(&"target"),
            "{}: no target, slot skipped",
            t.id
        );
        let b = render(&mut gpu, &p, &Settings::default(), &src);
        assert_eq!(a.output.rgba, b.output.rgba, "{}: deterministic", t.id);
        // The decode raster: the luma of the codes in the three channels.
        for (k, c) in PATCHES.iter().enumerate() {
            let px = a.native.pixel(k as u32 * band + band / 2, SRC.1 / 2);
            let want = mono_drive(c.map(|v| f64::from(v) / 255.0));
            for &v in &px[..3] {
                assert!(
                    (f64::from(v) - want).abs() <= want * 1e-3 + 1e-6,
                    "{} patch {c:?}: {px:?} vs {want}",
                    t.id
                );
            }
        }
        hash(&mut total, &a.output);
        hash(&mut total, &a.native);
    }
    println!("mono, no target, all five types: output and native hash {total:016x}");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// The CPC green monitor on a monochrome phosphor acts on the COLOUR picture: the decode raster keeps the colour, the target
/// writes the 9 : 3 : 1 grey (full red three times full green, unlike the BT.601 order where green is the heavier), the chain
/// after it still carries three equal channels, and every checked pixel of the target image is the oracle's `apply_mono`
/// within the usual budget (FP16 store on f32 arithmetic, 1e-3).
#[test]
fn mono_cpc_green_target_acts_on_the_colour() {
    let Some(mut gpu) = common::gpu_or_skip("phosphor types mono cpc green") else {
        return;
    };
    let counter = gpu.validation_counter();
    let colours: [[u8; 3]; 7] = [
        [255, 0, 0],
        [0, 255, 0],
        [0, 0, 255],
        [255, 255, 255],
        [128, 128, 128],
        [255, 255, 0],
        [0, 255, 255],
    ];
    let src = bands(&colours);
    let t = cpc_green();
    for (i, ty) in MONO_TYPES.iter().enumerate() {
        let p = Params {
            phosphor_type: index(i),
            source_colour_target: SOURCE_COLOUR_TARGET_AMSTRAD_CPC_GREEN_SCREEN,
            source_colour_variant: 1,
            ..Params::identity_gun()
        };
        let r = render(&mut gpu, &p, &Settings::identity(), &src);
        assert!(
            !gpu.last_skipped_slots().contains(&"target"),
            "{}: the slot runs",
            ty.id
        );
        let target = r.stage("target").expect("target image");
        for (k, c) in colours.iter().enumerate() {
            let (x, y) = band_centre(k, colours.len());
            // decode keeps the colour while a target is on.
            let native = r.native.pixel(x, y);
            for ch in 0..3 {
                assert!(
                    (f64::from(native[ch]) - f64::from(c[ch]) / 255.0).abs() <= 1e-3,
                    "{} {c:?}: decode raster {native:?}",
                    ty.id
                );
            }
            // The target's output: the oracle's grey on three equal channels.
            let want = t.apply_mono(c.map(|v| f64::from(v) / 255.0));
            let px = target.pixel(x, y);
            for ch in 0..3 {
                assert!(
                    (f64::from(px[ch]) - want[ch]).abs() <= 1e-3,
                    "{} {c:?}: target {px:?} vs {want:?}",
                    ty.id
                );
            }
        }
        // The 9 : 3 : 1 order, not the BT.601 one: red 9/13, green 3/13, blue 1/13.
        let grey = |k: usize| {
            let (x, y) = band_centre(k, colours.len());
            f64::from(target.pixel(x, y)[0])
        };
        assert!(
            (grey(0) / grey(1) - 3.0).abs() < 0.01,
            "{}: red / green {}",
            ty.id,
            grey(0) / grey(1)
        );
        assert!(
            (grey(1) / grey(2) - 3.0).abs() < 0.02,
            "{}: green / blue {}",
            ty.id,
            grey(1) / grey(2)
        );
        assert!((grey(3) - 1.0).abs() < 1e-3, "{}: white stays white", ty.id);
        // The chain after the target still has three equal channels (the one-gun tube).
        let (_, last) = r.stages.last().expect("chain image");
        for k in 0..colours.len() {
            let (x, y) = band_centre(k, colours.len());
            let px = last.pixel(x, y);
            for ch in 1..3 {
                assert!(
                    (f64::from(px[ch]) - f64::from(px[0])).abs() <= 1e-5 * f64::from(px[0]).max(1.0),
                    "{} band {k}: last stage {px:?}",
                    ty.id
                );
            }
        }
        // The target is not a no-op any more: on a colour picture it differs from the same tube with no target.
        let off = render(
            &mut gpu,
            &Params {
                source_colour_target: SOURCE_COLOUR_TARGET_NONE,
                ..p
            },
            &Settings::identity(),
            &src,
        );
        assert_ne!(
            off.output.rgba, r.output.rgba,
            "{}: the target changes the picture",
            ty.id
        );
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// The Commodore 64 on a monochrome phosphor: five colours of different hardware luma (red, blue, orange, light red, cyan) come
/// out as five different greys, the BT.601 luma of the hardware colours the target puts, within the usual budget of the
/// oracle. (With the luma first, as before the change, every colour was classified on the grey of its own luma and red, blue,
/// orange, brown and dark grey all landed on one dark grey.) On P22 the target colours are unchanged, with unequal channels.
#[test]
fn mono_c64_target_keeps_five_colours_apart() {
    let Some(mut gpu) = common::gpu_or_skip("phosphor types mono c64") else {
        return;
    };
    let counter = gpu.validation_counter();
    let base = Params {
        source_colour_target: SOURCE_COLOUR_TARGET_COMMODORE_64,
        source_colour_variant: 1,
        ..Params::identity_gun()
    };
    let target = phoxium::targets::for_value(base.source_colour_target).expect("the C64 target");
    let Transform::Signal(sig) = target.select(base.source_colour_variant) else {
        panic!("signal target")
    };
    // The C64's red, blue, orange, light red and cyan (palette indexes 2, 6, 8, 10, 3), as an emulator shows them: the
    // reference colours, quantised to the 8-bit codes of the frame. Five colours of five different hardware luma levels
    // (the VIC-II gives several colours the same luma, red and dark grey, blue and brown, orange and purple..., so those
    // pairs are one grey on a one-gun tube by design; the oracle's `apply_mono` says which).
    let picks = [2usize, 6, 8, 10, 3];
    let codes: Vec<[u8; 3]> = picks
        .iter()
        .map(|&i| sig.reference(i).map(|v| (v * 255.0).round() as u8))
        .collect();
    let src = bands(&codes);
    let t = Transform::Signal(sig);
    let wants: Vec<f64> = codes
        .iter()
        .map(|c| t.apply_mono(c.map(|v| f64::from(v) / 255.0))[0])
        .collect();
    for (i, ty) in MONO_TYPES.iter().enumerate() {
        let p = Params {
            phosphor_type: index(i),
            ..base
        };
        let r = render(&mut gpu, &p, &Settings::identity(), &src);
        let img = r.stage("target").expect("target image");
        let got: Vec<[f32; 4]> = (0..codes.len())
            .map(|k| {
                let (x, y) = band_centre(k, codes.len());
                img.pixel(x, y)
            })
            .collect();
        for (k, px) in got.iter().enumerate() {
            for ch in 0..3 {
                assert!(
                    (f64::from(px[ch]) - wants[k]).abs() <= 1e-3,
                    "{} colour {}: {px:?} vs {}",
                    ty.id,
                    picks[k],
                    wants[k]
                );
            }
        }
        // Five different greys (at least a few codes apart), as the oracle says.
        for a in 0..got.len() {
            for b in a + 1..got.len() {
                assert!(
                    (f64::from(got[a][0]) - f64::from(got[b][0])).abs() > 0.01,
                    "{}: colours {} and {} on one grey {:?} {:?}",
                    ty.id,
                    picks[a],
                    picks[b],
                    got[a],
                    got[b]
                );
            }
        }
    }
    // P22: the colour target as before: the hardware colours of the oracle, unequal channels.
    let r = render(&mut gpu, &base, &Settings::identity(), &src);
    let img = r.stage("target").expect("target image");
    for (k, c) in codes.iter().enumerate() {
        let (x, y) = band_centre(k, codes.len());
        let px = img.pixel(x, y);
        let want = t.apply(c.map(|v| f64::from(v) / 255.0));
        for ch in 0..3 {
            assert!(
                (f64::from(px[ch]) - want[ch]).abs() <= 1e-3,
                "p22 colour {}: {px:?} vs {want:?}",
                picks[k]
            );
        }
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

//! Whole-chain GPU cost per stage (performance work, PLAN T-48 / T-93), headless, no window.
//!
//! `chain_profile` renders 320 x 240 sources through the full chain (as the live path: `Settings::default()`,
//! host 2.2, a `B8G8R8A8_UNORM` target) at three output sizes, for several tube configurations and contents,
//! and prints the per-stage GPU milliseconds (median, and the spread p10..p90, of per-burst means). Every
//! scenario runs once per round and the rounds are interleaved, so a clock change on the shared GPU hits all
//! scenarios alike. Printed, not asserted. Run with
//! `cargo test --release -p ultimatecrt --test perf_profile -- --ignored --nocapture`.
//! Filters (comma lists of prefixes): `UCRT_PROFILE_SIZES` (e.g. `1920`), `UCRT_PROFILE_CONFIGS`,
//! `UCRT_PROFILE_CONTENT`; `UCRT_PROFILE_ROUNDS` (default 9), `UCRT_PROFILE_BURST` (frames per burst, default
//! 6); `UCRT_PROFILE_SET` (`id=value;id=value`, applied to every configuration).

use ultimatecrt::gpu::{Headless, SourceFrame, StageReadback, TargetFormat};
use ultimatecrt::params::Params;
use ultimatecrt::patterns::{generate, Pattern};
use ultimatecrt::render_cmd::{resolve_preset_in, set_param};
use ultimatecrt::scaling::View;

const W: u32 = 320;
const H: u32 = 240;

fn hash(a: u32, b: u32) -> u32 {
    let mut v = a.wrapping_mul(0x9E37_79B1) ^ b.wrapping_mul(0x85EB_CA77) ^ 0x1234_5678;
    v ^= v >> 15;
    v = v.wrapping_mul(0x2C1B_3C6D);
    v ^= v >> 12;
    v = v.wrapping_mul(0x297A_2D39);
    v ^ (v >> 15)
}

/// A game-like pixel-art frame (BGRA): banded sky, brick ground of 16 px tiles in a 16-colour palette,
/// sprites (the scene of `tests/stage_beam_fast.rs`).
fn game(w: u32, h: u32) -> Vec<u8> {
    const PAL: [[u8; 3]; 16] = [
        [0, 0, 0],
        [29, 43, 83],
        [126, 37, 83],
        [0, 135, 81],
        [171, 82, 54],
        [95, 87, 79],
        [194, 195, 199],
        [255, 241, 232],
        [255, 0, 77],
        [255, 163, 0],
        [255, 236, 39],
        [0, 228, 54],
        [41, 173, 255],
        [131, 118, 156],
        [255, 119, 168],
        [255, 204, 170],
    ];
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let c = if y < h / 3 {
                let band = (y / 8) % 3;
                let dither = y % 8 == 7 && (x + y) % 2 == 0;
                PAL[[1, 12, 6][((band + u32::from(dither)) % 3) as usize]]
            } else {
                let (tx, ty) = (x / 16, y / 16);
                let t = hash(tx, ty);
                let (lx, ly) = (x % 16, y % 16);
                if t.is_multiple_of(5) && (4..12).contains(&lx) && (4..12).contains(&ly) {
                    let (sx, sy) = (lx - 4, ly - 4);
                    let bit = |sx: u32, sy: u32| (hash(t, sy * 4 + sx.min(7 - sx)) & 1) == 1;
                    if bit(sx, sy) {
                        PAL[[8, 10, 14, 11][(t >> 8) as usize % 4]]
                    } else if (sx > 0 && bit(sx - 1, sy)) || (sx < 7 && bit(sx + 1, sy)) {
                        PAL[0]
                    } else {
                        PAL[[4, 5, 13, 3][(t >> 4) as usize % 4]]
                    }
                } else if ly % 8 == 0 || (lx + 8 * ((ly / 8) % 2)) % 16 == 0 {
                    PAL[5]
                } else {
                    PAL[[4, 5, 13, 3][(t >> 4) as usize % 4]]
                }
            };
            out.extend_from_slice(&[c[2], c[1], c[0], 255]);
        }
    }
    out
}

/// Uniform noise per pixel and channel (BGRA).
fn noise(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let v = hash(x, y);
            out.extend_from_slice(&[v as u8, (v >> 8) as u8, (v >> 16) as u8, 255]);
        }
    }
    out
}

/// The contents: (name, BGRA frame).
fn contents() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("game", game(W, H)),
        ("checker1", generate(&Pattern::Checker(1), W, H, 0)),
        ("noise", noise(W, H)),
        ("ramp", generate(&Pattern::Ramp, W, H, 0)),
        ("flat", generate(&Pattern::Flat(160), W, H, 0)),
    ]
}

/// The tube configurations: (name, parameters).
fn configs(extra: &[(String, String)]) -> Vec<(&'static str, Params)> {
    let preset = |id: &str| resolve_preset_in(id, None).expect("shipped preset").0.params();
    let mut flat01 = preset("pvm-20m4u");
    set_param(&mut flat01, "geometry_curvature", "flat").unwrap();
    let mut v = vec![
        ("defaults", Params::default()),
        ("01 flat", flat01),
        ("01", preset("pvm-20m4u")),
        ("05", preset("slot-tv-26in")),
    ];
    for (_, p) in &mut v {
        for (id, val) in extra {
            set_param(p, id, val).unwrap_or_else(|e| panic!("UCRT_PROFILE_SET {id}={val}: {e}"));
        }
    }
    v
}

fn keep(filter: &str, name: &str) -> bool {
    filter.is_empty() || filter.split(',').any(|f| name.starts_with(f.trim()))
}

fn pct(v: &[f64], p: f64) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    s[((s.len() - 1) as f64 * p).round() as usize]
}

#[test]
#[ignore = "profile: run explicitly, prints a table"]
fn chain_profile() {
    let mut gpu = Headless::new(false).expect("headless GPU");
    if let Some(why) = gpu.timing_unavailable() {
        eprintln!("chain_profile skipped: {why}");
        return;
    }
    gpu.set_read_stages(StageReadback::None);
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    let rounds: usize = env("UCRT_PROFILE_ROUNDS").parse().unwrap_or(9);
    let burst: usize = env("UCRT_PROFILE_BURST").parse().unwrap_or(6);
    let extra: Vec<(String, String)> = env("UCRT_PROFILE_SET")
        .split(';')
        .filter_map(|kv| {
            kv.split_once('=')
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        })
        .collect();
    let sizes: Vec<(u32, u32)> = [(1280, 720), (1920, 1080), (2560, 1440)]
        .into_iter()
        .filter(|s| keep(&env("UCRT_PROFILE_SIZES"), &s.0.to_string()))
        .collect();
    let configs: Vec<_> = configs(&extra)
        .into_iter()
        .filter(|(n, _)| keep(&env("UCRT_PROFILE_CONFIGS"), n))
        .collect();
    let contents: Vec<_> = contents()
        .into_iter()
        .filter(|(n, _)| keep(&env("UCRT_PROFILE_CONTENT"), n))
        .collect();
    let names = gpu.timing_names();
    struct Run {
        label: String,
        size: (u32, u32),
        cfg: usize,
        content: usize,
        frame: Vec<f64>,
        stages: Vec<Vec<Option<f64>>>,
    }
    let mut runs = Vec::new();
    for &size in &sizes {
        for (ci, (cn, _)) in configs.iter().enumerate() {
            for (ki, (kn, _)) in contents.iter().enumerate() {
                runs.push(Run {
                    label: format!("{}x{} | {cn} | {kn}", size.0, size.1),
                    size,
                    cfg: ci,
                    content: ki,
                    frame: Vec::new(),
                    stages: vec![Vec::new(); names.len()],
                });
            }
        }
    }
    println!(
        "chain_profile on {}: {} scenarios, {rounds} rounds of {burst}-frame bursts, extra {extra:?}",
        gpu.context().gpu.name,
        runs.len()
    );
    let view = View::full(W, H);
    let settings = ultimatecrt::gpu::Settings::default();
    for round in 0..=rounds {
        for r in &mut runs {
            let (_, params) = &configs[r.cfg];
            let bgra = &contents[r.content].1;
            let frames: Vec<_> = (0..burst)
                .map(|_| {
                    (
                        Some(SourceFrame {
                            width: W,
                            height: H,
                            bgra,
                        }),
                        view,
                    )
                })
                .collect();
            let _ = gpu.take_timing_average();
            gpu.render_burst(&frames, &settings, params, r.size, TargetFormat::Bgra8Unorm)
                .expect("render");
            let t = gpu.take_timing_average().expect("timings");
            if round == 0 {
                continue; // warm-up: pipelines, allocations, history
            }
            r.frame.push(t.sum_ms());
            for (k, n) in names.iter().enumerate() {
                r.stages[k].push(t.stage_ms(n));
            }
        }
    }
    let mut header = String::from("| scenario | total (p10..p90) |");
    for n in &names {
        header.push_str(&format!(" {n} |"));
    }
    println!("{header}");
    for r in &runs {
        let mut line = format!(
            "| {} | {:.3} ({:.2}..{:.2}) |",
            r.label,
            pct(&r.frame, 0.5),
            pct(&r.frame, 0.1),
            pct(&r.frame, 0.9)
        );
        for s in &r.stages {
            let v: Vec<f64> = s.iter().flatten().copied().collect();
            if v.len() == s.len() && !v.is_empty() {
                line.push_str(&format!(" {:.3} |", pct(&v, 0.5)));
            } else {
                line.push_str(" - |");
            }
        }
        println!("{line}");
    }
}

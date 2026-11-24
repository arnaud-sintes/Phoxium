//! A tool, not a check (ignored): renders a moving scene through the whole chain with persistence on, over frames
//! at several frame periods including the switches between them, and prints a hash of every output frame. Run it
//! before and after a change that must be bit-exact (the history layout of `phosphor`, its skipped channels) and
//! compare the printed lines: `cargo test -p phoxium --test phosphor_dump -- --ignored --nocapture`.
//!
//! The sequence: 12 frames at 16.667 ms (60 Hz: the red history is skipped), 12 at 6.944 ms (144 Hz: it is not),
//! 12 at 16.667 ms again (it is skipped again, its image must have been zero), 12 at 20 ms (50 Hz), with a restart of
//! the history (`clear_history`) in the middle of the last block.

use phoxium::gpu::{Headless, Settings, SourceFrame, TargetFormat};
use phoxium::params::Params;
use phoxium::render_cmd::resolve_preset_in;
use phoxium::scaling::View;

const W: u32 = 160;
const H: u32 = 120;

/// A bright block that moves 5 px a frame over a dim ramp, in colour (so every channel has a history to keep).
fn frame(n: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    let x0 = (n * 5) % (W - 24);
    for y in 0..H {
        for x in 0..W {
            let inside = x >= x0 && x < x0 + 24 && (30..70).contains(&y);
            let (b, g, r) = if inside {
                (220, 240, 255)
            } else {
                ((x * 255 / W) as u8 / 4, (y * 255 / H) as u8 / 4, 24)
            };
            v.extend_from_slice(&[b, g, r, 255]);
        }
    }
    v
}

fn fnv(bytes: &[f32]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    for f in bytes {
        for b in f.to_bits().to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

#[test]
#[ignore = "tool: prints hashes for a before/after comparison"]
fn history_hashes() {
    let mut gpu = Headless::new(false).expect("headless GPU");
    let preset = resolve_preset_in("arcade-19in", None)
        .expect("shipped preset")
        .0
        .params();
    let params: Params = preset;
    let settings = Settings::default();
    let view = View::full(W, H);
    let blocks: [(f32, u32); 4] = [(16.667, 12), (6.944, 12), (16.667, 12), (20.0, 12)];
    let mut n = 0;
    for (b, (ms, count)) in blocks.into_iter().enumerate() {
        gpu.set_frame_time_ms(Some(ms));
        for k in 0..count {
            if b == 3 && k == 6 {
                gpu.clear_history();
            }
            let bgra = frame(n);
            let r = gpu
                .render(
                    Some(SourceFrame {
                        width: W,
                        height: H,
                        bgra: &bgra,
                    }),
                    view,
                    &settings,
                    &params,
                    (320, 240),
                    TargetFormat::Rgba32F,
                )
                .expect("render");
            println!("frame {n:2} ({ms:6.3} ms): {:016x}", fnv(&r.output.rgba));
            n += 1;
        }
    }
}

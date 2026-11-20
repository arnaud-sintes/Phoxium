//! Cost per frame of the two capture paths (QUESTIONS CAP-2, ARCHITECTURE A29), headless, no window.
//!
//! Ignored by default (it takes about a minute and its numbers depend on the machine's load). Run it on a release
//! build:
//!
//! ```text
//! cargo test --release -p ultimatecrt --test zero_copy_bench -- --ignored --nocapture
//! ```
//!
//! For each source size it times, many times over, what each path costs per frame:
//! - **capture side** (the capture thread, wall time): CPU path = `CopySubresourceRegion` into the staging texture,
//!   `Map` (waits for the copy), row copy into the CPU buffer (`capture::read_back_region`, the capture code);
//!   zero-copy = `CopySubresourceRegion` into a shared slot, `End` of the event query, `Flush`, poll `GetData`
//!   (`SharedTextures::copy_into`, the capture code).
//! - **render side**: the GPU `upload` segment (buffer to image copy, CPU path only; zero-copy has none, its
//!   acquire barrier is in `decode`) and the `decode` segment, from the GPU timestamps; and the wall time of one
//!   whole headless frame with the chain off and a tiny target (staging memcpy, submission, the fence wait), whose
//!   difference between the paths is the render thread's share of the copy.
//!
//! Medians with the 10th and 90th percentiles. The GPU is shared with the desktop and a laptop GPU throttles:
//! the numbers compare the paths on one machine at one time, they are not absolute.

use std::time::Instant;

use ultimatecrt::adapters::Luid;
use ultimatecrt::capture::{self, SharedTextures};
use ultimatecrt::gpu::{Context, Headless, Settings, SourceFrame, StageReadback, TargetFormat};
use ultimatecrt::params::Params;
use ultimatecrt::scaling::View;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Texture2D, D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

const SIZES: [(u32, u32); 4] = [(1280, 720), (1920, 1080), (2560, 1440), (3840, 2160)];

/// (median, p10, p90) in ms.
fn stats(mut v: Vec<f64>) -> (f64, f64, f64) {
    v.sort_by(f64::total_cmp);
    let at = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
    (at(0.5), at(0.1), at(0.9))
}

fn fmt((m, lo, hi): (f64, f64, f64)) -> String {
    format!("{m:.3} [{lo:.3}-{hi:.3}]")
}

fn segment(gpu: &Headless, name: &str) -> Option<f64> {
    gpu.last_timings()?
        .stages
        .iter()
        .find(|s| s.name == name)
        .and_then(|s| s.ms)
}

#[test]
#[ignore = "measurement: run on a release build with --ignored --nocapture"]
fn capture_path_cost_per_frame() {
    let ctx = Context::new_headless_external(false).expect("a Vulkan device");
    let kind = ctx.external_memory().expect("the device imports Direct3D 11 textures");
    let luid = Luid::from_vulkan(ctx.gpu.luid.expect("a LUID"));
    let gpu_name = ctx.gpu.name.clone();
    let mut gpu = Headless::from_context(ctx).expect("renderer");
    gpu.enable_only(&[]).expect("chain off");
    gpu.set_read_stages(StageReadback::None);
    let (device, context, _) = capture::d3d11_device(Some(luid)).expect("Direct3D 11 device");
    let (s, p) = (Settings::identity(), Params::identity_gun());
    println!(
        "GPU: {gpu_name}, handle kind {}, chain off, target 64 x 64",
        kind.name()
    );
    println!(
        "| source | CPU path: capture copy+map+rows | zero-copy: GPU copy+event wait | CPU path: GPU upload | CPU path: render frame | zero-copy: render frame | decode (CPU path / zero-copy) |"
    );
    println!("|---|---|---|---|---|---|---|");
    for (w, h) in SIZES {
        let reps = if w * h > 4_000_000 { 120 } else { 240 };
        // The captured window: the client area at (8, 31) inside a frame, as WGC delivers it.
        let win = (w + 16, h + 39);
        let crop = (8, 31, w, h);
        let desc = D3D11_TEXTURE2D_DESC {
            Width: win.0,
            Height: win.1,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let pixels: Vec<u8> = (0..win.0 as usize * win.1 as usize * 4)
            .map(|i| (i * 7 % 251) as u8)
            .collect();
        let mut t: Option<ID3D11Texture2D> = None;
        // SAFETY: valid description and data covering the texture.
        unsafe {
            device
                .CreateTexture2D(&desc, None, Some(&mut t))
                .expect("window texture");
            context.UpdateSubresource(t.as_ref().unwrap(), 0, None, pixels.as_ptr().cast(), win.0 * 4, 0);
        }
        let window = t.unwrap();
        let pool = SharedTextures::create(&device, (w, h), kind, 1).expect("pool");
        let view = View::full(w, h);

        // Capture side, CPU path.
        let mut staging = None;
        let mut data = Vec::new();
        let mut cpu_capture = Vec::new();
        for i in 0..reps + 10 {
            let t0 = Instant::now();
            capture::read_back_region(&device, &context, &mut staging, &window, crop, &mut data).expect("read-back");
            if i >= 10 {
                cpu_capture.push(t0.elapsed().as_secs_f64() * 1e3);
            }
        }
        // Capture side, zero-copy.
        let mut zc_capture = Vec::new();
        for i in 0..reps + 10 {
            let slot = pool.begin_write().expect("slot");
            let t0 = Instant::now();
            pool.copy_into(&context, slot, &window, (crop.0, crop.1)).expect("copy");
            if i >= 10 {
                zc_capture.push(t0.elapsed().as_secs_f64() * 1e3);
            }
            pool.publish(slot, i as u64 + 1);
        }
        // Render side, CPU path: every frame uploads the client area.
        let frame = SourceFrame {
            width: w,
            height: h,
            bgra: &data,
        };
        let (mut cpu_frame, mut cpu_upload, mut cpu_decode) = (Vec::new(), Vec::new(), Vec::new());
        for i in 0..reps + 10 {
            let t0 = Instant::now();
            gpu.render(Some(frame), view, &s, &p, (64, 64), TargetFormat::Bgra8Unorm)
                .expect("CPU frame");
            if i >= 10 {
                cpu_frame.push(t0.elapsed().as_secs_f64() * 1e3);
                cpu_upload.extend(segment(&gpu, "upload"));
                cpu_decode.extend(segment(&gpu, "decode"));
            }
        }
        // Render side, zero-copy: every frame takes the newest slot.
        let (mut zc_frame, mut zc_decode) = (Vec::new(), Vec::new());
        let mut seq = 1_000_000u64;
        for i in 0..reps + 10 {
            let slot = pool.begin_write().expect("slot");
            pool.copy_into(&context, slot, &window, (crop.0, crop.1)).expect("copy");
            seq += 1;
            pool.publish(slot, seq);
            let f = pool.take(seq - 1).expect("frame");
            let t0 = Instant::now();
            gpu.render_shared(&f, view, &s, &p, (64, 64), TargetFormat::Bgra8Unorm)
                .expect("shared frame");
            if i >= 10 {
                zc_frame.push(t0.elapsed().as_secs_f64() * 1e3);
                zc_decode.extend(segment(&gpu, "decode"));
            }
        }
        let dec = |v: Vec<f64>| {
            if v.is_empty() {
                "n/a".to_string()
            } else {
                format!("{:.3}", stats(v).0)
            }
        };
        println!(
            "| {w} x {h} | {} | {} | {} | {} | {} | {} / {} |",
            fmt(stats(cpu_capture)),
            fmt(stats(zc_capture)),
            if cpu_upload.is_empty() {
                "n/a".to_string()
            } else {
                fmt(stats(cpu_upload))
            },
            fmt(stats(cpu_frame)),
            fmt(stats(zc_frame)),
            dec(cpu_decode),
            dec(zc_decode),
        );
    }
}

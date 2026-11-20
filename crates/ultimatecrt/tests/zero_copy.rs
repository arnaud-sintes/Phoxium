//! Zero-copy capture path (A29, QUESTIONS CAP-2), headless: no window, no capture session.
//!
//! A Direct3D 11 device is created on the adapter of the Vulkan test device (LUID match). A "captured window"
//! texture is filled with known pixels by `UpdateSubresource`; the real capture code copies its client area (a crop)
//! into the shared pool (`capture::SharedTextures::copy_into`, with the event-query wait), and the real pipeline
//! imports the pool and runs `decode` (and the chain) on it. The results are compared with the CPU path on the same
//! pixels, bit for bit.
//!
//! When the machine cannot share Direct3D 11 textures with Vulkan (no device, no extension, no import), the tests
//! print why and pass, unless `UCRT_REQUIRE_GPU` is set: then they fail (the development machine supports it).

// Only ssert_no_validation_messages is used here.
#[allow(dead_code)]
mod common;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ultimatecrt::adapters::Luid;
use ultimatecrt::capture::{self, SharedTextures};
use ultimatecrt::gpu::{
    gpu_required, validation_layer_available, Context, Headless, Settings, SourceFrame, StageReadback, TargetFormat,
};
use ultimatecrt::params::Params;
use ultimatecrt::scaling::View;
use ultimatecrt::zerocopy::{HandleKind, SlotState, POOL_SLOTS};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

/// The "window" texture the capture delivers (frame and title bar included) and the client area inside it.
const WINDOW: (u32, u32) = (200, 150);
const CROP: (u32, u32, u32, u32) = (8, 31, 160, 100);

/// The Direct3D 11 objects of a test. The device is multithread protected (as the capture makes it), so the
/// writer thread may use it.
#[derive(Clone)]
struct D3d {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
}

// SAFETY: the device is created with multithread protection (`capture::d3d11_device`), which serialises the calls
// of the writer thread and the main thread on the immediate context.
unsafe impl Send for D3d {}

/// A headless renderer whose device can import shared Direct3D 11 textures, with a Direct3D 11 device on the same
/// adapter, or `None` (skipped) when the machine cannot.
fn setup(test: &str) -> Option<(Headless, D3d, HandleKind)> {
    let validation = validation_layer_available();
    assert!(
        validation || !gpu_required(),
        "UCRT_REQUIRE_GPU is set but VK_LAYER_KHRONOS_validation is not installed"
    );
    let skip = |why: String| {
        assert!(!gpu_required(), "UCRT_REQUIRE_GPU is set but {why}");
        eprintln!("{test} skipped: {why}");
        None
    };
    let ctx = match Context::new_headless_external(validation) {
        Ok(c) => c,
        Err(e) => return skip(format!("no Vulkan 1.3 device ({e})")),
    };
    let Some(kind) = ctx.external_memory() else {
        return skip(format!(
            "{} cannot import Direct3D 11 textures (VK_KHR_external_memory_win32)",
            ctx.gpu.name
        ));
    };
    let Some(luid) = ctx.gpu.luid.map(Luid::from_vulkan) else {
        return skip(format!("{} reports no LUID", ctx.gpu.name));
    };
    let (device, context, got) = match capture::d3d11_device(Some(luid)) {
        Ok(d) => d,
        Err(e) => return skip(format!("no Direct3D 11 device ({e})")),
    };
    assert_eq!(
        got, luid,
        "the Direct3D 11 device is not on the Vulkan device's adapter"
    );
    let headless = Headless::from_context(ctx).expect("headless renderer");
    Some((headless, D3d { device, context }, kind))
}

/// A window texture to fill with `UpdateSubresource`.
fn window_texture(d: &D3d, size: (u32, u32)) -> ID3D11Texture2D {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: size.0,
        Height: size.1,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut t = None;
    // SAFETY: a valid description on a live device.
    unsafe { d.device.CreateTexture2D(&desc, None, Some(&mut t)) }.expect("window texture");
    t.unwrap()
}

fn upload(d: &D3d, tex: &ID3D11Texture2D, size: (u32, u32), bgra: &[u8]) {
    assert_eq!(bgra.len(), size.0 as usize * size.1 as usize * 4);
    // SAFETY: the data covers the whole texture with the given row pitch.
    unsafe {
        d.context
            .UpdateSubresource(tex, 0, None, bgra.as_ptr().cast(), size.0 * 4, 0);
    }
}

/// A window of `size` whose client area `crop` holds `inside(x, y)` (client coordinates) and whose frame is
/// magenta: a wrong crop shows.
fn window_pixels(size: (u32, u32), crop: (u32, u32, u32, u32), inside: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
    let mut v = Vec::with_capacity(size.0 as usize * size.1 as usize * 4);
    for y in 0..size.1 {
        for x in 0..size.0 {
            let inner = x >= crop.0 && x < crop.0 + crop.2 && y >= crop.1 && y < crop.1 + crop.3;
            v.extend_from_slice(&if inner {
                inside(x - crop.0, y - crop.1)
            } else {
                [255, 0, 255, 255]
            });
        }
    }
    v
}

/// The client area of a window image, as the CPU path delivers it.
fn crop_cpu(win: &[u8], size: (u32, u32), crop: (u32, u32, u32, u32)) -> Vec<u8> {
    let mut v = Vec::with_capacity(crop.2 as usize * crop.3 as usize * 4);
    for y in crop.1..crop.1 + crop.3 {
        let row = (y as usize * size.0 as usize + crop.0 as usize) * 4;
        v.extend_from_slice(&win[row..row + crop.2 as usize * 4]);
    }
    v
}

/// Rich content: every code value on every channel, position dependent, and frame dependent.
fn rich(n: u32) -> impl Fn(u32, u32) -> [u8; 4] {
    move |x, y| {
        let i = y * CROP.2 + x;
        [
            (i.wrapping_mul(7).wrapping_add(n * 31) & 0xFF) as u8,
            ((i / 3 + n * 17) & 0xFF) as u8,
            (i.wrapping_mul(13) ^ n) as u8,
            255,
        ]
    }
}

/// Frame `n` for the synchronisation test: G and B carry the frame number, R the position.
fn tagged(n: u64) -> impl Fn(u32, u32) -> [u8; 4] {
    move |x, y| {
        [
            ((n >> 8) & 0xFF) as u8,
            (n & 0xFF) as u8,
            ((x * 3 + y * 5) & 0xFF) as u8,
            255,
        ]
    }
}

/// Copies the client area of the window texture into a free slot with the capture code, publishes it as `seq`
/// and takes it as the render side does.
fn capture_into(d: &D3d, pool: &Arc<SharedTextures>, window: &ID3D11Texture2D, seq: u64) -> capture::SharedFrame {
    let slot = pool.begin_write().expect("a free slot");
    pool.copy_into(&d.context, slot, window, (CROP.0, CROP.1))
        .expect("GPU copy into the shared slot");
    assert!(pool.publish(slot, seq));
    pool.take(seq - 1).expect("the published frame")
}

/// The FP32 bits of an image, for bit-exact comparisons.
fn bits(v: &[f32]) -> Vec<u32> {
    v.iter().map(|f| f.to_bits()).collect()
}

/// `decode` on the shared texture gives the same native raster as the CPU path, bit for bit, and so does the whole
/// chain with the default parameters (same `B8G8R8A8_UNORM` texels, same `texelFetch`, same shaders). The CPU
/// path's bytes are also those of the real capture code (staging copy, map, rows) for that crop.
#[test]
fn shared_frames_decode_exactly_like_the_cpu_path() {
    let Some((mut gpu, d, kind)) = setup("shared_frames_decode_exactly_like_the_cpu_path") else {
        return;
    };
    eprintln!("handle kind: {}", kind.name());
    let counter = gpu.validation_counter();
    let window = window_texture(&d, WINDOW);
    let pool = SharedTextures::create(&d.device, (CROP.2, CROP.3), kind, 1).expect("shared pool");
    let view = View::full(CROP.2, CROP.3);
    let mut staging = None;
    let mut cpu_bytes = Vec::new();
    for n in 0..6u32 {
        let win = window_pixels(WINDOW, CROP, rich(n));
        upload(&d, &window, WINDOW, &win);
        let expected = crop_cpu(&win, WINDOW, CROP);
        // The real CPU-path copy of the capture thread.
        capture::read_back_region(&d.device, &d.context, &mut staging, &window, CROP, &mut cpu_bytes)
            .expect("CPU read-back");
        assert_eq!(cpu_bytes, expected, "the CPU path's crop");
        let frame = SourceFrame {
            width: CROP.2,
            height: CROP.3,
            bgra: &cpu_bytes,
        };
        let shared = capture_into(&d, &pool, &window, u64::from(n) + 1);
        // Identity transfers: the native raster is code / 255 in FP16.
        let (s, p) = (Settings::identity(), Params::identity_gun());
        let a = gpu
            .render(Some(frame), view, &s, &p, (CROP.2, CROP.3), TargetFormat::Rgba32F)
            .expect("CPU path frame");
        let b = gpu
            .render_shared(&shared, view, &s, &p, (CROP.2, CROP.3), TargetFormat::Rgba32F)
            .expect("zero-copy frame");
        assert_eq!(bits(&a.native.rgba), bits(&b.native.rgba), "decode output, frame {n}");
        assert_eq!(bits(&a.output.rgba), bits(&b.output.rgba), "output, frame {n}");
        // Spot check of the values themselves.
        let px = expected[..4].to_vec();
        let v = b.native.pixel(0, 0);
        assert_eq!(
            [v[0], v[1], v[2]].map(|c| (c * 255.0).round() as u8),
            [px[2], px[1], px[0]],
            "pixel (0, 0) of frame {n}"
        );
        // The whole default chain, both paths.
        let (s, p) = (Settings::default(), Params::default());
        gpu.set_read_stages(StageReadback::None);
        let a = gpu
            .render(Some(frame), view, &s, &p, (480, 300), TargetFormat::Bgra8Unorm)
            .expect("CPU path, default chain");
        let b = gpu
            .render_shared(&shared, view, &s, &p, (480, 300), TargetFormat::Bgra8Unorm)
            .expect("zero-copy, default chain");
        assert_eq!(
            bits(&a.output.rgba),
            bits(&b.output.rgba),
            "default chain output, frame {n}"
        );
        gpu.set_read_stages(StageReadback::All);
    }
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// The synchronisation and slot rules over 300 frames: a writer thread copies new frames into the three slots as
/// fast as it can (the capture thread), while the render side takes the newest one and renders it, sometimes two
/// frames in flight. Every frame read back must be one complete frame (every pixel carries the same frame number,
/// no tearing), the right one (the frame taken, never an older slot), with the position pattern intact (the crop).
#[test]
fn three_slots_over_300_frames_never_tear_or_go_stale() {
    let Some((mut gpu, d, kind)) = setup("three_slots_over_300_frames_never_tear_or_go_stale") else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.set_read_stages(StageReadback::All);
    let pool = SharedTextures::create(&d.device, (CROP.2, CROP.3), kind, 1).expect("shared pool");
    let stop = Arc::new(AtomicBool::new(false));
    let published = Arc::new(AtomicU64::new(0));
    let writer = {
        let (d, pool, stop, published) = (d.clone(), pool.clone(), stop.clone(), published.clone());
        std::thread::spawn(move || {
            let window = window_texture(&d, WINDOW);
            let mut waits = 0u64;
            let mut n = 0u64;
            while !stop.load(Ordering::Relaxed) {
                n += 1;
                upload(&d, &window, WINDOW, &window_pixels(WINDOW, CROP, tagged(n)));
                let slot = loop {
                    if let Some(s) = pool.begin_write() {
                        break s;
                    }
                    waits += 1;
                    if stop.load(Ordering::Relaxed) {
                        return (n, waits);
                    }
                    std::thread::yield_now();
                };
                pool.copy_into(&d.context, slot, &window, (CROP.0, CROP.1))
                    .expect("GPU copy");
                assert!(pool.publish(slot, n));
                published.store(n, Ordering::Release);
            }
            (n, waits)
        })
    };
    let view = View::full(CROP.2, CROP.3);
    let (s, p) = (Settings::identity(), Params::identity_gun());
    let take_newer = |after: u64| -> capture::SharedFrame {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(f) = pool.take(after) {
                return f;
            }
            assert!(Instant::now() < deadline, "no new frame within 5 s");
            std::thread::yield_now();
        }
    };
    let check = |r: &ultimatecrt::gpu::Rendered, seq: u64, i: usize| {
        let img = &r.native;
        for y in 0..img.height {
            for x in 0..img.width {
                let v = img.pixel(x, y);
                let code = |c: f32| (c * 255.0).round() as u64;
                let got = (code(v[2]) << 8) | code(v[1]);
                assert_eq!(
                    got & 0xFFFF,
                    seq & 0xFFFF,
                    "frame {i}: pixel ({x}, {y}) is from another frame (tearing or a stale slot)"
                );
                assert_eq!(
                    code(v[0]),
                    u64::from((x * 3 + y * 5) & 0xFF),
                    "frame {i}: pixel ({x}, {y}) moved (crop)"
                );
            }
        }
    };
    let mut last = 0;
    let mut frames = 0;
    let mut i = 0;
    while frames < 300 {
        if i % 10 == 9 {
            // Two frames in flight: the first stays held until its fence, the second is read back.
            let a = take_newer(last);
            let b = take_newer(a.seq());
            last = b.seq();
            let r = gpu
                .render_shared_burst(
                    &[(a, view), (b.clone(), view)],
                    &s,
                    &p,
                    (CROP.2, CROP.3),
                    TargetFormat::Rgba16F,
                )
                .expect("burst");
            check(&r, b.seq(), i);
            frames += 2;
        } else {
            let f = take_newer(last);
            assert!(f.seq() > last, "an older frame was taken");
            last = f.seq();
            let r = gpu
                .render_shared(&f, view, &s, &p, (CROP.2, CROP.3), TargetFormat::Rgba16F)
                .expect("frame");
            check(&r, f.seq(), i);
            frames += 1;
        }
        // Release by fence: after a call, only the source is held by the renderer.
        assert_eq!(gpu.shared_holds(), 1);
        i += 1;
    }
    stop.store(true, Ordering::Relaxed);
    let (written, waits) = writer.join().expect("writer thread");
    eprintln!(
        "{frames} frames rendered, {written} written by the capture side ({waits} waits for a free slot), last published {}",
        published.load(Ordering::Acquire)
    );
    assert!(written > 300);
    // Only the renderer's current source is still held.
    let states = pool.slot_states();
    assert_eq!(
        states.iter().filter(|s| **s == SlotState::Held).count(),
        1,
        "{states:?}"
    );
    drop(gpu);
    // The renderer gone, every slot is back.
    assert!(
        pool.slot_states().iter().all(|s| *s != SlotState::Held),
        "{:?}",
        pool.slot_states()
    );
    assert_eq!(POOL_SLOTS, 3);
    common::assert_no_validation_messages(&counter);
}

/// A new client size is a new pool: the renderer drains, imports it and drops the old one; the CPU path and back
/// work in between, and a frame of the old pool after the new one is imported again (a pool is never mixed up).
#[test]
fn a_new_pool_size_is_imported_and_the_cpu_path_still_works() {
    let Some((mut gpu, d, kind)) = setup("a_new_pool_size_is_imported_and_the_cpu_path_still_works") else {
        return;
    };
    let counter = gpu.validation_counter();
    let (s, p) = (Settings::identity(), Params::identity_gun());
    let window = window_texture(&d, WINDOW);
    let small = (CROP.0, CROP.1, 64, 40);
    let check = |r: &ultimatecrt::gpu::Rendered, size: (u32, u32), n: u32| {
        assert_eq!((r.native.width, r.native.height), size);
        let v = r.native.pixel(size.0 - 1, size.1 - 1);
        let c = rich(n)(size.0 - 1, size.1 - 1);
        assert_eq!(
            [v[0], v[1], v[2]].map(|x| (x * 255.0).round() as u8),
            [c[2], c[1], c[0]]
        );
    };
    // Pool 1, the full client area.
    upload(&d, &window, WINDOW, &window_pixels(WINDOW, CROP, rich(1)));
    let pool1 = SharedTextures::create(&d.device, (CROP.2, CROP.3), kind, 1).expect("pool 1");
    let f1 = capture_into(&d, &pool1, &window, 1);
    let r = gpu
        .render_shared(
            &f1,
            View::full(CROP.2, CROP.3),
            &s,
            &p,
            (CROP.2, CROP.3),
            TargetFormat::Rgba32F,
        )
        .expect("pool 1");
    check(&r, (CROP.2, CROP.3), 1);
    drop(f1);
    // Pool 2, a smaller client area (the window was resized).
    let pool2 = SharedTextures::create(&d.device, (small.2, small.3), kind, 2).expect("pool 2");
    let slot = pool2.begin_write().unwrap();
    pool2.copy_into(&d.context, slot, &window, (small.0, small.1)).unwrap();
    pool2.publish(slot, 2);
    let f2 = pool2.take(1).unwrap();
    let r = gpu
        .render_shared(
            &f2,
            View::full(small.2, small.3),
            &s,
            &p,
            (small.2, small.3),
            TargetFormat::Rgba32F,
        )
        .expect("pool 2");
    check(&r, (small.2, small.3), 1);
    // The old pool's slots are all released (the renderer drained it).
    assert!(pool1.slot_states().iter().all(|s| *s != SlotState::Held));
    drop(f2);
    // The CPU path: the shared source is let go once no frame reads it.
    let win = window_pixels(WINDOW, CROP, rich(3));
    let bytes = crop_cpu(&win, WINDOW, CROP);
    let r = gpu
        .render(
            Some(SourceFrame {
                width: CROP.2,
                height: CROP.3,
                bgra: &bytes,
            }),
            View::full(CROP.2, CROP.3),
            &s,
            &p,
            (CROP.2, CROP.3),
            TargetFormat::Rgba32F,
        )
        .expect("CPU frame");
    check(&r, (CROP.2, CROP.3), 3);
    assert_eq!(gpu.shared_holds(), 0);
    assert!(pool2.slot_states().iter().all(|s| *s != SlotState::Held));
    // Back to zero-copy on pool 1 (imported again).
    upload(&d, &window, WINDOW, &win);
    let f3 = capture_into(&d, &pool1, &window, 4);
    let r = gpu
        .render_shared(
            &f3,
            View::full(CROP.2, CROP.3),
            &s,
            &p,
            (CROP.2, CROP.3),
            TargetFormat::Rgba32F,
        )
        .expect("pool 1 again");
    check(&r, (CROP.2, CROP.3), 3);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// Failure injection: a device made without the extension refuses shared frames with a clear reason, a pool whose
/// handles are not shared textures fails to import; neither crashes, and the renderer keeps rendering CPU frames,
/// then zero-copy frames again from a good pool.
#[test]
fn import_failures_are_errors_and_the_cpu_path_goes_on() {
    let Some((mut gpu, d, kind)) = setup("import_failures_are_errors_and_the_cpu_path_goes_on") else {
        return;
    };
    let counter = gpu.validation_counter();
    let (s, p) = (Settings::identity(), Params::identity_gun());
    let view = View::full(CROP.2, CROP.3);
    let window = window_texture(&d, WINDOW);
    let win = window_pixels(WINDOW, CROP, rich(5));
    upload(&d, &window, WINDOW, &win);
    let bytes = crop_cpu(&win, WINDOW, CROP);
    let cpu = SourceFrame {
        width: CROP.2,
        height: CROP.3,
        bgra: &bytes,
    };
    let target = (CROP.2, CROP.3);

    // A device without the extension (the setting was off at start).
    let plain = Context::new_headless(false).expect("a plain device");
    assert_eq!(plain.external_memory(), None);
    assert!(!plain.external_memory_requested());
    let mut plain = Headless::from_context(plain).expect("renderer");
    let good = SharedTextures::create(&d.device, (CROP.2, CROP.3), kind, 1).expect("pool");
    let f = capture_into(&d, &good, &window, 1);
    let e = plain
        .render_shared(&f, view, &s, &p, target, TargetFormat::Rgba32F)
        .expect_err("no extension");
    assert!(e.to_string().contains("without VK_KHR_external_memory_win32"), "{e}");
    plain
        .render(Some(cpu), view, &s, &p, target, TargetFormat::Rgba32F)
        .expect("the CPU path after the refusal");
    assert_eq!(plain.shared_holds(), 0);
    drop(plain);
    drop(f);

    // Handles that are not shared textures.
    let bogus = SharedTextures::with_bogus_handles(&d.device, (CROP.2, CROP.3), 7).expect("pool");
    let slot = bogus.begin_write().unwrap();
    bogus.publish(slot, 1);
    let fb = bogus.take(0).unwrap();
    let e = gpu
        .render_shared(&fb, view, &s, &p, target, TargetFormat::Rgba32F)
        .expect_err("a bogus handle must not import");
    eprintln!("bogus import refused: {e}");
    drop(fb);
    assert_eq!(gpu.shared_holds(), 0);
    assert!(bogus.slot_states().iter().all(|s| *s != SlotState::Held));
    let r = gpu
        .render(Some(cpu), view, &s, &p, target, TargetFormat::Rgba32F)
        .expect("the CPU path after the failed import");
    let a = r.native.rgba.clone();
    // A good pool works again on the same renderer, with the same result as the CPU path.
    let f = capture_into(&d, &good, &window, 2);
    let r = gpu
        .render_shared(&f, view, &s, &p, target, TargetFormat::Rgba32F)
        .expect("zero-copy after the failure");
    assert_eq!(bits(&a), bits(&r.native.rgba));
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

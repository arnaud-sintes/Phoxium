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
use ultimatecrt::zerocopy::{CapturePath, HandleKind, SlotState, POOL_SLOTS};
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

/// The application's device enables `VK_KHR_external_memory_win32` whenever it can import (so the capture path can be
/// switched live): the CPU path must not notice. The same CPU frames through the whole default chain on a device
/// with the extension and on one without give the same output bit for bit, and both stay silent under validation.
#[test]
fn the_cpu_path_is_bit_identical_with_the_extension_enabled() {
    let Some((mut with_ext, _d, _kind)) = setup("the_cpu_path_is_bit_identical_with_the_extension_enabled") else {
        return;
    };
    let validation = validation_layer_available();
    let plain_ctx = Context::new_headless(validation).expect("a device without the extension");
    assert_eq!(plain_ctx.external_memory(), None);
    let mut plain = Headless::from_context(plain_ctx).expect("renderer");
    assert!(with_ext.context().external_memory().is_some());
    let (c1, c2) = (with_ext.validation_counter(), plain.validation_counter());
    let view = View::full(CROP.2, CROP.3);
    for n in 0..4u32 {
        let bytes = crop_cpu(&window_pixels(WINDOW, CROP, rich(n)), WINDOW, CROP);
        let frame = SourceFrame {
            width: CROP.2,
            height: CROP.3,
            bgra: &bytes,
        };
        let (s, p) = (Settings::default(), Params::default());
        for g in [&mut with_ext, &mut plain] {
            g.set_read_stages(StageReadback::All);
        }
        let a = with_ext
            .render(Some(frame), view, &s, &p, (480, 300), TargetFormat::Rgba32F)
            .expect("CPU path, extension enabled");
        let b = plain
            .render(Some(frame), view, &s, &p, (480, 300), TargetFormat::Rgba32F)
            .expect("CPU path, no extension");
        assert_eq!(bits(&a.native.rgba), bits(&b.native.rgba), "decode, frame {n}");
        assert_eq!(bits(&a.output.rgba), bits(&b.output.rgba), "output, frame {n}");
        assert_eq!(with_ext.shared_holds(), 0);
    }
    drop(with_ext);
    drop(plain);
    common::assert_no_validation_messages(&c1);
    common::assert_no_validation_messages(&c2);
}

/// Checks that every pixel of a rendered frame is frame `n` of [`tagged`] at its own position (no tearing, no
/// stale slot, the crop right).
fn assert_tagged(r: &ultimatecrt::gpu::Rendered, n: u64, what: &str) {
    let img = &r.native;
    assert_eq!((img.width, img.height), (CROP.2, CROP.3), "{what}: size");
    let code = |c: f32| (c * 255.0).round() as u64;
    for y in 0..img.height {
        for x in 0..img.width {
            let v = img.pixel(x, y);
            assert_eq!(
                (code(v[2]) << 8) | code(v[1]),
                n & 0xFFFF,
                "{what}: pixel ({x}, {y}) is not frame {n}"
            );
            assert_eq!(
                code(v[0]),
                u64::from((x * 3 + y * 5) & 0xFF),
                "{what}: pixel ({x}, {y}) moved"
            );
        }
    }
}

/// The frame tag of a rendered frame (pixel (0, 0)).
fn tag_of(r: &ultimatecrt::gpu::Rendered) -> u64 {
    let v = r.native.pixel(0, 0);
    let code = |c: f32| (c * 255.0).round() as u64;
    (code(v[2]) << 8) | code(v[1])
}

/// Renders the frame taken from the capture the way the application does: its shared texture when it has one (the
/// zero-copy path), else its CPU pixels. Returns the path the frame took, the result and, on the zero-copy path, a
/// hold on its shared slot (a clone of the renderer's).
fn render_taken(
    gpu: &mut Headless,
    frame: &mut capture::Frame,
) -> (CapturePath, ultimatecrt::gpu::Rendered, Option<capture::SharedFrame>) {
    let view = View::full(CROP.2, CROP.3);
    let (s, p) = (Settings::identity(), Params::identity_gun());
    match frame.shared.take() {
        Some(sh) => {
            let r = gpu
                .render_shared(&sh, view, &s, &p, (CROP.2, CROP.3), TargetFormat::Rgba32F)
                .expect("zero-copy frame");
            (CapturePath::ZeroCopy, r, Some(sh))
        }
        None => {
            assert!(!frame.pixels_old, "a CPU frame without its own pixels");
            let src = SourceFrame {
                width: frame.width,
                height: frame.height,
                bgra: &frame.data,
            };
            let r = gpu
                .render(Some(src), view, &s, &p, (CROP.2, CROP.3), TargetFormat::Rgba32F)
                .expect("CPU frame");
            (CapturePath::Cpu, r, None)
        }
    }
}

/// A synthetic capture (the real capture code, fed with textures instead of a window) on the Vulkan device's
/// adapter.
fn synthetic_capture(gpu: &Headless, d: &D3d) -> capture::Capture {
    let luid = gpu.context().gpu.luid.map(Luid::from_vulkan).expect("a LUID");
    capture::Capture::synthetic(d.device.clone(), d.context.clone(), luid).expect("synthetic capture")
}

/// The live switch, frame by frame: zero-copy, then the CPU copy, then zero-copy again, one captured frame per
/// rendered frame. Every frame is delivered once (sequence numbers 1, 2, 3, ... with no gap and no repeat), on the
/// path asked when it was captured, and renders as exactly that frame. At the first switch the capture holds a frame
/// for want of a free slot (every slot held by the render side): it is not lost, it goes through the CPU copy.
#[test]
fn switching_zero_copy_cpu_zero_copy_loses_and_repeats_no_frame() {
    let Some((mut gpu, d, _kind)) = setup("switching_zero_copy_cpu_zero_copy_loses_and_repeats_no_frame") else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.set_read_stages(StageReadback::All);
    let cap = synthetic_capture(&gpu, &d);
    let feed = cap.synthetic_feed();
    let window = window_texture(&d, WINDOW);
    let mut frame = capture::Frame::default();
    let mut held: Vec<capture::SharedFrame> = Vec::new();
    let mut on_path = [0usize; 2];
    for n in 1..=60u64 {
        let asked = if (21..=40).contains(&n) {
            CapturePath::Cpu
        } else {
            CapturePath::ZeroCopy
        };
        cap.set_path(asked);
        upload(&d, &window, WINDOW, &window_pixels(WINDOW, CROP, tagged(n)));
        feed.push(&window, CROP).expect("push");
        let mut expected = asked;
        if n == 20 {
            // Frames 17 and 18 kept by the test, frame 19 the renderer's source: every slot is held, so frame 20
            // waits in the capture. Nothing new to take.
            assert!(!cap.take_frame(&mut frame), "frame 20 delivered without a free slot");
            // The switch to the CPU copy: the kept frame goes through it at the next take.
            cap.set_path(CapturePath::Cpu);
            expected = CapturePath::Cpu;
        }
        assert!(cap.take_frame(&mut frame), "frame {n} not delivered");
        assert_eq!(frame.seq, n, "a frame was lost or repeated before frame {n}");
        let (path, r, hold) = render_taken(&mut gpu, &mut frame);
        assert_eq!(path, expected, "frame {n} took the wrong path");
        on_path[usize::from(path == CapturePath::Cpu)] += 1;
        assert_tagged(&r, n, &format!("frame {n} ({path})"));
        assert_eq!(tag_of(&r), n);
        // Nothing more until the next capture.
        assert!(!cap.take_frame(&mut frame), "frame {n} delivered twice");
        if n == 17 || n == 18 {
            // The render side holds two more slots (frames still in flight, as far as the capture knows).
            held.push(hold.expect("a zero-copy frame"));
        }
        if n == 20 {
            held.clear();
        }
    }
    assert_eq!(cap.take_failure(), None);
    assert_eq!(cap.path(), CapturePath::ZeroCopy);
    assert_eq!(on_path, [39, 21], "frames per path (zero-copy, CPU)");
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

/// The live switch while frames flow: a writer thread captures frames as fast as it can (each from its own window
/// texture of a ring of four, as successive capture frames are), while the render side takes the newest frame, renders
/// it and flips the path every 20 frames, 300 frames in all. Every frame rendered is one whole frame at its place,
/// newer than the one before (never an older or a repeated frame), and both paths carry frames.
#[test]
fn switching_paths_while_frames_flow_keeps_every_frame_whole_and_in_order() {
    let Some((mut gpu, d, _kind)) = setup("switching_paths_while_frames_flow_keeps_every_frame_whole_and_in_order")
    else {
        return;
    };
    let counter = gpu.validation_counter();
    gpu.set_read_stages(StageReadback::All);
    let cap = synthetic_capture(&gpu, &d);
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let (d, feed, stop) = (d.clone(), cap.synthetic_feed(), stop.clone());
        std::thread::spawn(move || {
            let ring: Vec<ID3D11Texture2D> = (0..4).map(|_| window_texture(&d, WINDOW)).collect();
            let mut n = 0u64;
            while !stop.load(Ordering::Relaxed) {
                n += 1;
                let tex = &ring[(n % 4) as usize];
                upload(&d, tex, WINDOW, &window_pixels(WINDOW, CROP, tagged(n)));
                feed.push(tex, CROP).expect("push");
                // A capture runs at the source's rate, not flat out.
                std::thread::sleep(Duration::from_micros(500));
            }
            n
        })
    };
    let mut frame = capture::Frame::default();
    let (mut last_seq, mut last_tag) = (0u64, 0u64);
    let mut on_path = [0usize; 2];
    let mut path = CapturePath::ZeroCopy;
    cap.set_path(path);
    for i in 0..300 {
        if i % 20 == 19 {
            path = if path == CapturePath::ZeroCopy {
                CapturePath::Cpu
            } else {
                CapturePath::ZeroCopy
            };
            cap.set_path(path);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while !cap.take_frame(&mut frame) {
            assert!(Instant::now() < deadline, "no new frame within 5 s");
            std::thread::yield_now();
        }
        assert!(frame.seq > last_seq, "frame {i}: an older or repeated frame");
        last_seq = frame.seq;
        let (took, r, _) = render_taken(&mut gpu, &mut frame);
        on_path[usize::from(took == CapturePath::Cpu)] += 1;
        let tag = tag_of(&r);
        assert!(
            tag > last_tag,
            "frame {i}: tag {tag} after {last_tag} (older or repeated content)"
        );
        last_tag = tag;
        assert_tagged(&r, tag, &format!("frame {i} ({took})"));
    }
    stop.store(true, Ordering::Relaxed);
    let written = writer.join().expect("writer thread");
    eprintln!(
        "300 frames rendered ({} zero-copy, {} CPU), {written} captured",
        on_path[0], on_path[1]
    );
    assert!(on_path[0] > 50 && on_path[1] > 50, "{on_path:?}");
    assert_eq!(cap.take_failure(), None);
    drop(gpu);
    common::assert_no_validation_messages(&counter);
}

// ---- The CPU analyses over zero-copy frames (the start-up crash of the 2026-11-20 live review) ----

use ultimatecrt::{analysis, bars, lifecycle, scaling};

/// The main loop's CPU analyses, run in the loop's order on each iteration (`app.rs`: the frame just taken goes
/// to the black-frame scan and the Auto picture area, then the integer-scale detection and the smoothed-source watch
/// run on the frame on screen), through the same `analysis` functions the loop calls.
struct LoopAnalyses {
    detector: scaling::Detector,
    detect_key: (u32, u32),
    smoothing: lifecycle::SmoothingWatch,
    auto: bars::Auto,
    health: lifecycle::Health,
    events: Vec<bars::AutoEvent>,
}

impl LoopAnalyses {
    fn new() -> Self {
        LoopAnalyses {
            detector: scaling::Detector::default(),
            detect_key: (0, 0),
            smoothing: lifecycle::SmoothingWatch::default(),
            auto: bars::Auto::new(bars::Config::default()),
            health: lifecycle::Health::new(0.0),
            events: Vec::new(),
        }
    }

    /// One loop iteration: `got` when a frame was just taken, else the frame on screen analysed again.
    fn run(&mut self, frame: &capture::Frame, got: bool, now: f64) {
        if got {
            analysis::note_health(&mut self.health, now, frame);
            analysis::feed_area(&mut self.auto, now, frame);
        }
        let region = scaling::Rect {
            x: 0,
            y: 0,
            w: frame.width,
            h: frame.height,
        };
        if (frame.width, frame.height) != self.detect_key {
            self.detect_key = (frame.width, frame.height);
            self.detector.reset();
        }
        analysis::feed_scale(&mut self.detector, frame, got, region);
        let eligible = self.detector.is_locked() && self.detector.divisor() == (1, 1);
        analysis::feed_smoothing(&mut self.smoothing, frame, got, region, eligible);
        if let Some(e) = self.auto.tick(now, (1, 1)) {
            self.events.push(e);
        }
    }
}

/// The invariant every consumer relies on: a frame's own pixels (`sample`) are this frame's whole picture, and any
/// picture it carries (`last_sample`) has the frame's size.
fn assert_consistent(frame: &capture::Frame, what: &str) {
    for (name, s) in [("sample", frame.sample()), ("last sample", frame.last_sample())] {
        if let Some(s) = s {
            assert_eq!(
                (s.width(), s.height()),
                (frame.width, frame.height),
                "{what}: {name} size"
            );
            assert!(
                frame.data.len() >= (frame.width * frame.height * 4) as usize,
                "{what}: {name} short"
            );
        }
    }
    if !frame.pixels_old && frame.data_size == (frame.width, frame.height) {
        assert_eq!(
            frame.data.len(),
            (frame.width * frame.height * 4) as usize,
            "{what}: a frame marked as carrying its own pixels has a buffer of another length"
        );
    }
}

/// The client-area pixels the CPU path would deliver for `inside` in `crop`.
fn client_pixels(crop: (u32, u32, u32, u32), inside: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
    let mut v = Vec::with_capacity((crop.2 * crop.3 * 4) as usize);
    for y in 0..crop.3 {
        for x in 0..crop.2 {
            v.extend_from_slice(&inside(x, y));
        }
    }
    v
}

/// The crash, deterministically: the capture thread publishes frame 1 with its sample; the main thread has not taken
/// it yet when frame 2 arrives with a sample due (a detection runs: every frame is sampled); while frame 2's sample is
/// read back, the main thread takes frame 1. Before the fix the capture thread had emptied frame 1's sample to reuse
/// its buffer, so frame 1 came out "with its own pixels" and an empty buffer, and the integer-scale detection indexed
/// it (`range start index ... out of range for slice of length 0`). Now frame 1 keeps its sample, frame 2 brings its
/// own, and the loop's analyses run on both.
#[test]
fn a_frame_taken_while_the_next_sample_is_read_back_keeps_its_pixels() {
    let Some((gpu, d, _kind)) = setup("a_frame_taken_while_the_next_sample_is_read_back_keeps_its_pixels") else {
        return;
    };
    let cap = synthetic_capture(&gpu, &d);
    cap.set_path(CapturePath::ZeroCopy);
    cap.set_full_rate_samples(true);
    let (to_main, from_writer) = std::sync::mpsc::channel::<&'static str>();
    let (to_writer, from_main) = std::sync::mpsc::channel::<()>();
    let writer = {
        let (d, feed) = (d.clone(), cap.synthetic_feed());
        std::thread::spawn(move || {
            let window = window_texture(&d, WINDOW);
            upload(&d, &window, WINDOW, &window_pixels(WINDOW, CROP, rich(1)));
            feed.push(&window, CROP).expect("push 1");
            to_main.send("published 1").unwrap();
            from_main.recv().unwrap();
            let (tx, rx) = (to_main.clone(), std::sync::Mutex::new(from_main));
            // Frame 2: the hook holds the capture thread between its sample's read-back and its publication.
            feed.set_sample_hook(Some(Box::new(move || {
                tx.send("reading frame 2").unwrap();
                rx.lock().unwrap().recv().unwrap();
            })));
            upload(&d, &window, WINDOW, &window_pixels(WINDOW, CROP, rich(2)));
            feed.push(&window, CROP).expect("push 2");
            feed.set_sample_hook(None);
            to_main.send("published 2").unwrap();
        })
    };
    let mut frame = capture::Frame::default();
    let mut loop_analyses = LoopAnalyses::new();
    assert_eq!(from_writer.recv().unwrap(), "published 1");
    to_writer.send(()).unwrap();
    assert_eq!(from_writer.recv().unwrap(), "reading frame 2");
    // The window of the crash.
    let got = cap.take_frame(&mut frame);
    let seen = (got, frame.seq, frame.pixels_old, frame.data.len());
    assert_consistent(&frame, "frame 1 taken during frame 2's read-back");
    loop_analyses.run(&frame, got, 0.0);
    to_writer.send(()).unwrap();
    assert_eq!(from_writer.recv().unwrap(), "published 2");
    writer.join().expect("writer thread");
    assert_eq!(
        seen,
        (true, 1, false, (CROP.2 * CROP.3 * 4) as usize),
        "frame 1 must come with its own whole sample"
    );
    assert_eq!(
        frame.sample().and_then(|s| s.packed()),
        Some(&client_pixels(CROP, rich(1))[..])
    );
    assert!(cap.take_frame(&mut frame), "frame 2");
    assert_eq!((frame.seq, frame.pixels_old), (2, false));
    assert_eq!(
        frame.sample().and_then(|s| s.packed()),
        Some(&client_pixels(CROP, rich(2))[..])
    );
    loop_analyses.run(&frame, true, 0.02);
    assert_eq!(cap.take_failure(), None);
}

/// The same race, at random: a writer captures as fast as it can with a sample on every frame (a detection runs)
/// while the main thread takes frames and runs the loop's analyses; every frame taken is consistent.
#[test]
fn frames_taken_while_samples_flow_are_always_consistent() {
    let Some((gpu, d, _kind)) = setup("frames_taken_while_samples_flow_are_always_consistent") else {
        return;
    };
    let cap = synthetic_capture(&gpu, &d);
    cap.set_path(CapturePath::ZeroCopy);
    cap.set_full_rate_samples(true);
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let (d, feed, stop) = (d.clone(), cap.synthetic_feed(), stop.clone());
        std::thread::spawn(move || {
            let ring: Vec<ID3D11Texture2D> = (0..4).map(|_| window_texture(&d, WINDOW)).collect();
            let mut n = 0u64;
            while !stop.load(Ordering::Relaxed) {
                n += 1;
                let tex = &ring[(n % 4) as usize];
                upload(&d, tex, WINDOW, &window_pixels(WINDOW, CROP, tagged(n)));
                feed.push(tex, CROP).expect("push");
            }
            n
        })
    };
    let mut frame = capture::Frame::default();
    let mut loop_analyses = LoopAnalyses::new();
    let (mut taken, mut own) = (0u32, 0u32);
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let got = cap.take_frame(&mut frame);
        if got {
            taken += 1;
            assert_consistent(&frame, &format!("frame {}", frame.seq));
            own += u32::from(frame.sample().is_some());
            // A frame dropped by the render side frees its slot.
            frame.shared = None;
        }
        loop_analyses.run(&frame, got, 0.0);
    }
    stop.store(true, Ordering::Relaxed);
    let written = writer.join().expect("writer thread");
    eprintln!("{taken} frames taken ({own} with their own sample), {written} captured");
    assert!(taken > 50 && own > 0, "{taken} taken, {own} with a sample");
    assert_eq!(cap.take_failure(), None);
}

/// The loop's analyses over the zero-copy frames of one session and the next: the first frame (sampled), frames
/// without a sample (skipped as new pixels, re-analysed on the newest picture of their size), a resize (the new size
/// is sampled at once; an older sample is never read as a picture of the new size), a frame whose sample is of
/// another size, and a restart (a new capture session, the main thread's frame keeping the last session's pixels).
#[test]
fn the_loop_analyses_take_zero_copy_frames_with_or_without_a_sample() {
    let Some((gpu, d, _kind)) = setup("the_loop_analyses_take_zero_copy_frames_with_or_without_a_sample") else {
        return;
    };
    let window = window_texture(&d, WINDOW);
    let small = (8, 31, 120, 90);
    let mut frame = capture::Frame::default();
    let mut a = LoopAnalyses::new();
    let now = std::cell::Cell::new(0.0);
    let step = |cap: &capture::Capture, frame: &mut capture::Frame, a: &mut LoopAnalyses, what: &str| -> bool {
        now.set(now.get() + 0.016);
        let got = cap.take_frame(frame);
        assert_consistent(frame, what);
        a.run(frame, got, now.get());
        frame.shared = None;
        got
    };
    let push = |cap: &capture::Capture, crop: (u32, u32, u32, u32), n: u32| {
        upload(&d, &window, WINDOW, &window_pixels(WINDOW, crop, rich(n)));
        cap.synthetic_feed().push(&window, crop).expect("push");
    };

    let cap = synthetic_capture(&gpu, &d);
    cap.set_path(CapturePath::ZeroCopy);
    // Before any frame: the loop analyses the empty frame on screen.
    assert!(!step(&cap, &mut frame, &mut a, "before the first frame"));
    // The first frame: sampled (a new size).
    push(&cap, CROP, 1);
    assert!(step(&cap, &mut frame, &mut a, "first frame"));
    assert!(frame.sample().is_some(), "the first frame of a size carries its sample");
    // Frames without a sample (the next one is due in 100 ms): no own pixels, the picture of their size stays.
    for n in 2..=4 {
        push(&cap, CROP, n);
        assert!(step(&cap, &mut frame, &mut a, "unsampled frame"));
        assert!(frame.sample().is_none() && frame.last_sample().is_some(), "frame {n}");
        // The frame on screen analysed again (a map change): the newest picture of its size.
        assert!(!step(&cap, &mut frame, &mut a, "re-analysis"));
    }
    // A resize: sampled at once, at the new size.
    push(&cap, small, 5);
    assert!(step(&cap, &mut frame, &mut a, "resized frame"));
    assert_eq!((frame.width, frame.height), (small.2, small.3));
    assert_eq!(
        frame.sample().and_then(|s| s.packed()),
        Some(&client_pixels(small, rich(5))[..])
    );
    // Back to the first size, and a frame of that size the main thread takes after a newer unsampled one: the
    // buffer it gets is the sample of the size before (its own frame's), consistent.
    push(&cap, CROP, 6);
    push(&cap, CROP, 7);
    assert!(step(&cap, &mut frame, &mut a, "sample of an earlier frame"));
    assert_eq!((frame.seq, frame.pixels_old), (7, true));
    assert_eq!(
        frame.last_sample().and_then(|s| s.packed()),
        Some(&client_pixels(CROP, rich(6))[..])
    );
    // The main thread's buffer holding a picture of another size (as after a resize while it held the old sample):
    // never read for this frame.
    let mut odd = capture::Frame {
        width: small.2,
        height: small.3,
        seq: frame.seq,
        pixels_old: false,
        ..capture::Frame::cpu(CROP.2, CROP.3, std::mem::take(&mut frame.data))
    };
    assert!(odd.sample().is_none() && odd.last_sample().is_none());
    a.run(&odd, true, now.get());
    a.run(&odd, false, now.get());
    frame.data = std::mem::take(&mut odd.data);
    frame.data_size = odd.data_size;
    // A restart: a new session; the loop's frame keeps the last session's pixels (its counter back to 0).
    drop(cap);
    let cap = synthetic_capture(&gpu, &d);
    cap.set_path(CapturePath::ZeroCopy);
    frame.seq = 0;
    assert!(!step(&cap, &mut frame, &mut a, "restart, before its first frame"));
    push(&cap, small, 8);
    assert!(step(&cap, &mut frame, &mut a, "restart, first frame"));
    assert_eq!(
        frame.sample().and_then(|s| s.packed()),
        Some(&client_pixels(small, rich(8))[..])
    );
    assert_eq!(cap.take_failure(), None);
}

/// The window of the detection-quality test: a client area of 192 x 144 (x3 of 64 x 48) with room for the frame.
const QWINDOW: (u32, u32) = (240, 200);
const QCROP: (u32, u32, u32, u32) = (8, 31, 192, 144);

/// Native pixel (nx, ny) of a busy 64 x 48 picture (bright: never taken for a black bar), changing with `n`.
fn native_px(nx: u32, ny: u32, n: u32) -> [u8; 4] {
    let v = ((nx * 7 + ny * 13 + n * 5) % 160) as u8;
    [64 + v, 224 - v, 64 + (v ^ 0x2A) % 160, 255]
}

/// Captures frames at about 60 Hz on the zero-copy path with the main loop's sampling rule (every frame sampled
/// while a detection runs, else 10 per second, or always 10 per second with `floor_only`) and runs the loop's
/// analyses, until `done` or `limit`. Returns the time taken and the number of frames that carried a sample.
fn run_detection(
    gpu: &Headless,
    d: &D3d,
    inside: impl Fn(u32, u32, u32) -> [u8; 4],
    floor_only: bool,
    a: &mut LoopAnalyses,
    limit: Duration,
    done: impl Fn(&LoopAnalyses) -> bool,
) -> (Duration, u32) {
    let cap = synthetic_capture(gpu, d);
    cap.set_path(CapturePath::ZeroCopy);
    let feed = cap.synthetic_feed();
    let window = window_texture(d, QWINDOW);
    let mut frame = capture::Frame::default();
    let t0 = Instant::now();
    let mut samples = 0;
    let mut n = 0;
    while !done(a) && t0.elapsed() < limit {
        n += 1;
        cap.set_full_rate_samples(!floor_only && (!a.detector.is_locked() || a.auto.running()));
        upload(
            d,
            &window,
            QWINDOW,
            &window_pixels(QWINDOW, QCROP, |x, y| inside(x, y, n)),
        );
        feed.push(&window, QCROP).expect("push");
        let got = cap.take_frame(&mut frame);
        assert_consistent(&frame, "detection frame");
        samples += u32::from(got && frame.sample().is_some());
        a.run(&frame, got, t0.elapsed().as_secs_f64());
        frame.shared = None;
        std::thread::sleep(Duration::from_millis(15));
    }
    assert_eq!(cap.take_failure(), None);
    (t0.elapsed(), samples)
}

/// Detection quality under zero-copy: the integer-scale detection locks to x3 from the samples, with the main loop's
/// rule (every frame sampled while it runs) in well under 3 s, and from samples at the 10-per-second floor alone after
/// its 30 informative frames (about 3 s); the Auto picture area finds the picture inside black bars.
#[test]
fn zero_copy_samples_detect_the_scale_and_the_picture_area() {
    let Some((gpu, d, _kind)) = setup("zero_copy_samples_detect_the_scale_and_the_picture_area") else {
        return;
    };
    let x3 = |x: u32, y: u32, n: u32| native_px(x / 3, y / 3, n);
    let mut a = LoopAnalyses::new();
    let (t, samples) = run_detection(&gpu, &d, x3, false, &mut a, Duration::from_secs(3), |a| {
        a.detector.is_locked()
    });
    eprintln!(
        "x3, loop rule: locked {} after {t:?}, {samples} samples",
        a.detector.is_locked()
    );
    assert!(a.detector.is_locked() && t <= Duration::from_secs(3), "{t:?}");
    assert_eq!(a.detector.divisor(), (3, 3));

    let mut a = LoopAnalyses::new();
    let (t, samples) = run_detection(&gpu, &d, x3, true, &mut a, Duration::from_secs(6), |a| {
        a.detector.is_locked()
    });
    eprintln!(
        "x3, 10 samples per second: locked {} after {t:?}, {samples} samples",
        a.detector.is_locked()
    );
    assert!(a.detector.is_locked(), "not locked after {t:?} ({samples} samples)");
    assert_eq!(a.detector.divisor(), (3, 3));
    assert!(
        samples <= 32 && t <= Duration::from_secs(5),
        "{samples} samples in {t:?}"
    );

    // Black bars: 24 px left and right, 18 px top and bottom, around a 144 x 108 picture (x3 of 48 x 36).
    let picture = scaling::Rect {
        x: 24,
        y: 18,
        w: 144,
        h: 108,
    };
    let barred = move |x: u32, y: u32, n: u32| {
        if x < picture.x || x >= picture.x + picture.w || y < picture.y || y >= picture.y + picture.h {
            [0, 0, 0, 255]
        } else {
            native_px((x - picture.x) / 3, (y - picture.y) / 3, n)
        }
    };
    let mut a = LoopAnalyses::new();
    let (t, samples) = run_detection(&gpu, &d, barred, false, &mut a, Duration::from_secs(3), |a| {
        !a.events.is_empty()
    });
    eprintln!("bars: {:?} after {t:?}, {samples} samples", a.events);
    assert!(
        matches!(
            a.events[..],
            [bars::AutoEvent::Detected(
                bars::Trigger::Attach,
                bars::Outcome::Found(_)
            )]
        ),
        "{:?}",
        a.events
    );
    assert_eq!(a.auto.rect(), Some(picture));
}

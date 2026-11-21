//! Headless GPU test of the ImGui overlay (T-08): a surface-less Vulkan 1.3 device, the
//! `dear-imgui-ash` renderer in dynamic-rendering mode, an offscreen RGBA8 target read back to the
//! CPU, input fed through the Win32 message translation. No window is created.
//!
//! Skips (passes with a message) without a Vulkan 1.3 device or without the validation layer,
//! unless `PHOXIUM_REQUIRE_GPU=1`, which turns both into failures. Everything runs in one test: Dear
//! ImGui keeps its current context in a process global, so parallel test threads would race.

use std::time::Instant;

use ash::vk;
use imgui_spike::gpu::{self, barrier, Context, GpuImage, HostBuffer};
use imgui_spike::input::{self, mouse_lparam, Win32Input};
use imgui_spike::overlay::{self, Load, MenuRects, MenuState, Overlay, MENU_POS, MENU_SIZE};
use imgui_spike::Res;

const W: u32 = 320;
const H: u32 = 240;
const FORMAT: vk::Format = vk::Format::R8G8B8A8_UNORM;

/// RGBA8 image read back from the GPU.
struct Image {
    px: Vec<u8>,
}

impl Image {
    fn at(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * W + x) * 4) as usize;
        [self.px[i], self.px[i + 1], self.px[i + 2], self.px[i + 3]]
    }

    /// Pixels inside the rectangle [x0, x1) x [y0, y1) for which `f` holds.
    fn count(&self, x0: u32, y0: u32, x1: u32, y1: u32, f: impl Fn([u8; 4]) -> bool) -> usize {
        (y0..y1)
            .flat_map(|y| (x0..x1).map(move |x| (x, y)))
            .filter(|&(x, y)| f(self.at(x, y)))
            .count()
    }

    /// Mean x of the pixels for which `f` holds (None when there is none).
    fn mean_x(&self, f: impl Fn([u8; 4]) -> bool) -> Option<f32> {
        let mut n = 0u32;
        let mut sum = 0u64;
        for y in 0..H {
            for x in 0..W {
                if f(self.at(x, y)) {
                    n += 1;
                    sum += u64::from(x);
                }
            }
        }
        (n > 0).then(|| sum as f32 / n as f32)
    }
}

fn near(a: [u8; 4], b: [u8; 4], tol: i32) -> bool {
    a.iter()
        .zip(b)
        .all(|(&x, y)| (i32::from(x) - i32::from(y)).abs() <= tol)
}

/// Device, overlay, one command buffer and fence, the offscreen target and its readback buffer.
struct Rig {
    overlay: Overlay,
    pool: vk::CommandPool,
    cb: vk::CommandBuffer,
    fence: vk::Fence,
    queries: vk::QueryPool,
    target: GpuImage,
    readback: HostBuffer,
    upload: HostBuffer,
    gpu_ms: Vec<f64>,
    /// Dropped last (field order): everything above is destroyed in `Rig::destroy` first.
    ctx: Context,
}

impl Rig {
    fn new(ctx: Context) -> Res<Self> {
        // SAFETY: every object is created on `ctx.device` and destroyed in `Rig::destroy` after a
        // device idle, before `ctx` drops.
        unsafe {
            let d = &ctx.device;
            let pool = d.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(ctx.queue_family)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )?;
            let cb = d.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )?[0];
            let fence = d.create_fence(&vk::FenceCreateInfo::default(), None)?;
            let queries = d.create_query_pool(
                &vk::QueryPoolCreateInfo::default()
                    .query_type(vk::QueryType::TIMESTAMP)
                    .query_count(2),
                None,
            )?;
            let target = GpuImage::create(
                &ctx,
                vk::Extent2D { width: W, height: H },
                FORMAT,
                vk::ImageUsageFlags::COLOR_ATTACHMENT
                    | vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::TRANSFER_DST,
            )?;
            let bytes = (W * H * 4) as usize;
            let readback = HostBuffer::create(&ctx, bytes, vk::BufferUsageFlags::TRANSFER_DST)?;
            let upload = HostBuffer::create(&ctx, bytes, vk::BufferUsageFlags::TRANSFER_SRC)?;
            let overlay = Overlay::new(&ctx, FORMAT, 1)?;
            Ok(Self {
                overlay,
                pool,
                cb,
                fence,
                queries,
                target,
                readback,
                upload,
                gpu_ms: Vec::new(),
                ctx,
            })
        }
    }

    /// One frame: optional background upload (else clear to `clear`), the overlay pass (built by
    /// `build` after feeding `events`), readback. Waits for the GPU.
    fn frame(
        &mut self,
        background: Option<&[u8]>,
        clear: [f32; 4],
        events: &[input::UiEvent],
        build: impl FnOnce(&dear_imgui_rs::Ui),
    ) -> Res<Image> {
        self.overlay.feed(events);
        // SAFETY: single command buffer and fence, waited before returning; all handles belong to
        // `self.ctx.device`; the mapped buffers are accessed within their sizes after the fence.
        unsafe {
            let d = &self.ctx.device;
            let cb = self.cb;
            d.reset_command_buffer(cb, vk::CommandBufferResetFlags::empty())?;
            d.begin_command_buffer(
                cb,
                &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            d.cmd_reset_query_pool(cb, self.queries, 0, 2);
            let img = self.target.image;
            let load = if let Some(bg) = background {
                std::ptr::copy_nonoverlapping(bg.as_ptr(), self.upload.mapped, bg.len());
                barrier(
                    d,
                    cb,
                    img,
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::NONE,
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_WRITE,
                );
                d.cmd_copy_buffer_to_image(
                    cb,
                    self.upload.buffer,
                    img,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[copy_region()],
                );
                barrier(
                    d,
                    cb,
                    img,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_WRITE,
                    vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                    vk::AccessFlags2::COLOR_ATTACHMENT_READ | vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
                );
                Load::Keep
            } else {
                barrier(
                    d,
                    cb,
                    img,
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::NONE,
                    vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                    vk::AccessFlags2::COLOR_ATTACHMENT_READ | vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
                );
                Load::Clear(clear)
            };
            d.cmd_write_timestamp2(cb, vk::PipelineStageFlags2::ALL_COMMANDS, self.queries, 0);
            self.overlay.record(
                cb,
                self.target.view,
                vk::Extent2D { width: W, height: H },
                load,
                1.0 / 60.0,
                build,
            )?;
            d.cmd_write_timestamp2(cb, vk::PipelineStageFlags2::ALL_COMMANDS, self.queries, 1);
            barrier(
                d,
                cb,
                img,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
                vk::PipelineStageFlags2::COPY,
                vk::AccessFlags2::TRANSFER_READ,
            );
            d.cmd_copy_image_to_buffer(
                cb,
                img,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                self.readback.buffer,
                &[copy_region()],
            );
            let host = [vk::MemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::COPY)
                .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::HOST)
                .dst_access_mask(vk::AccessFlags2::HOST_READ)];
            d.cmd_pipeline_barrier2(cb, &vk::DependencyInfo::default().memory_barriers(&host));
            d.end_command_buffer(cb)?;
            let cbs = [vk::CommandBufferSubmitInfo::default().command_buffer(cb)];
            let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cbs)];
            d.queue_submit2(self.ctx.queue, &submit, self.fence)?;
            d.wait_for_fences(&[self.fence], true, u64::MAX)?;
            d.reset_fences(&[self.fence])?;
            self.overlay.after_gpu_idle()?;
            if self.ctx.timestamp_valid_bits > 0 {
                let mut ts = [0u64; 2];
                d.get_query_pool_results(self.queries, 0, &mut ts, vk::QueryResultFlags::TYPE_64)?;
                self.gpu_ms
                    .push(ts[1].wrapping_sub(ts[0]) as f64 * f64::from(self.ctx.timestamp_period_ns) * 1e-6);
            }
            let px = std::slice::from_raw_parts(self.readback.mapped, self.readback.size).to_vec();
            Ok(Image { px })
        }
    }

    fn destroy(mut self) -> Res<gpu::ValidationCounter> {
        let counter = self.ctx.validation_counter();
        self.overlay.destroy()?;
        // SAFETY: the overlay waited for the device; nothing else is in flight.
        unsafe {
            let d = &self.ctx.device;
            d.device_wait_idle()?;
            self.upload.destroy(d);
            self.readback.destroy(d);
            self.target.destroy(d);
            d.destroy_query_pool(self.queries, None);
            d.destroy_fence(self.fence, None);
            d.destroy_command_pool(self.pool, None);
        }
        drop(self.ctx);
        Ok(counter)
    }
}

fn copy_region() -> vk::BufferImageCopy {
    vk::BufferImageCopy::default()
        .image_subresource(vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: 0,
            base_array_layer: 0,
            layer_count: 1,
        })
        .image_extent(vk::Extent3D {
            width: W,
            height: H,
            depth: 1,
        })
}

/// Builds the menu with `state` and records the widget rectangles.
fn menu<'a>(state: &'a mut MenuState, rects: &'a mut MenuRects) -> impl FnOnce(&dear_imgui_rs::Ui) + 'a {
    move |ui| *rects = overlay::settings_menu(ui, state)
}

/// Feeds one Win32 message through the translation (no window: the numbers a window procedure
/// would receive).
fn msg(conv: &mut Win32Input, m: u32, wparam: usize, lparam: isize) -> Vec<input::UiEvent> {
    let mut out = Vec::new();
    conv.translate(m, wparam, lparam, &mut out);
    out
}

fn at(p: [f32; 2]) -> isize {
    mouse_lparam(p[0] as i16, p[1] as i16)
}

const BLACK: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const RED: [u8; 4] = [255, 0, 0, 255];

#[test]
fn imgui_overlay_headless() {
    let require = gpu::gpu_required();
    let validation = gpu::validation_layer_available();
    assert!(
        validation || !require,
        "PHOXIUM_REQUIRE_GPU=1 but the validation layer is not installed"
    );
    let ctx = match Context::new_headless(validation) {
        Ok(c) => c,
        Err(e) => {
            assert!(!require, "PHOXIUM_REQUIRE_GPU=1 but no headless Vulkan device: {e}");
            eprintln!("imgui_overlay_headless skipped: {e}");
            return;
        }
    };
    eprintln!(
        "device: {}, validation {}, synchronization validation {}",
        ctx.gpu_name,
        ctx.validation_enabled(),
        ctx.sync_validation_enabled()
    );
    let t0 = Instant::now();
    let mut rig = Rig::new(ctx).expect("rig");
    eprintln!("overlay init: {:.1} ms", t0.elapsed().as_secs_f64() * 1e3);

    // --- 1. Opaque menu over a cleared target. ---
    overlay::test_style(&mut rig.overlay.imgui, [0.2, 0.4, 0.6, 1.0]);
    let mut state = MenuState {
        exposure: 0.0,
        ..MenuState::default()
    };
    let mut rects = MenuRects::default();
    // A few frames so that the font atlas exists and the layout is settled.
    let mut img = None;
    for _ in 0..3 {
        img = Some(
            rig.frame(None, BLACK, &[], menu(&mut state, &mut rects))
                .expect("frame"),
        );
    }
    let img = img.expect("image");
    let bg_px = [51, 102, 153, 255];
    let changed = img.count(0, 0, W, H, |p| p != [0, 0, 0, 255]);
    assert!(changed > 10_000, "the overlay drew only {changed} pixels");
    // Window background at a known pixel (bottom-left inside the window, away from widgets).
    let (wx, wy) = (MENU_POS[0] as u32 + 5, (MENU_POS[1] + MENU_SIZE[1]) as u32 - 5);
    assert!(
        near(img.at(wx, wy), bg_px, 1),
        "window bg at ({wx},{wy}) = {:?}",
        img.at(wx, wy)
    );
    // Outside the window the clear colour is untouched.
    assert_eq!(img.at(W - 3, H - 3), [0, 0, 0, 255]);
    // Text: the labels are drawn in pure green; the bitmap font has fully covered pixels.
    let text_px = img.count(0, 0, W, H, |p| p == GREEN);
    assert!(text_px > 100, "only {text_px} text pixels");
    // Slider grab (pure red) position for value 0.
    let x0 = img.mean_x(|p| p == RED).expect("slider grab visible at 0");
    eprintln!("rects: {rects:?}");

    state.exposure = 1.0;
    let img1 = rig
        .frame(None, BLACK, &[], menu(&mut state, &mut rects))
        .expect("frame");
    let x1 = img1.mean_x(|p| p == RED).expect("slider grab visible at 1");
    assert!(x1 > x0 + 100.0, "slider grab did not move: {x0} -> {x1}");

    // Text field content is rendered: more green pixels with a longer string.
    state.name = "imgui spike".into();
    let img2 = rig
        .frame(None, BLACK, &[], menu(&mut state, &mut rects))
        .expect("frame");
    let text_px2 = img2.count(0, 0, W, H, |p| p == GREEN);
    assert!(
        text_px2 > text_px + 30,
        "typed text not visible: {text_px} -> {text_px2}"
    );

    // --- 2. Input through the Win32 message translation (no window). ---
    state = MenuState {
        exposure: 0.0,
        ..MenuState::default()
    };
    let mut conv = Win32Input::new();
    let step = |rig: &mut Rig, state: &mut MenuState, rects: &mut MenuRects, ev: Vec<input::UiEvent>| {
        rig.frame(None, BLACK, &ev, menu(state, rects)).expect("input frame")
    };
    step(
        &mut rig,
        &mut state,
        &mut rects,
        msg(&mut conv, input::WM_SETFOCUS, 0, 0),
    );
    // Drag the slider: press near its left end, move to 90 %, release.
    let press = MenuRects::at(rects.slider, 0.05, 0.5);
    let drag = MenuRects::at(rects.slider, 0.9, 0.5);
    step(
        &mut rig,
        &mut state,
        &mut rects,
        msg(&mut conv, input::WM_MOUSEMOVE, 0, at(press)),
    );
    step(
        &mut rig,
        &mut state,
        &mut rects,
        msg(&mut conv, input::WM_LBUTTONDOWN, 1, at(press)),
    );
    let after_press = state.exposure;
    step(
        &mut rig,
        &mut state,
        &mut rects,
        msg(&mut conv, input::WM_MOUSEMOVE, 1, at(drag)),
    );
    step(
        &mut rig,
        &mut state,
        &mut rects,
        msg(&mut conv, input::WM_LBUTTONUP, 0, at(drag)),
    );
    eprintln!("slider: press {after_press:.3}, after drag {:.3}", state.exposure);
    assert!(after_press < 0.2, "press at the left end gave {after_press}");
    assert!(state.exposure > 0.8, "drag to 90 % gave {}", state.exposure);

    // Click into the text field and type "hi!", Backspace, Enter.
    let field = MenuRects::at(rects.text, 0.3, 0.5);
    step(
        &mut rig,
        &mut state,
        &mut rects,
        msg(&mut conv, input::WM_MOUSEMOVE, 0, at(field)),
    );
    step(
        &mut rig,
        &mut state,
        &mut rects,
        msg(&mut conv, input::WM_LBUTTONDOWN, 1, at(field)),
    );
    step(
        &mut rig,
        &mut state,
        &mut rects,
        msg(&mut conv, input::WM_LBUTTONUP, 0, at(field)),
    );
    for c in ['h', 'i', '!'] {
        step(
            &mut rig,
            &mut state,
            &mut rects,
            msg(&mut conv, input::WM_CHAR, c as usize, 1),
        );
    }
    let bs = 0x08;
    step(
        &mut rig,
        &mut state,
        &mut rects,
        msg(&mut conv, input::WM_KEYDOWN, bs, input::key_lparam(0x0E, false, false)),
    );
    step(
        &mut rig,
        &mut state,
        &mut rects,
        msg(&mut conv, input::WM_KEYUP, bs, input::key_lparam(0x0E, false, true)),
    );
    assert_eq!(state.name, "hi", "typed text");

    // Click the button.
    let button = MenuRects::at(rects.button, 0.5, 0.5);
    step(
        &mut rig,
        &mut state,
        &mut rects,
        msg(&mut conv, input::WM_MOUSEMOVE, 0, at(button)),
    );
    step(
        &mut rig,
        &mut state,
        &mut rects,
        msg(&mut conv, input::WM_LBUTTONDOWN, 1, at(button)),
    );
    step(
        &mut rig,
        &mut state,
        &mut rects,
        msg(&mut conv, input::WM_LBUTTONUP, 0, at(button)),
    );
    assert_eq!(state.applied, 1, "button click");

    // Open the combo and pick the third entry with the keyboard (Down, Down, Enter).
    let combo = MenuRects::at(rects.combo, 0.3, 0.5);
    step(
        &mut rig,
        &mut state,
        &mut rects,
        msg(&mut conv, input::WM_MOUSEMOVE, 0, at(combo)),
    );
    step(
        &mut rig,
        &mut state,
        &mut rects,
        msg(&mut conv, input::WM_LBUTTONDOWN, 1, at(combo)),
    );
    step(
        &mut rig,
        &mut state,
        &mut rects,
        msg(&mut conv, input::WM_LBUTTONUP, 0, at(combo)),
    );
    step(&mut rig, &mut state, &mut rects, Vec::new());
    for (vk, scan) in [(0x28usize, 0x50u32), (0x28, 0x50), (0x0D, 0x1C)] {
        let ext = vk == 0x28;
        step(
            &mut rig,
            &mut state,
            &mut rects,
            msg(&mut conv, input::WM_KEYDOWN, vk, input::key_lparam(scan, ext, false)),
        );
        step(
            &mut rig,
            &mut state,
            &mut rects,
            msg(&mut conv, input::WM_KEYUP, vk, input::key_lparam(scan, ext, true)),
        );
    }
    eprintln!("combo after Down, Down, Enter: {}", state.mask);
    assert_eq!(state.mask, 2, "combo keyboard selection");

    // --- 3. 50 % transparent window over an arbitrary background: blend within 1 code. ---
    overlay::test_style(&mut rig.overlay.imgui, [0.0, 0.0, 1.0, 0.5]);
    let bg: Vec<u8> = (0..H)
        .flat_map(|y| (0..W).flat_map(move |x| [(x * 255 / W) as u8, (y * 255 / H) as u8, 200, 255]))
        .collect();
    state = MenuState::default();
    let blended = rig
        .frame(Some(&bg), BLACK, &[], menu(&mut state, &mut rects))
        .expect("blend frame");
    let a = 128.0 / 255.0; // ImGui packs the style alpha 0.5 into a u8: 128.
    let mut worst = 0i32;
    for (x, y) in [(wx, wy), (wx + 40, wy - 3), (wx + 200, wy - 10)] {
        let i = ((y * W + x) * 4) as usize;
        let dst = [bg[i], bg[i + 1], bg[i + 2]];
        let src = [0.0f32, 0.0, 255.0];
        let expect: Vec<i32> = (0..3)
            .map(|c| (src[c] * a + f32::from(dst[c]) * (1.0 - a)).round() as i32)
            .collect();
        let got = blended.at(x, y);
        for c in 0..3 {
            worst = worst.max((i32::from(got[c]) - expect[c]).abs());
        }
        assert_eq!(got[3], 255, "alpha at ({x},{y})");
        eprintln!("blend ({x},{y}): bg {dst:?} got {got:?} expected {expect:?}");
    }
    assert!(worst <= 1, "blend error {worst} codes");
    // Outside the window the background is untouched.
    let i = (((H - 3) * W + W - 3) * 4) as usize;
    assert_eq!(blended.at(W - 3, H - 3), [bg[i], bg[i + 1], bg[i + 2], bg[i + 3]]);

    if !rig.gpu_ms.is_empty() {
        let mut v = rig.gpu_ms.clone();
        v.sort_by(f64::total_cmp);
        eprintln!(
            "overlay pass GPU time over {} frames ({}x{}): median {:.4} ms, max {:.4} ms",
            v.len(),
            W,
            H,
            v[v.len() / 2],
            v[v.len() - 1]
        );
    }
    let counter = rig.destroy().expect("destroy");
    if validation {
        assert_eq!(counter.count(), 0, "validation messages");
        eprintln!("validation messages: 0");
    }
}

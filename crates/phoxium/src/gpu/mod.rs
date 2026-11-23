//! Vulkan renderer: uploads the captured frame and runs it through the pipeline of GLSL stages of
//! `shaders/` into the swapchain. Vulkan 1.3 with dynamic rendering and synchronization2.
//!
//! Modules: `context` (instance, device choice, queue; with or without a window), `present`
//! (one `Presenter` per window), `pipeline` (the stage chain, its images and frames in flight),
//! `stages` (one module per stage), `ui_render` (the settings menu and overlay pass after `encode`,
//! T-52a). `Renderer` is the facade used by the application.
//! `headless` runs the same pipeline into an offscreen image and reads it back, without any window
//! (tests).
//!
//! Failures (R-01): what the renderer can recover from is not an error. A minimised window, a 0 x 0 surface, an
//! out-of-date or suboptimal swapchain and a lost surface give a [`FrameStatus`] (the swapchain or surface is
//! recreated before the next frame); a frame whose tube area is empty or too large for the GPU draws the surround
//! only, with a notice ([`Renderer::gpu_notice`]). A lost device (`ERROR_DEVICE_LOST`, or a fence that does not
//! signal within [`FENCE_TIMEOUT`]) is a [`GpuError::DeviceLost`]: the application saves its state and stops;
//! rebuilding the renderer after a device loss is not done yet.

use std::time::Duration;

use ash::vk;

use crate::params::Params;
use crate::scaling::View;

use crate::Res;

/// What [`Renderer::render`] did with a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameStatus {
    /// The frame was drawn and presented.
    Presented,
    /// Nothing to draw into: the window is minimised or its surface has a 0 side. The frame (if any) is staged
    /// for later. The caller should wait (about one refresh period, for messages) before the next call instead of
    /// calling again at once: nothing paces the loop while minimised.
    NothingToDraw,
    /// The frame was skipped while the swapchain or the surface is recreated (out of date, lost surface, or no
    /// image free within the acquire timeout): the next call draws normally.
    Skipped,
}

/// How long a wait for a frame's fence may take before the GPU counts as hung (R-01): longer than the Windows GPU
/// timeout (TDR, 2 s by default), so a real reset shows as `ERROR_DEVICE_LOST` first, and a wait beyond it means
/// the driver no longer answers.
pub const FENCE_TIMEOUT: Duration = Duration::from_secs(5);

/// A GPU failure the application must handle as such (R-01): it reads it with
/// `error.downcast_ref::<GpuError>()` and tells the user, after saving its state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GpuError {
    /// The device is gone: `ERROR_DEVICE_LOST` (driver reset after a timeout, driver update, GPU removed or switched),
    /// or `hung` when a fence did not signal within [`FENCE_TIMEOUT`]. The renderer cannot be used any more.
    DeviceLost { during: &'static str, hung: bool },
    /// The output window's surface was lost and could not be recreated.
    SurfaceLost { detail: String },
    /// Out of host or GPU memory.
    OutOfMemory { during: &'static str },
    /// No GPU of the machine has what the renderer needs (device selection, [`Context`]).
    Unsupported { reason: String },
}

impl GpuError {
    /// True for [`GpuError::DeviceLost`]: the renderer is unusable and the application should save and stop.
    pub fn is_device_lost(&self) -> bool {
        matches!(self, GpuError::DeviceLost { .. })
    }
}

impl std::fmt::Display for GpuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GpuError::DeviceLost { during, hung: false } => {
                write!(f, "GPU device lost (driver reset or update) while {during}")
            }
            GpuError::DeviceLost { during, hung: true } => write!(
                f,
                "GPU device lost (driver reset or update): the GPU did not finish {during} within {} s",
                FENCE_TIMEOUT.as_secs()
            ),
            GpuError::SurfaceLost { detail } => write!(f, "the output window's Vulkan surface was lost: {detail}"),
            GpuError::OutOfMemory { during } => write!(f, "out of GPU or system memory while {during}"),
            GpuError::Unsupported { reason } => write!(f, "this GPU is not supported: {reason}"),
        }
    }
}

impl std::error::Error for GpuError {}

/// What a Vulkan result means for the renderer ([`classify`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VkClass {
    /// `SUCCESS`.
    Ok,
    /// `SUBOPTIMAL_KHR`: works, recreate the swapchain when convenient.
    Suboptimal,
    /// `ERROR_OUT_OF_DATE_KHR`: recreate the swapchain, skip the frame.
    OutOfDate,
    /// `ERROR_SURFACE_LOST_KHR`: recreate the surface and the swapchain, skip the frame.
    SurfaceLost,
    /// `ERROR_DEVICE_LOST`: fatal for the renderer.
    DeviceLost,
    /// `TIMEOUT` or `NOT_READY` from a bounded wait.
    Timeout,
    /// `ERROR_OUT_OF_HOST_MEMORY`, `ERROR_OUT_OF_DEVICE_MEMORY`.
    OutOfMemory,
    /// Anything else (an error of the program or the driver).
    Other,
}

/// Classifies a Vulkan result (pure; R-01).
pub fn classify(r: vk::Result) -> VkClass {
    match r {
        vk::Result::SUCCESS => VkClass::Ok,
        vk::Result::SUBOPTIMAL_KHR => VkClass::Suboptimal,
        vk::Result::ERROR_OUT_OF_DATE_KHR => VkClass::OutOfDate,
        vk::Result::ERROR_SURFACE_LOST_KHR => VkClass::SurfaceLost,
        vk::Result::ERROR_DEVICE_LOST => VkClass::DeviceLost,
        vk::Result::TIMEOUT | vk::Result::NOT_READY => VkClass::Timeout,
        vk::Result::ERROR_OUT_OF_HOST_MEMORY | vk::Result::ERROR_OUT_OF_DEVICE_MEMORY => VkClass::OutOfMemory,
        _ => VkClass::Other,
    }
}

/// The error a failed Vulkan call becomes: [`GpuError`] for a lost device or surface and for lack of memory,
/// the plain `vk::Result` otherwise.
pub fn vk_error(r: vk::Result, during: &'static str) -> Box<dyn std::error::Error> {
    match classify(r) {
        VkClass::DeviceLost => Box::new(GpuError::DeviceLost { during, hung: false }),
        VkClass::OutOfMemory => Box::new(GpuError::OutOfMemory { during }),
        VkClass::SurfaceLost => Box::new(GpuError::SurfaceLost {
            detail: format!("{r} while {during}"),
        }),
        _ => Box::new(r),
    }
}

/// Lifts an error that is a bare `vk::Result` (any `?` on an ash call) to its [`vk_error`] form; other errors are
/// returned as they are.
pub fn lift(e: Box<dyn std::error::Error>, during: &'static str) -> Box<dyn std::error::Error> {
    match e.downcast_ref::<vk::Result>() {
        Some(&r) => vk_error(r, during),
        None => e,
    }
}

/// Waits for `fences` at most [`FENCE_TIMEOUT`]: a timeout is a hung GPU, [`GpuError::DeviceLost`] with `hung`
/// (never an endless wait, R-01).
///
/// # Safety
/// The fences are live fences of `d` that a submission will signal (or already did).
pub unsafe fn wait_fences(d: &ash::Device, fences: &[vk::Fence], during: &'static str) -> Res<()> {
    match d.wait_for_fences(fences, true, FENCE_TIMEOUT.as_nanos() as u64) {
        Ok(()) => Ok(()),
        Err(r) if classify(r) == VkClass::Timeout => Err(Box::new(GpuError::DeviceLost { during, hung: true })),
        Err(r) => Err(vk_error(r, during)),
    }
}

mod context;
mod external;
mod headless;
mod pipeline;
mod present;
mod stages;
mod timing;
mod ui_render;

pub use timing::{csv_columns, FrameTimings, StageTiming, TimingAverager};
pub use ui_render::UiPass;

pub use context::{device_meets, gpu_required, validation_layer_available, DeviceCaps, ValidationCounter};
pub use context::{list_gpus, vulkan_report, Context, DeviceRequest, ExternalMemoryProbe, GpuInfo, VulkanReport};
pub use headless::{Headless, ImageF32, Rendered, SourceFrame, TargetFormat};
pub use headless::{StageInput, StageReadback, StageRun};
pub use pipeline::{tube_rect_letterbox, FrameConstants, Settings, Transfer, FRAMES_IN_FLIGHT, NATIVE_FORMAT};
pub use present::{extent_for, Acquired, Presenter};
pub use stages::encode::{output_matrix, ClipCounts};
pub use stages::screen::{warp_plan, WarpPlan, WARP_SIGMA_STEPS};
pub use stages::{Domain, AMP_K};

use pipeline::{Pipeline, Target};

use crate::capture::SharedFrame;
use crate::zerocopy::{HandleKind, InFlight};

/// A new source frame for [`Renderer::render_source`].
pub enum Upload<'a> {
    /// A captured BGRA8 frame in CPU memory (CPU path, patterns, images): width, height, pixels.
    Cpu(u32, u32, &'a [u8]),
    /// A frame in a shared texture of the zero-copy capture path (A29): sampled in place, never copied.
    Shared(SharedFrame),
}

/// A snapshot of the output: the picture the CRT chain drew, as it was presented, without the menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotImage {
    pub width: u32,
    pub height: u32,
    /// RGBA8, top-down, tightly packed, alpha 255.
    pub rgba: Vec<u8>,
}

/// The pixels of a swapchain image copied to a buffer (`raw`, tightly packed, 4 bytes per pixel) as RGBA8 with alpha
/// 255; `format` is the swapchain format (`B8G8R8A8_UNORM` swaps red and blue, `R8G8B8A8_UNORM` keeps the order).
pub fn swapchain_to_rgba(format: vk::Format, raw: &[u8]) -> Vec<u8> {
    let bgra = format == vk::Format::B8G8R8A8_UNORM;
    let mut out = Vec::with_capacity(raw.len());
    for p in raw.as_chunks::<4>().0 {
        if bgra {
            out.extend_from_slice(&[p[2], p[1], p[0], 255]);
        } else {
            out.extend_from_slice(&[p[0], p[1], p[2], 255]);
        }
    }
    out
}

/// Renders the captured frame into one output window.
///
/// Every Vulkan handle starts null and is filled in by `new`, so `Drop` releases whatever was
/// created even when `new` fails half way (destroying a null handle is a no-op in Vulkan).
pub struct Renderer {
    pub gpu_name: String,
    presenter: Presenter,
    pipeline: Option<Pipeline>,
    /// Signalled by the acquire of frame slot i, waited by that slot's submission.
    image_available: [vk::Semaphore; FRAMES_IN_FLIGHT],
    settings: Settings,
    params: Params,
    /// Tiers the automatic quality drop takes off the selected tier (T-48, `quality::AutoQuality`), 0 normally.
    quality_drop: u32,
    /// Clip counters of the last frame that finished (T-46), refreshed by `render`.
    clip: ClipCounts,
    /// Set by the first device loss: every later call returns it without touching the device.
    lost: Option<GpuError>,
    /// Zero-copy (A29): the shared slots the renderer holds (the source, and those read by frames in flight).
    shared: InFlight<SharedFrame, FRAMES_IN_FLIGHT>,
    /// Why a shared frame could not be used (import or staging failed), until [`Renderer::take_external_failure`].
    external_failure: Option<String>,
    /// The device offers `VK_KHR_external_memory_win32` (queried once).
    zero_copy_offered: bool,
    /// A snapshot of the output was asked: the next frame is drawn without the menu and copied out ([`Renderer::request_snapshot`]).
    snapshot_wanted: bool,
    /// Host buffer the snapshot copy lands in (kept between snapshots; null until the first).
    snapshot_buffer: headless::HostBuffer,
    /// Pixels of `snapshot_buffer` (width, height) as it was made.
    snapshot_size: (u32, u32),
    /// The finished snapshot, until [`Renderer::take_snapshot`].
    snapshot_done: Option<SnapshotImage>,
    /// Destroyed last: `Drop for Renderer` releases every child object above first, then the
    /// field drop of `ctx` destroys the device and the instance.
    ctx: Context,
}

impl Renderer {
    /// Creates the Vulkan context on a device that can present to `hwnd` (the requested one, see
    /// [`DeviceRequest`]; the chosen device is [`Context::gpu`]), its presenter and the pipeline. The device gets
    /// `VK_KHR_external_memory_win32` whenever it offers it and imports the shared Direct3D 11 textures
    /// ([`Renderer::zero_copy_support`]), whatever the capture path asked at start, so the path can be switched both
    /// ways during the session (A29). The CPU path does not use the extension: its output is the same with or
    /// without it (`tests/zero_copy.rs`).
    pub fn new(hwnd: isize, hinstance: isize, gpu: DeviceRequest, validation: bool, integer_scale: bool) -> Res<Self> {
        // SAFETY: `hwnd` is the live output window of `hinstance`, owned by the application, which
        // drops the renderer before destroying the window. The presenter and pipeline are destroyed
        // by `Drop for Renderer` before `ctx`.
        unsafe {
            let (ctx, surface) = Context::with_window(hwnd, hinstance, gpu, validation, true)?;
            let presenter = Presenter::from_surface(&ctx, surface, Some((hwnd, hinstance)), hwnd_client_size(hwnd))?;
            let mut r = Self {
                gpu_name: ctx.gpu.name.clone(),
                presenter,
                pipeline: None,
                image_available: [vk::Semaphore::null(); FRAMES_IN_FLIGHT],
                settings: Settings {
                    integer_scale,
                    ..Settings::default()
                },
                params: Params::default(),
                quality_drop: 0,
                clip: ClipCounts::default(),
                lost: None,
                shared: InFlight::default(),
                external_failure: None,
                zero_copy_offered: ctx.external_memory_offered(),
                snapshot_wanted: false,
                snapshot_buffer: headless::HostBuffer::default(),
                snapshot_size: (0, 0),
                snapshot_done: None,
                ctx,
            };
            for s in &mut r.image_available {
                *s = r
                    .ctx
                    .device
                    .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?;
            }
            r.pipeline = Some(Pipeline::new(&r.ctx)?);
            Ok(r)
        }
    }

    /// Replaces the tube parameters used from the next frame on (the startup preset and settings overrides).
    pub fn set_params(&mut self, params: Params) {
        self.params = params;
    }

    /// The automatic quality drop in force (T-48, OUT-6): the frames are rendered `drop` tiers below the selected
    /// `quality_tier` ([`crate::quality::effective`]); the parameters themselves are not changed.
    pub fn set_quality_drop(&mut self, drop: u32) {
        self.quality_drop = drop;
    }

    /// Renders one frame. `frame` is a newly captured BGRA8 image, if any. `client` is the current
    /// client size of the output window.
    ///
    /// A given frame is always staged for upload, also when the window is minimised or the swapchain
    /// is out of date (the frame is then not drawn but kept for the next call), so the caller may
    /// consider it consumed once this returns `Ok`. The status says whether anything was drawn
    /// ([`FrameStatus::NothingToDraw`]: the caller waits before calling again). An `Err` holding a
    /// [`GpuError::DeviceLost`] means the renderer is unusable (every later call returns it again).
    pub fn render(&mut self, frame: Option<(u32, u32, &[u8])>, view: View, client: (u32, u32)) -> Res<FrameStatus> {
        self.render_with_ui(frame, view, client, None)
    }

    /// The GPU-side notice in force, for the title and the overlay: what the renderer limited or left out because
    /// of the GPU or the parameters (amplifier samples limited to the image size the GPU allows, an empty or
    /// too large tube area, the line images of the warp path), `None` when nothing is limited. Each one is also
    /// printed once (`notice: ...`) when it appears.
    pub fn gpu_notice(&self) -> Option<String> {
        self.pipeline.as_ref().and_then(Pipeline::notice)
    }

    /// The notices that appeared since the previous call, each once (for a log or a toast).
    pub fn take_gpu_notices(&mut self) -> Vec<String> {
        self.pipeline.as_mut().map(Pipeline::take_notices).unwrap_or_default()
    }

    /// The Vulkan context (device, queue, chosen GPU): the settings menu (`crate::ui::Ui`) creates its
    /// renderer on it, and must be dropped before the renderer.
    pub fn context(&self) -> &Context {
        &self.ctx
    }

    /// Presents without waiting for the vertical blank (tearing) or with vsync; the swapchain is recreated before
    /// the next frame when this changes. Without tearing support in the surface the mode stays vsync.
    pub fn set_tearing(&mut self, on: bool) {
        self.presenter.set_tearing(on);
    }

    /// Whether frames are presented without waiting for the vertical blank now (the swapchain was made so).
    pub fn tearing_active(&self) -> bool {
        self.presenter.present_mode() == vk::PresentModeKHR::IMMEDIATE
    }

    /// Format of the swapchain images (`B8G8R8A8_UNORM` or `R8G8B8A8_UNORM`, A7): the UI renderer is created
    /// for it.
    pub fn output_format(&self) -> vk::Format {
        self.presenter.format()
    }

    /// [`Renderer::render`] with the settings menu and overlay (T-52a) drawn over the output after `encode`,
    /// when `ui` has something to draw; nothing more is recorded otherwise.
    pub fn render_with_ui(
        &mut self,
        frame: Option<(u32, u32, &[u8])>,
        view: View,
        client: (u32, u32),
        ui: Option<&mut dyn UiPass>,
    ) -> Res<FrameStatus> {
        self.render_source(frame.map(|(w, h, d)| Upload::Cpu(w, h, d)), view, client, ui)
    }

    /// [`Renderer::render_with_ui`] with a source frame of either path ([`Upload`]). A shared frame that cannot be
    /// used (the device cannot import, the import fails) is not an error: it is dropped, the previous source stays,
    /// and the reason waits in [`Renderer::take_external_failure`] (the application falls back to the CPU copy). A
    /// lost device stays an error.
    pub fn render_source(
        &mut self,
        frame: Option<Upload<'_>>,
        view: View,
        client: (u32, u32),
        ui: Option<&mut dyn UiPass>,
    ) -> Res<FrameStatus> {
        if let Some(e) = &self.lost {
            return Err(Box::new(e.clone()));
        }
        let r = self.render_frame(frame, view, client, ui);
        r.map_err(|e| {
            let e = lift(e, "rendering a frame");
            if let Some(g) = e.downcast_ref::<GpuError>().filter(|g| g.is_device_lost()) {
                self.lost = Some(g.clone());
            }
            e
        })
    }

    /// Body of [`Renderer::render_with_ui`].
    fn render_frame(
        &mut self,
        frame: Option<Upload<'_>>,
        view: View,
        client: (u32, u32),
        ui: Option<&mut dyn UiPass>,
    ) -> Res<FrameStatus> {
        let pipeline = self.pipeline.as_mut().expect("pipeline created in new");
        // SAFETY: every handle used below belongs to `self.ctx`. `wait_slot` guarantees the slot's
        // previous submission finished, so its command buffer, staging and uniform buffers and its
        // image-available semaphore are free.
        unsafe {
            pipeline.wait_slot(&self.ctx)?;
            // The frame this slot submitted last is done: the shared slot it read, if any, may go back.
            self.shared.frame_done(pipeline.slot());
            self.clip = pipeline.clip_counts();
            match frame {
                Some(Upload::Cpu(w, h, data)) => {
                    pipeline.stage_source(&self.ctx, w, h, data)?;
                    self.shared.clear_current();
                }
                Some(Upload::Shared(f)) => {
                    stage_shared(&self.ctx, pipeline, &mut self.shared, f, &mut self.external_failure)?
                }
                None => {}
            }
            // Minimised, or the surface has no extent now (the old swapchain is kept): nothing to draw into.
            if !self.presenter.ensure(&self.ctx, client)? {
                return Ok(FrameStatus::NothingToDraw);
            }
            // Everything that can fail for a reason other than the device or the surface (settings,
            // pipeline creation, image allocation) is done before the acquire.
            let params = crate::quality::effective(&self.params, self.quality_drop);
            pipeline.prepare(
                &self.ctx,
                view,
                &self.settings,
                &params,
                self.presenter.extent(),
                self.presenter.format(),
            )?;

            let slot = pipeline.slot();
            let acquired = self.image_available[slot];
            let index = match self.presenter.acquire(&self.ctx, acquired)? {
                Acquired::Image { index, .. } => index,
                Acquired::Skip => return Ok(FrameStatus::Skipped),
            };
            if let Err(e) = self.draw(index, acquired, view, ui) {
                self.recover_after_acquire(slot);
                return Err(e);
            }
        }
        Ok(FrameStatus::Presented)
    }

    /// Records, submits and presents the frame into the acquired image `index`.
    ///
    /// # Safety
    /// As [`Renderer::render`]; `index` was acquired with `acquired` as its semaphore.
    unsafe fn draw(&mut self, index: u32, acquired: vk::Semaphore, view: View, ui: Option<&mut dyn UiPass>) -> Res<()> {
        // A snapshot is drawn without the menu: this one frame has no UI on it, and the copy leaves for the host.
        let snapshot = std::mem::take(&mut self.snapshot_wanted) && self.presenter.can_copy();
        let ui = if snapshot { None } else { ui };
        if snapshot {
            self.ensure_snapshot_buffer()?;
        }
        let pipeline = self.pipeline.as_mut().expect("pipeline created in new");
        let target = Target {
            image: self.presenter.image(index),
            view: self.presenter.view(index),
            extent: self.presenter.extent(),
            format: self.presenter.format(),
        };
        let params = crate::quality::effective(&self.params, self.quality_drop);
        let cb = pipeline.record_ui(&self.ctx, view, &self.settings, &params, &target, ui)?;
        if snapshot {
            // Colour attachment to transfer source, copy the picture out, transfer source to present.
            barrier(
                &self.ctx.device,
                cb,
                target.image,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
                vk::PipelineStageFlags2::COPY,
                vk::AccessFlags2::TRANSFER_READ,
            );
            self.ctx.device.cmd_copy_image_to_buffer(
                cb,
                target.image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                self.snapshot_buffer.buffer,
                &[vk::BufferImageCopy::default()
                    .image_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .image_extent(vk::Extent3D {
                        width: target.extent.width,
                        height: target.extent.height,
                        depth: 1,
                    })],
            );
            barrier(
                &self.ctx.device,
                cb,
                target.image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::ImageLayout::PRESENT_SRC_KHR,
                vk::PipelineStageFlags2::COPY,
                vk::AccessFlags2::TRANSFER_READ,
                vk::PipelineStageFlags2::NONE,
                vk::AccessFlags2::NONE,
            );
        } else {
            barrier(
                &self.ctx.device,
                cb,
                target.image,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                vk::ImageLayout::PRESENT_SRC_KHR,
                vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
                vk::PipelineStageFlags2::NONE,
                vk::AccessFlags2::NONE,
            );
        }
        let slot = pipeline.slot();
        let fence = pipeline.submit(
            &self.ctx,
            &[acquired],
            &[vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT],
            &[self.presenter.render_done(index)],
        )?;
        // Zero-copy: the shared slot this frame read stays held until its fence (`frame_done` in `render_frame`).
        self.shared.submitted(slot, pipeline.submitted_external());
        let presented = self.presenter.present(&self.ctx, index);
        if snapshot {
            // After the present, so the picture reaches the screen on time; the wait is one frame of GPU time.
            self.finish_snapshot(fence, target.extent, target.format)?;
        }
        presented
    }

    /// Asks for a snapshot of the output: the next frame drawn is copied out without the menu on it ([`Renderer::take_snapshot`]).
    /// False when the swapchain cannot be copied from (the surface did not offer `TRANSFER_SRC`).
    pub fn request_snapshot(&mut self) -> bool {
        if self.presenter.can_copy() {
            self.snapshot_wanted = true;
        }
        self.presenter.can_copy()
    }

    /// The snapshot of the last request, once its frame was drawn (taken at most once).
    pub fn take_snapshot(&mut self) -> Option<SnapshotImage> {
        self.snapshot_done.take()
    }

    /// Makes the host buffer of the snapshot fit the swapchain extent.
    ///
    /// # Safety
    /// The GPU no longer uses the previous buffer (its fence was waited in [`Renderer::finish_snapshot`]).
    unsafe fn ensure_snapshot_buffer(&mut self) -> Res<()> {
        let e = self.presenter.extent();
        let size = (e.width, e.height);
        if self.snapshot_buffer.buffer == vk::Buffer::null() || self.snapshot_size != size {
            self.snapshot_buffer.destroy(&self.ctx.device);
            self.snapshot_buffer = headless::HostBuffer::create(
                &self.ctx,
                size.0 as usize * size.1 as usize * 4,
                vk::BufferUsageFlags::TRANSFER_DST,
            )?;
            self.snapshot_size = size;
        }
        Ok(())
    }

    /// Waits for the frame that carried the copy and converts the buffer to RGBA.
    ///
    /// # Safety
    /// `fence` is the fence of the submission that copied into `snapshot_buffer`, made for `extent` and `format`.
    unsafe fn finish_snapshot(&mut self, fence: vk::Fence, extent: vk::Extent2D, format: vk::Format) -> Res<()> {
        wait_fences(&self.ctx.device, &[fence], "snapshot")?;
        let len = extent.width as usize * extent.height as usize * 4;
        // SAFETY: the buffer holds `len` mapped bytes (`ensure_snapshot_buffer`) and the GPU is done with them.
        let raw = std::slice::from_raw_parts(self.snapshot_buffer.mapped, len);
        self.snapshot_done = Some(SnapshotImage {
            width: extent.width,
            height: extent.height,
            rgba: swapchain_to_rgba(format, raw),
        });
        Ok(())
    }

    /// The handle kind of the shared textures the device imports, when the device supports it (A29); `None`
    /// otherwise.
    pub fn zero_copy_support(&self) -> Option<HandleKind> {
        self.ctx.external_memory()
    }

    /// What the zero-copy decision needs from the renderer (`zerocopy::decide`): the extension offered, the handle kind
    /// imported, and whether `capture` (the capture device's adapter) is the renderer's.
    pub fn zero_copy_check(&self, capture: Option<crate::adapters::Luid>) -> crate::zerocopy::Support {
        let render = self.ctx.gpu.luid.map(crate::adapters::Luid::from_vulkan);
        crate::zerocopy::Support {
            extension: self.zero_copy_offered,
            handle: self.ctx.external_memory(),
            same_adapter: match (capture, render) {
                (Some(c), Some(r)) => Some(c == r),
                _ => None,
            },
        }
    }

    /// Why the last shared frames could not be used (once), `None` when they could.
    pub fn take_external_failure(&mut self) -> Option<String> {
        self.external_failure.take()
    }

    /// The source is a shared texture (the zero-copy path runs end to end).
    pub fn source_is_shared(&self) -> bool {
        self.pipeline.as_ref().is_some_and(Pipeline::source_is_external)
    }

    /// Error path after a successful acquire: the image is acquired and will not be presented, and its
    /// semaphore may still be signalled. Waits for the device, rolls back the unsubmitted frame,
    /// replaces the semaphore and asks for a new swapchain before the next frame (which discards the
    /// acquired image), so the next frame starts clean. Best effort: a failure here is dropped, the caller
    /// reports the first error.
    ///
    /// # Safety
    /// As [`Renderer::render`].
    unsafe fn recover_after_acquire(&mut self, slot: usize) {
        let d = &self.ctx.device;
        let _ = d.device_wait_idle();
        if let Some(p) = self.pipeline.as_mut() {
            p.abandon();
        }
        if let Ok(fresh) = d.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) {
            d.destroy_semaphore(self.image_available[slot], None);
            self.image_available[slot] = fresh;
        }
        self.presenter.mark_stale();
    }

    /// Pixels clipped by the output transform in the last frame the GPU finished (COL-1 clip indicator
    /// for the UI): over panel white, and outside the panel gamut. Not the frame just submitted, but
    /// the one `FRAMES_IN_FLIGHT` frames earlier, so that reading never waits for the GPU.
    pub fn clip_counts(&self) -> ClipCounts {
        self.clip
    }

    /// SDR headroom readout of the last frame recorded (T-84, D-16; for the UI): mask peak, scanline peak,
    /// white matrix maximum and exposure, whose product ([`crt_model::output::Headroom::total`]) above 1 means
    /// a full-white field has elements brighter than panel white. `None` before the first frame.
    pub fn headroom(&self) -> Option<crt_model::output::Headroom> {
        self.pipeline.as_ref().and_then(Pipeline::headroom)
    }

    /// Names of the chain slots, in order (`signal`, `amp`, `gun`, `screen`, `phosphor`, `optics`; T-54 bypass).
    pub fn stage_names(&self) -> Vec<&'static str> {
        self.pipeline.as_ref().map(Pipeline::stage_names).unwrap_or_default()
    }

    /// The enable flag of the chain slot `name`, `None` for an unknown name.
    pub fn stage_enabled(&self, name: &str) -> Option<bool> {
        self.pipeline.as_ref().and_then(|p| p.is_enabled(name))
    }

    /// Turns the chain slot `name` on or off (off: bypassed, exact identity; A11 enable flags).
    pub fn set_stage_enabled(&mut self, name: &str, on: bool) -> Res<()> {
        self.pipeline
            .as_mut()
            .expect("pipeline created in new")
            .set_enabled(name, on)
    }

    /// GPU timings of the latest frame whose results were read (at most [`FRAMES_IN_FLIGHT`] frames
    /// behind the last one rendered; reading them never waits). `None` before the first, or when the
    /// device has no timestamp support ([`Renderer::timing_unavailable`]).
    pub fn timings(&self) -> Option<&FrameTimings> {
        self.pipeline.as_ref().and_then(Pipeline::timings)
    }

    /// Mean GPU timings of the frames read since the previous call (`--stats`, the diagnostics log, the
    /// overlay call it once per second), then restarts the average.
    pub fn take_timing_average(&mut self) -> Option<FrameTimings> {
        self.pipeline.as_mut().and_then(Pipeline::take_timing_average)
    }

    /// Why GPU timestamps are off, `None` when they work.
    pub fn timing_unavailable(&self) -> Option<&str> {
        self.pipeline.as_ref().and_then(Pipeline::timing_unavailable)
    }

    /// Names of the timed segments in order: `upload`, `decode`, the chain slots, `encode`, `ui`.
    pub fn timing_names(&self) -> Vec<&'static str> {
        self.pipeline.as_ref().map(Pipeline::timing_names).unwrap_or_default()
    }

    /// Clears the history of the stages that keep one (persistence, T-41): called on a source size
    /// change or a new capture session (`lifecycle::SizeWatch::take_history_clear`, SRC-8).
    pub fn clear_history(&mut self) {
        if let Some(p) = self.pipeline.as_mut() {
            p.clear_history();
        }
    }
}

/// Size of the burn-in wear map of the `wear` slot (T-43), texels.
pub const WEAR_MAP_SIZE: (u32, u32) = stages::wear::MAP_SIZE;

/// The wear map operations shared by the window and the headless renderer (`tube_state::WearHost`).
mod wear_host {
    use super::{Context, Pipeline, WEAR_MAP_SIZE};
    use crate::tube_state::WearMap;
    use crate::Res;

    fn wear(p: &mut Pipeline) -> Res<&mut super::stages::wear::Wear> {
        p.wear().ok_or_else(|| "the chain has no wear slot".into())
    }

    fn map(q: Vec<f32>) -> WearMap {
        WearMap {
            width: WEAR_MAP_SIZE.0,
            height: WEAR_MAP_SIZE.1,
            q,
        }
    }

    pub(super) fn set(p: &mut Pipeline, m: &WearMap) -> Res<()> {
        let m = m.resampled(WEAR_MAP_SIZE.0, WEAR_MAP_SIZE.1);
        wear(p)?.set_map(m.q)
    }

    pub(super) fn reset(p: &mut Pipeline) {
        if let Ok(w) = wear(p) {
            w.reset();
        }
    }

    pub(super) fn burn_in_now(p: &mut Pipeline, hours: f32) {
        if let Ok(w) = wear(p) {
            w.burn_in_now(f64::from(hours));
        }
    }

    pub(super) fn request(p: &mut Pipeline) {
        if let Ok(w) = wear(p) {
            w.request_snapshot();
        }
    }

    /// # Safety
    /// `ctx` is the pipeline's context.
    pub(super) unsafe fn take(p: &mut Pipeline, ctx: &Context) -> Res<Option<WearMap>> {
        Ok(wear(p)?.take_snapshot(ctx)?.map(map))
    }

    /// # Safety
    /// `ctx` is the pipeline's context; no frame is being recorded.
    pub(super) unsafe fn read(p: &mut Pipeline, ctx: &Context) -> Res<Option<WearMap>> {
        Ok(wear(p)?.read_now(ctx)?.map(map))
    }
}

impl crate::tube_state::WearHost for Renderer {
    fn wear_map_size(&self) -> (u32, u32) {
        WEAR_MAP_SIZE
    }
    fn set_wear_map(&mut self, map: &crate::tube_state::WearMap) -> Res<()> {
        wear_host::set(self.pipeline.as_mut().expect("pipeline created in new"), map)
    }
    fn reset_wear(&mut self) {
        wear_host::reset(self.pipeline.as_mut().expect("pipeline created in new"));
    }
    fn burn_in_now(&mut self, hours: f32) {
        wear_host::burn_in_now(self.pipeline.as_mut().expect("pipeline created in new"), hours);
    }
    fn request_wear_snapshot(&mut self) {
        wear_host::request(self.pipeline.as_mut().expect("pipeline created in new"));
    }
    fn take_wear_snapshot(&mut self) -> Res<Option<crate::tube_state::WearMap>> {
        // SAFETY: the pipeline was created on `self.ctx`.
        unsafe { wear_host::take(self.pipeline.as_mut().expect("pipeline created in new"), &self.ctx) }
    }
    fn read_wear_map(&mut self) -> Res<Option<crate::tube_state::WearMap>> {
        // SAFETY: as above; `render` records and submits within one call, so no frame is being recorded.
        unsafe { wear_host::read(self.pipeline.as_mut().expect("pipeline created in new"), &self.ctx) }
    }
}

impl crate::tube_state::WearHost for Headless {
    fn wear_map_size(&self) -> (u32, u32) {
        WEAR_MAP_SIZE
    }
    fn set_wear_map(&mut self, map: &crate::tube_state::WearMap) -> Res<()> {
        wear_host::set(self.pipeline_mut(), map)
    }
    fn reset_wear(&mut self) {
        wear_host::reset(self.pipeline_mut());
    }
    fn burn_in_now(&mut self, hours: f32) {
        wear_host::burn_in_now(self.pipeline_mut(), hours);
    }
    fn request_wear_snapshot(&mut self) {
        wear_host::request(self.pipeline_mut());
    }
    fn take_wear_snapshot(&mut self) -> Res<Option<crate::tube_state::WearMap>> {
        let (p, ctx) = self.parts();
        // SAFETY: the pipeline was created on `ctx`.
        unsafe { wear_host::take(p, ctx) }
    }
    fn read_wear_map(&mut self) -> Res<Option<crate::tube_state::WearMap>> {
        let (p, ctx) = self.parts();
        // SAFETY: as above; every headless call records and submits within the call.
        unsafe { wear_host::read(p, ctx) }
    }
}

/// Stages a shared frame (zero-copy, A29) as the pipeline's source and makes it the current hold. A new pool
/// generation is imported after a device idle (inside `stage_external`), so every earlier hold can go. A failure other
/// than a lost device is kept in `failure` (the first one) and the frame is dropped: the pipeline keeps drawing
/// `src` if it holds a frame.
///
/// # Safety
/// As [`Pipeline::stage_external`].
unsafe fn stage_shared(
    ctx: &Context,
    pipeline: &mut Pipeline,
    holds: &mut InFlight<SharedFrame, FRAMES_IN_FLIGHT>,
    frame: SharedFrame,
    failure: &mut Option<String>,
) -> Res<()> {
    match pipeline.stage_external(ctx, frame.pool(), frame.slot()) {
        Ok(imported) => {
            if imported {
                // Device idle when the generation changed: nothing reads the old pool any more.
                holds.drain();
            }
            holds.set_current(frame);
            Ok(())
        }
        Err(e) => {
            let e = lift(e, "importing a shared capture texture");
            if e.downcast_ref::<GpuError>().is_some_and(GpuError::is_device_lost) {
                return Err(e);
            }
            pipeline.drop_external_source();
            holds.clear_current();
            if failure.is_none() {
                *failure = Some(e.to_string());
            }
            Ok(())
        }
    }
}

fn color_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .level_count(1)
        .layer_count(1)
}

/// Records an image layout barrier (synchronization2).
///
/// # Safety
/// `cb` must be in the recording state on `device` and `image` must be a live image of `device`.
// A barrier needs all of these values and a struct would only add noise.
#[allow(clippy::too_many_arguments)]
unsafe fn barrier(
    d: &ash::Device,
    cb: vk::CommandBuffer,
    image: vk::Image,
    old: vk::ImageLayout,
    new: vk::ImageLayout,
    src_stage: vk::PipelineStageFlags2,
    src_access: vk::AccessFlags2,
    dst_stage: vk::PipelineStageFlags2,
    dst_access: vk::AccessFlags2,
) {
    let b = [vk::ImageMemoryBarrier2::default()
        .src_stage_mask(src_stage)
        .src_access_mask(src_access)
        .dst_stage_mask(dst_stage)
        .dst_access_mask(dst_access)
        .old_layout(old)
        .new_layout(new)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(color_range())];
    d.cmd_pipeline_barrier2(cb, &vk::DependencyInfo::default().image_memory_barriers(&b));
}

fn hwnd_client_size(hwnd: isize) -> (u32, u32) {
    crate::output::client_size(windows::Win32::Foundation::HWND(hwnd as *mut std::ffi::c_void))
}

impl Drop for Renderer {
    fn drop(&mut self) {
        // SAFETY: after `device_wait_idle` the GPU uses none of these objects. They are destroyed
        // in reverse order of creation, null ones (a failed `new`) are no-ops, and the device and
        // instance go last with the field drop of `ctx`.
        unsafe {
            let _ = self.ctx.device.device_wait_idle();
            if let Some(mut p) = self.pipeline.take() {
                p.destroy(&self.ctx);
            }
            for s in self.image_available {
                self.ctx.device.destroy_semaphore(s, None);
            }
            self.snapshot_buffer.destroy(&self.ctx.device);
            self.presenter.destroy(&self.ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The snapshot pixels leave the swapchain as RGBA with alpha 255, whichever channel order the swapchain has.
    #[test]
    fn swapchain_pixels_become_rgba() {
        let raw = [10, 20, 30, 0, 40, 50, 60, 7];
        assert_eq!(
            swapchain_to_rgba(vk::Format::B8G8R8A8_UNORM, &raw),
            vec![30, 20, 10, 255, 60, 50, 40, 255]
        );
        assert_eq!(
            swapchain_to_rgba(vk::Format::R8G8B8A8_UNORM, &raw),
            vec![10, 20, 30, 255, 40, 50, 60, 255]
        );
    }

    /// R-01: every Vulkan result the window path meets maps to one recovery.
    #[test]
    fn vulkan_results_are_classified() {
        assert_eq!(classify(vk::Result::SUCCESS), VkClass::Ok);
        assert_eq!(classify(vk::Result::SUBOPTIMAL_KHR), VkClass::Suboptimal);
        assert_eq!(classify(vk::Result::ERROR_OUT_OF_DATE_KHR), VkClass::OutOfDate);
        assert_eq!(classify(vk::Result::ERROR_SURFACE_LOST_KHR), VkClass::SurfaceLost);
        assert_eq!(classify(vk::Result::ERROR_DEVICE_LOST), VkClass::DeviceLost);
        assert_eq!(classify(vk::Result::TIMEOUT), VkClass::Timeout);
        assert_eq!(classify(vk::Result::NOT_READY), VkClass::Timeout);
        assert_eq!(classify(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY), VkClass::OutOfMemory);
        assert_eq!(classify(vk::Result::ERROR_OUT_OF_HOST_MEMORY), VkClass::OutOfMemory);
        assert_eq!(classify(vk::Result::ERROR_INITIALIZATION_FAILED), VkClass::Other);
    }

    /// R-01: a lost device becomes a [`GpuError`] whatever path it took (a `?` on an ash call boxes the bare
    /// `vk::Result`), with the message the application shows; other errors pass unchanged.
    #[test]
    fn device_loss_is_a_distinct_descriptive_error() {
        let e = lift(Box::new(vk::Result::ERROR_DEVICE_LOST), "rendering a frame");
        let g = e.downcast_ref::<GpuError>().expect("a GpuError");
        assert!(g.is_device_lost());
        let text = e.to_string();
        assert!(text.contains("GPU device lost (driver reset or update)"), "{text}");
        assert!(text.contains("rendering a frame"), "{text}");

        let hung = GpuError::DeviceLost {
            during: "a frame",
            hung: true,
        };
        assert!(hung.is_device_lost());
        assert!(hung.to_string().contains("did not finish a frame within 5 s"));

        let e = lift(Box::new(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY), "allocating an image");
        let g = e.downcast_ref::<GpuError>().expect("a GpuError");
        assert_eq!(
            g,
            &GpuError::OutOfMemory {
                during: "allocating an image"
            }
        );
        assert!(!g.is_device_lost());
        assert!(lift(Box::new(vk::Result::ERROR_SURFACE_LOST_KHR), "presenting")
            .downcast_ref::<GpuError>()
            .is_some_and(|g| matches!(g, GpuError::SurfaceLost { .. })));

        // Not a Vulkan result, or another one: unchanged.
        let e = lift("pipeline: something".into(), "x");
        assert_eq!(e.to_string(), "pipeline: something");
        let e = lift(Box::new(vk::Result::ERROR_INITIALIZATION_FAILED), "x");
        assert_eq!(
            e.downcast_ref::<vk::Result>(),
            Some(&vk::Result::ERROR_INITIALIZATION_FAILED)
        );

        let u = GpuError::Unsupported {
            reason: "x needs 256 invocations".into(),
        };
        assert_eq!(u.to_string(), "this GPU is not supported: x needs 256 invocations");
    }
}

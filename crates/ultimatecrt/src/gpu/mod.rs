//! Vulkan renderer: uploads the captured frame and runs it through the pipeline of GLSL stages of
//! `shaders/` into the swapchain. Vulkan 1.3 with dynamic rendering and synchronization2.
//!
//! Modules: `context` (instance, device choice, queue; with or without a window), `present`
//! (one `Presenter` per window), `pipeline` (the stage chain, its images and frames in flight),
//! `stages` (one module per stage), `ui_render` (the settings menu and overlay pass after `encode`,
//! T-52a). `Renderer` is the facade used by the application.
//! `headless` runs the same pipeline into an offscreen image and reads it back, without any window
//! (tests).

use ash::vk;

use crate::params::Params;
use crate::scaling::View;

use crate::Res;

mod context;
mod headless;
mod pipeline;
mod present;
mod stages;
mod timing;
mod ui_render;

pub use timing::{csv_columns, FrameTimings, StageTiming, TimingAverager};
pub use ui_render::UiPass;

pub use context::{gpu_required, validation_layer_available, ValidationCounter};
pub use context::{list_gpus, Context, GpuInfo};
pub use headless::{Headless, ImageF32, Rendered, SourceFrame, TargetFormat};
pub use headless::{StageInput, StageRun};
pub use pipeline::{tube_rect_letterbox, FrameConstants, Settings, Transfer, FRAMES_IN_FLIGHT, NATIVE_FORMAT};
pub use present::Presenter;
pub use stages::encode::{output_matrix, ClipCounts};
pub use stages::screen::{warp_plan, WarpPlan, WARP_SIGMA_STEPS};
pub use stages::{Domain, AMP_K};

use pipeline::{Pipeline, Target};

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
    /// Clip counters of the last frame that finished (T-46), refreshed by `render`.
    clip: ClipCounts,
    /// Destroyed last: `Drop for Renderer` releases every child object above first, then the
    /// field drop of `ctx` destroys the device and the instance.
    ctx: Context,
}

impl Renderer {
    /// Creates the Vulkan context on a device that can present to `hwnd` (the `--gpu` index if
    /// given), its presenter and the pipeline.
    pub fn new(hwnd: isize, hinstance: isize, gpu: Option<usize>, validation: bool, integer_scale: bool) -> Res<Self> {
        // SAFETY: `hwnd` is the live output window of `hinstance`, owned by the application, which
        // drops the renderer before destroying the window. The presenter and pipeline are destroyed
        // by `Drop for Renderer` before `ctx`.
        unsafe {
            let (ctx, surface) = Context::with_window(hwnd, hinstance, gpu, validation)?;
            let presenter = Presenter::from_surface(&ctx, surface, hwnd_client_size(hwnd))?;
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
                clip: ClipCounts::default(),
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

    /// Renders one frame. `frame` is a newly captured BGRA8 image, if any. `client` is the current
    /// client size of the output window.
    ///
    /// A given frame is always staged for upload, also when the window is minimised or the swapchain
    /// is out of date (the frame is then not drawn but kept for the next call), so the caller may
    /// consider it consumed once this returns `Ok`.
    pub fn render(&mut self, frame: Option<(u32, u32, &[u8])>, view: View, client: (u32, u32)) -> Res<()> {
        self.render_with_ui(frame, view, client, None)
    }

    /// The Vulkan context (device, queue, chosen GPU): the settings menu (`crate::ui::Ui`) creates its
    /// renderer on it, and must be dropped before the renderer.
    pub fn context(&self) -> &Context {
        &self.ctx
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
    ) -> Res<()> {
        let pipeline = self.pipeline.as_mut().expect("pipeline created in new");
        // SAFETY: every handle used below belongs to `self.ctx`. `wait_slot` guarantees the slot's
        // previous submission finished, so its command buffer, staging and uniform buffers and its
        // image-available semaphore are free.
        unsafe {
            pipeline.wait_slot(&self.ctx)?;
            self.clip = pipeline.clip_counts();
            if let Some((w, h, data)) = frame {
                pipeline.stage_source(&self.ctx, w, h, data)?;
            }
            if client.0 == 0 || client.1 == 0 {
                return Ok(()); // minimised
            }

            let extent = self.presenter.extent();
            if (extent.width, extent.height) != client {
                self.ctx.device.device_wait_idle()?;
                self.presenter.resize(&self.ctx, client)?;
            }
            // Everything that can fail for a reason other than the device or the surface (settings,
            // pipeline creation, image allocation) is done before the acquire.
            pipeline.prepare(
                &self.ctx,
                view,
                &self.settings,
                &self.params,
                self.presenter.extent(),
                self.presenter.format(),
            )?;

            let slot = pipeline.slot();
            let acquired = self.image_available[slot];
            let Some(index) = self.presenter.acquire(&self.ctx, client, acquired)? else {
                return Ok(());
            };
            if let Err(e) = self.draw(index, acquired, view, client, ui) {
                self.recover_after_acquire(slot, client);
                return Err(e);
            }
        }
        Ok(())
    }

    /// Records, submits and presents the frame into the acquired image `index`.
    ///
    /// # Safety
    /// As [`Renderer::render`]; `index` was acquired with `acquired` as its semaphore.
    unsafe fn draw(
        &mut self,
        index: u32,
        acquired: vk::Semaphore,
        view: View,
        client: (u32, u32),
        ui: Option<&mut dyn UiPass>,
    ) -> Res<()> {
        let pipeline = self.pipeline.as_mut().expect("pipeline created in new");
        let target = Target {
            image: self.presenter.image(index),
            view: self.presenter.view(index),
            extent: self.presenter.extent(),
            format: self.presenter.format(),
        };
        let cb = pipeline.record_ui(&self.ctx, view, &self.settings, &self.params, &target, ui)?;
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
        pipeline.submit(
            &self.ctx,
            &[acquired],
            &[vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT],
            &[self.presenter.render_done(index)],
        )?;
        self.presenter.present(&self.ctx, index, client)
    }

    /// Error path after a successful acquire: the image is acquired and will not be presented, and its
    /// semaphore may still be signalled. Waits for the device, rolls back the unsubmitted frame,
    /// replaces the semaphore and recreates the swapchain (which discards the acquired image), so the
    /// next frame starts clean. Best effort: a failure here is dropped, the caller reports the first error.
    ///
    /// # Safety
    /// As [`Renderer::render`].
    unsafe fn recover_after_acquire(&mut self, slot: usize, client: (u32, u32)) {
        let d = &self.ctx.device;
        let _ = d.device_wait_idle();
        if let Some(p) = self.pipeline.as_mut() {
            p.abandon();
        }
        if let Ok(fresh) = d.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) {
            d.destroy_semaphore(self.image_available[slot], None);
            self.image_available[slot] = fresh;
        }
        let _ = self.presenter.resize(&self.ctx, client);
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
            self.presenter.destroy(&self.ctx);
        }
    }
}

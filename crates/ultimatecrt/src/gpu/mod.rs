//! Vulkan renderer: uploads the captured frame and runs it through the pipeline of GLSL stages of
//! `shaders/` into the swapchain. Vulkan 1.3 with dynamic rendering and synchronization2.
//!
//! Modules: `context` (instance, device choice, queue; with or without a window), `present`
//! (one `Presenter` per window), `pipeline` (the stage chain, its images and frames in flight),
//! `stages` (one module per stage). `Renderer` is the facade used by the application.
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

pub use context::{gpu_required, validation_layer_available, ValidationCounter};
pub use context::{list_gpus, Context, GpuInfo};
pub use headless::{Headless, ImageF32, Rendered, SourceFrame, TargetFormat};
pub use pipeline::{tube_rect_letterbox, FrameConstants, Settings, Transfer, FRAMES_IN_FLIGHT, NATIVE_FORMAT};
pub use present::Presenter;

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

    /// Renders one frame. `frame` is a newly captured BGRA8 image, if any. `client` is the current
    /// client size of the output window.
    pub fn render(&mut self, frame: Option<(u32, u32, &[u8])>, view: View, client: (u32, u32)) -> Res<()> {
        if client.0 == 0 || client.1 == 0 {
            return Ok(()); // minimised
        }
        let pipeline = self.pipeline.as_mut().expect("pipeline created in new");
        // SAFETY: every handle used below belongs to `self.ctx`. `wait_slot` guarantees the slot's
        // previous submission finished, so its command buffer, staging and uniform buffers and its
        // image-available semaphore are free.
        unsafe {
            pipeline.wait_slot(&self.ctx)?;
            if let Some((w, h, data)) = frame {
                pipeline.stage_source(&self.ctx, w, h, data)?;
            }

            let extent = self.presenter.extent();
            if (extent.width, extent.height) != client {
                self.ctx.device.device_wait_idle()?;
                self.presenter.resize(&self.ctx, client)?;
            }

            let acquired = self.image_available[pipeline.slot()];
            let Some(index) = self.presenter.acquire(&self.ctx, client, acquired)? else {
                return Ok(());
            };
            let target = Target {
                image: self.presenter.image(index),
                view: self.presenter.view(index),
                extent: self.presenter.extent(),
                format: self.presenter.format(),
            };
            let cb = pipeline.record(&self.ctx, view, &self.settings, &self.params, &target)?;
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
            self.presenter.present(&self.ctx, index, client)?;
        }
        Ok(())
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

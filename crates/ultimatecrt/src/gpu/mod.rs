//! Vulkan renderer: uploads the captured frame to a texture and draws it into the swapchain with
//! the GLSL passes of `shaders/`. Vulkan 1.3 with dynamic rendering and synchronization2.
//!
//! The swapchain is B8G8R8A8_UNORM, so the passthrough shader writes captured values unchanged.
//!
//! Modules: `context` (instance, device choice, queue; with or without a window), `present`
//! (one `Presenter` per window), `upload` (frame texture and staging), `passthrough` (current
//! pipeline and command recording). `Renderer` is the facade used by the application.

use ash::vk;

use crate::scaling::View;

use crate::Res;

mod context;
mod passthrough;
mod present;
mod upload;

pub use context::{list_gpus, Context, GpuInfo};
pub use present::Presenter;

/// Renders the captured frame into one output window.
///
/// Every Vulkan handle starts null and is filled in by `new`, so `Drop` releases whatever was
/// created even when `new` fails half way (destroying a null handle is a no-op in Vulkan).
pub struct Renderer {
    pub gpu_name: String,
    presenter: Presenter,
    in_flight: vk::Fence,
    pool: vk::CommandPool,
    cmd: vk::CommandBuffer,
    set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    desc_pool: vk::DescriptorPool,
    desc_set: vk::DescriptorSet,
    sampler: vk::Sampler,
    tex: Option<Texture>,
    upload_pending: bool,
    integer_scale: bool,
    view: View,
    /// Destroyed last: `Drop for Renderer` releases every child object above first, then the
    /// field drop of `ctx` destroys the device and the instance.
    ctx: Context,
}

struct Texture {
    width: u32,
    height: u32,
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    staging: vk::Buffer,
    staging_memory: vk::DeviceMemory,
    mapped: *mut u8,
    layout: vk::ImageLayout,
}

impl Renderer {
    /// Creates the Vulkan context on a device that can present to `hwnd` (the `--gpu` index if
    /// given), its presenter and the passthrough pipeline.
    pub fn new(hwnd: isize, hinstance: isize, gpu: Option<usize>, validation: bool, integer_scale: bool) -> Res<Self> {
        // SAFETY: `hwnd` is the live output window of `hinstance`, owned by the application, which
        // drops the renderer before destroying the window. The presenter is destroyed by
        // `Drop for Renderer` before `ctx`.
        unsafe {
            let (ctx, surface) = Context::with_window(hwnd, hinstance, gpu, validation)?;
            let presenter = Presenter::from_surface(&ctx, surface, hwnd_client_size(hwnd))?;
            let mut r = Self {
                gpu_name: ctx.gpu.name.clone(),
                presenter,
                in_flight: vk::Fence::null(),
                pool: vk::CommandPool::null(),
                cmd: vk::CommandBuffer::null(),
                set_layout: vk::DescriptorSetLayout::null(),
                pipeline_layout: vk::PipelineLayout::null(),
                pipeline: vk::Pipeline::null(),
                desc_pool: vk::DescriptorPool::null(),
                desc_set: vk::DescriptorSet::null(),
                sampler: vk::Sampler::null(),
                tex: None,
                upload_pending: false,
                integer_scale,
                view: View::full(1, 1),
                ctx,
            };
            r.create_frame_objects()?;
            r.create_pipeline()?;
            Ok(r)
        }
    }

    /// Creates the command pool and buffer, the frame fence, the descriptor set of the captured
    /// frame, its sampler and the pipeline layout.
    ///
    /// # Safety
    /// Called once, from `new`, while those handles are still null.
    unsafe fn create_frame_objects(&mut self) -> Res<()> {
        let device = &self.ctx.device;
        self.pool = device.create_command_pool(
            &vk::CommandPoolCreateInfo::default()
                .queue_family_index(self.ctx.queue_family)
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
            None,
        )?;
        self.cmd = device.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::default()
                .command_pool(self.pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1),
        )?[0];
        self.in_flight = device.create_fence(
            &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
            None,
        )?;

        // Descriptor set: one combined image sampler (the captured frame).
        let bindings = [vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT)];
        self.set_layout = device
            .create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings), None)?;
        let sizes = [vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)];
        self.desc_pool = device.create_descriptor_pool(
            &vk::DescriptorPoolCreateInfo::default().max_sets(1).pool_sizes(&sizes),
            None,
        )?;
        let set_layouts = [self.set_layout];
        self.desc_set = device.allocate_descriptor_sets(
            &vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(self.desc_pool)
                .set_layouts(&set_layouts),
        )?[0];
        self.sampler = device.create_sampler(
            &vk::SamplerCreateInfo::default()
                .mag_filter(vk::Filter::NEAREST)
                .min_filter(vk::Filter::NEAREST)
                .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE),
            None,
        )?;

        let push = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
            .offset(0)
            .size(size_of::<passthrough::PushConstants>() as u32)];
        self.pipeline_layout = device.create_pipeline_layout(
            &vk::PipelineLayoutCreateInfo::default()
                .set_layouts(&set_layouts)
                .push_constant_ranges(&push),
            None,
        )?;
        Ok(())
    }

    /// Renders one frame. `frame` is a newly captured BGRA8 image, if any. `client` is the current
    /// client size of the output window.
    pub fn render(&mut self, frame: Option<(u32, u32, &[u8])>, view: View, client: (u32, u32)) -> Res<()> {
        self.view = view;
        if client.0 == 0 || client.1 == 0 {
            return Ok(()); // minimised
        }
        // SAFETY: every handle used below belongs to `self.ctx`; the fence wait guarantees the
        // previous frame no longer uses the command buffer, the staging buffer or the semaphores.
        unsafe {
            self.ctx.device.wait_for_fences(&[self.in_flight], true, u64::MAX)?;

            if let Some((w, h, data)) = frame {
                if self.tex.as_ref().map(|t| (t.width, t.height)) != Some((w, h)) {
                    self.create_texture(w, h)?;
                }
                let t = self.tex.as_ref().unwrap();
                std::ptr::copy_nonoverlapping(data.as_ptr(), t.mapped, (w as usize * h as usize * 4).min(data.len()));
                self.upload_pending = true;
            }

            let extent = self.presenter.extent();
            if (extent.width, extent.height) != client {
                self.ctx.device.device_wait_idle()?;
                self.presenter.resize(&self.ctx, client)?;
            }

            let Some(index) = self.presenter.acquire(&self.ctx, client)? else {
                return Ok(());
            };
            let device = &self.ctx.device;
            device.reset_fences(&[self.in_flight])?;
            device.reset_command_buffer(self.cmd, vk::CommandBufferResetFlags::empty())?;
            device.begin_command_buffer(
                self.cmd,
                &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            self.record(index);
            let device = &self.ctx.device;
            device.end_command_buffer(self.cmd)?;

            let wait = [self.presenter.image_available()];
            let stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
            let cmds = [self.cmd];
            let signal = [self.presenter.render_done(index)];
            device.queue_submit(
                self.ctx.queue,
                &[vk::SubmitInfo::default()
                    .wait_semaphores(&wait)
                    .wait_dst_stage_mask(&stages)
                    .command_buffers(&cmds)
                    .signal_semaphores(&signal)],
                self.in_flight,
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
            let device = &self.ctx.device;
            let _ = device.device_wait_idle();
            self.destroy_texture();
            let device = &self.ctx.device;
            device.destroy_pipeline(self.pipeline, None);
            device.destroy_pipeline_layout(self.pipeline_layout, None);
            device.destroy_sampler(self.sampler, None);
            device.destroy_descriptor_pool(self.desc_pool, None);
            device.destroy_descriptor_set_layout(self.set_layout, None);
            device.destroy_fence(self.in_flight, None);
            device.destroy_command_pool(self.pool, None);
            self.presenter.destroy(&self.ctx);
        }
    }
}

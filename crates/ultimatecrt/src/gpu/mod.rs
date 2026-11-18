//! Vulkan renderer: uploads the captured frame to a texture and draws it into the swapchain with
//! the GLSL passes of `shaders/`. Vulkan 1.3 with dynamic rendering and synchronization2.
//!
//! The swapchain is B8G8R8A8_UNORM, so the passthrough shader writes captured values unchanged.

use std::error::Error;

use ash::{khr, vk, Device, Entry, Instance};

use crate::scaling::View;

type Res<T> = Result<T, Box<dyn Error>>;

mod context;
mod passthrough;
mod present;
mod upload;

use context::gpu_info;
pub use context::list_gpus;

pub struct Renderer {
    _entry: Entry,
    instance: Instance,
    surface_fn: khr::surface::Instance,
    surface: vk::SurfaceKHR,
    pub gpu_name: String,
    physical: vk::PhysicalDevice,
    device: Device,
    queue: vk::Queue,
    swapchain_fn: khr::swapchain::Device,
    swapchain: vk::SwapchainKHR,
    format: vk::Format,
    extent: vk::Extent2D,
    images: Vec<vk::Image>,
    views: Vec<vk::ImageView>,
    render_done: Vec<vk::Semaphore>,
    image_available: vk::Semaphore,
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
    pub fn new(hwnd: isize, hinstance: isize, gpu: Option<usize>, validation: bool, integer_scale: bool) -> Res<Self> {
        unsafe {
            let entry = Entry::load()?;
            let app = vk::ApplicationInfo::default()
                .application_name(c"ultimatecrt")
                .api_version(vk::API_VERSION_1_3);
            let exts = [khr::surface::NAME.as_ptr(), khr::win32_surface::NAME.as_ptr()];
            let layers = [c"VK_LAYER_KHRONOS_validation".as_ptr()];
            let mut info = vk::InstanceCreateInfo::default()
                .application_info(&app)
                .enabled_extension_names(&exts);
            if validation {
                info = info.enabled_layer_names(&layers);
            }
            let instance = entry.create_instance(&info, None)?;

            let win32 = khr::win32_surface::Instance::new(&entry, &instance);
            let surface = win32.create_win32_surface(
                &vk::Win32SurfaceCreateInfoKHR::default().hinstance(hinstance).hwnd(hwnd),
                None,
            )?;
            let surface_fn = khr::surface::Instance::new(&entry, &instance);

            // Pick a device that can present to the surface: the requested index, else NVIDIA
            // discrete, else any discrete, else the first.
            let devices = instance.enumerate_physical_devices()?;
            let mut candidates = Vec::new();
            for (i, &pd) in devices.iter().enumerate() {
                let g = gpu_info(&instance, pd, i);
                let family = instance
                    .get_physical_device_queue_family_properties(pd)
                    .iter()
                    .enumerate()
                    .position(|(qi, q)| {
                        q.queue_flags.contains(vk::QueueFlags::GRAPHICS)
                            && surface_fn
                                .get_physical_device_surface_support(pd, qi as u32, surface)
                                .unwrap_or(false)
                    });
                if let Some(f) = family {
                    if g.api_version >= vk::API_VERSION_1_3 {
                        candidates.push((g, pd, f as u32));
                    }
                }
            }
            let chosen = match gpu {
                Some(i) => candidates.iter().position(|c| c.0.index == i),
                None => candidates
                    .iter()
                    .position(|c| c.0.vendor_id == 0x10DE && c.0.kind == vk::PhysicalDeviceType::DISCRETE_GPU)
                    .or_else(|| {
                        candidates
                            .iter()
                            .position(|c| c.0.kind == vk::PhysicalDeviceType::DISCRETE_GPU)
                    })
                    .or(if candidates.is_empty() { None } else { Some(0) }),
            }
            .ok_or("no Vulkan 1.3 device can present to the output window (see --list-gpus)")?;
            let (g, physical, queue_family) = (&candidates[chosen].0, candidates[chosen].1, candidates[chosen].2);
            let gpu_name = g.name.clone();

            let priorities = [1.0f32];
            let queues = [vk::DeviceQueueCreateInfo::default()
                .queue_family_index(queue_family)
                .queue_priorities(&priorities)];
            let dev_exts = [khr::swapchain::NAME.as_ptr()];
            let mut f13 = vk::PhysicalDeviceVulkan13Features::default()
                .dynamic_rendering(true)
                .synchronization2(true);
            let device = instance.create_device(
                physical,
                &vk::DeviceCreateInfo::default()
                    .queue_create_infos(&queues)
                    .enabled_extension_names(&dev_exts)
                    .push_next(&mut f13),
                None,
            )?;
            let queue = device.get_device_queue(queue_family, 0);
            let swapchain_fn = khr::swapchain::Device::new(&instance, &device);

            let pool = device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(queue_family)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )?;
            let cmd = device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )?[0];
            let image_available = device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?;
            let in_flight = device.create_fence(
                &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                None,
            )?;

            // Descriptor set: one combined image sampler (the captured frame).
            let bindings = [vk::DescriptorSetLayoutBinding::default()
                .binding(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT)];
            let set_layout = device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
                None,
            )?;
            let sizes = [vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)];
            let desc_pool = device.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default().max_sets(1).pool_sizes(&sizes),
                None,
            )?;
            let set_layouts = [set_layout];
            let desc_set = device.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(desc_pool)
                    .set_layouts(&set_layouts),
            )?[0];
            let sampler = device.create_sampler(
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
                .size(40)];
            let pipeline_layout = device.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default()
                    .set_layouts(&set_layouts)
                    .push_constant_ranges(&push),
                None,
            )?;

            let mut r = Self {
                _entry: entry,
                instance,
                surface_fn,
                surface,
                gpu_name,
                physical,
                device,
                queue,
                swapchain_fn,
                swapchain: vk::SwapchainKHR::null(),
                format: vk::Format::UNDEFINED,
                extent: vk::Extent2D { width: 0, height: 0 },
                images: Vec::new(),
                views: Vec::new(),
                render_done: Vec::new(),
                image_available,
                in_flight,
                pool,
                cmd,
                set_layout,
                pipeline_layout,
                pipeline: vk::Pipeline::null(),
                desc_pool,
                desc_set,
                sampler,
                tex: None,
                upload_pending: false,
                integer_scale,
                view: View::full(1, 1),
            };
            r.create_swapchain(hwnd_client_size(hwnd))?;
            r.create_pipeline()?;
            Ok(r)
        }
    }

    /// Renders one frame. `frame` is a newly captured BGRA8 image, if any. `client` is the current
    /// client size of the output window.
    pub fn render(&mut self, frame: Option<(u32, u32, &[u8])>, view: View, client: (u32, u32)) -> Res<()> {
        self.view = view;
        if client.0 == 0 || client.1 == 0 {
            return Ok(()); // minimised
        }
        unsafe {
            self.device.wait_for_fences(&[self.in_flight], true, u64::MAX)?;

            if let Some((w, h, data)) = frame {
                if self.tex.as_ref().map(|t| (t.width, t.height)) != Some((w, h)) {
                    self.create_texture(w, h)?;
                }
                let t = self.tex.as_ref().unwrap();
                std::ptr::copy_nonoverlapping(data.as_ptr(), t.mapped, (w as usize * h as usize * 4).min(data.len()));
                self.upload_pending = true;
            }

            if (self.extent.width, self.extent.height) != client {
                self.device.device_wait_idle()?;
                self.create_swapchain(client)?;
            }

            let (index, _) = match self.swapchain_fn.acquire_next_image(
                self.swapchain,
                u64::MAX,
                self.image_available,
                vk::Fence::null(),
            ) {
                Ok(v) => v,
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                    self.device.device_wait_idle()?;
                    self.create_swapchain(client)?;
                    return Ok(());
                }
                Err(e) => return Err(e.into()),
            };
            self.device.reset_fences(&[self.in_flight])?;
            self.device
                .reset_command_buffer(self.cmd, vk::CommandBufferResetFlags::empty())?;
            self.device.begin_command_buffer(
                self.cmd,
                &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            self.record(index as usize);
            self.device.end_command_buffer(self.cmd)?;

            let wait = [self.image_available];
            let stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
            let cmds = [self.cmd];
            let signal = [self.render_done[index as usize]];
            self.device.queue_submit(
                self.queue,
                &[vk::SubmitInfo::default()
                    .wait_semaphores(&wait)
                    .wait_dst_stage_mask(&stages)
                    .command_buffers(&cmds)
                    .signal_semaphores(&signal)],
                self.in_flight,
            )?;
            let swapchains = [self.swapchain];
            let indices = [index];
            match self.swapchain_fn.queue_present(
                self.queue,
                &vk::PresentInfoKHR::default()
                    .wait_semaphores(&signal)
                    .swapchains(&swapchains)
                    .image_indices(&indices),
            ) {
                Ok(_) | Err(vk::Result::SUBOPTIMAL_KHR) => {}
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                    self.device.device_wait_idle()?;
                    self.create_swapchain(client)?;
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
}

pub(super) fn color_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .level_count(1)
        .layer_count(1)
}

// A barrier needs all of these values and a struct would only add noise.
#[allow(clippy::too_many_arguments)]
/// Records an image layout barrier (synchronization2).
///
/// # Safety
/// `cb` must be in the recording state on `device` and `image` must be a live image of `device`.
pub(super) unsafe fn barrier(
    d: &Device,
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

pub(super) fn hwnd_client_size(hwnd: isize) -> (u32, u32) {
    crate::output::client_size(windows::Win32::Foundation::HWND(hwnd as *mut std::ffi::c_void))
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            self.destroy_texture();
            self.destroy_swapchain_views();
            self.swapchain_fn.destroy_swapchain(self.swapchain, None);
            self.device.destroy_pipeline(self.pipeline, None);
            self.device.destroy_pipeline_layout(self.pipeline_layout, None);
            self.device.destroy_sampler(self.sampler, None);
            self.device.destroy_descriptor_pool(self.desc_pool, None);
            self.device.destroy_descriptor_set_layout(self.set_layout, None);
            self.device.destroy_fence(self.in_flight, None);
            self.device.destroy_semaphore(self.image_available, None);
            self.device.destroy_command_pool(self.pool, None);
            self.device.destroy_device(None);
            self.surface_fn.destroy_surface(self.surface, None);
            self.instance.destroy_instance(None);
        }
    }
}

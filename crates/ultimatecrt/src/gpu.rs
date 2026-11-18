//! Vulkan renderer: uploads the captured frame to a texture and draws it into the swapchain with
//! the GLSL passes of `shaders/`. Vulkan 1.3 with dynamic rendering and synchronization2.
//!
//! The swapchain is B8G8R8A8_UNORM, so the passthrough shader writes captured values unchanged.

use std::{error::Error, io::Cursor};

use ash::{khr, vk, Device, Entry, Instance};

use crate::scaling::View;

type Res<T> = Result<T, Box<dyn Error>>;

const VERT_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/fullscreen.vert.spv"));
const FRAG_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/passthrough.frag.spv"));

pub struct GpuInfo {
    pub index: usize,
    pub name: String,
    pub kind: vk::PhysicalDeviceType,
    pub vendor_id: u32,
    pub api_version: u32,
}

/// Lists the Vulkan devices without creating a surface.
pub fn list_gpus() -> Res<Vec<GpuInfo>> {
    let entry = unsafe { Entry::load()? };
    let instance = unsafe { entry.create_instance(&vk::InstanceCreateInfo::default(), None)? };
    let out = unsafe { instance.enumerate_physical_devices()? }
        .into_iter()
        .enumerate()
        .map(|(index, pd)| gpu_info(&instance, pd, index))
        .collect();
    unsafe { instance.destroy_instance(None) };
    Ok(out)
}

fn gpu_info(instance: &Instance, pd: vk::PhysicalDevice, index: usize) -> GpuInfo {
    let p = unsafe { instance.get_physical_device_properties(pd) };
    GpuInfo {
        index,
        name: p
            .device_name_as_c_str()
            .map(|c| c.to_string_lossy().into_owned())
            .unwrap_or_default(),
        kind: p.device_type,
        vendor_id: p.vendor_id,
        api_version: p.api_version,
    }
}

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

    unsafe fn create_swapchain(&mut self, size: (u32, u32)) -> Res<()> {
        let caps = self
            .surface_fn
            .get_physical_device_surface_capabilities(self.physical, self.surface)?;
        let formats = self
            .surface_fn
            .get_physical_device_surface_formats(self.physical, self.surface)?;
        let chosen = formats
            .iter()
            .find(|f| f.format == vk::Format::B8G8R8A8_UNORM && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR)
            .or_else(|| {
                eprintln!(
                    "warning: the surface does not offer B8G8R8A8_UNORM with SRGB_NONLINEAR; using the first format, passthrough is not exact"
                );
                formats.first()
            })
            .ok_or("surface has no formats")?;
        self.format = chosen.format;
        self.extent = if caps.current_extent.width != u32::MAX {
            caps.current_extent
        } else {
            vk::Extent2D {
                width: size.0.clamp(caps.min_image_extent.width, caps.max_image_extent.width),
                height: size.1.clamp(caps.min_image_extent.height, caps.max_image_extent.height),
            }
        };
        let mut count = caps.min_image_count + 1;
        if caps.max_image_count > 0 {
            count = count.min(caps.max_image_count);
        }
        let old = self.swapchain;
        self.swapchain = self.swapchain_fn.create_swapchain(
            &vk::SwapchainCreateInfoKHR::default()
                .surface(self.surface)
                .min_image_count(count)
                .image_format(chosen.format)
                .image_color_space(chosen.color_space)
                .image_extent(self.extent)
                .image_array_layers(1)
                .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
                .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
                .pre_transform(caps.current_transform)
                .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
                .present_mode(vk::PresentModeKHR::FIFO)
                .clipped(true)
                .old_swapchain(old),
            None,
        )?;
        self.destroy_swapchain_views();
        if old != vk::SwapchainKHR::null() {
            self.swapchain_fn.destroy_swapchain(old, None);
        }
        self.images = self.swapchain_fn.get_swapchain_images(self.swapchain)?;
        for &image in &self.images {
            let view = self.device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(self.format)
                    .subresource_range(color_range()),
                None,
            )?;
            self.views.push(view);
            self.render_done.push(
                self.device
                    .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?,
            );
        }
        Ok(())
    }

    unsafe fn destroy_swapchain_views(&mut self) {
        for v in self.views.drain(..) {
            self.device.destroy_image_view(v, None);
        }
        for s in self.render_done.drain(..) {
            self.device.destroy_semaphore(s, None);
        }
    }

    unsafe fn create_pipeline(&mut self) -> Res<()> {
        let vert = self.shader_module(VERT_SPV)?;
        let frag = self.shader_module(FRAG_SPV)?;
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(vert)
                .name(c"main"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(frag)
                .name(c"main"),
        ];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
        let assembly =
            vk::PipelineInputAssemblyStateCreateInfo::default().topology(vk::PrimitiveTopology::TRIANGLE_STRIP);
        let viewport = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .line_width(1.0);
        let multisample =
            vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let attachments =
            [vk::PipelineColorBlendAttachmentState::default().color_write_mask(vk::ColorComponentFlags::RGBA)];
        let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&attachments);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
        let formats = [self.format];
        let mut rendering = vk::PipelineRenderingCreateInfo::default().color_attachment_formats(&formats);
        let info = vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&assembly)
            .viewport_state(&viewport)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .color_blend_state(&blend)
            .dynamic_state(&dynamic)
            .layout(self.pipeline_layout)
            .push_next(&mut rendering);
        let result = self
            .device
            .create_graphics_pipelines(vk::PipelineCache::null(), &[info], None);
        self.device.destroy_shader_module(vert, None);
        self.device.destroy_shader_module(frag, None);
        self.pipeline = result.map_err(|(_, e)| e)?[0];
        Ok(())
    }

    unsafe fn shader_module(&self, spv: &[u8]) -> Res<vk::ShaderModule> {
        let words = ash::util::read_spv(&mut Cursor::new(spv))?;
        Ok(self
            .device
            .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None)?)
    }

    fn memory_type(&self, bits: u32, flags: vk::MemoryPropertyFlags) -> Res<u32> {
        let props = unsafe { self.instance.get_physical_device_memory_properties(self.physical) };
        (0..props.memory_type_count)
            .find(|&i| bits & (1 << i) != 0 && props.memory_types[i as usize].property_flags.contains(flags))
            .ok_or_else(|| "no suitable memory type".into())
    }

    /// (Re)creates the frame texture and its staging buffer for a new capture size.
    unsafe fn create_texture(&mut self, width: u32, height: u32) -> Res<()> {
        self.device.device_wait_idle()?;
        self.destroy_texture();

        let image = self.device.create_image(
            &vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(vk::Format::B8G8R8A8_UNORM)
                .extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
                .initial_layout(vk::ImageLayout::UNDEFINED),
            None,
        )?;
        let req = self.device.get_image_memory_requirements(image);
        let memory = self.device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(req.size)
                .memory_type_index(self.memory_type(req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)?),
            None,
        )?;
        self.device.bind_image_memory(image, memory, 0)?;
        let view = self.device.create_image_view(
            &vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(vk::Format::B8G8R8A8_UNORM)
                .subresource_range(color_range()),
            None,
        )?;

        let bytes = width as u64 * height as u64 * 4;
        let staging = self.device.create_buffer(
            &vk::BufferCreateInfo::default()
                .size(bytes)
                .usage(vk::BufferUsageFlags::TRANSFER_SRC),
            None,
        )?;
        let sreq = self.device.get_buffer_memory_requirements(staging);
        let staging_memory = self.device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(sreq.size)
                .memory_type_index(self.memory_type(
                    sreq.memory_type_bits,
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                )?),
            None,
        )?;
        self.device.bind_buffer_memory(staging, staging_memory, 0)?;
        let mapped =
            self.device
                .map_memory(staging_memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty())? as *mut u8;

        let image_info = [vk::DescriptorImageInfo::default()
            .sampler(self.sampler)
            .image_view(view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        self.device.update_descriptor_sets(
            &[vk::WriteDescriptorSet::default()
                .dst_set(self.desc_set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&image_info)],
            &[],
        );
        self.tex = Some(Texture {
            width,
            height,
            image,
            memory,
            view,
            staging,
            staging_memory,
            mapped,
            layout: vk::ImageLayout::UNDEFINED,
        });
        Ok(())
    }

    unsafe fn destroy_texture(&mut self) {
        if let Some(t) = self.tex.take() {
            self.device.unmap_memory(t.staging_memory);
            self.device.destroy_buffer(t.staging, None);
            self.device.free_memory(t.staging_memory, None);
            self.device.destroy_image_view(t.view, None);
            self.device.destroy_image(t.image, None);
            self.device.free_memory(t.memory, None);
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

    unsafe fn record(&mut self, index: usize) {
        let cb = self.cmd;
        let d = &self.device;

        if self.upload_pending {
            if let Some(t) = self.tex.as_mut() {
                barrier(
                    d,
                    cb,
                    t.image,
                    t.layout,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::PipelineStageFlags2::FRAGMENT_SHADER,
                    vk::AccessFlags2::SHADER_READ,
                    vk::PipelineStageFlags2::TRANSFER,
                    vk::AccessFlags2::TRANSFER_WRITE,
                );
                d.cmd_copy_buffer_to_image(
                    cb,
                    t.staging,
                    t.image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[vk::BufferImageCopy::default()
                        .image_subresource(
                            vk::ImageSubresourceLayers::default()
                                .aspect_mask(vk::ImageAspectFlags::COLOR)
                                .layer_count(1),
                        )
                        .image_extent(vk::Extent3D {
                            width: t.width,
                            height: t.height,
                            depth: 1,
                        })],
                );
                barrier(
                    d,
                    cb,
                    t.image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::PipelineStageFlags2::TRANSFER,
                    vk::AccessFlags2::TRANSFER_WRITE,
                    vk::PipelineStageFlags2::FRAGMENT_SHADER,
                    vk::AccessFlags2::SHADER_READ,
                );
                t.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
            }
            self.upload_pending = false;
        }

        let target = self.images[index];
        barrier(
            d,
            cb,
            target,
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags2::NONE,
            vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
        );

        let attachment = [vk::RenderingAttachmentInfo::default()
            .image_view(self.views[index])
            .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::STORE)
            .clear_value(vk::ClearValue {
                color: vk::ClearColorValue {
                    float32: [0.0, 0.0, 0.0, 1.0],
                },
            })];
        let area = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: self.extent,
        };
        d.cmd_begin_rendering(
            cb,
            &vk::RenderingInfo::default()
                .render_area(area)
                .layer_count(1)
                .color_attachments(&attachment),
        );
        if self.tex.is_some() {
            // Letterbox the native raster into the window, optionally on integer scales only.
            let v = self.view;
            let (nw, nh) = (v.native.0 as f32, v.native.1 as f32);
            let (ew, eh) = (self.extent.width as f32, self.extent.height as f32);
            let mut scale = (ew / nw).min(eh / nh);
            if self.integer_scale && scale >= 1.0 {
                scale = scale.floor();
            }
            let (hx, hy) = (nw * scale / ew, nh * scale / eh);
            let rect: [f32; 4] = [-hx, -hy, hx, hy];
            d.cmd_set_viewport(
                cb,
                0,
                &[vk::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: ew,
                    height: eh,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            d.cmd_set_scissor(cb, 0, &[area]);
            d.cmd_bind_pipeline(cb, vk::PipelineBindPoint::GRAPHICS, self.pipeline);
            d.cmd_bind_descriptor_sets(
                cb,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipeline_layout,
                0,
                &[self.desc_set],
                &[],
            );
            let ints = [
                v.origin.0 as i32,
                v.origin.1 as i32,
                v.div.0 as i32,
                v.div.1 as i32,
                v.native.0 as i32,
                v.native.1 as i32,
            ];
            let mut bytes: Vec<u8> = rect.iter().flat_map(|f| f.to_ne_bytes()).collect();
            bytes.extend(ints.iter().flat_map(|i| i.to_ne_bytes()));
            d.cmd_push_constants(
                cb,
                self.pipeline_layout,
                vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                0,
                &bytes,
            );
            d.cmd_draw(cb, 4, 1, 0, 0);
        }
        d.cmd_end_rendering(cb);

        barrier(
            d,
            cb,
            target,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::ImageLayout::PRESENT_SRC_KHR,
            vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
            vk::PipelineStageFlags2::NONE,
            vk::AccessFlags2::NONE,
        );
    }
}

fn color_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .level_count(1)
        .layer_count(1)
}

// A barrier needs all of these values and a struct would only add noise.
#[allow(clippy::too_many_arguments)]
unsafe fn barrier(
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

fn hwnd_client_size(hwnd: isize) -> (u32, u32) {
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

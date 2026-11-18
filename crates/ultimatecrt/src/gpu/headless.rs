//! Headless renderer: runs the GPU passes without any window or swapchain, into an offscreen
//! floating-point image, and reads the result back to the CPU (TEC-7, ARCHITECTURE A16).
//!
//! Used by the GPU tests (`tests/gpu_*.rs`) and, later, by offline renders. Every call is
//! synchronous: it records one command buffer, submits it, waits for its fence and returns the
//! pixels. It allocates its scratch images and buffers per call, which is fine off the render loop.

use super::*;

/// Format of the offscreen target. Both are mandatory colour-attachment formats in Vulkan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetFormat {
    /// `R16G16B16A16_SFLOAT`, the planned internal format of the pipeline (A10).
    Rgba16F,
    /// `R32G32B32A32_SFLOAT`, for tests that need more precision than FP16.
    Rgba32F,
}

impl TargetFormat {
    fn vk(self) -> vk::Format {
        match self {
            TargetFormat::Rgba16F => vk::Format::R16G16B16A16_SFLOAT,
            TargetFormat::Rgba32F => vk::Format::R32G32B32A32_SFLOAT,
        }
    }

    fn bytes_per_pixel(self) -> usize {
        match self {
            TargetFormat::Rgba16F => 8,
            TargetFormat::Rgba32F => 16,
        }
    }
}

/// RGBA image of `f32` values, rows from top to bottom, 4 values per pixel.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageF32 {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<f32>,
}

impl ImageF32 {
    /// The RGBA value of pixel (`x`, `y`). Panics outside the image.
    pub fn pixel(&self, x: u32, y: u32) -> [f32; 4] {
        assert!(x < self.width && y < self.height, "pixel ({x}, {y}) outside the image");
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [self.rgba[i], self.rgba[i + 1], self.rgba[i + 2], self.rgba[i + 3]]
    }
}

/// Offscreen renderer on a surface-less [`Context`].
///
/// Every Vulkan handle starts null and is filled in by `new`, so `Drop` releases whatever was
/// created even when `new` fails half way (destroying a null handle is a no-op in Vulkan).
pub struct Headless {
    pool: vk::CommandPool,
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    desc_pool: vk::DescriptorPool,
    desc_set: vk::DescriptorSet,
    sampler: vk::Sampler,
    /// Passthrough pipelines, created on first use for each target format.
    pipelines: Vec<(vk::Format, vk::Pipeline)>,
    /// Destroyed last, by the field drop after `Drop for Headless`.
    ctx: Context,
}

impl Headless {
    /// Creates a headless context ([`Context::new_headless`]) and the objects of the passthrough pass.
    /// With `validation`, the validation layer must be installed and its messages are counted
    /// ([`Headless::validation_counter`]).
    pub fn new(validation: bool) -> Res<Self> {
        let ctx = Context::new_headless(validation)?;
        let mut h = Self {
            pool: vk::CommandPool::null(),
            cmd: vk::CommandBuffer::null(),
            fence: vk::Fence::null(),
            set_layout: vk::DescriptorSetLayout::null(),
            pipeline_layout: vk::PipelineLayout::null(),
            desc_pool: vk::DescriptorPool::null(),
            desc_set: vk::DescriptorSet::null(),
            sampler: vk::Sampler::null(),
            pipelines: Vec::new(),
            ctx,
        };
        // SAFETY: called once on fresh null handles of a live device.
        unsafe { h.create_objects()? };
        Ok(h)
    }

    /// The Vulkan context (device, queue, chosen GPU).
    pub fn context(&self) -> &Context {
        &self.ctx
    }

    /// Shared count of validation messages, readable after `self` is dropped.
    pub fn validation_counter(&self) -> ValidationCounter {
        self.ctx.validation_counter()
    }

    /// # Safety
    /// Called once, from `new`, while the handles are still null.
    unsafe fn create_objects(&mut self) -> Res<()> {
        let d = &self.ctx.device;
        self.pool = d.create_command_pool(
            &vk::CommandPoolCreateInfo::default()
                .queue_family_index(self.ctx.queue_family)
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
            None,
        )?;
        self.cmd = d.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::default()
                .command_pool(self.pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1),
        )?[0];
        self.fence = d.create_fence(&vk::FenceCreateInfo::default(), None)?;
        self.set_layout = passthrough::create_set_layout(d)?;
        let sizes = [vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)];
        self.desc_pool = d.create_descriptor_pool(
            &vk::DescriptorPoolCreateInfo::default().max_sets(1).pool_sizes(&sizes),
            None,
        )?;
        let set_layouts = [self.set_layout];
        self.desc_set = d.allocate_descriptor_sets(
            &vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(self.desc_pool)
                .set_layouts(&set_layouts),
        )?[0];
        self.sampler = passthrough::create_sampler(d)?;
        self.pipeline_layout = passthrough::create_pipeline_layout(d, self.set_layout)?;
        Ok(())
    }

    /// Passthrough pipeline for `format`, created on first use.
    ///
    /// # Safety
    /// The device and `pipeline_layout` must be live (true after `new`).
    unsafe fn pipeline(&mut self, format: vk::Format) -> Res<vk::Pipeline> {
        if let Some(&(_, p)) = self.pipelines.iter().find(|(f, _)| *f == format) {
            return Ok(p);
        }
        let p = passthrough::create_pipeline(&self.ctx.device, self.pipeline_layout, format)?;
        self.pipelines.push((format, p));
        Ok(p)
    }

    /// Uploads a BGRA8 (UNORM) image, as captured, runs the passthrough pass over its full raster
    /// (no crop, divisor 1) into a `format` target of the same size, and reads the target back.
    ///
    /// The values are the shader output unchanged: code k becomes k / 255 (rounded to FP16 for
    /// [`TargetFormat::Rgba16F`]), alpha is 1.
    pub fn passthrough(&mut self, width: u32, height: u32, bgra: &[u8], format: TargetFormat) -> Res<ImageF32> {
        if width == 0 || height == 0 {
            return Err("headless passthrough: empty image".into());
        }
        let in_bytes = width as usize * height as usize * 4;
        if bgra.len() < in_bytes {
            return Err(format!(
                "headless passthrough: {} bytes given, {width}x{height} BGRA8 needs {in_bytes}",
                bgra.len()
            )
            .into());
        }
        let mut s = Scratch::default();
        // SAFETY: every handle is created on `self.ctx.device`. `run` waits for the fence of its
        // only submission before returning Ok; on any path the device is idled before `s` is
        // destroyed, so the GPU no longer uses the scratch objects.
        unsafe {
            let out = self.run(&mut s, width, height, &bgra[..in_bytes], format);
            let _ = self.ctx.device.device_wait_idle();
            s.destroy(&self.ctx.device);
            out
        }
    }

    /// Body of [`Headless::passthrough`]; every object it creates goes into `s`.
    ///
    /// # Safety
    /// `s` must be empty (null handles); the caller idles the device and destroys `s` afterwards.
    unsafe fn run(
        &mut self,
        s: &mut Scratch,
        width: u32,
        height: u32,
        bgra: &[u8],
        format: TargetFormat,
    ) -> Res<ImageF32> {
        let pipeline = self.pipeline(format.vk())?;
        let ctx = &self.ctx;
        let d = &ctx.device;
        let extent = vk::Extent2D { width, height };
        let out_bytes = width as usize * height as usize * format.bytes_per_pixel();

        s.src = GpuImage::create(
            ctx,
            extent,
            vk::Format::B8G8R8A8_UNORM,
            vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED,
        )?;
        s.dst = GpuImage::create(
            ctx,
            extent,
            format.vk(),
            vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC,
        )?;
        s.upload = HostBuffer::create(ctx, bgra.len(), vk::BufferUsageFlags::TRANSFER_SRC)?;
        s.readback = HostBuffer::create(ctx, out_bytes, vk::BufferUsageFlags::TRANSFER_DST)?;
        // Host writes before vkQueueSubmit are visible to the device (implicit host write ordering).
        std::ptr::copy_nonoverlapping(bgra.as_ptr(), s.upload.mapped, bgra.len());

        let image_info = [vk::DescriptorImageInfo::default()
            .sampler(self.sampler)
            .image_view(s.src.view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        // The set is not in use: the previous call waited for its fence.
        d.update_descriptor_sets(
            &[vk::WriteDescriptorSet::default()
                .dst_set(self.desc_set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&image_info)],
            &[],
        );

        let cb = self.cmd;
        d.reset_command_buffer(cb, vk::CommandBufferResetFlags::empty())?;
        d.begin_command_buffer(
            cb,
            &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        )?;

        // Upload: staging buffer -> source image, then make it readable by the fragment shader.
        barrier(
            d,
            cb,
            s.src.image,
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::PipelineStageFlags2::NONE,
            vk::AccessFlags2::NONE,
            vk::PipelineStageFlags2::COPY,
            vk::AccessFlags2::TRANSFER_WRITE,
        );
        d.cmd_copy_buffer_to_image(
            cb,
            s.upload.buffer,
            s.src.image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &[buffer_image_copy(extent)],
        );
        barrier(
            d,
            cb,
            s.src.image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            vk::PipelineStageFlags2::COPY,
            vk::AccessFlags2::TRANSFER_WRITE,
            vk::PipelineStageFlags2::FRAGMENT_SHADER,
            vk::AccessFlags2::SHADER_SAMPLED_READ,
        );

        // Draw the full raster into the target.
        barrier(
            d,
            cb,
            s.dst.image,
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::PipelineStageFlags2::NONE,
            vk::AccessFlags2::NONE,
            vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
        );
        let attachment = [vk::RenderingAttachmentInfo::default()
            .image_view(s.dst.view)
            .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::STORE)
            .clear_value(vk::ClearValue {
                color: vk::ClearColorValue {
                    float32: [0.0, 0.0, 0.0, 0.0],
                },
            })];
        let area = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent,
        };
        d.cmd_begin_rendering(
            cb,
            &vk::RenderingInfo::default()
                .render_area(area)
                .layer_count(1)
                .color_attachments(&attachment),
        );
        d.cmd_set_viewport(
            cb,
            0,
            &[vk::Viewport {
                x: 0.0,
                y: 0.0,
                width: width as f32,
                height: height as f32,
                min_depth: 0.0,
                max_depth: 1.0,
            }],
        );
        d.cmd_set_scissor(cb, 0, &[area]);
        d.cmd_bind_pipeline(cb, vk::PipelineBindPoint::GRAPHICS, pipeline);
        d.cmd_bind_descriptor_sets(
            cb,
            vk::PipelineBindPoint::GRAPHICS,
            self.pipeline_layout,
            0,
            &[self.desc_set],
            &[],
        );
        passthrough::PushConstants::new([-1.0, -1.0, 1.0, 1.0], View::full(width, height)).push(
            d,
            cb,
            self.pipeline_layout,
        );
        d.cmd_draw(cb, 4, 1, 0, 0);
        d.cmd_end_rendering(cb);

        // Read back: target -> host-visible buffer, then make the copy visible to host reads.
        barrier(
            d,
            cb,
            s.dst.image,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
            vk::PipelineStageFlags2::COPY,
            vk::AccessFlags2::TRANSFER_READ,
        );
        d.cmd_copy_image_to_buffer(
            cb,
            s.dst.image,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            s.readback.buffer,
            &[buffer_image_copy(extent)],
        );
        let to_host = [vk::BufferMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::COPY)
            .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::HOST)
            .dst_access_mask(vk::AccessFlags2::HOST_READ)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .buffer(s.readback.buffer)
            .offset(0)
            .size(vk::WHOLE_SIZE)];
        d.cmd_pipeline_barrier2(cb, &vk::DependencyInfo::default().buffer_memory_barriers(&to_host));
        d.end_command_buffer(cb)?;

        let cmds = [cb];
        d.queue_submit(
            ctx.queue,
            &[vk::SubmitInfo::default().command_buffers(&cmds)],
            self.fence,
        )?;
        let waited = d.wait_for_fences(&[self.fence], true, u64::MAX);
        d.reset_fences(&[self.fence])?;
        waited?;

        // The readback memory is HOST_COHERENT: after the HOST_READ barrier and the fence wait,
        // the copy is visible to the host without an invalidate.
        let bytes = std::slice::from_raw_parts(s.readback.mapped as *const u8, out_bytes);
        let rgba = match format {
            TargetFormat::Rgba16F => bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&b| f16_to_f32(u16::from_le_bytes(b)))
                .collect(),
            TargetFormat::Rgba32F => bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|&b| f32::from_le_bytes(b))
                .collect(),
        };
        Ok(ImageF32 { width, height, rgba })
    }
}

impl Drop for Headless {
    fn drop(&mut self) {
        // SAFETY: after `device_wait_idle` the GPU uses none of these objects. They are destroyed
        // in reverse order of creation, null ones (a failed `new`) are no-ops, and the device and
        // instance go last with the field drop of `ctx`.
        unsafe {
            let d = &self.ctx.device;
            let _ = d.device_wait_idle();
            for (_, p) in self.pipelines.drain(..) {
                d.destroy_pipeline(p, None);
            }
            d.destroy_pipeline_layout(self.pipeline_layout, None);
            d.destroy_sampler(self.sampler, None);
            d.destroy_descriptor_pool(self.desc_pool, None);
            d.destroy_descriptor_set_layout(self.set_layout, None);
            d.destroy_fence(self.fence, None);
            d.destroy_command_pool(self.pool, None);
        }
    }
}

/// Copy region covering a whole tightly packed 2D colour image.
fn buffer_image_copy(extent: vk::Extent2D) -> vk::BufferImageCopy {
    vk::BufferImageCopy::default()
        .image_subresource(
            vk::ImageSubresourceLayers::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .layer_count(1),
        )
        .image_extent(vk::Extent3D {
            width: extent.width,
            height: extent.height,
            depth: 1,
        })
}

/// Objects of one headless call. Null handles by default; `destroy` releases what was created.
#[derive(Default)]
struct Scratch {
    src: GpuImage,
    dst: GpuImage,
    upload: HostBuffer,
    readback: HostBuffer,
}

impl Scratch {
    /// # Safety
    /// The GPU must have finished every use of the objects (device idle).
    unsafe fn destroy(&mut self, d: &ash::Device) {
        self.readback.destroy(d);
        self.upload.destroy(d);
        self.dst.destroy(d);
        self.src.destroy(d);
    }
}

/// Device-local 2D image with one mip level, one layer, and its colour view.
#[derive(Default)]
pub(super) struct GpuImage {
    pub image: vk::Image,
    pub memory: vk::DeviceMemory,
    pub view: vk::ImageView,
}

impl GpuImage {
    /// Creates the image (initial layout `UNDEFINED`), binds device-local memory and creates the view.
    /// On error, the partly created objects are destroyed.
    ///
    /// # Safety
    /// `ctx` must be live; the caller destroys the result with [`GpuImage::destroy`] once the GPU
    /// no longer uses it.
    pub unsafe fn create(
        ctx: &Context,
        extent: vk::Extent2D,
        format: vk::Format,
        usage: vk::ImageUsageFlags,
    ) -> Res<Self> {
        let d = &ctx.device;
        let mut g = Self {
            image: d.create_image(
                &vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(format)
                    .extent(vk::Extent3D {
                        width: extent.width,
                        height: extent.height,
                        depth: 1,
                    })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(usage)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .initial_layout(vk::ImageLayout::UNDEFINED),
                None,
            )?,
            ..Self::default()
        };
        let r = (|| -> Res<()> {
            let req = d.get_image_memory_requirements(g.image);
            g.memory = d.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(req.size)
                    .memory_type_index(ctx.memory_type(req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)?),
                None,
            )?;
            d.bind_image_memory(g.image, g.memory, 0)?;
            g.view = d.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(g.image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(format)
                    .subresource_range(color_range()),
                None,
            )?;
            Ok(())
        })();
        match r {
            Ok(()) => Ok(g),
            Err(e) => {
                g.destroy(d);
                Err(e)
            }
        }
    }

    /// Destroys the view, image and memory (null handles are no-ops) and resets them to null.
    ///
    /// # Safety
    /// The GPU must have finished every use of the image.
    pub unsafe fn destroy(&mut self, d: &ash::Device) {
        d.destroy_image_view(self.view, None);
        d.destroy_image(self.image, None);
        d.free_memory(self.memory, None);
        *self = Self::default();
    }
}

/// Host-visible, host-coherent buffer, persistently mapped (host-cached memory preferred, for reads).
pub(super) struct HostBuffer {
    pub buffer: vk::Buffer,
    pub memory: vk::DeviceMemory,
    pub mapped: *mut u8,
}

impl Default for HostBuffer {
    fn default() -> Self {
        Self {
            buffer: vk::Buffer::null(),
            memory: vk::DeviceMemory::null(),
            mapped: std::ptr::null_mut(),
        }
    }
}

impl HostBuffer {
    /// Creates a buffer of `size` bytes and maps all of it. On error, the partly created objects
    /// are destroyed.
    ///
    /// # Safety
    /// `ctx` must be live; the caller destroys the result with [`HostBuffer::destroy`] once the GPU
    /// no longer uses it, and accesses `mapped` only within `size` bytes.
    pub unsafe fn create(ctx: &Context, size: usize, usage: vk::BufferUsageFlags) -> Res<Self> {
        let d = &ctx.device;
        let mut b = Self {
            buffer: d.create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(size as u64)
                    .usage(usage)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE),
                None,
            )?,
            ..Self::default()
        };
        let r = (|| -> Res<()> {
            let req = d.get_buffer_memory_requirements(b.buffer);
            let coherent = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
            let index = ctx
                .memory_type(req.memory_type_bits, coherent | vk::MemoryPropertyFlags::HOST_CACHED)
                .or_else(|_| ctx.memory_type(req.memory_type_bits, coherent))?;
            b.memory = d.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(req.size)
                    .memory_type_index(index),
                None,
            )?;
            d.bind_buffer_memory(b.buffer, b.memory, 0)?;
            b.mapped = d.map_memory(b.memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty())? as *mut u8;
            Ok(())
        })();
        match r {
            Ok(()) => Ok(b),
            Err(e) => {
                b.destroy(d);
                Err(e)
            }
        }
    }

    /// Destroys the buffer and frees its memory (which unmaps it); null handles are no-ops.
    ///
    /// # Safety
    /// The GPU must have finished every use of the buffer; `mapped` must not be used afterwards.
    pub unsafe fn destroy(&mut self, d: &ash::Device) {
        d.destroy_buffer(self.buffer, None);
        d.free_memory(self.memory, None);
        *self = Self::default();
    }
}

/// Converts an IEEE 754 binary16 value to `f32` (exact: every half value is an `f32` value).
pub(super) fn f16_to_f32(h: u16) -> f32 {
    let negative = h & 0x8000 != 0;
    let exp = u32::from((h >> 10) & 0x1f);
    let man = u32::from(h & 0x3ff);
    let sign = if negative { 1u32 << 31 } else { 0 };
    let bits = match (exp, man) {
        (0, 0) => sign,
        (0, _) => {
            // Subnormal: man * 2^-24.
            let v = man as f32 / 16_777_216.0;
            return if negative { -v } else { v };
        }
        (31, _) => sign | 0x7f80_0000 | (man << 13),
        _ => sign | ((exp + 112) << 23) | (man << 13),
    };
    f32::from_bits(bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats_convert_exactly() {
        assert_eq!(f16_to_f32(0x0000), 0.0);
        assert!(f16_to_f32(0x8000).is_sign_negative());
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0xc000), -2.0);
        assert_eq!(f16_to_f32(0x3800), 0.5);
        assert_eq!(f16_to_f32(0x7bff), 65504.0);
        assert_eq!(f16_to_f32(0x0400), 2f32.powi(-14));
        assert_eq!(f16_to_f32(0x0001), 2f32.powi(-24));
        assert_eq!(f16_to_f32(0x83ff), -1023.0 * 2f32.powi(-24));
        assert_eq!(f16_to_f32(0x7c00), f32::INFINITY);
        assert!(f16_to_f32(0x7e00).is_nan());
        // 1/3 rounds to 0x3555 = 1365/4096.
        assert_eq!(f16_to_f32(0x3555), 1365.0 / 4096.0);
    }

    #[test]
    fn image_pixel_reads_rows_top_to_bottom() {
        let img = ImageF32 {
            width: 2,
            height: 2,
            rgba: (0..16).map(|v| v as f32).collect(),
        };
        assert_eq!(img.pixel(1, 0), [4.0, 5.0, 6.0, 7.0]);
        assert_eq!(img.pixel(0, 1), [8.0, 9.0, 10.0, 11.0]);
    }
}

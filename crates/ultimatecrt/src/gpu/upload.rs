//! Frame texture and staging buffer upload.

use super::*;

impl Renderer {
    /// (Re)creates the frame texture and its staging buffer for a new capture size.
    ///
    /// # Safety
    /// The device and descriptor set must be live; the function waits for device idle before replacing the old texture.
    pub(super) unsafe fn create_texture(&mut self, width: u32, height: u32) -> Res<()> {
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

    /// Destroys the frame texture and staging buffer, if any.
    ///
    /// # Safety
    /// The GPU must have finished every use of the texture (device idle).
    pub(super) unsafe fn destroy_texture(&mut self) {
        if let Some(t) = self.tex.take() {
            self.device.unmap_memory(t.staging_memory);
            self.device.destroy_buffer(t.staging, None);
            self.device.free_memory(t.staging_memory, None);
            self.device.destroy_image_view(t.view, None);
            self.device.destroy_image(t.image, None);
            self.device.free_memory(t.memory, None);
        }
    }
}

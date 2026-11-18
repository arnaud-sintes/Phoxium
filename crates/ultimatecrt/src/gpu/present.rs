//! Swapchain creation and teardown for the output window.

use super::*;

impl Renderer {
    /// (Re)creates the swapchain, its image views and per-image semaphores for `size`.
    ///
    /// # Safety
    /// The surface and device must be live and the GPU must not use the old swapchain images (device idle).
    pub(super) unsafe fn create_swapchain(&mut self, size: (u32, u32)) -> Res<()> {
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

    /// Destroys the swapchain image views and per-image semaphores.
    ///
    /// # Safety
    /// The GPU must have finished every use of them (device idle).
    pub(super) unsafe fn destroy_swapchain_views(&mut self) {
        for v in self.views.drain(..) {
            self.device.destroy_image_view(v, None);
        }
        for s in self.render_done.drain(..) {
            self.device.destroy_semaphore(s, None);
        }
    }
}

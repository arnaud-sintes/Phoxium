//! Presentation to one window: surface, swapchain, image views and per-image semaphores.
//!
//! One [`Presenter`] per output window, all sharing one [`Context`]. A presenter has no `Drop`:
//! it does not own the device, so its owner destroys it with [`Presenter::destroy`] before the
//! context, as `Renderer` does for its `Texture`.

use ash::{khr, vk};

use super::context::{create_win32_surface, Context};
use super::{color_range, Res};

pub struct Presenter {
    swapchain_fn: khr::swapchain::Device,
    surface: vk::SurfaceKHR,
    swapchain: vk::SwapchainKHR,
    format: vk::Format,
    /// The surface offers a 10-bit UNORM format with `SRGB_NONLINEAR` (COL-4); only recorded, the
    /// swapchain stays `B8G8R8A8_UNORM` (A7).
    ten_bit_offered: bool,
    extent: vk::Extent2D,
    images: Vec<vk::Image>,
    views: Vec<vk::ImageView>,
    /// Signalled by the frame submission, waited by the present of the same image.
    render_done: Vec<vk::Semaphore>,
}

impl Presenter {
    /// Creates a presenter for a further window of an existing window context.
    ///
    /// # Safety
    /// `ctx` must have been created with [`Context::with_window`]; `hwnd` must be a live window of
    /// `hinstance` and stay alive until [`Presenter::destroy`]. The presenter must be destroyed
    /// with [`Presenter::destroy`] before `ctx` is dropped.
    pub unsafe fn new(ctx: &Context, hwnd: isize, hinstance: isize, size: (u32, u32)) -> Res<Self> {
        if !ctx.can_present {
            return Err("this Vulkan context was created without presentation support".into());
        }
        let surface = create_win32_surface(&ctx.entry, &ctx.instance, hwnd, hinstance)?;
        Self::from_surface(ctx, surface, size)
    }

    /// Creates a presenter on `surface` and takes ownership of it (it is destroyed on error too).
    ///
    /// # Safety
    /// `surface` must be a live surface of `ctx.instance` whose window stays alive until
    /// [`Presenter::destroy`]; `ctx` must be a window context. The presenter must be destroyed with
    /// [`Presenter::destroy`] before `ctx` is dropped.
    pub unsafe fn from_surface(ctx: &Context, surface: vk::SurfaceKHR, size: (u32, u32)) -> Res<Self> {
        let mut p = Self {
            swapchain_fn: khr::swapchain::Device::new(&ctx.instance, &ctx.device),
            surface,
            swapchain: vk::SwapchainKHR::null(),
            format: vk::Format::UNDEFINED,
            ten_bit_offered: false,
            extent: vk::Extent2D { width: 0, height: 0 },
            images: Vec::new(),
            views: Vec::new(),
            render_done: Vec::new(),
        };
        let result = (|| -> Res<()> {
            let supported =
                ctx.surface_fn
                    .get_physical_device_surface_support(ctx.physical, ctx.queue_family, surface)?;
            if !supported {
                return Err(format!("{} cannot present to this window", ctx.gpu.name).into());
            }
            p.resize(ctx, size)
        })();
        match result {
            Ok(()) => Ok(p),
            Err(e) => {
                p.destroy(ctx);
                Err(e)
            }
        }
    }

    pub fn format(&self) -> vk::Format {
        self.format
    }

    /// Whether the surface offers a 10-bit SDR format (COL-4). The swapchain does not use it yet: `encode`
    /// already dithers to 1023 levels for such a target, once a later task switches the format.
    pub fn ten_bit_offered(&self) -> bool {
        self.ten_bit_offered
    }

    pub fn extent(&self) -> vk::Extent2D {
        self.extent
    }

    pub fn image(&self, index: u32) -> vk::Image {
        self.images[index as usize]
    }

    pub fn view(&self, index: u32) -> vk::ImageView {
        self.views[index as usize]
    }

    /// Semaphore the frame submission signals for image `index`; [`Presenter::present`] waits on it.
    pub fn render_done(&self, index: u32) -> vk::Semaphore {
        self.render_done[index as usize]
    }

    /// (Re)creates the swapchain, its image views and per-image semaphores for `size`.
    ///
    /// # Safety
    /// `ctx` is the presenter's context and the GPU no longer uses the old swapchain images, views
    /// or semaphores (device idle).
    pub unsafe fn resize(&mut self, ctx: &Context, size: (u32, u32)) -> Res<()> {
        let caps = ctx
            .surface_fn
            .get_physical_device_surface_capabilities(ctx.physical, self.surface)?;
        let formats = ctx
            .surface_fn
            .get_physical_device_surface_formats(ctx.physical, self.surface)?;
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
        self.ten_bit_offered = formats.iter().any(|f| {
            matches!(
                f.format,
                vk::Format::A2B10G10R10_UNORM_PACK32 | vk::Format::A2R10G10B10_UNORM_PACK32
            ) && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
        });
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
        self.destroy_views(ctx);
        if old != vk::SwapchainKHR::null() {
            self.swapchain_fn.destroy_swapchain(old, None);
        }
        self.images = self.swapchain_fn.get_swapchain_images(self.swapchain)?;
        for &image in &self.images {
            let view = ctx.device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(self.format)
                    .subresource_range(color_range()),
                None,
            )?;
            self.views.push(view);
            self.render_done
                .push(ctx.device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?);
        }
        Ok(())
    }

    /// Acquires the next image, signalling `signal` (one semaphore per frame in flight, owned by the
    /// caller). Returns `None` when the swapchain was out of date: it has been recreated for `size`
    /// and the frame is skipped (`signal` is not signalled).
    ///
    /// # Safety
    /// `ctx` is the presenter's context and `signal` is a live binary semaphore with no pending
    /// signal or wait (the submission that last waited on it has completed: its fence was waited).
    pub unsafe fn acquire(&mut self, ctx: &Context, size: (u32, u32), signal: vk::Semaphore) -> Res<Option<u32>> {
        match self
            .swapchain_fn
            .acquire_next_image(self.swapchain, u64::MAX, signal, vk::Fence::null())
        {
            Ok((index, _)) => Ok(Some(index)),
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                ctx.device.device_wait_idle()?;
                self.resize(ctx, size)?;
                Ok(None)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Presents image `index` on `ctx.queue` after [`Presenter::render_done`]`(index)`; recreates
    /// the swapchain for `size` if it is out of date.
    ///
    /// # Safety
    /// `ctx` is the presenter's context and `index` was acquired by [`Presenter::acquire`] and
    /// submitted with a signal of `render_done(index)`.
    pub unsafe fn present(&mut self, ctx: &Context, index: u32, size: (u32, u32)) -> Res<()> {
        let wait = [self.render_done(index)];
        let swapchains = [self.swapchain];
        let indices = [index];
        match self.swapchain_fn.queue_present(
            ctx.queue,
            &vk::PresentInfoKHR::default()
                .wait_semaphores(&wait)
                .swapchains(&swapchains)
                .image_indices(&indices),
        ) {
            Ok(_) | Err(vk::Result::SUBOPTIMAL_KHR) => Ok(()),
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                ctx.device.device_wait_idle()?;
                self.resize(ctx, size)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Destroys the swapchain, its views and semaphores, then the surface. Idempotent.
    ///
    /// # Safety
    /// `ctx` is the presenter's context and the GPU has finished every use of them (device idle).
    pub unsafe fn destroy(&mut self, ctx: &Context) {
        self.destroy_views(ctx);
        self.images.clear();
        if self.swapchain != vk::SwapchainKHR::null() {
            self.swapchain_fn.destroy_swapchain(self.swapchain, None);
            self.swapchain = vk::SwapchainKHR::null();
        }
        if self.surface != vk::SurfaceKHR::null() {
            ctx.surface_fn.destroy_surface(self.surface, None);
            self.surface = vk::SurfaceKHR::null();
        }
    }

    /// # Safety
    /// The GPU has finished every use of the views and semaphores (device idle).
    unsafe fn destroy_views(&mut self, ctx: &Context) {
        for v in self.views.drain(..) {
            ctx.device.destroy_image_view(v, None);
        }
        for s in self.render_done.drain(..) {
            ctx.device.destroy_semaphore(s, None);
        }
    }
}

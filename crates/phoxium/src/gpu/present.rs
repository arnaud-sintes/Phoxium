//! Presentation to one window: surface, swapchain, image views and per-image semaphores.
//!
//! One [`Presenter`] per output window, all sharing one [`Context`]. A presenter has no `Drop`:
//! it does not own the device, so its owner destroys it with [`Presenter::destroy`] before the
//! context, as `Renderer` does for its `Texture`.
//!
//! Recovery (R-01, R-36, R-43): the swapchain is recreated at the start of a frame ([`Presenter::ensure`]) when the
//! window size changed, when acquire or present said it is out of date or suboptimal, or after a failure left it
//! inconsistent; a lost surface (`ERROR_SURFACE_LOST_KHR`) is recreated from the window it was made for. A surface
//! whose current extent has a 0 side (a minimise race) keeps the old swapchain and the frame is not drawn. The
//! per-image `render_done` semaphores of a replaced swapchain may still be waited by a present the presentation
//! engine has not finished (`device_wait_idle` does not cover it, VUID-vkDestroySemaphore-semaphore-05149): they are
//! retired, and destroyed once every image of the new swapchain was acquired again, or in [`Presenter::destroy`].

use ash::{khr, vk};

use super::context::{create_win32_surface, Context};
use super::{color_range, GpuError, Res};

/// What [`Presenter::acquire`] got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Acquired {
    /// Image `index`, its semaphore will be signalled. With `suboptimal` the swapchain still works but is recreated
    /// before the next frame.
    Image { index: u32, suboptimal: bool },
    /// No image this time (out of date, surface lost, or none available within the timeout): the semaphore is not
    /// signalled, the frame is skipped, and the swapchain is recreated before the next one when needed.
    Skip,
}

/// How long an acquire may wait for a free image: with FIFO an image frees at the next vertical blank, so a longer
/// wait means the window cannot present now (hidden, being recreated); the frame is then skipped.
const ACQUIRE_TIMEOUT_NS: u64 = 1_000_000_000;

/// Retired semaphores kept at most; older ones are destroyed when a newer one is retired (the device was idled by
/// every recreation that retired them).
const RETIRED_MAX: usize = 64;

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
    /// The window (`hwnd`, `hinstance`) the surface was made for: a lost surface is recreated from it.
    window: Option<(isize, isize)>,
    /// The window size the swapchain was last made for.
    requested: (u32, u32),
    /// The swapchain must be recreated before the next acquire (out of date, suboptimal, a failed recreation, an
    /// acquired image given up).
    stale: bool,
    /// The surface was lost: it is recreated (then the swapchain) before the next acquire.
    surface_lost: bool,
    /// Successful acquires so far, the clock of the retired semaphores.
    acquires: u64,
    /// The swapchain images can be copied from (`TRANSFER_SRC` was offered): a snapshot of the output is possible.
    can_copy: bool,
    /// Asked for: present without waiting for the vertical blank when the surface offers it ([`choose_present_mode`]).
    tearing: bool,
    /// The present mode the swapchain was made with.
    present_mode: vk::PresentModeKHR,
    /// Semaphores of replaced swapchains and the acquire count from which they may be destroyed.
    retired: Vec<(vk::Semaphore, u64)>,
}

/// The present mode for a surface that offers `offered`: `IMMEDIATE` (no wait for the vertical blank, may tear) when
/// `tearing` is asked and offered, else `FIFO`, which every surface offers and which waits for the vertical blank.
pub fn choose_present_mode(offered: &[vk::PresentModeKHR], tearing: bool) -> vk::PresentModeKHR {
    if tearing && offered.contains(&vk::PresentModeKHR::IMMEDIATE) {
        vk::PresentModeKHR::IMMEDIATE
    } else {
        vk::PresentModeKHR::FIFO
    }
}

/// The swapchain extent for a window client of `size`: the surface's current extent when it has one, else `size`
/// clamped to the surface's limits; `None` when it has a 0 side (minimised, or a minimise race): no swapchain can be
/// made then.
pub fn extent_for(caps: &vk::SurfaceCapabilitiesKHR, size: (u32, u32)) -> Option<vk::Extent2D> {
    let e = if caps.current_extent.width != u32::MAX {
        caps.current_extent
    } else {
        vk::Extent2D {
            width: size.0.clamp(
                caps.min_image_extent.width,
                caps.max_image_extent.width.max(caps.min_image_extent.width),
            ),
            height: size.1.clamp(
                caps.min_image_extent.height,
                caps.max_image_extent.height.max(caps.min_image_extent.height),
            ),
        }
    };
    (e.width > 0 && e.height > 0 && size.0 > 0 && size.1 > 0).then_some(e)
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
        Self::from_surface(ctx, surface, Some((hwnd, hinstance)), size)
    }

    /// Creates a presenter on `surface` and takes ownership of it (it is destroyed on error too). `window` is the
    /// window (`hwnd`, `hinstance`) the surface was made for, from which a lost surface is recreated (`None`: a lost
    /// surface is an error).
    ///
    /// # Safety
    /// `surface` must be a live surface of `ctx.instance` whose window stays alive until
    /// [`Presenter::destroy`]; `ctx` must be a window context. The presenter must be destroyed with
    /// [`Presenter::destroy`] before `ctx` is dropped.
    pub unsafe fn from_surface(
        ctx: &Context,
        surface: vk::SurfaceKHR,
        window: Option<(isize, isize)>,
        size: (u32, u32),
    ) -> Res<Self> {
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
            window,
            requested: (0, 0),
            stale: true,
            surface_lost: false,
            acquires: 0,
            tearing: false,
            present_mode: vk::PresentModeKHR::FIFO,
            can_copy: false,
            retired: Vec::new(),
        };
        let result = (|| -> Res<()> {
            p.check_support(ctx)?;
            // A window created minimised has no extent yet: the swapchain is made by the first frame it has one.
            p.resize(ctx, size).map(|_| ())
        })();
        match result {
            Ok(()) => Ok(p),
            Err(e) => {
                p.destroy(ctx);
                Err(e)
            }
        }
    }

    /// # Safety
    /// `ctx` is the presenter's context and the surface is live.
    unsafe fn check_support(&self, ctx: &Context) -> Res<()> {
        let supported =
            ctx.surface_fn
                .get_physical_device_surface_support(ctx.physical, ctx.queue_family, self.surface)?;
        if !supported {
            return Err(format!("{} cannot present to this window", ctx.gpu.name).into());
        }
        Ok(())
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

    /// True when a swapchain with its views and semaphores exists for every image (the images can be drawn).
    pub fn ready(&self) -> bool {
        self.swapchain != vk::SwapchainKHR::null()
            && !self.images.is_empty()
            && self.views.len() == self.images.len()
            && self.render_done.len() == self.images.len()
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

    /// Asks for a new swapchain before the next acquire (an acquired image was given up, or the caller knows the
    /// swapchain no longer fits).
    pub fn mark_stale(&mut self) {
        self.stale = true;
    }

    /// Present without waiting for the vertical blank (tearing, lower latency, variable refresh) or with vsync (FIFO).
    /// A change recreates the swapchain before the next frame; the mode in force is [`Presenter::present_mode`].
    pub fn set_tearing(&mut self, on: bool) {
        if self.tearing != on {
            self.tearing = on;
            self.stale = true;
        }
    }

    /// Whether the swapchain images can be copied to a buffer (a snapshot of the output).
    pub fn can_copy(&self) -> bool {
        self.can_copy
    }

    /// The present mode the swapchain was made with.
    pub fn present_mode(&self) -> vk::PresentModeKHR {
        self.present_mode
    }

    /// Makes the swapchain fit a window client of `size` before a frame: recreates the lost surface, then the
    /// swapchain when the size changed, when it is stale, or when it is incomplete. Returns false when there is
    /// nothing to draw into (a 0 side: the old swapchain is kept and the frame is skipped).
    ///
    /// # Safety
    /// `ctx` is the presenter's context; no frame of this presenter is recorded and not yet submitted.
    pub unsafe fn ensure(&mut self, ctx: &Context, size: (u32, u32)) -> Res<bool> {
        if size.0 == 0 || size.1 == 0 {
            return Ok(false);
        }
        if self.surface_lost {
            ctx.device.device_wait_idle()?;
            self.recreate_surface(ctx)?;
        }
        if self.stale || self.requested != size || !self.ready() {
            ctx.device.device_wait_idle()?;
            return match self.resize(ctx, size) {
                // Lost while the swapchain was being made: recreated by the next frame.
                Err(e) if e.downcast_ref::<vk::Result>() == Some(&vk::Result::ERROR_SURFACE_LOST_KHR) => {
                    self.surface_lost = true;
                    Ok(false)
                }
                r => r,
            };
        }
        Ok(true)
    }

    /// (Re)creates the swapchain, its image views and per-image semaphores for `size`. Returns false, keeping the
    /// old swapchain (still stale), when the surface has a 0 extent now (minimised). On an error the presenter
    /// stays consistent: no views, no images, stale, so [`Presenter::ensure`] tries again before the next frame.
    ///
    /// # Safety
    /// `ctx` is the presenter's context and the GPU no longer uses the old swapchain images or views
    /// (device idle); the old `render_done` semaphores are retired, not destroyed.
    pub unsafe fn resize(&mut self, ctx: &Context, size: (u32, u32)) -> Res<bool> {
        self.stale = true;
        let caps = ctx
            .surface_fn
            .get_physical_device_surface_capabilities(ctx.physical, self.surface)?;
        let Some(extent) = extent_for(&caps, size) else {
            return Ok(false);
        };
        let formats = ctx
            .surface_fn
            .get_physical_device_surface_formats(ctx.physical, self.surface)?;
        let offered: Vec<(vk::Format, vk::ColorSpaceKHR)> = formats.iter().map(|f| (f.format, f.color_space)).collect();
        let (format, color_space) = choose_format(&offered)?;
        let modes = ctx
            .surface_fn
            .get_physical_device_surface_present_modes(ctx.physical, self.surface)
            .unwrap_or_default();
        let present_mode = choose_present_mode(&modes, self.tearing);
        let can_copy = caps.supported_usage_flags.contains(vk::ImageUsageFlags::TRANSFER_SRC);
        let usage = if can_copy {
            vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC
        } else {
            vk::ImageUsageFlags::COLOR_ATTACHMENT
        };
        let mut count = caps.min_image_count + 1;
        if caps.max_image_count > 0 {
            count = count.min(caps.max_image_count);
        }
        let old = self.swapchain;
        let new = self.swapchain_fn.create_swapchain(
            &vk::SwapchainCreateInfoKHR::default()
                .surface(self.surface)
                .min_image_count(count)
                .image_format(format)
                .image_color_space(color_space)
                .image_extent(extent)
                .image_array_layers(1)
                .image_usage(usage)
                .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
                .pre_transform(caps.current_transform)
                .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
                .present_mode(present_mode)
                .clipped(true)
                .old_swapchain(old),
            None,
        )?;
        // From here the old swapchain is retired (even on an error below): its views go, its semaphores wait.
        self.retire_views(ctx);
        self.images.clear();
        if old != vk::SwapchainKHR::null() {
            self.swapchain_fn.destroy_swapchain(old, None);
        }
        self.swapchain = new;
        self.present_mode = present_mode;
        self.can_copy = can_copy;
        self.format = format;
        self.extent = extent;
        self.ten_bit_offered = formats.iter().any(|f| {
            matches!(
                f.format,
                vk::Format::A2B10G10R10_UNORM_PACK32 | vk::Format::A2R10G10B10_UNORM_PACK32
            ) && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
        });
        let images = self.swapchain_fn.get_swapchain_images(self.swapchain)?;
        for &image in &images {
            let made = (|| -> Res<(vk::ImageView, vk::Semaphore)> {
                let view = ctx.device.create_image_view(
                    &vk::ImageViewCreateInfo::default()
                        .image(image)
                        .view_type(vk::ImageViewType::TYPE_2D)
                        .format(self.format)
                        .subresource_range(color_range()),
                    None,
                )?;
                match ctx.device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) {
                    Ok(s) => Ok((view, s)),
                    Err(e) => {
                        ctx.device.destroy_image_view(view, None);
                        Err(e.into())
                    }
                }
            })();
            match made {
                Ok((view, sem)) => {
                    self.views.push(view);
                    self.render_done.push(sem);
                }
                Err(e) => {
                    // Nothing was submitted with these yet: they can go at once.
                    self.destroy_views_now(ctx);
                    return Err(e);
                }
            }
        }
        self.images = images;
        self.requested = size;
        self.stale = false;
        Ok(true)
    }

    /// Recreates the surface (and drops the swapchain) after `ERROR_SURFACE_LOST_KHR`.
    ///
    /// # Safety
    /// `ctx` is the presenter's context and the device is idle.
    unsafe fn recreate_surface(&mut self, ctx: &Context) -> Res<()> {
        let Some((hwnd, hinstance)) = self.window else {
            return Err(GpuError::SurfaceLost {
                detail: "the presenter does not know its window".into(),
            }
            .into());
        };
        self.retire_views(ctx);
        self.images.clear();
        if self.swapchain != vk::SwapchainKHR::null() {
            self.swapchain_fn.destroy_swapchain(self.swapchain, None);
            self.swapchain = vk::SwapchainKHR::null();
        }
        if self.surface != vk::SurfaceKHR::null() {
            ctx.surface_fn.destroy_surface(self.surface, None);
            self.surface = vk::SurfaceKHR::null();
        }
        let lost = |e: &dyn std::fmt::Display| GpuError::SurfaceLost {
            detail: format!("recreating the surface failed: {e}"),
        };
        self.surface = create_win32_surface(&ctx.entry, &ctx.instance, hwnd, hinstance).map_err(|e| lost(&*e))?;
        self.check_support(ctx).map_err(|e| lost(&*e))?;
        self.surface_lost = false;
        self.stale = true;
        println!("notice: the output surface was lost and has been recreated");
        Ok(())
    }

    /// Acquires the next image, signalling `signal` (one semaphore per frame in flight, owned by the caller). See
    /// [`Acquired`]: out of date, surface lost and a timeout skip the frame (recreated before the next one).
    ///
    /// # Safety
    /// `ctx` is the presenter's context, [`Presenter::ensure`] returned true for this frame, and `signal` is a live
    /// binary semaphore with no pending signal or wait (the submission that last waited on it has completed: its
    /// fence was waited).
    pub unsafe fn acquire(&mut self, ctx: &Context, signal: vk::Semaphore) -> Res<Acquired> {
        let r = self
            .swapchain_fn
            .acquire_next_image(self.swapchain, ACQUIRE_TIMEOUT_NS, signal, vk::Fence::null());
        match r {
            Ok((index, suboptimal)) => {
                self.stale |= suboptimal;
                self.acquires += 1;
                self.destroy_due_retired(ctx);
                Ok(Acquired::Image { index, suboptimal })
            }
            Err(e) => match super::classify(e) {
                super::VkClass::OutOfDate => {
                    self.stale = true;
                    Ok(Acquired::Skip)
                }
                super::VkClass::SurfaceLost => {
                    self.surface_lost = true;
                    Ok(Acquired::Skip)
                }
                super::VkClass::Timeout => Ok(Acquired::Skip),
                _ => Err(e.into()),
            },
        }
    }

    /// Presents image `index` on `ctx.queue` after [`Presenter::render_done`]`(index)`. Out of date, suboptimal and
    /// a lost surface are not errors: the swapchain (or surface) is recreated before the next frame. The wait on the
    /// semaphore happens in every one of those cases (Vulkan: the present is still enqueued).
    ///
    /// # Safety
    /// `ctx` is the presenter's context and `index` was acquired by [`Presenter::acquire`] and
    /// submitted with a signal of `render_done(index)`.
    pub unsafe fn present(&mut self, ctx: &Context, index: u32) -> Res<()> {
        let wait = [self.render_done(index)];
        let swapchains = [self.swapchain];
        let indices = [index];
        let r = self.swapchain_fn.queue_present(
            ctx.queue,
            &vk::PresentInfoKHR::default()
                .wait_semaphores(&wait)
                .swapchains(&swapchains)
                .image_indices(&indices),
        );
        match r {
            Ok(suboptimal) => {
                self.stale |= suboptimal;
                Ok(())
            }
            Err(e) => match super::classify(e) {
                super::VkClass::OutOfDate | super::VkClass::Suboptimal => {
                    self.stale = true;
                    Ok(())
                }
                super::VkClass::SurfaceLost => {
                    self.surface_lost = true;
                    Ok(())
                }
                _ => Err(e.into()),
            },
        }
    }

    /// Destroys the swapchain, its views and semaphores (retired ones too), then the surface. Idempotent.
    ///
    /// # Safety
    /// `ctx` is the presenter's context and the GPU has finished every use of them (device idle).
    pub unsafe fn destroy(&mut self, ctx: &Context) {
        self.destroy_views_now(ctx);
        for (s, _) in self.retired.drain(..) {
            ctx.device.destroy_semaphore(s, None);
        }
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

    /// Destroys the views and moves the semaphores to the retired list: they may be destroyed once every image of
    /// the next swapchain was acquired once more (its presents, and so the old ones before them, are done).
    ///
    /// # Safety
    /// The GPU has finished every use of the views (device idle).
    unsafe fn retire_views(&mut self, ctx: &Context) {
        for v in self.views.drain(..) {
            ctx.device.destroy_image_view(v, None);
        }
        // The next swapchain has about as many images as this one (at least 2): wait for that many acquires, plus one.
        let due = self.acquires + self.render_done.len().max(2) as u64 + 1;
        for s in self.render_done.drain(..) {
            self.retired.push((s, due));
        }
        while self.retired.len() > RETIRED_MAX {
            let (s, _) = self.retired.remove(0);
            ctx.device.destroy_semaphore(s, None);
        }
    }

    /// Destroys the retired semaphores whose time came ([`Presenter::retire_views`]).
    ///
    /// # Safety
    /// `ctx` is the presenter's context; called after a successful acquire.
    unsafe fn destroy_due_retired(&mut self, ctx: &Context) {
        let now = self.acquires;
        let mut i = 0;
        while i < self.retired.len() {
            if self.retired[i].1 <= now {
                let (s, _) = self.retired.swap_remove(i);
                // Every image of the swapchain that replaced the semaphore's was acquired again since, so the
                // presents queued before the replacement (the only ones that waited on it) completed.
                ctx.device.destroy_semaphore(s, None);
            } else {
                i += 1;
            }
        }
    }

    /// Destroys the views and the semaphores at once (nothing was ever submitted with them, or the device is idle
    /// and no present is pending).
    ///
    /// # Safety
    /// The GPU has finished every use of them.
    unsafe fn destroy_views_now(&mut self, ctx: &Context) {
        for v in self.views.drain(..) {
            ctx.device.destroy_image_view(v, None);
        }
        for s in self.render_done.drain(..) {
            ctx.device.destroy_semaphore(s, None);
        }
    }

    /// Semaphores retired and not yet destroyed (tests and diagnostics).
    pub fn retired_semaphores(&self) -> usize {
        self.retired.len()
    }
}

/// Picks the swapchain format among the `(format, colour space)` pairs the surface offers: a UNORM
/// format with `SRGB_NONLINEAR`, preferring B8G8R8A8, then R8G8B8A8 (both are dithered at 8 bits by
/// `encode`; the 10-bit formats are not chosen until the swapchain handles them, see
/// [`Presenter::ten_bit_offered`]). `encode` applies the host transfer itself, so an `*_SRGB` format
/// would encode twice and switch the dither off: it is an error, never a fallback.
fn choose_format(offered: &[(vk::Format, vk::ColorSpaceKHR)]) -> Res<(vk::Format, vk::ColorSpaceKHR)> {
    for want in [vk::Format::B8G8R8A8_UNORM, vk::Format::R8G8B8A8_UNORM] {
        if let Some(&pair) = offered
            .iter()
            .find(|&&(f, cs)| f == want && cs == vk::ColorSpaceKHR::SRGB_NONLINEAR)
        {
            return Ok(pair);
        }
    }
    Err(format!("the surface offers no B8G8R8A8_UNORM or R8G8B8A8_UNORM format with SRGB_NONLINEAR (offered: {offered:?}); an _SRGB format would encode the output twice").into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NL: vk::ColorSpaceKHR = vk::ColorSpaceKHR::SRGB_NONLINEAR;

    #[test]
    fn tearing_is_used_only_when_asked_and_offered() {
        use vk::PresentModeKHR as M;
        let all = [M::FIFO, M::MAILBOX, M::IMMEDIATE];
        assert_eq!(choose_present_mode(&all, false), M::FIFO, "vsync is the default");
        assert_eq!(choose_present_mode(&all, true), M::IMMEDIATE);
        assert_eq!(
            choose_present_mode(&[M::FIFO, M::MAILBOX], true),
            M::FIFO,
            "not offered: vsync"
        );
        assert_eq!(
            choose_present_mode(&[], true),
            M::FIFO,
            "no list (a failed query): vsync"
        );
    }

    #[test]
    fn prefers_bgra_unorm_then_rgba_unorm() {
        let all = [
            (vk::Format::B8G8R8A8_SRGB, NL),
            (vk::Format::R8G8B8A8_UNORM, NL),
            (vk::Format::B8G8R8A8_UNORM, NL),
        ];
        assert_eq!(choose_format(&all).unwrap().0, vk::Format::B8G8R8A8_UNORM);
        assert_eq!(choose_format(&all[..2]).unwrap().0, vk::Format::R8G8B8A8_UNORM);
    }

    fn caps(current: (u32, u32), min: (u32, u32), max: (u32, u32)) -> vk::SurfaceCapabilitiesKHR {
        let e = |(width, height)| vk::Extent2D { width, height };
        vk::SurfaceCapabilitiesKHR {
            current_extent: e(current),
            min_image_extent: e(min),
            max_image_extent: e(max),
            ..Default::default()
        }
    }

    /// R-36: a 0 side (minimised, or the minimise race between the client size and the surface query) gives no
    /// swapchain extent, so the old swapchain is kept and the frame skipped.
    #[test]
    fn extent_for_refuses_a_zero_side() {
        let fixed = caps((1920, 1080), (1, 1), (16384, 16384));
        assert_eq!(
            extent_for(&fixed, (1920, 1080)),
            Some(vk::Extent2D {
                width: 1920,
                height: 1080
            })
        );
        // The surface's extent wins over the client size (they differ for a frame during a resize).
        assert_eq!(extent_for(&fixed, (800, 600)).map(|e| e.width), Some(1920));
        // Minimised: Windows reports a 0 x 0 current extent.
        assert_eq!(extent_for(&caps((0, 0), (0, 0), (16384, 16384)), (1920, 1080)), None);
        assert_eq!(extent_for(&caps((1920, 0), (0, 0), (16384, 16384)), (1920, 1080)), None);
        // A 0 client with a stale extent: nothing to draw either.
        assert_eq!(extent_for(&fixed, (0, 600)), None);
        // No current extent (u32::MAX): the client size, clamped to the surface's limits.
        let free = caps((u32::MAX, u32::MAX), (64, 64), (4096, 4096));
        assert_eq!(
            extent_for(&free, (8000, 10)),
            Some(vk::Extent2D {
                width: 4096,
                height: 64
            })
        );
        assert_eq!(extent_for(&free, (0, 10)), None);
        // Inverted limits from a broken driver do not panic.
        let odd = caps((u32::MAX, u32::MAX), (100, 100), (50, 50));
        assert_eq!(extent_for(&odd, (10, 10)).map(|e| e.width), Some(100));
    }

    #[test]
    fn rejects_srgb_only_and_wrong_colour_space() {
        assert!(choose_format(&[(vk::Format::B8G8R8A8_SRGB, NL), (vk::Format::R8G8B8A8_SRGB, NL)]).is_err());
        assert!(choose_format(&[(vk::Format::B8G8R8A8_UNORM, vk::ColorSpaceKHR::EXTENDED_SRGB_LINEAR_EXT)]).is_err());
        assert!(choose_format(&[]).is_err());
    }
}

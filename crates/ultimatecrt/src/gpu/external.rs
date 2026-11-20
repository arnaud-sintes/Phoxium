//! Imported shared textures of the zero-copy capture path (A29): the Vulkan images of one capture pool
//! (`capture::SharedTextures`), imported once per pool with `VK_KHR_external_memory_win32` and sampled by `decode`
//! in place of the uploaded `src` image.
//!
//! Each image is created with `VkExternalMemoryImageCreateInfo` (handle type `D3D11_TEXTURE` for an NT handle,
//! `D3D11_TEXTURE_KMT` for a KMT handle), `B8G8R8A8_UNORM` like the CPU path's `src` (same texels, same
//! `texelFetch`, so `decode` writes the same values), optimal tiling, usage `SAMPLED | TRANSFER_SRC`, initial layout
//! `UNDEFINED`, bound to a dedicated allocation that imports the handle. The memory type is one the handle allows
//! (`vkGetMemoryWin32HandlePropertiesKHR`) among those the image allows.
//!
//! Ownership per frame (pipeline.rs): the image belongs to `VK_QUEUE_FAMILY_EXTERNAL` between frames. A frame that
//! reads it acquires it (external to the graphics family, layout to `GENERAL`, from `UNDEFINED` on its first use),
//! `decode` samples it in `GENERAL`, and the frame releases it back to the external family (layout kept). The
//! Direct3D 11 copy that filled the texture finished on the GPU before the slot was published (the capture's event
//! query), so no semaphore is needed.

use std::sync::Arc;

use ash::vk;

use super::context::Context;
use crate::capture::SharedTextures;
use crate::zerocopy::HandleKind;
use crate::Res;

/// The Vulkan handle type of a [`HandleKind`].
pub(super) fn handle_type(kind: HandleKind) -> vk::ExternalMemoryHandleTypeFlags {
    match kind {
        HandleKind::Nt => vk::ExternalMemoryHandleTypeFlags::D3D11_TEXTURE,
        HandleKind::Kmt => vk::ExternalMemoryHandleTypeFlags::D3D11_TEXTURE_KMT,
    }
}

/// One imported texture.
#[derive(Default)]
pub(super) struct Imported {
    pub image: vk::Image,
    pub memory: vk::DeviceMemory,
    pub view: vk::ImageView,
    /// A submitted frame has transitioned it out of `UNDEFINED` (its later acquires keep the contents).
    pub initialized: bool,
}

impl Imported {
    /// # Safety
    /// The GPU no longer uses the objects.
    unsafe fn destroy(&mut self, d: &ash::Device) {
        d.destroy_image_view(self.view, None);
        d.destroy_image(self.image, None);
        d.free_memory(self.memory, None);
        *self = Self::default();
    }
}

/// The imported images of one capture pool.
pub(super) struct ExternalSet {
    pub generation: u64,
    pub images: Vec<Imported>,
    /// Keeps the Direct3D 11 textures (and their handles) alive as long as the imports.
    _pool: Arc<SharedTextures>,
}

impl ExternalSet {
    /// The identity of the imported pool (its address, kept alive by this set so it cannot be reused meanwhile).
    pub fn pool_id(&self) -> usize {
        Arc::as_ptr(&self._pool) as usize
    }

    /// Imports every texture of `pool` on `ctx`. All or nothing: on an error every import made is destroyed.
    ///
    /// # Safety
    /// `ctx` is live and was created with the external memory extension ([`Context::external_memory`]).
    pub unsafe fn import(ctx: &Context, pool: &Arc<SharedTextures>) -> Res<Self> {
        let mut set = Self {
            generation: pool.generation(),
            images: Vec::new(),
            _pool: pool.clone(),
        };
        for h in pool.handles() {
            match import_one(ctx, h, pool.kind(), pool.size()) {
                Ok(i) => set.images.push(i),
                Err(e) => {
                    set.destroy(&ctx.device);
                    return Err(e);
                }
            }
        }
        Ok(set)
    }

    /// # Safety
    /// The GPU no longer uses the images (device idle).
    pub unsafe fn destroy(&mut self, d: &ash::Device) {
        for i in &mut self.images {
            i.destroy(d);
        }
        self.images.clear();
    }
}

/// Imports one shared texture as a sampled image.
///
/// # Safety
/// As [`ExternalSet::import`].
unsafe fn import_one(ctx: &Context, handle: isize, kind: HandleKind, size: (u32, u32)) -> Res<Imported> {
    let d = &ctx.device;
    let win32 = ctx
        .external_fn
        .as_ref()
        .ok_or("the Vulkan device was created without VK_KHR_external_memory_win32")?;
    let ty = handle_type(kind);
    let mut out = Imported::default();
    let r = (|| -> Res<()> {
        let mut external = vk::ExternalMemoryImageCreateInfo::default().handle_types(ty);
        out.image = d.create_image(
            &vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(vk::Format::B8G8R8A8_UNORM)
                .extent(vk::Extent3D {
                    width: size.0,
                    height: size.1,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_SRC)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED)
                .push_next(&mut external),
            None,
        )?;
        let mut dedicated_req = vk::MemoryDedicatedRequirements::default();
        let mut req = vk::MemoryRequirements2::default().push_next(&mut dedicated_req);
        d.get_image_memory_requirements2(&vk::ImageMemoryRequirementsInfo2::default().image(out.image), &mut req);
        let req = req.memory_requirements;
        let mut props = vk::MemoryWin32HandlePropertiesKHR::default();
        win32
            .get_memory_win32_handle_properties(ty, handle, &mut props)
            .map_err(|e| format!("the shared texture handle was refused ({e})"))?;
        let bits = match req.memory_type_bits & props.memory_type_bits {
            0 => req.memory_type_bits,
            b => b,
        };
        let type_index = ctx
            .memory_type(bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)
            .or_else(|_| ctx.memory_type(bits, vk::MemoryPropertyFlags::empty()))?;
        let mut import = vk::ImportMemoryWin32HandleInfoKHR::default()
            .handle_type(ty)
            .handle(handle);
        // Always dedicated: Direct3D 11 textures are dedicated-only on the drivers seen, and it is valid anyway.
        let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(out.image);
        out.memory = d
            .allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(req.size)
                    .memory_type_index(type_index)
                    .push_next(&mut import)
                    .push_next(&mut dedicated),
                None,
            )
            .map_err(|e| format!("importing the shared texture failed ({e})"))?;
        d.bind_image_memory(out.image, out.memory, 0)?;
        out.view = d.create_image_view(
            &vk::ImageViewCreateInfo::default()
                .image(out.image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(vk::Format::B8G8R8A8_UNORM)
                .subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1)
                        .layer_count(1),
                ),
            None,
        )?;
        Ok(())
    })();
    match r {
        Ok(()) => Ok(out),
        Err(e) => {
            out.destroy(d);
            Err(e)
        }
    }
}

/// Records the acquire of an imported image for `decode` (external family to `family`, into `GENERAL`; from
/// `UNDEFINED` on its first use) or, with `release`, its release back to the external family after `decode`.
///
/// # Safety
/// `cb` is recording on `d`; `image` is a live imported image.
pub(super) unsafe fn ownership_barrier(
    d: &ash::Device,
    cb: vk::CommandBuffer,
    image: vk::Image,
    family: u32,
    initialized: bool,
    release: bool,
) {
    let range = vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .level_count(1)
        .layer_count(1);
    let b = if release {
        vk::ImageMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
            .src_access_mask(vk::AccessFlags2::NONE)
            .dst_stage_mask(vk::PipelineStageFlags2::NONE)
            .dst_access_mask(vk::AccessFlags2::NONE)
            .old_layout(vk::ImageLayout::GENERAL)
            .new_layout(vk::ImageLayout::GENERAL)
            .src_queue_family_index(family)
            .dst_queue_family_index(vk::QUEUE_FAMILY_EXTERNAL)
    } else {
        vk::ImageMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::NONE)
            .src_access_mask(vk::AccessFlags2::NONE)
            .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
            .dst_access_mask(vk::AccessFlags2::SHADER_SAMPLED_READ)
            .old_layout(if initialized {
                vk::ImageLayout::GENERAL
            } else {
                vk::ImageLayout::UNDEFINED
            })
            .new_layout(vk::ImageLayout::GENERAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_EXTERNAL)
            .dst_queue_family_index(family)
    };
    let b = [b.image(image).subresource_range(range)];
    d.cmd_pipeline_barrier2(cb, &vk::DependencyInfo::default().image_memory_barriers(&b));
}

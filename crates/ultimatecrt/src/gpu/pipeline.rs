//! Pipeline core (ARCHITECTURE 3.7, A10, A11, TEC-6): the fixed ordered chain of stages, the images
//! between them, the per-frame uniform buffers and the frames in flight.
//!
//! Chain today: upload (captured BGRA8 -> `src` image) -> `decode` (compute, `src` -> `native` RGBA16F)
//! -> `encode` (fragment, `native` -> output target). T-20b inserts the remaining stage slots between
//! `decode` and `encode` (see `stages/mod.rs` for the stage contract).
//!
//! Frames in flight: [`FRAMES_IN_FLIGHT`] slots, each with its own command buffer, fence, staging upload
//! buffer, `FrameConstants` and `Params` uniform buffers and descriptor sets. The CPU writes a slot only
//! after waiting for its fence ([`Pipeline::wait_slot`]). The images (`src`, `native`) are shared by the
//! slots: every frame records barriers against the previous frame's use of them, and since all frames run
//! on one queue in submission order those barriers order the frames too.
//!
//! Images are (re)allocated when their size changes: `src` with the captured frame size, `native` with
//! the native raster of the [`View`]. Reallocation idles the device first. A reallocated image starts
//! `UNDEFINED`, so any history it held is cleared (SRC-8: history cleared on mode change; the history
//! images of later stages follow the same rule).
//!
//! Per-frame use: [`Pipeline::wait_slot`], optionally [`Pipeline::stage_source`] with a new frame,
//! [`Pipeline::record`] (returns the slot's command buffer, still recording, with the target in
//! `COLOR_ATTACHMENT_OPTIMAL`), the caller's final commands on the target, then [`Pipeline::submit`].

use std::time::Instant;

use ash::vk;

use super::barrier;
use super::context::Context;
use super::headless::{GpuImage, HostBuffer};
use super::stages::{self, decode, decode::Decode, encode, encode::Encode};
use crate::params::{Params, PARAMS_STD140_SIZE};
use crate::scaling::View;
use crate::Res;

/// Frames the CPU may record ahead of the GPU.
pub const FRAMES_IN_FLIGHT: usize = 2;

/// Format of the captured frame image (as delivered by the capture, A3).
const SOURCE_FORMAT: vk::Format = vk::Format::B8G8R8A8_UNORM;

/// Format of the native raster and of every intermediate image (A10: FP16, linear light after the gun).
pub const NATIVE_FORMAT: vk::Format = vk::Format::R16G16B16A16_SFLOAT;

/// A transfer function between code values and linear light (`shaders/include/common.glsl`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Transfer {
    /// Value unchanged.
    Identity,
    /// Pure power law, L = V^gamma.
    Power(f32),
    /// Piecewise sRGB (IEC 61966-2-1) [K].
    Srgb,
}

impl Transfer {
    /// The `TRANSFER_*` constant and the exponent passed to the shaders.
    pub(super) fn shader_pair(self) -> (u32, f32) {
        match self {
            Transfer::Identity => (0, 1.0),
            Transfer::Power(g) => (1, g),
            Transfer::Srgb => (2, 2.4),
        }
    }
}

/// Pipeline settings that are not yet entries of the parameter table.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// Transfer applied by `decode` to the code value V = code / 255. Until the gun stage exists (T-20b,
    /// T-34) it stands in for the gun law, so that the chain ends in linear light. With
    /// [`Transfer::Identity`] the native raster holds code values (drive voltages), the input contract
    /// of the voltage-domain stages.
    pub source: Transfer,
    /// Host display EOTF; `encode` applies its inverse (COL-3).
    pub host: Transfer,
    /// Scale the tube rectangle by whole factors only (when at least 1).
    pub integer_scale: bool,
}

impl Default for Settings {
    /// Tube gamma 2.4 [E] (SRC-5 default: captured values are drive voltages) and host pure 2.2 [E]
    /// (COL-3 default).
    fn default() -> Self {
        Self {
            source: Transfer::Power(2.4),
            host: Transfer::Power(2.2),
            integer_scale: false,
        }
    }
}

impl Settings {
    /// Identity transfers on both sides: the output codes equal the captured codes (passthrough).
    pub fn identity() -> Self {
        Self {
            source: Transfer::Identity,
            host: Transfer::Identity,
            integer_scale: false,
        }
    }
}

/// Per-frame constants, std140, laid out as the `FrameConstants` block of `shaders/include/common.glsl`
/// (48 bytes, 4-byte fields only, no padding; a test checks the offsets against the GLSL).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameConstants {
    /// Tube rectangle in target pixels: x0, y0, x1, y1 (exclusive).
    pub tube_rect_px: [f32; 4],
    /// Native raster size.
    pub native_size: [i32; 2],
    /// Output target size.
    pub target_size: [i32; 2],
    /// Host pixels per tube millimetre; 0 until the tube is placed (T-30).
    pub px_per_mm: f32,
    /// Scanline pitch in tube millimetres; 0 until the tube is placed (T-30).
    pub line_pitch_mm: f32,
    /// Frames submitted since the pipeline was created.
    pub frame_index: u32,
    /// Time since the previous frame in ms (0 on the first frame).
    pub frame_time_ms: f32,
}

/// Tube rectangle (x0, y0, x1, y1 in target pixels) of a `native` raster letterboxed into `target`,
/// centred, scaled uniformly, by whole factors only with `integer` (when the scale is at least 1).
/// Until the tube is placed by T-30, the tube is the letterboxed native raster.
pub fn tube_rect_letterbox(native: (u32, u32), target: (u32, u32), integer: bool) -> [f32; 4] {
    let (nw, nh) = (native.0.max(1) as f32, native.1.max(1) as f32);
    let (tw, th) = (target.0 as f32, target.1 as f32);
    let mut scale = (tw / nw).min(th / nh);
    if integer && scale >= 1.0 {
        scale = scale.floor();
    }
    let (w, h) = (nw * scale, nh * scale);
    let (x0, y0) = (((tw - w) / 2.0).floor(), ((th - h) / 2.0).floor());
    [x0, y0, x0 + w, y0 + h]
}

/// The colour target of one frame (a swapchain image or a headless image).
pub(super) struct Target {
    pub image: vk::Image,
    pub view: vk::ImageView,
    pub extent: vk::Extent2D,
    pub format: vk::Format,
}

/// Resources of one frame in flight. Null handles by default.
#[derive(Default)]
struct Slot {
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    /// Captured frame waiting for upload, sized for the `src` image.
    staging: HostBuffer,
    /// The staging buffer holds a frame not yet copied to `src`.
    upload: bool,
    frame_ubo: HostBuffer,
    params_ubo: HostBuffer,
    decode_set: vk::DescriptorSet,
    encode_set: vk::DescriptorSet,
}

/// The stage chain, its images and its frames in flight. It does not own the device: its owner calls
/// [`Pipeline::destroy`] before dropping the [`Context`].
pub struct Pipeline {
    pool: vk::CommandPool,
    desc_pool: vk::DescriptorPool,
    sampler: vk::Sampler,
    decode: Decode,
    encode: Encode,
    slots: Vec<Slot>,
    current: usize,
    /// Captured frame image, `SOURCE_FORMAT`, `src_size`.
    src: GpuImage,
    src_size: (u32, u32),
    /// `src` holds an uploaded frame (or one is recorded for upload).
    src_valid: bool,
    /// Native raster, `NATIVE_FORMAT`, `native_size`.
    native: GpuImage,
    native_size: (u32, u32),
    frame_index: u32,
    last_frame: Option<Instant>,
}

impl Pipeline {
    /// Creates the stage objects and the frames in flight. Images are created on first use.
    ///
    /// # Safety
    /// `ctx` must be live; the caller destroys the result with [`Pipeline::destroy`] before `ctx`.
    pub(super) unsafe fn new(ctx: &Context) -> Res<Self> {
        let mut p = Self {
            pool: vk::CommandPool::null(),
            desc_pool: vk::DescriptorPool::null(),
            sampler: vk::Sampler::null(),
            decode: Decode::default(),
            encode: Encode::default(),
            slots: Vec::new(),
            current: 0,
            src: GpuImage::default(),
            src_size: (0, 0),
            src_valid: false,
            native: GpuImage::default(),
            native_size: (0, 0),
            frame_index: 0,
            last_frame: None,
        };
        match p.create(ctx) {
            Ok(()) => Ok(p),
            Err(e) => {
                p.destroy(ctx);
                Err(e)
            }
        }
    }

    /// # Safety
    /// As [`Pipeline::new`]; `self` is empty.
    unsafe fn create(&mut self, ctx: &Context) -> Res<()> {
        let d = &ctx.device;
        self.decode.create(d)?;
        self.encode.create(d)?;
        self.sampler = d.create_sampler(
            &vk::SamplerCreateInfo::default()
                .mag_filter(vk::Filter::NEAREST)
                .min_filter(vk::Filter::NEAREST)
                .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE),
            None,
        )?;
        self.pool = d.create_command_pool(
            &vk::CommandPoolCreateInfo::default()
                .queue_family_index(ctx.queue_family)
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
            None,
        )?;
        let n = FRAMES_IN_FLIGHT as u32;
        let sizes = [
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::UNIFORM_BUFFER)
                .descriptor_count(4 * n),
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(2 * n),
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::STORAGE_IMAGE)
                .descriptor_count(n),
        ];
        self.desc_pool = d.create_descriptor_pool(
            &vk::DescriptorPoolCreateInfo::default()
                .max_sets(2 * n)
                .pool_sizes(&sizes),
            None,
        )?;
        let cmds = d.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::default()
                .command_pool(self.pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(n),
        )?;
        for cmd in cmds {
            let mut s = Slot { cmd, ..Slot::default() };
            // Pushed first so that `destroy` releases whatever is created below on an error.
            self.slots.push(Slot::default());
            let r = (|| -> Res<()> {
                s.fence = d.create_fence(
                    &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                    None,
                )?;
                s.frame_ubo =
                    HostBuffer::create(ctx, size_of::<FrameConstants>(), vk::BufferUsageFlags::UNIFORM_BUFFER)?;
                s.params_ubo = HostBuffer::create(ctx, PARAMS_STD140_SIZE, vk::BufferUsageFlags::UNIFORM_BUFFER)?;
                let layouts = [self.decode.set_layout, self.encode.set_layout];
                let sets = d.allocate_descriptor_sets(
                    &vk::DescriptorSetAllocateInfo::default()
                        .descriptor_pool(self.desc_pool)
                        .set_layouts(&layouts),
                )?;
                (s.decode_set, s.encode_set) = (sets[0], sets[1]);
                Ok(())
            })();
            *self.slots.last_mut().unwrap() = s;
            r?;
        }
        // The uniform buffers never change: write their descriptors once.
        for s in &self.slots {
            let params = [vk::DescriptorBufferInfo::default()
                .buffer(s.params_ubo.buffer)
                .range(PARAMS_STD140_SIZE as u64)];
            let frame = [vk::DescriptorBufferInfo::default()
                .buffer(s.frame_ubo.buffer)
                .range(size_of::<FrameConstants>() as u64)];
            let mut writes = Vec::new();
            for set in [s.decode_set, s.encode_set] {
                writes.push(
                    vk::WriteDescriptorSet::default()
                        .dst_set(set)
                        .dst_binding(stages::BINDING_PARAMS)
                        .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                        .buffer_info(&params),
                );
                writes.push(
                    vk::WriteDescriptorSet::default()
                        .dst_set(set)
                        .dst_binding(stages::BINDING_FRAME)
                        .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                        .buffer_info(&frame),
                );
            }
            d.update_descriptor_sets(&writes, &[]);
        }
        Ok(())
    }

    /// Waits until the current frame slot is free (its previous submission finished). The CPU may then
    /// write the slot's buffers.
    ///
    /// # Safety
    /// `ctx` is the pipeline's context.
    pub(super) unsafe fn wait_slot(&mut self, ctx: &Context) -> Res<()> {
        let fence = self.slots[self.current].fence;
        ctx.device.wait_for_fences(&[fence], true, u64::MAX)?;
        Ok(())
    }

    /// Copies a captured BGRA8 frame of `width` x `height` into the current slot's staging buffer; the
    /// next [`Pipeline::record`] uploads it. Reallocates `src` and the staging buffers (device idled
    /// first) when the size changed.
    ///
    /// # Safety
    /// `ctx` is the pipeline's context and [`Pipeline::wait_slot`] was called for the current slot.
    pub(super) unsafe fn stage_source(&mut self, ctx: &Context, width: u32, height: u32, bgra: &[u8]) -> Res<()> {
        if width == 0 || height == 0 {
            return Err("pipeline: empty source frame".into());
        }
        let bytes = width as usize * height as usize * 4;
        if bgra.len() < bytes {
            return Err(format!(
                "pipeline: {} bytes given, {width}x{height} BGRA8 needs {bytes}",
                bgra.len()
            )
            .into());
        }
        if (width, height) != self.src_size {
            self.reallocate_src(ctx, width, height)?;
        }
        let s = &mut self.slots[self.current];
        // Host writes before vkQueueSubmit are visible to the device (implicit host write ordering).
        std::ptr::copy_nonoverlapping(bgra.as_ptr(), s.staging.mapped, bytes);
        s.upload = true;
        Ok(())
    }

    /// # Safety
    /// `ctx` is the pipeline's context.
    unsafe fn reallocate_src(&mut self, ctx: &Context, width: u32, height: u32) -> Res<()> {
        let d = &ctx.device;
        d.device_wait_idle()?;
        self.src.destroy(d);
        self.src_size = (0, 0);
        self.src_valid = false;
        for s in &mut self.slots {
            s.staging.destroy(d);
            s.upload = false;
        }
        let extent = vk::Extent2D { width, height };
        self.src = GpuImage::create(
            ctx,
            extent,
            SOURCE_FORMAT,
            vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED,
        )?;
        for s in &mut self.slots {
            s.staging = HostBuffer::create(
                ctx,
                width as usize * height as usize * 4,
                vk::BufferUsageFlags::TRANSFER_SRC,
            )?;
        }
        self.src_size = (width, height);
        let info = [vk::DescriptorImageInfo::default()
            .sampler(self.sampler)
            .image_view(self.src.view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        for s in &self.slots {
            d.update_descriptor_sets(
                &[vk::WriteDescriptorSet::default()
                    .dst_set(s.decode_set)
                    .dst_binding(decode::BINDING_SRC)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(&info)],
                &[],
            );
        }
        Ok(())
    }

    /// # Safety
    /// `ctx` is the pipeline's context.
    unsafe fn reallocate_native(&mut self, ctx: &Context, native: (u32, u32)) -> Res<()> {
        let d = &ctx.device;
        d.device_wait_idle()?;
        self.native.destroy(d);
        self.native_size = (0, 0);
        self.native = GpuImage::create(
            ctx,
            vk::Extent2D {
                width: native.0,
                height: native.1,
            },
            NATIVE_FORMAT,
            vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_SRC,
        )?;
        self.native_size = native;
        let storage = [vk::DescriptorImageInfo::default()
            .image_view(self.native.view)
            .image_layout(vk::ImageLayout::GENERAL)];
        let sampled = [vk::DescriptorImageInfo::default()
            .sampler(self.sampler)
            .image_view(self.native.view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        for s in &self.slots {
            d.update_descriptor_sets(
                &[
                    vk::WriteDescriptorSet::default()
                        .dst_set(s.decode_set)
                        .dst_binding(decode::BINDING_NATIVE)
                        .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                        .image_info(&storage),
                    vk::WriteDescriptorSet::default()
                        .dst_set(s.encode_set)
                        .dst_binding(encode::BINDING_INPUT)
                        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                        .image_info(&sampled),
                ],
                &[],
            );
        }
        Ok(())
    }

    /// The native raster image and its size, once a frame has been decoded into it (tests read it back).
    pub(super) fn native_image(&self) -> Option<(vk::Image, (u32, u32))> {
        (self.native.image != vk::Image::null()).then_some((self.native.image, self.native_size))
    }

    /// Records the frame into the current slot's command buffer and returns it, still recording: upload
    /// of a staged frame, `decode`, then `encode` into `target`, which ends in `COLOR_ATTACHMENT_OPTIMAL`
    /// (whole target written: tube rectangle plus black surround). Without any source frame yet, the
    /// target is only cleared to black. Writes the slot's `FrameConstants` and `Params` buffers.
    ///
    /// # Safety
    /// `ctx` is the pipeline's context; [`Pipeline::wait_slot`] was called for the current slot;
    /// `target` is a live image and view of `ctx` usable as a colour attachment, whose previous contents
    /// may be discarded and whose previous use is ordered before this submission (acquire semaphore
    /// waited at `COLOR_ATTACHMENT_OUTPUT`, or a fresh image).
    pub(super) unsafe fn record(
        &mut self,
        ctx: &Context,
        view: View,
        settings: &Settings,
        params: &Params,
        target: &Target,
    ) -> Res<vk::CommandBuffer> {
        let staged = self.slots[self.current].upload;
        let have_source = (self.src_valid || staged) && view.native.0 > 0 && view.native.1 > 0;
        if have_source && view.native != self.native_size {
            self.reallocate_native(ctx, view.native)?;
        }
        let pipeline = self.encode.pipeline(&ctx.device, target.format)?;

        let now = Instant::now();
        let frame_time_ms = self
            .last_frame
            .map_or(0.0, |t| now.duration_since(t).as_secs_f32() * 1000.0);
        self.last_frame = Some(now);
        let fc = FrameConstants {
            tube_rect_px: tube_rect_letterbox(
                view.native,
                (target.extent.width, target.extent.height),
                settings.integer_scale,
            ),
            native_size: [view.native.0 as i32, view.native.1 as i32],
            target_size: [target.extent.width as i32, target.extent.height as i32],
            px_per_mm: 0.0,
            line_pitch_mm: 0.0,
            frame_index: self.frame_index,
            frame_time_ms,
        };
        let s = &mut self.slots[self.current];
        // SAFETY: FrameConstants is #[repr(C)] with 4-byte fields only (no padding); both buffers are
        // mapped, host coherent, at least as large as what is copied, and not in use (fence waited).
        std::ptr::copy_nonoverlapping(
            (&fc as *const FrameConstants).cast::<u8>(),
            s.frame_ubo.mapped,
            size_of::<FrameConstants>(),
        );
        std::ptr::copy_nonoverlapping(params.to_std140().as_ptr(), s.params_ubo.mapped, PARAMS_STD140_SIZE);

        let d = &ctx.device;
        let cb = s.cmd;
        d.reset_command_buffer(cb, vk::CommandBufferResetFlags::empty())?;
        d.begin_command_buffer(
            cb,
            &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        )?;

        if staged {
            // The whole image is overwritten: discard (UNDEFINED) after the previous frames' decode reads.
            barrier(
                d,
                cb,
                self.src.image,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::PipelineStageFlags2::COMPUTE_SHADER,
                vk::AccessFlags2::NONE,
                vk::PipelineStageFlags2::COPY,
                vk::AccessFlags2::TRANSFER_WRITE,
            );
            d.cmd_copy_buffer_to_image(
                cb,
                s.staging.buffer,
                self.src.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[full_copy(self.src_size)],
            );
            barrier(
                d,
                cb,
                self.src.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::PipelineStageFlags2::COPY,
                vk::AccessFlags2::TRANSFER_WRITE,
                vk::PipelineStageFlags2::COMPUTE_SHADER,
                vk::AccessFlags2::SHADER_SAMPLED_READ,
            );
            s.upload = false;
            self.src_valid = true;
        }

        if have_source {
            // The decode dispatch overwrites every texel: discard after the previous frames' reads
            // (encode, or a test readback copy).
            barrier(
                d,
                cb,
                self.native.image,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::GENERAL,
                vk::PipelineStageFlags2::FRAGMENT_SHADER | vk::PipelineStageFlags2::COPY,
                vk::AccessFlags2::NONE,
                vk::PipelineStageFlags2::COMPUTE_SHADER,
                vk::AccessFlags2::SHADER_STORAGE_WRITE,
            );
            self.decode.record(d, cb, s.decode_set, view, settings.source);
            barrier(
                d,
                cb,
                self.native.image,
                vk::ImageLayout::GENERAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::PipelineStageFlags2::COMPUTE_SHADER,
                vk::AccessFlags2::SHADER_STORAGE_WRITE,
                vk::PipelineStageFlags2::FRAGMENT_SHADER,
                vk::AccessFlags2::SHADER_SAMPLED_READ,
            );
        }

        barrier(
            d,
            cb,
            target.image,
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags2::NONE,
            vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
        );
        self.encode.record(
            d,
            cb,
            pipeline,
            have_source.then_some(s.encode_set),
            target.view,
            target.extent,
            settings.host,
        );
        Ok(cb)
    }

    /// Ends the current slot's command buffer, submits it (waiting on `wait` at the given stages,
    /// signalling `signal`) with the slot's fence, and moves to the next slot. Returns that fence.
    ///
    /// # Safety
    /// `ctx` is the pipeline's context and the command buffer was returned by [`Pipeline::record`] and
    /// completed by the caller; the semaphores are live and in a state valid for the submission.
    pub(super) unsafe fn submit(
        &mut self,
        ctx: &Context,
        wait: &[vk::Semaphore],
        wait_stages: &[vk::PipelineStageFlags],
        signal: &[vk::Semaphore],
    ) -> Res<vk::Fence> {
        let d = &ctx.device;
        let s = &self.slots[self.current];
        d.end_command_buffer(s.cmd)?;
        d.reset_fences(&[s.fence])?;
        let cmds = [s.cmd];
        d.queue_submit(
            ctx.queue,
            &[vk::SubmitInfo::default()
                .wait_semaphores(wait)
                .wait_dst_stage_mask(wait_stages)
                .command_buffers(&cmds)
                .signal_semaphores(signal)],
            s.fence,
        )?;
        let fence = s.fence;
        self.current = (self.current + 1) % FRAMES_IN_FLIGHT;
        self.frame_index = self.frame_index.wrapping_add(1);
        Ok(fence)
    }

    /// Index of the current frame slot, in `0..FRAMES_IN_FLIGHT`.
    pub(super) fn slot(&self) -> usize {
        self.current
    }

    /// Destroys every object (null ones are no-ops). Idempotent.
    ///
    /// # Safety
    /// `ctx` is the pipeline's context and the GPU has finished every use of the pipeline (device idle).
    pub(super) unsafe fn destroy(&mut self, ctx: &Context) {
        let d = &ctx.device;
        self.native.destroy(d);
        self.src.destroy(d);
        for mut s in self.slots.drain(..) {
            s.params_ubo.destroy(d);
            s.frame_ubo.destroy(d);
            s.staging.destroy(d);
            d.destroy_fence(s.fence, None);
        }
        // Destroying the pools frees the command buffers and descriptor sets.
        d.destroy_descriptor_pool(self.desc_pool, None);
        d.destroy_command_pool(self.pool, None);
        d.destroy_sampler(self.sampler, None);
        self.encode.destroy(d);
        self.decode.destroy(d);
        self.desc_pool = vk::DescriptorPool::null();
        self.pool = vk::CommandPool::null();
        self.sampler = vk::Sampler::null();
    }
}

/// Copy region covering a whole tightly packed 2D colour image of `size`.
pub(super) fn full_copy(size: (u32, u32)) -> vk::BufferImageCopy {
    vk::BufferImageCopy::default()
        .image_subresource(
            vk::ImageSubresourceLayers::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .layer_count(1),
        )
        .image_extent(vk::Extent3D {
            width: size.0,
            height: size.1,
            depth: 1,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The GLSL `FrameConstants` block (`shaders/include/common.glsl`) declares every member with an
    /// explicit offset; each must equal the Rust field offset, and the sizes must match.
    #[test]
    fn frame_constants_match_the_glsl_block() {
        let glsl = include_str!("../../../../shaders/include/common.glsl");
        let block = glsl
            .split("uniform FrameConstants {")
            .nth(1)
            .and_then(|s| s.split("} frame;").next())
            .expect("FrameConstants block in common.glsl");
        let members: Vec<(usize, String)> = block
            .lines()
            .filter_map(|l| {
                let l = l.trim().strip_prefix("layout(offset = ")?;
                let (offset, rest) = l.split_once(')')?;
                let name = rest.trim().trim_end_matches(';').split_whitespace().last()?;
                Some((offset.parse().ok()?, name.to_string()))
            })
            .collect();
        let rust = [
            (std::mem::offset_of!(FrameConstants, tube_rect_px), "tube_rect_px"),
            (std::mem::offset_of!(FrameConstants, native_size), "native_size"),
            (std::mem::offset_of!(FrameConstants, target_size), "target_size"),
            (std::mem::offset_of!(FrameConstants, px_per_mm), "px_per_mm"),
            (std::mem::offset_of!(FrameConstants, line_pitch_mm), "line_pitch_mm"),
            (std::mem::offset_of!(FrameConstants, frame_index), "frame_index"),
            (std::mem::offset_of!(FrameConstants, frame_time_ms), "frame_time_ms"),
        ];
        let rust: Vec<(usize, String)> = rust.iter().map(|&(o, n)| (o, n.to_string())).collect();
        assert_eq!(members, rust);
        assert_eq!(size_of::<FrameConstants>(), 48);
        assert!(glsl.contains("std140, 48 bytes"));
    }

    #[test]
    fn letterbox_centres_the_native_raster() {
        // Same size: the whole target.
        assert_eq!(
            tube_rect_letterbox((256, 240), (256, 240), false),
            [0.0, 0.0, 256.0, 240.0]
        );
        // 256x240 into 1920x1080: scale 4.5, 1152x1080, centred horizontally.
        assert_eq!(
            tube_rect_letterbox((256, 240), (1920, 1080), false),
            [384.0, 0.0, 1536.0, 1080.0]
        );
        // Integer scaling: scale 4, 1024x960.
        assert_eq!(
            tube_rect_letterbox((256, 240), (1920, 1080), true),
            [448.0, 60.0, 1472.0, 1020.0]
        );
        // A target smaller than the raster is never scaled to an integer (scale < 1).
        assert_eq!(
            tube_rect_letterbox((256, 240), (128, 240), true),
            [0.0, 60.0, 128.0, 180.0]
        );
    }

    #[test]
    fn transfers_map_to_the_glsl_constants() {
        // TRANSFER_IDENTITY = 0, TRANSFER_POWER = 1, TRANSFER_SRGB = 2 in common.glsl.
        assert_eq!(Transfer::Identity.shader_pair().0, 0);
        assert_eq!(Transfer::Power(2.2).shader_pair(), (1, 2.2));
        assert_eq!(Transfer::Srgb.shader_pair().0, 2);
        let glsl = include_str!("../../../../shaders/include/common.glsl");
        for c in ["TRANSFER_IDENTITY = 0u", "TRANSFER_POWER = 1u", "TRANSFER_SRGB = 2u"] {
            assert!(glsl.contains(c), "{c}");
        }
    }
}

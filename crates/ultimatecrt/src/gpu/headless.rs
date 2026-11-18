//! Headless renderer: runs the GPU pipeline without any window or swapchain, into an offscreen
//! image, and reads the result back to the CPU (TEC-7, ARCHITECTURE A16).
//!
//! Used by the GPU tests (`tests/gpu_*.rs`) and, later, by offline renders. It drives the same
//! [`Pipeline`] as the window renderer (same stages, frames in flight, reallocation). Every call is
//! synchronous: it submits one frame (or a burst of frames in flight), waits for the last fence and
//! returns the pixels. It allocates its targets and readback buffers per call, which is fine off the
//! render loop.

use super::pipeline::{full_copy, Pipeline, Settings, Target};
use super::*;
use crate::params::Params;

/// Format of the offscreen target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetFormat {
    /// `R16G16B16A16_SFLOAT`, the internal format of the pipeline (A10).
    Rgba16F,
    /// `R32G32B32A32_SFLOAT`, for tests that need more precision than FP16.
    Rgba32F,
    /// `B8G8R8A8_UNORM`, the swapchain format (A7): values read back as `code / 255`.
    Bgra8Unorm,
}

impl TargetFormat {
    fn vk(self) -> vk::Format {
        match self {
            TargetFormat::Rgba16F => vk::Format::R16G16B16A16_SFLOAT,
            TargetFormat::Rgba32F => vk::Format::R32G32B32A32_SFLOAT,
            TargetFormat::Bgra8Unorm => vk::Format::B8G8R8A8_UNORM,
        }
    }

    fn bytes_per_pixel(self) -> usize {
        match self {
            TargetFormat::Rgba16F => 8,
            TargetFormat::Rgba32F => 16,
            TargetFormat::Bgra8Unorm => 4,
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

/// Result of one headless frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Rendered {
    /// The output target, as written by `encode`.
    pub output: ImageF32,
    /// The native raster written by `decode` (RGBA16F, converted exactly to `f32`).
    pub native: ImageF32,
}

/// Offscreen renderer on a surface-less [`Context`].
pub struct Headless {
    pipeline: Pipeline,
    /// Destroyed last, by the field drop after `Drop for Headless`.
    ctx: Context,
}

impl Headless {
    /// Creates a headless context ([`Context::new_headless`]) and the pipeline.
    /// With `validation`, the validation layer must be installed and its messages are counted
    /// ([`Headless::validation_counter`]).
    pub fn new(validation: bool) -> Res<Self> {
        let ctx = Context::new_headless(validation)?;
        // SAFETY: `ctx` is live; `Drop for Headless` destroys the pipeline before the context.
        let pipeline = unsafe { Pipeline::new(&ctx)? };
        Ok(Self { pipeline, ctx })
    }

    /// The Vulkan context (device, queue, chosen GPU).
    pub fn context(&self) -> &Context {
        &self.ctx
    }

    /// Shared count of validation messages, readable after `self` is dropped.
    pub fn validation_counter(&self) -> ValidationCounter {
        self.ctx.validation_counter()
    }

    /// Uploads a BGRA8 (UNORM) image, as captured, runs the pipeline with identity transfers over its
    /// full raster (no crop, divisor 1) into a `format` target of the same size, and reads the target
    /// back. Code k becomes k / 255 (rounded to the target format), alpha is 1.
    pub fn passthrough(&mut self, width: u32, height: u32, bgra: &[u8], format: TargetFormat) -> Res<ImageF32> {
        let frame = SourceFrame { width, height, bgra };
        let r = self.render(
            Some(frame),
            View::full(width, height),
            &Settings::identity(),
            &Params::default(),
            (width, height),
            format,
        )?;
        Ok(r.output)
    }

    /// Runs one frame of the pipeline: uploads `frame` if given (otherwise the previous upload is
    /// reused), decodes the native raster of `view`, encodes into a `target`-sized image of `format`,
    /// and reads back the target and the native raster.
    pub fn render(
        &mut self,
        frame: Option<SourceFrame<'_>>,
        view: View,
        settings: &Settings,
        params: &Params,
        target: (u32, u32),
        format: TargetFormat,
    ) -> Res<Rendered> {
        self.render_burst(&[(frame, view)], settings, params, target, format)
    }

    /// Runs `frames` back to back like the window loop: each frame waits only for its own frame slot
    /// ([`super::FRAMES_IN_FLIGHT`] frames in flight), into its own target image; only the last frame
    /// is read back (target and native raster). Exercises the synchronisation between frames in
    /// flight under the validation layer.
    pub fn render_burst(
        &mut self,
        frames: &[(Option<SourceFrame<'_>>, View)],
        settings: &Settings,
        params: &Params,
        target: (u32, u32),
        format: TargetFormat,
    ) -> Res<Rendered> {
        if target.0 == 0 || target.1 == 0 || frames.is_empty() {
            return Err("headless render: empty target or no frame".into());
        }
        let mut s = Scratch::default();
        // SAFETY: every handle is created on `self.ctx.device`. `run` waits for the fence of its last
        // submission before returning Ok; on any path the device is idled before `s` is destroyed, so
        // the GPU no longer uses the scratch objects.
        unsafe {
            let out = self.run(&mut s, frames, settings, params, target, format);
            let _ = self.ctx.device.device_wait_idle();
            s.destroy(&self.ctx.device);
            out
        }
    }

    /// Body of [`Headless::render_burst`]; every object it creates goes into `s`.
    ///
    /// # Safety
    /// `s` must be empty (null handles); the caller idles the device and destroys `s` afterwards.
    unsafe fn run(
        &mut self,
        s: &mut Scratch,
        frames: &[(Option<SourceFrame<'_>>, View)],
        settings: &Settings,
        params: &Params,
        target: (u32, u32),
        format: TargetFormat,
    ) -> Res<Rendered> {
        let ctx = &self.ctx;
        let d = &ctx.device;
        let extent = vk::Extent2D {
            width: target.0,
            height: target.1,
        };
        let out_bytes = target.0 as usize * target.1 as usize * format.bytes_per_pixel();
        s.readback = HostBuffer::create(ctx, out_bytes, vk::BufferUsageFlags::TRANSFER_DST)?;

        // Every frame but the last: recorded and submitted without waiting for the GPU.
        let (last, earlier) = frames.split_last().expect("frames is not empty");
        for (frame, view) in earlier {
            let t = self.next_target(s, extent, format)?;
            self.pipeline.wait_slot(ctx)?;
            if let Some(f) = frame {
                self.pipeline.stage_source(ctx, f.width, f.height, f.bgra)?;
            }
            self.pipeline.record(ctx, *view, settings, params, &t)?;
            self.pipeline.submit(ctx, &[], &[], &[])?;
        }

        let (frame, view) = last;
        let t = self.next_target(s, extent, format)?;
        self.pipeline.wait_slot(ctx)?;
        if let Some(f) = frame {
            self.pipeline.stage_source(ctx, f.width, f.height, f.bgra)?;
        }
        let cb = self.pipeline.record(ctx, *view, settings, params, &t)?;
        let dst = t.image;

        // Read back the target: -> transfer source, copy to a host-visible buffer.
        barrier(
            d,
            cb,
            dst,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
            vk::PipelineStageFlags2::COPY,
            vk::AccessFlags2::TRANSFER_READ,
        );
        d.cmd_copy_image_to_buffer(
            cb,
            dst,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            s.readback.buffer,
            &[full_copy(target)],
        );
        // Read back the native raster too (the next frame's decode discards it after this copy).
        let native = self.pipeline.native_image();
        if let Some((image, size)) = native {
            s.native_readback = HostBuffer::create(
                ctx,
                size.0 as usize * size.1 as usize * 8,
                vk::BufferUsageFlags::TRANSFER_DST,
            )?;
            barrier(
                d,
                cb,
                image,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::PipelineStageFlags2::FRAGMENT_SHADER,
                vk::AccessFlags2::NONE,
                vk::PipelineStageFlags2::COPY,
                vk::AccessFlags2::TRANSFER_READ,
            );
            d.cmd_copy_image_to_buffer(
                cb,
                image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                s.native_readback.buffer,
                &[full_copy(size)],
            );
        }
        let to_host = [vk::MemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::COPY)
            .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::HOST)
            .dst_access_mask(vk::AccessFlags2::HOST_READ)];
        d.cmd_pipeline_barrier2(cb, &vk::DependencyInfo::default().memory_barriers(&to_host));

        let fence = self.pipeline.submit(ctx, &[], &[], &[])?;
        d.wait_for_fences(&[fence], true, u64::MAX)?;

        // The readback memory is HOST_COHERENT: after the HOST_READ barrier and the fence wait, the
        // copies are visible to the host without an invalidate.
        let bytes = std::slice::from_raw_parts(s.readback.mapped as *const u8, out_bytes);
        let rgba = match format {
            TargetFormat::Rgba16F => half_to_f32(bytes),
            TargetFormat::Rgba32F => bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|&b| f32::from_le_bytes(b))
                .collect(),
            TargetFormat::Bgra8Unorm => bytes
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[2], p[1], p[0], p[3]].map(|c| f32::from(c) / 255.0))
                .collect(),
        };
        let output = ImageF32 {
            width: target.0,
            height: target.1,
            rgba,
        };
        let native = match native {
            Some((_, (w, h))) => ImageF32 {
                width: w,
                height: h,
                rgba: half_to_f32(std::slice::from_raw_parts(
                    s.native_readback.mapped as *const u8,
                    w as usize * h as usize * 8,
                )),
            },
            None => ImageF32 {
                width: 0,
                height: 0,
                rgba: Vec::new(),
            },
        };
        Ok(Rendered { output, native })
    }
}

/// A captured frame handed to [`Headless::render`]: BGRA8, rows top to bottom, tightly packed.
#[derive(Clone, Copy, Debug)]
pub struct SourceFrame<'a> {
    pub width: u32,
    pub height: u32,
    pub bgra: &'a [u8],
}

/// Little-endian binary16 values to `f32`, exactly.
fn half_to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&b| f16_to_f32(u16::from_le_bytes(b)))
        .collect()
}

impl Drop for Headless {
    fn drop(&mut self) {
        // SAFETY: after `device_wait_idle` the GPU uses none of the pipeline's objects; the device and
        // instance go last with the field drop of `ctx`.
        unsafe {
            let _ = self.ctx.device.device_wait_idle();
            self.pipeline.destroy(&self.ctx);
        }
    }
}

impl Headless {
    /// Creates a new target image of `extent` and `format` in `s` and returns it.
    ///
    /// # Safety
    /// `s` is destroyed by the caller once the GPU no longer uses it.
    unsafe fn next_target(&self, s: &mut Scratch, extent: vk::Extent2D, format: TargetFormat) -> Res<Target> {
        let image = GpuImage::create(
            &self.ctx,
            extent,
            format.vk(),
            vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC,
        )?;
        let t = Target {
            image: image.image,
            view: image.view,
            extent,
            format: format.vk(),
        };
        s.targets.push(image);
        Ok(t)
    }
}

/// Objects of one headless call. Null handles by default; `destroy` releases what was created.
#[derive(Default)]
struct Scratch {
    /// One target image per frame (frames in flight never share a target).
    targets: Vec<GpuImage>,
    readback: HostBuffer,
    native_readback: HostBuffer,
}

impl Scratch {
    /// # Safety
    /// The GPU must have finished every use of the objects (device idle).
    unsafe fn destroy(&mut self, d: &ash::Device) {
        self.native_readback.destroy(d);
        self.readback.destroy(d);
        for t in &mut self.targets {
            t.destroy(d);
        }
        self.targets.clear();
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

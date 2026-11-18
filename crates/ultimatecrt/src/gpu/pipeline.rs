//! Pipeline core (ARCHITECTURE 3.7, A10, A11, TEC-6): the fixed ordered chain of stages, the images
//! between them, the per-frame uniform buffers and the frames in flight.
//!
//! Chain: upload (captured BGRA8 -> `src` image) -> `decode` (compute, `src` -> `native` RGBA16F) ->
//! the chain slots of [`stages::CHAIN`] (`signal`, `amp`, `gun`, `screen`, `phosphor`, `optics`) ->
//! `encode` (fragment, last tube-domain image -> output target). See `stages/mod.rs` for the slot
//! contract and ARCHITECTURE 3.7 for the image contract.
//!
//! Frames in flight: [`FRAMES_IN_FLIGHT`] slots, each with its own command buffer, fence, staging upload
//! buffers, `FrameConstants` and `Params` uniform buffers and descriptor sets (one per pass, derived
//! from the chain). The CPU writes a slot only after waiting for its fence ([`Pipeline::wait_slot`]).
//! The images are shared by the frames: every image tracks its last recorded access (layout, stages,
//! write access), and every barrier is derived from that state, so a frame's barriers order it after
//! the previous frames' use of the same image (one queue, submission order = recording order). Stages
//! never record barriers.
//!
//! Images are (re)allocated when their size changes: `src` with the captured frame size, `native` with
//! the native raster, each slot output with its domain size. Reallocation idles the device first. A
//! reallocated image starts `UNDEFINED`, so any history it held is cleared (SRC-8: history cleared on
//! mode change; the history images of later stages follow the same rule).
//!
//! Per-frame use: [`Pipeline::wait_slot`], optionally [`Pipeline::stage_source`] with a new frame,
//! [`Pipeline::record`] (returns the slot's command buffer, still recording, with the target in
//! `COLOR_ATTACHMENT_OPTIMAL`), the caller's final commands on the target, then [`Pipeline::submit`].

use std::time::Instant;

use ash::vk;

use super::barrier;
use super::context::Context;
use super::headless::{GpuImage, HostBuffer, ImageF32};
use super::stages::{self, decode, decode::Decode, encode, encode::Encode, ComputePass, Domain, Registration, Stage};
use super::timing::{self, FrameTimings, GpuTimer, TimingAverager};
use crate::params::{Params, PARAMS_STD140_SIZE};
use crate::scaling::View;
use crate::Res;

/// Frames the CPU may record ahead of the GPU.
pub const FRAMES_IN_FLIGHT: usize = 2;

/// Format of the captured frame image (as delivered by the capture, A3).
const SOURCE_FORMAT: vk::Format = vk::Format::B8G8R8A8_UNORM;

/// Format of the native raster and of every intermediate image (A10: FP16, linear light after the gun).
pub const NATIVE_FORMAT: vk::Format = vk::Format::R16G16B16A16_SFLOAT;

/// Format of an image injected by a stage test ([`Pipeline::stage_injected`]): FP32, read with
/// `texelFetch` like any chain image.
pub(super) const INJECT_FORMAT: vk::Format = vk::Format::R32G32B32A32_SFLOAT;

/// SPIR-V of the bypass of a domain-changing slot (nearest resample, exact copy of texels).
const RESAMPLE_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/resample.comp.spv"));

/// Stages that may sample a chain image (any chain stage, or `encode`). A barrier into
/// `SHADER_READ_ONLY_OPTIMAL` makes the image visible to all of them.
const SHADER_READERS: vk::PipelineStageFlags2 = vk::PipelineStageFlags2::from_raw(
    vk::PipelineStageFlags2::COMPUTE_SHADER.as_raw() | vk::PipelineStageFlags2::FRAGMENT_SHADER.as_raw(),
);

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
    /// Rejects a power law whose exponent is not a finite positive number whose inverse is finite
    /// (the shaders compute `pow(l, 1 / gamma)`).
    pub fn validate(self) -> Res<()> {
        match self {
            Transfer::Power(g) if !(g > 0.0 && g.is_finite() && g.recip().is_finite()) => {
                Err(format!("transfer: power-law exponent {g} must be finite and greater than 0").into())
            }
            _ => Ok(()),
        }
    }

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
    /// Transfer applied by `decode` to the code value V = code / 255. Until the gun law exists (T-34)
    /// it stands in for it, so that the chain ends in linear light. With [`Transfer::Identity`] the
    /// native raster holds code values (drive voltages), the input contract of the voltage-domain
    /// stages (`signal`, `amp`, `gun`).
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
    /// Checks both transfers ([`Transfer::validate`]); every frame is refused with an invalid one.
    pub fn validate(&self) -> Res<()> {
        self.source.validate()?;
        self.host.validate()
    }

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
/// (60 bytes, 4-byte fields only, no padding; a test checks the offsets against the GLSL).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameConstants {
    /// Tube rectangle in target pixels: x0, y0, x1, y1 (exclusive), whole pixels.
    pub tube_rect_px: [f32; 4],
    /// Native raster size.
    pub native_size: [i32; 2],
    /// Output target size.
    pub target_size: [i32; 2],
    /// Host pixels per tube millimetre; 0 in the unplaced defaults ([`FrameConstants::new`]).
    pub px_per_mm: f32,
    /// Scanline pitch in tube millimetres; 0 in the unplaced defaults ([`FrameConstants::new`]).
    pub line_pitch_mm: f32,
    /// Frames submitted since the pipeline was created.
    pub frame_index: u32,
    /// Time since the previous frame in ms (0 on the first frame).
    pub frame_time_ms: f32,
    /// Size of the tube-domain images (`screen` output and later): one texel per host pixel of the
    /// tube rectangle (T-20b image contract).
    pub tube_size: [i32; 2],
    /// Horizontal supersampling factor K of the amp-domain images (`amp` and `gun` outputs): their size
    /// is `native_size * (K, 1)` (T-20b image contract, default [`stages::AMP_K`]).
    pub amp_k: i32,
}

impl FrameConstants {
    /// The constants of a `native` raster letterboxed into `target` (see [`tube_rect_letterbox`]), with
    /// K = [`stages::AMP_K`], tube not placed (px per mm and line pitch 0), frame index and time 0.
    /// Stage tests start from this and set the fields they inject.
    pub fn new(native: (u32, u32), target: (u32, u32), integer_scale: bool) -> Self {
        let r = tube_rect_letterbox(native, target, integer_scale);
        Self {
            tube_rect_px: r,
            native_size: [native.0 as i32, native.1 as i32],
            target_size: [target.0 as i32, target.1 as i32],
            tube_size: [(r[2] - r[0]) as i32, (r[3] - r[1]) as i32],
            amp_k: stages::AMP_K as i32,
            ..Self::default()
        }
    }

    /// Size of the images of `domain` (image contract).
    pub fn domain_size(&self, domain: Domain) -> (u32, u32) {
        let n = (self.native_size[0].max(0) as u32, self.native_size[1].max(0) as u32);
        match domain {
            Domain::Native => n,
            Domain::Amp => (n.0 * self.amp_k.max(0) as u32, n.1),
            Domain::Tube => (self.tube_size[0].max(0) as u32, self.tube_size[1].max(0) as u32),
        }
    }
}

/// Tube rectangle (x0, y0, x1, y1 in target pixels) of a `native` raster letterboxed into `target`,
/// centred, scaled uniformly, by whole factors only with `integer` (when the scale is at least 1). The
/// corners are whole pixels (size rounded to the nearest pixel). Until the tube is placed by T-30, the
/// tube is the letterboxed native raster.
pub fn tube_rect_letterbox(native: (u32, u32), target: (u32, u32), integer: bool) -> [f32; 4] {
    let (nw, nh) = (native.0.max(1) as f32, native.1.max(1) as f32);
    let (tw, th) = (target.0 as f32, target.1 as f32);
    let mut scale = (tw / nw).min(th / nh);
    if integer && scale >= 1.0 {
        scale = scale.floor();
    }
    let (w, h) = ((nw * scale).round(), (nh * scale).round());
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

/// An image of the pipeline and its last recorded access, from which every barrier is derived.
#[derive(Default)]
pub(super) struct Img {
    gpu: GpuImage,
    size: (u32, u32),
    format: vk::Format,
    layout: vk::ImageLayout,
    /// Stages of the accesses since the last barrier (the source scope of the next barrier).
    stages: vk::PipelineStageFlags2,
    /// The write access when the last access was a write not yet made available, else `NONE`.
    write: vk::AccessFlags2,
}

/// The tracked access state of an [`Img`], saved before a frame is recorded.
#[derive(Clone, Copy, Default)]
struct Track {
    /// The image the state belongs to: a reallocated image keeps its fresh `UNDEFINED` state.
    image: vk::Image,
    layout: vk::ImageLayout,
    stages: vk::PipelineStageFlags2,
    write: vk::AccessFlags2,
}

impl Img {
    fn track(&self) -> Track {
        Track {
            image: self.gpu.image,
            layout: self.layout,
            stages: self.stages,
            write: self.write,
        }
    }

    fn restore(&mut self, t: Track) {
        if t.image == self.gpu.image {
            (self.layout, self.stages, self.write) = (t.layout, t.stages, t.write);
        }
    }

    /// Recreates the image (the device is idle), state reset to `UNDEFINED`.
    ///
    /// # Safety
    /// `ctx` is live and the GPU no longer uses the old image.
    unsafe fn alloc(
        &mut self,
        ctx: &Context,
        size: (u32, u32),
        format: vk::Format,
        usage: vk::ImageUsageFlags,
    ) -> Res<()> {
        self.gpu.destroy(&ctx.device);
        *self = Self::default();
        let extent = vk::Extent2D {
            width: size.0,
            height: size.1,
        };
        self.gpu = GpuImage::create(ctx, extent, format, usage)?;
        self.size = size;
        self.format = format;
        Ok(())
    }

    /// Barrier into a write that overwrites the whole image (contents discarded), after every recorded
    /// access to it.
    ///
    /// # Safety
    /// `cb` is recording on `d` and the image is live.
    unsafe fn use_for_write(
        &mut self,
        d: &ash::Device,
        cb: vk::CommandBuffer,
        layout: vk::ImageLayout,
        stage: vk::PipelineStageFlags2,
        access: vk::AccessFlags2,
    ) {
        let (src_stage, src_access) = (self.stages, self.write);
        barrier(
            d,
            cb,
            self.gpu.image,
            vk::ImageLayout::UNDEFINED,
            layout,
            src_stage,
            src_access,
            stage,
            access,
        );
        (self.layout, self.stages, self.write) = (layout, stage, access);
    }

    /// Barrier into a read in `layout` by `stage`, contents kept. Omitted when the image is already in
    /// `layout`, readable by `stage` and not written since.
    ///
    /// # Safety
    /// `cb` is recording on `d` and the image is live.
    unsafe fn use_for_read(
        &mut self,
        d: &ash::Device,
        cb: vk::CommandBuffer,
        layout: vk::ImageLayout,
        stage: vk::PipelineStageFlags2,
        access: vk::AccessFlags2,
    ) {
        if self.layout == layout && self.write.is_empty() && self.stages.contains(stage) {
            return;
        }
        barrier(
            d,
            cb,
            self.gpu.image,
            self.layout,
            layout,
            self.stages,
            self.write,
            stage,
            access,
        );
        (self.layout, self.stages, self.write) = (layout, stage, vk::AccessFlags2::NONE);
    }

    /// Barrier for a shader read (any chain stage or `encode`).
    ///
    /// # Safety
    /// As [`Img::use_for_read`].
    unsafe fn use_for_shader_read(&mut self, d: &ash::Device, cb: vk::CommandBuffer) {
        self.use_for_read(
            d,
            cb,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            SHADER_READERS,
            vk::AccessFlags2::SHADER_SAMPLED_READ,
        );
    }

    /// Barrier for a whole-image compute storage write.
    ///
    /// # Safety
    /// As [`Img::use_for_write`].
    unsafe fn use_for_storage_write(&mut self, d: &ash::Device, cb: vk::CommandBuffer) {
        self.use_for_write(
            d,
            cb,
            vk::ImageLayout::GENERAL,
            vk::PipelineStageFlags2::COMPUTE_SHADER,
            vk::AccessFlags2::SHADER_STORAGE_WRITE,
        );
    }
}

/// One of the pipeline's images, as named by the routing of a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ImgRef {
    /// The `decode` output.
    Native,
    /// An image injected by a stage test.
    Inject,
    /// The output image of chain slot `i`.
    Link(usize),
}

/// How a chain slot is recorded in a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// The slot's stage runs.
    Run,
    /// Bypassed, same domain: nothing recorded, the next slot reads the same image.
    Skip,
    /// Bypassed, domain change: nearest resample into the slot's output image.
    Resample,
}

/// A chain slot: its registration, its stage, its enable flag and its output image.
struct Link {
    reg: &'static Registration,
    stage: Box<dyn Stage>,
    enabled: bool,
    out: Img,
}

/// Descriptor sets of one chain slot in one frame slot, with the views they were last written with.
#[derive(Clone, Copy, Default)]
struct LinkSets {
    /// Set of the stage's pass (null for a placeholder).
    run: vk::DescriptorSet,
    run_bound: [vk::ImageView; 2],
    /// Set of the resample bypass (null when the slot keeps its domain).
    bypass: vk::DescriptorSet,
    bypass_bound: [vk::ImageView; 2],
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
    /// Injected image waiting for upload (stage tests) and its size in bytes.
    inject_staging: HostBuffer,
    inject_bytes: usize,
    /// Position of the injected image: 0 replaces the `decode` output, i + 1 the output of slot i.
    inject_at: Option<usize>,
    frame_ubo: HostBuffer,
    params_ubo: HostBuffer,
    decode_set: vk::DescriptorSet,
    encode_set: vk::DescriptorSet,
    encode_bound: vk::ImageView,
    /// Clip counters written by `encode` (`encode::CLIP_BYTES` bytes), cleared by the CPU at record time.
    clip_buf: HostBuffer,
    links: Vec<LinkSets>,
}

/// The stage chain, its images and its frames in flight. It does not own the device: its owner calls
/// [`Pipeline::destroy`] before dropping the [`Context`].
pub struct Pipeline {
    pool: vk::CommandPool,
    desc_pool: vk::DescriptorPool,
    sampler: vk::Sampler,
    decode: Decode,
    encode: Encode,
    resample: ComputePass,
    chain: Vec<Link>,
    slots: Vec<Slot>,
    current: usize,
    /// Captured frame image, `SOURCE_FORMAT`.
    src: Img,
    /// `src` holds an uploaded frame (or one is recorded for upload).
    src_valid: bool,
    /// Native raster, `NATIVE_FORMAT`.
    native: Img,
    /// Image injected by a stage test, `INJECT_FORMAT`.
    inject: Img,
    /// Routing of the last recorded frame: `decode` (when it ran) then every slot with the image it left
    /// for the next one (a skipped slot names its input). Empty when no source was rendered.
    routed: Vec<(&'static str, ImgRef)>,
    /// Per-frame routing scratch (no allocation in the render loop): (slot, mode, input).
    plan: Vec<(usize, Mode, ImgRef)>,
    frame_index: u32,
    last_frame: Option<Instant>,
    /// Access state of `src`, `native`, `inject` and every chain output before the frame being
    /// recorded; restored when that frame is abandoned or its submission fails.
    saved: Vec<Track>,
    /// A frame was recorded and is neither submitted nor abandoned.
    recorded: bool,
    /// The recorded frame copies a staged frame to `src`: [`Pipeline::submit`] then marks `src` valid
    /// and the slot's staging buffer consumed.
    uploads: bool,
    /// GPU timestamps per stage (T-21, `gpu/timing.rs`).
    timer: GpuTimer,
    /// Timings of the latest frame whose results were read.
    timings: Option<FrameTimings>,
    /// Mean of the timings read since [`Pipeline::take_timing_average`].
    average: TimingAverager,
}

/// What [`Pipeline::plan_frame`] decided for a frame.
#[derive(Clone, Copy)]
struct Planned {
    staged: bool,
    have_source: bool,
    encode_pipeline: vk::Pipeline,
    /// The image `encode` reads (meaningful with `have_source`).
    last: ImgRef,
}

/// Checks the image contract of a chain: the first slot reads the native domain, each slot reads the
/// previous slot's output domain, the last writes the tube domain, names are unique.
pub(super) fn check_chain(chain: &[Registration]) -> Res<()> {
    let mut domain = Domain::Native;
    for (i, r) in chain.iter().enumerate() {
        if r.input != domain {
            return Err(format!(
                "stage chain: `{}` reads {:?}, the previous output is {domain:?}",
                r.name, r.input
            )
            .into());
        }
        if chain[..i].iter().any(|o| o.name == r.name) {
            return Err(format!("stage chain: duplicate slot `{}`", r.name).into());
        }
        domain = r.output;
    }
    if domain != Domain::Tube {
        return Err(format!("stage chain: ends in {domain:?}, `encode` reads the tube domain").into());
    }
    Ok(())
}

impl Pipeline {
    /// Creates the stage objects of [`stages::CHAIN`] and the frames in flight. Images are created on
    /// first use.
    ///
    /// # Safety
    /// `ctx` must be live; the caller destroys the result with [`Pipeline::destroy`] before `ctx`.
    pub(super) unsafe fn new(ctx: &Context) -> Res<Self> {
        Self::with_chain(ctx, stages::CHAIN)
    }

    /// As [`Pipeline::new`] with another chain (tests add a stage with it).
    ///
    /// # Safety
    /// As [`Pipeline::new`].
    pub(super) unsafe fn with_chain(ctx: &Context, chain: &'static [Registration]) -> Res<Self> {
        check_chain(chain)?;
        let mut p = Self {
            pool: vk::CommandPool::null(),
            desc_pool: vk::DescriptorPool::null(),
            sampler: vk::Sampler::null(),
            decode: Decode::default(),
            encode: Encode::default(),
            resample: ComputePass::default(),
            chain: chain
                .iter()
                .map(|reg| Link {
                    reg,
                    stage: (reg.new)(),
                    enabled: true,
                    out: Img::default(),
                })
                .collect(),
            slots: Vec::new(),
            current: 0,
            src: Img::default(),
            src_valid: false,
            native: Img::default(),
            inject: Img::default(),
            routed: Vec::with_capacity(chain.len() + 1),
            plan: Vec::with_capacity(chain.len()),
            frame_index: 0,
            last_frame: None,
            saved: Vec::with_capacity(chain.len() + 3),
            recorded: false,
            uploads: false,
            timer: GpuTimer::new(chain.iter().map(|r| r.name)),
            timings: None,
            average: TimingAverager::default(),
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
    /// As [`Pipeline::new`]; `self` holds no Vulkan object yet.
    unsafe fn create(&mut self, ctx: &Context) -> Res<()> {
        let d = &ctx.device;
        self.timer.create(ctx)?;
        self.decode.create(d)?;
        self.encode.create(d)?;
        self.resample.create(d, RESAMPLE_SPV, 0)?;
        for l in &mut self.chain {
            l.stage.create(d)?;
        }
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
        // Set layouts of one frame slot, derived from the chain: decode, encode, then per slot its
        // pass's set and, for a domain-changing slot, the resample set.
        let mut layouts = vec![self.decode.set_layout, self.encode.set_layout];
        for l in &self.chain {
            if let Some(p) = l.stage.pass() {
                layouts.push(p.set_layout);
            }
            if l.reg.input != l.reg.output {
                layouts.push(self.resample.set_layout);
            }
        }
        // Every set has 2 uniform buffers and at most one sampled and one storage image.
        let n = (FRAMES_IN_FLIGHT * layouts.len()) as u32;
        let sizes = [
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::UNIFORM_BUFFER)
                .descriptor_count(2 * n),
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(n),
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::STORAGE_IMAGE)
                .descriptor_count(n),
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::STORAGE_BUFFER)
                .descriptor_count(n),
        ];
        self.desc_pool = d.create_descriptor_pool(
            &vk::DescriptorPoolCreateInfo::default().max_sets(n).pool_sizes(&sizes),
            None,
        )?;
        let cmds = d.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::default()
                .command_pool(self.pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(FRAMES_IN_FLIGHT as u32),
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
                s.clip_buf = HostBuffer::create(ctx, encode::CLIP_BYTES, vk::BufferUsageFlags::STORAGE_BUFFER)?;
                let sets = d.allocate_descriptor_sets(
                    &vk::DescriptorSetAllocateInfo::default()
                        .descriptor_pool(self.desc_pool)
                        .set_layouts(&layouts),
                )?;
                let mut it = sets.into_iter();
                s.decode_set = it.next().unwrap_or_default();
                s.encode_set = it.next().unwrap_or_default();
                for l in &self.chain {
                    let mut ls = LinkSets::default();
                    if l.stage.pass().is_some() {
                        ls.run = it.next().unwrap_or_default();
                    }
                    if l.reg.input != l.reg.output {
                        ls.bypass = it.next().unwrap_or_default();
                    }
                    s.links.push(ls);
                }
                Ok(())
            })();
            *self.slots.last_mut().unwrap() = s;
            r?;
        }
        // The uniform buffers never change: write their descriptors once, in every set.
        for s in &self.slots {
            let params = [vk::DescriptorBufferInfo::default()
                .buffer(s.params_ubo.buffer)
                .range(PARAMS_STD140_SIZE as u64)];
            let frame = [vk::DescriptorBufferInfo::default()
                .buffer(s.frame_ubo.buffer)
                .range(size_of::<FrameConstants>() as u64)];
            let mut all = vec![s.decode_set, s.encode_set];
            for ls in &s.links {
                all.extend(
                    [ls.run, ls.bypass]
                        .into_iter()
                        .filter(|x| *x != vk::DescriptorSet::null()),
                );
            }
            let mut writes = Vec::new();
            for set in all {
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
            // The clip counters of the slot, in its `encode` set.
            let clip = [vk::DescriptorBufferInfo::default()
                .buffer(s.clip_buf.buffer)
                .range(encode::CLIP_BYTES as u64)];
            writes.push(
                vk::WriteDescriptorSet::default()
                    .dst_set(s.encode_set)
                    .dst_binding(encode::BINDING_CLIP)
                    .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                    .buffer_info(&clip),
            );
            d.update_descriptor_sets(&writes, &[]);
        }
        Ok(())
    }

    /// Clip counters of the frame slot `slot` (T-46), as `encode` left them.
    ///
    /// # Safety
    /// The slot's last submission must have finished (its fence waited), and `slot` is in
    /// `0..FRAMES_IN_FLIGHT`.
    unsafe fn clip_counts_of(&self, slot: usize) -> encode::ClipCounts {
        let p = self.slots[slot].clip_buf.mapped.cast::<u32>();
        encode::ClipCounts {
            over_white: p.read(),
            out_of_gamut: p.add(1).read(),
        }
    }

    /// Clip counters of the frame slot that [`Pipeline::wait_slot`] just freed, that is of the frame
    /// submitted [`FRAMES_IN_FLIGHT`] frames ago (all zero before it ran): the non-blocking read of the
    /// window renderer.
    ///
    /// # Safety
    /// [`Pipeline::wait_slot`] was called for the current slot and the slot is not yet recorded again.
    pub(super) unsafe fn clip_counts(&self) -> encode::ClipCounts {
        self.clip_counts_of(self.current)
    }

    /// Clip counters of the frame submitted last, for readbacks that waited for its fence.
    ///
    /// # Safety
    /// The fence of the last submission was waited.
    pub(super) unsafe fn last_clip_counts(&self) -> encode::ClipCounts {
        self.clip_counts_of((self.current + FRAMES_IN_FLIGHT - 1) % FRAMES_IN_FLIGHT)
    }

    /// Names of the chain slots, in order.
    pub(super) fn stage_names(&self) -> Vec<&'static str> {
        self.chain.iter().map(|l| l.reg.name).collect()
    }

    /// Turns the slot `name` on or off. A slot that is off is bypassed as an exact identity.
    pub(super) fn set_enabled(&mut self, name: &str, on: bool) -> Res<()> {
        let l = self
            .chain
            .iter_mut()
            .find(|l| l.reg.name == name)
            .ok_or_else(|| format!("no stage slot named `{name}`"))?;
        l.enabled = on;
        Ok(())
    }

    /// The enable flag of the slot `name`.
    pub(super) fn is_enabled(&self, name: &str) -> Option<bool> {
        self.chain.iter().find(|l| l.reg.name == name).map(|l| l.enabled)
    }

    /// Waits until the current frame slot is free (its previous submission finished). The CPU may then
    /// write the slot's buffers.
    ///
    /// # Safety
    /// `ctx` is the pipeline's context.
    pub(super) unsafe fn wait_slot(&mut self, ctx: &Context) -> Res<()> {
        let fence = self.slots[self.current].fence;
        ctx.device.wait_for_fences(&[fence], true, u64::MAX)?;
        // The slot's previous frame finished: read its timestamps (no extra wait).
        self.collect(ctx, self.current);
        Ok(())
    }

    /// Reads the timestamps of the last submission of frame slot `slot` into the latest timings and the
    /// running average.
    ///
    /// # Safety
    /// That submission has completed (fence waited or device idle).
    unsafe fn collect(&mut self, ctx: &Context, slot: usize) {
        if let Some(t) = self.timer.harvest(&ctx.device, slot) {
            self.average.push(&t);
            self.timings = Some(t);
        }
    }

    /// Reads the timestamps of every submitted frame not read yet, oldest first.
    ///
    /// # Safety
    /// The device is idle.
    pub(super) unsafe fn collect_all(&mut self, ctx: &Context) {
        for k in 0..FRAMES_IN_FLIGHT {
            self.collect(ctx, (self.current + k) % FRAMES_IN_FLIGHT);
        }
    }

    /// GPU timings of the latest frame whose results were read (the frame N-k at most
    /// [`FRAMES_IN_FLIGHT`] frames behind the CPU). `None` before any, or without timestamp support.
    pub(super) fn timings(&self) -> Option<&FrameTimings> {
        self.timings.as_ref()
    }

    /// Mean GPU timings of the frames read since the last call, then restarts the average.
    pub(super) fn take_timing_average(&mut self) -> Option<FrameTimings> {
        self.average.take()
    }

    /// Why GPU timestamps are off (device or queue without support), `None` when they work.
    pub(super) fn timing_unavailable(&self) -> Option<&str> {
        self.timer.unavailable()
    }

    /// Segment names of the timings in order: `upload`, `decode`, the chain slots, `encode`.
    pub(super) fn timing_names(&self) -> Vec<&'static str> {
        self.timer.names().to_vec()
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
        if (width, height) != self.src.size {
            self.reallocate_src(ctx, width, height)?;
        }
        let s = &mut self.slots[self.current];
        // Host writes before vkQueueSubmit are visible to the device (implicit host write ordering).
        std::ptr::copy_nonoverlapping(bgra.as_ptr(), s.staging.mapped, bytes);
        s.upload = true;
        Ok(())
    }

    /// Stage tests: copies `image` (RGBA f32) for upload by the next [`Pipeline::record_with`], where it
    /// replaces the `decode` output (`at` = 0) or the output of chain slot `at - 1`; the slots before it
    /// and `decode` do not run in that frame. Its size must be the size of that domain in the frame's
    /// constants (checked by `record_with`).
    ///
    /// # Safety
    /// As [`Pipeline::stage_source`].
    pub(super) unsafe fn stage_injected(&mut self, ctx: &Context, at: usize, image: &ImageF32) -> Res<()> {
        if at > self.chain.len() {
            return Err(format!("pipeline: injection position {at} after the last slot").into());
        }
        let size = (image.width, image.height);
        let bytes = size.0 as usize * size.1 as usize * 16;
        if size.0 == 0 || size.1 == 0 || image.rgba.len() * 4 != bytes {
            return Err("pipeline: injected image is empty or its data does not match its size".into());
        }
        let d = &ctx.device;
        if size != self.inject.size {
            d.device_wait_idle()?;
            self.inject.alloc(
                ctx,
                size,
                INJECT_FORMAT,
                vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_SRC,
            )?;
            self.invalidate_bindings();
        }
        let s = &mut self.slots[self.current];
        if s.inject_bytes < bytes {
            s.inject_staging.destroy(d);
            s.inject_bytes = 0;
            s.inject_staging = HostBuffer::create(ctx, bytes, vk::BufferUsageFlags::TRANSFER_SRC)?;
            s.inject_bytes = bytes;
        }
        std::ptr::copy_nonoverlapping(image.rgba.as_ptr().cast::<u8>(), s.inject_staging.mapped, bytes);
        s.inject_at = Some(at);
        Ok(())
    }

    /// Forgets the views every chain and encode descriptor set was written with (after a reallocation).
    fn invalidate_bindings(&mut self) {
        for s in &mut self.slots {
            s.encode_bound = vk::ImageView::null();
            for ls in &mut s.links {
                ls.run_bound = [vk::ImageView::null(); 2];
                ls.bypass_bound = [vk::ImageView::null(); 2];
            }
        }
    }

    /// # Safety
    /// `ctx` is the pipeline's context.
    unsafe fn reallocate_src(&mut self, ctx: &Context, width: u32, height: u32) -> Res<()> {
        let d = &ctx.device;
        d.device_wait_idle()?;
        self.src_valid = false;
        for s in &mut self.slots {
            s.staging.destroy(d);
            s.upload = false;
        }
        self.src.alloc(
            ctx,
            (width, height),
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
        let info = [vk::DescriptorImageInfo::default()
            .sampler(self.sampler)
            .image_view(self.src.gpu.view)
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

    /// Usage of the chain images (and `native`): written by compute, sampled, read back by tests.
    fn chain_usage() -> vk::ImageUsageFlags {
        vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_SRC
    }

    /// # Safety
    /// `ctx` is the pipeline's context.
    unsafe fn reallocate_native(&mut self, ctx: &Context, native: (u32, u32)) -> Res<()> {
        let d = &ctx.device;
        d.device_wait_idle()?;
        self.native.alloc(ctx, native, NATIVE_FORMAT, Self::chain_usage())?;
        self.invalidate_bindings();
        let storage = [vk::DescriptorImageInfo::default()
            .image_view(self.native.gpu.view)
            .image_layout(vk::ImageLayout::GENERAL)];
        for s in &self.slots {
            d.update_descriptor_sets(
                &[vk::WriteDescriptorSet::default()
                    .dst_set(s.decode_set)
                    .dst_binding(decode::BINDING_NATIVE)
                    .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                    .image_info(&storage)],
                &[],
            );
        }
        Ok(())
    }

    fn img(&mut self, r: ImgRef) -> &mut Img {
        match r {
            ImgRef::Native => &mut self.native,
            ImgRef::Inject => &mut self.inject,
            ImgRef::Link(i) => &mut self.chain[i].out,
        }
    }

    /// Records the frame into the current slot's command buffer and returns it, still recording: upload
    /// of a staged frame, `decode`, the chain, then `encode` into `target`, which ends in
    /// `COLOR_ATTACHMENT_OPTIMAL` (whole target written: tube rectangle plus black surround). Without
    /// any source frame yet, the target is only cleared to black. The `FrameConstants` are computed
    /// from `view`, `settings` and the target ([`FrameConstants::new`], frame index and time).
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
        let now = Instant::now();
        let mut fc = FrameConstants::placed(
            view.native,
            (target.extent.width, target.extent.height),
            settings.integer_scale,
            params,
        );
        fc.frame_index = self.frame_index;
        fc.frame_time_ms = self
            .last_frame
            .map_or(0.0, |t| now.duration_since(t).as_secs_f32() * 1000.0);
        self.last_frame = Some(now);
        self.record_with(ctx, view, fc, settings, params, target)
    }

    /// Does the fallible preparation of the frame [`Pipeline::record`] will record for a target of
    /// `extent` and `format`: validation of the settings, `encode` pipeline, (re)allocation of the
    /// images, descriptor sets. The window renderer calls it before acquiring a swapchain image, so that
    /// those failures never leave an acquired image unpresented. `record` repeats it at no cost.
    ///
    /// # Safety
    /// `ctx` is the pipeline's context and [`Pipeline::wait_slot`] was called for the current slot.
    pub(super) unsafe fn prepare(
        &mut self,
        ctx: &Context,
        view: View,
        settings: &Settings,
        extent: vk::Extent2D,
        format: vk::Format,
    ) -> Res<()> {
        let fc = FrameConstants::new(view.native, (extent.width, extent.height), settings.integer_scale);
        self.plan_frame(ctx, view, &fc, settings, format).map(|_| ())
    }

    /// As [`Pipeline::record`] with the given `FrameConstants` (stage tests inject theirs). The image
    /// sizes follow `fc` (image contract); `fc.native_size` must equal `view.native` unless an injected
    /// image replaces the `decode` output or a later one.
    ///
    /// # Safety
    /// As [`Pipeline::record`].
    pub(super) unsafe fn record_with(
        &mut self,
        ctx: &Context,
        view: View,
        fc: FrameConstants,
        settings: &Settings,
        params: &Params,
        target: &Target,
    ) -> Res<vk::CommandBuffer> {
        // A frame recorded but never submitted nor abandoned: its image states are rolled back.
        self.abandon();
        let cur = self.current;
        let planned = self.plan_frame(ctx, view, &fc, settings, target.format);
        // An injection is used by this frame or dropped with it (on an error).
        let inject_at = self.slots[cur].inject_at.take();
        let Planned {
            staged,
            have_source,
            encode_pipeline,
            last,
            ..
        } = planned?;
        self.record_commands(
            ctx,
            view,
            &fc,
            settings,
            params,
            target,
            inject_at,
            staged,
            have_source,
            encode_pipeline,
            last,
        )
    }

    /// Everything a frame needs before its commands are recorded, none of it recorded: the validity of
    /// the settings and constants, the `encode` pipeline for `target_format`, the routing of the chain,
    /// the (re)allocation of the images and the descriptor sets. Idempotent for a given slot, `view`,
    /// constants and format, so that [`Pipeline::prepare`] can run it before the swapchain image is
    /// acquired and [`Pipeline::record_with`] again after.
    ///
    /// # Safety
    /// As [`Pipeline::record`].
    unsafe fn plan_frame(
        &mut self,
        ctx: &Context,
        view: View,
        fc: &FrameConstants,
        settings: &Settings,
        target_format: vk::Format,
    ) -> Res<Planned> {
        settings.validate()?;
        let cur = self.current;
        let inject_at = self.slots[cur].inject_at;
        let staged = self.slots[cur].upload;
        let native = fc.domain_size(Domain::Native);
        let have_source = inject_at.is_some() || ((self.src_valid || staged) && native.0 > 0 && native.1 > 0);
        if inject_at.is_none() && have_source && native != view.native {
            return Err("pipeline: FrameConstants native size differs from the view".into());
        }
        if fc.amp_k < 1 {
            return Err("pipeline: amp_k (K) must be at least 1".into());
        }
        let encode_pipeline = self.encode.pipeline(&ctx.device, target_format)?;

        // Routing and image sizes, decided before recording (descriptor sets are written before the
        // command buffer binds them).
        self.plan.clear();
        self.routed.clear();
        let mut input = ImgRef::Native;
        if have_source {
            let first = match inject_at {
                Some(at) => {
                    let domain = if at == 0 {
                        Domain::Native
                    } else {
                        self.chain[at - 1].reg.output
                    };
                    if self.inject.size != fc.domain_size(domain) {
                        return Err(format!(
                            "pipeline: injected image is {:?}, the {domain:?} domain is {:?}",
                            self.inject.size,
                            fc.domain_size(domain)
                        )
                        .into());
                    }
                    input = ImgRef::Inject;
                    at
                }
                None => {
                    if native != self.native.size {
                        self.reallocate_native(ctx, native)?;
                    }
                    self.routed.push(("decode", ImgRef::Native));
                    0
                }
            };
            for i in first..self.chain.len() {
                let l = &self.chain[i];
                let mode = if l.enabled && l.stage.pass().is_some() {
                    Mode::Run
                } else if l.reg.input == l.reg.output {
                    Mode::Skip
                } else {
                    Mode::Resample
                };
                if mode != Mode::Skip {
                    let size = fc.domain_size(l.reg.output);
                    if size.0 == 0 || size.1 == 0 {
                        return Err(format!("pipeline: empty {:?} domain for `{}`", l.reg.output, l.reg.name).into());
                    }
                    if self.chain[i].out.size != size {
                        ctx.device.device_wait_idle()?;
                        self.chain[i].out.alloc(ctx, size, NATIVE_FORMAT, Self::chain_usage())?;
                        self.invalidate_bindings();
                    }
                }
                self.plan.push((i, mode, input));
                if mode != Mode::Skip {
                    input = ImgRef::Link(i);
                }
                self.routed.push((self.chain[i].reg.name, input));
            }
            // Descriptor sets of this frame slot (rewritten only when their images changed).
            for k in 0..self.plan.len() {
                let (i, mode, inp) = self.plan[k];
                if mode == Mode::Skip {
                    continue;
                }
                let views = [self.img(inp).gpu.view, self.chain[i].out.gpu.view];
                let ls = &mut self.slots[cur].links[i];
                let (set, bound) = match mode {
                    Mode::Run => (ls.run, &mut ls.run_bound),
                    _ => (ls.bypass, &mut ls.bypass_bound),
                };
                if *bound != views {
                    write_io(&ctx.device, set, self.sampler, views);
                    *bound = views;
                }
            }
            let last = self.img(input).gpu.view;
            let s = &mut self.slots[cur];
            if s.encode_bound != last {
                let info = [vk::DescriptorImageInfo::default()
                    .sampler(self.sampler)
                    .image_view(last)
                    .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
                ctx.device.update_descriptor_sets(
                    &[vk::WriteDescriptorSet::default()
                        .dst_set(s.encode_set)
                        .dst_binding(encode::BINDING_INPUT)
                        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                        .image_info(&info)],
                    &[],
                );
                s.encode_bound = last;
            }
        }
        Ok(Planned {
            staged,
            have_source,
            encode_pipeline,
            last: input,
        })
    }

    /// Writes the constants and records the commands of a planned frame.
    ///
    /// # Safety
    /// As [`Pipeline::record`], after [`Pipeline::plan_frame`] succeeded for the same arguments.
    // The plan's values travel as arguments: a struct would only repeat `Planned`.
    #[allow(clippy::too_many_arguments)]
    unsafe fn record_commands(
        &mut self,
        ctx: &Context,
        view: View,
        fc: &FrameConstants,
        settings: &Settings,
        params: &Params,
        target: &Target,
        inject_at: Option<usize>,
        staged: bool,
        have_source: bool,
        encode_pipeline: vk::Pipeline,
        input: ImgRef,
    ) -> Res<vk::CommandBuffer> {
        let cur = self.current;
        let s = &mut self.slots[cur];
        // SAFETY: FrameConstants is #[repr(C)] with 4-byte fields only (no padding); both buffers are
        // mapped, host coherent, at least as large as what is copied, and not in use (fence waited).
        std::ptr::copy_nonoverlapping(
            (fc as *const FrameConstants).cast::<u8>(),
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
        self.timer.begin(d, cb, cur);

        // From here nothing fails, but the frame only counts once submitted: save the access state of
        // every image so that an unsubmitted frame can be rolled back ([`Pipeline::abandon`]).
        self.saved.clear();
        self.saved.push(self.src.track());
        self.saved.push(self.native.track());
        self.saved.push(self.inject.track());
        self.saved.extend(self.chain.iter().map(|l| l.out.track()));
        self.recorded = true;
        self.uploads = staged;

        if staged {
            self.src.use_for_write(
                d,
                cb,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::PipelineStageFlags2::COPY,
                vk::AccessFlags2::TRANSFER_WRITE,
            );
            d.cmd_copy_buffer_to_image(
                cb,
                self.slots[cur].staging.buffer,
                self.src.gpu.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[full_copy(self.src.size)],
            );
        }
        if inject_at.is_some() {
            self.inject.use_for_write(
                d,
                cb,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::PipelineStageFlags2::COPY,
                vk::AccessFlags2::TRANSFER_WRITE,
            );
            d.cmd_copy_buffer_to_image(
                cb,
                self.slots[cur].inject_staging.buffer,
                self.inject.gpu.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[full_copy(self.inject.size)],
            );
        }
        if staged || inject_at.is_some() {
            self.timer.mark(d, cb, cur, timing::UPLOAD, false);
        }

        if have_source {
            if inject_at.is_none() {
                // Decode overwrites every texel of `native`.
                self.src.use_for_read(
                    d,
                    cb,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::PipelineStageFlags2::COMPUTE_SHADER,
                    vk::AccessFlags2::SHADER_SAMPLED_READ,
                );
                self.native.use_for_storage_write(d, cb);
                self.decode
                    .record(d, cb, self.slots[cur].decode_set, view, settings.source);
                self.timer.mark(d, cb, cur, timing::DECODE, false);
            }
            for k in 0..self.plan.len() {
                let (i, mode, inp) = self.plan[k];
                if mode == Mode::Skip {
                    continue;
                }
                self.img(inp).use_for_shader_read(d, cb);
                self.chain[i].out.use_for_storage_write(d, cb);
                let size = self.chain[i].out.size;
                let ls = self.slots[cur].links[i];
                if mode == Mode::Run {
                    self.chain[i].stage.record(d, cb, ls.run, size);
                } else {
                    self.resample.dispatch(d, cb, ls.bypass, &[], size);
                }
                self.timer.mark(d, cb, cur, timing::chain(i), mode == Mode::Resample);
            }
            self.img(input).use_for_shader_read(d, cb);
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
        let s = &self.slots[cur];
        // The slot's previous frame is done (`wait_slot`): clear the counters `encode` increments.
        std::ptr::write_bytes(s.clip_buf.mapped, 0, encode::CLIP_BYTES);
        self.encode.record(
            d,
            cb,
            encode_pipeline,
            have_source.then_some(s.encode_set),
            target.view,
            target.extent,
            settings.host,
            params,
            target.format,
        );
        self.timer.mark(d, cb, cur, self.timer.encode(), false);
        Ok(cb)
    }

    /// Routing of the last recorded frame: `("decode", Native)` when decode ran, then every chain slot
    /// with the image it left for the next one (a bypassed same-domain slot names its input).
    pub(super) fn routed(&self) -> &[(&'static str, ImgRef)] {
        &self.routed
    }

    /// Records the barrier that makes image `r` a copy source (tracked like every other access) and
    /// returns the image, its size and format. For readbacks after [`Pipeline::record`], in the same
    /// command buffer.
    ///
    /// # Safety
    /// `cb` is the command buffer returned by the last `record`, still recording; `r` comes from
    /// [`Pipeline::routed`] of that frame.
    pub(super) unsafe fn prepare_readback(
        &mut self,
        d: &ash::Device,
        cb: vk::CommandBuffer,
        r: ImgRef,
    ) -> (vk::Image, (u32, u32), vk::Format) {
        let img = self.img(r);
        img.use_for_read(
            d,
            cb,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            vk::PipelineStageFlags2::COPY,
            vk::AccessFlags2::TRANSFER_READ,
        );
        (img.gpu.image, img.size, img.format)
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
        let cur = self.current;
        let (cmd, fence) = (self.slots[cur].cmd, self.slots[cur].fence);
        if self.recorded {
            self.timer.finish(d, cmd, cur);
        }
        if let Err(e) = d.end_command_buffer(cmd) {
            self.abandon();
            return Err(e.into());
        }
        if let Err(e) = d.reset_fences(&[fence]) {
            self.abandon();
            return Err(e.into());
        }
        let cmds = [cmd];
        if let Err(e) = d.queue_submit(
            ctx.queue,
            &[vk::SubmitInfo::default()
                .wait_semaphores(wait)
                .wait_dst_stage_mask(wait_stages)
                .command_buffers(&cmds)
                .signal_semaphores(signal)],
            fence,
        ) {
            // The fence was reset and nothing will signal it: replace it by a signalled one, or the next
            // `wait_slot` of this slot would never return.
            d.destroy_fence(fence, None);
            self.slots[cur].fence = d
                .create_fence(
                    &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                    None,
                )
                .unwrap_or_default();
            self.abandon();
            return Err(e.into());
        }
        // The frame is on the GPU: its upload counts now.
        if std::mem::take(&mut self.uploads) {
            self.slots[cur].upload = false;
            self.src_valid = true;
        }
        self.recorded = false;
        self.timer.submitted(cur);
        self.current = (self.current + 1) % FRAMES_IN_FLIGHT;
        self.frame_index = self.frame_index.wrapping_add(1);
        Ok(fence)
    }

    /// Gives up the frame recorded by the last [`Pipeline::record`] that is not submitted: the tracked
    /// access state of every image goes back to what it was before (the commands never ran), and the
    /// staged frame, if any, stays staged for the next record. No-op without such a frame. Called by
    /// [`Pipeline::submit`] on failure and by [`Pipeline::record_with`] for a forgotten frame; its
    /// callers call it when they fail between `record` and `submit`.
    pub(super) fn abandon(&mut self) {
        if !std::mem::take(&mut self.recorded) {
            return;
        }
        self.uploads = false;
        let mut saved = self.saved.iter().copied();
        let imgs = [&mut self.src, &mut self.native, &mut self.inject]
            .into_iter()
            .chain(self.chain.iter_mut().map(|l| &mut l.out));
        for img in imgs {
            if let Some(t) = saved.next() {
                img.restore(t);
            }
        }
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
        self.native.gpu.destroy(d);
        self.src.gpu.destroy(d);
        self.inject.gpu.destroy(d);
        for l in &mut self.chain {
            l.out.gpu.destroy(d);
            l.stage.destroy(d);
        }
        for mut s in self.slots.drain(..) {
            s.params_ubo.destroy(d);
            s.clip_buf.destroy(d);
            s.frame_ubo.destroy(d);
            s.staging.destroy(d);
            s.inject_staging.destroy(d);
            d.destroy_fence(s.fence, None);
        }
        // Destroying the pools frees the command buffers and descriptor sets.
        d.destroy_descriptor_pool(self.desc_pool, None);
        d.destroy_command_pool(self.pool, None);
        d.destroy_sampler(self.sampler, None);
        self.timer.destroy(d);
        self.resample.destroy(d);
        self.encode.destroy(d);
        self.decode.destroy(d);
        self.desc_pool = vk::DescriptorPool::null();
        self.pool = vk::CommandPool::null();
        self.sampler = vk::Sampler::null();
    }
}

/// Writes the input (binding 2, sampled) and output (binding 3, storage) images of a chain set.
///
/// # Safety
/// `set` is a live set with the standard chain bindings, not bound in a recording command buffer.
unsafe fn write_io(d: &ash::Device, set: vk::DescriptorSet, sampler: vk::Sampler, views: [vk::ImageView; 2]) {
    let input = [vk::DescriptorImageInfo::default()
        .sampler(sampler)
        .image_view(views[0])
        .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
    let output = [vk::DescriptorImageInfo::default()
        .image_view(views[1])
        .image_layout(vk::ImageLayout::GENERAL)];
    d.update_descriptor_sets(
        &[
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(stages::BINDING_INPUT)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&input),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(stages::BINDING_OUTPUT)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&output),
        ],
        &[],
    );
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
            (std::mem::offset_of!(FrameConstants, tube_size), "tube_size"),
            (std::mem::offset_of!(FrameConstants, amp_k), "amp_k"),
        ];
        let rust: Vec<(usize, String)> = rust.iter().map(|&(o, n)| (o, n.to_string())).collect();
        assert_eq!(members, rust);
        assert_eq!(size_of::<FrameConstants>(), 60);
        assert!(glsl.contains("std140, 60 bytes"));
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
        // Corners are whole pixels: 256x224 into 300x300, scale 1.171875, 300x262.5 -> 300x263.
        assert_eq!(
            tube_rect_letterbox((256, 224), (300, 300), false),
            [0.0, 18.0, 300.0, 281.0]
        );
    }

    #[test]
    fn frame_constants_follow_the_image_contract() {
        let fc = FrameConstants::new((256, 240), (1920, 1080), false);
        assert_eq!(fc.amp_k, 4);
        assert_eq!(fc.domain_size(Domain::Native), (256, 240));
        assert_eq!(fc.domain_size(Domain::Amp), (1024, 240));
        assert_eq!(fc.tube_size, [1152, 1080]);
        assert_eq!(fc.domain_size(Domain::Tube), (1152, 1080));
    }

    #[test]
    fn the_chain_is_ordered_and_consistent() {
        let names: Vec<&str> = stages::CHAIN.iter().map(|r| r.name).collect();
        assert_eq!(names, ["signal", "amp", "gun", "screen", "phosphor", "optics"]);
        check_chain(stages::CHAIN).expect("valid chain");
        let domains: Vec<(Domain, Domain)> = stages::CHAIN.iter().map(|r| (r.input, r.output)).collect();
        use Domain::*;
        assert_eq!(
            domains,
            [
                (Native, Native),
                (Native, Amp),
                (Amp, Amp),
                (Amp, Tube),
                (Tube, Tube),
                (Tube, Tube)
            ]
        );
        // A chain that breaks the contract is refused.
        assert!(check_chain(&stages::CHAIN[..3]).is_err());
        assert!(check_chain(&[stages::signal::SLOT, stages::screen::SLOT]).is_err());
    }

    #[test]
    fn power_exponents_are_validated() {
        for g in [2.4, 1.0, 0.5, 1e-3, 100.0] {
            assert!(Transfer::Power(g).validate().is_ok(), "{g}");
        }
        for g in [0.0, -0.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1e-45] {
            assert!(Transfer::Power(g).validate().is_err(), "{g}");
        }
        assert!(Transfer::Identity.validate().is_ok());
        assert!(Transfer::Srgb.validate().is_ok());
        assert!(Settings::default().validate().is_ok());
        assert!(Settings::identity().validate().is_ok());
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

//! Pipeline core (ARCHITECTURE 3.7, A10, A11, TEC-6): the fixed ordered chain of stages, the images
//! between them, the per-frame uniform buffers and the frames in flight.
//!
//! Chain: upload (captured BGRA8 -> `src` image) -> `decode` (compute, `src` -> `native` RGBA16F) ->
//! the chain slots of [`stages::CHAIN`] (`signal`, `amp`, `gun`, `screen`, `phosphor`, `optics`) ->
//! `encode` (fragment, last tube-domain image -> output target) -> the UI pass when the settings menu or the
//! overlay is shown (`ui_render.rs`, T-52a: display space, over the output, outside the chain). See
//! `stages/mod.rs` for the slot contract and ARCHITECTURE 3.7 for the image contract.
//!
//! Frames in flight: [`FRAMES_IN_FLIGHT`] slots, each with its own command buffer, fence, staging upload
//! buffers, `FrameConstants` and `Params` uniform buffers and descriptor sets (one per pass, derived
//! from the chain). The CPU writes a slot only after waiting for its fence ([`Pipeline::wait_slot`]).
//! The images are shared by the frames: every image tracks its last recorded access (layout, stages,
//! write access), and every barrier is derived from that state, so a frame's barriers order it after
//! the previous frames' use of the same image (one queue, submission order = recording order). Stages
//! never record barriers on the chain images; a stage that owns other objects (history images, state
//! buffers, line images, several passes: `abl`, `screen`, `wear`, `phosphor`, `optics`) records the
//! barriers on those itself.
//!
//! Images are (re)allocated when their size changes: `src` with the captured frame size, `native` with
//! the native raster, each slot output with its domain size. Reallocation idles the device first. A
//! reallocated image starts `UNDEFINED`, so any history it held is cleared (SRC-8: history cleared on
//! mode change; the history images of later stages follow the same rule).
//!
//! Limits (R-02, R-01): the amplifier K of a live frame is limited to what the device allows for one image
//! ([`crate::quality::clamp_amp_k`], [`Context::image_limits`]); a frame whose tube area is empty or whose
//! images would exceed the device's largest image side is not an error: only the surround is drawn. Both
//! raise a notice ([`Pipeline::notice`]), printed once.
//!
//! Per-frame use: [`Pipeline::wait_slot`], optionally [`Pipeline::stage_source`] with a new frame,
//! [`Pipeline::record`] (returns the slot's command buffer, still recording, with the target in
//! `COLOR_ATTACHMENT_OPTIMAL`), the caller's final commands on the target, then [`Pipeline::submit`].

use std::time::Instant;

use ash::vk;

use super::barrier;
use super::context::Context;
use super::external::{self, ExternalSet};
use super::headless::{DeviceBuffer, GpuImage, HostBuffer, ImageF32};
use super::stages::{self, decode, decode::Decode, encode, encode::Encode, ComputePass, Domain, Registration, Stage};
use super::timing::{self, FrameTimings, GpuTimer, TimingAverager};
use super::ui_render::{self, UiPass};
use crate::headroom::{effective_mode, headroom, OutputTransform};
use crate::params::{Params, PARAMS_STD140_SIZE};
use crate::quality::{self, ImageLimits};
use crate::scaling::View;
use crate::Res;
use crt_model::output::Headroom;

/// GPU-side notices (R-02, R-01): one message per kind, printed once (`notice: ...`) when it appears or changes,
/// queued for [`Notices::take_fresh`], and readable as the notice in force ([`Notices::current`]).
#[derive(Default)]
pub(super) struct Notices {
    active: Vec<(&'static str, String)>,
    fresh: Vec<String>,
}

impl Notices {
    /// Sets the message of `kind` (`None` clears it). Kinds: a chain slot name for a stage's notice
    /// ([`Stage::notice`]), `limit: ...` for the pipeline's own (never a slot name).
    pub(super) fn set(&mut self, kind: &'static str, msg: Option<String>) {
        let i = self.active.iter().position(|(k, _)| *k == kind);
        match (i, msg) {
            (Some(i), None) => {
                self.active.remove(i);
            }
            (Some(i), Some(m)) if self.active[i].1 == m => {}
            (i, Some(m)) => {
                println!("notice: {m}");
                self.fresh.push(m.clone());
                match i {
                    Some(i) => self.active[i].1 = m,
                    None => self.active.push((kind, m)),
                }
            }
            (None, None) => {}
        }
    }

    /// The notices in force, joined, `None` when there is none.
    pub(super) fn current(&self) -> Option<String> {
        (!self.active.is_empty()).then(|| {
            self.active
                .iter()
                .map(|(_, m)| m.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        })
    }

    /// The messages that appeared or changed since the last call.
    pub(super) fn take_fresh(&mut self) -> Vec<String> {
        std::mem::take(&mut self.fresh)
    }
}

/// Replaces the fence of a failed submission (reset, so nothing would ever signal it) by a new signalled one from
/// `create`, destroying `old` (R-28). `None` when no fence could be made: `old` is kept (never a null fence), and
/// the caller must not wait on it.
fn replace_fence(
    old: vk::Fence,
    create: impl FnOnce() -> ash::prelude::VkResult<vk::Fence>,
    destroy: impl FnOnce(vk::Fence),
) -> Option<vk::Fence> {
    match create() {
        Ok(f) if f != vk::Fence::null() => {
            destroy(old);
            Some(f)
        }
        _ => None,
    }
}

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
    /// Optional transfer applied by `decode` to the drive voltage V. Since T-34 the gun law is the `gun`
    /// slot (`params/gun.toml`), so the default is [`Transfer::Identity`]: the native raster holds the
    /// drive voltage, the input contract of the voltage-domain stages (`signal`, `amp`, `gun`). Any other
    /// value is the pre-T-34 stand-in and applies a second transfer before the gun law.
    pub source: Transfer,
    /// Host display EOTF; `encode` applies its inverse (COL-3).
    pub host: Transfer,
    /// Scale the tube rectangle by whole factors only (when at least 1).
    pub integer_scale: bool,
}

impl Default for Settings {
    /// Drive voltage out of `decode` (the tube gamma 2.4 is the default gun law, `params/gun.toml`; SRC-5:
    /// captured values are drive voltages) and host pure 2.2 [E] (COL-3 default).
    fn default() -> Self {
        Self {
            source: Transfer::Identity,
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

/// The entries of the target table that follows the [`FrameConstants`] in the frame uniform buffer (`target_table` of
/// `common.glsl`): one `vec4` per hardware colour of a signal colour target, then one per reference colour, from entry
/// `SIGNAL_MAX`: twice [`crt_model::target::SIGNAL_MAX`] entries, 8 KiB, so that the frame uniform buffer (96 bytes of
/// constants and the table) stays within the 16 KiB of `maxUniformBufferRange` that Vulkan guarantees.
pub const FRAME_TABLE_VEC4: usize = 2 * crt_model::target::SIGNAL_MAX;

/// Bytes of the frame uniform buffer: the constants, then the target table.
const FRAME_BLOCK_BYTES: usize = size_of::<FrameConstants>() + 16 * FRAME_TABLE_VEC4;

/// Per-frame constants, std140, laid out as the `FrameConstants` block of `shaders/include/common.glsl`
/// (96 bytes, 4-byte fields only, two explicit padding fields; a test checks the offsets against the GLSL).
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
    /// Real time since the pipeline was created, in milliseconds (the sum of the frame times; wraps after 49 days).
    /// The GLSL block has padding here (bytes 60 to 63, to the 16-byte alignment of the std140 `vec3` below): the
    /// shaders do not read it; the CPU plans of the time-based modules do ([`FrameConstants::source_frame`]).
    pub elapsed_ms: u32,
    /// White balance gains of the `gun` stage (T-34): per-channel factors on the beam current. `new` gives
    /// (1, 1, 1); `placed` computes them from the parameter table (`src/white.rs`).
    pub white_gain: [f32; 3],
    /// Padding to the 8-byte alignment of the std140 `ivec2` below (bytes 76 to 79).
    pub(crate) pad1: i32,
    /// Margin (host px, per side, x and y) of the tube image around the raster (T-37): the tube image is
    /// the raster rectangle grown by this margin, so the spot's skirt past the raster's ends is drawn
    /// (`tube_size - 2 margin` is the raster). 0 in [`FrameConstants::new`]: the tube image is the raster.
    pub raster_margin: [i32; 2],
    /// Mask footprint per axis, x and y (T-39c), decided on the CPU in f64 by
    /// [`FrameConstants::with_mask_tiers`] (the pipeline applies it to every frame it records): 0 = one whole
    /// period (prefiltered, closed form), 1 = the host pixel (area integral), between: the box width in host
    /// px of the blend band.
    pub mask_box_px: [f32; 2],
}

impl FrameConstants {
    /// The number of source frames that have passed at `rate_hz` frames per second of real time: the time base of
    /// the signal impairments, the tape and the TV flicker, so that their hum, crawl, roll and flicker run in real
    /// seconds on any display rate (a 144 Hz output redraws the same source frame for a while, as a real tube
    /// shows the same field). Computed in f64 from [`FrameConstants::elapsed_ms`].
    pub fn source_frame(&self, rate_hz: f64) -> u32 {
        (f64::from(self.elapsed_ms) * rate_hz.max(1e-6) / 1000.0).floor() as u32
    }

    /// These constants at the real time of source frame `frame` at `rate_hz` (tests): the smallest whole
    /// millisecond for which [`FrameConstants::source_frame`] gives `frame`.
    pub fn at_source_frame(mut self, frame: u32, rate_hz: f64) -> Self {
        let mut ms = (f64::from(frame) * 1000.0 / rate_hz).ceil() as u32;
        while self.with_elapsed(ms).source_frame(rate_hz) < frame {
            ms += 1;
        }
        self.elapsed_ms = ms;
        self
    }

    fn with_elapsed(mut self, ms: u32) -> Self {
        self.elapsed_ms = ms;
        self
    }

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
            white_gain: [1.0; 3],
            mask_box_px: [1.0; 2],
            ..Self::default()
        }
    }

    /// Size of the images of `domain` (image contract). Saturating: an absurd K gives a width the pipeline
    /// refuses as too large for the device, never an overflow.
    pub fn domain_size(&self, domain: Domain) -> (u32, u32) {
        let n = (self.native_size[0].max(0) as u32, self.native_size[1].max(0) as u32);
        match domain {
            Domain::Native => n,
            Domain::Amp => (n.0.saturating_mul(self.amp_k.max(0) as u32), n.1),
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
    /// Same domain and not run (disabled, placeholder, or an identity stage, [`Stage::is_identity`]):
    /// nothing recorded, the next slot reads the same image.
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
    /// The image view decode_set's source binding was last written with (`src`, or an imported image of the
    /// zero-copy path, A29), null when it must be written again.
    decode_src: vk::ImageView,
    encode_set: vk::DescriptorSet,
    encode_bound: vk::ImageView,
    /// Clip counters as the CPU reads them (`encode::CLIP_BYTES` bytes): a copy of `clip_dev` made at the end of
    /// the frame.
    clip_buf: HostBuffer,
    /// The clip counters `encode` increments, in device-local memory (atomics on host memory cross the bus
    /// one by one), cleared on the GPU before `encode`.
    clip_dev: DeviceBuffer,
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
    /// Zero-copy (A29): the imported images of the capture pool in use.
    ext: Option<ExternalSet>,
    /// The imported image that is the source now (in place of `src`), `None` on the CPU path.
    ext_current: Option<usize>,
    /// The imported image the frame being recorded reads (committed by `submit`: its first use is then done).
    ext_recorded: Option<usize>,
    /// The last submitted frame read an imported image (the renderer keeps that slot held until its fence).
    ext_submitted: bool,
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
    /// Real time since the pipeline was created, ms (the sum of the frame times).
    elapsed_ms: f64,
    last_frame: Option<Instant>,
    /// Frame period, ms, given to every recorded frame instead of the measured one (headless tests of the
    /// real-time stages through the whole chain, `Headless::set_frame_time_ms`); `None` (the window loop):
    /// measured.
    frame_time_override: Option<f32>,
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
    /// SDR headroom readout of the last recorded frame and the inputs it was computed from (T-84): computed
    /// again only when the parameters, the placement or the `screen` routing change.
    headroom: Option<(HeadroomKey, Headroom)>,
    /// What one image may be on the device (R-02).
    limits: ImageLimits,
    /// What the pipeline limited or left out (R-02, R-01).
    notices: Notices,
    /// Set when a failed submission left a slot without a usable fence (R-28): every later frame fails with it.
    broken: Option<String>,
    /// Test fault: the staging buffer of this frame slot fails to allocate in `reallocate_src` (R-27).
    #[cfg(test)]
    pub(super) fail_staging_alloc: Option<usize>,
}

/// The inputs of the headroom readout: parameters, px per mm, line pitch (bits) and whether `screen` runs.
type HeadroomKey = (Params, u32, u32, bool);

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
            ext: None,
            ext_current: None,
            ext_recorded: None,
            ext_submitted: false,
            native: Img::default(),
            inject: Img::default(),
            routed: Vec::with_capacity(chain.len() + 1),
            plan: Vec::with_capacity(chain.len()),
            frame_index: 0,
            elapsed_ms: 0.0,
            last_frame: None,
            frame_time_override: None,
            saved: Vec::with_capacity(chain.len() + 3),
            recorded: false,
            uploads: false,
            timer: GpuTimer::new(chain.iter().map(|r| r.name)),
            timings: None,
            average: TimingAverager::default(),
            headroom: None,
            limits: ctx.image_limits(),
            notices: Notices::default(),
            broken: None,
            #[cfg(test)]
            fail_staging_alloc: None,
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
            l.stage.allocate(ctx)?;
        }
        // T-37: the ABL state buffer (owned by `abl`) is read by `screen` for raster_scale.
        if let Some(abl) = self.chain.iter().find_map(|l| l.stage.abl_state()) {
            for l in &mut self.chain {
                l.stage.link_abl_state(d, abl);
            }
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
        // Every set of this pool has 2 uniform buffers and at most one combined image sampler, one storage image
        // and one storage buffer (checked against each layout: decode = sampler + storage image, encode = sampler +
        // the clip counters' storage buffer, chain passes and resample = the standard layout of sampler + storage
        // image). Stage-owned sets (`abl`, `screen`, `optics`, `phosphor`, `wear`) come from their own pools.
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
                s.frame_ubo = HostBuffer::create(ctx, FRAME_BLOCK_BYTES, vk::BufferUsageFlags::UNIFORM_BUFFER)?;
                s.params_ubo = HostBuffer::create(ctx, PARAMS_STD140_SIZE, vk::BufferUsageFlags::UNIFORM_BUFFER)?;
                s.clip_buf = HostBuffer::create(ctx, encode::CLIP_BYTES, vk::BufferUsageFlags::TRANSFER_DST)?;
                // R-50: read before the slot's first frame finished (`clip_counts`): zero, not whatever the
                // allocation held. Mapped, host coherent, `CLIP_BYTES` long, not in use yet.
                std::ptr::write_bytes(s.clip_buf.mapped, 0, encode::CLIP_BYTES);
                s.clip_dev = DeviceBuffer::create(
                    ctx,
                    encode::CLIP_BYTES,
                    vk::BufferUsageFlags::STORAGE_BUFFER
                        | vk::BufferUsageFlags::TRANSFER_SRC
                        | vk::BufferUsageFlags::TRANSFER_DST,
                )?;
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
                .range(FRAME_BLOCK_BYTES as u64)];
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
                .buffer(s.clip_dev.buffer)
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

    /// SDR headroom readout of the last recorded frame (T-84, `crate::headroom`), `None` before the first.
    pub(super) fn headroom(&self) -> Option<Headroom> {
        self.headroom.map(|(_, h)| h)
    }

    /// The notices in force (R-02, R-01), joined; `None` when nothing is limited.
    pub(super) fn notice(&self) -> Option<String> {
        self.notices.current()
    }

    /// The notices that appeared since the previous call.
    pub(super) fn take_notices(&mut self) -> Vec<String> {
        self.notices.take_fresh()
    }

    /// The image limits the pipeline applies (the device's, [`Context::image_limits`]).
    pub(super) fn image_limits(&self) -> ImageLimits {
        self.limits
    }

    /// The size of `src` and how many frame slots have a staging buffer (tests of R-27).
    #[cfg(test)]
    pub(super) fn source_allocation(&self) -> ((u32, u32), usize) {
        (
            self.src.size,
            self.slots.iter().filter(|s| !s.staging.mapped.is_null()).count(),
        )
    }

    /// Replaces the image limits (tests: a small device without one).
    pub(super) fn set_image_limits(&mut self, limits: ImageLimits) {
        self.limits = limits;
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

    /// Clears the history of every stage that keeps one (T-41, SRC-8): the next frame shows no
    /// afterglow of the frames before it.
    pub(super) fn clear_history(&mut self) {
        for l in &mut self.chain {
            l.stage.clear_history();
        }
    }

    /// Gives every frame recorded from now on the period `ms` instead of the measured one (`None`: measured
    /// again).
    pub(super) fn set_frame_time_override(&mut self, ms: Option<f32>) {
        self.frame_time_override = ms;
    }

    /// The burn-in wear map of the `wear` slot (T-43), `None` in a chain without it.
    pub(super) fn wear(&mut self) -> Option<&mut stages::wear::Wear> {
        self.chain.iter_mut().find_map(|l| l.stage.wear())
    }

    /// Waits until the current frame slot is free (its previous submission finished). The CPU may then
    /// write the slot's buffers. The wait is bounded ([`super::FENCE_TIMEOUT`]): a GPU that does not finish
    /// the frame in time is reported as lost ([`super::GpuError::DeviceLost`]), never waited for ever.
    ///
    /// # Safety
    /// `ctx` is the pipeline's context.
    pub(super) unsafe fn wait_slot(&mut self, ctx: &Context) -> Res<()> {
        if let Some(b) = &self.broken {
            return Err(b.clone().into());
        }
        let fence = self.slots[self.current].fence;
        super::wait_fences(&ctx.device, &[fence], "a frame")?;
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

    /// Segment names of the timings in order: `upload`, `decode`, the chain slots, `encode`, `ui`.
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
        let max = self.limits.max_dim;
        if width > max || height > max {
            return Err(format!(
                "pipeline: the captured frame is {width}x{height}, larger than this GPU's {max} px image limit"
            )
            .into());
        }
        if (width, height) != self.src.size {
            self.reallocate_src(ctx, width, height)?;
        }
        let s = &mut self.slots[self.current];
        // R-27: `reallocate_src` never leaves a size without its buffers; checked anyway before writing.
        if s.staging.mapped.is_null() {
            return Err("pipeline: the source staging buffer is missing".into());
        }
        // Host writes before vkQueueSubmit are visible to the device (implicit host write ordering).
        std::ptr::copy_nonoverlapping(bgra.as_ptr(), s.staging.mapped, bytes);
        s.upload = true;
        // The uploaded frame is the source from now on (not an imported image).
        self.ext_current = None;
        Ok(())
    }

    /// Zero-copy (A29): makes `slot` of the shared texture pool `pool` the source of the next frames, in place of
    /// `src`. The pool's textures are imported once (all of them, the device idled first, the previous pool's
    /// imports destroyed) when its generation is new. A frame staged in an upload buffer and not recorded yet is
    /// dropped (this one is newer). On an error nothing is the source any more until the next staged frame, and
    /// the pipeline keeps working with the CPU path. Returns true when the pool was imported (the device was idled:
    /// no frame reads any earlier shared texture any more).
    ///
    /// # Safety
    /// `ctx` is the pipeline's context and [`Pipeline::wait_slot`] was called for the current slot.
    pub(super) unsafe fn stage_external(
        &mut self,
        ctx: &Context,
        pool: &std::sync::Arc<crate::capture::SharedTextures>,
        slot: usize,
    ) -> Res<bool> {
        if ctx.external_memory().is_none() {
            return Err(if ctx.external_memory_requested() {
                "zero-copy: the Vulkan device cannot import Direct3D 11 textures".into()
            } else {
                "zero-copy: the Vulkan device was created without VK_KHR_external_memory_win32".into()
            });
        }
        let (width, height) = pool.size();
        let max = self.limits.max_dim;
        if width == 0 || height == 0 || width > max || height > max {
            return Err(
                format!("zero-copy: a shared frame of {width}x{height} (this GPU's image limit is {max} px)").into(),
            );
        }
        // A new pool by generation or by identity (a restarted capture session never repeats a generation, and the
        // identity check covers pools made by hand): never sample the textures of a pool that is not this one.
        let imported = crate::zerocopy::needs_import(
            self.ext.as_ref().map(|e| (e.generation, e.pool_id())),
            (pool.generation(), std::sync::Arc::as_ptr(pool) as usize),
        );
        if imported {
            ctx.device.device_wait_idle()?;
            self.ext_current = None;
            if let Some(mut old) = self.ext.take() {
                old.destroy(&ctx.device);
            }
            for s in &mut self.slots {
                s.decode_src = vk::ImageView::null();
            }
            self.ext = Some(ExternalSet::import(ctx, pool)?);
        }
        if slot >= self.ext.as_ref().map_or(0, |e| e.images.len()) {
            return Err(format!("zero-copy: no shared slot {slot}").into());
        }
        for s in &mut self.slots {
            s.upload = false;
        }
        self.ext_current = Some(slot);
        Ok(imported)
    }

    /// The source is an imported shared texture (zero-copy), not `src`.
    pub(super) fn source_is_external(&self) -> bool {
        self.ext_current.is_some()
    }

    /// The last submitted frame read an imported shared texture.
    pub(super) fn submitted_external(&self) -> bool {
        self.ext_submitted
    }

    /// Forgets the imported source (after a zero-copy failure): the next frames draw `src` if it holds a frame.
    pub(super) fn drop_external_source(&mut self) {
        self.ext_current = None;
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

    /// Reallocates `src` and every slot's staging buffer for a captured frame of `width` x `height`. All or
    /// nothing (R-27): the staging buffers and the image are made first and committed together; on any failure
    /// `src` is left empty (size 0 x 0) and no slot has a staging buffer, so the next frame of any size
    /// reallocates again instead of writing through a null mapping.
    ///
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
        self.src.gpu.destroy(d);
        self.src = Img::default();
        let mut staging: Vec<HostBuffer> = Vec::with_capacity(self.slots.len());
        let made = (|| -> Res<()> {
            for _i in 0..self.slots.len() {
                #[cfg(test)]
                if self.fail_staging_alloc == Some(_i) {
                    return Err("pipeline: staging allocation failed (test fault)".into());
                }
                staging.push(HostBuffer::create(
                    ctx,
                    width as usize * height as usize * 4,
                    vk::BufferUsageFlags::TRANSFER_SRC,
                )?);
            }
            Ok(())
        })();
        let made = made.and_then(|()| {
            self.src.alloc(
                ctx,
                (width, height),
                SOURCE_FORMAT,
                vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED,
            )
        });
        if let Err(e) = made {
            for mut b in staging {
                b.destroy(d);
            }
            self.src.gpu.destroy(d);
            self.src = Img::default();
            return Err(e);
        }
        for (s, b) in self.slots.iter_mut().zip(staging) {
            s.staging = b;
            // Written again by `plan_frame` (with this `src`, or an imported image).
            s.decode_src = vk::ImageView::null();
        }
        Ok(())
    }

    /// Points `decode_set` of frame slot `cur` at the frame's source: `src` (`SHADER_READ_ONLY_OPTIMAL`) or the
    /// imported image of the zero-copy path (`GENERAL`). Written only when it changed.
    ///
    /// # Safety
    /// The slot's previous submission finished ([`Pipeline::wait_slot`]).
    unsafe fn bind_decode_source(&mut self, d: &ash::Device, cur: usize) {
        let (view, layout) = match (self.ext_current, &self.ext) {
            (Some(i), Some(e)) => (e.images[i].view, vk::ImageLayout::GENERAL),
            _ => (self.src.gpu.view, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL),
        };
        let s = &mut self.slots[cur];
        if s.decode_src == view || view == vk::ImageView::null() {
            return;
        }
        let info = [vk::DescriptorImageInfo::default()
            .sampler(self.sampler)
            .image_view(view)
            .image_layout(layout)];
        d.update_descriptor_sets(
            &[vk::WriteDescriptorSet::default()
                .dst_set(s.decode_set)
                .dst_binding(decode::BINDING_SRC)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&info)],
            &[],
        );
        s.decode_src = view;
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
    /// `COLOR_ATTACHMENT_OPTIMAL` (whole target written: tube rectangle plus the surround, the clear colour
    /// `source_surround_color`). Without any source frame yet, or when the frame cannot run the chain (an empty
    /// tube area, an image too large for the device), the target is only cleared to the surround colour. The
    /// `FrameConstants` are computed
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
        self.record_ui(ctx, view, settings, params, target, None)
    }

    /// As [`Pipeline::record`], then the UI pass of `ui` (T-52a) as the last step, after `encode`, when it has
    /// something to draw and was made for the target's format ([`ui_render`]): the menu and overlay are drawn
    /// in display space on the target and never go through the chain. Without it nothing is recorded (the
    /// output is bit-exact) and the `ui` timing segment stays empty.
    ///
    /// # Safety
    /// As [`Pipeline::record`]; `ui` satisfies [`UiPass::draw`]'s contract for the returned command buffer.
    pub(super) unsafe fn record_ui(
        &mut self,
        ctx: &Context,
        view: View,
        settings: &Settings,
        params: &Params,
        target: &Target,
        ui: Option<&mut dyn UiPass>,
    ) -> Res<vk::CommandBuffer> {
        // PHY-19: a monochrome phosphor type resolved into the parameters (crate::phosphor_type); T-42: a worn gun's
        // grown spot, seen by every user of the spot law (crate::ageing).
        let params = &crate::phosphor_type::render_params(params);
        let now = Instant::now();
        let mut fc = self.live_constants(view, settings, params, target.extent);
        fc.frame_index = self.frame_index;
        fc.frame_time_ms = self.frame_time_override.unwrap_or_else(|| {
            self.last_frame
                .map_or(0.0, |t| now.duration_since(t).as_secs_f32() * 1000.0)
        });
        self.last_frame = Some(now);
        // Real time for the time-based modules: the sum of the frame times (the first frame is time 0).
        self.elapsed_ms += f64::from(fc.frame_time_ms);
        fc.elapsed_ms = (self.elapsed_ms as u64) as u32;
        self.record_frame(ctx, view, fc, settings, params, target, ui)
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
        params: &Params,
        extent: vk::Extent2D,
        format: vk::Format,
    ) -> Res<()> {
        // The same constants as `record` (image sizes included), so its plan reallocates nothing.
        let params = &crate::phosphor_type::render_params(params);
        let fc = self.live_constants(view, settings, params, extent);
        self.plan_frame(ctx, view, &fc, settings, params, format).map(|_| ())
    }

    /// The `FrameConstants` of a live frame (before its index and time): the tube placed from the
    /// parameters ([`FrameConstants::placed`]), grown by the spot's skirt margin
    /// ([`FrameConstants::with_skirt_margin`], T-37) when the beam draws the tube image: the `screen` slot
    /// runs (enabled, written, and not skipped by an injection after it). Otherwise the `screen` slot is
    /// bypassed by the nearest-neighbour resample, which would stretch the raster over the margin, so the tube
    /// image stays the raster rectangle. `with_skirt_margin` itself leaves unplaced constants unchanged
    /// (beam_eval's nearest-sample fallback has no skirt either).
    ///
    /// R-02: the amplifier K is limited to what the device allows for one amp-domain image, in every quality
    /// tier ([`quality::clamp_amp_k`]), with a notice while it is.
    fn live_constants(
        &mut self,
        view: View,
        settings: &Settings,
        params: &Params,
        extent: vk::Extent2D,
    ) -> FrameConstants {
        let mut fc = FrameConstants::placed(
            view.native,
            (extent.width, extent.height),
            settings.integer_scale,
            params,
        );
        let asked = fc.amp_k;
        let (k, clamped) = quality::clamp_amp_k(asked, view.native, self.limits);
        fc.amp_k = k;
        let l = self.limits;
        self.notices.set(
            "limit: amp K",
            clamped.then(|| {
                format!(
                    "amplifier samples limited to {k} (asked {asked}): a {} px wide source would exceed this GPU's \
                     image limit ({} px, {} MiB per image)",
                    view.native.0,
                    l.max_dim,
                    l.budget_bytes >> 20
                )
            }),
        );
        let inject_at = self.slots.get(self.current).and_then(|s| s.inject_at);
        let screen_runs = self.chain.iter().enumerate().any(|(i, l)| {
            l.reg.name == "screen" && l.enabled && l.stage.pass().is_some() && inject_at.is_none_or(|at| at <= i)
        });
        if screen_runs {
            fc.with_skirt_margin(params)
        } else {
            fc
        }
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
        let params = &crate::phosphor_type::render_params(params);
        self.record_frame(ctx, view, fc, settings, params, target, None)
    }

    /// [`Pipeline::record_with`] with the optional UI pass of [`Pipeline::record_ui`].
    ///
    /// # Safety
    /// As [`Pipeline::record_ui`].
    // The frame's inputs plus the UI: a struct would only repeat the public signatures.
    #[allow(clippy::too_many_arguments)]
    unsafe fn record_frame(
        &mut self,
        ctx: &Context,
        view: View,
        fc: FrameConstants,
        settings: &Settings,
        params: &Params,
        target: &Target,
        ui: Option<&mut dyn UiPass>,
    ) -> Res<vk::CommandBuffer> {
        // A frame recorded but never submitted nor abandoned: its image states are rolled back.
        self.abandon();
        let cur = self.current;
        let planned = self.plan_frame(ctx, view, &fc, settings, params, target.format);
        // An injection is used by this frame or dropped with it (on an error).
        let inject_at = self.slots[cur].inject_at.take();
        let Planned {
            staged,
            have_source,
            encode_pipeline,
            last,
            ..
        } = planned?;
        // Stage-owned resources (history images, T-41), once the routing of the frame is known.
        let mut abl_runs = false;
        for i in 0..self.chain.len() {
            let runs = have_source && self.plan.iter().any(|&(j, m, _)| j == i && m == Mode::Run);
            // `screen` decides its beam path in `prepare` (T-48: an unscaled raster when `abl` does not run),
            // so every stage learns whether an earlier `abl` runs before its own `prepare`.
            self.chain[i].stage.set_abl_runs(abl_runs);
            self.chain[i].stage.prepare(ctx, &fc, params, runs)?;
            abl_runs |= runs && self.chain[i].stage.abl_state().is_some();
            let (name, notice) = (self.chain[i].reg.name, self.chain[i].stage.notice());
            self.notices.set(name, notice);
        }
        // T-37: `screen` reads `raster_scale` from the ABL state only when `abl` updates it in this frame.
        for l in &self.chain {
            l.stage.set_abl_runs(abl_runs);
        }
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
            ui,
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
        params: &Params,
        target_format: vk::Format,
    ) -> Res<Planned> {
        settings.validate()?;
        let cur = self.current;
        let inject_at = self.slots[cur].inject_at;
        let staged = self.slots[cur].upload;
        let native = fc.domain_size(Domain::Native);
        let source = self.src_valid || staged || self.ext_current.is_some();
        let mut have_source = inject_at.is_some() || (source && native.0 > 0 && native.1 > 0);
        if inject_at.is_none() && have_source && native != view.native {
            return Err("pipeline: FrameConstants native size differs from the view".into());
        }
        if fc.amp_k < 1 {
            return Err("pipeline: amp_k (K) must be at least 1".into());
        }
        // R-01, R-02: an empty tube area (tube parameters at 0) or an image larger than the device allows is
        // not an error: the frame draws the surround only, with a notice, until the parameters or the source
        // change. The staged frame stays staged.
        if have_source {
            let blocked = self.blocked(fc, inject_at);
            self.notices.set("limit: frame", blocked.clone());
            if blocked.is_some() {
                have_source = false;
            }
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
                    self.bind_decode_source(&ctx.device, cur);
                    self.routed.push(("decode", ImgRef::Native));
                    0
                }
            };
            for i in first..self.chain.len() {
                let l = &self.chain[i];
                let same_domain = l.reg.input == l.reg.output;
                // An identity stage of a same-domain slot is routed around like a disabled slot, decided
                // again every frame from this frame's parameters.
                let mode = if l.enabled && l.stage.pass().is_some() && !(same_domain && l.stage.is_identity(params, fc))
                {
                    Mode::Run
                } else if same_domain {
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

    /// Why a frame with a source cannot run the chain, `None` when it can: an image it needs (the native
    /// raster unless injected, then the output of every slot from the injection point on) is empty or larger
    /// than the device's largest image side.
    fn blocked(&self, fc: &FrameConstants, inject_at: Option<usize>) -> Option<String> {
        let mut domains = Vec::with_capacity(3);
        if inject_at.is_none() {
            domains.push(Domain::Native);
        }
        for l in &self.chain[inject_at.unwrap_or(0).min(self.chain.len())..] {
            if !domains.contains(&l.reg.output) {
                domains.push(l.reg.output);
            }
        }
        let max = self.limits.max_dim;
        for d in domains {
            let (w, h) = fc.domain_size(d);
            if w == 0 || h == 0 {
                return Some(match d {
                    Domain::Tube => "the tube area is empty (a tube size, aspect or height parameter at 0?): only \
                                     the surround is drawn"
                        .to_string(),
                    _ => format!("the {d:?} image is empty: only the surround is drawn"),
                });
            }
            if w > max || h > max {
                return Some(format!(
                    "the {d:?} image would be {w} x {h} px, more than this GPU's {max} px limit: only the surround \
                     is drawn"
                ));
            }
        }
        None
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
        ui: Option<&mut dyn UiPass>,
    ) -> Res<vk::CommandBuffer> {
        let cur = self.current;
        // T-84: the headroom readout of this frame (the `screen` slot draws the mask and the scanlines only
        // when it runs) and the mode the frame really runs (T-84b: `auto` resolved, uploaded to the shader).
        let screen = have_source
            && self
                .plan
                .iter()
                .any(|&(i, m, _)| m == Mode::Run && self.chain[i].reg.name == "screen");
        let key = (*params, fc.px_per_mm.to_bits(), fc.line_pitch_mm.to_bits(), screen);
        let h = match self.headroom {
            Some((k, h)) if k == key => h,
            _ => {
                let h = headroom(params, fc.px_per_mm, fc.line_pitch_mm, screen);
                self.headroom = Some((key, h));
                h
            }
        };
        let mut resolved = *params;
        resolved.output_headroom_mode = effective_mode(params, h.mask_peak_physical);
        let params = &resolved;
        let s = &mut self.slots[cur];
        // T-39c: the mask tiers of this frame, decided once on the CPU (the shader does not decide).
        let fc = &fc.with_mask_tiers(params);
        // SAFETY: FrameConstants is #[repr(C)] with 4-byte fields only (no padding); both buffers are
        // mapped, host coherent, at least as large as what is copied, and not in use (fence waited).
        std::ptr::copy_nonoverlapping(
            (fc as *const FrameConstants).cast::<u8>(),
            s.frame_ubo.mapped,
            size_of::<FrameConstants>(),
        );
        // The hardware colours of a signal colour target, after the constants (the shader reads `target_table`).
        if let Some(table) = crate::targets::frame_table(params) {
            std::ptr::copy_nonoverlapping(
                table.as_ptr().cast::<u8>(),
                s.frame_ubo.mapped.add(size_of::<FrameConstants>()),
                table.len() * 16,
            );
        }
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
        self.ext_recorded = None;

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
                // Zero-copy (A29): the imported image is acquired from the external owner for `decode` and released
                // after it; otherwise `src` is read.
                let ext = self.ext_current.and_then(|i| {
                    self.ext
                        .as_ref()
                        .map(|e| (i, e.images[i].image, e.images[i].initialized))
                });
                match ext {
                    Some((_, image, initialized)) => {
                        external::ownership_barrier(d, cb, image, ctx.queue_family, initialized, false)
                    }
                    None => self.src.use_for_read(
                        d,
                        cb,
                        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                        vk::PipelineStageFlags2::COMPUTE_SHADER,
                        vk::AccessFlags2::SHADER_SAMPLED_READ,
                    ),
                }
                // Decode overwrites every texel of `native`.
                self.native.use_for_storage_write(d, cb);
                self.decode
                    .record(d, cb, self.slots[cur].decode_set, view, settings.source);
                if let Some((i, image, _)) = ext {
                    external::ownership_barrier(d, cb, image, ctx.queue_family, true, true);
                    self.ext_recorded = Some(i);
                }
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
        // T-84: the output transform of the headroom mode, from the readout computed above.
        let xf = OutputTransform::new(params, &h);
        let s = &self.slots[cur];
        // Clear the counters `encode` increments, on the GPU: after the previous frames' encode and copy of
        // this buffer (queue order), before this encode.
        clip_barrier(
            d,
            cb,
            s.clip_dev.buffer,
            vk::PipelineStageFlags2::FRAGMENT_SHADER | vk::PipelineStageFlags2::COPY,
            vk::AccessFlags2::SHADER_STORAGE_WRITE | vk::AccessFlags2::SHADER_STORAGE_READ,
            vk::PipelineStageFlags2::ALL_TRANSFER,
            vk::AccessFlags2::TRANSFER_WRITE,
        );
        d.cmd_fill_buffer(cb, s.clip_dev.buffer, 0, vk::WHOLE_SIZE, 0);
        clip_barrier(
            d,
            cb,
            s.clip_dev.buffer,
            vk::PipelineStageFlags2::ALL_TRANSFER,
            vk::AccessFlags2::TRANSFER_WRITE,
            vk::PipelineStageFlags2::FRAGMENT_SHADER,
            vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::SHADER_STORAGE_WRITE,
        );
        self.encode.record(
            d,
            cb,
            encode_pipeline,
            have_source.then_some(s.encode_set),
            target.view,
            target.extent,
            // The tube rectangle as the target shows it: a turned tube (TATE) is turned into the output.
            crate::placement::viewer_rect(
                crate::placement::Turn::from_params(params),
                fc.tube_rect_px,
                (fc.target_size[0] as f32, fc.target_size[1] as f32),
            ),
            settings.host,
            params,
            xf,
            target.format,
        );
        // The counters to the host-visible copy the CPU reads after the fence.
        let s = &self.slots[cur];
        clip_barrier(
            d,
            cb,
            s.clip_dev.buffer,
            vk::PipelineStageFlags2::FRAGMENT_SHADER,
            vk::AccessFlags2::SHADER_STORAGE_WRITE,
            vk::PipelineStageFlags2::COPY,
            vk::AccessFlags2::TRANSFER_READ,
        );
        d.cmd_copy_buffer(
            cb,
            s.clip_dev.buffer,
            s.clip_buf.buffer,
            &[vk::BufferCopy::default().size(encode::CLIP_BYTES as u64)],
        );
        self.timer.mark(d, cb, cur, self.timer.encode(), false);
        // T-52a: the settings menu and the overlay, in display space over what `encode` wrote. Nothing is
        // recorded when the UI has nothing to draw (bit-exact output, empty `ui` segment).
        if let Some(ui) = ui.filter(|u| u.wants_frame() && u.format() == target.format) {
            ui_render::record(d, cb, target, ui)?;
            self.timer.mark(d, cb, cur, self.timer.ui(), false);
        }
        // A fence wait does not make device writes visible to the host: the clip counters copied above
        // are read on the CPU after the frame (windowed `Renderer::render` and headless alike).
        barrier_to_host(d, cb, true);
        Ok(cb)
    }

    /// Routing of the last recorded frame: `("decode", Native)` when decode ran, then every chain slot
    /// with the image it left for the next one (a bypassed same-domain slot names its input).
    pub(super) fn routed(&self) -> &[(&'static str, ImgRef)] {
        &self.routed
    }

    /// Chain slots that recorded a pass in the last planned frame: a stage or a resample copy (decode and
    /// encode not counted). A slot before an injection point is not part of the frame.
    pub(super) fn chain_passes(&self) -> usize {
        self.plan.iter().filter(|p| p.1 != Mode::Skip).count()
    }

    /// Slots the last planned frame routed around (disabled, placeholder or identity, same domain).
    pub(super) fn skipped_slots(&self) -> Vec<&'static str> {
        self.plan
            .iter()
            .filter(|p| p.1 == Mode::Skip)
            .map(|p| self.chain[p.0].reg.name)
            .collect()
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
            // `wait_slot` of this slot would never return. Never a null fence (R-28): when none can be made,
            // the old one stays (destroyed with the pipeline) and the pipeline is broken, so `wait_slot` fails
            // instead of waiting on it.
            let fresh = replace_fence(
                fence,
                || {
                    d.create_fence(
                        &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                        None,
                    )
                },
                |f| d.destroy_fence(f, None),
            );
            match fresh {
                Some(f) => self.slots[cur].fence = f,
                None => {
                    self.broken = Some(format!(
                        "pipeline: a frame submission failed ({e}) and its fence could not be replaced"
                    ))
                }
            }
            self.abandon();
            return Err(e.into());
        }
        // The frame is on the GPU: its upload counts now.
        if std::mem::take(&mut self.uploads) {
            self.slots[cur].upload = false;
            self.src_valid = true;
        }
        // Zero-copy: the imported image it read has left `UNDEFINED`.
        let ext = self.ext_recorded.take();
        if let (Some(i), Some(e)) = (ext, self.ext.as_mut()) {
            e.images[i].initialized = true;
        }
        self.ext_submitted = ext.is_some();
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
        self.ext_recorded = None;
        // Stage-owned state the frame consumed (pending history clears and layout transitions).
        for l in &mut self.chain {
            l.stage.abandon();
        }
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
        if let Some(mut e) = self.ext.take() {
            e.destroy(d);
        }
        self.ext_current = None;
        self.inject.gpu.destroy(d);
        for l in &mut self.chain {
            l.out.gpu.destroy(d);
            l.stage.destroy(d);
        }
        for mut s in self.slots.drain(..) {
            s.params_ubo.destroy(d);
            s.clip_buf.destroy(d);
            s.clip_dev.destroy(d);
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

/// One buffer memory barrier on the whole of `buffer` (the clip counters).
///
/// # Safety
/// `cb` is a command buffer in the recording state of `d`; `buffer` is live.
unsafe fn clip_barrier(
    d: &ash::Device,
    cb: vk::CommandBuffer,
    buffer: vk::Buffer,
    src_stage: vk::PipelineStageFlags2,
    src_access: vk::AccessFlags2,
    dst_stage: vk::PipelineStageFlags2,
    dst_access: vk::AccessFlags2,
) {
    let b = [vk::BufferMemoryBarrier2::default()
        .src_stage_mask(src_stage)
        .src_access_mask(src_access)
        .dst_stage_mask(dst_stage)
        .dst_access_mask(dst_access)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .buffer(buffer)
        .size(vk::WHOLE_SIZE)];
    d.cmd_pipeline_barrier2(cb, &vk::DependencyInfo::default().buffer_memory_barriers(&b));
}

/// Records the barrier that makes the shader storage writes, and with `copies` the transfer writes (the clip
/// counters' copy, readbacks), visible to the host reads that follow the frame's fence wait. Shared by the
/// windowed and the headless paths.
///
/// # Safety
/// `cb` is a command buffer in the recording state of `d`.
pub(super) unsafe fn barrier_to_host(d: &ash::Device, cb: vk::CommandBuffer, copies: bool) {
    let mut src_stage = vk::PipelineStageFlags2::FRAGMENT_SHADER;
    let mut src_access = vk::AccessFlags2::SHADER_STORAGE_WRITE;
    if copies {
        src_stage |= vk::PipelineStageFlags2::COPY;
        src_access |= vk::AccessFlags2::TRANSFER_WRITE;
    }
    let to_host = [vk::MemoryBarrier2::default()
        .src_stage_mask(src_stage)
        .src_access_mask(src_access)
        .dst_stage_mask(vk::PipelineStageFlags2::HOST)
        .dst_access_mask(vk::AccessFlags2::HOST_READ)];
    d.cmd_pipeline_barrier2(cb, &vk::DependencyInfo::default().memory_barriers(&to_host));
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
            (std::mem::offset_of!(FrameConstants, white_gain), "white_gain"),
            (std::mem::offset_of!(FrameConstants, raster_margin), "raster_margin"),
            (std::mem::offset_of!(FrameConstants, mask_box_px), "mask_box_px"),
        ];
        let rust: Vec<(usize, String)> = rust.iter().map(|&(o, n)| (o, n.to_string())).collect();
        // The target table follows the constants (its offset is the struct size, a multiple of 16).
        let mut rust = rust;
        rust.push((size_of::<FrameConstants>(), format!("target_table[{FRAME_TABLE_VEC4}]")));
        assert_eq!(members, rust);
        assert_eq!(size_of::<FrameConstants>(), 96);
        assert!(glsl.contains("std140, 96 bytes"));
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
        // R-02: no overflow for an absurd K (the pipeline then refuses the size as too large).
        let huge = FrameConstants {
            amp_k: i32::MAX,
            ..FrameConstants::new((4096, 240), (1920, 1080), false)
        };
        assert_eq!(huge.domain_size(Domain::Amp), (u32::MAX, 240));
    }

    /// R-28: a failed fence creation keeps the old fence (never a null one) and destroys nothing; a successful
    /// one replaces and destroys the old.
    #[test]
    fn a_fence_is_replaced_only_by_a_real_one() {
        use vk::Handle;
        let old = vk::Fence::from_raw(7);
        let mut destroyed = Vec::new();
        assert_eq!(
            replace_fence(old, || Err(vk::Result::ERROR_OUT_OF_HOST_MEMORY), |f| destroyed.push(f)),
            None
        );
        assert_eq!(
            replace_fence(old, || Ok(vk::Fence::null()), |f| destroyed.push(f)),
            None
        );
        assert!(destroyed.is_empty(), "the old fence is kept");
        let new = vk::Fence::from_raw(9);
        assert_eq!(replace_fence(old, || Ok(new), |f| destroyed.push(f)), Some(new));
        assert_eq!(destroyed, [old]);
    }

    /// GPU notices are printed and queued once per change, and cleared per kind.
    #[test]
    fn notices_fire_once_per_change() {
        let mut n = Notices::default();
        assert_eq!(n.current(), None);
        n.set("amp", Some("K limited to 6".into()));
        n.set("amp", Some("K limited to 6".into()));
        n.set("frame", None);
        assert_eq!(n.take_fresh(), ["K limited to 6"]);
        assert!(n.take_fresh().is_empty(), "taken once");
        n.set("frame", Some("tube empty".into()));
        assert_eq!(n.current().as_deref(), Some("K limited to 6; tube empty"));
        n.set("amp", Some("K limited to 4".into()));
        assert_eq!(n.take_fresh(), ["tube empty", "K limited to 4"]);
        n.set("amp", None);
        assert_eq!(n.current().as_deref(), Some("tube empty"));
        n.set("frame", None);
        assert_eq!(n.current(), None);
        assert!(n.take_fresh().is_empty(), "clearing raises nothing");
    }

    #[test]
    fn the_chain_is_ordered_and_consistent() {
        let names: Vec<&str> = stages::CHAIN.iter().map(|r| r.name).collect();
        assert_eq!(
            names,
            ["target", "signal", "amp", "impair", "tape", "tv", "gun", "abl", "screen", "wear", "phosphor", "optics"]
        );
        check_chain(stages::CHAIN).expect("valid chain");
        let domains: Vec<(Domain, Domain)> = stages::CHAIN.iter().map(|r| (r.input, r.output)).collect();
        use Domain::*;
        assert_eq!(
            domains,
            [
                (Native, Native),
                (Native, Native),
                (Native, Amp),
                (Amp, Amp),
                (Amp, Amp),
                (Amp, Amp),
                (Amp, Amp),
                (Amp, Amp),
                (Amp, Tube),
                (Tube, Tube),
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

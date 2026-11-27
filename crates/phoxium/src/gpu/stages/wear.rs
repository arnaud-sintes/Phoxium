//! Slot `wear` (linear light, tube -> tube): the phosphor efficiency of a worn tube, tube-wide ageing (T-42) and
//! the burn-in wear map (T-43); QUESTIONS PHY-14, `shaders/wear.comp` and `shaders/wear_accumulate.comp`,
//! parameters `params/ageing.toml` and `params/burnin.toml`, oracles `crt_model::ageing` and `crt_model::burnin`.
//!
//! Between `screen` and `phosphor`: the input is the light of a fresh phosphor (beam current times mask), the
//! charge the burn-in map accumulates; the efficiency multiplies it before saturation and persistence (the
//! persistence is linear, so a worn phosphor's afterglow is scaled the same way).
//!
//! Per frame, when the slot runs: (1) the apply pass, y = x / (1 + c (Q_age + Q(p))) per channel, into the
//! slot's output; (2) with burn-in on and a charge to add, the accumulate pass over the map texels (Kahan
//! summation into two RGBA32F images); (3) on request, a copy of the map into a host buffer followed by a
//! `vkCmdSetEvent2`, polled without waiting ([`Wear::take_snapshot`]): the periodic save never stalls a frame.
//!
//! **Identity:** with ageing off (or no hours) and burn-in off ([`crate::ageing::wear_active`]) the stage says
//! [`Stage::is_identity`] and the pipeline skips the slot: no pass, no image, bit for bit unchanged.
//!
//! **The map** ([`MAP_SIZE`] texels, fixed, over the visible area of the tube in mm; over the whole tube image
//! when the tube is not placed): two RGBA32F storage images (charge, Kahan compensation) owned by the stage
//! (set 1), allocated when the slot first runs or when the CPU hands a map over, kept for the session (also
//! while burn-in is off, so turning it on again keeps the wear), never cleared by a source change (the wear is
//! the tube's, not the picture's). CPU operations (upload of a loaded map, reset to a new tube, blocking
//! readback at exit) idle the device and run in a one-time command buffer; they are rare.
//!
//! **Known deviation (review R-14, listed in DESIGN section 12):** the accumulation box-averages the light after
//! the mask over each texel of the fixed map, so with a resolved mask (and with the scanlines) the texel grid
//! beats against the triad and line pitches and a uniform field charges neighbouring texels unequally
//! (physically every stripe of one colour wears the same). Not fixed for now (opt-in feature); the fix would
//! accumulate the beam light before the mask, or the mask prefiltered.
//!
//! **Time base (review R-10):** the charge of a frame uses its measured period clamped to
//! [`crt_model::phosphor::MAX_FRAME_STEP_MS`] ([`crt_model::burnin::frame_hours`]), so the first frame after a
//! minimise or a stall never wears the map by the whole pause times the acceleration.
//!
//! The stage records its own barriers on the map: a memory barrier at the start of every run orders the
//! previous frame's accumulation and the one-time copies before this frame's reads and writes; one between
//! the two passes (the apply pass reads what the accumulation then writes).

use ash::vk;

use super::{binding, compute_pipeline, push, ComputePass, Domain, Registration, Stage, GROUP};
use crate::gpu::headless::{GpuImage, HostBuffer};
use crate::gpu::{barrier, vk_error, wait_fences, Context, FrameConstants, GpuError};
use crate::params::Params;
use crate::Res;

const APPLY_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/wear.comp.spv"));
const ACCUMULATE_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/wear_accumulate.comp.spv"));
/// The standard pass of the slot (its set layout is set 0 of both pipelines); never dispatched.
const COPY_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/resample.comp.spv"));

/// Size of the burn-in map in texels (width, height): fixed, independent of the host resolution (T-43). About
/// 0.6 mm per texel on a 20 in 4:3 tube (386 x 290 mm visible).
pub const MAP_SIZE: (u32, u32) = (640, 480);
const MAP_FORMAT: vk::Format = vk::Format::R32G32B32A32_SFLOAT;
const MAP_TEXELS: usize = MAP_SIZE.0 as usize * MAP_SIZE.1 as usize;
const MAP_BYTES: usize = MAP_TEXELS * 16;

/// Registration of the slot (see [`super::CHAIN`]).
pub(in crate::gpu) const SLOT: Registration = Registration {
    name: "wear",
    input: Domain::Tube,
    output: Domain::Tube,
    new,
};

fn new() -> Box<dyn Stage> {
    Box::<Wear>::default()
}

/// Push constants of `wear.comp` and `wear_accumulate.comp` (std430, 40 bytes, no padding).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(in crate::gpu) struct Push {
    /// rgb: Pfahnl constant per channel (per reference hour); a: tube-wide charge (reference hours).
    pub c_qage: [f32; 4],
    /// xy: host px of the map corner in the tube image; zw: host px per texel.
    pub place: [f32; 4],
    /// Reference hours per unit light added by this frame's accumulation.
    pub scale: f32,
    /// 1: the burn-in map is applied.
    pub use_map: u32,
}

/// The placement of the map over a tube image (`crt_model::burnin::placement`, in f32).
pub(in crate::gpu) fn map_place(fc: &FrameConstants, params: &Params) -> [f32; 4] {
    let tube = fc.domain_size(Domain::Tube);
    let visible = [
        f64::from(params.tube_visible_height_mm) * f64::from(params.tube_aspect),
        f64::from(params.tube_visible_height_mm),
    ];
    let p = crt_model::burnin::placement(
        [f64::from(tube.0), f64::from(tube.1)],
        f64::from(fc.px_per_mm),
        visible,
        [MAP_SIZE.0 as usize, MAP_SIZE.1 as usize],
    );
    [
        p.origin_px[0] as f32,
        p.origin_px[1] as f32,
        p.texel_px[0] as f32,
        p.texel_px[1] as f32,
    ]
}

/// Reference hours per unit light that one frame adds to the map: the frame's display hours
/// ([`crt_model::burnin::frame_hours`]: the measured period `frame_time_ms` clamped to
/// [`crt_model::phosphor::MAX_FRAME_STEP_MS`], times the acceleration; review R-10) plus the 'burn in now'
/// hours `extra_h`, times `per_light_hour`. The first frame after a minimise or a stall therefore wears like
/// a 250 ms frame at most, not like the whole pause times the acceleration.
fn charge_scale(frame_time_ms: f32, acceleration: f32, extra_h: f64, per_light_hour: f64) -> f64 {
    let frame_h = crt_model::burnin::frame_hours(f64::from(frame_time_ms), f64::from(acceleration));
    (frame_h + extra_h) * per_light_hour
}

/// The bits of every parameter the wear law depends on (cache key of the calibration).
fn law_key(p: &Params) -> [u32; 13] {
    let mut k = [0u32; 13];
    let vals = [
        p.ageing_rates_rgb[0],
        p.ageing_rates_rgb[1],
        p.ageing_rates_rgb[2],
        p.ageing_ref_loss,
        p.ageing_ref_hours,
        p.tube_red_xy[0],
        p.tube_red_xy[1],
        p.tube_green_xy[0],
        p.tube_green_xy[1],
        p.tube_blue_xy[0],
        p.tube_blue_xy[1],
        p.tube_white_xy[0],
        p.tube_white_xy[1],
    ];
    for (o, v) in k.iter_mut().zip(vals) {
        *o = v.to_bits();
    }
    k
}

/// The asynchronous map readback of the periodic save.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Snapshot {
    #[default]
    Idle,
    /// Asked for: the next frame that runs the slot copies the map.
    Requested,
    /// Recorded in a frame: the event tells when the copy is done.
    InFlight,
}

#[derive(Default)]
pub(in crate::gpu) struct Wear {
    pass: ComputePass,
    map_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    apply: vk::Pipeline,
    accumulate: vk::Pipeline,
    pool: vk::DescriptorPool,
    map_set: vk::DescriptorSet,
    /// Charge (rgb, reference hours) and its Kahan compensation, `MAP_SIZE`, layout `GENERAL` once allocated.
    sum: GpuImage,
    comp: GpuImage,
    allocated: bool,
    law: Option<([u32; 13], [f32; 3])>,
    push: Push,
    /// Decided by `prepare` for the frame being recorded.
    runs: bool,
    accumulate_now: bool,
    readback_now: bool,
    /// A map handed over by the CPU (rgb per texel, `MAP_SIZE`), uploaded by the next `prepare`.
    pending_upload: Option<Vec<f32>>,
    /// A new tube asked for: the map is cleared by the next `prepare`.
    pending_reset: bool,
    /// 'Burn in now': hours to add with the next accumulation, and those the recorded frame consumed.
    extra_hours: f64,
    extra_used: f64,
    readback: HostBuffer,
    event: vk::Event,
    snapshot: Snapshot,
}

impl Wear {
    /// Hands a map over (rgb per texel, `MAP_SIZE`); uploaded before the next frame.
    pub(in crate::gpu) fn set_map(&mut self, rgb: Vec<f32>) -> Res<()> {
        if rgb.len() != MAP_TEXELS * 3 {
            return Err(format!("wear map: {} values, {} expected", rgb.len(), MAP_TEXELS * 3).into());
        }
        self.pending_upload = Some(rgb);
        self.pending_reset = false;
        Ok(())
    }

    /// A new tube: the map is cleared before the next frame.
    pub(in crate::gpu) fn reset(&mut self) {
        self.pending_upload = None;
        self.pending_reset = true;
    }

    /// Adds `hours` (at real time, without the acceleration) of the next frame's picture to the map.
    pub(in crate::gpu) fn burn_in_now(&mut self, hours: f64) {
        if hours.is_finite() && hours > 0.0 {
            self.extra_hours += hours;
        }
    }

    /// Asks for an asynchronous copy of the map, made by the next frame that runs the slot.
    pub(in crate::gpu) fn request_snapshot(&mut self) {
        if self.snapshot == Snapshot::Idle {
            self.snapshot = Snapshot::Requested;
        }
    }

    /// The copy asked for by [`Wear::request_snapshot`] once the GPU made it (rgb per texel), never waiting.
    ///
    /// # Safety
    /// `ctx` is the pipeline's context.
    pub(in crate::gpu) unsafe fn take_snapshot(&mut self, ctx: &Context) -> Res<Option<Vec<f32>>> {
        if self.snapshot != Snapshot::InFlight || !ctx.device.get_event_status(self.event)? {
            return Ok(None);
        }
        let rgb = rgba_to_rgb(self.readback.mapped);
        ctx.device.reset_event(self.event)?;
        self.snapshot = Snapshot::Idle;
        Ok(Some(rgb))
    }

    /// The map now (rgb per texel), `None` before it exists: idles the device and copies it.
    ///
    /// # Safety
    /// `ctx` is the pipeline's context and no command buffer of the pipeline is recording.
    pub(in crate::gpu) unsafe fn read_now(&mut self, ctx: &Context) -> Res<Option<Vec<f32>>> {
        if let Some(rgb) = &self.pending_upload {
            return Ok(Some(rgb.clone()));
        }
        if self.pending_reset || !self.allocated {
            return Ok(self.pending_reset.then(|| vec![0.0; MAP_TEXELS * 3]));
        }
        let d = &ctx.device;
        d.device_wait_idle()?;
        let mut buf = HostBuffer::create(ctx, MAP_BYTES, vk::BufferUsageFlags::TRANSFER_DST)?;
        let r = one_shot(ctx, |cb| {
            d.cmd_copy_image_to_buffer(cb, self.sum.image, vk::ImageLayout::GENERAL, buf.buffer, &[map_copy()]);
            to_host(d, cb);
        });
        let out = r.map(|()| rgba_to_rgb(buf.mapped));
        buf.destroy(d);
        out.map(Some)
    }

    /// # Safety
    /// The GPU no longer uses the map images.
    unsafe fn free_map(&mut self, d: &ash::Device) {
        self.sum.destroy(d);
        self.comp.destroy(d);
        self.allocated = false;
    }

    /// Creates the two map images, cleared to 0 in layout `GENERAL`.
    ///
    /// # Safety
    /// `ctx` is live and the device idle.
    unsafe fn alloc_map(&mut self, ctx: &Context) -> Res<()> {
        let extent = vk::Extent2D {
            width: MAP_SIZE.0,
            height: MAP_SIZE.1,
        };
        let usage =
            vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST;
        self.sum = GpuImage::create(ctx, extent, MAP_FORMAT, usage)?;
        self.comp = GpuImage::create(ctx, extent, MAP_FORMAT, usage)?;
        let infos = [self.sum.view, self.comp.view].map(|v| {
            [vk::DescriptorImageInfo::default()
                .image_view(v)
                .image_layout(vk::ImageLayout::GENERAL)]
        });
        let writes: Vec<vk::WriteDescriptorSet> = infos
            .iter()
            .enumerate()
            .map(|(i, info)| {
                vk::WriteDescriptorSet::default()
                    .dst_set(self.map_set)
                    .dst_binding(i as u32)
                    .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                    .image_info(info)
            })
            .collect();
        ctx.device.update_descriptor_sets(&writes, &[]);
        let d = &ctx.device;
        let (sum, comp) = (self.sum.image, self.comp.image);
        one_shot(ctx, |cb| {
            for img in [sum, comp] {
                barrier(
                    d,
                    cb,
                    img,
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::GENERAL,
                    vk::PipelineStageFlags2::NONE,
                    vk::AccessFlags2::NONE,
                    vk::PipelineStageFlags2::CLEAR,
                    vk::AccessFlags2::TRANSFER_WRITE,
                );
            }
            clear(d, cb, &[sum, comp]);
        })?;
        self.allocated = true;
        Ok(())
    }

    /// Uploads `rgb` into the charge image and clears the compensation, or clears both (`None`).
    ///
    /// # Safety
    /// `ctx` is live, the map allocated and the device idle.
    unsafe fn write_map(&mut self, ctx: &Context, rgb: Option<&[f32]>) -> Res<()> {
        let d = &ctx.device;
        let (sum, comp) = (self.sum.image, self.comp.image);
        let Some(rgb) = rgb else {
            return one_shot(ctx, |cb| clear(d, cb, &[sum, comp]));
        };
        let mut staging = HostBuffer::create(ctx, MAP_BYTES, vk::BufferUsageFlags::TRANSFER_SRC)?;
        let dst = staging.mapped.cast::<f32>();
        for (t, px) in rgb.as_chunks::<3>().0.iter().enumerate() {
            for (c, v) in px.iter().enumerate() {
                dst.add(t * 4 + c).write(if v.is_finite() { v.max(0.0) } else { 0.0 });
            }
            dst.add(t * 4 + 3).write(0.0);
        }
        let r = one_shot(ctx, |cb| {
            clear(d, cb, &[comp]);
            d.cmd_copy_buffer_to_image(cb, staging.buffer, sum, vk::ImageLayout::GENERAL, &[map_copy()]);
        });
        staging.destroy(d);
        r
    }
}

/// The RGBA32F map in host memory as rgb per texel.
///
/// # Safety
/// `mapped` holds `MAP_BYTES` bytes of a finished copy, visible to the host.
unsafe fn rgba_to_rgb(mapped: *mut u8) -> Vec<f32> {
    let src = std::slice::from_raw_parts(mapped.cast::<f32>(), MAP_TEXELS * 4);
    let mut out = Vec::with_capacity(MAP_TEXELS * 3);
    for px in src.as_chunks::<4>().0.iter() {
        out.extend_from_slice(&px[..3]);
    }
    out
}

fn map_copy() -> vk::BufferImageCopy {
    vk::BufferImageCopy::default()
        .image_subresource(
            vk::ImageSubresourceLayers::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .layer_count(1),
        )
        .image_extent(vk::Extent3D {
            width: MAP_SIZE.0,
            height: MAP_SIZE.1,
            depth: 1,
        })
}

/// Clears `images` (layout `GENERAL`) to 0.
///
/// # Safety
/// `cb` is recording on `d`; the images are in layout `GENERAL` with `TRANSFER_DST` usage.
unsafe fn clear(d: &ash::Device, cb: vk::CommandBuffer, images: &[vk::Image]) {
    let range = [vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .level_count(1)
        .layer_count(1)];
    for &img in images {
        d.cmd_clear_color_image(
            cb,
            img,
            vk::ImageLayout::GENERAL,
            &vk::ClearColorValue { float32: [0.0; 4] },
            &range,
        );
    }
}

/// Transfer writes before host reads.
///
/// # Safety
/// `cb` is recording on `d`.
unsafe fn to_host(d: &ash::Device, cb: vk::CommandBuffer) {
    let m = [vk::MemoryBarrier2::default()
        .src_stage_mask(vk::PipelineStageFlags2::COPY)
        .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
        .dst_stage_mask(vk::PipelineStageFlags2::HOST)
        .dst_access_mask(vk::AccessFlags2::HOST_READ)];
    d.cmd_pipeline_barrier2(cb, &vk::DependencyInfo::default().memory_barriers(&m));
}

/// Records `f` into a one-time command buffer, submits it and waits for it (at most
/// [`crate::gpu::FENCE_TIMEOUT`]).
///
/// # Safety
/// `ctx` is live; the commands of `f` are valid on its queue.
unsafe fn one_shot(ctx: &Context, f: impl FnOnce(vk::CommandBuffer)) -> Res<()> {
    let d = &ctx.device;
    let pool = d.create_command_pool(
        &vk::CommandPoolCreateInfo::default()
            .queue_family_index(ctx.queue_family)
            .flags(vk::CommandPoolCreateFlags::TRANSIENT),
        None,
    )?;
    let r = (|| -> Res<()> {
        let cb = d.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::default()
                .command_pool(pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1),
        )?[0];
        d.begin_command_buffer(
            cb,
            &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        )?;
        f(cb);
        // Everything written here is visible to the frames that follow (the per-frame barrier of the stage
        // also covers it, through the submission order).
        let m = [vk::MemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::ALL_TRANSFER)
            .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
            .dst_access_mask(vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE)];
        d.cmd_pipeline_barrier2(cb, &vk::DependencyInfo::default().memory_barriers(&m));
        d.end_command_buffer(cb)?;
        let fence = d.create_fence(&vk::FenceCreateInfo::default(), None)?;
        let cbs = [cb];
        let submit = vk::SubmitInfo::default().command_buffers(&cbs);
        // R-01: a bounded wait; a GPU that does not finish in time is a lost device, never an endless wait.
        let done = d
            .queue_submit(ctx.queue, &[submit], fence)
            .map_err(|r| vk_error(r, "submitting a burn-in map transfer"))
            .and_then(|()| wait_fences(d, &[fence], "a burn-in map transfer"));
        if !hung(&done) {
            d.destroy_fence(fence, None);
        }
        done
    })();
    // A hung GPU may still use the command buffer and the fence: they are leaked rather than destroyed in use (the
    // device is lost anyway).
    if !hung(&r) {
        d.destroy_command_pool(pool, None);
    }
    r
}

/// True when `r` failed on a GPU that did not finish within [`crate::gpu::FENCE_TIMEOUT`].
fn hung(r: &Res<()>) -> bool {
    r.as_ref()
        .err()
        .and_then(|e| e.downcast_ref::<GpuError>())
        .is_some_and(|g| matches!(g, GpuError::DeviceLost { hung: true, .. }))
}

impl Stage for Wear {
    fn pass(&self) -> Option<&ComputePass> {
        Some(&self.pass)
    }

    unsafe fn create(&mut self, d: &ash::Device) -> Res<()> {
        self.pass.create(d, COPY_SPV, 0)?;
        let s = vk::ShaderStageFlags::COMPUTE;
        let bindings = [0, 1].map(|b| binding(b, vk::DescriptorType::STORAGE_IMAGE, s));
        self.map_layout =
            d.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings), None)?;
        let sets = [self.pass.set_layout, self.map_layout];
        let ranges = [vk::PushConstantRange::default()
            .stage_flags(s)
            .offset(0)
            .size(size_of::<Push>() as u32)];
        self.layout = d.create_pipeline_layout(
            &vk::PipelineLayoutCreateInfo::default()
                .set_layouts(&sets)
                .push_constant_ranges(&ranges),
            None,
        )?;
        self.apply = compute_pipeline(d, self.layout, APPLY_SPV)?;
        self.accumulate = compute_pipeline(d, self.layout, ACCUMULATE_SPV)?;
        let sizes = [vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::STORAGE_IMAGE)
            .descriptor_count(2)];
        self.pool = d.create_descriptor_pool(
            &vk::DescriptorPoolCreateInfo::default().max_sets(1).pool_sizes(&sizes),
            None,
        )?;
        let layouts = [self.map_layout];
        self.map_set = d.allocate_descriptor_sets(
            &vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(self.pool)
                .set_layouts(&layouts),
        )?[0];
        self.event = d.create_event(&vk::EventCreateInfo::default(), None)?;
        Ok(())
    }

    /// Ageing off (or no charge) and burn-in off: the efficiency is 1 everywhere and nothing accumulates.
    fn is_identity(&self, params: &Params, _fc: &FrameConstants) -> bool {
        !crate::ageing::wear_active(params)
    }

    unsafe fn prepare(&mut self, ctx: &Context, fc: &FrameConstants, params: &Params, runs: bool) -> Res<()> {
        let d = &ctx.device;
        self.runs = runs;
        self.accumulate_now = false;
        self.readback_now = false;
        self.extra_used = 0.0;
        // The map is needed when the slot runs, or to receive what the CPU handed over.
        let cpu_op = self.pending_upload.is_some() || self.pending_reset;
        if (runs || cpu_op) && !self.allocated {
            d.device_wait_idle()?;
            if let Err(e) = self.alloc_map(ctx) {
                self.free_map(d);
                return Err(e);
            }
            self.pending_reset = false; // a new map is already a new tube
        }
        if cpu_op {
            d.device_wait_idle()?;
            let upload = self.pending_upload.take();
            self.pending_reset = false;
            self.write_map(ctx, upload.as_deref())?;
        }
        if !runs {
            return Ok(());
        }
        let key = law_key(params);
        let c = match self.law {
            Some((k, c)) if k == key => c,
            _ => {
                let law = crate::ageing::wear_law(params);
                let c = law.c.map(|v| v as f32);
                self.law = Some((key, c));
                c
            }
        };
        let burn = params.burnin_enable;
        let per_light_hour = crate::ageing::charge_per_light_hour(params);
        let mut scale = 0.0;
        if burn {
            self.extra_used = std::mem::take(&mut self.extra_hours);
            scale = charge_scale(
                fc.upstream_ms(),
                params.burnin_acceleration,
                self.extra_used,
                per_light_hour,
            );
        }
        let scale = if scale.is_finite() { scale as f32 } else { 0.0 };
        self.push = Push {
            c_qage: [c[0], c[1], c[2], crate::ageing::age_charge(params) as f32],
            place: map_place(fc, params),
            scale,
            use_map: u32::from(burn),
        };
        self.accumulate_now = burn && scale > 0.0;
        if self.snapshot == Snapshot::Requested {
            if self.readback.buffer == vk::Buffer::null() {
                self.readback = HostBuffer::create(ctx, MAP_BYTES, vk::BufferUsageFlags::TRANSFER_DST)?;
            }
            self.readback_now = true;
            self.snapshot = Snapshot::InFlight;
        }
        Ok(())
    }

    fn abandon(&mut self) {
        // The frame is not submitted: its 'burn in now' hours and its readback are given back.
        self.extra_hours += std::mem::take(&mut self.extra_used);
        if self.readback_now {
            self.snapshot = Snapshot::Requested;
            self.readback_now = false;
        }
        self.accumulate_now = false;
    }

    unsafe fn record(&self, d: &ash::Device, cb: vk::CommandBuffer, set: vk::DescriptorSet, size: (u32, u32)) {
        let compute = vk::PipelineStageFlags2::COMPUTE_SHADER;
        let rw = vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::SHADER_STORAGE_WRITE;
        // The previous frame's accumulation (and the one-time copies) before this frame's map accesses.
        let m = [vk::MemoryBarrier2::default()
            .src_stage_mask(compute | vk::PipelineStageFlags2::ALL_TRANSFER)
            .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE | vk::AccessFlags2::TRANSFER_WRITE)
            .dst_stage_mask(compute | vk::PipelineStageFlags2::COPY)
            .dst_access_mask(rw | vk::AccessFlags2::TRANSFER_READ)];
        d.cmd_pipeline_barrier2(cb, &vk::DependencyInfo::default().memory_barriers(&m));
        d.cmd_bind_descriptor_sets(
            cb,
            vk::PipelineBindPoint::COMPUTE,
            self.layout,
            0,
            &[set, self.map_set],
            &[],
        );
        push(d, cb, self.layout, vk::ShaderStageFlags::COMPUTE, &self.push);
        d.cmd_bind_pipeline(cb, vk::PipelineBindPoint::COMPUTE, self.apply);
        d.cmd_dispatch(cb, size.0.div_ceil(GROUP), size.1.div_ceil(GROUP), 1);
        if self.accumulate_now {
            // The apply pass read the map that the accumulation now writes.
            let m = [vk::MemoryBarrier2::default()
                .src_stage_mask(compute)
                .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_READ)
                .dst_stage_mask(compute)
                .dst_access_mask(rw)];
            d.cmd_pipeline_barrier2(cb, &vk::DependencyInfo::default().memory_barriers(&m));
            d.cmd_bind_pipeline(cb, vk::PipelineBindPoint::COMPUTE, self.accumulate);
            d.cmd_dispatch(cb, MAP_SIZE.0.div_ceil(GROUP), MAP_SIZE.1.div_ceil(GROUP), 1);
        }
        if self.readback_now {
            let m = [vk::MemoryBarrier2::default()
                .src_stage_mask(compute)
                .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::COPY)
                .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)];
            d.cmd_pipeline_barrier2(cb, &vk::DependencyInfo::default().memory_barriers(&m));
            d.cmd_copy_image_to_buffer(
                cb,
                self.sum.image,
                vk::ImageLayout::GENERAL,
                self.readback.buffer,
                &[map_copy()],
            );
            // The copy made visible to the host, then the event, set once everything before it is done
            // (`vkCmdSetEvent2` may not name the host stage itself).
            to_host(d, cb);
            let e = [vk::MemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
            d.cmd_set_event2(cb, self.event, &vk::DependencyInfo::default().memory_barriers(&e));
        }
    }

    unsafe fn destroy(&mut self, d: &ash::Device) {
        self.free_map(d);
        self.readback.destroy(d);
        d.destroy_event(self.event, None);
        d.destroy_descriptor_pool(self.pool, None);
        d.destroy_pipeline(self.apply, None);
        d.destroy_pipeline(self.accumulate, None);
        d.destroy_pipeline_layout(self.layout, None);
        d.destroy_descriptor_set_layout(self.map_layout, None);
        self.pass.destroy(d);
        *self = Self::default();
    }

    fn wear(&mut self) -> Option<&mut Wear> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The push block is 40 bytes: c_qage at 0, place at 16, scale at 32, use_map at 36 (std430 of both shaders).
    #[test]
    fn push_layout_matches_the_shaders() {
        assert_eq!(size_of::<Push>(), 40);
        assert_eq!(std::mem::offset_of!(Push, place), 16);
        assert_eq!(std::mem::offset_of!(Push, scale), 32);
        assert_eq!(std::mem::offset_of!(Push, use_map), 36);
        for glsl in [
            include_str!("../../../../../shaders/wear.comp"),
            include_str!("../../../../../shaders/wear_accumulate.comp"),
        ] {
            for m in ["vec4 c_qage;", "vec4 place;", "float scale;", "uint use_map;"] {
                assert!(glsl.contains(m), "{m}");
            }
        }
    }

    /// Review R-10: the charge of the first frame after a 10 minute minimise (600000 ms) equals that of a
    /// 250 ms frame (the clamp), not 600 h at an acceleration of 3600; a normal frame is unchanged, the
    /// first frame (0 ms) adds only the 'burn in now' hours.
    #[test]
    fn long_frame_gap_does_not_wear_by_hours() {
        let per = 2.0;
        let gap = charge_scale(600_000.0, 3600.0, 0.0, per);
        assert_eq!(gap, charge_scale(250.0, 3600.0, 0.0, per));
        assert!((gap - 0.25 * 3600.0 / 3600.0 * per).abs() < 1e-9, "{gap}");
        let frame = charge_scale(1000.0 / 60.0, 3600.0, 0.0, per);
        assert!((frame - per / 60.0).abs() < 1e-6, "{frame}");
        assert_eq!(charge_scale(0.0, 3600.0, 0.5, per), 0.5 * per);
        assert_eq!(charge_scale(f32::NAN, f32::INFINITY, 0.0, per), 0.0);
    }

    /// Unplaced tube: the map covers the tube image; placed: the visible mm, centred.
    #[test]
    fn map_placement() {
        let fc = FrameConstants::new((128, 96), (128, 96), false);
        assert_eq!(map_place(&fc, &Params::default()), [0.0, 0.0, 0.2, 0.2]);
        let mut fc = FrameConstants::new((320, 240), (1000, 800), false);
        fc.tube_size = [1000, 800];
        fc.px_per_mm = 2.0;
        let p = Params {
            tube_visible_height_mm: 300.0,
            tube_aspect: 4.0 / 3.0,
            ..Params::default()
        };
        // 400 x 300 mm at 2 px per mm in a 1000 x 800 image: corner (100, 100), 1.25 px per texel.
        let place = map_place(&fc, &p);
        for (got, want) in place.iter().zip([100.0, 100.0, 1.25, 1.25]) {
            assert!((got - want).abs() < 1e-4, "{place:?}");
        }
    }
}

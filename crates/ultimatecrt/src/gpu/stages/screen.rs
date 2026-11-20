//! Slot `screen` (amp -> tube): light emitted by the phosphor screen at each host pixel of the tube
//! rectangle, product form L_c = mask_eval() x beam_eval() (`shaders/screen.comp`). The beam
//! (`shaders/include/beam.glsl`, T-36a, T-36b, T-36c), the mask (`shaders/include/mask.glsl`, T-39a, T-39b)
//! and the geometry (`shaders/include/geometry.glsl`, T-37) are evaluated per host pixel; the geometry is an
//! identity at its default parameters. The shader's workgroup is one beam tile (`BEAM_TILE_W` x
//! `BEAM_TILE_H`, T-36c: the standard quality shares the line profiles of the tile in shared memory).
//!
//! T-37, `raster_scale` (ABL raster breathing, T-35): it is a function of the ABL state that the GPU writes
//! during the frame (the `abl` reduce pass), after the CPU has written `FrameConstants`, so it cannot be a
//! frame constant. The least invasive correct option is to read the state where it lives: this stage binds the
//! `abl` slot's state buffer read-only as set 1 (`ABL_STATE_SET` of `abl.glsl`), handed over by the pipeline
//! once every stage is created ([`Stage::link_abl_state`]). No copy, no extra pass, no CPU readback; the
//! `abl` stage ends its recording with a barrier from its reduce write to later compute reads. Until linked
//! (a chain without `abl`), set 1 points at a small buffer of this stage that is never read in a meaningful
//! way: the shader reads the state only when `abl_breathing` is not 0 and the `abl` slot runs in this frame
//! (T-37 review: the pipeline tells the stage, [`Stage::set_abl_runs`], and the stage passes it as a push
//! constant; a disabled or skipped `abl` gives `raster_scale` 1, never a stale state), and ignores a scale
//! outside [0.5, 2].
//!
//! # Warp path (T-91)
//!
//! A non-separable geometry (curved faceplate, pincushion, trapezoid, rotation) with the standard quality is
//! recorded as two passes instead of `screen.comp` (`shaders/include/beam_warp.glsl` has the design and the
//! error bound): `screen_lines.comp` writes the standard beam's pass-1 entries of every raster line at raster
//! columns on a uniform grid into the stage's line images, `screen_warp.comp` evaluates each host pixel from
//! them through the geometry, then the mask and the opening. The decision is the CPU's, per frame, from the
//! parameters and constants ([`warp_plan`], in [`Stage::prepare`]): the identity and the separable geometries
//! (margin, overscan, `raster_scale`, convergence) keep the single `screen.comp` pass, bit for bit and at the
//! same cost, and nothing of the warp path is recorded or allocated for them. This is the per-stage form of
//! `Stage::is_identity`: `screen` changes domain, so the pipeline never skips it, and a slot `warp` between
//! beam and mask would need a raster-space domain the image contract does not have (and would cut the slot
//! `screen` that every test and timing names). So the slot stays one slot; like `abl` it records two passes and
//! the barrier between them, and like `phosphor` it owns images that are not chain images (the line images,
//! five `rgba16f` storage images in set 2, allocated by `prepare` on first use and on a size change, freed while
//! the warp path is off) and records its own barriers on them. The timing segment `screen` covers both passes.
//!
//! # Interlace (T-32)
//!
//! The field raster of `beam_raster` (`shaders/include/beam.glsl`, `interlace.glsl`) is compiled behind the
//! specialization constant 0 (`UCRT_INTERLACE`). The pipelines created in [`Stage::create`] leave it false, so with
//! `interlace_mode` off the driver compiles the progressive raster alone and the picture is bit-identical to the
//! build without the module (a branch on the parameter alone changed the driver's code enough to flip a few FP16
//! results, about 12 values of 3.7 million). The variants with the constant true (the three shaders) are created
//! by `prepare` the first time a frame needs them and kept until `destroy`.

use std::cell::Cell;

use ash::vk;
use crt_model::beam::SpotGrowth;

use super::{
    binding, compute_pipeline, push, shader_module, standard_set_layout, ComputePass, Domain, Registration, Stage,
};
use crate::gpu::headless::GpuImage;
use crate::gpu::{barrier, Context, FrameConstants};
use crate::params::{Params, BEAM_QUALITY_STANDARD, GEOMETRY_CURVATURE_FLAT, INTERLACE_MODE_OFF};
use crate::quality::{self, Tier};
use crate::Res;

const SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/screen.comp.spv"));
/// Warp path, pass 1: the line images (`shaders/screen_lines.comp`).
const LINES_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/screen_lines.comp.spv"));
/// Warp path, pass 2: host pixels from the line images, mask, opening (`shaders/screen_warp.comp`).
const WARP_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/screen_warp.comp.spv"));

/// Workgroup size of `screen.comp`: `BEAM_TILE_W`, `BEAM_TILE_H` of `shaders/include/beam.glsl`.
const SCREEN_GROUP: (u32, u32) = (8, 32);
/// Workgroup sizes of `screen_lines.comp` and `screen_warp.comp`.
const LINES_GROUP: (u32, u32) = (64, 1);
const WARP_GROUP: (u32, u32) = (8, 8);

/// Size of the ABL state (`AblState` of `abl.glsl`: four floats).
const STATE_BYTES: u64 = 16;

/// Line images of the warp path (`beam_warp.glsl` set 2: core sum h, sum h sigma^2, sum h sigma^4; tail sum
/// h, sum h sigma^2).
const LINE_IMAGES: usize = 5;
const LINE_FORMAT: vk::Format = vk::Format::R16G16B16A16_SFLOAT;

/// Smallest spot sigma at peak current, in line-image grid steps, for the warp path (`beam_warp.glsl`
/// Sampling: the Catmull-Rom error at 1.2 steps is about 1.2 % of white at 6 px per line, full current, no
/// growth; tests/stage_beam_warp.rs).
pub const WARP_SIGMA_STEPS: f64 = 1.2;
/// Grid steps per raster px the warp path may use (`ox`); a spot below `WARP_SIGMA_STEPS / 4` raster px keeps
/// the reference form.
const WARP_OVERSAMPLING: [u32; 3] = [1, 2, 4];

/// Registration of the slot (see [`super::CHAIN`]).
pub(in crate::gpu) const SLOT: Registration = Registration {
    name: "screen",
    input: Domain::Amp,
    output: Domain::Tube,
    new,
};

fn new() -> Box<dyn Stage> {
    Box::<Screen>::default()
}

/// The warp path of a frame ([`warp_plan`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WarpPlan {
    /// Line-image columns per raster px.
    pub ox: u32,
    /// Raster px of the line images beyond the tube image on each side (the spot's reach, the convergence
    /// and two Catmull-Rom taps).
    pub pad: i32,
    /// Size of the line images: columns, raster lines.
    pub size: (u32, u32),
}

/// Push constants of `screen_lines.comp` and `screen_warp.comp` (`ScreenPush`, 12 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct WarpPush {
    abl_runs: u32,
    ox: u32,
    pad: i32,
}

/// True when the host -> raster map is separable per axis (`geometry_separable` of `geometry.glsl`, the
/// same test on the same parameters): the single `screen.comp` pass applies.
fn separable(p: &Params) -> bool {
    (p.geometry_curvature == GEOMETRY_CURVATURE_FLAT || p.geometry_radius_mm <= 0.0 || p.geometry_radius_mm.is_nan())
        && p.geometry_pincushion == 0.0
        && p.geometry_trapezoid == 0.0
        && p.geometry_rotation_deg == 0.0
}

/// Lines of the beam raster (`beam_raster` of `beam.glsl`, the same f32 arithmetic): native rows times the
/// line multiplier round(raster height / (pitch x rows)), clamped to [1, 16].
fn raster_lines(fc: &FrameConstants) -> u32 {
    let pitch = fc.line_pitch_mm * fc.px_per_mm;
    let h = (fc.tube_size[1] - 2 * fc.raster_margin[1]) as f32;
    let rows = fc.native_size[1].max(1);
    let mult = (h / (pitch * rows as f32)).round().clamp(1.0, 16.0) as i32;
    (rows * mult).max(1) as u32
}

/// The warp path for these constants and parameters, `None` when `screen.comp` alone applies: the tube is
/// placed, the quality is `standard`, the geometry is not separable, and the spot sigma at peak current (in
/// raster px, overscan included) is at least [`WARP_SIGMA_STEPS`] grid steps at some `ox` of 1, 2 or 4 (the
/// smallest is taken). Below 0.3 raster px the standard quality keeps the per-pixel reference form.
pub fn warp_plan(fc: &FrameConstants, params: &Params) -> Option<WarpPlan> {
    let positive = |v: f32| v > 0.0; // false for NaN
    let sep = separable(params);
    // T-48: the `exact` tier keeps the tile path of `screen.comp` for the separable geometries (the output before
    // the tiers, bit for bit); the other tiers take the line-image path for them too (2.5 times cheaper).
    if !positive(fc.px_per_mm)
        || !positive(fc.line_pitch_mm)
        || params.beam_quality != BEAM_QUALITY_STANDARD
        || (sep && quality::tier(params) == Tier::Exact)
    {
        return None;
    }
    let ppm = f64::from(fc.px_per_mm);
    let growth = SpotGrowth {
        fwhm_mid: f64::from(params.beam_fwhm_mid_mm),
        fwhm_peak: f64::from(params.beam_fwhm_peak_mm),
        n: f64::from(params.beam_growth_exponent),
    };
    // Raster px per host px of the size step, raster_scale (GPU state, within [0.95, 1.05] for the breathing
    // range) taken as 1 for the sampling rule and as up to 1.1 for the reach.
    let im = |ov: f32| 1.0 / f64::from(1.0 + ov).max(0.5);
    let sigma_peak = growth.fwhm(1.0) / 2.354_820_045 * ppm * im(params.geometry_overscan_x);
    // A separable geometry whose raster is not scaled (no overscan, no ABL breathing: raster px = host px) puts
    // every host pixel's footprint on a grid column at ox 1 (Catmull-Rom weights (0, 1, 0, 0)): no interpolation,
    // whatever the spot size, so the line images hold exactly the tile path's entries (to their FP16 storage).
    let aligned =
        sep && params.geometry_overscan_x == 0.0 && params.geometry_overscan_y == 0.0 && params.abl_breathing == 0.0;
    let steps = quality::warp_sigma_steps(params, WARP_SIGMA_STEPS);
    let ox = if aligned {
        1
    } else {
        WARP_OVERSAMPLING
            .into_iter()
            .find(|&o| sigma_peak * f64::from(o) >= steps)?
    };
    // Reach of pass 1 (beam_warp.glsl): 4 sigma at twice the peak current (the tail's when on) plus the largest
    // misconvergence, in raster px, as the skirt margin (FrameConstants::with_skirt_margin).
    let mut sigma_mm = growth.fwhm(2.0).max(growth.fwhm(0.0)) / 2.354_820_045;
    if params.beam_tail_weight > 0.0 {
        sigma_mm *= f64::from(params.beam_tail_sigma_ratio).max(1.0);
    }
    let conv = |s: f32, d: f32, r: f32| f64::from(s.abs().max(d.abs()) + r.abs());
    let conv_mm = conv(
        params.geometry_conv_red_dx_mm,
        params.geometry_conv_red_dy_mm,
        params.geometry_conv_red_radial_mm,
    )
    .max(conv(
        params.geometry_conv_green_dx_mm,
        params.geometry_conv_green_dy_mm,
        params.geometry_conv_green_radial_mm,
    ))
    .max(conv(
        params.geometry_conv_blue_dx_mm,
        params.geometry_conv_blue_dy_mm,
        params.geometry_conv_blue_radial_mm,
    ));
    let im_max = 1.1 * im(params.geometry_overscan_x.min(params.geometry_overscan_y));
    let reach = (4.0 * sigma_mm + conv_mm) * ppm * im_max;
    if !reach.is_finite() {
        return None;
    }
    let pad = reach.ceil().min(4096.0) as i32 + 3;
    let cols = (fc.tube_size[0].max(1) + 2 * pad - 1) as u32 * ox + 1;
    Some(WarpPlan {
        ox,
        pad,
        size: (cols, raster_lines(fc)),
    })
}

/// Which specialized variant of the three pipelines a frame needs: the interlace raster (T-32, constant 0
/// `UCRT_INTERLACE` of `shaders/include/interlace.glsl`) and the beam reach of the `low` quality tier (T-48,
/// constant 1 `BEAM_REACH_SIGMAS` of `shaders/include/beam.glsl`, 3 instead of 4). The plain pipelines (neither)
/// are created unspecialized, so the progressive full-reach code is compiled exactly as before the constants.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct VariantKey {
    interlace: bool,
    short_reach: bool,
}

/// A variant of a pipeline: `spv` with the specialization constants of `key`.
///
/// # Safety
/// As [`compute_pipeline`].
unsafe fn variant_pipeline(
    d: &ash::Device,
    layout: vk::PipelineLayout,
    spv: &[u8],
    key: VariantKey,
) -> Res<vk::Pipeline> {
    let module = shader_module(d, spv)?;
    let interlace = if key.interlace { vk::TRUE } else { vk::FALSE };
    let reach: f32 = if key.short_reach { 3.0 } else { 4.0 };
    let mut data = [0u8; 8];
    data[..4].copy_from_slice(&interlace.to_ne_bytes());
    data[4..].copy_from_slice(&reach.to_ne_bytes());
    let mut entries = vec![vk::SpecializationMapEntry::default()
        .constant_id(0)
        .offset(0)
        .size(size_of::<vk::Bool32>())];
    if key.short_reach {
        entries.push(
            vk::SpecializationMapEntry::default()
                .constant_id(1)
                .offset(4)
                .size(size_of::<f32>()),
        );
    }
    let spec = vk::SpecializationInfo::default().map_entries(&entries).data(&data);
    let info = vk::ComputePipelineCreateInfo::default()
        .stage(
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::COMPUTE)
                .module(module)
                .name(c"main")
                .specialization_info(&spec),
        )
        .layout(layout);
    let result = d.create_compute_pipelines(vk::PipelineCache::null(), &[info], None);
    d.destroy_shader_module(module, None);
    Ok(result.map_err(|(_, e)| e)?[0])
}

/// A specialized variant of the stage's three pipelines ([`VariantKey`]).
#[derive(Clone, Copy)]
struct VariantPipelines {
    screen: vk::Pipeline,
    lines: vk::Pipeline,
    warp: vk::Pipeline,
}

#[derive(Default)]
struct Screen {
    /// T-32, T-48: the specialized variants (interlace, short reach), created on first use; the variant the
    /// frame being recorded uses (the default key: the plain pipelines).
    variant_pipelines: Vec<(VariantKey, VariantPipelines)>,
    variant: VariantKey,
    /// The pass; its pipeline layout has the standard set 0 and the ABL state set 1.
    pass: ComputePass,
    state_layout: vk::DescriptorSetLayout,
    pool: vk::DescriptorPool,
    state_set: vk::DescriptorSet,
    /// Stand-in for the ABL state until [`Stage::link_abl_state`].
    fallback: vk::Buffer,
    memory: vk::DeviceMemory,
    /// The `abl` slot runs in the frame being recorded (push constant `abl_runs` of `screen.comp`).
    abl_runs: Cell<bool>,
    /// Warp path: set 2 (the line images), the layout of both passes (sets 0, 1, 2 and [`WarpPush`]), the
    /// two pipelines, the set and the images.
    lines_layout: vk::DescriptorSetLayout,
    warp_layout: vk::PipelineLayout,
    lines_pipeline: vk::Pipeline,
    warp_pipeline: vk::Pipeline,
    lines_set: vk::DescriptorSet,
    lines: [GpuImage; LINE_IMAGES],
    /// Size of the line images, (0, 0) when none.
    lines_size: (u32, u32),
    /// Decided by `prepare` for the frame being recorded: the warp path, or `None` for `screen.comp`.
    warp: Option<WarpPlan>,
    /// The line images are new (layout `UNDEFINED`): the next recorded warp frame transitions them.
    fresh: bool,
    transition: bool,
}

impl Screen {
    /// Points set 1 at `buffer`.
    ///
    /// # Safety
    /// `buffer` is a live buffer of `d` of at least [`STATE_BYTES`], usable as a storage buffer.
    unsafe fn bind_state(&self, d: &ash::Device, buffer: vk::Buffer) {
        let info = [vk::DescriptorBufferInfo::default().buffer(buffer).range(STATE_BYTES)];
        d.update_descriptor_sets(
            &[vk::WriteDescriptorSet::default()
                .dst_set(self.state_set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&info)],
            &[],
        );
    }

    /// # Safety
    /// The GPU no longer uses the line images.
    unsafe fn free_lines(&mut self, d: &ash::Device) {
        for img in &mut self.lines {
            img.destroy(d);
        }
        self.lines_size = (0, 0);
    }

    /// # Safety
    /// `ctx` is live, the old images are freed and set 2 is not in use.
    unsafe fn alloc_lines(&mut self, ctx: &Context, size: (u32, u32)) -> Res<()> {
        let extent = vk::Extent2D {
            width: size.0,
            height: size.1,
        };
        for img in &mut self.lines {
            *img = GpuImage::create(ctx, extent, LINE_FORMAT, vk::ImageUsageFlags::STORAGE)?;
        }
        let infos: Vec<[vk::DescriptorImageInfo; 1]> = self
            .lines
            .iter()
            .map(|img| {
                [vk::DescriptorImageInfo::default()
                    .image_view(img.view)
                    .image_layout(vk::ImageLayout::GENERAL)]
            })
            .collect();
        let writes: Vec<vk::WriteDescriptorSet> = infos
            .iter()
            .enumerate()
            .map(|(i, info)| {
                vk::WriteDescriptorSet::default()
                    .dst_set(self.lines_set)
                    .dst_binding(i as u32)
                    .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                    .image_info(info)
            })
            .collect();
        ctx.device.update_descriptor_sets(&writes, &[]);
        self.lines_size = size;
        self.fresh = true;
        Ok(())
    }

    /// Records the warp path's two passes over a tube image of `size`.
    ///
    /// # Safety
    /// As [`Stage::record`]; the line images are allocated with `plan.size`.
    unsafe fn record_warp(
        &self,
        d: &ash::Device,
        cb: vk::CommandBuffer,
        set: vk::DescriptorSet,
        size: (u32, u32),
        plan: WarpPlan,
    ) {
        let compute = vk::PipelineStageFlags2::COMPUTE_SHADER;
        let read = vk::AccessFlags2::SHADER_STORAGE_READ;
        let write = vk::AccessFlags2::SHADER_STORAGE_WRITE;
        if self.transition {
            for img in &self.lines {
                barrier(
                    d,
                    cb,
                    img.image,
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::GENERAL,
                    vk::PipelineStageFlags2::NONE,
                    vk::AccessFlags2::NONE,
                    compute,
                    write,
                );
            }
        } else {
            // The previous frame's pass 1 (writes) and pass 2 (reads) of the line images before this pass 1.
            let m = [vk::MemoryBarrier2::default()
                .src_stage_mask(compute)
                .src_access_mask(write)
                .dst_stage_mask(compute)
                .dst_access_mask(write)];
            d.cmd_pipeline_barrier2(cb, &vk::DependencyInfo::default().memory_barriers(&m));
        }
        let bind = vk::PipelineBindPoint::COMPUTE;
        let sets = [set, self.state_set, self.lines_set];
        let pc = WarpPush {
            abl_runs: u32::from(self.abl_runs.get()),
            ox: plan.ox,
            pad: plan.pad,
        };
        let (lines_pipeline, warp_pipeline) = match self.variants() {
            Some(v) => (v.lines, v.warp),
            None => (self.lines_pipeline, self.warp_pipeline),
        };
        d.cmd_bind_pipeline(cb, bind, lines_pipeline);
        d.cmd_bind_descriptor_sets(cb, bind, self.warp_layout, 0, &sets, &[]);
        push(d, cb, self.warp_layout, vk::ShaderStageFlags::COMPUTE, &pc);
        d.cmd_dispatch(
            cb,
            plan.size.0.div_ceil(LINES_GROUP.0),
            plan.size.1.div_ceil(LINES_GROUP.1),
            1,
        );
        // Pass 1's line images before pass 2 reads them.
        let m = [vk::MemoryBarrier2::default()
            .src_stage_mask(compute)
            .src_access_mask(write)
            .dst_stage_mask(compute)
            .dst_access_mask(read)];
        d.cmd_pipeline_barrier2(cb, &vk::DependencyInfo::default().memory_barriers(&m));
        // Same layout: the sets and push constants stay bound.
        d.cmd_bind_pipeline(cb, bind, warp_pipeline);
        d.cmd_dispatch(cb, size.0.div_ceil(WARP_GROUP.0), size.1.div_ceil(WARP_GROUP.1), 1);
    }

    /// The specialized variant the frame being recorded uses (T-32 fields, T-48 low tier), `None` for the plain
    /// pipelines.
    fn variants(&self) -> Option<VariantPipelines> {
        if self.variant == VariantKey::default() {
            return None;
        }
        self.variant_pipelines
            .iter()
            .find(|(k, _)| *k == self.variant)
            .map(|(_, v)| *v)
    }
}

impl Stage for Screen {
    fn pass(&self) -> Option<&ComputePass> {
        Some(&self.pass)
    }

    unsafe fn create(&mut self, d: &ash::Device) -> Res<()> {
        let s = vk::ShaderStageFlags::COMPUTE;
        self.pass.set_layout = standard_set_layout(d)?;
        let state = [binding(0, vk::DescriptorType::STORAGE_BUFFER, s)];
        self.state_layout =
            d.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&state), None)?;
        let set_layouts = [self.pass.set_layout, self.state_layout];
        let push = [vk::PushConstantRange::default().stage_flags(s).offset(0).size(4)];
        self.pass.layout = d.create_pipeline_layout(
            &vk::PipelineLayoutCreateInfo::default()
                .set_layouts(&set_layouts)
                .push_constant_ranges(&push),
            None,
        )?;
        self.pass.pipeline = compute_pipeline(d, self.pass.layout, SPV)?;

        // Warp path (T-91): set 2 and the layout of its two passes.
        let lines: Vec<_> = (0..LINE_IMAGES as u32)
            .map(|b| binding(b, vk::DescriptorType::STORAGE_IMAGE, s))
            .collect();
        self.lines_layout =
            d.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&lines), None)?;
        let warp_sets = [self.pass.set_layout, self.state_layout, self.lines_layout];
        let warp_push = [vk::PushConstantRange::default()
            .stage_flags(s)
            .offset(0)
            .size(size_of::<WarpPush>() as u32)];
        self.warp_layout = d.create_pipeline_layout(
            &vk::PipelineLayoutCreateInfo::default()
                .set_layouts(&warp_sets)
                .push_constant_ranges(&warp_push),
            None,
        )?;
        self.lines_pipeline = compute_pipeline(d, self.warp_layout, LINES_SPV)?;
        self.warp_pipeline = compute_pipeline(d, self.warp_layout, WARP_SPV)?;

        self.fallback = d.create_buffer(
            &vk::BufferCreateInfo::default()
                .size(STATE_BYTES)
                .usage(vk::BufferUsageFlags::STORAGE_BUFFER)
                .sharing_mode(vk::SharingMode::EXCLUSIVE),
            None,
        )?;
        let req = d.get_buffer_memory_requirements(self.fallback);
        if req.memory_type_bits == 0 {
            return Err("screen: no memory type for the ABL state stand-in".into());
        }
        self.memory = d.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(req.size)
                .memory_type_index(req.memory_type_bits.trailing_zeros()),
            None,
        )?;
        d.bind_buffer_memory(self.fallback, self.memory, 0)?;

        let sizes = [
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::STORAGE_BUFFER)
                .descriptor_count(1),
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::STORAGE_IMAGE)
                .descriptor_count(LINE_IMAGES as u32),
        ];
        self.pool = d.create_descriptor_pool(
            &vk::DescriptorPoolCreateInfo::default().max_sets(2).pool_sizes(&sizes),
            None,
        )?;
        let layouts = [self.state_layout, self.lines_layout];
        let sets = d.allocate_descriptor_sets(
            &vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(self.pool)
                .set_layouts(&layouts),
        )?;
        self.state_set = sets[0];
        self.lines_set = sets[1];
        self.bind_state(d, self.fallback);
        Ok(())
    }

    unsafe fn link_abl_state(&mut self, d: &ash::Device, buffer: vk::Buffer) {
        self.bind_state(d, buffer);
    }

    fn set_abl_runs(&self, runs: bool) {
        self.abl_runs.set(runs);
    }

    unsafe fn prepare(&mut self, ctx: &Context, fc: &FrameConstants, params: &Params, runs: bool) -> Res<()> {
        self.transition = false;
        // T-32, T-48: the specialized variants of the pipelines (fields, the low tier's reach), created the first
        // time a frame needs them.
        self.variant = VariantKey {
            interlace: runs && params.interlace_mode != INTERLACE_MODE_OFF,
            short_reach: runs && quality::beam_reach_sigmas(params) < 4.0,
        };
        let key = self.variant;
        if key != VariantKey::default() && !self.variant_pipelines.iter().any(|(k, _)| *k == key) {
            let d = &ctx.device;
            let v = VariantPipelines {
                screen: variant_pipeline(d, self.pass.layout, SPV, key)?,
                lines: variant_pipeline(d, self.warp_layout, LINES_SPV, key)?,
                warp: variant_pipeline(d, self.warp_layout, WARP_SPV, key)?,
            };
            self.variant_pipelines.push((key, v));
        }
        self.warp = if runs { warp_plan(fc, params) } else { None };
        match self.warp {
            None => {
                if self.lines_size != (0, 0) {
                    ctx.device.device_wait_idle()?;
                    self.free_lines(&ctx.device);
                }
            }
            Some(plan) => {
                if plan.size != self.lines_size {
                    ctx.device.device_wait_idle()?;
                    self.free_lines(&ctx.device);
                    self.alloc_lines(ctx, plan.size)?;
                }
                self.transition = std::mem::take(&mut self.fresh);
            }
        }
        Ok(())
    }

    fn abandon(&mut self) {
        // The abandoned frame did not transition the new line images: the next warp frame does it.
        self.fresh |= self.transition;
        self.transition = false;
    }

    unsafe fn record(&self, d: &ash::Device, cb: vk::CommandBuffer, set: vk::DescriptorSet, size: (u32, u32)) {
        if let Some(plan) = self.warp {
            self.record_warp(d, cb, set, size, plan);
            return;
        }
        // As `ComputePass::dispatch_group` with the tile workgroup, plus the ABL state set 1.
        let sets = [set, self.state_set];
        let bind = vk::PipelineBindPoint::COMPUTE;
        let pipeline = self.variants().map_or(self.pass.pipeline, |v| v.screen);
        d.cmd_bind_pipeline(cb, bind, pipeline);
        d.cmd_bind_descriptor_sets(cb, bind, self.pass.layout, 0, &sets, &[]);
        let runs = u32::from(self.abl_runs.get());
        d.cmd_push_constants(
            cb,
            self.pass.layout,
            vk::ShaderStageFlags::COMPUTE,
            0,
            &runs.to_ne_bytes(),
        );
        d.cmd_dispatch(cb, size.0.div_ceil(SCREEN_GROUP.0), size.1.div_ceil(SCREEN_GROUP.1), 1);
    }

    unsafe fn destroy(&mut self, d: &ash::Device) {
        self.free_lines(d);
        for (_, v) in self.variant_pipelines.drain(..) {
            d.destroy_pipeline(v.screen, None);
            d.destroy_pipeline(v.lines, None);
            d.destroy_pipeline(v.warp, None);
        }
        d.destroy_pipeline(self.lines_pipeline, None);
        d.destroy_pipeline(self.warp_pipeline, None);
        d.destroy_pipeline_layout(self.warp_layout, None);
        d.destroy_descriptor_set_layout(self.lines_layout, None);
        d.destroy_descriptor_pool(self.pool, None);
        d.destroy_descriptor_set_layout(self.state_layout, None);
        d.destroy_buffer(self.fallback, None);
        d.free_memory(self.memory, None);
        self.pass.destroy(d);
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{GEOMETRY_CURVATURE_CYLINDRICAL, GEOMETRY_CURVATURE_SPHERICAL};

    /// The push block is 12 bytes: abl_runs, ox, pad (std430 of the two warp shaders).
    #[test]
    fn warp_push_layout_matches_the_shaders() {
        assert_eq!(size_of::<WarpPush>(), 12);
        for glsl in [
            include_str!("../../../../../shaders/screen_lines.comp"),
            include_str!("../../../../../shaders/screen_warp.comp"),
        ] {
            // Independent of the checkout's line endings (a CRLF checkout of the shader sources).
            let glsl = glsl.replace("\r\n", "\n");
            assert!(glsl.contains("uint abl_runs;\n    uint ox;\n    int pad;"));
        }
    }

    /// The CPU's separability test is the shader's: in the `exact` tier the warp path is chosen exactly for the
    /// non-separable geometries with the standard quality, never for the identity or the separable ones (T-48:
    /// the other tiers take the line-image path for those too, on the aligned grid ox 1 when the raster is not
    /// scaled), and the oversampling follows the spot (PVM-20M4U at 1440 lines: sigma 1.69 px at peak, ox 1;
    /// FWHM 0.6 mm at 2 px per mm: sigma 0.51 px, ox 4 (0.51 x 2 = 1.02 < 1.2); FWHM 0.1 mm at 2 px per mm:
    /// 0.085 px, reference).
    #[test]
    fn warp_plan_follows_the_geometry_and_the_spot() {
        let mut fc = FrameConstants::new((320, 240), (1920, 1440), false);
        fc.px_per_mm = 1440.0 / 290.0;
        fc.line_pitch_mm = 290.0 / 240.0;
        let exact = Params {
            quality_tier: crate::params::QUALITY_TIER_EXACT,
            ..Params::default()
        };
        assert_eq!(warp_plan(&fc, &exact), None, "identity");
        let separable = [
            Params {
                geometry_overscan_x: 0.05,
                geometry_conv_red_radial_mm: 1.0,
                ..exact
            },
            Params {
                geometry_radius_mm: 900.0,
                ..exact
            },
        ];
        for p in separable {
            assert_eq!(warp_plan(&fc, &p), None, "separable");
        }
        // Standard tier: the line-image path for the identity (aligned, ox 1 whatever the spot) and for the
        // separable geometries (the sampling rule where the raster is scaled).
        let d = Params::default();
        let tiny = Params {
            beam_fwhm_mid_mm: 0.05,
            beam_fwhm_peak_mm: 0.05,
            ..d
        };
        assert_eq!(warp_plan(&fc, &d).map(|w| w.ox), Some(1), "identity, standard");
        assert_eq!(warp_plan(&fc, &tiny).map(|w| w.ox), Some(1), "aligned grid, tiny spot");
        for p in separable {
            let p = Params {
                quality_tier: d.quality_tier,
                ..p
            };
            assert_eq!(warp_plan(&fc, &p).map(|w| w.ox), Some(1), "separable, standard");
        }
        let scaled_tiny = Params {
            geometry_overscan_x: 0.05,
            ..tiny
        };
        assert_eq!(
            warp_plan(&fc, &scaled_tiny),
            None,
            "scaled raster, spot under the grid rule"
        );
        for p in [
            Params {
                geometry_curvature: GEOMETRY_CURVATURE_SPHERICAL,
                ..d
            },
            Params {
                geometry_curvature: GEOMETRY_CURVATURE_CYLINDRICAL,
                ..d
            },
            Params {
                geometry_pincushion: 0.02,
                ..d
            },
            Params {
                geometry_trapezoid: -0.01,
                ..d
            },
            Params {
                geometry_rotation_deg: 0.5,
                ..d
            },
        ] {
            let plan = warp_plan(&fc, &p).expect("warp");
            assert_eq!(plan.ox, 1);
            assert_eq!(plan.size.1, 240);
            // 4 sigma at I = 2 (FWHM 1.086 mm) x 1.1 + 3: ceil(4 x 0.4612 x 4.9655 x 1.1) + 3 = 11 + 3.
            assert_eq!(plan.pad, 14, "{plan:?}");
            assert_eq!(plan.size.0, 1920 + 2 * 14);
            let reference = Params {
                beam_quality: crate::params::BEAM_QUALITY_REFERENCE,
                ..p
            };
            assert_eq!(warp_plan(&fc, &reference), None, "reference quality");
        }
        let curved = Params {
            geometry_curvature: GEOMETRY_CURVATURE_SPHERICAL,
            ..d
        };
        let mut small = fc;
        small.px_per_mm = 2.0;
        let spot = |fwhm: f32| Params {
            beam_fwhm_mid_mm: fwhm,
            beam_fwhm_peak_mm: fwhm,
            ..curved
        };
        assert_eq!(warp_plan(&small, &spot(0.6)).map(|w| w.ox), Some(4));
        assert_eq!(warp_plan(&small, &spot(1.2)).map(|w| w.ox), Some(2));
        assert_eq!(warp_plan(&small, &spot(0.1)), None);
        let mut unplaced = fc;
        unplaced.px_per_mm = 0.0;
        assert_eq!(warp_plan(&unplaced, &curved), None, "unplaced");
    }
}

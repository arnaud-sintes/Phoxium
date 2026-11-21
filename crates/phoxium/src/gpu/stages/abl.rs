//! Slot `abl` (amp -> amp): automatic beam limiting and raster breathing (T-35, QUESTIONS PHY-13,
//! `shaders/abl.comp`, `shaders/reduce.comp`, `shaders/include/abl.glsl`, parameters `params/abl.toml`,
//! oracle `crt_model::abl`).
//!
//! Runs after `gun`, on the beam current. Two passes per frame, both recorded by this module:
//! 1. apply (`abl.comp`, 8x8 groups over the amp image): output = input x g(apl_f), where apl_f is the
//!    low-passed APL the reduce pass left at the end of the previous frame (one frame of latency);
//!    `abl_enable` off copies the texel unchanged (exact identity);
//! 2. reduce (`reduce.comp`, one workgroup of 256): the frame APL of the input (unlimited current) in FP32,
//!    then the low-pass with the real frame time `FrameConstants::frame_time_ms`, into the state. In every
//!    quality tier but `exact` (T-48) the sum is parallel instead: `reduce_partial.comp` (one workgroup per
//!    4096 texels, partial sums in the state buffer from byte [`PARTIALS_OFFSET`], set 1 binding 1) then
//!    `reduce_final.comp` (adds the partials, same state update). One workgroup walking a 1.2 Mtexel amp image
//!    (K = 16) cost about 0.35 ms; the same sum in another order differs by the FP32 rounding only.
//!
//! Design (PHY-13 "a reduction pass, a frame of latency or an estimate"): the state lives on the GPU, in
//! a 16-byte storage buffer owned by this stage (set 1, `ABL_STATE_SET` of `abl.glsl`). No CPU readback
//! (gate 10) and no wait: the gain of frame N+1 is computed by the GPU from frame N in queue order. The
//! buffer is never mapped, so its memory is `DEVICE_LOCAL` (R-30, allocated in [`Stage::allocate`]); it is
//! cleared by `vkCmdFillBuffer` in the first recorded frame.
//! This stage records the barriers on its own buffer (the pipeline owns and tracks images only): at the
//! start, the previous frame's reduce writes (and the first clear) before this frame's apply reads; between
//! the passes, the apply reads before the reduce writes.
//!
//! `raster_scale` = 1 + `abl_breathing` (apl_f - `abl_breathing_ref`) is a function of the same state
//! (`abl_raster_scale` in `abl.glsl`); the geometry stage (T-37) reads it from this state (see the T-35
//! report: `FrameConstants` is written by the CPU before the GPU measures the APL).
//!
//! Skipped as an identity ([`Stage::is_identity`]): with `abl_enable`, `abl_breathing` and `abl_probe` all
//! off the pipeline records neither pass (the apply pass would be a full-image copy). The state is not
//! touched while skipped, and nothing reads it (`screen` reads it only when `abl_breathing` is not 0); the
//! slot's `prepare` sees `runs` false and clears the state in the first frame that runs again.
//!
//! Cold start (T-37 review): the clear sets apl_f to `abl_breathing_ref` when the breathing is on, so
//! `raster_scale` starts at exactly 1 and then follows the APL with the low-pass, and to 0 otherwise (cold
//! start on black for the limiter). `screen` takes `raster_scale` 1 in a frame where this slot does not run.

use std::cell::Cell;

use ash::vk;

use super::{binding, compute_pipeline, standard_set_layout, ComputePass, Domain, Registration, Stage, GROUP};
use crate::gpu::FrameConstants;
use crate::params::Params;
use crate::quality::{self, Tier};
use crate::Res;

const APPLY_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/abl.comp.spv"));
const REDUCE_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/reduce.comp.spv"));
/// Parallel reduce (T-48): partial sums, then the final sum and the state update.
const PARTIAL_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/reduce_partial.comp.spv"));
const FINAL_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/reduce_final.comp.spv"));

/// Size of the state (`AblState` of `abl.glsl`: four floats).
const STATE_BYTES: u64 = 16;
/// The partial sums of the parallel reduce follow the state in the same buffer, from this offset (the largest
/// `minStorageBufferOffsetAlignment` Vulkan allows), at most [`MAX_PARTIALS`] floats.
const PARTIALS_OFFSET: u64 = 256;
/// Texels per partial sum and the largest number of partials (`ABL_REDUCE_CHUNK`, `ABL_MAX_PARTIALS` of
/// `reduce_partial.comp` and `reduce_final.comp`).
const REDUCE_CHUNK: u64 = 4096;
const MAX_PARTIALS: u64 = 65536;

/// Workgroups of `reduce_partial.comp` for `n` texels (the shaders' chunk rule: 4096 texels, more in steps of 256
/// when `n` needs more than [`MAX_PARTIALS`] groups).
fn partial_groups(n: u64) -> u32 {
    let chunk = REDUCE_CHUNK.max(n.div_ceil(MAX_PARTIALS).div_ceil(256) * 256);
    n.div_ceil(chunk).max(1) as u32
}

/// Registration of the slot (see [`super::CHAIN`]).
pub(in crate::gpu) const SLOT: Registration = Registration {
    name: "abl",
    input: Domain::Amp,
    output: Domain::Amp,
    new,
};

fn new() -> Box<dyn Stage> {
    Box::<Abl>::default()
}

#[derive(Default)]
struct Abl {
    /// The apply pass; its pipeline layout has the standard set 0 and the state set 1.
    pass: ComputePass,
    reduce: vk::Pipeline,
    /// The parallel reduce (T-48) and whether the frame being recorded uses it (every tier but `exact`).
    reduce_partial: vk::Pipeline,
    reduce_final: vk::Pipeline,
    parallel: Cell<bool>,
    state_layout: vk::DescriptorSetLayout,
    pool: vk::DescriptorPool,
    state_set: vk::DescriptorSet,
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    /// The clear of the state buffer was recorded.
    cleared: Cell<bool>,
    /// The frame being recorded clears the state (given back by [`Stage::abandon`]).
    clearing: Cell<bool>,
    /// Value of apl_f after a clear: `abl_breathing_ref` while the breathing is on, else 0 (set by
    /// `prepare`).
    seed: Cell<f32>,
}

impl Stage for Abl {
    fn pass(&self) -> Option<&ComputePass> {
        Some(&self.pass)
    }

    unsafe fn create(&mut self, d: &ash::Device) -> Res<()> {
        let s = vk::ShaderStageFlags::COMPUTE;
        self.pass.set_layout = standard_set_layout(d)?;
        let state = [
            binding(0, vk::DescriptorType::STORAGE_BUFFER, s),
            binding(1, vk::DescriptorType::STORAGE_BUFFER, s),
        ];
        self.state_layout =
            d.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&state), None)?;
        let set_layouts = [self.pass.set_layout, self.state_layout];
        self.pass.layout =
            d.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&set_layouts), None)?;
        self.pass.pipeline = compute_pipeline(d, self.pass.layout, APPLY_SPV)?;
        self.reduce = compute_pipeline(d, self.pass.layout, REDUCE_SPV)?;
        self.reduce_partial = compute_pipeline(d, self.pass.layout, PARTIAL_SPV)?;
        self.reduce_final = compute_pipeline(d, self.pass.layout, FINAL_SPV)?;

        self.buffer = d.create_buffer(
            &vk::BufferCreateInfo::default()
                .size(PARTIALS_OFFSET + 4 * MAX_PARTIALS)
                .usage(vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::TRANSFER_DST)
                .sharing_mode(vk::SharingMode::EXCLUSIVE),
            None,
        )?;
        // Its memory and the descriptor writes follow in `allocate`, which has the context.

        let sizes = [vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::STORAGE_BUFFER)
            .descriptor_count(2)];
        self.pool = d.create_descriptor_pool(
            &vk::DescriptorPoolCreateInfo::default().max_sets(1).pool_sizes(&sizes),
            None,
        )?;
        let layouts = [self.state_layout];
        self.state_set = d.allocate_descriptor_sets(
            &vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(self.pool)
                .set_layouts(&layouts),
        )?[0];
        Ok(())
    }

    /// R-30: the state buffer lives in `DEVICE_LOCAL` memory (it is never mapped; the first allowed type when the
    /// device offers no device-local one for it), bound before the pipeline hands it to `screen`.
    unsafe fn allocate(&mut self, ctx: &crate::gpu::Context) -> Res<()> {
        let d = &ctx.device;
        let req = d.get_buffer_memory_requirements(self.buffer);
        if req.memory_type_bits == 0 {
            return Err("abl: no memory type for the state buffer".into());
        }
        let index = ctx
            .memory_type(req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)
            .unwrap_or(req.memory_type_bits.trailing_zeros());
        self.memory = d.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(req.size)
                .memory_type_index(index),
            None,
        )?;
        d.bind_buffer_memory(self.buffer, self.memory, 0)?;
        let info = [vk::DescriptorBufferInfo::default()
            .buffer(self.buffer)
            .range(STATE_BYTES)];
        let partials = [vk::DescriptorBufferInfo::default()
            .buffer(self.buffer)
            .offset(PARTIALS_OFFSET)
            .range(4 * MAX_PARTIALS)];
        d.update_descriptor_sets(
            &[
                vk::WriteDescriptorSet::default()
                    .dst_set(self.state_set)
                    .dst_binding(0)
                    .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                    .buffer_info(&info),
                vk::WriteDescriptorSet::default()
                    .dst_set(self.state_set)
                    .dst_binding(1)
                    .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                    .buffer_info(&partials),
            ],
            &[],
        );
        Ok(())
    }

    unsafe fn record(&self, d: &ash::Device, cb: vk::CommandBuffer, set: vk::DescriptorSet, size: (u32, u32)) {
        let compute = vk::PipelineStageFlags2::COMPUTE_SHADER;
        let mut src_stage = compute;
        let mut src_access = vk::AccessFlags2::SHADER_STORAGE_WRITE;
        if !self.cleared.get() {
            // The clear comes after the earlier reads of the state: `screen` binds it every frame, also
            // while this slot is skipped (identity), so the first run after a skip must wait for them.
            self.state_barrier(
                d,
                cb,
                (
                    compute,
                    vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::SHADER_STORAGE_WRITE,
                ),
                (vk::PipelineStageFlags2::CLEAR, vk::AccessFlags2::TRANSFER_WRITE),
            );
            d.cmd_fill_buffer(cb, self.buffer, 0, 4, self.seed.get().to_bits());
            d.cmd_fill_buffer(cb, self.buffer, 4, vk::WHOLE_SIZE, 0);
            src_stage |= vk::PipelineStageFlags2::CLEAR;
            src_access |= vk::AccessFlags2::TRANSFER_WRITE;
            self.cleared.set(true);
            self.clearing.set(true);
        }
        // Previous frame's reduce (and the first clear) -> this frame's apply and reduce.
        self.state_barrier(
            d,
            cb,
            (src_stage, src_access),
            (
                compute,
                vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::SHADER_STORAGE_WRITE,
            ),
        );
        let sets = [set, self.state_set];
        let bind = vk::PipelineBindPoint::COMPUTE;
        d.cmd_bind_pipeline(cb, bind, self.pass.pipeline);
        d.cmd_bind_descriptor_sets(cb, bind, self.pass.layout, 0, &sets, &[]);
        d.cmd_dispatch(cb, size.0.div_ceil(GROUP), size.1.div_ceil(GROUP), 1);
        // The apply pass reads the state before the reduce pass writes it (write after read).
        self.state_barrier(
            d,
            cb,
            (compute, vk::AccessFlags2::SHADER_STORAGE_READ),
            (compute, vk::AccessFlags2::SHADER_STORAGE_WRITE),
        );
        if self.parallel.get() {
            d.cmd_bind_pipeline(cb, bind, self.reduce_partial);
            d.cmd_dispatch(cb, partial_groups(u64::from(size.0) * u64::from(size.1)), 1, 1);
            // The partial sums before the final pass reads them.
            self.state_barrier(
                d,
                cb,
                (compute, vk::AccessFlags2::SHADER_STORAGE_WRITE),
                (compute, vk::AccessFlags2::SHADER_STORAGE_READ),
            );
            d.cmd_bind_pipeline(cb, bind, self.reduce_final);
        } else {
            d.cmd_bind_pipeline(cb, bind, self.reduce);
        }
        d.cmd_dispatch(cb, 1, 1, 1);
        // T-37: the reduce writes before the later compute stages (`screen`, raster_scale) read the state.
        self.state_barrier(
            d,
            cb,
            (compute, vk::AccessFlags2::SHADER_STORAGE_WRITE),
            (compute, vk::AccessFlags2::SHADER_STORAGE_READ),
        );
    }

    /// Limiter, breathing and probe all off: the apply pass copies the texels and nothing reads the
    /// state (`screen` reads it only when `abl_breathing` is not 0), so the slot is skipped, reduce pass
    /// included. `prepare` then sees `runs` false and arms the cold start, which is what the reduce pass
    /// did in this configuration anyway (it kept the state at 0). With the breathing on and the limiter off
    /// the slot runs: the reduce pass keeps the state, `raster_scale` comes from it.
    fn is_identity(&self, params: &Params, _fc: &FrameConstants) -> bool {
        !params.abl_enable && params.abl_breathing == 0.0 && !params.abl_probe
    }

    fn abl_state(&self) -> Option<vk::Buffer> {
        Some(self.buffer)
    }

    unsafe fn prepare(
        &mut self,
        _ctx: &crate::gpu::Context,
        _fc: &crate::gpu::FrameConstants,
        params: &crate::params::Params,
        runs: bool,
    ) -> Res<()> {
        self.clearing.set(false);
        self.parallel.set(quality::tier(params) != Tier::Exact);
        let breathing = params.abl_breathing != 0.0 && params.abl_breathing_ref.is_finite();
        self.seed.set(if breathing { params.abl_breathing_ref } else { 0.0 });
        if !runs {
            // Slot skipped (disabled or no source): the state no longer follows the picture, the slot
            // restarts cold (black) when it runs again. `abl_enable` off is different: the reduce
            // pass keeps tracking the APL (raster breathing works with the limiter off).
            self.cleared.set(false);
        }
        Ok(())
    }

    /// Resets the APL state (cold start on black) at the next recorded frame.
    fn clear_history(&mut self) {
        self.cleared.set(false);
    }

    fn abandon(&mut self) {
        if self.clearing.take() {
            self.cleared.set(false);
        }
    }

    unsafe fn destroy(&mut self, d: &ash::Device) {
        d.destroy_pipeline(self.reduce, None);
        d.destroy_pipeline(self.reduce_partial, None);
        d.destroy_pipeline(self.reduce_final, None);
        d.destroy_descriptor_pool(self.pool, None);
        d.destroy_descriptor_set_layout(self.state_layout, None);
        d.destroy_buffer(self.buffer, None);
        d.free_memory(self.memory, None);
        self.pass.destroy(d);
        *self = Self::default();
    }
}

impl Abl {
    /// Records a barrier on the state buffer from `src` to `dst` (stages, accesses).
    ///
    /// # Safety
    /// `cb` is recording on `d`.
    unsafe fn state_barrier(
        &self,
        d: &ash::Device,
        cb: vk::CommandBuffer,
        src: (vk::PipelineStageFlags2, vk::AccessFlags2),
        dst: (vk::PipelineStageFlags2, vk::AccessFlags2),
    ) {
        let b = [vk::BufferMemoryBarrier2::default()
            .src_stage_mask(src.0)
            .src_access_mask(src.1)
            .dst_stage_mask(dst.0)
            .dst_access_mask(dst.1)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .buffer(self.buffer)
            .offset(0)
            .size(vk::WHOLE_SIZE)];
        d.cmd_pipeline_barrier2(cb, &vk::DependencyInfo::default().buffer_memory_barriers(&b));
    }
}

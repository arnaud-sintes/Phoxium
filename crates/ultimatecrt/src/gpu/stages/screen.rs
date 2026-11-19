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

use std::cell::Cell;

use ash::vk;

use super::{binding, compute_pipeline, standard_set_layout, ComputePass, Domain, Registration, Stage};
use crate::Res;

const SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/screen.comp.spv"));

/// Workgroup size of `screen.comp`: `BEAM_TILE_W`, `BEAM_TILE_H` of `shaders/include/beam.glsl`.
const SCREEN_GROUP: (u32, u32) = (8, 32);

/// Size of the ABL state (`AblState` of `abl.glsl`: four floats).
const STATE_BYTES: u64 = 16;

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

#[derive(Default)]
struct Screen {
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

        let sizes = [vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::STORAGE_BUFFER)
            .descriptor_count(1)];
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
        self.bind_state(d, self.fallback);
        Ok(())
    }

    unsafe fn link_abl_state(&mut self, d: &ash::Device, buffer: vk::Buffer) {
        self.bind_state(d, buffer);
    }

    fn set_abl_runs(&self, runs: bool) {
        self.abl_runs.set(runs);
    }

    unsafe fn record(&self, d: &ash::Device, cb: vk::CommandBuffer, set: vk::DescriptorSet, size: (u32, u32)) {
        // As `ComputePass::dispatch_group` with the tile workgroup, plus the ABL state set 1.
        let sets = [set, self.state_set];
        let bind = vk::PipelineBindPoint::COMPUTE;
        d.cmd_bind_pipeline(cb, bind, self.pass.pipeline);
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
        d.destroy_descriptor_pool(self.pool, None);
        d.destroy_descriptor_set_layout(self.state_layout, None);
        d.destroy_buffer(self.fallback, None);
        d.free_memory(self.memory, None);
        self.pass.destroy(d);
        *self = Self::default();
    }
}

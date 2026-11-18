//! Pipeline stages (ARCHITECTURE 3.7, A11, TEC-6): one module per stage, in physical order.
//!
//! A stage module owns its Vulkan objects (descriptor set layout, pipeline layout, pipeline) and records
//! its pass; [`super::pipeline::Pipeline`] owns the images, the per-frame buffers and the barriers between
//! stages. Every stage follows the same contract:
//! - set 0, binding 0: `Params` uniform buffer (`params.glsl`, T-10), per frame in flight;
//! - set 0, binding 1: `FrameConstants` uniform buffer (`shaders/include/common.glsl`), per frame in flight;
//! - set 0, bindings 2 and up: the stage's input images (combined image sampler, read with `texelFetch`)
//!   then its output (storage image for compute stages);
//! - stage settings that are not yet parameters go in a small push-constant block declared in the shader
//!   and mirrored by a `#[repr(C)]` struct in the module.
//!
//! Stages today: `decode` (compute, captured frame -> native raster) and `encode` (fragment, linear
//! light -> output target). T-20b inserts the remaining slots between them.

use std::io::Cursor;

use ash::vk;

use crate::Res;

pub(super) mod decode;
pub(super) mod encode;

/// Descriptor binding of the `Params` uniform buffer in every stage.
pub(super) const BINDING_PARAMS: u32 = 0;
/// Descriptor binding of the `FrameConstants` uniform buffer in every stage.
pub(super) const BINDING_FRAME: u32 = 1;

/// The two uniform-buffer bindings every stage declares, visible to `stages`.
pub(super) fn common_bindings(stages: vk::ShaderStageFlags) -> [vk::DescriptorSetLayoutBinding<'static>; 2] {
    [BINDING_PARAMS, BINDING_FRAME].map(|b| {
        vk::DescriptorSetLayoutBinding::default()
            .binding(b)
            .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
            .descriptor_count(1)
            .stage_flags(stages)
    })
}

/// A descriptor binding of one `ty` descriptor visible to `stages`.
pub(super) fn binding(
    b: u32,
    ty: vk::DescriptorType,
    stages: vk::ShaderStageFlags,
) -> vk::DescriptorSetLayoutBinding<'static> {
    vk::DescriptorSetLayoutBinding::default()
        .binding(b)
        .descriptor_type(ty)
        .descriptor_count(1)
        .stage_flags(stages)
}

/// Pipeline layout with one set layout and one push-constant range of `push_size` bytes at offset 0.
///
/// # Safety
/// `d` must be a live device and `set_layout` one of its layouts; the caller destroys the result.
pub(super) unsafe fn pipeline_layout(
    d: &ash::Device,
    set_layout: vk::DescriptorSetLayout,
    stages: vk::ShaderStageFlags,
    push_size: u32,
) -> Res<vk::PipelineLayout> {
    let set_layouts = [set_layout];
    let push = [vk::PushConstantRange::default()
        .stage_flags(stages)
        .offset(0)
        .size(push_size)];
    Ok(d.create_pipeline_layout(
        &vk::PipelineLayoutCreateInfo::default()
            .set_layouts(&set_layouts)
            .push_constant_ranges(&push),
        None,
    )?)
}

/// Records `vkCmdPushConstants` for the whole of `value`.
///
/// # Safety
/// `cb` must be recording on `d`; `layout` must have a push range of `size_of::<T>()` bytes at offset 0
/// for `stages`; `T` must be `#[repr(C)]` plain data without padding.
pub(super) unsafe fn push<T: Copy>(
    d: &ash::Device,
    cb: vk::CommandBuffer,
    layout: vk::PipelineLayout,
    stages: vk::ShaderStageFlags,
    value: &T,
) {
    // SAFETY: the caller guarantees `T` is plain data without padding, so all its bytes are initialised.
    let bytes = std::slice::from_raw_parts((value as *const T).cast::<u8>(), size_of::<T>());
    d.cmd_push_constants(cb, layout, stages, 0, bytes);
}

/// Creates a shader module from SPIR-V bytes.
///
/// # Safety
/// `d` must be a live device; the caller destroys the returned module.
pub(super) unsafe fn shader_module(d: &ash::Device, spv: &[u8]) -> Res<vk::ShaderModule> {
    let words = ash::util::read_spv(&mut Cursor::new(spv))?;
    Ok(d.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None)?)
}

/// Creates a compute pipeline from SPIR-V bytes (entry point `main`).
///
/// # Safety
/// `d` must be a live device and `layout` one of its pipeline layouts; the caller destroys the result.
pub(super) unsafe fn compute_pipeline(d: &ash::Device, layout: vk::PipelineLayout, spv: &[u8]) -> Res<vk::Pipeline> {
    let module = shader_module(d, spv)?;
    let info = vk::ComputePipelineCreateInfo::default()
        .stage(
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::COMPUTE)
                .module(module)
                .name(c"main"),
        )
        .layout(layout);
    let result = d.create_compute_pipelines(vk::PipelineCache::null(), &[info], None);
    d.destroy_shader_module(module, None);
    Ok(result.map_err(|(_, e)| e)?[0])
}

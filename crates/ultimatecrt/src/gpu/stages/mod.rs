//! Pipeline stages (ARCHITECTURE 3.7, A11, TEC-6): one module per stage, in physical order.
//!
//! A stage module owns its Vulkan objects (descriptor set layout, pipeline layout, pipeline) and records
//! its pass; [`super::pipeline::Pipeline`] owns the images, the per-frame buffers and the barriers between
//! stages. Every stage follows the same contract:
//! - set 0, binding 0: `Params` uniform buffer (`params.glsl`, T-10), per frame in flight;
//! - set 0, binding 1: `FrameConstants` uniform buffer (`shaders/include/common.glsl`), per frame in flight;
//! - set 0, bindings 2 and up: the stage's input images (combined image sampler, read with `texelFetch`)
//!   then its output (write-only storage image for compute stages). No stage updates an image in place:
//!   a stage that maps a domain to itself (`gun`) reads the previous image and writes its own (a
//!   ping-pong by construction); a stage that needs its previous output (persistence, T-41) owns its
//!   history images;
//! - stage settings that are not yet parameters go in a small push-constant block declared in the shader
//!   and mirrored by a `#[repr(C)]` struct in the module.
//!
//! Stages: `decode` (compute, captured frame -> native raster) and `encode` (fragment, linear light ->
//! output target) are owned by the pipeline directly; between them runs the fixed ordered chain of
//! slots [`CHAIN`] (T-20b, A11, TEC-6), in physical order: `signal`, `amp`, `gun`, `abl`, `screen`,
//! `wear` (ageing and burn-in, T-42, T-43), `phosphor`, `optics`.
//!
//! # Chain slots (T-20b)
//!
//! A slot is a [`Registration`]: a name, the [`Domain`] of the image it reads and of the image it
//! writes, and a constructor of its [`Stage`]. A chain stage is a compute pass with the standard
//! bindings 0 `Params`, 1 `FrameConstants`, 2 its input (combined image sampler, `texelFetch`), 3 its
//! output (storage image `rgba16f`, the size of its output domain). The pipeline owns the images, the
//! descriptor sets, the routing and every barrier; a stage only records its dispatch.
//!
//! A slot is bypassed when its enable flag is off or when its stage is a placeholder (no pass yet).
//! A bypassed slot is an exact identity: a slot whose input and output domains are equal is skipped
//! (the next slot reads the same image); a slot that changes domain (`amp`: native -> amp, `screen`:
//! amp -> tube) runs the pipeline's nearest-neighbour resample (`shaders/resample.comp`), which copies
//! FP16 values without arithmetic.
//!
//! A same-domain slot whose stage says [`Stage::is_identity`] for the frame's parameters is skipped the
//! same way (`phosphor` with persistence and saturation off, `abl` with the limiter, the breathing and the
//! probe off): the pipeline records nothing for it in that frame.
//!
//! Filling a slot (T-30, T-33, T-34, ...): replace the placeholder in the slot's own module by a type
//! implementing [`Stage`] (usually a [`ComputePass`] plus its shader). Adding a slot: a new module with
//! its `SLOT` constant, its `mod` line and one line in [`CHAIN`].

use std::io::Cursor;

use ash::vk;

use crate::Res;

pub(super) mod decode;
pub(super) mod encode;

pub(super) mod abl;
pub(super) mod amp;
pub(super) mod composite;
pub(super) mod gun;
pub(super) mod optics;
pub(super) mod phosphor;
pub(super) mod screen;
pub(super) mod signal;
pub(super) mod wear;

#[cfg(test)]
pub(super) mod dummy;

/// The fixed ordered chain between `decode` and `encode` (A11, TEC-6): one registration line per slot,
/// in physical order (`CRT.md`, "Suggested physical pipeline order").
pub(super) const CHAIN: &[Registration] = &[
    signal::SLOT,
    amp::SLOT,
    gun::SLOT,
    abl::SLOT,
    screen::SLOT,
    wear::SLOT,
    phosphor::SLOT,
    optics::SLOT,
];

/// The image domains of the chain (image contract, ARCHITECTURE 3.7). Every chain image is
/// `R16G16B16A16_SFLOAT`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Domain {
    /// `native_size`: one texel per native source pixel (decode output, voltage domain).
    Native,
    /// `amp_size` = (`native_size.x * amp_k`, `native_size.y`): native rows supersampled horizontally by
    /// K (`FrameConstants::amp_k`, default [`AMP_K`]); drive voltage before `gun`, beam current after.
    Amp,
    /// `tube_size`: one texel per host pixel of the tube rectangle, linear light.
    Tube,
}

/// Default horizontal supersampling factor K of the `amp` output (image contract, T-33).
pub const AMP_K: u32 = 4;

/// A slot of the chain.
pub(super) struct Registration {
    /// Unique slot name (enable flags, test helpers, timings).
    pub name: &'static str,
    /// Domain of the image the slot reads (the previous slot's output domain).
    pub input: Domain,
    /// Domain of the image the slot writes.
    pub output: Domain,
    /// Creates the slot's stage (objects are created later by [`Stage::create`]).
    pub new: fn() -> Box<dyn Stage>,
}

/// A chain stage. The pipeline calls `create` once, `record` when the slot runs, `destroy` once.
pub(super) trait Stage {
    /// The compute pass, or `None` for a placeholder (the slot is then bypassed as an identity).
    fn pass(&self) -> Option<&ComputePass>;

    /// Creates the Vulkan objects; on error what was created stays for [`Stage::destroy`].
    ///
    /// # Safety
    /// `d` must be a live device.
    unsafe fn create(&mut self, d: &ash::Device) -> Res<()>;

    /// The stage is an exact identity for these parameters and constants: its output equals its input
    /// bit for bit, and running it has no effect that a later stage or a later frame needs (no state
    /// buffer or history that it would update, no probe texel). The pipeline then routes around a slot
    /// that keeps its domain exactly like a disabled one (no pass, no image, no barrier, the next slot
    /// reads the previous image), evaluated every frame; `prepare` is called with `runs` false, so the
    /// stage restarts its state when it runs again. A slot that changes domain always runs (its output
    /// has another size): the answer is ignored there. Must be pure and cheap. Default: `false`.
    fn is_identity(&self, _params: &crate::params::Params, _fc: &crate::gpu::FrameConstants) -> bool {
        false
    }

    /// Records the stage over an output image of `size`. Default: the pass without push constants.
    ///
    /// # Safety
    /// `cb` is recording on `d`; `set` is a set of the pass's layout with every binding written and the
    /// images in the layouts of the contract (input `SHADER_READ_ONLY_OPTIMAL`, output `GENERAL`).
    unsafe fn record(&self, d: &ash::Device, cb: vk::CommandBuffer, set: vk::DescriptorSet, size: (u32, u32)) {
        if let Some(p) = self.pass() {
            p.dispatch(d, cb, set, &[], size);
        }
    }

    /// Destroys the objects (null ones are no-ops).
    ///
    /// # Safety
    /// The GPU must have finished every use of them.
    unsafe fn destroy(&mut self, d: &ash::Device);

    /// Prepares what the stage owns beyond the chain images (its history images, T-41) for the frame
    /// about to be recorded, after the routing is known; `runs` is false when the slot does not run in
    /// that frame. May idle the device and allocate. Default: nothing.
    ///
    /// # Safety
    /// `ctx` is the pipeline's context and no command buffer using the stage's objects is recording.
    unsafe fn prepare(
        &mut self,
        _ctx: &crate::gpu::Context,
        _fc: &crate::gpu::FrameConstants,
        _params: &crate::params::Params,
        _runs: bool,
    ) -> Res<()> {
        Ok(())
    }

    /// Forgets the stage's history (T-41; SRC-8: source size change, new capture session): the next
    /// frame starts from a cleared history. Default: nothing.
    fn clear_history(&mut self) {}

    /// The frame last prepared and recorded is abandoned (never submitted, `Pipeline::abandon`):
    /// the stage restores what `prepare` or `record` consumed for it (a pending history clear, a layout
    /// transition of new images), so the next frame does them again. Default: nothing.
    fn abandon(&mut self) {}

    /// The ABL state buffer (`AblState` of `abl.glsl`) when this stage owns it (`abl`, T-35). Default: none.
    fn abl_state(&self) -> Option<vk::Buffer> {
        None
    }

    /// Receives the ABL state buffer of the chain once every stage is created (T-37: `screen` reads
    /// `raster_scale` from it). Default: nothing.
    ///
    /// # Safety
    /// `d` is the device of the stage's objects and `buffer` a live buffer of `abl` on it.
    unsafe fn link_abl_state(&mut self, _d: &ash::Device, _buffer: vk::Buffer) {}

    /// Tells the stage whether the `abl` slot runs in the frame about to be recorded (its state is then
    /// up to date for this frame), after every `prepare` (T-37 review: `screen` takes `raster_scale` 1 when
    /// it does not). Default: nothing.
    fn set_abl_runs(&self, _runs: bool) {}

    /// The burn-in wear map of the `wear` slot (T-43), for the CPU operations on it (load, reset, save).
    /// Default: none.
    fn wear(&mut self) -> Option<&mut wear::Wear> {
        None
    }
}

/// A slot whose stage is not written yet: always bypassed (exact identity). No slot uses it since T-31
/// filled `signal`; kept for new slots.
#[allow(dead_code)]
#[derive(Default)]
pub(super) struct Placeholder;

impl Stage for Placeholder {
    fn pass(&self) -> Option<&ComputePass> {
        None
    }
    unsafe fn create(&mut self, _d: &ash::Device) -> Res<()> {
        Ok(())
    }
    unsafe fn destroy(&mut self, _d: &ash::Device) {}
}

/// Binding of a chain stage's input image (combined image sampler).
pub(super) const BINDING_INPUT: u32 = 2;
/// Binding of a chain stage's output image (storage image).
pub(super) const BINDING_OUTPUT: u32 = 3;
/// Workgroup size of the chain compute shaders (`local_size_x`, `local_size_y`).
pub(super) const GROUP: u32 = 8;

/// A compute pass with the standard chain bindings and an optional push-constant block.
#[derive(Default)]
pub(super) struct ComputePass {
    pub set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    push_size: u32,
}

impl ComputePass {
    /// Creates the standard set layout, the pipeline layout (`push_size` bytes of push constants, 0 for
    /// none) and the pipeline from `spv`. On error, what was created stays for [`ComputePass::destroy`].
    ///
    /// # Safety
    /// `d` must be a live device and `self` empty.
    pub unsafe fn create(&mut self, d: &ash::Device, spv: &[u8], push_size: u32) -> Res<()> {
        self.set_layout = standard_set_layout(d)?;
        self.layout = pipeline_layout(d, self.set_layout, vk::ShaderStageFlags::COMPUTE, push_size)?;
        self.push_size = push_size;
        self.pipeline = compute_pipeline(d, self.layout, spv)?;
        Ok(())
    }

    /// Records the dispatch over an output of `size` (8x8 groups), with `push` as push constants
    /// (its length must equal the `push_size` of [`ComputePass::create`]).
    ///
    /// # Safety
    /// As [`Stage::record`].
    pub unsafe fn dispatch(
        &self,
        d: &ash::Device,
        cb: vk::CommandBuffer,
        set: vk::DescriptorSet,
        push: &[u8],
        size: (u32, u32),
    ) {
        self.dispatch_group(d, cb, set, push, size, (GROUP, GROUP));
    }

    /// [`ComputePass::dispatch`] for a shader whose workgroup is `group` (`local_size_x`, `local_size_y`).
    ///
    /// # Safety
    /// As [`Stage::record`].
    pub unsafe fn dispatch_group(
        &self,
        d: &ash::Device,
        cb: vk::CommandBuffer,
        set: vk::DescriptorSet,
        push: &[u8],
        size: (u32, u32),
        group: (u32, u32),
    ) {
        debug_assert_eq!(push.len() as u32, self.push_size);
        d.cmd_bind_pipeline(cb, vk::PipelineBindPoint::COMPUTE, self.pipeline);
        d.cmd_bind_descriptor_sets(cb, vk::PipelineBindPoint::COMPUTE, self.layout, 0, &[set], &[]);
        if !push.is_empty() {
            d.cmd_push_constants(cb, self.layout, vk::ShaderStageFlags::COMPUTE, 0, push);
        }
        d.cmd_dispatch(cb, size.0.div_ceil(group.0), size.1.div_ceil(group.1), 1);
    }

    /// Destroys the objects (null ones are no-ops) and resets them to null.
    ///
    /// # Safety
    /// The GPU must have finished every use of them.
    pub unsafe fn destroy(&mut self, d: &ash::Device) {
        d.destroy_pipeline(self.pipeline, None);
        d.destroy_pipeline_layout(self.layout, None);
        d.destroy_descriptor_set_layout(self.set_layout, None);
        *self = Self::default();
    }
}

/// The standard set layout of a chain stage: 0 `Params`, 1 `FrameConstants`, 2 input, 3 output.
///
/// # Safety
/// `d` must be a live device; the caller destroys the result.
pub(super) unsafe fn standard_set_layout(d: &ash::Device) -> Res<vk::DescriptorSetLayout> {
    let s = vk::ShaderStageFlags::COMPUTE;
    let [p, f] = common_bindings(s);
    let bindings = [
        p,
        f,
        binding(BINDING_INPUT, vk::DescriptorType::COMBINED_IMAGE_SAMPLER, s),
        binding(BINDING_OUTPUT, vk::DescriptorType::STORAGE_IMAGE, s),
    ];
    Ok(d.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings), None)?)
}

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
    // A push-constant range of size 0 is invalid: `push_size` 0 means no range.
    let ranges = if push_size == 0 { &[][..] } else { &push[..] };
    Ok(d.create_pipeline_layout(
        &vk::PipelineLayoutCreateInfo::default()
            .set_layouts(&set_layouts)
            .push_constant_ranges(ranges),
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

#[cfg(test)]
mod fp16_store_guard {
    /// The `imageStore` statements of `src` into an `rgba16f` image whose value is not wrapped in
    /// `round_f16` (every FP16 chain store rounds to nearest, `common.glsl`). `//` comments are ignored.
    fn unrounded_fp16_stores(src: &str) -> Vec<String> {
        let code: String = src
            .lines()
            .map(|l| l.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        let images: Vec<&str> = code
            .split(';')
            .filter(|decl| decl.contains("rgba16f") && decl.contains("image"))
            .filter_map(|decl| decl.split_whitespace().last())
            .collect();
        code.split(';')
            .filter_map(|stmt| {
                let call = &stmt[stmt.find("imageStore(")? + "imageStore(".len()..];
                let name = call.split(',').next()?.trim();
                (images.contains(&name) && !call.contains("round_f16(")).then(|| stmt.trim().to_string())
            })
            .collect()
    }

    /// Every `imageStore` into an `rgba16f` image of `shaders/*.comp` goes through `round_f16`, so a new
    /// stage cannot store truncated FP16 values (review of T-41: `phosphor.comp` had missed it).
    #[test]
    fn every_fp16_store_is_rounded() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../shaders");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).expect("shaders folder") {
            let path = entry.expect("entry").path();
            if path.extension().is_some_and(|e| e == "comp") {
                let src = std::fs::read_to_string(&path).expect("shader");
                let bad = unrounded_fp16_stores(&src);
                assert!(
                    bad.is_empty(),
                    "{}: FP16 store without round_f16: {bad:?}",
                    path.display()
                );
                checked += 1;
            }
        }
        assert!(checked >= 8, "only {checked} shaders found");
    }

    /// The guard itself: it finds an unrounded FP16 store and ignores FP32 images and comments.
    #[test]
    fn the_guard_finds_an_unrounded_store() {
        let src = "layout(set = 0, binding = 3, rgba16f) uniform writeonly image2D out_img;\n\
                   layout(set = 1, binding = 0, rgba32f) uniform image2D hist;\n\
                   void main() {\n\
                   // imageStore(out_img, p, v);\n\
                   imageStore(hist, p, v);\n\
                   imageStore(out_img, p, round_f16(v));\n\
                   imageStore(out_img, p,\n        v);\n}";
        assert_eq!(
            unrounded_fp16_stores(src),
            vec!["imageStore(out_img, p,\n        v)".to_string()]
        );
    }
}

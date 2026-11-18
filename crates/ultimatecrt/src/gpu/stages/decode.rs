//! Stage `decode` (compute): captured BGRA8 frame -> native raster RGBA16F (`shaders/decode.comp`).
//!
//! Bindings: 0 `Params`, 1 `FrameConstants`, 2 the captured frame (combined image sampler, read with
//! `texelFetch` at block centres from the [`View`]), 3 the native raster (storage image, written).

use ash::vk;

use super::super::pipeline::Transfer;
use crate::scaling::View;
use crate::Res;

const SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/decode.comp.spv"));

/// Workgroup size of `decode.comp` (`local_size_x`, `local_size_y`).
const GROUP: u32 = 8;

/// Binding of the captured frame.
pub(in crate::gpu) const BINDING_SRC: u32 = 2;
/// Binding of the native raster (output).
pub(in crate::gpu) const BINDING_NATIVE: u32 = 3;

const STAGES: vk::ShaderStageFlags = vk::ShaderStageFlags::COMPUTE;

/// Push constants of `decode.comp`, laid out as its `Push` block (24 bytes, no padding).
#[repr(C)]
#[derive(Clone, Copy)]
struct Push {
    origin: [i32; 2],
    div: [i32; 2],
    transfer: u32,
    gamma: f32,
}

/// Vulkan objects of the decode stage.
#[derive(Default)]
pub(in crate::gpu) struct Decode {
    pub set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
}

impl Decode {
    /// Creates the set layout, pipeline layout and compute pipeline. On error, what was created stays in
    /// `self` for [`Decode::destroy`].
    ///
    /// # Safety
    /// `d` must be a live device and `self` empty (null handles).
    pub unsafe fn create(&mut self, d: &ash::Device) -> Res<()> {
        let [p, f] = super::common_bindings(STAGES);
        let bindings = [
            p,
            f,
            super::binding(BINDING_SRC, vk::DescriptorType::COMBINED_IMAGE_SAMPLER, STAGES),
            super::binding(BINDING_NATIVE, vk::DescriptorType::STORAGE_IMAGE, STAGES),
        ];
        self.set_layout =
            d.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings), None)?;
        self.layout = super::pipeline_layout(d, self.set_layout, STAGES, size_of::<Push>() as u32)?;
        self.pipeline = super::compute_pipeline(d, self.layout, SPV)?;
        Ok(())
    }

    /// Records the decode dispatch over the `native` raster of `view`.
    ///
    /// # Safety
    /// `cb` is recording on `d`; `set` is a set of [`Decode::set_layout`] whose images are in the layouts
    /// the stage expects (frame `SHADER_READ_ONLY_OPTIMAL`, native raster `GENERAL`), with the barriers
    /// recorded by the caller.
    pub unsafe fn record(
        &self,
        d: &ash::Device,
        cb: vk::CommandBuffer,
        set: vk::DescriptorSet,
        view: View,
        source: Transfer,
    ) {
        let (transfer, gamma) = source.shader_pair();
        let push = Push {
            origin: [view.origin.0 as i32, view.origin.1 as i32],
            div: [view.div.0 as i32, view.div.1 as i32],
            transfer,
            gamma,
        };
        d.cmd_bind_pipeline(cb, vk::PipelineBindPoint::COMPUTE, self.pipeline);
        d.cmd_bind_descriptor_sets(cb, vk::PipelineBindPoint::COMPUTE, self.layout, 0, &[set], &[]);
        super::push(d, cb, self.layout, STAGES, &push);
        d.cmd_dispatch(cb, view.native.0.div_ceil(GROUP), view.native.1.div_ceil(GROUP), 1);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_block_is_24_bytes_without_padding() {
        assert_eq!(size_of::<Push>(), 24);
        assert_eq!(std::mem::offset_of!(Push, transfer), 16);
        assert_eq!(std::mem::offset_of!(Push, gamma), 20);
    }
}

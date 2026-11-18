//! Stage `encode` (fragment): linear light -> output target (`shaders/encode.frag`, drawn with
//! `shaders/fullscreen.vert`, one triangle over the whole target).
//!
//! Bindings: 0 `Params`, 1 `FrameConstants`, 2 the linear-light input (combined image sampler, read with
//! `texelFetch`). Inside the tube rectangle the input is scaled by `exposure`, clipped to panel white
//! and encoded with the inverse host EOTF; outside it the target is black (surround).

use ash::vk;

use super::super::pipeline::Transfer;
use crate::Res;

const VERT_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/fullscreen.vert.spv"));
const FRAG_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/encode.frag.spv"));

/// Binding of the linear-light input.
pub(in crate::gpu) const BINDING_INPUT: u32 = 2;

const STAGES: vk::ShaderStageFlags = vk::ShaderStageFlags::FRAGMENT;

/// Push constants of `encode.frag`, laid out as its `Push` block (8 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
struct Push {
    transfer: u32,
    gamma: f32,
}

/// Vulkan objects of the encode stage. The graphics pipeline depends on the target format, so one is
/// created per format on first use.
#[derive(Default)]
pub(in crate::gpu) struct Encode {
    pub set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pipelines: Vec<(vk::Format, vk::Pipeline)>,
}

impl Encode {
    /// Creates the set layout and pipeline layout. On error, what was created stays in `self` for
    /// [`Encode::destroy`].
    ///
    /// # Safety
    /// `d` must be a live device and `self` empty (null handles).
    pub unsafe fn create(&mut self, d: &ash::Device) -> Res<()> {
        let [p, f] = super::common_bindings(STAGES);
        let bindings = [
            p,
            f,
            super::binding(BINDING_INPUT, vk::DescriptorType::COMBINED_IMAGE_SAMPLER, STAGES),
        ];
        self.set_layout =
            d.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings), None)?;
        self.layout = super::pipeline_layout(d, self.set_layout, STAGES, size_of::<Push>() as u32)?;
        Ok(())
    }

    /// The graphics pipeline for a colour target of `format`, created on first use.
    ///
    /// # Safety
    /// `d` must be the live device of [`Encode::create`].
    pub unsafe fn pipeline(&mut self, d: &ash::Device, format: vk::Format) -> Res<vk::Pipeline> {
        if let Some(&(_, p)) = self.pipelines.iter().find(|(f, _)| *f == format) {
            return Ok(p);
        }
        let p = create_pipeline(d, self.layout, format)?;
        self.pipelines.push((format, p));
        Ok(p)
    }

    /// Records the encode draw into the colour attachment `target` of `extent`, cleared to black first.
    /// `pipeline` comes from [`Encode::pipeline`] for the target's format.
    ///
    /// # Safety
    /// `cb` is recording on `d` outside any rendering; `target` is a view of an image in
    /// `COLOR_ATTACHMENT_OPTIMAL`; `set` is a set of [`Encode::set_layout`] whose input image is in
    /// `SHADER_READ_ONLY_OPTIMAL`, with the barriers recorded by the caller.
    // A draw needs all of these values and a struct would only add noise.
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn record(
        &self,
        d: &ash::Device,
        cb: vk::CommandBuffer,
        pipeline: vk::Pipeline,
        set: Option<vk::DescriptorSet>,
        target: vk::ImageView,
        extent: vk::Extent2D,
        host: Transfer,
    ) {
        let attachment = [vk::RenderingAttachmentInfo::default()
            .image_view(target)
            .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::STORE)
            .clear_value(vk::ClearValue {
                color: vk::ClearColorValue {
                    float32: [0.0, 0.0, 0.0, 1.0],
                },
            })];
        let area = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent,
        };
        d.cmd_begin_rendering(
            cb,
            &vk::RenderingInfo::default()
                .render_area(area)
                .layer_count(1)
                .color_attachments(&attachment),
        );
        // Without a set (no frame decoded yet) the target is only cleared to black.
        if let Some(set) = set {
            d.cmd_set_viewport(
                cb,
                0,
                &[vk::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: extent.width as f32,
                    height: extent.height as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            d.cmd_set_scissor(cb, 0, &[area]);
            d.cmd_bind_pipeline(cb, vk::PipelineBindPoint::GRAPHICS, pipeline);
            d.cmd_bind_descriptor_sets(cb, vk::PipelineBindPoint::GRAPHICS, self.layout, 0, &[set], &[]);
            let (transfer, gamma) = host.shader_pair();
            super::push(d, cb, self.layout, STAGES, &Push { transfer, gamma });
            d.cmd_draw(cb, 3, 1, 0, 0);
        }
        d.cmd_end_rendering(cb);
    }

    /// Destroys the objects (null ones are no-ops) and resets them to null.
    ///
    /// # Safety
    /// The GPU must have finished every use of them.
    pub unsafe fn destroy(&mut self, d: &ash::Device) {
        for (_, p) in self.pipelines.drain(..) {
            d.destroy_pipeline(p, None);
        }
        d.destroy_pipeline_layout(self.layout, None);
        d.destroy_descriptor_set_layout(self.set_layout, None);
        *self = Self::default();
    }
}

/// Creates the encode graphics pipeline (dynamic rendering, one colour attachment of `format`, dynamic
/// viewport and scissor, no blending, no vertex input).
///
/// # Safety
/// `d` must be a live device and `layout` the encode pipeline layout; the caller destroys the pipeline.
unsafe fn create_pipeline(d: &ash::Device, layout: vk::PipelineLayout, format: vk::Format) -> Res<vk::Pipeline> {
    let vert = super::shader_module(d, VERT_SPV)?;
    let frag = match super::shader_module(d, FRAG_SPV) {
        Ok(m) => m,
        Err(e) => {
            d.destroy_shader_module(vert, None);
            return Err(e);
        }
    };
    let stages = [
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::VERTEX)
            .module(vert)
            .name(c"main"),
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::FRAGMENT)
            .module(frag)
            .name(c"main"),
    ];
    let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
    let assembly = vk::PipelineInputAssemblyStateCreateInfo::default().topology(vk::PrimitiveTopology::TRIANGLE_LIST);
    let viewport = vk::PipelineViewportStateCreateInfo::default()
        .viewport_count(1)
        .scissor_count(1);
    let raster = vk::PipelineRasterizationStateCreateInfo::default()
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(vk::CullModeFlags::NONE)
        .line_width(1.0);
    let multisample =
        vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(vk::SampleCountFlags::TYPE_1);
    let attachments =
        [vk::PipelineColorBlendAttachmentState::default().color_write_mask(vk::ColorComponentFlags::RGBA)];
    let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&attachments);
    let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
    let formats = [format];
    let mut rendering = vk::PipelineRenderingCreateInfo::default().color_attachment_formats(&formats);
    let info = vk::GraphicsPipelineCreateInfo::default()
        .stages(&stages)
        .vertex_input_state(&vertex_input)
        .input_assembly_state(&assembly)
        .viewport_state(&viewport)
        .rasterization_state(&raster)
        .multisample_state(&multisample)
        .color_blend_state(&blend)
        .dynamic_state(&dynamic)
        .layout(layout)
        .push_next(&mut rendering);
    let result = d.create_graphics_pipelines(vk::PipelineCache::null(), &[info], None);
    d.destroy_shader_module(vert, None);
    d.destroy_shader_module(frag, None);
    Ok(result.map_err(|(_, e)| e)?[0])
}

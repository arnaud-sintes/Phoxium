//! Stage `encode` (fragment): linear light -> output target (`shaders/encode.frag`, drawn with
//! `shaders/fullscreen.vert`, one triangle over the whole target).
//!
//! Bindings: 0 `Params`, 1 `FrameConstants`, 2 the linear-light input (combined image sampler, read with
//! `texelFetch`), 3 the clip counters (storage buffer, one per frame in flight, owned by the pipeline).
//! Inside the tube rectangle the input goes through the output transform (T-46, `shaders/encode.frag`):
//! exposure, the colour matrix from the tube primaries and white to sRGB primaries, clip counters, soft
//! shoulder, inverse host EOTF, optional dither; outside it the target is black (surround).

use ash::vk;
use crt_model::colour::Mat3;
use crt_model::output::{tube_to_display, IDENTITY};

use super::super::pipeline::Transfer;
use crate::params::Params;
use crate::Res;

const VERT_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/fullscreen.vert.spv"));
const FRAG_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/encode.frag.spv"));

/// Binding of the linear-light input.
pub(in crate::gpu) const BINDING_INPUT: u32 = 2;
/// Binding of the clip counters (storage buffer of [`CLIP_BYTES`] bytes).
pub(in crate::gpu) const BINDING_CLIP: u32 = 3;
/// Size of the clip counter buffer: `over_white` and `out_of_gamut`, two `u32`.
pub(in crate::gpu) const CLIP_BYTES: usize = 8;

/// Pixels the output transform could not show faithfully in a frame (T-46, COL-1 clip indicator).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClipCounts {
    /// Pixels of the tube rectangle with a channel above panel white after exposure and colour matrix
    /// (soft-clipped, or hard-clipped when the knee is 1).
    pub over_white: u32,
    /// Pixels with a negative channel after the colour matrix (outside the panel gamut, clipped to 0).
    pub out_of_gamut: u32,
}

const STAGES: vk::ShaderStageFlags = vk::ShaderStageFlags::FRAGMENT;

/// Push constants of `encode.frag`, laid out as its `Push` block (64 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
struct Push {
    /// Rows of the tube -> display matrix, padded to vec4.
    matrix: [[f32; 4]; 3],
    transfer: u32,
    gamma: f32,
    /// Quantisation steps of the target, 0 for a float target (no dither).
    levels: f32,
    pad: u32,
}

/// The linear colour matrix from the tube primaries and white (`colour.toml`) to sRGB/Rec.709 primaries
/// with D65, from [`crt_model::output::tube_to_display`], or the identity with `output_match_srgb`. A
/// degenerate tube (a white or primary with y <= 0, or collinear primaries) also gives the identity, so
/// that a bad UI value cannot panic the render loop.
pub fn output_matrix(p: &Params) -> Mat3 {
    if p.output_match_srgb {
        return IDENTITY;
    }
    let xy = |v: [f32; 2]| (f64::from(v[0]), f64::from(v[1]));
    let (r, g, b, w) = (
        xy(p.tube_red_xy),
        xy(p.tube_green_xy),
        xy(p.tube_blue_xy),
        xy(p.tube_white_xy),
    );
    let valid = [r, g, b, w]
        .iter()
        .all(|c| c.0.is_finite() && c.1.is_finite() && c.1 > 0.0);
    let area = (g.0 - r.0) * (b.1 - r.1) - (g.1 - r.1) * (b.0 - r.0);
    if !valid || area.abs() < 1e-9 {
        return IDENTITY;
    }
    tube_to_display(r, g, b, w)
}

/// Quantisation steps of a target format, the range of the dither (0: float or unknown, not dithered).
fn levels(format: vk::Format) -> f32 {
    match format {
        vk::Format::B8G8R8A8_UNORM | vk::Format::R8G8B8A8_UNORM => 255.0,
        vk::Format::A2B10G10R10_UNORM_PACK32 | vk::Format::A2R10G10B10_UNORM_PACK32 => 1023.0,
        _ => 0.0,
    }
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
            super::binding(BINDING_CLIP, vk::DescriptorType::STORAGE_BUFFER, STAGES),
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
        params: &Params,
        format: vk::Format,
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
            let m = output_matrix(params);
            let row = |i: usize| [m[i][0] as f32, m[i][1] as f32, m[i][2] as f32, 0.0];
            let push = Push {
                matrix: [row(0), row(1), row(2)],
                transfer,
                gamma,
                levels: levels(format),
                pad: 0,
            };
            super::push(d, cb, self.layout, STAGES, &push);
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

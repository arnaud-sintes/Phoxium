//! Passthrough pipeline: shader modules, layouts, pipeline and command recording.
//!
//! The free functions build the objects for any colour target format, so the window
//! [`Renderer`] and the offscreen [`super::Headless`] renderer share them.

use std::io::Cursor;

use super::*;

/// Push constants of the passthrough pipeline, laid out as the GLSL `push_constant` block.
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct PushConstants {
    /// Destination rectangle in NDC: min x, min y, max x, max y.
    rect: [f32; 4],
    /// Source origin (x, y), divisor (x, y), native size (w, h).
    ints: [i32; 6],
}

impl PushConstants {
    /// Push constants drawing the native raster of `view` into `rect` (NDC, min x, min y, max x, max y).
    pub(super) fn new(rect: [f32; 4], view: View) -> Self {
        Self {
            rect,
            ints: [
                view.origin.0 as i32,
                view.origin.1 as i32,
                view.div.0 as i32,
                view.div.1 as i32,
                view.native.0 as i32,
                view.native.1 as i32,
            ],
        }
    }

    /// Records `vkCmdPushConstants` for the whole block.
    ///
    /// # Safety
    /// `cb` must be recording on `d` and `layout` must be a pipeline layout made by [`create_pipeline_layout`].
    pub(super) unsafe fn push(&self, d: &ash::Device, cb: vk::CommandBuffer, layout: vk::PipelineLayout) {
        // SAFETY: PushConstants is #[repr(C)] plain data (f32 and i32 only, no padding).
        let bytes = std::slice::from_raw_parts((self as *const Self).cast::<u8>(), size_of::<Self>());
        d.cmd_push_constants(
            cb,
            layout,
            vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
            0,
            bytes,
        );
    }
}

const VERT_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/fullscreen.vert.spv"));
const FRAG_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/passthrough.frag.spv"));

/// Descriptor set layout of the passthrough pass: one combined image sampler (the source frame).
///
/// # Safety
/// `d` must be a live device; the caller destroys the returned layout.
pub(super) unsafe fn create_set_layout(d: &ash::Device) -> Res<vk::DescriptorSetLayout> {
    let bindings = [vk::DescriptorSetLayoutBinding::default()
        .binding(0)
        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
        .descriptor_count(1)
        .stage_flags(vk::ShaderStageFlags::FRAGMENT)];
    Ok(d.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings), None)?)
}

/// Pipeline layout of the passthrough pass: `set_layout` plus the [`PushConstants`] range.
///
/// # Safety
/// `d` must be a live device and `set_layout` one of its layouts; the caller destroys the result.
pub(super) unsafe fn create_pipeline_layout(
    d: &ash::Device,
    set_layout: vk::DescriptorSetLayout,
) -> Res<vk::PipelineLayout> {
    let set_layouts = [set_layout];
    let push = [vk::PushConstantRange::default()
        .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
        .offset(0)
        .size(size_of::<PushConstants>() as u32)];
    Ok(d.create_pipeline_layout(
        &vk::PipelineLayoutCreateInfo::default()
            .set_layouts(&set_layouts)
            .push_constant_ranges(&push),
        None,
    )?)
}

/// Nearest, clamp-to-edge sampler (the shader uses `texelFetch`, the sampler only completes the
/// combined image sampler descriptor).
///
/// # Safety
/// `d` must be a live device; the caller destroys the sampler.
pub(super) unsafe fn create_sampler(d: &ash::Device) -> Res<vk::Sampler> {
    Ok(d.create_sampler(
        &vk::SamplerCreateInfo::default()
            .mag_filter(vk::Filter::NEAREST)
            .min_filter(vk::Filter::NEAREST)
            .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE),
        None,
    )?)
}

/// Creates a shader module from SPIR-V bytes.
///
/// # Safety
/// `d` must be a live device; the caller destroys the returned module.
pub(super) unsafe fn shader_module(d: &ash::Device, spv: &[u8]) -> Res<vk::ShaderModule> {
    let words = ash::util::read_spv(&mut Cursor::new(spv))?;
    Ok(d.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None)?)
}

/// Creates the passthrough graphics pipeline (dynamic rendering, one colour attachment of `format`,
/// dynamic viewport and scissor, no blending).
///
/// # Safety
/// `d` must be a live device and `layout` a pipeline layout of it made by [`create_pipeline_layout`];
/// the caller destroys the pipeline.
pub(super) unsafe fn create_pipeline(
    d: &ash::Device,
    layout: vk::PipelineLayout,
    format: vk::Format,
) -> Res<vk::Pipeline> {
    let vert = shader_module(d, VERT_SPV)?;
    let frag = match shader_module(d, FRAG_SPV) {
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
    let assembly = vk::PipelineInputAssemblyStateCreateInfo::default().topology(vk::PrimitiveTopology::TRIANGLE_STRIP);
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

impl Renderer {
    /// Creates the passthrough graphics pipeline for the swapchain format.
    ///
    /// # Safety
    /// The device and `pipeline_layout` must be live, the swapchain format set, and no previous pipeline alive.
    pub(super) unsafe fn create_pipeline(&mut self) -> Res<()> {
        self.pipeline = create_pipeline(&self.ctx.device, self.pipeline_layout, self.presenter.format())?;
        Ok(())
    }

    /// Records the frame command buffer for swapchain image `index`.
    ///
    /// # Safety
    /// `index` must be an acquired swapchain image, `cmd` must not be in use by the GPU (fence waited) and the pipeline must exist.
    pub(super) unsafe fn record(&mut self, index: u32) {
        let cb = self.cmd;
        let d = &self.ctx.device;

        if self.upload_pending {
            if let Some(t) = self.tex.as_mut() {
                barrier(
                    d,
                    cb,
                    t.image,
                    t.layout,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::PipelineStageFlags2::FRAGMENT_SHADER,
                    vk::AccessFlags2::SHADER_READ,
                    vk::PipelineStageFlags2::TRANSFER,
                    vk::AccessFlags2::TRANSFER_WRITE,
                );
                d.cmd_copy_buffer_to_image(
                    cb,
                    t.staging,
                    t.image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[vk::BufferImageCopy::default()
                        .image_subresource(
                            vk::ImageSubresourceLayers::default()
                                .aspect_mask(vk::ImageAspectFlags::COLOR)
                                .layer_count(1),
                        )
                        .image_extent(vk::Extent3D {
                            width: t.width,
                            height: t.height,
                            depth: 1,
                        })],
                );
                barrier(
                    d,
                    cb,
                    t.image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::PipelineStageFlags2::TRANSFER,
                    vk::AccessFlags2::TRANSFER_WRITE,
                    vk::PipelineStageFlags2::FRAGMENT_SHADER,
                    vk::AccessFlags2::SHADER_READ,
                );
                t.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
            }
            self.upload_pending = false;
        }

        let target = self.presenter.image(index);
        barrier(
            d,
            cb,
            target,
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags2::NONE,
            vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
        );

        let attachment = [vk::RenderingAttachmentInfo::default()
            .image_view(self.presenter.view(index))
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
            extent: self.presenter.extent(),
        };
        d.cmd_begin_rendering(
            cb,
            &vk::RenderingInfo::default()
                .render_area(area)
                .layer_count(1)
                .color_attachments(&attachment),
        );
        if self.tex.is_some() {
            // Letterbox the native raster into the window, optionally on integer scales only.
            let v = self.view;
            let (nw, nh) = (v.native.0 as f32, v.native.1 as f32);
            let (ew, eh) = (area.extent.width as f32, area.extent.height as f32);
            let mut scale = (ew / nw).min(eh / nh);
            if self.integer_scale && scale >= 1.0 {
                scale = scale.floor();
            }
            let (hx, hy) = (nw * scale / ew, nh * scale / eh);
            let rect: [f32; 4] = [-hx, -hy, hx, hy];
            d.cmd_set_viewport(
                cb,
                0,
                &[vk::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: ew,
                    height: eh,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            d.cmd_set_scissor(cb, 0, &[area]);
            d.cmd_bind_pipeline(cb, vk::PipelineBindPoint::GRAPHICS, self.pipeline);
            d.cmd_bind_descriptor_sets(
                cb,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipeline_layout,
                0,
                &[self.desc_set],
                &[],
            );
            PushConstants::new(rect, v).push(d, cb, self.pipeline_layout);
            d.cmd_draw(cb, 4, 1, 0, 0);
        }
        d.cmd_end_rendering(cb);

        barrier(
            d,
            cb,
            target,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::ImageLayout::PRESENT_SRC_KHR,
            vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
            vk::PipelineStageFlags2::NONE,
            vk::AccessFlags2::NONE,
        );
    }
}

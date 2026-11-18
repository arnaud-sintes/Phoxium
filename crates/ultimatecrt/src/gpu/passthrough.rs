//! Passthrough pipeline: shader modules, pipeline and command recording.

use std::io::Cursor;

use super::*;

/// Push constants of the passthrough pipeline, laid out as the GLSL `push_constant` block.
#[repr(C)]
#[derive(Clone, Copy)]
struct PushConstants {
    /// Destination rectangle in NDC: min x, min y, max x, max y.
    rect: [f32; 4],
    /// Source origin (x, y), divisor (x, y), native size (w, h).
    ints: [i32; 6],
}

const VERT_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/fullscreen.vert.spv"));
const FRAG_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/passthrough.frag.spv"));

impl Renderer {
    /// Creates the passthrough graphics pipeline.
    ///
    /// # Safety
    /// The device and `pipeline_layout` must be live, the swapchain format set, and no previous pipeline alive.
    pub(super) unsafe fn create_pipeline(&mut self) -> Res<()> {
        let vert = self.shader_module(VERT_SPV)?;
        let frag = self.shader_module(FRAG_SPV)?;
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
        let assembly =
            vk::PipelineInputAssemblyStateCreateInfo::default().topology(vk::PrimitiveTopology::TRIANGLE_STRIP);
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
        let formats = [self.format];
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
            .layout(self.pipeline_layout)
            .push_next(&mut rendering);
        let result = self
            .device
            .create_graphics_pipelines(vk::PipelineCache::null(), &[info], None);
        self.device.destroy_shader_module(vert, None);
        self.device.destroy_shader_module(frag, None);
        self.pipeline = result.map_err(|(_, e)| e)?[0];
        Ok(())
    }

    /// Creates a shader module from SPIR-V bytes.
    ///
    /// # Safety
    /// The device must be live; the caller destroys the returned module.
    pub(super) unsafe fn shader_module(&self, spv: &[u8]) -> Res<vk::ShaderModule> {
        let words = ash::util::read_spv(&mut Cursor::new(spv))?;
        Ok(self
            .device
            .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None)?)
    }

    pub(super) fn memory_type(&self, bits: u32, flags: vk::MemoryPropertyFlags) -> Res<u32> {
        let props = unsafe { self.instance.get_physical_device_memory_properties(self.physical) };
        (0..props.memory_type_count)
            .find(|&i| bits & (1 << i) != 0 && props.memory_types[i as usize].property_flags.contains(flags))
            .ok_or_else(|| "no suitable memory type".into())
    }

    /// Records the frame command buffer for swapchain image `index`.
    ///
    /// # Safety
    /// `index` must be an acquired swapchain image, `cmd` must not be in use by the GPU (fence waited) and the pipeline must exist.
    pub(super) unsafe fn record(&mut self, index: usize) {
        let cb = self.cmd;
        let d = &self.device;

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

        let target = self.images[index];
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
            .image_view(self.views[index])
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
            extent: self.extent,
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
            let (ew, eh) = (self.extent.width as f32, self.extent.height as f32);
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
            let pc = PushConstants {
                rect,
                ints: [
                    v.origin.0 as i32,
                    v.origin.1 as i32,
                    v.div.0 as i32,
                    v.div.1 as i32,
                    v.native.0 as i32,
                    v.native.1 as i32,
                ],
            };
            // SAFETY: PushConstants is #[repr(C)] plain data (f32 and i32 only, no padding).
            let bytes =
                std::slice::from_raw_parts((&pc as *const PushConstants).cast::<u8>(), size_of::<PushConstants>());
            d.cmd_push_constants(
                cb,
                self.pipeline_layout,
                vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                0,
                bytes,
            );
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

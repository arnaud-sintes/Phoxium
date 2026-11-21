//! The UI pass (T-52a, T-52b, ARCHITECTURE A17): the settings menu and the stats overlay drawn over the
//! output image, as the last step of a frame, after `encode`.
//!
//! The UI is not part of the tube: it is drawn in display space directly on the image `encode` wrote (the
//! swapchain image, or the headless target), so it never goes through the CRT chain and is never dithered,
//! shouldered or clipped by the output transform. The ImGui colours are sRGB codes written unchanged into the
//! UNORM target (`dear-imgui-ash` with `framebuffer_srgb` off), which is what a UI over an SDR panel wants.
//!
//! The pipeline records the pass only when the UI has something to draw ([`UiPass::wants_frame`]: menu open
//! or overlay on); otherwise nothing at all is recorded, so the output is bit-exact and the UI costs nothing.
//! The pass is a dynamic-rendering scope (no `VkRenderPass`) on the target in `COLOR_ATTACHMENT_OPTIMAL`, load
//! op `LOAD` (the UI blends over the picture), after a barrier that orders it after `encode`'s writes.
//!
//! The implementor (`crate::ui::Ui`) builds the ImGui frame inside [`UiPass::draw`]: Dear ImGui's frame borrows
//! its context until it is drawn, so building and recording happen in one call, on the render thread.

use ash::vk;

use super::pipeline::Target;
use super::{barrier, Res};

/// Something drawn over the output after `encode`: the ImGui settings menu and overlay.
pub trait UiPass {
    /// Whether the next frame draws anything. `false`: the pipeline records no UI pass at all.
    fn wants_frame(&self) -> bool;

    /// The colour attachment format the UI renderer was created for. A target of another format gets no UI
    /// pass (the renderer's pipeline would not match it).
    fn format(&self) -> vk::Format;

    /// Builds the UI frame and records its draw commands into `cb`, inside a dynamic-rendering scope the
    /// caller opened on a target of `extent` and [`UiPass::format`]. Texture uploads (the font atlas) may be
    /// submitted to the device queue before this returns, ahead of the frame's own submission.
    ///
    /// # Safety
    /// `cb` is a primary command buffer of the UI's device, recording, inside that scope; it is submitted to
    /// the same queue the UI renderer was created with; the GPU has finished the frame recorded
    /// [`super::FRAMES_IN_FLIGHT`] UI passes ago (the slot fence was waited).
    unsafe fn draw(&mut self, cb: vk::CommandBuffer, extent: vk::Extent2D) -> Res<()>;
}

/// Records the UI pass on `target` (left in `COLOR_ATTACHMENT_OPTIMAL` by `encode`, and still in it after):
/// the barrier after `encode`, the dynamic-rendering scope with load op `LOAD`, the UI's draw commands. The
/// scope is closed also when the UI fails, so that the command buffer stays valid for an abandon.
///
/// # Safety
/// `cb` is recording on `d`, outside any rendering scope; `target` was written by `encode` in this command
/// buffer and is in `COLOR_ATTACHMENT_OPTIMAL`; `ui` satisfies [`UiPass::draw`]'s contract for `cb`.
pub(super) unsafe fn record(d: &ash::Device, cb: vk::CommandBuffer, target: &Target, ui: &mut dyn UiPass) -> Res<()> {
    // `encode` wrote the target as a colour attachment; the UI reads it (blending) and writes it.
    barrier(
        d,
        cb,
        target.image,
        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
        vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
        vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
        vk::AccessFlags2::COLOR_ATTACHMENT_READ | vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
    );
    let attachments = [vk::RenderingAttachmentInfo::default()
        .image_view(target.view)
        .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
        .load_op(vk::AttachmentLoadOp::LOAD)
        .store_op(vk::AttachmentStoreOp::STORE)];
    d.cmd_begin_rendering(
        cb,
        &vk::RenderingInfo::default()
            .render_area(vk::Rect2D {
                offset: vk::Offset2D::default(),
                extent: target.extent,
            })
            .layer_count(1)
            .color_attachments(&attachments),
    );
    let r = ui.draw(cb, target.extent);
    d.cmd_end_rendering(cb);
    r
}

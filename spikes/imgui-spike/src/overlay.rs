//! Dear ImGui on our ash device: a context, the `dear-imgui-ash` renderer in dynamic-rendering mode
//! (no `VkRenderPass`), and the settings-menu content shared by the headless tests and the live
//! harness.

use ash::vk;
use dear_imgui_ash::{AshRenderer, AshRendererConfig, DynamicRendering, Options, TextureRetirementBatch};
use dear_imgui_rs::{Condition, ConfigFlags, Context as ImContext, StyleColor, Ui, WindowFlags};

use crate::gpu::Context;
use crate::input::{self, UiEvent};
use crate::Res;

/// What the colour attachment holds when the overlay pass begins.
#[derive(Clone, Copy, Debug)]
pub enum Load {
    /// Clear to this colour first (UNORM values).
    Clear([f32; 4]),
    /// Keep the content (overlay on top of the CRT output).
    Keep,
}

/// State edited by the settings menu (stand-ins for real parameters).
#[derive(Clone, Debug, PartialEq)]
pub struct MenuState {
    pub exposure: f32,
    pub mask: usize,
    pub name: String,
    pub applied: u32,
    /// One-shot request: give the keyboard focus to the text field in the next frame.
    pub focus_text: bool,
}

impl Default for MenuState {
    fn default() -> Self {
        Self {
            exposure: 0.5,
            mask: 0,
            name: String::new(),
            applied: 0,
            focus_text: false,
        }
    }
}

pub const MASKS: [&str; 3] = ["aperture grille", "slot mask", "delta dot"];

/// Window geometry of the settings menu (top-left corner and size, in pixels).
pub const MENU_POS: [f32; 2] = [10.0, 10.0];
pub const MENU_SIZE: [f32; 2] = [300.0, 200.0];

/// Screen rectangles (min, max) of the menu widgets, as laid out in the frame just built.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MenuRects {
    pub slider: [[f32; 2]; 2],
    pub combo: [[f32; 2]; 2],
    pub text: [[f32; 2]; 2],
    pub button: [[f32; 2]; 2],
}

impl MenuRects {
    /// A point inside `r` at fractions (`fx`, `fy`) of its size.
    pub fn at(r: [[f32; 2]; 2], fx: f32, fy: f32) -> [f32; 2] {
        [r[0][0] + (r[1][0] - r[0][0]) * fx, r[0][1] + (r[1][1] - r[0][1]) * fy]
    }
}

/// The settings menu: a fixed window with a slider, a combo, a text field and a button.
pub fn settings_menu(ui: &Ui, s: &mut MenuState) -> MenuRects {
    let mut rects = MenuRects::default();
    let rect = |ui: &Ui| [ui.item_rect_min(), ui.item_rect_max()];
    ui.window("Settings (F12)")
        .position(MENU_POS, Condition::Always)
        .size(MENU_SIZE, Condition::Always)
        .flags(
            WindowFlags::NO_RESIZE | WindowFlags::NO_MOVE | WindowFlags::NO_COLLAPSE | WindowFlags::NO_SAVED_SETTINGS,
        )
        .build(|| {
            ui.slider("exposure", 0.0f32, 1.0f32, &mut s.exposure);
            rects.slider = rect(ui);
            ui.combo_simple_string("mask", &mut s.mask, &MASKS);
            rects.combo = rect(ui);
            if std::mem::take(&mut s.focus_text) {
                ui.set_keyboard_focus_here();
            }
            ui.input_text("name", &mut s.name).build();
            rects.text = rect(ui);
            if ui.button("Apply") {
                s.applied += 1;
            }
            rects.button = rect(ui);
        });
    rects
}

/// Read-only overlay (stats): never takes input.
pub fn stats_overlay(ui: &Ui, lines: &[String], pos: [f32; 2]) {
    ui.window("stats")
        .position(pos, Condition::Always)
        .flags(
            WindowFlags::NO_INPUTS
                | WindowFlags::NO_TITLE_BAR
                | WindowFlags::ALWAYS_AUTO_RESIZE
                | WindowFlags::NO_SAVED_SETTINGS
                | WindowFlags::NO_FOCUS_ON_APPEARING
                | WindowFlags::NO_NAV,
        )
        .build(|| {
            for l in lines {
                ui.text(l);
            }
        });
}

/// ImGui context plus the ash renderer. Call [`Overlay::destroy`] before the Vulkan context drops.
pub struct Overlay {
    pub imgui: ImContext,
    renderer: Option<AshRenderer>,
    pool: vk::CommandPool,
    device: ash::Device,
    retire: Option<TextureRetirementBatch>,
}

impl Overlay {
    /// Creates the ImGui context (no ini file) and the renderer for colour attachments of `format`.
    ///
    /// # Safety
    /// `ctx` must outlive the overlay until [`Overlay::destroy`]; every command buffer recorded by
    /// [`Overlay::record`] is submitted to `ctx.queue`.
    pub unsafe fn new(ctx: &Context, format: vk::Format, in_flight_frames: usize) -> Res<Self> {
        let mut imgui = ImContext::create();
        imgui.set_ini_filename(None::<String>)?;
        imgui.set_log_filename(None::<String>)?;
        // Keyboard navigation (arrows, Enter, Tab between widgets) inside the menu.
        let flags = imgui.io().config_flags() | ConfigFlags::NAV_ENABLE_KEYBOARD;
        imgui.io_mut().set_config_flags(flags);
        let pool = ctx.device.create_command_pool(
            &vk::CommandPoolCreateInfo::default()
                .queue_family_index(ctx.queue_family)
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
            None,
        )?;
        let config = AshRendererConfig::with_dynamic_rendering(
            ctx.device.clone(),
            ctx.queue,
            pool,
            DynamicRendering {
                color_attachment_format: format,
                depth_attachment_format: None,
            },
        )
        .with_options(Options {
            in_flight_frames,
            ..Options::default()
        });
        let renderer = match AshRenderer::with_default_allocator(&ctx.instance, ctx.physical, config, &mut imgui) {
            Ok(r) => r,
            Err(e) => {
                ctx.device.destroy_command_pool(pool, None);
                return Err(e.into());
            }
        };
        Ok(Self {
            imgui,
            renderer: Some(renderer),
            pool,
            device: ctx.device.clone(),
            retire: None,
        })
    }

    /// Feeds input events into the ImGui IO.
    pub fn feed(&mut self, events: &[UiEvent]) {
        let io = self.imgui.io_mut();
        for &e in events {
            input::apply(io, e);
        }
    }

    /// Builds one ImGui frame with `build`, then records the overlay pass into `cb`: dynamic
    /// rendering on `view` (layout `COLOR_ATTACHMENT_OPTIMAL`, which the caller sets), `load`
    /// first, then the ImGui draw lists. Texture uploads (font atlas) are submitted by the renderer
    /// to the queue before this returns.
    ///
    /// # Safety
    /// `cb` is recording on the device, `view` is a live view of a colour attachment of the format
    /// given to [`Overlay::new`] and size `extent`, in `COLOR_ATTACHMENT_OPTIMAL`; the previous use
    /// of the renderer's mesh slot (the frame `in_flight_frames` ago) has completed on the GPU.
    pub unsafe fn record(
        &mut self,
        cb: vk::CommandBuffer,
        view: vk::ImageView,
        extent: vk::Extent2D,
        load: Load,
        delta_s: f32,
        build: impl FnOnce(&Ui),
    ) -> Res<()> {
        let renderer = self.renderer.as_mut().ok_or("overlay destroyed")?;
        {
            let io = self.imgui.io_mut();
            io.set_display_size([extent.width as f32, extent.height as f32]);
            io.set_delta_time(delta_s.max(1e-4));
        }
        let frame = self.imgui.begin_frame();
        build(frame.ui());
        let pending = frame.render(renderer.renderer_consumer()?);
        let (reconciled, retire) = renderer.prepare_frame(pending)?;
        if retire.is_some() {
            self.retire = retire;
        }
        let (load_op, clear) = match load {
            Load::Clear(c) => (vk::AttachmentLoadOp::CLEAR, c),
            Load::Keep => (vk::AttachmentLoadOp::LOAD, [0.0; 4]),
        };
        let attachments = [vk::RenderingAttachmentInfo::default()
            .image_view(view)
            .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .load_op(load_op)
            .store_op(vk::AttachmentStoreOp::STORE)
            .clear_value(vk::ClearValue {
                color: vk::ClearColorValue { float32: clear },
            })];
        let info = vk::RenderingInfo::default()
            .render_area(vk::Rect2D {
                offset: vk::Offset2D::default(),
                extent,
            })
            .layer_count(1)
            .color_attachments(&attachments);
        self.device.cmd_begin_rendering(cb, &info);
        let r = renderer.cmd_draw(cb, reconciled);
        self.device.cmd_end_rendering(cb);
        r?;
        Ok(())
    }

    /// After the submission of the last recorded frame has completed: releases textures ImGui
    /// retired (none in practice for this spike: the font atlas lives as long as the context).
    pub fn after_gpu_idle(&mut self) -> Res<()> {
        if let (Some(r), Some(batch)) = (self.renderer.as_mut(), self.retire.take()) {
            r.wait_for_texture_retirements(batch)?;
        }
        Ok(())
    }

    /// Shuts the renderer down (waits for the device) and destroys the command pool.
    pub fn destroy(&mut self) -> Res<()> {
        if let Some(mut r) = self.renderer.take() {
            r.shutdown(&mut self.imgui)?;
        }
        if self.pool != vk::CommandPool::null() {
            // SAFETY: the renderer is shut down (device idle), nothing uses the pool any more.
            unsafe { self.device.destroy_command_pool(self.pool, None) };
            self.pool = vk::CommandPool::null();
        }
        Ok(())
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        // Dear ImGui aborts the process when a context is destroyed with a renderer still attached
        // (for example while unwinding from a failed assertion): shut the renderer down first.
        let _ = self.destroy();
    }
}

/// Sets the style colours used by the headless tests to known values and removes rounding and
/// borders, so that pixels can be checked exactly.
pub fn test_style(imgui: &mut ImContext, window_bg: [f32; 4]) {
    let style = imgui.style_mut();
    style.set_window_rounding(0.0);
    style.set_color(StyleColor::WindowBg, window_bg);
    style.set_color(StyleColor::Text, [0.0, 1.0, 0.0, 1.0]);
    style.set_color(StyleColor::SliderGrab, [1.0, 0.0, 0.0, 1.0]);
    style.set_color(StyleColor::SliderGrabActive, [1.0, 0.0, 0.0, 1.0]);
}

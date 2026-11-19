//! The settings menu and the stats overlay (T-52a, T-52b, ARCHITECTURE A17, TEC-5, OUT-3, D-7, D-17).
//!
//! Dear ImGui (`dear-imgui-rs`, `dear-imgui-ash` in dynamic-rendering mode, pinned `=0.22.0`, chosen by T-08)
//! drawn inside the output window, over the picture, after `encode` ([`crate::gpu::UiPass`]): no second
//! window, no platform backend (the output window's raw Win32 messages are translated by [`input`]).
//!
//! [`Ui`] owns the ImGui context and the ImGui Vulkan renderer. Lifetime and order (also see `Drop for Ui`):
//! - created after the Vulkan device ([`Ui::new`] takes the renderer's [`Context`]), on the render thread;
//! - destroyed before the device: `Drop for Ui` shuts the ImGui renderer down (it idles the device), destroys
//!   its command pool, and only then lets the ImGui context drop. Dear ImGui aborts the process when a context
//!   is destroyed with a renderer still attached, so the shutdown must come first, also while unwinding;
//! - one ImGui context per process (Dear ImGui keeps the current context in a global): a second `Ui` may only
//!   be created after the first is dropped (tests serialise on a mutex).
//!
//! Per frame, on the render thread: [`Ui::frame`] hands over the state to show (parameters, readouts, timings)
//! and returns what the user changed in the previous frame ([`UiOutput`]); the renderer then calls
//! [`crate::gpu::UiPass::draw`] inside its frame, which builds the ImGui frame (NewFrame, the windows, Render),
//! uploads the textures the frame needs (the font atlas, on the device queue, before the frame's submission)
//! and records the draw lists. The menu and the overlay cost nothing while both are hidden: no ImGui frame and
//! no GPU pass.
//!
//! Input: while the menu is open, the output window queues its input messages (`crate::output`), and the app
//! passes them to [`Ui::handle_win32_message`]. The overlay never takes input.

pub mod input;
pub mod menu;
pub mod overlay;

use std::path::PathBuf;
use std::time::Instant;

use ash::vk;
use dear_imgui_ash::{AshRenderer, AshRendererConfig, DynamicRendering, Options, TextureRetirementBatch};
use dear_imgui_rs::{ConfigFlags, Context as ImContext, MouseCursor};

use crate::gpu::{Context, FrameTimings, UiPass, FRAMES_IN_FLIGHT};
use crate::params::Params;
use crate::presets::Library;
use crate::{settings, Res};

use input::{UiEvent, Win32Input};
use menu::{CloseReason, Menu, MenuModel, MenuRects};

/// Default menu key (D-17: F10, since F12 is registered system-wide by another program on the dev machine).
pub const DEFAULT_MENU_KEY: &str = "F10";

/// Longest frame time handed to ImGui (after the UI was hidden for a while).
const MAX_DELTA_S: f32 = 0.1;

/// What the UI shows in the next frame, given by the app every frame ([`Ui::frame`]).
#[derive(Clone, Debug, Default)]
pub struct UiState {
    /// The parameters in use (the menu edits a copy and returns it in [`UiOutput::params`]).
    pub params: Params,
    /// The id of the preset the parameters come from, if any.
    pub preset: Option<String>,
    /// SDR headroom readout of the last frame (`Renderer::headroom`, T-84).
    pub headroom: Option<crt_model::output::Headroom>,
    /// Placement of the source in the output (`placement::readout`, T-96).
    pub placement: Option<crate::placement::Readout>,
    /// Mean GPU timings per stage of the last second (`Renderer::take_timing_average`, T-21).
    pub timings: Option<FrameTimings>,
    pub capture_fps: f64,
    pub render_fps: f64,
}

/// What the user changed through the menu in the frame last drawn ([`Ui::frame`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UiOutput {
    /// New parameters to use (a widget moved or a preset was selected).
    pub params: Option<Params>,
    /// The preset selected (its id); `params` holds its parameters, before any source profile.
    pub preset: Option<String>,
}

/// How a [`Ui`] is set up.
pub struct UiConfig {
    /// The output window (`HWND` as an integer), or `None` (headless: no focus hand-off, no cursor).
    pub output: Option<isize>,
    /// The captured window: the fallback for the foreground when the menu closes.
    pub source: Option<isize>,
    /// Where `settings.toml` lives: the menu layout is saved there when the menu closes. `None`: never saved.
    pub settings_dir: Option<PathBuf>,
    /// Menu position, size, opacity and collapsed state.
    pub layout: settings::UiState,
    /// The presets the selector lists.
    pub presets: Library,
    /// The menu key, as shown in the menu.
    pub menu_key: String,
}

impl UiConfig {
    /// The live app: the output and source windows, the layout and presets of `%APPDATA%\ultimateCRT`.
    pub fn live(output: isize, source: isize) -> Self {
        let dir = settings::default_dir();
        let global = dir
            .as_deref()
            .map(|d| settings::load(d).settings.global)
            .unwrap_or_default();
        let presets = Library::load(dir.as_deref().map(crate::presets::user_dir).as_deref());
        Self {
            output: Some(output),
            source: (source != 0).then_some(source),
            layout: global.ui.clone(),
            menu_key: global
                .hotkeys
                .get("toggle_menu")
                .cloned()
                .unwrap_or_else(|| DEFAULT_MENU_KEY.to_string()),
            settings_dir: dir,
            presets,
        }
    }

    /// No window, default layout, the embedded presets, nothing saved (tests, offline use).
    pub fn headless() -> Self {
        Self {
            output: None,
            source: None,
            settings_dir: None,
            layout: settings::UiState::default(),
            presets: Library::embedded(),
            menu_key: DEFAULT_MENU_KEY.to_string(),
        }
    }
}

/// The ImGui context, its Vulkan renderer, the menu and the overlay. See the module documentation for the
/// creation and destruction order.
pub struct Ui {
    renderer: Option<AshRenderer>,
    pool: vk::CommandPool,
    device: ash::Device,
    format: vk::Format,
    /// Textures ImGui retired in a frame; released at the next draw, after a device idle.
    retire: Option<TextureRetirementBatch>,
    input: Win32Input,
    menu: Menu,
    overlay: bool,
    output: Option<isize>,
    source: Option<isize>,
    settings_dir: Option<PathBuf>,
    layout: settings::UiState,
    /// The layout as last saved (or loaded).
    saved_layout: settings::UiState,
    presets: Library,
    menu_key: String,
    /// What the next frame shows; the menu edits `state.params` in place.
    state: UiState,
    /// What the frames drawn since the last [`Ui::frame`] changed.
    out: UiOutput,
    filter: String,
    /// Give the keyboard focus to the menu window in the next frame (set when it opens).
    focus_menu: bool,
    rects: MenuRects,
    overlay_rect: [[f32; 2]; 2],
    cursor: Option<MouseCursor>,
    dpi: f32,
    last: Option<Instant>,
    /// Dropped last (declared last, and `Drop for Ui` shuts the renderer down before).
    imgui: ImContext,
}

impl Ui {
    /// Creates the ImGui context (no ini or log file, keyboard navigation on) and its Vulkan renderer for
    /// colour attachments of `format` (the output's swapchain format, `Renderer::output_format`), with
    /// [`FRAMES_IN_FLIGHT`] mesh buffers. Fails when another `Ui` (another ImGui context) exists.
    ///
    /// The `Ui` must be dropped before `ctx` (before the `Renderer` or `Headless` that owns it); its frames
    /// must be drawn by a pipeline on `ctx`, whose submissions go to `ctx.queue`.
    pub fn new(ctx: &Context, format: vk::Format, config: UiConfig) -> Res<Ui> {
        let mut imgui = ImContext::create();
        imgui.set_ini_filename(None::<String>)?;
        imgui.set_log_filename(None::<String>)?;
        // Keyboard navigation (arrows, Enter, Tab between widgets) inside the menu.
        let flags = imgui.io().config_flags() | ConfigFlags::NAV_ENABLE_KEYBOARD;
        imgui.io_mut().set_config_flags(flags);
        // SAFETY: the pool is created on the live device of `ctx` and destroyed in `Drop for Ui` after the
        // renderer's shutdown (device idle), or below when the renderer cannot be created.
        let pool = unsafe {
            ctx.device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(ctx.queue_family)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )?
        };
        let config_vk = AshRendererConfig::with_dynamic_rendering(
            ctx.device.clone(),
            ctx.queue,
            pool,
            DynamicRendering {
                color_attachment_format: format,
                depth_attachment_format: None,
            },
        )
        .with_options(Options {
            in_flight_frames: FRAMES_IN_FLIGHT,
            ..Options::default()
        });
        // SAFETY: instance, physical device, device, queue and pool are the live objects of `ctx`; the renderer
        // is shut down in `Drop for Ui`, before the device (the caller drops the `Ui` before `ctx`).
        let created =
            unsafe { AshRenderer::with_default_allocator(&ctx.instance, ctx.physical, config_vk, &mut imgui) };
        let renderer = match created {
            Ok(r) => r,
            Err(e) => {
                // SAFETY: nothing uses the pool yet.
                unsafe { ctx.device.destroy_command_pool(pool, None) };
                return Err(e.into());
            }
        };
        let mut ui = Ui {
            renderer: Some(renderer),
            pool,
            device: ctx.device.clone(),
            format,
            retire: None,
            input: Win32Input::new(),
            menu: Menu::default(),
            overlay: false,
            output: config.output,
            source: config.source,
            settings_dir: config.settings_dir,
            saved_layout: config.layout.clone(),
            layout: config.layout,
            presets: config.presets,
            menu_key: config.menu_key,
            state: UiState::default(),
            out: UiOutput::default(),
            filter: String::new(),
            focus_menu: false,
            rects: MenuRects::default(),
            overlay_rect: [[0.0; 2]; 2],
            cursor: None,
            dpi: 1.0,
            last: None,
            imgui,
        };
        if let Some(out) = ui.output {
            // Per-monitor DPI (the process is per-monitor aware v2): sizes and fonts follow the monitor.
            // SAFETY: plain query on the output window.
            let dpi = unsafe {
                windows::Win32::UI::HiDpi::GetDpiForWindow(windows::Win32::Foundation::HWND(
                    out as *mut std::ffi::c_void,
                ))
            };
            ui.set_dpi_scale(input::dpi_scale(dpi.max(96)));
        }
        Ok(ui)
    }

    /// Opens the settings menu when it is closed, closes it when it is open (the menu key, T-54).
    /// Opening takes the foreground from the source and routes the output's input to the menu; closing gives
    /// both back ([`menu::Menu`]). Call it from the hotkey handler: `SetForegroundWindow` is allowed there.
    pub fn request_toggle(&mut self) {
        if self.menu.is_open() {
            self.close(CloseReason::Toggle);
        } else {
            self.open();
        }
    }

    /// Shows or hides the read-only stats overlay (T-52b). It never takes input or focus.
    pub fn toggle_overlay(&mut self) {
        self.overlay = !self.overlay;
    }

    /// Whether the settings menu is open.
    pub fn is_open(&self) -> bool {
        self.menu.is_open()
    }

    /// Whether the stats overlay is shown.
    pub fn overlay_on(&self) -> bool {
        self.overlay
    }

    /// Whether the UI takes the output's mouse and keyboard input (the menu is open).
    pub fn wants_input(&self) -> bool {
        self.menu.is_open()
    }

    /// Feeds one raw message of the output window (`msg`, `WPARAM`, `LPARAM` as integers) to the UI. Returns
    /// true when the UI consumed it (the window procedure answers it without `DefWindowProc`: it already did
    /// so for the messages it queued, by [`input::consumes`]). While the menu is closed only DPI changes are
    /// used. A deactivation of the output while the menu is open closes the menu without taking the
    /// foreground back ([`menu::on_activate`]).
    pub fn handle_win32_message(&mut self, msg: u32, wparam: usize, lparam: isize) -> bool {
        if msg == input::WM_ACTIVATE {
            if let Some(reason) = menu::on_activate(self.menu.is_open(), wparam & 0xFFFF != input::WA_INACTIVE) {
                self.close(reason);
            }
            return false;
        }
        let open = self.menu.is_open();
        let mut events = Vec::new();
        let used = self.input.translate(msg, wparam, lparam, &mut events);
        for e in events {
            match e {
                UiEvent::DpiScale(s) => self.set_dpi_scale(s),
                e if open => input::apply(self.imgui.io_mut(), e),
                _ => {}
            }
        }
        used && open && input::consumes(msg)
    }

    /// Starts a frame: takes `state` as what the next drawn frame shows, and returns what the user changed in
    /// the frames drawn since the last call. The app applies [`UiOutput::params`] (from the next frame on);
    /// the next frame already shows them. Cheap while the UI is hidden (nothing is copied).
    pub fn frame(&mut self, state: &UiState) -> UiOutput {
        let out = std::mem::take(&mut self.out);
        if self.wants_frame() {
            self.state = state.clone();
            if let Some(p) = out.params {
                self.state.params = p;
            }
            if out.preset.is_some() {
                self.state.preset = out.preset.clone();
            }
        }
        out
    }

    /// Updates the captured window (after a reattach, T-56): the fallback for the foreground on close.
    pub fn set_source(&mut self, source: Option<isize>) {
        self.source = source.filter(|&h| h != 0);
    }

    /// The widget rectangles of the menu in the last frame drawn (tests, live check).
    pub fn menu_rects(&self) -> MenuRects {
        self.rects
    }

    /// The overlay window rectangle (min, max) in the last frame drawn.
    pub fn overlay_rect(&self) -> [[f32; 2]; 2] {
        self.overlay_rect
    }

    /// The preset filter text of the menu.
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// The menu layout (position, size, opacity, collapsed) as the last frame left it.
    pub fn layout(&self) -> &settings::UiState {
        &self.layout
    }

    /// The window the last close gave the foreground to.
    pub fn last_restore(&self) -> Option<isize> {
        self.menu.last_restore
    }

    /// The ImGui context (style, fonts): tests set known colours; T-53 may add fonts.
    pub fn imgui(&mut self) -> &mut ImContext {
        &mut self.imgui
    }

    fn open(&mut self) {
        self.menu.open(self.output);
        let mut events = Vec::new();
        if self.output.is_some() {
            // Modifiers already held when the menu opens (the menu key's own chord).
            self.input.set_modifiers(modifiers_now(), &mut events);
        }
        events.push(UiEvent::Focus(true));
        for e in events {
            input::apply(self.imgui.io_mut(), e);
        }
        self.focus_menu = true;
    }

    fn close(&mut self, reason: CloseReason) {
        self.menu.close(self.output, self.source, reason);
        let mut events = Vec::new();
        self.input.set_modifiers(input::Modifiers::default(), &mut events);
        events.extend([UiEvent::MousePos([-f32::MAX, -f32::MAX]), UiEvent::Focus(false)]);
        for e in events {
            input::apply(self.imgui.io_mut(), e);
        }
        self.cursor = None;
        self.save_layout();
    }

    /// Writes the menu layout to `settings.toml` when it changed (never over a file that failed to load).
    fn save_layout(&mut self) {
        let Some(dir) = &self.settings_dir else { return };
        if self.layout == self.saved_layout {
            return;
        }
        let loaded = settings::load(dir);
        if loaded.warning.is_some() {
            return;
        }
        let mut s = loaded.settings;
        s.global.ui = self.layout.clone();
        match settings::save(dir, &s) {
            Ok(()) => self.saved_layout = self.layout.clone(),
            Err(e) => eprintln!("warning: cannot save the menu layout: {e}"),
        }
    }

    fn set_dpi_scale(&mut self, scale: f32) {
        if scale > 0.0 && scale.is_finite() && (scale - self.dpi).abs() > 1e-3 {
            let style = self.imgui.style_mut();
            style.scale_all_sizes(scale / self.dpi);
            style.set_font_scale_dpi(scale);
            self.dpi = scale;
        }
    }
}

/// Modifier keys held now (`GetKeyState`), for the menu opening while the chord is still down.
fn modifiers_now() -> input::Modifiers {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT};
    // SAFETY: plain key-state queries.
    let down =
        |vk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY| unsafe { GetKeyState(i32::from(vk.0)) } < 0;
    input::Modifiers {
        ctrl: down(VK_CONTROL),
        shift: down(VK_SHIFT),
        alt: down(VK_MENU),
        win: down(VK_LWIN) || down(VK_RWIN),
    }
}

impl UiPass for Ui {
    fn wants_frame(&self) -> bool {
        self.menu.is_open() || self.overlay
    }

    fn format(&self) -> vk::Format {
        self.format
    }

    unsafe fn draw(&mut self, cb: vk::CommandBuffer, extent: vk::Extent2D) -> Res<()> {
        let renderer = self.renderer.as_mut().ok_or("the UI renderer is shut down")?;
        if let Some(batch) = self.retire.take() {
            // Rare (font atlas rebuilt): idles the device; the frame being recorded uses no ImGui texture yet.
            renderer.wait_for_texture_retirements(batch)?;
        }
        let now = Instant::now();
        let dt = self
            .last
            .map_or(1.0 / 60.0, |t| now.duration_since(t).as_secs_f32())
            .clamp(1e-4, MAX_DELTA_S);
        self.last = Some(now);
        let display = [extent.width as f32, extent.height as f32];
        {
            let io = self.imgui.io_mut();
            io.set_display_size(display);
            io.set_delta_time(dt);
        }
        let open = self.menu.is_open();
        let mut result = None;
        let mut overlay_rect = None;
        let frame = self.imgui.begin_frame();
        let ui = frame.ui();
        if self.overlay {
            let last = self.overlay_rect;
            let size = [last[1][0] - last[0][0], last[1][1] - last[0][1]];
            overlay_rect = Some(overlay::build(ui, &overlay::lines(&self.state), display, size));
        }
        if open {
            let focus = std::mem::take(&mut self.focus_menu);
            result = Some(menu::build(
                ui,
                MenuModel {
                    state: &self.state.clone(),
                    params: &mut self.state.params,
                    filter: &mut self.filter,
                    layout: &mut self.layout,
                    library: &self.presets,
                    key: &self.menu_key,
                    focus,
                },
            ));
        }
        self.cursor = ui.mouse_cursor();
        let pending = frame.render(renderer.renderer_consumer()?);
        let (reconciled, retire) = renderer.prepare_frame(pending)?;
        if retire.is_some() {
            self.retire = retire;
        }
        renderer.cmd_draw(cb, reconciled)?;
        if let Some(r) = overlay_rect {
            self.overlay_rect = r;
        }
        if let Some(r) = result {
            self.rects = r.rects;
            if r.params_changed {
                self.out.params = Some(self.state.params);
            }
            if r.preset.is_some() {
                self.out.preset = r.preset.clone();
                self.state.preset = r.preset;
            }
        }
        if open && self.output.is_some() {
            // The cursor shape ImGui asks for, shown over the client area by the window procedure.
            crate::output::set_menu_cursor(input::cursor_id(self.cursor));
        }
        Ok(())
    }
}

impl Drop for Ui {
    fn drop(&mut self) {
        // The menu open at exit: give the focus and the window styles back first.
        if self.menu.is_open() {
            self.close(CloseReason::Toggle);
        }
        // Dear ImGui aborts the process when its context is destroyed with a renderer still attached (also
        // while unwinding from a panic): shut the renderer down (it idles the device) before `imgui` drops.
        if let Some(mut r) = self.renderer.take() {
            if let Err(e) = r.shutdown(&mut self.imgui) {
                eprintln!("warning: ImGui renderer shutdown: {e}");
            }
        }
        if self.pool != vk::CommandPool::null() {
            // SAFETY: the renderer is shut down (device idle); nothing uses the pool any more.
            unsafe { self.device.destroy_command_pool(self.pool, None) };
            self.pool = vk::CommandPool::null();
        }
    }
}

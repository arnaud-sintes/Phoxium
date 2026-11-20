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
//!
//! Hotkeys (T-54): [`Ui`] implements `crate::controls::UiHook`; the menu key (default F10, fallbacks
//! Ctrl+Alt+F10 and Ctrl+Alt+F12) calls [`Ui::request_toggle`], the overlay key (Ctrl+Alt+O)
//! [`Ui::toggle_overlay`]. The registration problems are shown in the menu ([`UiConfig::hotkey_problems`]).
//! Settings: the UI holds no settings of its own; the app's session settings (`live::Live`) give the layout
//! and take it back ([`Ui::take_layout_change`]); parameter edits, the preset, a new preset library and the
//! adapter choice go back through [`UiOutput`] (T-53, the panels in [`panels`]).

pub mod input;
pub mod menu;
pub mod overlay;
pub mod panels;
pub mod picker;
pub mod theme;

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
use menu::{CloseReason, HotkeyRow, Menu, MenuModel, MenuRects, PresetForm, SystemModel};

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
    /// What the parameters are compared against (the modified marks) and reset to: the preset in force with
    /// the source profile and the command-line options on top, as the app computes it. `None`: the preset of
    /// `preset` from the menu's library, else the table defaults.
    pub baseline: Option<Params>,
    /// SDR headroom readout of the last frame (`Renderer::headroom`, T-84).
    pub headroom: Option<crt_model::output::Headroom>,
    /// Placement of the source in the output (`placement::readout`, T-96).
    pub placement: Option<crate::placement::Readout>,
    /// Mean GPU timings per stage of the last second (`Renderer::take_timing_average`, T-21).
    pub timings: Option<FrameTimings>,
    pub capture_fps: f64,
    pub render_fps: f64,
    /// The source as the menu's Source section shows it (A23).
    pub source: SourceView,
}

/// The captured window as the menu shows it (A23): what was attached and what the automatic handling found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SourceView {
    /// The attached window, `None` without one.
    pub attached: Option<AttachedView>,
    /// While a lost source is waited for: what is waited for and for how long (`pixel-demo.exe (class), 12 s
    /// left`).
    pub waiting: Option<String>,
}

/// One attached window and what was detected on it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AttachedView {
    pub exe: Option<String>,
    pub class: String,
    pub title: String,
    /// Captured frame size (px).
    pub frame: (u32, u32),
    /// Native raster after the crop and the divisor.
    pub native: (u32, u32),
    /// The integer scale in words: `x3 detected`, `x2 x 3 (manual)`, `detecting...`, `none (x1)`.
    pub scale: String,
    /// The capture area in words: `client area`, `manual 8, 4, 320 x 240`, `whole window`.
    pub crop: String,
    /// A source profile (`[[source]]` for this program and class) is applied.
    pub profile: bool,
}

/// The source selection settings the menu and the picker edit (`[global.source]`, A23).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SourcePrefs {
    pub ask_at_start: bool,
    /// Seconds a lost source is waited for; 0 forever.
    pub wait_s: f64,
}

impl Default for SourcePrefs {
    fn default() -> Self {
        let d = settings::SourceSettings::default();
        Self {
            ask_at_start: d.ask_at_start,
            wait_s: d.wait_s,
        }
    }
}

/// A source request of the picker or the menu's Source section (A23), for the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceAction {
    /// Capture this window (the picker's Capture, Enter, double-click).
    Attach(isize),
    /// The picker was closed without a choice; the source still attached stays.
    Cancel,
    /// Stop capturing (the menu's Detach): the "Choose a window" screen comes back.
    Detach,
    /// Open the picker with the source still attached (the menu's Change source).
    ChangeSource,
    /// Enumerate the windows again now.
    Refresh,
    /// Take the preview again now.
    RefreshPreview,
}

/// Which content the open menu mode shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// The settings menu (the menu key).
    Settings,
    /// The window picker (no source, or Change source).
    Picker,
}

/// What the user changed through the menu in the frame last drawn ([`Ui::frame`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UiOutput {
    /// New parameters to use (a widget moved or a preset was selected).
    pub params: Option<Params>,
    /// The preset selected (its id); `params` holds its parameters, before any source profile. Also set when a
    /// user preset was saved or updated (it is selected), then `params` are the current parameters.
    pub preset: Option<String>,
    /// The preset library after a save, an update or a reload (the app hands it to the preset hotkeys).
    pub library: Option<Library>,
    /// The `[global.gpu] adapter` value chosen in the menu, for the settings (used at the next start).
    pub gpu_adapter: Option<String>,
    /// A burn-in action (T-43), for `tube_state::TubeState`.
    pub wear_action: Option<WearAction>,
    /// A source request (A23): attach, cancel, detach, change source, refresh.
    pub source: Option<SourceAction>,
    /// `[global.source]` changed in the picker or the menu (A23).
    pub source_prefs: Option<SourcePrefs>,
}

/// A burn-in action of the menu's ageing panel (T-43).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WearAction {
    /// Wear the current picture for this many hours at once.
    BurnInNow(f32),
    /// Start a new tube: the wear map cleared and its file replaced.
    NewTube,
}

/// How a [`Ui`] is set up.
pub struct UiConfig {
    /// The output window (`HWND` as an integer), or `None` (headless: no focus hand-off, no cursor).
    pub output: Option<isize>,
    /// The captured window: the fallback for the foreground when the menu closes.
    pub source: Option<isize>,
    /// Menu position, size, opacity and collapsed state.
    pub layout: settings::UiState,
    /// The presets the selector lists.
    pub presets: Library,
    /// The menu key, as shown in the menu (the chord registered for the menu, which may be a fallback).
    pub menu_key: String,
    /// Hotkey registration problems (a chord taken by another program, a fallback in force, an action
    /// without a hotkey), one line each, shown in the menu (`hotkeys::Registration::problems`).
    pub hotkey_problems: Vec<String>,
    /// The hotkeys as registered, listed in the GPU and system panel.
    pub hotkeys: Vec<HotkeyRow>,
    /// The user preset folder (`presets::user_dir`): "Save as new" writes there. `None`: no saving.
    pub presets_dir: Option<PathBuf>,
    /// The settings folder, shown in the GPU and system panel.
    pub settings_dir: Option<PathBuf>,
    /// The session's adapter selection and list (`adapters::current`).
    pub gpu: Option<crate::adapters::Current>,
    /// `[global.gpu] adapter` as the settings hold it now.
    pub gpu_setting: String,
    /// `[global.source]` ask at start and lost-source wait (A23).
    pub source_prefs: SourcePrefs,
}

impl UiConfig {
    /// The live app: the output and source windows, and what the shared session settings, the preset library,
    /// the hotkey registration and the adapter selection hold. The UI keeps no settings of its own: the layout,
    /// the parameters and the adapter choice go back to the app ([`Ui::take_layout_change`], [`UiOutput`]).
    pub fn live(
        output: isize,
        source: isize,
        settings: &settings::Settings,
        settings_dir: Option<PathBuf>,
        presets: Library,
        hotkeys: &crate::hotkeys::Registration,
    ) -> Self {
        Self {
            output: Some(output),
            source: (source != 0).then_some(source),
            layout: settings.global.ui.clone(),
            menu_key: hotkeys
                .bound(crate::hotkeys::Action::ToggleMenu)
                .map_or_else(|| "(no menu hotkey)".to_string(), |c| c.to_string()),
            hotkey_problems: hotkeys.problems(),
            hotkeys: hotkey_rows(hotkeys),
            presets,
            presets_dir: settings_dir.as_deref().map(crate::presets::user_dir),
            settings_dir,
            gpu: crate::adapters::current().cloned(),
            gpu_setting: settings.global.gpu.adapter.clone(),
            source_prefs: SourcePrefs {
                ask_at_start: settings.global.source.ask_at_start,
                wait_s: settings.global.source.wait_s,
            },
        }
    }

    /// No window, default layout, the embedded presets, nothing saved (tests, offline use).
    pub fn headless() -> Self {
        Self {
            output: None,
            source: None,
            layout: settings::UiState::default(),
            presets: Library::embedded(),
            menu_key: DEFAULT_MENU_KEY.to_string(),
            hotkey_problems: Vec::new(),
            hotkeys: Vec::new(),
            presets_dir: None,
            settings_dir: None,
            gpu: None,
            gpu_setting: settings::GPU_ADAPTER_AUTO.to_string(),
            source_prefs: SourcePrefs::default(),
        }
    }
}

/// The hotkey rows of the menu from the registration outcome.
pub fn hotkey_rows(reg: &crate::hotkeys::Registration) -> Vec<HotkeyRow> {
    reg.outcomes
        .iter()
        .map(|o| HotkeyRow {
            action: capitalised(o.action.describe()),
            bound: o.bound.map(|c| c.to_string()),
            wanted: o.wanted.to_string(),
        })
        .collect()
}

fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
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
    layout: settings::UiState,
    /// The layout as last handed to the app ([`Ui::take_layout_change`]), or as configured.
    saved_layout: settings::UiState,
    presets: Library,
    menu_key: String,
    hotkey_problems: Vec<String>,
    /// What the next frame shows; the menu edits `state.params` in place.
    state: UiState,
    /// What the frames drawn since the last [`Ui::frame`] changed.
    out: UiOutput,
    filter: String,
    /// Parameter search text and the Advanced toggle.
    search: String,
    advanced: bool,
    preset_form: PresetForm,
    system: SystemModel,
    /// Give the keyboard focus to the menu window in the next frame (set when it opens).
    focus_menu: bool,
    rects: MenuRects,
    /// Widget rectangles by key in the last frame drawn ([`Ui::item_rect`]).
    items: Vec<(String, [[f32; 2]; 2])>,
    overlay_rect: [[f32; 2]; 2],
    cursor: Option<MouseCursor>,
    /// UI scale: the DPI scale of the output's monitor (1 at 96 dpi).
    dpi: f32,
    fonts: theme::Fonts,
    last: Option<Instant>,
    /// A fixed frame time for ImGui instead of the measured one ([`Ui::set_frame_time`]).
    fixed_dt: Option<f32>,
    /// What the open menu mode shows (A23).
    view: View,
    /// No source is attached: the menu mode never closes, it falls back to the picker (A23).
    needs_source: bool,
    /// The window picker (A23).
    picker: picker::Picker,
    /// `[global.source]` as the menu and the picker edit it.
    prefs: SourcePrefs,
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
        // The theme (colours, metrics at 100 %) and its fonts, before the renderer sees the atlas.
        let fonts = theme::load_fonts(&mut imgui);
        theme::apply(imgui.style_mut(), 1.0);
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
            saved_layout: config.layout.clone(),
            layout: config.layout,
            presets: config.presets,
            menu_key: config.menu_key,
            hotkey_problems: config.hotkey_problems,
            state: UiState::default(),
            out: UiOutput::default(),
            filter: String::new(),
            search: String::new(),
            advanced: false,
            preset_form: PresetForm {
                dir: config.presets_dir,
                ..PresetForm::default()
            },
            system: SystemModel {
                gpu: config.gpu,
                gpu_setting: config.gpu_setting,
                hotkeys: config.hotkeys,
                settings_dir: config.settings_dir,
            },
            focus_menu: false,
            rects: MenuRects::default(),
            items: Vec::new(),
            overlay_rect: [[0.0; 2]; 2],
            cursor: None,
            dpi: 1.0,
            fonts,
            last: None,
            fixed_dt: None,
            view: View::Settings,
            needs_source: false,
            picker: picker::Picker {
                ask_at_start: config.source_prefs.ask_at_start,
                ..picker::Picker::default()
            },
            prefs: config.source_prefs,
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
    ///
    /// With the picker shown (A23), the menu key switches to the settings; closing the settings without a source
    /// goes back to the picker (the menu mode stays on: there is nothing else to show).
    pub fn request_toggle(&mut self) {
        if !self.menu.is_open() {
            self.view = View::Settings;
            self.open();
            return;
        }
        match self.view {
            View::Picker => {
                self.view = View::Settings;
                self.focus_menu = true;
            }
            View::Settings if self.needs_source => self.view = View::Picker,
            View::Settings => self.close(CloseReason::Toggle),
        }
    }

    // ---- Source selection (A23) ----

    /// Shows the window picker: opens the menu mode if needed (the output becomes activating and takes the
    /// foreground, as for the menu). `attached`: a source is still captured behind it (Change source: Esc and
    /// Cancel go back to it); without one the picker cannot be closed, only replaced by the settings.
    pub fn show_picker(&mut self, attached: bool) {
        self.picker.attached = attached;
        self.needs_source = !attached;
        self.picker.ask_at_start = self.prefs.ask_at_start;
        self.view = View::Picker;
        if !self.menu.is_open() {
            self.open();
        }
    }

    /// A window was attached (A23): it becomes the source, the picker closes and the foreground goes to that
    /// window (the non-activating behaviour of the output resumes). The picker's message is cleared.
    pub fn attached(&mut self, source: isize) {
        self.set_source(Some(source));
        self.needs_source = false;
        self.picker.attached = true;
        self.picker.message = None;
        if self.menu.is_open() {
            self.close(CloseReason::Attached);
        }
    }

    /// The source is gone (detached, or given up after the wait): the menu mode cannot close any more and the
    /// app shows the picker ([`Ui::show_picker`]).
    pub fn source_gone(&mut self) {
        self.set_source(None);
        self.needs_source = true;
        self.picker.attached = false;
    }

    /// Whether the picker is shown.
    pub fn picker_open(&self) -> bool {
        self.menu.is_open() && self.view == View::Picker
    }

    /// What the open menu mode shows.
    pub fn view(&self) -> View {
        self.view
    }

    /// The picker's state (tests, the app's preview requests).
    pub fn picker(&self) -> &picker::Picker {
        &self.picker
    }

    /// Replaces the picker's window list (sorted by the app).
    pub fn set_picker_rows(&mut self, rows: Vec<crate::picker::Row>) {
        self.picker.set_rows(rows);
    }

    /// The message card of the picker: (good news, text), or none.
    pub fn set_picker_message(&mut self, message: Option<(bool, String)>) {
        self.picker.message = message;
    }

    /// `[global.source]` as the settings hold it (the menu and the picker show it).
    pub fn set_source_prefs(&mut self, prefs: SourcePrefs) {
        self.prefs = prefs;
        self.picker.ask_at_start = prefs.ask_at_start;
    }

    /// The source selection settings shown.
    pub fn source_prefs(&self) -> SourcePrefs {
        self.prefs
    }

    /// The preview of `hwnd` (RGBA, `w` x `h`): replaces the previous one (its texture is released).
    pub fn set_thumbnail(&mut self, hwnd: isize, w: u32, h: u32, rgba: &[u8]) {
        self.drop_thumbnail();
        if let Ok(data) = dear_imgui_rs::OwnedTextureData::from_pixels(dear_imgui_rs::TextureFormat::RGBA32, w, h, rgba)
        {
            let tex = self.imgui.register_texture(data);
            self.picker.thumb = Some(picker::Thumb {
                hwnd,
                tex,
                size: [w as f32, h as f32],
            });
            self.picker.thumb_failed = None;
        }
    }

    /// The preview capture of `hwnd` gave no frame in time.
    pub fn set_thumbnail_failed(&mut self, hwnd: isize) {
        if self.picker.thumb.is_some_and(|t| t.hwnd == hwnd) {
            self.drop_thumbnail();
        }
        self.picker.thumb_failed = Some(hwnd);
    }

    /// Applies a picker action: the app's requests go to [`UiOutput::source`]; Cancel also closes the picker
    /// (the foreground goes back as for the menu key) when a source is attached.
    fn picker_action(&mut self, a: picker::PickerAction) {
        use picker::PickerAction as A;
        match a {
            A::Attach(h) => self.out.source = Some(SourceAction::Attach(h)),
            A::Cancel => {
                if !self.needs_source {
                    self.out.source = Some(SourceAction::Cancel);
                    self.close(CloseReason::Toggle);
                }
            }
            A::Refresh => self.out.source = Some(SourceAction::Refresh),
            A::RefreshPreview => self.out.source = Some(SourceAction::RefreshPreview),
            A::AskAtStart(on) => {
                self.prefs.ask_at_start = on;
                self.out.source_prefs = Some(self.prefs);
            }
        }
    }

    fn drop_thumbnail(&mut self) {
        if let Some(t) = self.picker.thumb.take() {
            let _ = self.imgui.remove_texture(t.tex);
        }
    }

    /// The icon of the executable `path` (RGBA 32 x 32), or `None` when it has none (a placeholder is drawn).
    pub fn set_icon(&mut self, path: &str, icon: Option<(u32, u32, Vec<u8>)>) {
        let tex = icon.and_then(|(w, h, rgba)| {
            dear_imgui_rs::OwnedTextureData::from_pixels(dear_imgui_rs::TextureFormat::RGBA32, w, h, &rgba)
                .ok()
                .map(|d| self.imgui.register_texture(d))
        });
        self.picker.icons.insert(path.to_string(), tex);
    }

    /// The executables listed whose icon is not loaded yet.
    pub fn missing_icons(&self) -> Vec<String> {
        self.picker.missing_icons()
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
    /// foreground back ([`menu::on_activate`]). Esc while the menu is open closes it and gives the foreground
    /// back, like the menu key ([`menu::closes_on_key`]); the output window never sees that Esc (it would
    /// close the output).
    pub fn handle_win32_message(&mut self, msg: u32, wparam: usize, lparam: isize) -> bool {
        if msg == input::WM_ACTIVATE {
            // Without a source the picker stays when the user goes to another window (A23): there is nothing
            // else to show, and closing would bring it back at once, taking the foreground again.
            let closable = !self.needs_source;
            if let Some(reason) =
                menu::on_activate(self.menu.is_open() && closable, wparam & 0xFFFF != input::WA_INACTIVE)
            {
                if self.view == View::Picker {
                    self.out.source = Some(SourceAction::Cancel);
                }
                self.close(reason);
            }
            return false;
        }
        let open = self.menu.is_open();
        // The picker's keys and its search text (A23): handled here, Dear ImGui never sees them.
        if open && self.view == View::Picker {
            if msg == input::WM_KEYDOWN {
                let (used, action) = self.picker.on_key(wparam);
                if let Some(a) = action {
                    self.picker_action(a);
                }
                if used {
                    return true;
                }
            } else if msg == input::WM_CHAR && self.picker.on_char(wparam) {
                return true;
            }
        }
        if menu::closes_on_key(open, msg, wparam) {
            // Esc in the settings: as the menu key (back to the picker without a source).
            self.request_toggle();
            return true;
        }
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
                // The app's baseline is still the old preset's until its next frame.
                self.state.baseline = None;
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

    /// The parameter search text of the menu.
    pub fn search(&self) -> &str {
        &self.search
    }

    /// The rectangle (min, max) of the widget recorded under `key` in the last frame drawn
    /// ([`menu::MenuResult::items`]: `widget:<id>`, `label:<id>`, `header:<panel>`, `save_preset`, ...).
    pub fn item_rect(&self, key: &str) -> Option<[[f32; 2]; 2]> {
        self.items.iter().rev().find(|(k, _)| k == key).map(|(_, r)| *r)
    }

    /// The preset library the menu lists.
    pub fn library(&self) -> &Library {
        &self.presets
    }

    /// The last message of the preset tools: (success, text).
    pub fn preset_message(&self) -> Option<&(bool, String)> {
        self.preset_form.message.as_ref()
    }

    /// Replaces the preset library (the app reloaded it).
    pub fn set_library(&mut self, library: Library) {
        self.presets = library;
    }

    /// The menu layout (position, size, opacity, collapsed) as the last frame left it.
    pub fn layout(&self) -> &settings::UiState {
        &self.layout
    }

    /// The menu layout when it changed since the last call (or since the configuration), for the app to keep
    /// in the shared settings and write (`Live::set_ui_layout`). `None` while the menu is open (the layout is
    /// handed over once the menu closes, not on every frame of a drag).
    pub fn take_layout_change(&mut self) -> Option<settings::UiState> {
        if self.menu.is_open() || self.layout == self.saved_layout {
            return None;
        }
        self.saved_layout = self.layout.clone();
        Some(self.layout.clone())
    }

    /// Closes the menu if it is open, giving the foreground back (at exit, before the layout is saved).
    pub fn close_menu(&mut self) {
        if self.menu.is_open() {
            self.close(CloseReason::Toggle);
        }
    }

    /// The hotkey registration problems the menu shows.
    pub fn hotkey_problems(&self) -> &[String] {
        &self.hotkey_problems
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
    }

    /// Sets the UI scale (sizes and fonts; 1 at 96 dpi). The live app follows the output's monitor DPI by
    /// itself (`GetDpiForWindow` at creation, `WM_DPICHANGED`); headless callers (tests, previews) set it here.
    pub fn set_ui_scale(&mut self, scale: f32) {
        self.set_dpi_scale(scale);
    }

    /// Makes every UI frame last `dt` seconds for ImGui (double-click and tooltip timing) instead of the measured
    /// time; `None` goes back to the clock. Headless tests use it, as their frames are slow and irregular.
    pub fn set_frame_time(&mut self, dt: Option<f32>) {
        self.fixed_dt = dt;
    }

    /// The UI scale in effect.
    pub fn ui_scale(&self) -> f32 {
        self.dpi
    }

    fn set_dpi_scale(&mut self, scale: f32) {
        if scale > 0.0 && scale.is_finite() && (scale - self.dpi).abs() > 1e-3 {
            // The theme writes every size from its tokens at the new scale (nothing accumulates).
            theme::apply(self.imgui.style_mut(), scale);
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

/// The hotkeys (T-54) reach the menu and the overlay through this: the menu key toggles the menu (and with it
/// the focus hand-off and the activation of the output, `crate::output::set_menu_open`), the overlay key the
/// overlay.
impl crate::controls::UiHook for Ui {
    fn toggle_menu(&mut self) {
        self.request_toggle();
    }

    fn toggle_overlay(&mut self) {
        Ui::toggle_overlay(self);
    }

    fn menu_open(&self) -> bool {
        self.is_open()
    }

    fn wants_input(&self) -> bool {
        Ui::wants_input(self)
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
            .fixed_dt
            .unwrap_or_else(|| self.last.map_or(1.0 / 60.0, |t| now.duration_since(t).as_secs_f32()))
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
        let mut picked = None;
        let mut overlay_rect = None;
        let frame = self.imgui.begin_frame();
        let ui = frame.ui();
        if self.overlay {
            let last = self.overlay_rect;
            let size = [last[1][0] - last[0][0], last[1][1] - last[0][1]];
            let rows = overlay::lines(&self.state);
            overlay_rect = Some(overlay::build(ui, &rows, &self.fonts, display, size));
        }
        if open && self.view == View::Picker {
            picked = Some(picker::build(ui, &mut self.picker, &self.fonts, &self.menu_key));
        } else if open {
            let focus = std::mem::take(&mut self.focus_menu);
            result = Some(menu::build(
                ui,
                MenuModel {
                    state: &self.state.clone(),
                    params: &mut self.state.params,
                    filter: &mut self.filter,
                    search: &mut self.search,
                    advanced: &mut self.advanced,
                    layout: &mut self.layout,
                    library: &self.presets,
                    presets: &mut self.preset_form,
                    system: &mut self.system,
                    key: &self.menu_key,
                    hotkey_problems: &self.hotkey_problems,
                    focus,
                    fonts: self.fonts,
                    prefs: &mut self.prefs,
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
            self.items = r.items;
            if let Some(lib) = r.library {
                self.presets = lib.clone();
                self.out.library = Some(lib);
            }
            if r.params_changed {
                self.out.params = Some(self.state.params);
            }
            if r.preset.is_some() {
                self.out.preset = r.preset.clone();
                self.state.preset = r.preset;
                // The new preset is the reference until the app hands over its own baseline.
                self.state.baseline = None;
            }
            if r.gpu_adapter.is_some() {
                self.out.gpu_adapter = r.gpu_adapter;
            }
            if r.wear_action.is_some() {
                self.out.wear_action = r.wear_action;
            }
            if r.prefs_changed {
                self.picker.ask_at_start = self.prefs.ask_at_start;
                self.out.source_prefs = Some(self.prefs);
            }
            if let Some(a) = r.source {
                self.out.source = Some(a);
                if a == SourceAction::ChangeSource {
                    // The picker replaces the settings at once; the app learns it from the action.
                    self.show_picker(!self.needs_source);
                }
            }
        }
        if let Some(r) = picked {
            self.items = r.items;
            if let Some(a) = r.action {
                self.picker_action(a);
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

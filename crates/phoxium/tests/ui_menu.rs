//! Headless GPU tests of the settings menu and the stats overlay (T-52a, T-52b): Dear ImGui drawn by the
//! pipeline after `encode` into an offscreen `B8G8R8A8_UNORM` target (the swapchain format), input fed as raw
//! Win32 messages through `Ui::handle_win32_message`. No window is created and no focus changes (a headless
//! `Ui` has no output window).
//!
//! Dear ImGui allows one context per process: every test holds [`IMGUI`] while its `Ui` lives. ImGui takes
//! one conflicting input event per frame, so the tests render one frame per event.

mod common;

use std::sync::{Mutex, MutexGuard};

use dear_imgui_rs::StyleColor;
use phoxium::gpu::{Headless, ImageF32, Settings, SourceFrame, StageReadback, TargetFormat};
use phoxium::params::Params;
use phoxium::presets::Library;
use phoxium::scaling::View;
use phoxium::ui::input::{
    key_lparam, mouse_lparam, WM_CHAR, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_MOUSEWHEEL,
};
use phoxium::ui::menu::MenuRects;
use phoxium::ui::{Ui, UiConfig, UiOutput, UiState};

static IMGUI: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    IMGUI.lock().unwrap_or_else(|e| e.into_inner())
}

/// A test background: a gradient, every pixel distinct enough to show any overdraw.
fn background(w: u32, h: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let (r, g, b) = ((x * 255 / w.max(1)) as u8, (y * 255 / h.max(1)) as u8, 96u8);
            v.extend_from_slice(&[b, g, r, 255]);
        }
    }
    v
}

/// Headless renderer, a `Ui` on it, the background frame and what the UI shows. The `Ui` is declared first,
/// so it drops before the renderer (its ImGui renderer lives on the renderer's device).
struct Rig {
    ui: Ui,
    gpu: Headless,
    w: u32,
    h: u32,
    bg: Vec<u8>,
    state: UiState,
    /// Everything the UI returned so far, merged (the last value of each field wins).
    out: UiOutput,
    /// Render with the parameters the menu edits (else the identity gun, a passthrough under the UI).
    live_params: bool,
    /// The background is on the GPU: later frames reuse the upload (nothing under test changes it).
    uploaded: bool,
}

impl Rig {
    fn new(test: &str, w: u32, h: u32) -> Option<Rig> {
        Self::with_config(test, w, h, UiConfig::headless())
    }

    fn with_config(test: &str, w: u32, h: u32, config: UiConfig) -> Option<Rig> {
        let mut gpu = common::gpu_or_skip(test)?;
        // The UI draws after `encode`, over whatever the chain made: these tests are about the UI, so every
        // chain slot is off (bypassed: an exact identity, or a plain resample where the domain changes) instead
        // of running the beam and mask on every frame. `full_chain` turns them back on.
        gpu.enable_only(&[]).expect("chain slots off");
        // Only the output is looked at: the chain images (four times the output's texels at K = 4) are not
        // read back.
        gpu.set_read_stages(StageReadback::None);
        let mut ui =
            Ui::new(gpu.context(), Headless::vk_format(TargetFormat::Bgra8Unorm), config).expect("create the UI");
        // Headless frames are slow and irregular: a fixed 60 Hz frame time keeps ImGui's timing (double-click) stable.
        ui.set_frame_time(Some(1.0 / 60.0));
        Some(Rig {
            ui,
            gpu,
            w,
            h,
            bg: background(w, h),
            state: UiState {
                params: Params::identity_gun(),
                ..UiState::default()
            },
            out: UiOutput::default(),
            live_params: false,
            uploaded: false,
        })
    }

    /// Every chain slot on again (the whole physical chain under the UI).
    fn full_chain(&mut self) {
        for n in self.gpu.stage_names() {
            self.gpu.set_stage_enabled(n, true).expect("chain slot on");
        }
    }

    /// Renders one frame of the background (uploaded on the first render only, then reused by the pipeline)
    /// with `params`, the UI drawn over it when `ui`.
    fn render(&mut self, params: &Params, ui: bool) -> ImageF32 {
        let upload = !std::mem::replace(&mut self.uploaded, true);
        let src = upload.then_some(SourceFrame {
            width: self.w,
            height: self.h,
            bgra: &self.bg,
        });
        let (view, size) = (View::full(self.w, self.h), (self.w, self.h));
        let r = if ui {
            self.gpu.render_with_ui(
                src,
                view,
                &Settings::identity(),
                params,
                size,
                TargetFormat::Bgra8Unorm,
                &mut self.ui,
            )
        } else {
            self.gpu
                .render(src, view, &Settings::identity(), params, size, TargetFormat::Bgra8Unorm)
        };
        r.expect("render").output
    }

    /// Feeds `msgs` to the UI, then renders one frame with the UI over the passthrough of the background.
    fn frame(&mut self, msgs: &[(u32, usize, isize)]) -> ImageF32 {
        for &(m, w, l) in msgs {
            self.ui.handle_win32_message(m, w, l);
        }
        let out = self.ui.frame(&self.state);
        if let Some(p) = out.params {
            self.state.params = p;
            self.out.params = Some(p);
        }
        if out.preset.is_some() {
            self.state.preset = out.preset.clone();
            self.out.preset = out.preset;
        }
        if out.library.is_some() {
            self.out.library = out.library;
        }
        if out.gpu_adapter.is_some() {
            self.out.gpu_adapter = out.gpu_adapter;
        }
        if out.capture_path.is_some() {
            self.out.capture_path = out.capture_path;
        }
        let params = if self.live_params {
            self.state.params
        } else {
            Params::identity_gun()
        };
        self.render(&params, true)
    }

    /// The output without any UI (plain `render`).
    fn plain(&mut self) -> ImageF32 {
        self.render(&Params::identity_gun(), false)
    }

    /// Clicks at `p`: move, press, release, one frame each.
    fn click(&mut self, p: [f32; 2]) {
        let l = mouse_lparam(p[0] as i16, p[1] as i16);
        self.frame(&[(WM_MOUSEMOVE, 0, l)]);
        self.frame(&[(WM_LBUTTONDOWN, 1, l)]);
        self.frame(&[(WM_LBUTTONUP, 0, l)]);
    }

    /// Taps a key (virtual key `vk`, scan code `scan`).
    fn key(&mut self, vk: usize, scan: u32, extended: bool) {
        self.frame(&[(WM_KEYDOWN, vk, key_lparam(scan, extended, false))]);
        self.frame(&[(WM_KEYUP, vk, key_lparam(scan, extended, true))]);
    }

    /// The output for `params` without any UI (plain `render`).
    fn plain_with(&mut self, params: Params) -> ImageF32 {
        self.render(&params, false)
    }

    /// The rectangle of the menu item `key` in the last frame (`widget:<id>`, `header:<panel>`, ...).
    fn item(&self, key: &str) -> [[f32; 2]; 2] {
        let r = self
            .ui
            .item_rect(key)
            .unwrap_or_else(|| panic!("no menu item {key} in the last frame"));
        // Above the pinned footer (its first row is the opacity slider): else a click lands on the footer.
        let footer = self.ui.menu_rects().opacity[0][1];
        assert!(
            r[1][1] < footer - 8.0,
            "menu item {key} {r:?} is scrolled under the footer ({footer})"
        );
        r
    }

    /// Clicks the centre of the menu item `key`, then lets one frame settle.
    fn click_item(&mut self, key: &str) {
        self.click(MenuRects::at(self.item(key), 0.5, 0.5));
        self.frame(&[]);
    }

    /// Drags on a slider from fraction `from` to fraction `to` of its width.
    fn drag(&mut self, r: [[f32; 2]; 2], from: f32, to: f32) {
        let a = MenuRects::at(r, from, 0.5);
        let b = MenuRects::at(r, to, 0.5);
        let (la, lb) = (
            mouse_lparam(a[0] as i16, a[1] as i16),
            mouse_lparam(b[0] as i16, b[1] as i16),
        );
        self.frame(&[(WM_MOUSEMOVE, 0, la)]);
        self.frame(&[(WM_LBUTTONDOWN, 1, la)]);
        self.frame(&[(WM_MOUSEMOVE, 1, lb)]);
        self.frame(&[(WM_LBUTTONUP, 0, lb)]);
        self.frame(&[]);
    }

    /// Double-clicks at `p` (the second press as `WM_LBUTTONDBLCLK`, as Windows sends it).
    fn double_click(&mut self, p: [f32; 2]) {
        let l = mouse_lparam(p[0] as i16, p[1] as i16);
        self.frame(&[(WM_MOUSEMOVE, 0, l)]);
        self.frame(&[(WM_LBUTTONDOWN, 1, l)]);
        self.frame(&[(WM_LBUTTONUP, 0, l)]);
        self.frame(&[(WM_LBUTTONDBLCLK, 1, l)]);
        self.frame(&[(WM_LBUTTONUP, 0, l)]);
        self.frame(&[]);
    }

    /// Turns the wheel `notches` times towards the user (scrolls the menu body down), over the menu's middle.
    fn scroll_down(&mut self, notches: u32) {
        let p = MenuRects::at(self.ui.menu_rects().window, 0.5, 0.5);
        let l = mouse_lparam(p[0] as i16, p[1] as i16);
        self.frame(&[(WM_MOUSEMOVE, 0, l)]);
        let down = (((-120_i16) as u16) as usize) << 16;
        for _ in 0..notches {
            self.frame(&[(WM_MOUSEWHEEL, down, l)]);
        }
        self.frame(&[]);
    }

    /// Scrolls the menu body so that the item `key` comes near its top (Dear ImGui scrolls 5 lines of the 15 px
    /// body font per notch at 100 %).
    fn scroll_to(&mut self, key: &str) {
        let top = self.item("search")[0][1];
        let y = self.ui.item_rect(key).expect("the item to scroll to")[0][1];
        let notches = ((y - top) / 75.0).floor().max(0.0) as u32;
        self.scroll_down(notches);
    }

    /// Types `text` into the focused text field, one character per frame.
    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.frame(&[(WM_CHAR, c as usize, 1)]);
        }
        self.frame(&[]);
    }

    /// Selects the preset `id` with the preset filter (typed, then erased) and the combo (Down, Enter).
    fn select_preset(&mut self, id: &str) {
        let filter = self.ui.menu_rects().filter;
        self.click(MenuRects::at(filter, 0.5, 0.5));
        self.type_text(id);
        self.click(MenuRects::at(self.ui.menu_rects().preset, 0.5, 0.5));
        self.key(0x28, 0x50, true); // Down: the only entry
        self.key(0x0D, 0x1C, false); // Enter
        self.frame(&[]);
        self.click(MenuRects::at(filter, 0.5, 0.5));
        for _ in 0..id.chars().count() {
            self.key(0x08, 0x0E, false); // Backspace
        }
        self.frame(&[]);
    }

    /// Opens the menu and lets it settle.
    fn open_menu(&mut self) {
        self.ui.request_toggle();
        self.frame(&[]);
        self.frame(&[]);
    }

    fn finish(self) {
        let counter = self.gpu.validation_counter();
        drop(self.ui);
        drop(self.gpu);
        common::assert_no_validation_messages(&counter);
    }
}

fn code(v: f32) -> i32 {
    (v * 255.0).round() as i32
}

fn differs(a: [f32; 4], b: [f32; 4]) -> bool {
    a.iter().zip(b).any(|(&x, y)| code(x) != code(y))
}

/// Pixels of `a` and `b` that differ, inside and outside the rectangle `r` (grown by `margin` px).
fn diff_counts(a: &ImageF32, b: &ImageF32, r: [[f32; 2]; 2], margin: f32) -> (usize, usize) {
    let (mut inside, mut outside) = (0, 0);
    for y in 0..a.height {
        for x in 0..a.width {
            if !differs(a.pixel(x, y), b.pixel(x, y)) {
                continue;
            }
            let (fx, fy) = (x as f32, y as f32);
            if fx >= r[0][0] - margin && fx < r[1][0] + margin && fy >= r[0][1] - margin && fy < r[1][1] + margin {
                inside += 1;
            } else {
                outside += 1;
            }
        }
    }
    (inside, outside)
}

#[test]
fn hidden_ui_records_nothing_and_the_output_is_bit_exact() {
    let _g = lock();
    let Some(mut rig) = Rig::new("hidden_ui_records_nothing_and_the_output_is_bit_exact", 320, 240) else {
        return;
    };
    let plain = rig.plain();
    let with_hidden_ui = rig.frame(&[]);
    assert!(plain == with_hidden_ui, "a hidden UI changed the output");
    let t = rig.gpu.last_timings().cloned();
    if let Some(t) = t {
        assert_eq!(t.stage_ms("ui"), None, "a hidden UI recorded a pass");
        assert_eq!(t.stages.last().map(|s| s.name), Some("ui"));
    }
    // Shown (ImGui hides an auto-sized window in its first frame), then hidden: bit exact again.
    rig.ui.toggle_overlay();
    rig.frame(&[]);
    let shown = rig.frame(&[]);
    assert!(plain != shown, "the overlay drew nothing");
    rig.ui.toggle_overlay();
    assert!(plain == rig.frame(&[]), "hidden again, the output is not the plain one");
    rig.finish();
}

#[test]
fn menu_draws_inside_its_window_only() {
    let _g = lock();
    let Some(mut rig) = Rig::new("menu_draws_inside_its_window_only", 800, 640) else {
        return;
    };
    let plain = rig.plain();
    rig.ui.request_toggle();
    assert!(rig.ui.is_open() && rig.ui.wants_input());
    rig.frame(&[]);
    let img = rig.frame(&[]);
    let r = rig.ui.menu_rects().window;
    assert!(
        r[1][0] > r[0][0] + 100.0 && r[1][1] > r[0][1] + 100.0,
        "menu window {r:?}"
    );
    let (inside, outside) = diff_counts(&img, &plain, r, 1.0);
    let area = ((r[1][0] - r[0][0]) * (r[1][1] - r[0][1])) as usize;
    assert!(
        inside > area / 2,
        "menu pixels: {inside} of {area} differ from the background"
    );
    assert_eq!(outside, 0, "the menu drew outside its window");
    // Closed: the output is the plain one again.
    rig.ui.request_toggle();
    assert!(!rig.ui.is_open() && !rig.ui.wants_input());
    assert!(rig.frame(&[]) == plain, "closed, the output is not the plain one");
    rig.finish();
}

#[test]
fn slider_combo_and_text_input_through_win32_messages() {
    let _g = lock();
    let Some(mut rig) = Rig::new("slider_combo_and_text_input_through_win32_messages", 800, 720) else {
        return;
    };
    rig.ui.request_toggle();
    rig.frame(&[]);
    rig.frame(&[]);
    let r: MenuRects = rig.ui.menu_rects();

    // Exposure slider: press near its left end, drag to its right end, release.
    let before = rig.state.params.exposure;
    let lo = MenuRects::at(r.exposure, 0.02, 0.5);
    let hi = MenuRects::at(r.exposure, 0.60, 0.5);
    let lo_l = mouse_lparam(lo[0] as i16, lo[1] as i16);
    let hi_l = mouse_lparam(hi[0] as i16, hi[1] as i16);
    rig.frame(&[(WM_MOUSEMOVE, 0, lo_l)]);
    rig.frame(&[(WM_LBUTTONDOWN, 1, lo_l)]);
    rig.frame(&[]); // the change of a frame comes back with the next one
    let low = rig.state.params.exposure;
    rig.frame(&[(WM_MOUSEMOVE, 1, hi_l)]);
    rig.frame(&[(WM_LBUTTONUP, 0, hi_l)]);
    rig.frame(&[]);
    let high = rig.state.params.exposure;
    let meta = phoxium::params::meta("exposure").unwrap();
    let (min, max) = meta.soft_range.unwrap();
    eprintln!("exposure: {before} -> {low} (press) -> {high} (drag), soft range {min}..{max}");
    // The exposure slider is logarithmic (its range spans two decades): compare track positions.
    let log = phoxium::ui::panels::is_log(meta);
    let at = |v: f32| phoxium::ui::theme::slider_fraction(f64::from(v), min, max, log);
    assert!(
        at(low) < 0.1,
        "the press did not move the slider to its left end: {low}"
    );
    assert!(at(high) > at(low) + 0.3, "the drag did not move the slider");
    assert_eq!(
        rig.out.params.map(|p| p.exposure),
        Some(high),
        "the change was not returned"
    );

    // Headroom mode combo: click, Down, Enter selects another mode (keyboard navigation wraps from the last
    // entry, `auto`, the default, to the first).
    let mode = rig.state.params.output_headroom_mode;
    let modes = phoxium::params::meta("output_headroom_mode").unwrap().values.len() as u32;
    rig.click(MenuRects::at(rig.ui.menu_rects().headroom_mode, 0.3, 0.5));
    rig.key(0x28, 0x50, true); // Down
    rig.key(0x0D, 0x1C, false); // Enter
    rig.frame(&[]);
    let new = rig.state.params.output_headroom_mode;
    eprintln!("headroom mode: {mode} -> {new} (of {modes})");
    assert_ne!(new, mode, "the combo did not change the mode");
    assert_eq!(new, (mode + 1) % modes, "Down did not select the next mode");
    assert_eq!(rig.out.params.map(|p| p.output_headroom_mode), Some(new));

    // Preset filter: click into the text field, type.
    rig.click(MenuRects::at(rig.ui.menu_rects().filter, 0.3, 0.5));
    for c in "Zq!".chars() {
        rig.frame(&[(WM_CHAR, c as usize, 1)]);
    }
    rig.key(0x08, 0x0E, false); // Backspace
    rig.frame(&[]);
    assert_eq!(rig.ui.filter(), "Zq");
    rig.finish();
}

/// R-05 (owner decision Q1): a value typed into a slider (Ctrl+click, type, Enter) beyond its range is clamped
/// to the slider range, on the logarithmic exposure slider (0.05 to 8): 500 gives 8, 0 and -3 give 0.05.
#[test]
fn a_typed_value_beyond_the_slider_range_is_clamped() {
    let _g = lock();
    let Some(mut rig) = Rig::new("a_typed_value_beyond_the_slider_range_is_clamped", 800, 720) else {
        return;
    };
    rig.open_menu();
    let (lo, hi) = phoxium::params::meta("exposure").unwrap().soft_range.unwrap();
    for (text, want) in [("500", hi as f32), ("0", lo as f32), ("-3", lo as f32), ("2.5", 2.5)] {
        let p = MenuRects::at(rig.ui.menu_rects().exposure, 0.5, 0.5);
        let l = mouse_lparam(p[0] as i16, p[1] as i16);
        // Ctrl+click opens the text entry of the slider (its text selected), then type and Enter.
        rig.frame(&[(WM_KEYDOWN, 0x11, key_lparam(0x1D, false, false))]);
        rig.frame(&[(WM_MOUSEMOVE, 0, l)]);
        rig.frame(&[(WM_LBUTTONDOWN, 1, l)]);
        rig.frame(&[(WM_LBUTTONUP, 0, l)]);
        rig.frame(&[(WM_KEYUP, 0x11, key_lparam(0x1D, false, true))]);
        rig.type_text(text);
        rig.key(0x0D, 0x1C, false); // Enter
        rig.frame(&[]);
        let got = rig.state.params.exposure;
        eprintln!("typed {text}: exposure {got}");
        assert_eq!(got, want, "typed {text}");
    }
    rig.finish();
}

/// The glow weight slider of the Optics panel (owner request 2026-11-20) takes a typed value: beyond its slider
/// range (0 to 0.3) it is clamped to it, inside it is kept; the panel shows the master switch first.
#[test]
fn a_typed_glow_weight_is_clamped_to_its_slider() {
    let _g = lock();
    let Some(mut rig) = Rig::with_config(
        "a_typed_glow_weight_is_clamped_to_its_slider",
        1280,
        1080,
        tall_config(),
    ) else {
        return;
    };
    rig.state.params = Params::default();
    rig.open_menu();
    rig.click_item("header:Output and headroom");
    rig.click_item("header:Optics");
    let enable = rig.item("widget:optics_enable");
    let glow = rig.item("widget:optics_bloom_weight");
    assert!(enable[0][1] < glow[0][1], "the master switch is the first row");
    let (_, hi) = phoxium::params::meta("optics_bloom_weight")
        .unwrap()
        .soft_range
        .unwrap();
    for (text, want) in [("5", hi as f32), ("0.12", 0.12), ("-1", 0.0)] {
        let p = MenuRects::at(rig.item("widget:optics_bloom_weight"), 0.5, 0.5);
        let l = mouse_lparam(p[0] as i16, p[1] as i16);
        rig.frame(&[(WM_KEYDOWN, 0x11, key_lparam(0x1D, false, false))]);
        rig.frame(&[(WM_MOUSEMOVE, 0, l)]);
        rig.frame(&[(WM_LBUTTONDOWN, 1, l)]);
        rig.frame(&[(WM_LBUTTONUP, 0, l)]);
        rig.frame(&[(WM_KEYUP, 0x11, key_lparam(0x1D, false, true))]);
        rig.type_text(text);
        rig.key(0x0D, 0x1C, false); // Enter
        rig.frame(&[]);
        let got = rig.state.params.optics_bloom_weight;
        eprintln!("typed {text}: glow weight {got}");
        assert_eq!(got, want, "typed {text}");
    }
    rig.finish();
}

#[test]
fn preset_selector_applies_the_preset() {
    let _g = lock();
    let Some(mut rig) = Rig::new("preset_selector_applies_the_preset", 800, 720) else {
        return;
    };
    let lib = Library::embedded();
    assert!(lib.list().len() >= 2, "need two presets");
    rig.ui.request_toggle();
    rig.frame(&[]);
    rig.frame(&[]);
    // Open the combo (no current preset: the popup lists every preset), Down twice, Enter: one is chosen.
    rig.click(MenuRects::at(rig.ui.menu_rects().preset, 0.3, 0.5));
    rig.key(0x28, 0x50, true);
    rig.key(0x28, 0x50, true);
    rig.key(0x0D, 0x1C, false);
    rig.frame(&[]);
    let id = rig.out.preset.clone().expect("no preset was selected");
    let preset = lib.find(&id).expect("selected preset exists");
    assert_eq!(
        rig.out.params,
        Some(preset.params()),
        "the preset's parameters were not applied"
    );
    assert_eq!(rig.state.params, preset.params());
    eprintln!("selected preset {id}");
    rig.finish();
}

#[test]
fn overlay_blends_over_the_output_and_takes_no_input() {
    let _g = lock();
    let Some(mut rig) = Rig::new("overlay_blends_over_the_output_and_takes_no_input", 640, 360) else {
        return;
    };
    {
        let style = rig.ui.imgui().style_mut();
        style.set_window_rounding(0.0);
        style.set_window_border_size(0.0);
        style.set_color(StyleColor::WindowBg, [0.0, 0.0, 1.0, 1.0]);
    }
    let plain = rig.plain();
    rig.ui.toggle_overlay();
    assert!(rig.ui.overlay_on() && !rig.ui.is_open() && !rig.ui.wants_input());
    // The overlay takes no input: a click is not consumed.
    assert!(!rig.ui.handle_win32_message(WM_LBUTTONDOWN, 1, mouse_lparam(600, 20)));
    // ImGui sizes an auto-resized window in its first frames; the overlay is placed from the size of the
    // previous frame, so it settles in the third.
    rig.frame(&[]);
    rig.frame(&[]);
    let img = rig.frame(&[]);
    let r = rig.ui.overlay_rect();
    assert!(r[1][0] > r[0][0] + 50.0, "overlay window {r:?}");
    // Top right, MARGIN px from the corner.
    assert!(
        (r[1][0] - (640.0 - phoxium::ui::overlay::MARGIN)).abs() <= 1.0,
        "overlay window {r:?}"
    );
    let (_, outside) = diff_counts(&img, &plain, r, 1.0);
    assert_eq!(outside, 0, "the overlay drew outside its window");
    // A padding pixel (no text): window colour (0, 0, 1) at alpha BG_ALPHA over the background.
    let a = phoxium::ui::overlay::BG_ALPHA;
    let a_code = (a * 255.0).round() / 255.0; // ImGui packs the colour to 8 bits
    let mut checked = 0;
    for (dx, dy) in [(2.0, 2.0), (3.0, 3.0), (2.0, 4.0)] {
        let (x, y) = ((r[0][0] + dx) as u32, (r[0][1] + dy) as u32);
        let (dst, out) = (plain.pixel(x, y), img.pixel(x, y));
        let want = [
            dst[0] * (1.0 - a_code),
            dst[1] * (1.0 - a_code),
            a_code + dst[2] * (1.0 - a_code),
        ];
        for c in 0..3 {
            assert!(
                (code(out[c]) - code(want[c])).abs() <= 1,
                "pixel ({x}, {y}) channel {c}: {} vs {} (dst {dst:?})",
                code(out[c]),
                code(want[c])
            );
        }
        checked += 1;
    }
    assert_eq!(checked, 3);
    rig.finish();
}

/// Median of the `ui` timing segment over `n` frames.
fn median_ui_ms(rig: &mut Rig, n: usize) -> Option<f64> {
    let mut ms = Vec::new();
    for _ in 0..n {
        rig.frame(&[]);
        if let Some(v) = rig.gpu.last_timings().and_then(|t| t.stage_ms("ui")) {
            ms.push(v);
        }
    }
    ms.sort_by(f64::total_cmp);
    ms.get(ms.len() / 2).copied()
}

#[test]
fn ui_cost_at_1920x1080() {
    let _g = lock();
    let Some(mut rig) = Rig::new("ui_cost_at_1920x1080", 1920, 1080) else {
        return;
    };
    if let Some(why) = rig.gpu.timing_unavailable() {
        eprintln!("ui_cost_at_1920x1080: GPU timings unavailable ({why}), cost not measured");
        rig.finish();
        return;
    }
    rig.ui.toggle_overlay();
    let overlay = median_ui_ms(&mut rig, 8).expect("overlay timed");
    rig.ui.request_toggle();
    let both = median_ui_ms(&mut rig, 8).expect("menu timed");
    // T-53: the heaviest menu, every parameter row drawn (a search that matches nearly all of them).
    rig.click_item("search");
    rig.type_text("_");
    let rows = phoxium::params::PARAMS_META
        .iter()
        .filter(|m| rig.ui.item_rect(&format!("label:{}", m.id)).is_some())
        .count();
    let full = median_ui_ms(&mut rig, 8).expect("full menu timed");
    let encode = rig.gpu.last_timings().and_then(|t| t.stage_ms("encode"));
    eprintln!(
        "UI GPU cost at 1920x1080 (median of 8 frames): overlay {overlay:.4} ms, menu + overlay {both:.4} ms, \
         menu with {rows} parameter rows + overlay {full:.4} ms (encode {encode:.4?} ms)"
    );
    // Generous bounds (headless, idle clocks): the plan's overlay budget is 0.2 ms (T-52b).
    assert!(overlay < 1.0, "overlay {overlay} ms");
    assert!(both < 2.0, "menu {both} ms");
    assert!(rows > 50, "the search drew {rows} rows");
    assert!(full < 3.0, "full menu {full} ms");
    rig.finish();
}

#[test]
fn ui_can_be_created_and_destroyed_repeatedly() {
    let _g = lock();
    for round in 0..3 {
        let Some(mut rig) = Rig::new("ui_can_be_created_and_destroyed_repeatedly", 320, 240) else {
            return;
        };
        rig.ui.request_toggle();
        rig.ui.toggle_overlay();
        rig.frame(&[]);
        rig.frame(&[]);
        // Drop the first `Ui` (renderer shutdown, then the ImGui context), then a second one on the same device.
        let Rig { ui, gpu, .. } = rig;
        drop(ui);
        let ui2 = Ui::new(
            gpu.context(),
            Headless::vk_format(TargetFormat::Bgra8Unorm),
            UiConfig::headless(),
        )
        .expect("second UI on the same device");
        let mut rig = Rig {
            ui: ui2,
            gpu,
            w: 320,
            h: 240,
            bg: background(320, 240),
            state: UiState::default(),
            out: UiOutput::default(),
            live_params: false,
            uploaded: false,
        };
        rig.ui.toggle_overlay();
        rig.frame(&[]);
        eprintln!("round {round}: created, drawn and destroyed twice");
        rig.finish();
    }
}

// ---------------------------------------------------------------------------------------------------------
// Preview images of the menu's look (owner request 2026-11-19), for a human to look at.

/// A picture with some structure under the menu: colour bars over a dark-to-light ramp (BGRA).
fn preview_background(w: u32, h: u32) -> Vec<u8> {
    const BARS: [[u8; 3]; 8] = [
        [192, 192, 192],
        [192, 192, 0],
        [0, 192, 192],
        [0, 192, 0],
        [192, 0, 192],
        [192, 0, 0],
        [0, 0, 192],
        [16, 16, 16],
    ];
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = if y < h * 2 / 3 {
                BARS[(x * 8 / w.max(1)) as usize]
            } else {
                let g = (x * 255 / w.max(1)) as u8;
                [g, g, g]
            };
            v.extend_from_slice(&[b, g, r, 255]);
        }
    }
    v
}

/// What the preview menu shows: a headroom readout above 1, a placement, per-stage timings.
fn preview_state() -> UiState {
    use phoxium::gpu::{FrameTimings, StageTiming};
    let params = Params::default();
    let stage = |name, ms: Option<f64>, bypassed| StageTiming { name, ms, bypassed };
    UiState {
        params,
        preset: Library::embedded().list().first().map(|p| p.id.clone()),
        baseline: None,
        headroom: Some(crt_model::output::Headroom {
            mask_peak: 1.842,
            mask_peak_physical: 1.842,
            scanline_peak: 1.318,
            matrix_max: 1.036,
            exposure: 1.0,
        }),
        placement: Some(phoxium::placement::readout(&params, (320, 240), (1280, 720), true)),
        timings: Some(FrameTimings {
            frames: 60,
            frame_ms: 4.94,
            stages: vec![
                stage("upload", Some(0.031), false),
                stage("decode", Some(0.042), false),
                stage("amp", Some(0.462), false),
                stage("gun", Some(0.101), false),
                stage("abl", Some(0.611), false),
                stage("screen", Some(2.418), false),
                stage("phosphor", Some(0.982), false),
                stage("optics", Some(0.012), true),
                stage("encode", Some(0.701), false),
                stage("ui", None, false),
            ],
        }),
        capture_fps: 59.94,
        render_fps: 60.0,
        quality: None,
        source: Default::default(),
        profiles: Vec::new(),
        default_profile: None,
        capture: Default::default(),
        click_through: false,
        frame_cap: 0,
        tearing: false,
        keep_awake: false,
        tearing_active: false,
        max_capture_rate: false,
        snapshot_message: None,
        notices: Vec::new(),
    }
}

/// Writes `img` (RGBA, display codes) to `target/ui-preview/<name>.png` in the workspace.
fn write_png(img: &ImageF32, name: &str) -> std::path::PathBuf {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-preview");
    std::fs::create_dir_all(&dir).expect("create target/ui-preview");
    let path = dir.join(format!("{name}.png"));
    let file = std::io::BufWriter::new(std::fs::File::create(&path).expect("create the PNG"));
    let mut enc = png::Encoder::new(file, img.width, img.height);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().expect("PNG header");
    let data: Vec<u8> = img
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[0], p[1], p[2]].map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8))
        .collect();
    w.write_image_data(&data).expect("PNG data");
    path.canonicalize().unwrap_or(path)
}

/// One preview: the menu (and the overlay) at `w` x `h` and UI scale `scale`; `expand` opens the sections
/// closed by default; `combo` opens the preset list.
fn preview(name: &str, w: u32, h: u32, scale: f32, expand: bool, combo: bool) {
    let config = UiConfig {
        hotkey_problems: vec!["F10 is registered by another program: the menu uses Ctrl+Alt+F10 instead.".to_string()],
        menu_key: "Ctrl+Alt+F10".to_string(),
        layout: phoxium::settings::UiState {
            menu_h: if expand { 1240.0 / scale.max(1.0) } else { 560.0 },
            ..phoxium::settings::UiState::default()
        },
        ..UiConfig::headless()
    };
    let Some(mut rig) = Rig::with_config(name, w, h, config) else {
        return;
    };
    // The previews show the menu over the whole chain, as the app does.
    rig.full_chain();
    rig.bg = preview_background(w, h);
    rig.state = preview_state();
    rig.ui.set_ui_scale(scale);
    rig.ui.request_toggle();
    rig.ui.toggle_overlay();
    for _ in 0..3 {
        rig.frame(&[]);
    }
    if expand {
        rig.click(MenuRects::at(rig.ui.menu_rects().source_header, 0.5, 0.5));
        rig.frame(&[]);
        rig.click(MenuRects::at(rig.ui.menu_rects().performance_header, 0.5, 0.5));
    }
    if combo {
        rig.click(MenuRects::at(rig.ui.menu_rects().preset, 0.5, 0.5));
    }
    // Hover nothing; settle the auto-sized overlay.
    rig.frame(&[(WM_MOUSEMOVE, 0, mouse_lparam(i16::MAX, i16::MAX))]);
    rig.frame(&[]);
    let img = rig.frame(&[]);
    let r = rig.ui.menu_rects();
    assert!(
        r.window[1][0] <= w as f32 + 0.5 && r.window[1][1] <= h as f32 + 0.5,
        "{name}: the menu {:?} leaves the {w}x{h} output",
        r.window
    );
    let path = write_png(&img, name);
    eprintln!("{name}: menu {:?}, preview {}", r.window, path.display());
    rig.finish();
}

#[test]
fn menu_preview_images() {
    let _g = lock();
    preview("menu-1280x720-100", 1280, 720, 1.0, false, false);
    preview("menu-1280x720-100-presets", 1280, 720, 1.0, false, true);
    preview("menu-1280x720-150", 1280, 720, 1.5, false, false);
    preview("menu-2560x1440-200", 2560, 1440, 2.0, false, false);
    preview("menu-2560x1440-100-expanded", 2560, 1440, 1.0, true, false);
}

// ---------------------------------------------------------------------------------------------------------
// T-53: the generated parameter panels, the preset tools and the GPU and system panel.

fn temp_dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("phoxium-ui-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("create the temp folder");
    d
}

/// A tall menu (every panel fits without scrolling) on a 1280 x 1080 output.
fn tall_config() -> UiConfig {
    UiConfig {
        layout: phoxium::settings::UiState {
            menu_h: 1060.0,
            menu_w: 520.0,
            menu_y: 4.0,
            ..phoxium::settings::UiState::default()
        },
        ..UiConfig::headless()
    }
}

#[test]
fn a_slider_changes_its_parameter_and_the_rendered_frame() {
    let _g = lock();
    let Some(mut rig) = Rig::new("a_slider_changes_its_parameter_and_the_rendered_frame", 800, 720) else {
        return;
    };
    // The rendered frame must follow the menu through the whole physical chain.
    rig.full_chain();
    rig.live_params = true;
    rig.state.params.exposure = 0.5;
    rig.open_menu();
    let before_img = rig.plain_with(rig.state.params);
    rig.drag(rig.item("widget:exposure"), 0.02, 0.9);
    let p = rig.state.params;
    eprintln!("exposure 0.5 -> {}", p.exposure);
    assert!(p.exposure > 2.0, "the log slider did not move: {}", p.exposure);
    assert_eq!(
        rig.out.params.map(|q| q.exposure),
        Some(p.exposure),
        "the change was not returned"
    );
    let after_img = rig.plain_with(p);
    let changed = (0..after_img.height)
        .flat_map(|y| (0..after_img.width).map(move |x| (x, y)))
        .filter(|&(x, y)| differs(after_img.pixel(x, y), before_img.pixel(x, y)))
        .count();
    assert!(
        changed > (800 * 720) / 4,
        "the rendered frame did not follow: {changed} pixels changed"
    );
    // A modified parameter is marked: the output panel header counts it.
    rig.finish();
}

#[test]
fn double_click_and_panel_reset_restore_the_reference() {
    let _g = lock();
    let Some(mut rig) = Rig::with_config(
        "double_click_and_panel_reset_restore_the_reference",
        1280,
        1080,
        tall_config(),
    ) else {
        return;
    };
    rig.state.params = Params::default();
    rig.open_menu();
    let default = Params::default();
    rig.drag(rig.item("widget:exposure"), 0.5, 0.95);
    assert_ne!(rig.state.params.exposure, default.exposure);
    // Double-click on the label: back to the reference (no preset: the table default).
    rig.double_click(MenuRects::at(rig.item("label:exposure"), 0.3, 0.5));
    assert_eq!(
        rig.state.params.exposure, default.exposure,
        "double-click did not reset"
    );
    // Two changes, then the panel's reset button.
    rig.drag(rig.item("widget:exposure"), 0.5, 0.95);
    rig.drag(rig.item("widget:output_knee"), 0.5, 0.95);
    assert!(rig.state.params.exposure != default.exposure && rig.state.params.output_knee != default.output_knee);
    rig.click_item("reset:Output and headroom");
    assert_eq!(rig.state.params, default, "the panel reset left changes");
    assert!(
        rig.ui.item_rect("reset:Output and headroom").is_none(),
        "nothing left to reset"
    );
    rig.finish();
}

#[test]
fn search_and_the_advanced_toggle_choose_the_rows() {
    let _g = lock();
    let Some(mut rig) = Rig::with_config(
        "search_and_the_advanced_toggle_choose_the_rows",
        1280,
        1080,
        tall_config(),
    ) else {
        return;
    };
    rig.state.params = Params::default();
    rig.open_menu();
    // Mask panel, advanced hidden: the mask tier (advanced) has no row; the pitch has one.
    rig.click_item("header:Mask");
    assert!(rig.ui.item_rect("widget:mask_pitch_mm").is_some());
    assert!(rig.ui.item_rect("widget:mask_tier").is_none(), "an advanced row shows");
    rig.click_item("advanced");
    assert!(
        rig.ui.item_rect("widget:mask_tier").is_some(),
        "Advanced did not show it"
    );
    rig.click_item("advanced");
    // A search lists the matching rows of every panel, advanced ones included, and nothing else.
    rig.click_item("search");
    rig.type_text("tier");
    assert_eq!(rig.ui.search(), "tier");
    assert!(rig.ui.item_rect("widget:mask_tier").is_some());
    assert!(
        rig.ui.item_rect("widget:exposure").is_none(),
        "a non-matching row shows"
    );
    assert!(rig.ui.item_rect("header:Mask").is_none(), "panels show during a search");
    // A slider found by the search edits its parameter.
    let before = rig.state.params.mask_tier_blend;
    rig.drag(rig.item("widget:mask_tier_blend"), 0.05, 0.95);
    assert!(rig.state.params.mask_tier_blend > before + 0.3);
    // No match: no row at all.
    rig.click_item("search");
    rig.type_text("zzq");
    assert!(rig.ui.item_rect("widget:mask_tier").is_none());
    rig.finish();
}

/// Owner feedback 2026-11-20 (QUESTIONS PHY-17): the persistence decay scale is reachable from the menu and a
/// typed value is clamped to its slider range (1 to 30): 500 gives 30, 0.5 gives 1, 20 stays 20.
#[test]
fn the_decay_scale_takes_a_typed_value() {
    let _g = lock();
    let Some(mut rig) = Rig::with_config("the_decay_scale_takes_a_typed_value", 1280, 1080, tall_config()) else {
        return;
    };
    rig.state.params = Params::default();
    rig.open_menu();
    rig.click_item("search");
    rig.type_text("decay scale");
    let (lo, hi) = phoxium::params::meta("phosphor_decay_scale")
        .unwrap()
        .soft_range
        .unwrap();
    for (text, want) in [("500", hi as f32), ("0.5", lo as f32), ("20", 20.0)] {
        let p = MenuRects::at(rig.item("widget:phosphor_decay_scale"), 0.5, 0.5);
        let l = mouse_lparam(p[0] as i16, p[1] as i16);
        rig.frame(&[(WM_KEYDOWN, 0x11, key_lparam(0x1D, false, false))]);
        rig.frame(&[(WM_MOUSEMOVE, 0, l)]);
        rig.frame(&[(WM_LBUTTONDOWN, 1, l)]);
        rig.frame(&[(WM_LBUTTONUP, 0, l)]);
        rig.frame(&[(WM_KEYUP, 0x11, key_lparam(0x1D, false, true))]);
        rig.type_text(text);
        rig.key(0x0D, 0x1C, false); // Enter
        rig.frame(&[]);
        let got = rig.state.params.phosphor_decay_scale;
        eprintln!("typed {text}: decay scale {got}");
        assert_eq!(got, want, "typed {text}");
    }
    rig.finish();
}

/// The colour target's variant name sits under its variant row: nothing for `none`, nothing for a target without
/// variants, else one line that follows the variant number. No notes or provenance in the menu.
#[test]
fn the_colour_target_variant_name_shows_under_its_row() {
    let _g = lock();
    let Some(mut rig) = Rig::with_config(
        "the_colour_target_variant_name_shows_under_its_row",
        1280,
        1080,
        tall_config(),
    ) else {
        return;
    };
    rig.state.params = Params::default();
    rig.open_menu();
    rig.click_item("header:Output and headroom");
    rig.click_item(&format!("header:{}", phoxium::ui::panels::SOURCE));
    assert!(rig.ui.item_rect("widget:source_colour_variant").is_some());
    assert!(
        rig.ui.item_rect("colour_target:variant").is_none(),
        "no name with no target"
    );
    rig.state.params.source_colour_target = phoxium::params::SOURCE_COLOUR_TARGET_NINTENDO_SUPER_GAME_BOY;
    rig.state.params.source_colour_variant = 9;
    rig.frame(&[]);
    let (row, name) = (
        rig.ui.item_rect("widget:source_colour_variant").unwrap(),
        rig.ui.item_rect("colour_target:variant").expect("the variant name"),
    );
    assert!(name[0][1] > row[1][1], "the name sits under the variant row");
    assert!(
        rig.ui.item_rect("colour_target:notes").is_none() && rig.ui.item_rect("colour_target:provenance").is_none(),
        "no notes or provenance in the menu"
    );
    // A target without variants: nothing to name.
    rig.state.params.source_colour_target = phoxium::params::SOURCE_COLOUR_TARGET_COMMODORE_64;
    rig.frame(&[]);
    let has_variants = phoxium::targets::for_value(rig.state.params.source_colour_target).is_some_and(|t| !t.variants.is_empty());
    assert_eq!(rig.ui.item_rect("colour_target:variant").is_some(), has_variants);
    // Back to none: the name goes.
    rig.state.params.source_colour_target = phoxium::params::SOURCE_COLOUR_TARGET_NONE;
    rig.frame(&[]);
    assert!(rig.ui.item_rect("colour_target:variant").is_none());
    rig.finish();
}

#[test]
fn every_panel_draws_every_parameter_once() {
    let _g = lock();
    let Some(mut rig) = Rig::with_config("every_panel_draws_every_parameter_once", 1280, 1080, tall_config()) else {
        return;
    };
    rig.state.params = Params::default();
    rig.open_menu();
    rig.click_item("advanced");
    // Every header fits on the tall menu once the open output panel is closed.
    rig.click_item("header:Output and headroom");
    let mut seen = std::collections::BTreeSet::new();
    for def in phoxium::ui::panels::PANELS {
        let ids: Vec<&str> = phoxium::ui::panels::panel_params(def.name)
            .iter()
            .map(|m| m.id)
            .collect();
        if def.name == phoxium::ui::panels::OTHER && ids.is_empty() {
            continue;
        }
        let key = format!("header:{}", def.name);
        rig.click_item(&key);
        for id in &ids {
            let has = rig.ui.item_rect(&format!("widget:{id}")).is_some()
                || rig.ui.item_rect(&format!("widget:{id}.0")).is_some();
            assert!(has, "{id} has no widget in {}", def.name);
            assert!(seen.insert(*id), "{id} drawn twice");
        }
        rig.click_item(&key);
    }
    assert_eq!(seen.len(), phoxium::params::PARAMS_META.len());
    rig.finish();
}

#[test]
fn user_preset_save_and_load_round_trip() {
    let _g = lock();
    let dir = temp_dir("presets");
    let config = UiConfig {
        presets_dir: Some(dir.clone()),
        ..tall_config()
    };
    let Some(mut rig) = Rig::with_config("user_preset_save_and_load_round_trip", 1280, 1080, config) else {
        return;
    };
    rig.state.params = Params::default();
    rig.open_menu();
    rig.drag(rig.item("widget:exposure"), 0.5, 0.8);
    let edited = rig.state.params;
    assert_ne!(edited, Params::default());
    // A shipped preset's name is refused: nothing is written.
    let shipped = Library::embedded().list()[0].id.clone();
    rig.click_item("preset_name");
    rig.type_text(&shipped);
    rig.click_item("save_preset");
    let (ok, msg) = rig.ui.preset_message().cloned().expect("a message");
    assert!(!ok && msg.contains("shipped"), "{msg}");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "a file was written");
    // Clear the name, type a new one, save.
    rig.click_item("preset_name");
    for _ in 0..shipped.len() {
        rig.key(0x08, 0x0E, false); // Backspace
    }
    rig.type_text("My Test Tube");
    rig.click_item("save_preset");
    let (ok, msg) = rig.ui.preset_message().cloned().expect("a message");
    assert!(ok, "{msg}");
    assert!(dir.join("my-test-tube.toml").exists());
    assert_eq!(
        rig.out.preset.as_deref(),
        Some("my-test-tube"),
        "the new preset is not selected"
    );
    let lib = rig.out.library.clone().expect("the library was not handed over");
    assert!(lib.find("my-test-tube").is_some());
    assert_eq!(
        lib.list().len(),
        Library::embedded().list().len() + 1,
        "shipped presets changed"
    );
    // Load: the preset read back from the file gives the edited parameters.
    let loaded = Library::load(Some(&dir));
    assert_eq!(loaded.find("my-test-tube").unwrap().params(), edited);
    // Select another preset, then the saved one again: the parameters come back.
    rig.select_preset(&shipped);
    assert_eq!(rig.state.preset.as_deref(), Some(shipped.as_str()));
    assert_ne!(rig.state.params, edited);
    rig.select_preset("my-test-tube");
    assert_eq!(rig.state.preset.as_deref(), Some("my-test-tube"));
    assert_eq!(
        rig.state.params, edited,
        "the saved preset does not give the edited parameters back"
    );
    // Edit, then update the user preset in place, then revert after another edit.
    rig.drag(rig.item("widget:exposure"), 0.5, 0.3);
    let edited2 = rig.state.params;
    rig.click_item("update_preset");
    assert_eq!(
        Library::load(Some(&dir)).find("my-test-tube").unwrap().params(),
        edited2
    );
    rig.drag(rig.item("widget:exposure"), 0.5, 0.9);
    assert_ne!(rig.state.params, edited2);
    rig.click_item("revert");
    assert_eq!(rig.state.params, edited2, "revert did not restore the preset");
    rig.finish();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The preset combo lists the shipped presets sorted by name, a horizontal separator, then the user presets
/// sorted by name on their own; a row picks the preset the order says (so the order shown is the order of the
/// library, which the previous and next preset hotkeys follow). With a filter that leaves one group there is no
/// separator.
#[test]
fn preset_combo_lists_shipped_then_a_separator_then_user_presets_each_sorted() {
    let _g = lock();
    let dir = temp_dir("preset-groups");
    // Saved in an order that is neither the name order nor the id order; the names sort case-insensitively.
    for (id, name) in [("zz-mine", "Zz mine"), ("aa-mine", "bb Mine"), ("bb-mine", "Aa mine")] {
        phoxium::presets::Preset::from_params(id, name, &Params::default())
            .unwrap()
            .save(&dir)
            .unwrap();
    }
    let library = Library::load(Some(&dir));
    let ids: Vec<String> = library.list().iter().map(|p| p.id.clone()).collect();
    assert_eq!(ids.len(), 13);
    assert_eq!(
        &ids[10..],
        ["bb-mine", "aa-mine", "zz-mine"],
        "user presets by name: Aa, bb, Zz"
    );
    assert_eq!(library.user_start(), Some(10));
    let config = UiConfig {
        presets: library,
        presets_dir: Some(dir.clone()),
        ..tall_config()
    };
    let Some(mut rig) = Rig::with_config("preset_combo_lists_shipped_then_user_each_sorted", 1280, 1080, config) else {
        return;
    };
    rig.state.preset = Some(ids[0].clone());
    rig.open_menu();
    let combo = |rig: &mut Rig| {
        rig.click(MenuRects::at(rig.ui.menu_rects().preset, 0.5, 0.5));
        rig.frame(&[]);
    };
    combo(&mut rig);
    let rows: Vec<[[f32; 2]; 2]> = (0..ids.len())
        .map(|i| {
            rig.ui
                .item_rect(&format!("preset:{i}"))
                .unwrap_or_else(|| panic!("row {i} is not in the open combo"))
        })
        .collect();
    assert!(
        rows.windows(2).all(|w| w[0][1][1] <= w[1][0][1] + 0.5),
        "rows run top to bottom"
    );
    let separator = rig.ui.item_rect("preset:separator").expect("the separator row");
    assert!(
        rows[9][1][1] <= separator[0][1] + 0.5 && separator[1][1] <= rows[10][0][1] + 0.5,
        "the separator {separator:?} sits between the last shipped row {:?} and the first user row {:?}",
        rows[9],
        rows[10]
    );
    // A preview of the open combo (look at it: shipped, a rule, then the user presets with their tag).
    rig.frame(&[(WM_MOUSEMOVE, 0, mouse_lparam(i16::MAX, i16::MAX))]);
    let img = rig.frame(&[]);
    let path = write_png(&img, "menu-preset-groups");
    eprintln!("menu-preset-groups: preview {}", path.display());
    // The rows pick the presets in the order of the library: the first, the last shipped one, the first and the
    // last user ones.
    for i in [0, 9, 10, 12] {
        let r = rig.ui.item_rect(&format!("preset:{i}")).expect("row in the open combo");
        rig.click(MenuRects::at(r, 0.5, 0.5));
        rig.frame(&[]);
        assert_eq!(rig.out.preset.as_deref(), Some(ids[i].as_str()), "row {i}");
        rig.out.preset = None;
        combo(&mut rig);
    }
    // A filter that leaves only user presets, or only shipped ones, draws no separator.
    // (The first click on the filter closes the open combo, the second focuses the box.)
    let filter = rig.ui.menu_rects().filter;
    rig.click(MenuRects::at(filter, 0.5, 0.5));
    rig.click(MenuRects::at(filter, 0.5, 0.5));
    rig.type_text("-mine");
    combo(&mut rig);
    assert!(rig.ui.item_rect("preset:2").is_some() && rig.ui.item_rect("preset:3").is_none());
    assert!(rig.ui.item_rect("preset:separator").is_none(), "only user presets");
    rig.click(MenuRects::at(filter, 0.5, 0.5));
    rig.click(MenuRects::at(filter, 0.5, 0.5));
    for _ in 0..5 {
        rig.key(0x08, 0x0E, false);
    }
    rig.type_text("sony");
    combo(&mut rig);
    assert!(rig.ui.item_rect("preset:0").is_some() && rig.ui.item_rect("preset:separator").is_none());
    rig.finish();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A made-up adapter list like the reference PC's: the RTX (Vulkan 0) and the Intel UHD driving the monitors.
fn fake_gpu() -> phoxium::adapters::Current {
    use phoxium::adapters::{AdapterEntry, AdapterId, Current, GpuSelection, Luid, Owner, Reason, VulkanDevice};
    let id = |name: &str, vendor: u32, device: u32, luid: Luid| AdapterId {
        name: name.into(),
        vendor,
        device,
        subsys: 0,
        nth: 1,
        luid: Some(luid),
    };
    let rtx = Luid { high: 0, low: 0x17801 };
    let uhd = Luid { high: 0, low: 0x10301 };
    Current {
        selection: GpuSelection {
            chosen: Some(VulkanDevice {
                index: 0,
                name: "NVIDIA RTX A2000 Laptop GPU".into(),
                luid: Some(rtx),
            }),
            reason: Reason::Default,
            owner: Some(Owner {
                name: "Intel(R) UHD Graphics".into(),
                luid: uhd,
            }),
            warnings: vec!["the chosen adapter does not drive the covered monitor (cross-adapter present)".into()],
            missing: None,
            upgraded_setting: None,
        },
        adapters: vec![
            AdapterEntry {
                name: "NVIDIA RTX A2000 Laptop GPU".into(),
                luid: Some(rtx),
                owns_monitors: false,
                software: false,
                vulkan_index: Some(0),
                id: Some(id("NVIDIA RTX A2000 Laptop GPU", 0x10DE, 0x25B8, rtx)),
            },
            AdapterEntry {
                name: "Intel(R) UHD Graphics".into(),
                luid: Some(uhd),
                owns_monitors: true,
                software: false,
                vulkan_index: Some(1),
                id: Some(id("Intel(R) UHD Graphics", 0x8086, 0x9A60, uhd)),
            },
        ],
        setting: "auto".into(),
    }
}

fn system_config() -> UiConfig {
    use phoxium::ui::menu::HotkeyRow;
    UiConfig {
        gpu: Some(fake_gpu()),
        hotkeys: vec![
            HotkeyRow {
                action: "Quit".into(),
                bound: Some("Ctrl+Alt+End".into()),
                wanted: "Ctrl+Alt+End".into(),
            },
            HotkeyRow {
                action: "Settings menu".into(),
                bound: Some("Ctrl+Alt+F10".into()),
                wanted: "F10".into(),
            },
            HotkeyRow {
                action: "Overlay".into(),
                bound: None,
                wanted: "Ctrl+Alt+F9".into(),
            },
        ],
        settings_dir: Some(std::path::PathBuf::from(r"C:\Users\someone\AppData\Roaming\Phoxium")),
        ..tall_config()
    }
}

#[test]
fn the_adapter_combo_returns_the_setting_for_the_next_start() {
    let _g = lock();
    let Some(mut rig) = Rig::with_config(
        "the_adapter_combo_returns_the_setting_for_the_next_start",
        1280,
        1080,
        system_config(),
    ) else {
        return;
    };
    rig.open_menu();
    rig.click_item("header:Output and headroom");
    rig.click_item("header:GPU and system");
    rig.click_item("gpu_adapter");
    rig.key(0x28, 0x50, true); // Down: from Automatic to the first adapter
    rig.key(0x0D, 0x1C, false);
    rig.frame(&[]);
    let want = fake_gpu().adapters[0].setting_value();
    assert_eq!(rig.out.gpu_adapter.as_deref(), Some(want.as_str()));
    assert_eq!(rig.out.params, None, "the adapter choice changed parameters");
    rig.finish();
}

/// A29: the Source section's Capture combo (zero-copy by default) switches to the CPU copy and back through Win32
/// messages, the status line follows the path in force at once, the Performance section only shows it, and a
/// machine that cannot run zero-copy shows that choice disabled with the reason.
#[test]
fn the_capture_combo_switches_both_ways_and_shows_the_path_in_force() {
    use phoxium::ui::CaptureView;
    use phoxium::zerocopy::CapturePath;
    let _g = lock();
    let Some(mut rig) = Rig::with_config(
        "the_capture_combo_switches_both_ways_and_shows_the_path_in_force",
        1280,
        1080,
        system_config(),
    ) else {
        return;
    };
    // The app's state at start: the default, running.
    rig.state.capture = CaptureView {
        copy_ms: Some(0.2),
        ..CaptureView::default()
    };
    assert_eq!(rig.state.capture.requested, CapturePath::ZeroCopy);
    rig.open_menu();
    rig.click_item("header:Output and headroom");
    rig.click_item("header:Source");
    rig.item("capture_path:active");
    // Zero-copy -> CPU copy: Down, Enter.
    rig.click_item("capture_path");
    rig.key(0x28, 0x50, true);
    rig.key(0x0D, 0x1C, false);
    rig.frame(&[]);
    assert_eq!(rig.out.capture_path, Some(CapturePath::Cpu));
    assert_eq!(rig.out.params, None, "the capture path changed parameters");
    // The app applies it (its next state).
    rig.state.capture = CaptureView {
        requested: CapturePath::Cpu,
        active: CapturePath::Cpu,
        copy_ms: Some(1.25),
        ..CaptureView::default()
    };
    rig.out.capture_path = None;
    rig.frame(&[]);
    // CPU copy -> zero-copy: a click on the entry of the open list.
    rig.click_item("capture_path");
    let zero = rig
        .ui
        .item_rect("capture_path:zero_copy")
        .expect("the zero-copy entry in the open combo");
    rig.click(MenuRects::at(zero, 0.5, 0.5));
    rig.frame(&[]);
    assert_eq!(rig.out.capture_path, Some(CapturePath::ZeroCopy));
    // The Performance section shows the path in force, read-only (the Source section closed meanwhile).
    rig.click_item("header:Source");
    rig.click_item("header:Performance and diagnostics");
    rig.item("capture_path:status");
    assert!(
        rig.ui.item_rect("capture_path").is_none(),
        "the choice exists once, in the Source section"
    );
    rig.click_item("header:Performance and diagnostics");
    rig.click_item("header:Source");
    // The app's answer on a machine that cannot share textures: the CPU copy, the reason, zero-copy disabled.
    let why = "capture and rendering run on different adapters".to_string();
    rig.state.capture = CaptureView {
        requested: CapturePath::ZeroCopy,
        active: CapturePath::Cpu,
        reason: Some(why.clone()),
        unavailable: Some(why.clone()),
        copy_ms: Some(1.25),
    };
    rig.frame(&[]);
    rig.item("capture_path:reason");
    // Choose the CPU copy explicitly, then try to come back: the disabled entry cannot be chosen.
    rig.out.capture_path = None;
    rig.click_item("capture_path");
    rig.key(0x28, 0x50, true);
    rig.key(0x0D, 0x1C, false);
    rig.frame(&[]);
    assert_eq!(rig.out.capture_path, Some(CapturePath::Cpu));
    rig.state.capture = CaptureView {
        requested: CapturePath::Cpu,
        active: CapturePath::Cpu,
        reason: None,
        unavailable: Some(why),
        copy_ms: Some(1.25),
    };
    rig.out.capture_path = None;
    rig.frame(&[]);
    rig.item("capture_path:reason");
    rig.click_item("capture_path");
    // Click the disabled zero-copy entry, then Up and Enter: nothing is chosen.
    let zero = rig
        .ui
        .item_rect("capture_path:zero_copy")
        .expect("the zero-copy entry in the open combo");
    rig.click(MenuRects::at(zero, 0.5, 0.5));
    rig.key(0x26, 0x48, true);
    rig.key(0x0D, 0x1C, false);
    rig.frame(&[]);
    assert_eq!(rig.out.capture_path, None, "a disabled zero-copy entry was chosen");
    rig.finish();
}

/// One panel preview: the menu at 1280 x 720 (100 %) after `setup` (clicks), written to `target/ui-preview`.
fn panel_preview(name: &str, config: UiConfig, setup: impl FnOnce(&mut Rig)) {
    let Some(mut rig) = Rig::with_config(name, 1280, 720, config) else {
        return;
    };
    rig.full_chain();
    rig.bg = preview_background(1280, 720);
    rig.state = preview_state();
    // The preset's own parameters: only what the setup changes shows as modified.
    rig.state.params = Library::embedded().list()[0].params();
    rig.open_menu();
    setup(&mut rig);
    rig.frame(&[(WM_MOUSEMOVE, 0, mouse_lparam(i16::MAX, i16::MAX))]);
    rig.frame(&[]);
    let img = rig.frame(&[]);
    let path = write_png(&img, name);
    eprintln!("{name}: preview {}", path.display());
    rig.finish();
}

#[test]
fn panel_preview_images() {
    let _g = lock();
    let menu_at = |x: f32| UiConfig {
        layout: phoxium::settings::UiState {
            menu_x: x,
            menu_h: 690.0,
            ..phoxium::settings::UiState::default()
        },
        ..UiConfig::headless()
    };
    // The mask panel with the advanced rows and two changed values.
    panel_preview("menu-panel-mask", menu_at(20.0), |rig| {
        rig.state.params.mask_pitch_mm = 0.42;
        rig.state.params.mask_type = phoxium::params::MASK_TYPE_SLOT_MASK;
        rig.click_item("header:Output and headroom");
        rig.click_item("advanced");
        rig.click_item("header:Mask");
        rig.scroll_to("header:Mask");
    });
    // Gun and white: vectors (per-gun sliders, chromaticities) under subheadings.
    panel_preview("menu-panel-gun", menu_at(20.0), |rig| {
        rig.click_item("header:Output and headroom");
        rig.click_item("header:Gun and white");
        rig.scroll_to("header:Gun and white");
    });
    // GPU and system.
    panel_preview(
        "menu-panel-system",
        UiConfig {
            layout: menu_at(20.0).layout,
            ..system_config()
        },
        |rig| {
            rig.click_item("header:Output and headroom");
            // The last header: scroll to the end of the body first, open it, then scroll to its rows.
            rig.scroll_down(20);
            rig.click_item("header:GPU and system");
            rig.scroll_down(20);
        },
    );
    // A search.
    panel_preview("menu-search", menu_at(20.0), |rig| {
        rig.click_item("search");
        rig.type_text("white");
    });
    // A colour target with variants: the variant name under its row (Mega Drive, Regen), found by a search.
    panel_preview("menu-colour-target", menu_at(20.0), |rig| {
        rig.state.params.source_colour_target = phoxium::params::SOURCE_COLOUR_TARGET_SEGA_MEGA_DRIVE;
        rig.state.params.source_colour_variant = 3;
        rig.click_item("search");
        rig.type_text("colour variant");
    });
}

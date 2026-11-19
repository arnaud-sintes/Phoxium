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
use ultimatecrt::gpu::{Headless, ImageF32, Settings, SourceFrame, TargetFormat};
use ultimatecrt::params::Params;
use ultimatecrt::presets::Library;
use ultimatecrt::scaling::View;
use ultimatecrt::ui::input::{
    key_lparam, mouse_lparam, WM_CHAR, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
};
use ultimatecrt::ui::menu::MenuRects;
use ultimatecrt::ui::{Ui, UiConfig, UiOutput, UiState};

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
}

impl Rig {
    fn new(test: &str, w: u32, h: u32) -> Option<Rig> {
        Self::with_config(test, w, h, UiConfig::headless())
    }

    fn with_config(test: &str, w: u32, h: u32, config: UiConfig) -> Option<Rig> {
        let gpu = common::gpu_or_skip(test)?;
        let ui = Ui::new(gpu.context(), Headless::vk_format(TargetFormat::Bgra8Unorm), config).expect("create the UI");
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
        })
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
        let src = SourceFrame {
            width: self.w,
            height: self.h,
            bgra: &self.bg,
        };
        self.gpu
            .render_with_ui(
                Some(src),
                View::full(self.w, self.h),
                &Settings::identity(),
                &Params::identity_gun(),
                (self.w, self.h),
                TargetFormat::Bgra8Unorm,
                &mut self.ui,
            )
            .expect("render with the UI")
            .output
    }

    /// The output without any UI (plain `render`).
    fn plain(&mut self) -> ImageF32 {
        let src = SourceFrame {
            width: self.w,
            height: self.h,
            bgra: &self.bg,
        };
        self.gpu
            .render(
                Some(src),
                View::full(self.w, self.h),
                &Settings::identity(),
                &Params::identity_gun(),
                (self.w, self.h),
                TargetFormat::Bgra8Unorm,
            )
            .expect("render")
            .output
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
    let low = rig.state.params.exposure;
    rig.frame(&[(WM_MOUSEMOVE, 1, hi_l)]);
    rig.frame(&[(WM_LBUTTONUP, 0, hi_l)]);
    rig.frame(&[]);
    let high = rig.state.params.exposure;
    let meta = ultimatecrt::params::meta("exposure").unwrap();
    let (min, max) = meta.soft_range.unwrap();
    eprintln!("exposure: {before} -> {low} (press) -> {high} (drag), soft range {min}..{max}");
    assert!(
        high > low + 0.3 * (max - min) as f32,
        "the drag did not move the slider"
    );
    assert_eq!(
        rig.out.params.map(|p| p.exposure),
        Some(high),
        "the change was not returned"
    );

    // Headroom mode combo: click, Down, Enter selects another mode (keyboard navigation wraps from the last
    // entry, `auto`, the default, to the first).
    let mode = rig.state.params.output_headroom_mode;
    let modes = ultimatecrt::params::meta("output_headroom_mode").unwrap().values.len() as u32;
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
        (r[1][0] - (640.0 - ultimatecrt::ui::overlay::MARGIN)).abs() <= 1.0,
        "overlay window {r:?}"
    );
    let (_, outside) = diff_counts(&img, &plain, r, 1.0);
    assert_eq!(outside, 0, "the overlay drew outside its window");
    // A padding pixel (no text): window colour (0, 0, 1) at alpha BG_ALPHA over the background.
    let a = ultimatecrt::ui::overlay::BG_ALPHA;
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
    let encode = rig.gpu.last_timings().and_then(|t| t.stage_ms("encode"));
    eprintln!(
        "UI GPU cost at 1920x1080 (median of 8 frames): overlay {overlay:.4} ms, menu + overlay {both:.4} ms \
         (encode {encode:.4?} ms)"
    );
    // Generous bounds (headless, idle clocks): the plan's overlay budget is 0.2 ms (T-52b).
    assert!(overlay < 1.0, "overlay {overlay} ms");
    assert!(both < 2.0, "menu {both} ms");
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
    use ultimatecrt::gpu::{FrameTimings, StageTiming};
    let params = Params::default();
    let stage = |name, ms: Option<f64>, bypassed| StageTiming { name, ms, bypassed };
    UiState {
        params,
        preset: Library::embedded().list().first().map(|p| p.id.clone()),
        headroom: Some(crt_model::output::Headroom {
            mask_peak: 1.842,
            mask_peak_physical: 1.842,
            scanline_peak: 1.318,
            matrix_max: 1.036,
            exposure: 1.0,
        }),
        placement: Some(ultimatecrt::placement::readout(&params, (320, 240), (1280, 720), true)),
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
        layout: ultimatecrt::settings::UiState {
            menu_h: if expand { 1240.0 / scale.max(1.0) } else { 560.0 },
            ..ultimatecrt::settings::UiState::default()
        },
        ..UiConfig::headless()
    };
    let Some(mut rig) = Rig::with_config(name, w, h, config) else {
        return;
    };
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

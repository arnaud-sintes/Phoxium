//! Headless GPU tests of the window picker and the "Choose a window to capture" screen (A23): the card drawn by
//! the UI layer after `encode` into an offscreen `B8G8R8A8_UNORM` target, synthetic windows as rows, input fed
//! as raw Win32 messages through `Ui::handle_win32_message`. No window is created, enumerated or captured and no
//! focus changes (a headless `Ui` has no output window).
//!
//! Dear ImGui allows one context per process: every test holds [`IMGUI`] while its `Ui` lives.

mod common;

use std::sync::{Mutex, MutexGuard};

use ultimatecrt::gpu::{Headless, ImageF32, Settings, SourceFrame, StageReadback, TargetFormat};
use ultimatecrt::params::Params;
use ultimatecrt::picker::{self, Row};
use ultimatecrt::scaling::View;
use ultimatecrt::ui::input::{
    key_lparam, mouse_lparam, WM_CHAR, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE,
};
use ultimatecrt::ui::menu::MenuRects;
use ultimatecrt::ui::{AttachedView, SourceAction, SourceView, Ui, UiConfig, UiState, View as UiView};

static IMGUI: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    IMGUI.lock().unwrap_or_else(|e| e.into_inner())
}

/// Six synthetic windows, as the app's enumeration would list them (one minimised).
fn windows() -> Vec<Row> {
    let row = |hwnd: isize, title: &str, exe: &str, class: &str, w: i32, h: i32, monitor: usize| Row {
        hwnd,
        title: title.into(),
        exe: Some(exe.into()),
        exe_path: Some(format!(r"C:\Games\{exe}")),
        class: class.into(),
        width: w,
        height: h,
        monitor: Some(monitor),
        minimized: false,
    };
    let mut rows = vec![
        row(
            0x101,
            "Super Metroid (USA) - Snes9x 1.62.3",
            "snes9x-x64.exe",
            "Snes9X: WndClass",
            1024,
            896,
            0,
        ),
        row(0x102, "pixel-demo", "pixel-demo.exe", "pixel_demo", 1280, 960, 0),
        row(0x103, "Mina the Hollower", "Mina.exe", "UnityWndClass", 1920, 1080, 1),
        row(
            0x104,
            "Dolphin 2412 | JIT64 DC | Direct3D 11 | Metroid Prime",
            "Dolphin.exe",
            "Qt5QWindowIcon",
            1280,
            1056,
            1,
        ),
        row(0x105, "notes.txt - Notepad", "Notepad.exe", "Notepad", 900, 700, 0),
        row(
            0x106,
            "RetroArch 1.19.1 - Mesen",
            "retroarch.exe",
            "RetroArch",
            160,
            28,
            0,
        ),
    ];
    rows[5].minimized = true;
    rows
}

/// A 32 x 32 RGBA icon: a rounded square in `rgb` with a lighter inner square.
fn icon(rgb: [u8; 3]) -> (u32, u32, Vec<u8>) {
    let mut v = Vec::with_capacity(32 * 32 * 4);
    for y in 0..32i32 {
        for x in 0..32i32 {
            let (dx, dy) = ((x - 16).abs(), (y - 16).abs());
            let inside = dx.max(dy) < 14 && !(dx > 10 && dy > 10 && (dx - 10) * (dx - 10) + (dy - 10) * (dy - 10) > 16);
            let inner = dx.max(dy) < 6;
            let c = if inner { rgb.map(|c| c / 2 + 128) } else { rgb };
            v.extend_from_slice(&[c[0], c[1], c[2], if inside { 255 } else { 0 }]);
        }
    }
    (32, 32, v)
}

/// A pixel-art-like preview (BGRA 256 x 224 scene, sky, ground, a few blocks), downscaled as the app does.
fn preview_picture() -> (u32, u32, Vec<u8>) {
    let (w, h) = (256u32, 224u32);
    let mut bgra = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = if y > 180 {
                if (x / 16 + y / 16) % 2 == 0 {
                    [160, 96, 48]
                } else {
                    [136, 80, 40]
                }
            } else if (100..116).contains(&y) && (64..160).contains(&x) && (x / 16) % 2 == 0 {
                [224, 160, 64]
            } else if (150..180).contains(&y) && (180..196).contains(&x) {
                [48, 160, 64]
            } else {
                [92, 148, 252 - (y / 4) as u8]
            };
            bgra.extend_from_slice(&[b, g, r, 255]);
        }
    }
    picker::thumbnail(&bgra, w, h, 480, 270)
}

struct Rig {
    ui: Ui,
    gpu: Headless,
    w: u32,
    h: u32,
    bg: Vec<u8>,
    state: UiState,
    /// Every source action returned so far, in order.
    actions: Vec<SourceAction>,
    uploaded: bool,
}

impl Rig {
    fn new(test: &str, w: u32, h: u32) -> Option<Rig> {
        let mut gpu = common::gpu_or_skip(test)?;
        gpu.enable_only(&[]).expect("chain slots off");
        gpu.set_read_stages(StageReadback::None);
        let mut ui = Ui::new(
            gpu.context(),
            Headless::vk_format(TargetFormat::Bgra8Unorm),
            UiConfig::headless(),
        )
        .expect("create the UI");
        ui.set_frame_time(Some(1.0 / 60.0));
        Some(Rig {
            ui,
            gpu,
            w,
            h,
            // The idle frame: black (no source).
            bg: [0u8, 0, 0, 255].repeat((w * h) as usize),
            state: UiState {
                params: Params::identity_gun(),
                ..UiState::default()
            },
            actions: Vec::new(),
            uploaded: false,
        })
    }

    fn render(&mut self, ui: bool) -> ImageF32 {
        let upload = !std::mem::replace(&mut self.uploaded, true);
        let src = upload.then_some(SourceFrame {
            width: self.w,
            height: self.h,
            bgra: &self.bg,
        });
        let (view, size) = (View::full(self.w, self.h), (self.w, self.h));
        let params = Params::identity_gun();
        let r = if ui {
            self.gpu.render_with_ui(
                src,
                view,
                &Settings::identity(),
                &params,
                size,
                TargetFormat::Bgra8Unorm,
                &mut self.ui,
            )
        } else {
            self.gpu.render(
                src,
                view,
                &Settings::identity(),
                &params,
                size,
                TargetFormat::Bgra8Unorm,
            )
        };
        r.expect("render").output
    }

    fn frame(&mut self, msgs: &[(u32, usize, isize)]) -> ImageF32 {
        for &(m, w, l) in msgs {
            self.ui.handle_win32_message(m, w, l);
        }
        let out = self.ui.frame(&self.state);
        if let Some(a) = out.source {
            self.actions.push(a);
        }
        self.render(true)
    }

    fn key(&mut self, vk: usize, scan: u32, extended: bool) {
        self.frame(&[(WM_KEYDOWN, vk, key_lparam(scan, extended, false))]);
        self.frame(&[(WM_KEYUP, vk, key_lparam(scan, extended, true))]);
    }

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.frame(&[(WM_CHAR, c as usize, 1)]);
        }
    }

    fn item(&self, key: &str) -> [[f32; 2]; 2] {
        self.ui
            .item_rect(key)
            .unwrap_or_else(|| panic!("no picker item {key} in the last frame"))
    }

    fn click(&mut self, p: [f32; 2]) {
        let l = mouse_lparam(p[0] as i16, p[1] as i16);
        self.frame(&[(WM_MOUSEMOVE, 0, l)]);
        self.frame(&[(WM_LBUTTONDOWN, 1, l)]);
        self.frame(&[(WM_LBUTTONUP, 0, l)]);
        // What the release frame did is handed over at the next `Ui::frame`.
        self.frame(&[]);
    }

    fn double_click(&mut self, p: [f32; 2]) {
        let l = mouse_lparam(p[0] as i16, p[1] as i16);
        self.frame(&[(WM_MOUSEMOVE, 0, l)]);
        self.frame(&[(WM_LBUTTONDOWN, 1, l)]);
        self.frame(&[(WM_LBUTTONUP, 0, l)]);
        self.frame(&[(WM_LBUTTONDBLCLK, 1, l)]);
        self.frame(&[(WM_LBUTTONUP, 0, l)]);
        self.frame(&[]);
    }

    /// The no-source screen with the six synthetic windows, icons for all but one, settled.
    fn picker_with_windows(&mut self) {
        self.ui.source_gone();
        self.ui.show_picker(false);
        self.ui.set_picker_rows(windows());
        let colours = [
            [233, 162, 59],
            [116, 203, 156],
            [242, 126, 122],
            [110, 160, 240],
            [190, 190, 200],
        ];
        for (r, c) in windows().iter().zip(colours) {
            let path = r.exe_path.clone().unwrap();
            self.ui.set_icon(&path, Some(icon(c)));
        }
        // The minimised one has no icon file: a placeholder.
        let last = windows()[5].exe_path.clone().unwrap();
        self.ui.set_icon(&last, None);
        assert!(self.ui.missing_icons().is_empty());
        for _ in 0..3 {
            self.frame(&[]);
        }
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

/// Pixels inside `r` that differ between `a` and `b`.
fn changed_inside(a: &ImageF32, b: &ImageF32, r: [[f32; 2]; 2]) -> usize {
    let mut n = 0;
    for y in (r[0][1].max(0.0) as u32)..(r[1][1].min(a.height as f32) as u32) {
        for x in (r[0][0].max(0.0) as u32)..(r[1][0].min(a.width as f32) as u32) {
            let (p, q) = (a.pixel(x, y), b.pixel(x, y));
            if p.iter().zip(q).any(|(&x, y)| code(x) != code(y)) {
                n += 1;
            }
        }
    }
    n
}

#[test]
fn the_no_source_screen_draws_the_card() {
    let _g = lock();
    let Some(mut rig) = Rig::new("the_no_source_screen_draws_the_card", 1280, 720) else {
        return;
    };
    let plain = rig.render(false);
    assert!(!rig.ui.picker_open());
    rig.ui.source_gone();
    rig.ui.show_picker(false);
    assert!(rig.ui.picker_open() && rig.ui.is_open());
    assert_eq!(rig.ui.view(), UiView::Picker);
    rig.frame(&[]);
    let img = rig.frame(&[]);
    let card = rig.item("picker:card");
    let (cw, ch) = (card[1][0] - card[0][0], card[1][1] - card[0][1]);
    assert!(cw > 600.0 && ch > 400.0, "card {card:?}");
    // Centred.
    assert!(((card[0][0] + card[1][0]) * 0.5 - 640.0).abs() < 2.0);
    assert!(((card[0][1] + card[1][1]) * 0.5 - 360.0).abs() < 2.0);
    let inside = changed_inside(&plain, &img, card);
    assert!(
        inside as f32 > 0.9 * cw * ch,
        "the card is not drawn: {inside} px changed"
    );
    // With no windows, the empty-list text; Capture is disabled (Enter does nothing).
    rig.key(0x0D, 0x1C, false);
    assert!(rig.actions.is_empty(), "{:?}", rig.actions);
    rig.finish();
}

#[test]
fn synthetic_windows_are_chosen_by_keys_and_mouse() {
    let _g = lock();
    let Some(mut rig) = Rig::new("synthetic_windows_are_chosen_by_keys_and_mouse", 1280, 720) else {
        return;
    };
    rig.picker_with_windows();
    for r in windows() {
        rig.item(&format!("picker:row:{}", r.hwnd));
    }
    assert_eq!(rig.ui.picker().selected, Some(0x101), "the first row is selected");
    // Down, Down, Up, Enter: the second window.
    rig.key(0x28, 0x50, true);
    rig.key(0x28, 0x50, true);
    rig.key(0x26, 0x48, true);
    assert_eq!(rig.ui.picker().selected, Some(0x102));
    rig.key(0x0D, 0x1C, false);
    assert_eq!(rig.actions, [SourceAction::Attach(0x102)]);
    // A click selects, a double-click captures.
    let r4 = rig.item("picker:row:260");
    rig.click(MenuRects::at(r4, 0.5, 0.5));
    assert_eq!(rig.ui.picker().selected, Some(0x104));
    assert_eq!(rig.actions.len(), 1, "a single click does not capture");
    let r3 = rig.item("picker:row:259");
    rig.double_click(MenuRects::at(r3, 0.5, 0.5));
    assert_eq!(rig.actions.last(), Some(&SourceAction::Attach(0x103)));
    // The Capture button captures the selection.
    let n = rig.actions.len();
    let capture = rig.item("picker:capture");
    rig.click(MenuRects::at(capture, 0.5, 0.5));
    assert_eq!(rig.actions.len(), n + 1);
    assert_eq!(rig.actions.last(), Some(&SourceAction::Attach(0x103)));
    // Typing searches; the rows the search hides are not drawn; Esc clears.
    rig.type_text("metroid");
    assert_eq!(rig.ui.picker().query, "metroid");
    rig.frame(&[]);
    assert!(rig.ui.item_rect("picker:row:258").is_none(), "pixel-demo is hidden");
    rig.item("picker:row:257");
    rig.item("picker:row:260");
    assert_eq!(rig.ui.picker().selected, Some(0x101), "the first match is selected");
    rig.key(0x1B, 0x01, false);
    assert_eq!(rig.ui.picker().query, "");
    // Esc without a source: the picker stays, nothing is cancelled.
    rig.key(0x1B, 0x01, false);
    assert!(rig.ui.picker_open());
    assert!(!rig.actions.contains(&SourceAction::Cancel));
    // Refresh asks the app for a new enumeration.
    let refresh = rig.item("picker:refresh");
    rig.click(MenuRects::at(refresh, 0.5, 0.5));
    assert_eq!(rig.actions.last(), Some(&SourceAction::Refresh));
    rig.finish();
}

#[test]
fn the_menu_key_and_an_attach_leave_the_picker() {
    let _g = lock();
    let Some(mut rig) = Rig::new("the_menu_key_and_an_attach_leave_the_picker", 1280, 720) else {
        return;
    };
    rig.picker_with_windows();
    // The menu key shows the settings; closing them without a source comes back to the picker.
    rig.ui.request_toggle();
    rig.frame(&[]);
    assert_eq!(rig.ui.view(), UiView::Settings);
    assert!(rig.ui.is_open());
    rig.key(0x1B, 0x01, false); // Esc in the settings
    assert!(
        rig.ui.picker_open(),
        "without a source the settings close to the picker"
    );
    // The app attached the chosen window: the picker closes (the foreground goes to it in the live app).
    rig.ui.attached(0x102);
    assert!(!rig.ui.is_open());
    // Change source from the settings: the picker with Cancel; Esc cancels and closes.
    rig.ui.request_toggle();
    rig.frame(&[]);
    rig.frame(&[]);
    let header = rig.item("header:Source");
    rig.click(MenuRects::at(header, 0.5, 0.5));
    let change = rig.item("source:change");
    rig.click(MenuRects::at(change, 0.5, 0.5));
    assert_eq!(rig.actions.last(), Some(&SourceAction::ChangeSource));
    assert!(rig.ui.picker_open());
    rig.frame(&[]);
    rig.item("picker:cancel");
    rig.key(0x1B, 0x01, false);
    assert_eq!(rig.actions.last(), Some(&SourceAction::Cancel));
    assert!(!rig.ui.is_open());
    // The ask-at-start box goes back to the app as a preference.
    rig.ui.show_picker(true);
    rig.frame(&[]);
    let ask = rig.item("picker:ask");
    let before = rig.ui.source_prefs().ask_at_start;
    rig.click(MenuRects::at(ask, 0.05, 0.5));
    assert_eq!(rig.ui.source_prefs().ask_at_start, !before);
    rig.finish();
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

/// The picker with six synthetic windows, the pixel-demo row selected with its preview, at `w` x `h` and UI
/// scale `scale`.
fn picker_preview(name: &str, w: u32, h: u32, scale: f32, message: Option<&str>) {
    let Some(mut rig) = Rig::new(name, w, h) else {
        return;
    };
    rig.ui.set_ui_scale(scale);
    rig.picker_with_windows();
    rig.key(0x28, 0x50, true); // Down: pixel-demo
    let (tw, th, px) = preview_picture();
    rig.ui.set_thumbnail(0x102, tw, th, &px);
    rig.ui.set_picker_message(message.map(|m| (false, m.to_string())));
    rig.frame(&[(WM_MOUSEMOVE, 0, mouse_lparam(i16::MAX, i16::MAX))]);
    rig.frame(&[]);
    let img = rig.frame(&[]);
    let card = rig.item("picker:card");
    assert!(
        card[1][0] <= w as f32 && card[1][1] <= h as f32,
        "{name}: card {card:?}"
    );
    let path = write_png(&img, name);
    eprintln!("{name}: card {card:?}, preview {}", path.display());
    rig.finish();
}

#[test]
fn picker_preview_images() {
    let _g = lock();
    picker_preview("picker-1280x720-100", 1280, 720, 1.0, None);
    picker_preview(
        "picker-1920x1080-100-message",
        1920,
        1080,
        1.0,
        Some("Mina.exe - Mina the Hollower closed and did not come back within 30 s."),
    );
    picker_preview("picker-2560x1440-200", 2560, 1440, 2.0, None);
}

#[test]
fn source_section_preview_image() {
    let _g = lock();
    let name = "menu-source-section";
    let Some(mut rig) = Rig::new(name, 1280, 720) else {
        return;
    };
    rig.state.source = SourceView {
        attached: Some(AttachedView {
            exe: Some("pixel-demo.exe".into()),
            class: "pixel_demo".into(),
            title: "pixel-demo".into(),
            frame: (1280, 960),
            native: (320, 240),
            scale: "x4 detected".into(),
            crop: "client area".into(),
            profile: true,
        }),
        waiting: None,
    };
    rig.ui.request_toggle();
    for _ in 0..3 {
        rig.frame(&[]);
    }
    let header = rig.item("header:Source");
    rig.click(MenuRects::at(header, 0.5, 0.5));
    rig.item("source:change");
    rig.item("source:detach");
    rig.item("source:detected");
    rig.frame(&[(WM_MOUSEMOVE, 0, mouse_lparam(i16::MAX, i16::MAX))]);
    let img = rig.frame(&[]);
    let path = write_png(&img, name);
    eprintln!("{name}: preview {}", path.display());
    // Detach goes back to the app.
    let detach = rig.item("source:detach");
    rig.click(MenuRects::at(detach, 0.5, 0.5));
    assert_eq!(rig.actions.last(), Some(&SourceAction::Detach));
    rig.finish();
}

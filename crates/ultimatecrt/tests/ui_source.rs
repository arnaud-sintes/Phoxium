//! Headless GPU tests of the menu's Source section controls (owner requirement 2026-11-20): Detect black bars,
//! Reset crop, Undo, Detect at attach, the detection thresholds (Advanced) and the source scaling multiplier
//! (Auto, None, x1 to x10, X and Y separately). The menu is drawn by the UI layer into an offscreen
//! `B8G8R8A8_UNORM` target and driven by raw Win32 messages through `Ui::handle_win32_message`; what the app
//! would do with the requests is checked with the same pure functions it calls (`bars`, `scaling::Multiplier`).
//! No window is created or captured and no focus changes (a headless `Ui` has no output window).
//!
//! Dear ImGui allows one context per process: every test holds [`IMGUI`] while its `Ui` lives.

mod common;

use std::sync::{Mutex, MutexGuard};

use ultimatecrt::bars::{Bars, Found};
use ultimatecrt::gpu::{Headless, ImageF32, Settings, SourceFrame, StageReadback, TargetFormat};
use ultimatecrt::params::{Params, SOURCE_DIVISOR_MODE_MANUAL, SOURCE_DIVISOR_MODE_NONE};
use ultimatecrt::scaling::{Multiplier, Rect, View};
use ultimatecrt::settings::SourceProfile;
use ultimatecrt::ui::input::{mouse_lparam, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE};
use ultimatecrt::ui::menu::MenuRects;
use ultimatecrt::ui::{AttachedView, BarsView, SourceAction, SourceView, Ui, UiConfig, UiState};

static IMGUI: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    IMGUI.lock().unwrap_or_else(|e| e.into_inner())
}

/// A 256 x 224 game at x3 with a 32 px bar left and right, as the Source section would describe it.
fn attached() -> AttachedView {
    AttachedView {
        exe: Some("snes9x-x64.exe".into()),
        class: "Snes9X: WndClass".into(),
        title: "Super Metroid (USA) - Snes9x 1.62.3".into(),
        frame: (832, 672),
        native: (256, 224),
        scale: "x3 detected".into(),
        crop: "client area".into(),
        profile: false,
        multiplier: Multiplier::Auto,
        detected: Some((3, 3)),
        crop_manual: false,
        bars: BarsView::default(),
    }
}

struct Rig {
    ui: Ui,
    gpu: Headless,
    w: u32,
    h: u32,
    bg: Vec<u8>,
    state: UiState,
    actions: Vec<SourceAction>,
    uploaded: bool,
}

impl Rig {
    fn new(test: &str, w: u32, h: u32) -> Option<Rig> {
        let mut gpu = common::gpu_or_skip(test)?;
        gpu.enable_only(&[]).expect("chain slots off");
        gpu.set_read_stages(StageReadback::None);
        let mut config = UiConfig::headless();
        // A tall menu: the whole Source section fits without scrolling.
        config.layout.menu_h = h as f32 - 40.0;
        config.layout.menu_w = 520.0;
        let mut ui =
            Ui::new(gpu.context(), Headless::vk_format(TargetFormat::Bgra8Unorm), config).expect("create the UI");
        ui.set_frame_time(Some(1.0 / 60.0));
        // A picture with black bars behind the menu (pillarbox), as a captured emulator window.
        let mut bg = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let inside = x >= w / 8 && x < w - w / 8;
                let c = if inside {
                    [(70 + (x / 24 + y / 24) % 2 * 40) as u8, 110, 160]
                } else {
                    [0, 0, 0]
                };
                bg.extend_from_slice(&[c[2], c[1], c[0], 255]);
            }
        }
        Some(Rig {
            ui,
            gpu,
            w,
            h,
            bg,
            state: UiState {
                params: Params::identity_gun(),
                source: SourceView {
                    attached: Some(attached()),
                    waiting: None,
                },
                ..UiState::default()
            },
            actions: Vec::new(),
            uploaded: false,
        })
    }

    fn render(&mut self) -> ImageF32 {
        let upload = !std::mem::replace(&mut self.uploaded, true);
        let src = upload.then_some(SourceFrame {
            width: self.w,
            height: self.h,
            bgra: &self.bg,
        });
        let (view, size) = (View::full(self.w, self.h), (self.w, self.h));
        let params = Params::identity_gun();
        self.gpu
            .render_with_ui(
                src,
                view,
                &Settings::identity(),
                &params,
                size,
                TargetFormat::Bgra8Unorm,
                &mut self.ui,
            )
            .expect("render")
            .output
    }

    fn frame(&mut self, msgs: &[(u32, usize, isize)]) -> ImageF32 {
        for &(m, w, l) in msgs {
            self.ui.handle_win32_message(m, w, l);
        }
        let out = self.ui.frame(&self.state);
        if let Some(a) = out.source {
            self.actions.push(a);
        }
        self.render()
    }

    fn item(&self, key: &str) -> [[f32; 2]; 2] {
        self.ui
            .item_rect(key)
            .unwrap_or_else(|| panic!("no menu item {key} in the last frame"))
    }

    fn click_at(&mut self, p: [f32; 2]) {
        let l = mouse_lparam(p[0] as i16, p[1] as i16);
        self.frame(&[(WM_MOUSEMOVE, 0, l)]);
        self.frame(&[(WM_LBUTTONDOWN, 1, l)]);
        self.frame(&[(WM_LBUTTONUP, 0, l)]);
        // What the release frame did is handed over at the next `Ui::frame`.
        self.frame(&[]);
    }

    fn click(&mut self, key: &str) {
        let r = self.item(key);
        self.click_at(MenuRects::at(r, 0.5, 0.5));
    }

    /// Clicks a check box (on its box, at the left of the item).
    fn tick(&mut self, key: &str) {
        let r = self.item(key);
        self.click_at(MenuRects::at(r, 0.03, 0.5));
    }

    /// The menu open with the Source section expanded.
    fn open_source(&mut self) {
        self.ui.request_toggle();
        for _ in 0..3 {
            self.frame(&[]);
        }
        self.click("header:Source");
        self.item("source:detect_bars");
    }

    fn finish(self) {
        let counter = self.gpu.validation_counter();
        drop(self.ui);
        drop(self.gpu);
        common::assert_no_validation_messages(&counter);
    }
}

/// Detect black bars issues the request; Reset crop and Undo only act when there is something to reset or undo;
/// Detect at attach goes back as a preference; the app's handling of the outcome stores a manual crop.
#[test]
fn black_bar_buttons_issue_requests() {
    let _g = lock();
    let Some(mut rig) = Rig::new("black_bar_buttons_issue_requests", 1280, 900) else {
        return;
    };
    rig.open_source();
    rig.click("source:detect_bars");
    assert_eq!(rig.actions, [SourceAction::DetectBars]);
    // No manual crop and nothing to undo: both buttons are disabled.
    rig.click("source:reset_crop");
    rig.click("source:undo_crop");
    assert_eq!(rig.actions.len(), 1, "{:?}", rig.actions);
    // While a detection runs the button is disabled too.
    rig.state.source.attached.as_mut().unwrap().bars.running = true;
    rig.frame(&[]);
    rig.click("source:detect_bars");
    assert_eq!(rig.actions.len(), 1);
    // The app found bars (32 px left and right): the crop is manual, the bars and the message are shown.
    let found = Found {
        bars: Bars {
            left: 32,
            top: 0,
            right: 32,
            bottom: 0,
        },
        picture: Rect {
            x: 32,
            y: 0,
            w: 768,
            h: 672,
        },
        limited: false,
        snapped_to: (3, 3),
    };
    {
        let a = rig.state.source.attached.as_mut().unwrap();
        a.bars = BarsView {
            running: false,
            found: Some(found),
            message: Some(ultimatecrt::bars::Outcome::Found(found).message()),
            can_undo: true,
        };
        a.crop_manual = true;
    }
    rig.frame(&[]);
    rig.item("source:bars");
    rig.item("source:bars_message");
    rig.click("source:reset_crop");
    assert_eq!(rig.actions.last(), Some(&SourceAction::ResetCrop));
    rig.click("source:undo_crop");
    assert_eq!(rig.actions.last(), Some(&SourceAction::UndoCrop));
    // What the app does with the detection: a manual crop in the source profile of the program.
    let (mut params, mut profile, mut cli) = (Params::default(), SourceProfile::default(), None);
    ultimatecrt::bars::set_manual_crop(&mut params, &mut profile, &mut cli, found.picture);
    assert_eq!(params.source_crop_mode, ultimatecrt::params::SOURCE_CROP_MODE_MANUAL);
    assert_eq!(profile.crop, Some([32, 0, 800, 672]));
    // Detect at attach: off by default, a preference of the settings.
    assert!(!rig.ui.source_prefs().detect_bars_at_attach);
    rig.tick("source:detect_at_attach");
    assert!(rig.ui.source_prefs().detect_bars_at_attach);
    // The thresholds are Advanced: hidden until the Advanced box is ticked.
    assert!(rig.ui.item_rect("source:bars_tolerance").is_none());
    rig.tick("advanced");
    rig.item("source:bars_tolerance");
    rig.item("source:bars_min");
    rig.item("source:bars_max_crop");
    rig.finish();
}

/// The multiplier combo: Auto (with the detected value), None and x1 to x10; X and Y separately in Advanced.
/// Each choice goes back as a request that the app applies to the parameters and the source profile.
#[test]
fn multiplier_is_chosen_in_the_source_section() {
    let _g = lock();
    let Some(mut rig) = Rig::new("multiplier_is_chosen_in_the_source_section", 1280, 900) else {
        return;
    };
    rig.open_source();
    rig.item("source:multiplier_note");
    // Open the list, pick x2 (entries: Auto, None, x1, x2, ...).
    rig.click("source:multiplier");
    rig.item("source:multiplier:0");
    rig.click("source:multiplier:3");
    assert_eq!(
        rig.actions.last(),
        Some(&SourceAction::SetMultiplier(Multiplier::Manual(2, 2)))
    );
    let apply = |m: Multiplier| {
        let (mut p, mut profile) = (Params::default(), SourceProfile::default());
        m.apply(&mut p, &mut profile);
        (p.source_divisor_mode, profile)
    };
    let (mode, profile) = apply(Multiplier::Manual(2, 2));
    assert_eq!(
        (mode, profile.divisor_pair()),
        (SOURCE_DIVISOR_MODE_MANUAL, Some((2, 2)))
    );
    // The app shows the forced value; it disagrees with the detection (x3): the hint is drawn.
    rig.state.source.attached.as_mut().unwrap().multiplier = Multiplier::Manual(2, 2);
    rig.frame(&[]);
    assert_eq!(
        Multiplier::Manual(2, 2).disagreement(Some((3, 3))).as_deref(),
        Some("detected x3, forced x2")
    );
    rig.item("source:multiplier_note");
    // None.
    rig.click("source:multiplier");
    rig.click("source:multiplier:1");
    assert_eq!(rig.actions.last(), Some(&SourceAction::SetMultiplier(Multiplier::None)));
    assert_eq!(apply(Multiplier::None).0, SOURCE_DIVISOR_MODE_NONE);
    // Advanced: X and Y separately; Y x4 keeps X at the forced x2.
    assert!(rig.ui.item_rect("source:split").is_none());
    rig.tick("advanced");
    rig.tick("source:split");
    rig.click("source:multiplier_y");
    rig.click("source:multiplier_y:3");
    assert_eq!(
        rig.actions.last(),
        Some(&SourceAction::SetMultiplier(Multiplier::Manual(2, 4)))
    );
    let (_, profile) = apply(Multiplier::Manual(2, 4));
    assert_eq!((profile.divisor, profile.divisor_v), (Some(2), Some(4)));
    // Back to Auto.
    rig.click("source:multiplier");
    rig.click("source:multiplier:0");
    assert_eq!(rig.actions.last(), Some(&SourceAction::SetMultiplier(Multiplier::Auto)));
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

/// Preview images of the Source section: after a detection with a forced multiplier that disagrees (basic), and
/// with Advanced on (thresholds, X and Y separately).
#[test]
fn source_controls_preview_images() {
    let _g = lock();
    for (name, advanced) in [("menu-source-bars", false), ("menu-source-bars-advanced", true)] {
        let Some(mut rig) = Rig::new(name, 1280, 900) else {
            return;
        };
        let found = Found {
            bars: Bars {
                left: 32,
                top: 0,
                right: 32,
                bottom: 0,
            },
            picture: Rect {
                x: 32,
                y: 0,
                w: 768,
                h: 672,
            },
            limited: false,
            snapped_to: (3, 3),
        };
        {
            let a = rig.state.source.attached.as_mut().unwrap();
            a.crop = "crop 32, 0, 768 \u{00D7} 672".into();
            a.native = (384, 336);
            a.scale = "x2 (manual, detected x3)".into();
            a.multiplier = Multiplier::Manual(2, 2);
            a.crop_manual = true;
            a.profile = true;
            a.bars = BarsView {
                running: false,
                found: Some(found),
                message: Some(ultimatecrt::bars::Outcome::Found(found).message()),
                can_undo: true,
            };
        }
        rig.open_source();
        if advanced {
            rig.tick("advanced");
            rig.tick("source:split");
        }
        rig.frame(&[(WM_MOUSEMOVE, 0, mouse_lparam(i16::MAX, i16::MAX))]);
        rig.frame(&[]);
        let img = rig.frame(&[]);
        let window = rig.ui.menu_rects().window;
        assert!(window[1][1] <= 900.0, "{name}: menu {window:?}");
        let path = write_png(&img, name);
        eprintln!("{name}: preview {}", path.display());
        rig.finish();
    }
}

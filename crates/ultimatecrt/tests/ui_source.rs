//! Headless GPU tests of the menu's Source section controls (owner requirements 2026-11-20): the picture area
//! (Auto, Manual, Whole client area; Detect now, Back to Auto, Reset, Undo; the four manual limits with their
//! nudges, drag and snapping; the resize warning; the detection thresholds in Advanced) and the source scaling
//! multiplier (Auto, None, x1 to x10, X and Y separately). The menu is drawn by the UI layer into an offscreen
//! `B8G8R8A8_UNORM` target and driven by raw Win32 messages through `Ui::handle_win32_message`; what the app
//! would do with the requests is checked with the same pure functions it calls (`bars`, `scaling::Multiplier`).
//! No window is created or captured and no focus changes (a headless `Ui` has no output window).
//!
//! Dear ImGui allows one context per process: every test holds [`IMGUI`] while its `Ui` lives.

mod common;

use std::sync::{Mutex, MutexGuard};

use ultimatecrt::bars::{edit_limit, AreaMode, Bars, Found, Side};
use ultimatecrt::gpu::{Headless, ImageF32, Settings, SourceFrame, StageReadback, TargetFormat};
use ultimatecrt::params::{Params, SOURCE_CROP_MODE_MANUAL, SOURCE_DIVISOR_MODE_MANUAL, SOURCE_DIVISOR_MODE_NONE};
use ultimatecrt::scaling::{Multiplier, Rect, View};
use ultimatecrt::settings::SourceProfile;
use ultimatecrt::ui::input::{mouse_lparam, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE};
use ultimatecrt::ui::menu::MenuRects;
use ultimatecrt::ui::{AreaView, AttachedView, BarsView, SourceAction, SourceView, Ui, UiConfig, UiState};

static IMGUI: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    IMGUI.lock().unwrap_or_else(|e| e.into_inner())
}

/// The owner's geometry: a 256 x 224 game at x3 (768 x 672) centred in a 914 x 754 client area, the picture area
/// in Auto with the detected limits, as the Source section would describe it.
fn attached() -> AttachedView {
    AttachedView {
        exe: Some("pixel-demo.exe".into()),
        class: "pixel_demo".into(),
        title: "pixel-demo | scene 256x224 integer x3".into(),
        frame: (914, 754),
        native: (256, 224),
        scale: "x3 detected".into(),
        crop: "crop 73, 41, 768 \u{00D7} 672".into(),
        profile: false,
        multiplier: Multiplier::Auto,
        detected: Some((3, 3)),
        crop_manual: true,
        bars: BarsView::default(),
        area: AreaView {
            mode: AreaMode::Auto,
            whole_window: false,
            limits: LIMITS,
            client: (914, 754),
            divisor: (3, 3),
            set_for: None,
        },
    }
}

const LIMITS: Bars = Bars {
    left: 73,
    top: 41,
    right: 73,
    bottom: 41,
};

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
        Rig::with_menu_width(test, w, h, 520.0)
    }

    fn with_menu_width(test: &str, w: u32, h: u32, menu_w: f32) -> Option<Rig> {
        let mut gpu = common::gpu_or_skip(test)?;
        gpu.enable_only(&[]).expect("chain slots off");
        gpu.set_read_stages(StageReadback::None);
        let mut config = UiConfig::headless();
        // A tall menu: the whole Source section fits without scrolling.
        config.layout.menu_h = h as f32 - 40.0;
        config.layout.menu_w = menu_w;
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
        self.item("source:detect_now");
    }

    fn area(&mut self) -> &mut AreaView {
        &mut self.state.source.attached.as_mut().unwrap().area
    }

    /// What the app does with a picture-area request, on the state the menu shows next.
    fn apply(&mut self, a: SourceAction) {
        let area = self.area();
        match a {
            SourceAction::SetLimits(b) => {
                area.limits = b;
                area.mode = AreaMode::Manual;
            }
            SourceAction::SetAreaMode(m) => {
                area.mode = m;
                if m == AreaMode::Client {
                    area.limits = Bars::default();
                }
            }
            _ => {}
        }
        self.frame(&[]);
    }

    /// Drags the mouse from `a` to `b` (fractions of the item `key`), button held.
    fn drag(&mut self, key: &str, a: f32, b: f32) {
        let r = self.item(key);
        let p = |t: f32| {
            let q = MenuRects::at(r, t, 0.5);
            mouse_lparam(q[0] as i16, q[1] as i16)
        };
        self.frame(&[(WM_MOUSEMOVE, 0, p(a))]);
        self.frame(&[(WM_LBUTTONDOWN, 1, p(a))]);
        for i in 1..=4 {
            let t = a + (b - a) * i as f32 / 4.0;
            self.frame(&[(WM_MOUSEMOVE, 1, p(t))]);
        }
        self.frame(&[(WM_LBUTTONUP, 0, p(b))]);
        self.frame(&[]);
    }

    fn finish(self) {
        let counter = self.gpu.validation_counter();
        drop(self.ui);
        drop(self.gpu);
        common::assert_no_validation_messages(&counter);
    }
}

/// The picture-area mode and buttons: the combo (Auto, Manual, Whole client area), Detect now (disabled while a
/// detection runs), Back to Auto (only off Auto), Reset (disabled for the whole client area) and Undo (only with
/// something to undo); the thresholds stay in Advanced.
#[test]
fn picture_area_mode_and_buttons_issue_requests() {
    let _g = lock();
    let Some(mut rig) = Rig::new("picture_area_mode_and_buttons_issue_requests", 1280, 1000) else {
        return;
    };
    rig.open_source();
    rig.item("source:picture");
    rig.click("source:detect_now");
    assert_eq!(rig.actions, [SourceAction::DetectNow]);
    // Auto: no Back to Auto; nothing to undo.
    assert!(rig.ui.item_rect("source:back_to_auto").is_none());
    rig.click("source:undo_crop");
    assert_eq!(rig.actions.len(), 1, "{:?}", rig.actions);
    // While a detection runs, Detect now is disabled.
    rig.state.source.attached.as_mut().unwrap().bars.running = true;
    rig.frame(&[]);
    rig.click("source:detect_now");
    assert_eq!(rig.actions.len(), 1);
    rig.state.source.attached.as_mut().unwrap().bars.running = false;
    // The combo: Manual (keeps the limits in force), then Whole client area.
    rig.click("source:area");
    rig.item("source:area:0");
    rig.click("source:area:1");
    assert_eq!(rig.actions.last(), Some(&SourceAction::SetAreaMode(AreaMode::Manual)));
    rig.apply(SourceAction::SetAreaMode(AreaMode::Manual));
    rig.click("source:back_to_auto");
    assert_eq!(rig.actions.last(), Some(&SourceAction::SetAreaMode(AreaMode::Auto)));
    rig.click("source:reset_crop");
    assert_eq!(rig.actions.last(), Some(&SourceAction::ResetCrop));
    rig.click("source:area");
    rig.click("source:area:2");
    assert_eq!(rig.actions.last(), Some(&SourceAction::SetAreaMode(AreaMode::Client)));
    rig.apply(SourceAction::SetAreaMode(AreaMode::Client));
    // Whole client area: Reset has nothing to reset.
    let n = rig.actions.len();
    rig.click("source:reset_crop");
    assert_eq!(rig.actions.len(), n);
    // Undo, once there is something to undo.
    rig.state.source.attached.as_mut().unwrap().bars.can_undo = true;
    rig.frame(&[]);
    rig.click("source:undo_crop");
    assert_eq!(rig.actions.last(), Some(&SourceAction::UndoCrop));
    // A detection's outcome is shown.
    let found = Found {
        bars: LIMITS,
        picture: Rect {
            x: 73,
            y: 41,
            w: 768,
            h: 672,
        },
        limited: None,
        snapped_to: (3, 3),
    };
    rig.state.source.attached.as_mut().unwrap().bars.message = Some(ultimatecrt::bars::Outcome::Found(found).message());
    rig.frame(&[]);
    rig.item("source:bars_message");
    // The thresholds are Advanced: hidden until the Advanced box is ticked.
    assert!(rig.ui.item_rect("source:bars_tolerance").is_none());
    rig.tick("advanced");
    rig.item("source:bars_tolerance");
    rig.item("source:bars_min");
    rig.item("source:bars_max_crop");
    // The whole window captured (crop mode none): the picture area does not apply, no limits shown.
    rig.area().whole_window = true;
    rig.frame(&[]);
    rig.frame(&[]);
    assert!(rig.ui.item_rect("source:limit_left").is_none());
    rig.finish();
}

/// The four manual limits: the detected values are shown in Auto; a nudge switches to Manual from them; with a
/// multiplier known the nudges and drags move in whole source pixels ("Snap to source pixels", on by default), off
/// they move by 1 px; the limits never cross; the app stores the result as a manual crop; a resize of the window
/// since the limits were set shows a warning.
#[test]
fn manual_limits_nudge_drag_and_snap() {
    let _g = lock();
    let Some(mut rig) = Rig::new("manual_limits_nudge_drag_and_snap", 1280, 1000) else {
        return;
    };
    rig.open_source();
    for key in [
        "source:limit_left",
        "source:limit_top",
        "source:limit_right",
        "source:limit_bottom",
    ] {
        rig.item(key);
        rig.item(&format!("{key}:minus"));
        rig.item(&format!("{key}:plus"));
    }
    let client = (914, 754);
    // Auto, x3 known: + on Left moves one source pixel (3 px) and switches to Manual.
    rig.click("source:limit_left:plus");
    let want = Bars { left: 76, ..LIMITS };
    assert_eq!(rig.actions.last(), Some(&SourceAction::SetLimits(want)));
    assert_eq!(want.picture(client).unwrap().w % 3, 0);
    rig.apply(SourceAction::SetLimits(want));
    // - on Bottom.
    rig.click("source:limit_bottom:minus");
    let want = Bars { bottom: 38, ..want };
    assert_eq!(rig.actions.last(), Some(&SourceAction::SetLimits(want)));
    rig.apply(SourceAction::SetLimits(want));
    // Snap off: 1 px steps.
    rig.tick("source:snap");
    rig.click("source:limit_top:plus");
    let want = Bars { top: 42, ..want };
    assert_eq!(rig.actions.last(), Some(&SourceAction::SetLimits(want)));
    rig.apply(SourceAction::SetLimits(want));
    // A drag on Right (snap on again): a whole number of source pixels across.
    rig.tick("source:snap");
    let n = rig.actions.len();
    rig.drag("source:limit_right", 0.1, 0.3);
    assert!(rig.actions.len() > n, "the drag asked for new limits");
    let Some(SourceAction::SetLimits(dragged)) = rig.actions.last().copied() else {
        panic!("{:?}", rig.actions.last())
    };
    assert!((150..400).contains(&dragged.right), "{dragged:?}");
    assert_eq!(dragged.picture(client).unwrap().w % 3, 0, "{dragged:?}");
    assert_eq!(
        dragged,
        edit_limit(want, Side::Right, i64::from(dragged.right), client, (3, 3))
    );
    rig.apply(SourceAction::SetLimits(dragged));
    // Pushing Left far right never crosses Right: the picture keeps at least 8 px (9 at x3).
    let crossed = edit_limit(dragged, Side::Left, 5000, client, (3, 3));
    let p = crossed.picture(client).unwrap();
    assert!(p.w >= 8 && p.w.is_multiple_of(3), "{p:?}");
    // What the app stores: a manual crop for this program, with the client size it was set for.
    let (mut params, mut profile, mut cli) = (Params::default(), SourceProfile::default(), None);
    ultimatecrt::bars::set_manual_crop(
        &mut params,
        &mut profile,
        &mut cli,
        dragged.picture(client).unwrap(),
        client,
    );
    assert_eq!(params.source_crop_mode, SOURCE_CROP_MODE_MANUAL);
    assert_eq!(profile.crop_rect(), dragged.picture(client));
    assert_eq!(profile.crop_client, Some([914, 754]));
    // The window was resized since: the warning offers Detect now.
    assert!(rig.ui.item_rect("source:area_warning").is_none());
    {
        let a = rig.area();
        a.set_for = Some((914, 754));
        a.client = (1000, 800);
    }
    rig.frame(&[]);
    rig.item("source:area_warning");
    // No multiplier known: the snap box is disabled and steps are 1 px.
    {
        let a = rig.area();
        a.set_for = None;
        a.client = client;
        a.divisor = (1, 1);
        a.limits = LIMITS;
    }
    rig.frame(&[]);
    rig.click("source:limit_right:plus");
    assert_eq!(
        rig.actions.last(),
        Some(&SourceAction::SetLimits(Bars { right: 74, ..LIMITS }))
    );
    rig.finish();
}

/// The multiplier combo: Auto (with the detected value), None and x1 to x10; X and Y separately in Advanced.
/// Each choice goes back as a request that the app applies to the parameters and the source profile.
#[test]
fn multiplier_is_chosen_in_the_source_section() {
    let _g = lock();
    // 1080 px high: the Source section's rows (capture path included) and the advanced X and Y lists fit above the
    // pinned footer.
    let Some(mut rig) = Rig::new("multiplier_is_chosen_in_the_source_section", 1280, 1080) else {
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

/// Preview images of the Source section: Auto with the detected picture area of the owner's geometry (basic),
/// Manual limits set for another window size with a forced multiplier and Advanced on (thresholds, X and Y), and
/// Manual at the default menu width (460 px: the button row wraps instead of clipping).
#[test]
fn source_controls_preview_images() {
    let _g = lock();
    for (name, advanced, menu_w) in [
        ("menu-source-area-auto", false, 520.0),
        ("menu-source-area-manual", true, 520.0),
        ("menu-source-area-narrow", true, 460.0),
    ] {
        let Some(mut rig) = Rig::with_menu_width(name, 1280, 1000, menu_w) else {
            return;
        };
        let found = Found {
            bars: LIMITS,
            picture: Rect {
                x: 73,
                y: 41,
                w: 768,
                h: 672,
            },
            limited: None,
            snapped_to: (3, 3),
        };
        {
            let a = rig.state.source.attached.as_mut().unwrap();
            a.profile = true;
            a.bars = BarsView {
                running: false,
                found: Some(found),
                message: Some(ultimatecrt::bars::Outcome::Found(found).message()),
                can_undo: advanced,
            };
            if advanced {
                a.native = (384, 336);
                a.scale = "x2 (manual, detected x3)".into();
                a.multiplier = Multiplier::Manual(2, 2);
                a.area.mode = AreaMode::Manual;
                a.area.divisor = (2, 2);
                a.area.limits = Bars {
                    left: 76,
                    top: 41,
                    right: 70,
                    bottom: 38,
                };
                a.area.set_for = Some((832, 720));
                a.bars.message = None;
            }
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
        assert!(window[1][1] <= 1000.0, "{name}: menu {window:?}");
        let path = write_png(&img, name);
        eprintln!("{name}: preview {}", path.display());
        rig.finish();
    }
}

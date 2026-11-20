//! Headless GPU tests of the menu features added on 2026-11-20 (owner request "finish the menu"): the hotkey
//! rebinding (Change, a chord typed as Win32 key messages or arriving as a registered hotkey, Esc, Backspace,
//! conflicts, a chord another program owns, the menu's safety chord, Reset all), the preset files (rename,
//! duplicate, export, delete with its confirmation, import from the drop folder with a conflict, the dialog
//! requests and results), the saved per-game profiles, the interface size and the resets, the About section and
//! the first-run hint of the picker. The menu is drawn into an offscreen `B8G8R8A8_UNORM` target and driven by raw
//! Win32 messages (`Ui::handle_win32_message`); what the app does with a request is done here with the same pure
//! functions it calls (`hotkeys::rebind` on a fake registrar, `preset_files`, `profiles`). No window, no dialog,
//! no clipboard and no global hotkey is touched (a headless `Ui` has no output window). Each test writes a
//! preview to `target/ui-preview/`.
//!
//! Dear ImGui allows one context per process: every test holds [`IMGUI`] while its `Ui` lives.

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use ultimatecrt::gpu::{Headless, ImageF32, Settings, SourceFrame, StageReadback, TargetFormat};
use ultimatecrt::hotkeys::{
    plan_registration, rebind, register_all, reset_all, update_menu_fallback, Action, Bindings, Chord, Registrar,
    Registration, ERROR_HOTKEY_ALREADY_REGISTERED,
};
use ultimatecrt::params::Params;
use ultimatecrt::presets::{Library, Preset};
use ultimatecrt::profiles::{ProfileAction, ProfileRow};
use ultimatecrt::scaling::{Multiplier, View};
use ultimatecrt::settings::UiScale;
use ultimatecrt::ui::files_panel::{DialogRequest, DialogResult};
use ultimatecrt::ui::hotkeys_panel::HotkeyEdit;
use ultimatecrt::ui::input::{
    key_lparam, mouse_lparam, WM_CHAR, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_SYSKEYDOWN,
    WM_SYSKEYUP,
};
use ultimatecrt::ui::menu::MenuRects;
use ultimatecrt::ui::system_panels::ResetKind;
use ultimatecrt::ui::{AttachedView, SourceView, Ui, UiConfig, UiOutput, UiState};

static IMGUI: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    IMGUI.lock().unwrap_or_else(|e| e.into_inner())
}

fn chord(t: &str) -> Chord {
    t.parse().unwrap()
}

/// The system's hotkey table, as the tests' stand-in for `RegisterHotKey`: chords other programs own refuse.
#[derive(Default)]
struct FakeSystem {
    taken: Vec<Chord>,
    registered: HashMap<i32, Chord>,
}

impl Registrar for FakeSystem {
    fn register(&mut self, id: i32, c: &Chord) -> Result<(), u32> {
        if self.taken.contains(c) || self.registered.values().any(|r| r == c) {
            return Err(ERROR_HOTKEY_ALREADY_REGISTERED);
        }
        self.registered.insert(id, *c);
        Ok(())
    }

    fn unregister(&mut self, id: i32) {
        self.registered.remove(&id);
    }
}

fn registered(system: &mut FakeSystem) -> Registration {
    let mut reg = register_all(system, &plan_registration(&Bindings::default()));
    update_menu_fallback(system, &mut reg);
    reg
}

/// A temporary settings folder with the user preset folder inside.
fn temp_settings(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ucrt-uiextras-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("presets")).unwrap();
    d
}

struct Rig {
    ui: Ui,
    gpu: Headless,
    w: u32,
    h: u32,
    bg: Vec<u8>,
    state: UiState,
    out: Vec<UiOutput>,
    uploaded: bool,
}

impl Rig {
    fn new(test: &str, w: u32, h: u32, config: UiConfig) -> Option<Rig> {
        let mut gpu = common::gpu_or_skip(test)?;
        gpu.enable_only(&[]).expect("chain slots off");
        gpu.set_read_stages(StageReadback::None);
        let mut ui =
            Ui::new(gpu.context(), Headless::vk_format(TargetFormat::Bgra8Unorm), config).expect("create the UI");
        ui.set_frame_time(Some(1.0 / 60.0));
        // Colour bars behind the menu, as a captured picture.
        let mut bg = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let c: [u8; 3] = match (x * 6 / w, y * 2 / h) {
                    (0, _) => [190, 190, 190],
                    (1, _) => [190, 190, 0],
                    (2, _) => [0, 190, 190],
                    (3, _) => [0, 190, 0],
                    (4, _) => [190, 0, 190],
                    _ => [190, 0, 0],
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
                ..UiState::default()
            },
            out: Vec::new(),
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
        if let Some(lib) = &out.library {
            self.ui.set_library(lib.clone());
        }
        if out != UiOutput::default() {
            self.out.push(out);
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
        self.frame(&[]);
    }

    fn click(&mut self, key: &str) {
        let r = self.item(key);
        self.click_at(MenuRects::at(r, 0.5, 0.5));
    }

    /// Clicks a check box on its box.
    fn tick(&mut self, key: &str) {
        let r = self.item(key);
        self.click_at(MenuRects::at(r, 0.03, 0.5));
    }

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.frame(&[(WM_CHAR, c as usize, 1)]);
        }
        self.frame(&[]);
    }

    /// A key down and up, as `WM_KEYDOWN` or (with Alt held) `WM_SYSKEYDOWN`.
    fn key(&mut self, vk: usize, sys: bool) {
        let (down, up) = if sys {
            (WM_SYSKEYDOWN, WM_SYSKEYUP)
        } else {
            (WM_KEYDOWN, WM_KEYUP)
        };
        self.frame(&[(down, vk, key_lparam(0, false, false))]);
        self.frame(&[(up, vk, key_lparam(0, false, true))]);
    }

    /// Ctrl and Alt held, `vk` pressed, everything released: the messages Windows sends for Ctrl+Alt+key.
    fn ctrl_alt(&mut self, vk: usize) {
        self.frame(&[
            (WM_KEYDOWN, 0x11, key_lparam(0x1D, false, false)),
            (WM_SYSKEYDOWN, 0x12, key_lparam(0x38, false, false)),
            (WM_SYSKEYDOWN, vk, key_lparam(0, false, false)),
        ]);
        self.frame(&[
            (WM_SYSKEYUP, vk, key_lparam(0, false, true)),
            (WM_SYSKEYUP, 0x12, key_lparam(0x38, false, true)),
            (WM_KEYUP, 0x11, key_lparam(0x1D, false, true)),
        ]);
    }

    fn open_menu(&mut self) {
        self.ui.request_toggle();
        for _ in 0..3 {
            self.frame(&[]);
        }
        // The output panel (open by default) is long: closed, so the sections below are on screen.
        if self.ui.item_rect("widget:exposure").is_some() {
            self.click("header:Output and headroom");
        }
    }

    /// The outputs of the frames since the last call, merged (the last request of each kind).
    fn take(&mut self) -> UiOutput {
        let mut m = UiOutput::default();
        for o in std::mem::take(&mut self.out) {
            m.hotkey = o.hotkey.or(m.hotkey);
            m.profile = o.profile.or(m.profile);
            m.reset = o.reset.or(m.reset);
            m.dialog = o.dialog.or(m.dialog);
            m.library = o.library.or(m.library);
            m.preset = o.preset.or(m.preset);
            m.params = o.params.or(m.params);
        }
        m
    }

    fn preview(&mut self, name: &str) {
        self.frame(&[(WM_MOUSEMOVE, 0, mouse_lparam(i16::MAX, i16::MAX))]);
        self.frame(&[]);
        let img = self.frame(&[]);
        let path = write_png(&img, name);
        eprintln!("{name}: preview {}", path.display());
    }

    fn finish(self) {
        let counter = self.gpu.validation_counter();
        drop(self.ui);
        drop(self.gpu);
        common::assert_no_validation_messages(&counter);
    }
}

/// A menu tall enough for every section opened in the tests.
fn tall(config: UiConfig, h: f32) -> UiConfig {
    let mut c = config;
    c.layout.menu_h = h - 40.0;
    c.layout.menu_w = 520.0;
    c.layout.menu_y = 4.0;
    c
}

/// Writes `img` (RGBA, display codes) to `target/ui-preview/<name>.png` in the workspace.
fn write_png(img: &ImageF32, name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-preview");
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

/// Hotkey rebinding: Change, then the keys as Win32 messages; Esc cancels without closing the menu; Backspace
/// gives the default; a chord of our own actions is refused with its reason, a chord another program owns is
/// refused by the system and the old one kept; a registered hotkey pressed while waiting is taken as the chord
/// (Quit always passes); the menu key moved keeps F10 as its safety chord and says so; Quit is rebindable (review
/// D-01); a Ctrl+Alt+letter chord that types a character with AltGr is accepted with a warning; Reset all.
#[test]
fn hotkeys_are_rebound_from_the_menu() {
    let _g = lock();
    let mut system = FakeSystem {
        taken: vec![chord("Ctrl+Alt+P")],
        ..FakeSystem::default()
    };
    let mut reg = registered(&mut system);
    // A German layout as far as the test goes: AltGr+M types 'µ'.
    fn german(vk: u16, shift: bool) -> Option<String> {
        (vk == u16::from(b'M') && !shift).then(|| "'\u{B5}' (keyboard layout de-DE)".to_string())
    }
    let config = UiConfig {
        registration: Some(reg.clone()),
        layout_probe: Some(german),
        ..tall(UiConfig::headless(), 1400.0)
    };
    let Some(mut rig) = Rig::new("hotkeys_are_rebound_from_the_menu", 1280, 1400, config) else {
        return;
    };
    rig.open_menu();
    rig.click("header:GPU and system");
    for a in Action::ALL {
        rig.item(&format!("hotkey:{}", a.id()));
        rig.item(&format!("hotkey:{}:change", a.id()));
    }
    // The app's part: register, then hand the registration back.
    let apply = |rig: &mut Rig, reg: &mut Registration, system: &mut FakeSystem| -> Option<Result<String, String>> {
        let edit = rig.take().hotkey?;
        let r = match edit {
            HotkeyEdit::Set(a, c) => rebind(system, reg, a, c),
            HotkeyEdit::ResetAll => {
                let p = reset_all(system, reg);
                if p.is_empty() {
                    Ok("defaults".into())
                } else {
                    Err(p.join(" "))
                }
            }
        };
        rig.ui
            .set_hotkeys(reg, Some((r.is_ok(), r.clone().unwrap_or_else(|e| e))));
        rig.frame(&[]);
        Some(r)
    };

    // Overlay -> Ctrl+Alt+M by key messages.
    rig.click("hotkey:toggle_overlay:change");
    assert_eq!(rig.ui.capturing_hotkey(), Some(Action::ToggleOverlay));
    rig.item("hotkey:toggle_overlay:cancel");
    rig.ctrl_alt(b'M' as usize);
    assert_eq!(rig.ui.capturing_hotkey(), None);
    assert!(apply(&mut rig, &mut reg, &mut system).unwrap().is_ok());
    assert_eq!(reg.bound(Action::ToggleOverlay), Some(chord("Ctrl+Alt+M")));
    assert_eq!(reg.to_settings()["toggle_overlay"], "Ctrl+Alt+M");
    // Ctrl+Alt+M is AltGr+M, which types 'µ' on the (fake) German layout: accepted, with the warning.
    let w = rig.ui.hotkey_warning().expect("an AltGr warning").to_string();
    assert!(w.contains("AltGr+M") && w.contains("de-DE"), "{w}");
    rig.item("hotkeys:warning");

    // Esc cancels the wait and the menu stays open.
    rig.click("hotkey:toggle_bypass:change");
    rig.key(0x1B, false);
    assert!(rig.ui.is_open(), "Esc while waiting does not close the menu");
    assert_eq!(rig.ui.capturing_hotkey(), None);
    assert!(rig.take().hotkey.is_none());

    // A bare letter is refused and the wait goes on; F11 is ours (fullscreen): refused with the reason.
    rig.click("hotkey:toggle_bypass:change");
    rig.key(b'Q' as usize, false);
    assert_eq!(rig.ui.capturing_hotkey(), Some(Action::ToggleBypass));
    assert!(rig.ui.hotkey_message().unwrap().1.contains("needs Ctrl, Alt or Win"));
    rig.key(0x7A, false); // F11
    assert!(rig.take().hotkey.is_none());
    let (ok, m) = rig.ui.hotkey_message().unwrap().clone();
    assert!(!ok && m.contains("already used for fullscreen"), "{m}");

    // A chord another program owns: the system refuses, the old chord is kept and the reason shown.
    rig.click("hotkey:toggle_bypass:change");
    rig.ctrl_alt(b'P' as usize);
    let r = apply(&mut rig, &mut reg, &mut system).unwrap();
    assert!(r.unwrap_err().contains("Ctrl+Alt+F7 is kept"));
    assert_eq!(reg.bound(Action::ToggleBypass), Some(chord("Ctrl+Alt+F7")));
    rig.item("hotkeys:message");
    assert!(
        rig.ui.hotkey_warning().is_none(),
        "Ctrl+Alt+P types nothing with AltGr here"
    );

    // Backspace: back to the default (the overlay was moved above).
    rig.click("hotkey:toggle_overlay:change");
    rig.key(0x08, false);
    assert!(apply(&mut rig, &mut reg, &mut system).unwrap().is_ok());
    assert_eq!(reg.bound(Action::ToggleOverlay), Some(chord("Ctrl+Alt+F9")));

    // Quit can be rebound like the others (review D-01).
    rig.click("hotkey:quit:change");
    rig.ctrl_alt(0x24); // Home
    assert!(apply(&mut rig, &mut reg, &mut system).unwrap().is_ok());
    assert_eq!(reg.bound(Action::Quit), Some(chord("Ctrl+Alt+Home")));
    assert_eq!(reg.to_settings()["quit"], "Ctrl+Alt+Home");

    // A registered hotkey arriving while waiting is the chord (our own keys never come as key messages)...
    rig.click("hotkey:next_preset:change");
    assert!(
        !rig.ui.capture_chord(Action::Quit, chord("Ctrl+Alt+Home")),
        "Quit always runs"
    );
    assert!(rig.ui.capture_chord(Action::ToggleOverlay, chord("Ctrl+Alt+F9")));
    assert!(rig.take().hotkey.is_none(), "the overlay's chord is refused");
    // ... and nothing is taken when nothing waits.
    assert!(!rig.ui.capture_chord(Action::ToggleOverlay, chord("Ctrl+Alt+F9")));

    // The menu key moved: F10 stays as the safety chord, and the menu says so.
    rig.click("hotkey:toggle_menu:change");
    rig.ctrl_alt(0x7B); // F12
    assert!(apply(&mut rig, &mut reg, &mut system).unwrap().is_ok());
    assert_eq!(reg.bound(Action::ToggleMenu), Some(chord("Ctrl+Alt+F12")));
    assert_eq!(reg.menu_fallback, Some(chord("F10")));
    rig.item("hotkeys:fallback");
    rig.preview("menu-hotkeys");

    // Reset all.
    rig.click("hotkeys:reset_all");
    assert!(apply(&mut rig, &mut reg, &mut system).unwrap().is_ok());
    assert_eq!(reg.to_settings(), ultimatecrt::settings::default_hotkeys());
    assert_eq!(reg.menu_fallback, None);
    // Waiting, for the preview.
    rig.click("hotkey:toggle_fullscreen:change");
    rig.preview("menu-hotkeys-waiting");
    rig.finish();
}

/// A user preset file with the id of the first shipped preset (so it is the first entry of the lists, where the
/// combo shows it without scrolling); renaming it brings the shipped one back.
fn user_preset(dir: &Path, name: &str) -> String {
    let id = Library::embedded().list()[0].id.clone();
    let p = Params {
        exposure: 1.4,
        ..Params::default()
    };
    Preset::from_params(&id, name, &p).unwrap().save(dir).unwrap();
    id
}

fn files_config(settings_dir: &Path, h: f32) -> UiConfig {
    let presets = settings_dir.join("presets");
    UiConfig {
        presets: Library::load(Some(&presets)),
        presets_dir: Some(presets),
        settings_dir: Some(settings_dir.to_path_buf()),
        dialogs: true,
        ..tall(UiConfig::headless(), h)
    }
}

/// Preset files: rename (the file moves), duplicate, export to the export folder, delete with its confirmation
/// (never a shipped preset; not the preset in force), import from the drop folder (a conflict asks Replace or a
/// copy), and the optional dialogs as requests whose results the menu applies.
#[test]
fn preset_files_are_managed_from_the_menu() {
    let _g = lock();
    let dir = temp_settings("files");
    let presets = dir.join("presets");
    let id = user_preset(&presets, "My Tube");
    let Some(mut rig) = Rig::new(
        "preset_files_are_managed_from_the_menu",
        1280,
        1400,
        files_config(&dir, 1400.0),
    ) else {
        return;
    };
    rig.open_menu();
    // The Preset section is open by default; its file tools start hidden ("Manage files").
    assert!(rig.ui.item_rect("files:preset").is_none());
    rig.click("files:toggle");
    // Choose the user preset (the first entry).
    rig.click("files:preset");
    rig.click("files:preset:0");
    rig.item("files:rename");
    rig.item("files:delete");
    // Rename.
    rig.click("files:rename_name");
    rig.type_text("Old Faithful");
    rig.click("files:rename");
    assert!(rig.ui.files_message().unwrap().0, "{:?}", rig.ui.files_message());
    assert!(presets.join("old-faithful.toml").exists() && !presets.join(format!("{id}.toml")).exists());
    assert!(rig.ui.library().find("old-faithful").is_some());
    assert!(rig.take().library.is_some(), "the app gets the new library");
    // Duplicate and Export.
    rig.click("files:duplicate");
    assert!(presets.join("old-faithful-copy.toml").exists());
    rig.click("files:export");
    let exported = dir.join("export").join("old-faithful-copy.toml");
    assert!(exported.exists(), "{:?}", rig.ui.files_message());
    // Delete: a confirmation first.
    rig.click("files:delete");
    rig.item("files:delete_confirm");
    rig.preview("menu-preset-files-delete");
    rig.click("files:delete_cancel");
    assert!(presets.join("old-faithful-copy.toml").exists(), "Cancel keeps the file");
    rig.click("files:delete");
    rig.click("files:delete_confirm");
    assert!(!presets.join("old-faithful-copy.toml").exists());
    // A shipped preset offers no Rename or Delete.
    rig.click("files:preset");
    rig.click("files:preset:0");
    assert!(rig.ui.item_rect("files:delete").is_none() && rig.ui.item_rect("files:rename").is_none());
    // Import from the drop folder: the exported copy (its id is free again) and a conflicting one.
    let inbox = dir.join("import");
    std::fs::create_dir_all(&inbox).unwrap();
    std::fs::copy(&exported, inbox.join("a-copy.toml")).unwrap();
    std::fs::copy(presets.join("old-faithful.toml"), inbox.join("b-same-id.toml")).unwrap();
    std::fs::write(inbox.join("c-broken.toml"), "schema = 1\n").unwrap();
    rig.click("files:rescan");
    rig.click("files:import:0");
    assert!(
        rig.ui.library().find("old-faithful-copy").is_some(),
        "{:?}",
        rig.ui.files_message()
    );
    rig.click("files:import:2");
    assert!(!rig.ui.files_message().unwrap().0, "a broken file is refused");
    rig.click("files:import:1");
    rig.item("files:replace");
    rig.preview("menu-preset-files-import");
    rig.click("files:import_copy");
    assert!(
        rig.ui.library().find("old-faithful-2").is_some(),
        "{:?}",
        rig.ui.files_message()
    );
    // The dialogs: requests for the app, then their results.
    rig.click("files:export_dialog");
    let Some(DialogRequest::Export(dialog_id)) = rig.take().dialog else {
        panic!("no export dialog request")
    };
    let chosen = dir.join("elsewhere").join("shared.toml");
    rig.ui.dialog_result(DialogResult::Export {
        id: dialog_id,
        path: Some(chosen.clone()),
    });
    assert!(chosen.exists());
    rig.click("files:import_dialog");
    assert_eq!(rig.take().dialog, Some(DialogRequest::Import));
    rig.ui.dialog_result(DialogResult::Import(None));
    // The file just exported has the id of a user preset: the same confirmation as from the folder.
    rig.ui.dialog_result(DialogResult::Import(Some(chosen)));
    rig.frame(&[]);
    rig.item("files:replace");
    rig.click("files:import_cancel");
    assert!(rig.ui.item_rect("files:replace").is_none());
    rig.finish();
    let _ = std::fs::remove_dir_all(&dir);
}

fn attached() -> AttachedView {
    AttachedView {
        exe: Some("snes9x.exe".into()),
        class: "Snes9X".into(),
        title: "Snes9x".into(),
        frame: (768, 672),
        native: (256, 224),
        scale: "x3 detected".into(),
        crop: "client area".into(),
        profile: true,
        multiplier: Multiplier::Auto,
        detected: Some((3, 3)),
        ..AttachedView::default()
    }
}

fn profile_rows() -> Vec<ProfileRow> {
    vec![
        ProfileRow {
            exe: "snes9x.exe".into(),
            class: "Snes9X".into(),
            summary: "picture auto  \u{00B7}  x3".into(),
            last_used: true,
            current: true,
        },
        ProfileRow {
            exe: "MinaTheHollower.exe".into(),
            class: "UnityWndClass".into(),
            summary: "crop 1280 \u{00D7} 720 at 0, 0  \u{00B7}  multiplier auto  \u{00B7}  +1 more".into(),
            last_used: false,
            current: false,
        },
    ]
}

/// Saved profiles: the list, "Use for current source", Delete and "Reset this game's profile", each a request
/// for the app; the deletions and the reset go through a confirmation.
#[test]
fn saved_profiles_are_listed_and_managed() {
    let _g = lock();
    let Some(mut rig) = Rig::new(
        "saved_profiles_are_listed_and_managed",
        1280,
        1400,
        tall(UiConfig::headless(), 1400.0),
    ) else {
        return;
    };
    rig.state.source = SourceView {
        attached: Some(attached()),
        waiting: None,
    };
    rig.state.profiles = profile_rows();
    rig.open_menu();
    rig.click("header:Source");
    rig.item("profile:0");
    rig.item("profile:1");
    // The current program's own profile cannot be "used" for itself.
    rig.click("profile:0:use");
    assert!(rig.take().profile.is_none());
    rig.click("profile:1:use");
    assert_eq!(
        rig.take().profile,
        Some(ProfileAction::UseForCurrent {
            exe: "MinaTheHollower.exe".into(),
            class: "UnityWndClass".into()
        })
    );
    rig.click("profile:1:delete");
    rig.item("profile:1:confirm");
    rig.preview("menu-profiles");
    rig.click("profile:1:cancel");
    assert!(rig.take().profile.is_none());
    rig.click("profile:1:delete");
    rig.click("profile:1:confirm");
    assert_eq!(
        rig.take().profile,
        Some(ProfileAction::Delete {
            exe: "MinaTheHollower.exe".into(),
            class: "UnityWndClass".into()
        })
    );
    rig.click("profiles:reset_current");
    rig.click("profiles:reset_confirm");
    assert_eq!(rig.take().profile, Some(ProfileAction::ResetCurrent));
    // Without a source: no reset button, "Use" disabled.
    rig.state.source = SourceView::default();
    rig.frame(&[]);
    assert!(rig.ui.item_rect("profiles:reset_current").is_none());
    rig.click("profile:1:use");
    assert!(rig.take().profile.is_none());
    rig.finish();
}

/// Interface: the size applies at once and goes back with the layout; the resets need a confirmation, and
/// "Reset everything" puts the size back to Automatic. Then the About section.
#[test]
fn interface_size_resets_and_about() {
    let _g = lock();
    let dir = temp_settings("about");
    let mut config = files_config(&dir, 1400.0);
    config.vulkan_device = Some("NVIDIA RTX A2000 Laptop GPU".into());
    let Some(mut rig) = Rig::new("interface_size_resets_and_about", 1280, 1400, config) else {
        return;
    };
    rig.open_menu();
    rig.click("header:Interface");
    rig.click("ui_scale");
    rig.click("ui_scale:3"); // Automatic, 75, 100, 125 %
    assert_eq!(rig.ui.scale_setting(), UiScale::Manual(1.25));
    assert!((rig.ui.ui_scale() - 1.25).abs() < 1e-6);
    rig.frame(&[]);
    rig.click("ui_scale");
    rig.click("ui_scale:2"); // 100 %
    assert!((rig.ui.ui_scale() - 1.0).abs() < 1e-6);
    // Reset look overrides: confirmed, then a request.
    rig.click("reset:look");
    rig.item("reset:confirm");
    rig.click("reset:cancel");
    assert!(rig.take().reset.is_none());
    rig.click("reset:look");
    rig.click("reset:confirm");
    assert_eq!(rig.take().reset, Some(ResetKind::Look));
    // Reset everything: the size back to Automatic in the menu itself.
    rig.click("reset:everything");
    rig.preview("menu-interface-reset");
    rig.click("reset:confirm");
    assert_eq!(rig.take().reset, Some(ResetKind::Everything));
    assert_eq!(rig.ui.scale_setting(), UiScale::Auto);
    rig.ui
        .set_reset_message((true, "Everything is back to its default.".into()));
    rig.frame(&[]);
    rig.item("reset:message");
    // The layout handed back when the menu closes carries the size.
    rig.click("ui_scale");
    rig.click("ui_scale:6"); // 200 %
    rig.ui.request_toggle();
    rig.frame(&[]);
    let layout = rig.ui.take_layout_change().expect("the layout changed");
    assert_eq!(layout.scale, UiScale::Manual(2.0));
    rig.ui.set_scale_setting(UiScale::Auto);
    // About (the Interface section closed again, so the whole About section is on screen).
    rig.open_menu();
    rig.click("header:Interface");
    rig.click("header:About");
    rig.item("about:version");
    rig.item("about:url");
    rig.item("about:gpu");
    rig.click("about:copy");
    assert_eq!(rig.ui.copied(), Some(dir.display().to_string().as_str()));
    rig.click("about:licences");
    rig.item("about:table");
    rig.preview("menu-about");
    rig.finish();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The first-run hint: on the picker of a first start (`first_run_done = false`), with the keys in force; "Do not
/// show again" and "Got it" dismiss it for good (written with the layout); never shown with an older settings file.
#[test]
fn the_first_run_hint_is_shown_once() {
    let _g = lock();
    let mut system = FakeSystem::default();
    let reg = registered(&mut system);
    let mut config = UiConfig {
        registration: Some(reg),
        ..UiConfig::headless()
    };
    config.layout.first_run_done = Some(false);
    let Some(mut rig) = Rig::new("the_first_run_hint_is_shown_once", 1280, 720, config) else {
        return;
    };
    assert_eq!(
        rig.ui.first_run_hint(),
        Some("Press F10 for the menu, F11 for fullscreen, Ctrl+Alt+End to quit.")
    );
    rig.ui.show_picker(false);
    for _ in 0..3 {
        rig.frame(&[]);
    }
    rig.item("picker:first_run");
    rig.preview("picker-first-run");
    rig.tick("picker:first_run_never");
    rig.click("picker:first_run_ok");
    assert_eq!(rig.ui.first_run_hint(), None);
    assert!(rig.ui.item_rect("picker:first_run").is_none());
    rig.ui.attached(0x1234);
    rig.frame(&[]);
    let layout = rig.ui.take_layout_change().expect("the layout changed");
    assert_eq!(layout.first_run_done, Some(true));
    rig.finish();
    // An older settings file (no key): no hint.
    let Some(rig) = Rig::new("the_first_run_hint_is_shown_once", 1280, 720, UiConfig::headless()) else {
        return;
    };
    assert_eq!(rig.ui.first_run_hint(), None);
    rig.finish();
}

/// The menu stays usable on a 1280 x 720 output at 200 %: it lies on the output, its body scrolls and its
/// sections open; the window picker fits too.
#[test]
fn the_menu_is_usable_at_720p_and_200_percent() {
    let _g = lock();
    let dir = temp_settings("200");
    let mut config = files_config(&dir, 720.0);
    config.layout = ultimatecrt::settings::UiState::default();
    config.layout.scale = UiScale::Manual(2.0);
    let Some(mut rig) = Rig::new("the_menu_is_usable_at_720p_and_200_percent", 1280, 720, config) else {
        return;
    };
    assert!((rig.ui.ui_scale() - 2.0).abs() < 1e-6);
    rig.ui.request_toggle();
    for _ in 0..3 {
        rig.frame(&[]);
    }
    let w = rig.ui.menu_rects().window;
    assert!(
        w[0][0] >= 0.0 && w[0][1] >= 0.0 && w[1][0] <= 1280.0 && w[1][1] <= 720.0,
        "{w:?}"
    );
    assert!(w[1][1] - w[0][1] > 600.0, "the menu uses the height: {w:?}");
    // The opacity row of the footer is on screen.
    let o = rig.ui.menu_rects().opacity;
    assert!(o[1][1] <= 720.0 && o[0][1] > w[0][1], "{o:?}");
    rig.preview("menu-200pct-720p");
    // Scroll the body to the end: the last sections are reachable.
    let body = MenuRects::at(w, 0.5, 0.5);
    let l = mouse_lparam(body[0] as i16, body[1] as i16);
    rig.frame(&[(WM_MOUSEMOVE, 0, l)]);
    for _ in 0..40 {
        rig.frame(&[(0x020A, ((-120i32 as u32 as usize) & 0xFFFF) << 16, l)]);
    }
    let about = rig.item("header:About");
    assert!(about[1][1] <= o[0][1], "About is reachable above the footer: {about:?}");
    rig.click("header:About");
    rig.item("about:version");
    rig.preview("menu-200pct-720p-about");
    rig.ui.request_toggle();
    rig.frame(&[]);
    // The picker at 200 %.
    rig.ui.show_picker(false);
    for _ in 0..3 {
        rig.frame(&[]);
    }
    let card = rig.item("picker:card");
    assert!(card[1][0] <= 1280.0 && card[1][1] <= 720.0, "{card:?}");
    rig.item("picker:capture");
    rig.finish();
    let _ = std::fs::remove_dir_all(&dir);
}

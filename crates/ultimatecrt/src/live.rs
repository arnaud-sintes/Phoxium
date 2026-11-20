//! Live controls (T-54, T-57): applies the hotkey actions the output window queued to the running application:
//! window mode, preset stepping, A/B bypass, quit, and the follow-up of monitor changes. The decisions are the
//! pure functions of `controls.rs` and `modes.rs`; this file only reads and writes the window, the renderer and
//! the settings file. Everything is driven from one call per loop iteration, [`Live::tick`].

use std::path::PathBuf;

use windows::Win32::Foundation::HWND;

use crate::{
    controls::{self, Command, Controls, SlotFlags, Step, UiHook},
    gpu,
    lifecycle::LostWatch,
    modes::{IRect, ModeState, Placement, WindowMode},
    output,
    params::Params,
    presets::Library,
    settings::{self, Settings},
};

pub struct Live {
    /// The state the actions change (`quit`, `bypass`).
    pub controls: Controls,
    pub mode: ModeState,
    library: Library,
    /// Id of the preset in force, `None` when the parameters are the table defaults or a preset file.
    current_preset: Option<String>,
    bypass_saved: Option<SlotFlags>,
    /// The settings as loaded, the one instance of the session; `global.output.windowed` (on F11 and at exit)
    /// and `global.ui` (the menu layout, [`Live::set_ui_layout`]) are written back.
    settings: Settings,
    /// Where the settings are written; `None`: never written (no settings folder, or a file the session must
    /// not overwrite, [`writable_dir`]).
    settings_dir: Option<PathBuf>,
    /// The background writer (R-31), started at the first write.
    writer: Option<settings::Writer>,
    /// Outer size of the framed window when nothing is remembered (the initial client size plus the frame).
    default_outer: (i32, i32),
    last_follow: std::time::Instant,
    /// When the menu last changed the settings without them being written yet ([`Live::flush_settings`]).
    dirty_since: Option<std::time::Instant>,
}

/// How long the settings wait after the last menu change before they are written (a drag writes once).
pub const SAVE_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(1);

impl Live {
    pub fn new(
        fullscreen: bool,
        settings: Settings,
        settings_dir: Option<PathBuf>,
        library: Library,
        current_preset: Option<String>,
        default_outer: (i32, i32),
    ) -> Live {
        let saved = settings.global.output.windowed.map(IRect::from_array);
        Live {
            controls: Controls::default(),
            mode: ModeState::new(
                if fullscreen {
                    WindowMode::Fullscreen
                } else {
                    WindowMode::Windowed
                },
                saved,
            ),
            library,
            current_preset,
            bypass_saved: None,
            settings,
            settings_dir,
            writer: None,
            default_outer,
            last_follow: std::time::Instant::now(),
            dirty_since: None,
        }
    }

    /// One loop iteration: drains the hotkey actions, follows a monitor change, and sets the host cursor
    /// (hidden over the output while the UI does not want input; there is no click-through option yet).
    /// `lost` learns the mode so that only a fullscreen output drops to a window when the source is lost.
    pub fn tick(
        &mut self,
        ui: &mut dyn UiHook,
        hwnd: HWND,
        renderer: &mut gpu::Renderer,
        params: &mut Params,
        lost: &mut LostWatch,
    ) {
        for (action, chord) in output::take_actions() {
            // The menu waiting for a new hotkey takes a registered chord as that new chord (Quit always runs).
            if ui.capture_chord(action, chord) {
                continue;
            }
            if let Some(command) = controls::dispatch(action, &mut self.controls, ui) {
                self.run(command, hwnd, renderer, params, lost);
            }
        }
        if output::take_display_changed() {
            let monitors = output::monitor_irects();
            if let Some(p) = self.mode.display_change(output::outer_rect(hwnd), &monitors) {
                output::apply_placement(hwnd, &p);
                println!("output: monitors changed, {}", describe(&p, &monitors));
            }
        }
        output::set_cursor_hidden(!ui.wants_input());
        // Follows the framed window about once a second, so a rectangle is known at exit even when the window
        // cannot be read then (minimised).
        if self.mode.mode == WindowMode::Windowed && self.last_follow.elapsed() >= std::time::Duration::from_secs(1) {
            self.last_follow = std::time::Instant::now();
            self.mode.remember(output::outer_rect(hwnd), &output::monitor_irects());
        }
    }

    fn run(
        &mut self,
        command: Command,
        hwnd: HWND,
        renderer: &mut gpu::Renderer,
        params: &mut Params,
        lost: &mut LostWatch,
    ) {
        match command {
            Command::SetBypass(on) => {
                let current: Vec<(&'static str, bool)> = renderer
                    .stage_names()
                    .into_iter()
                    .map(|n| (n, renderer.stage_enabled(n).unwrap_or(true)))
                    .collect();
                for (name, enabled) in controls::bypass_plan(on, &current, &mut self.bypass_saved) {
                    if let Err(e) = renderer.set_stage_enabled(name, enabled) {
                        eprintln!("warning: bypass: {e}");
                    }
                }
                println!("{}", controls::bypass_line(on));
            }
            Command::StepPreset(step) => {
                match controls::step_preset(&self.library, self.current_preset.as_deref(), step, params) {
                    Some(s) => {
                        *params = s.params;
                        renderer.set_params(*params);
                        println!(
                            "preset {}: {} ({})",
                            match step {
                                Step::Next => "next",
                                Step::Previous => "previous",
                            },
                            s.name,
                            s.id
                        );
                        self.current_preset = Some(s.id);
                    }
                    None => println!("preset: none available"),
                }
            }
            Command::ToggleWindowMode => {
                let monitors = output::monitor_irects();
                let placement = self.mode.toggle(
                    output::outer_rect(hwnd),
                    output::monitor_of(hwnd),
                    &monitors,
                    self.default_outer,
                );
                // Non-activating in both modes, except while the settings menu is open (it needs the focus):
                // `output::sync_activation` decides, so F11 works with the menu open or closed.
                output::apply_placement(hwnd, &placement);
                lost.set_fullscreen(matches!(placement, Placement::Fullscreen { .. }));
                println!("output: {}", describe(&placement, &monitors));
                self.save_window_rect();
            }
        }
    }

    /// The settings of the session: the one instance the hotkeys, the window rectangle and the menu layout
    /// share (the menu reads its layout and menu key from it, and writes the layout back with
    /// [`Live::set_ui_layout`]).
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The presets the hotkeys step through (the menu lists the same ones).
    pub fn library(&self) -> &Library {
        &self.library
    }

    /// Id of the preset in force (stepped by the hotkeys or selected in the menu).
    pub fn current_preset(&self) -> Option<&str> {
        self.current_preset.as_deref()
    }

    /// The menu selected a preset: the next preset hotkey steps from it.
    pub fn set_current_preset(&mut self, id: Option<String>) {
        self.current_preset = id;
    }

    /// The menu replaced the preset library (a user preset saved or updated, the files reloaded): the preset
    /// hotkeys step through the new list.
    pub fn set_library(&mut self, library: Library) {
        self.library = library;
    }

    /// The menu selected a preset (T-53): it becomes the settings preset and its overrides start empty. Written
    /// with the next [`Live::flush_settings`].
    pub fn note_preset_selected(&mut self, id: Option<&str>) {
        let id = id.unwrap_or_default();
        let g = &mut self.settings.global;
        if g.preset != id || !g.overrides.is_empty() {
            g.preset = id.to_string();
            g.overrides.clear();
            self.mark_dirty();
        }
    }

    /// The menu changed parameters from `before` to `after` (T-53): the touched ids are recorded as overrides of
    /// the preset in force, against `reference` ([`settings::record_edit`]). When the settings preset is not
    /// the preset in force (a preset stepped by a hotkey or given on the command line), the settings follow the
    /// preset in force first. Written with the next [`Live::flush_settings`] (debounced).
    pub fn note_params_edit(&mut self, before: &Params, after: &Params, reference: &Params) {
        let current = self.current_preset.clone().unwrap_or_default();
        let g = &mut self.settings.global;
        let mut dirty = false;
        if g.preset != current {
            g.preset = current;
            g.overrides.clear();
            dirty = true;
        }
        dirty |= settings::record_edit(&mut g.overrides, before, after, reference);
        if dirty {
            self.mark_dirty();
        }
    }

    /// The menu chose the adapter for the next start (`[global.gpu] adapter`, HW-3): written at once.
    pub fn set_gpu_adapter(&mut self, value: String) {
        if self.settings.global.gpu.adapter != value {
            self.settings.global.gpu.adapter = value;
            self.mark_dirty();
            self.flush_settings(true);
        }
    }

    /// The menu chose the capture path (`[global.capture] path`, A29): written at once.
    pub fn set_capture_path(&mut self, path: crate::zerocopy::CapturePath) {
        if self.settings.global.capture.path != path.id() {
            self.settings.global.capture.path = path.id().to_string();
            self.mark_dirty();
            self.flush_settings(true);
        }
    }

    /// Writes the settings now (the load upgraded something, `settings::Loaded::notes`: written back at once so
    /// that the upgrade and its note happen once).
    pub fn save_now(&mut self) {
        self.mark_dirty();
        self.flush_settings(true);
    }

    fn mark_dirty(&mut self) {
        self.dirty_since = Some(std::time::Instant::now());
    }

    /// The hotkeys after a change in the menu (`[global.hotkeys]`, the chords asked for): written at once.
    pub fn set_hotkeys(&mut self, chords: std::collections::BTreeMap<String, String>) {
        if self.settings.global.hotkeys != chords {
            self.settings.global.hotkeys = chords;
            self.mark_dirty();
            self.flush_settings(true);
        }
    }

    /// No settings file was found at start: the first-run hint is due (`[global.ui] first_run_done = false`,
    /// written with the next write). The menu shows it until it is dismissed.
    pub fn mark_first_run(&mut self) {
        if self.settings.global.ui.first_run_done.is_none() {
            self.settings.global.ui.first_run_done = Some(false);
        }
    }

    /// "Reset look overrides": the look parameter overrides cleared (quality kept), written at once.
    pub fn reset_look_overrides(&mut self) {
        // The settings follow the preset in force first (a preset stepped by a hotkey).
        let current = self.current_preset.clone().unwrap_or_default();
        let g = &mut self.settings.global;
        let mut changed = g.preset != current;
        g.preset = current;
        changed |= settings::reset_look_overrides(&mut g.overrides);
        if changed {
            self.mark_dirty();
            self.flush_settings(true);
        }
    }

    /// "Reset everything to defaults" (`Settings::reset_everything`): the pending writes finish, a copy of
    /// `settings.toml` is kept (`settings.toml.bak`), then the reset settings are written at once. Returns the
    /// copy's path (`None`: no file, or the settings are never written).
    pub fn reset_everything(&mut self) -> Option<PathBuf> {
        self.flush_settings(true);
        let _ = self.wait_saved();
        let backup = self.settings_dir.as_deref().and_then(settings::backup_current);
        if let Some(current) = self.current_preset.clone() {
            self.settings.global.preset = current;
        }
        self.settings.reset_everything();
        self.mark_dirty();
        self.flush_settings(true);
        backup
    }

    /// Removes the per-game profile of `exe` and `class` (the menu's "Saved profiles"): written at once. Returns
    /// whether one was removed.
    pub fn remove_source_profile(&mut self, exe: &str, class: &str) -> bool {
        let removed = self.settings.remove_source(exe, class);
        if removed {
            self.mark_dirty();
            self.flush_settings(true);
        }
        removed
    }

    /// Writes the settings when the menu changed them: once no change came for [`SAVE_DEBOUNCE`] (a slider
    /// drag writes once, at its end), or at once with `now` (the menu closed, the app exits). The write runs on
    /// the background writer (R-31); a failure is a warning, never an error. Returns whether a write was queued.
    pub fn flush_settings(&mut self, now: bool) -> bool {
        let Some(since) = self.dirty_since else {
            return false;
        };
        if !now && since.elapsed() < SAVE_DEBOUNCE {
            return false;
        }
        self.dirty_since = None;
        self.write()
    }

    /// Queues the settings for the background writer. False when they are never written (`settings_dir`).
    fn write(&mut self) -> bool {
        let Some(dir) = &self.settings_dir else {
            return false;
        };
        self.writer
            .get_or_insert_with(|| settings::Writer::new(dir.clone()))
            .save(&self.settings);
        true
    }

    /// Waits until the queued writes are on disk (tests, and the exit through [`Live::finish`]). Returns the
    /// error of the last failed write, if any.
    pub fn wait_saved(&self) -> Option<String> {
        self.writer.as_ref().and_then(settings::Writer::wait_idle)
    }

    /// The menu layout changed (the menu closed after a move, resize or opacity change): kept in the shared
    /// settings and written to the file. A failure is a warning, never an error.
    pub fn set_ui_layout(&mut self, layout: settings::UiState) {
        if layout == self.settings.global.ui {
            return;
        }
        self.settings.global.ui = layout;
        self.write();
    }

    /// A fullscreen output becomes the framed window of F11 (non-activating, at the remembered rectangle): a
    /// lost source given up after the wait (A23), so the only monitor shows the window picker in a normal
    /// window. Nothing happens when the output is already a window. Returns whether it changed.
    pub fn go_windowed(&mut self, hwnd: HWND) -> bool {
        if self.mode.mode != WindowMode::Fullscreen {
            return false;
        }
        let monitors = output::monitor_irects();
        let placement = self.mode.toggle(
            output::outer_rect(hwnd),
            output::monitor_of(hwnd),
            &monitors,
            self.default_outer,
        );
        output::apply_placement(hwnd, &placement);
        println!("output: {}", describe(&placement, &monitors));
        self.save_window_rect();
        true
    }

    /// Whether the output is the fullscreen one.
    pub fn is_fullscreen(&self) -> bool {
        self.mode.mode == WindowMode::Fullscreen
    }

    /// A window was attached (A23): it becomes `[global.source] last`, written at once.
    pub fn remember_source(&mut self, last: settings::LastSource) {
        if self.settings.global.source.last.as_ref() != Some(&last) {
            self.settings.global.source.last = Some(last);
            self.mark_dirty();
            self.flush_settings(true);
        }
    }

    /// The menu or the picker changed `[global.source] ask_at_start` or `wait_s` (A23): written with the next
    /// [`Live::flush_settings`] (debounced).
    /// The wait is clamped to 0 or more before the comparison; a NaN wait leaves the stored one (R-39).
    pub fn set_source_prefs(&mut self, ask_at_start: bool, wait_s: f64) {
        let s = &mut self.settings.global.source;
        let wait_s = if wait_s.is_nan() { s.wait_s } else { wait_s.max(0.0) };
        if s.ask_at_start != ask_at_start || s.wait_s != wait_s {
            s.ask_at_start = ask_at_start;
            s.wait_s = wait_s;
            self.mark_dirty();
        }
    }

    /// The menu changed the picture-area detection thresholds of `[global.source]`: written with the next
    /// [`Live::flush_settings`] (debounced).
    pub fn set_bars_prefs(&mut self, config: crate::bars::Config) {
        let s = &mut self.settings.global.source;
        let (tolerance, min, max_crop) = (config.tolerance, config.min_bar.max(1), config.max_crop);
        if s.bars_tolerance != tolerance || s.bars_min != min || s.bars_largest_crop != max_crop {
            s.bars_tolerance = tolerance;
            s.bars_min = min;
            s.bars_largest_crop = max_crop;
            self.mark_dirty();
        }
    }

    /// The Source section changed the source profile of the attached program and class (the picture area mode, a
    /// reset or undo, the scaling multiplier): stored in `[[source]]` (replacing the profile of the same
    /// executable and class) and written at once, or with the next [`Live::flush_settings`] when `debounced` (a
    /// manual limit dragged or nudged). A profile without an executable is not stored.
    pub fn save_source_profile(&mut self, profile: &settings::SourceProfile, debounced: bool) {
        if profile.exe.is_empty() || self.settings.find_source(&profile.exe, &profile.class) == Some(profile) {
            return;
        }
        self.settings.upsert_source(profile.clone());
        self.mark_dirty();
        if !debounced {
            self.flush_settings(true);
        }
    }

    /// At exit, on every exit path (the window still exists, hidden or not; a null `hwnd` skips the window):
    /// remembers where the framed window is, writes the settings and waits for the writer to finish.
    pub fn finish(&mut self, hwnd: HWND) {
        // SAFETY: plain query on a window handle (null or destroyed: false).
        if self.mode.mode == WindowMode::Windowed
            && unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(hwnd)) }.as_bool()
        {
            // A minimised window reports an off-screen rectangle: `remember` keeps the last usable one.
            self.mode.remember(output::outer_rect(hwnd), &output::monitor_irects());
        }
        self.save_window_rect();
        self.flush_settings(true);
        if let Some(w) = self.writer.take() {
            // Each failure was already reported on stderr by the writer.
            let _ = w.finish();
        }
    }

    /// Writes `global.output.windowed` when it changed. A failure is a warning, never an error.
    fn save_window_rect(&mut self) {
        let rect = self.mode.windowed.map(IRect::to_array);
        if rect == self.settings.global.output.windowed {
            return;
        }
        self.settings.global.output.windowed = rect;
        self.write();
    }
}

/// The folder the session writes its settings to (R-15): the settings folder, unless the file found there
/// must not be overwritten (unreadable, or a newer schema: [`settings::Loaded::writable`]).
pub fn writable_dir(dir: Option<&std::path::Path>, loaded: &settings::Loaded) -> Option<PathBuf> {
    dir.filter(|_| loaded.writable).map(std::path::Path::to_path_buf)
}

/// The console text of a placement: `fullscreen on monitor 1 (2560x1440 at 2560,0)` or `windowed 1296x759 at 100,80`.
fn describe(p: &Placement, monitors: &[IRect]) -> String {
    match p {
        Placement::Fullscreen { cover } => {
            let index = monitors.iter().position(|m| m == cover);
            format!(
                "fullscreen on monitor {} ({}x{} at {},{})",
                index.map_or("?".to_string(), |i| i.to_string()),
                cover.width(),
                cover.height(),
                cover.left,
                cover.top
            )
        }
        Placement::Windowed { rect } => format!(
            "windowed {}x{} at {},{}",
            rect.width(),
            rect.height(),
            rect.left,
            rect.top
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T-53: menu edits go to the overrides of the preset in force and are written debounced or on demand; a
    /// preset selection resets the overrides; the adapter choice is written at once.
    #[test]
    fn menu_changes_reach_the_settings_file() {
        let dir = std::env::temp_dir().join(format!("ucrt-live-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let lib = Library::embedded();
        let id = lib.list()[0].id.clone();
        let mut live = Live::new(
            false,
            Settings::default(),
            Some(dir.clone()),
            lib.clone(),
            Some(id.clone()),
            (800, 600),
        );
        let reference = lib.list()[0].params();
        let mut after = reference;
        after.exposure = 1.5;
        live.note_params_edit(&reference, &after, &reference);
        assert!(!live.flush_settings(false), "written before the debounce delay");
        assert!(live.flush_settings(true));
        assert!(!live.flush_settings(true), "nothing left to write");
        let saved = stored(&live, &dir);
        assert_eq!(saved.global.preset, id);
        assert_eq!(saved.global.overrides.get("exposure"), Some(&toml::Value::Float(1.5)));
        assert_eq!(
            live.settings().global.overrides,
            saved.global.overrides,
            "one shared instance"
        );
        // Selecting another preset: it becomes the settings preset, the overrides start empty.
        let other = lib.list()[1].id.clone();
        live.note_preset_selected(Some(&other));
        live.flush_settings(true);
        let saved = stored(&live, &dir);
        assert_eq!(
            (saved.global.preset.as_str(), saved.global.overrides.len()),
            (other.as_str(), 0)
        );
        // The adapter choice is written without waiting.
        live.set_gpu_adapter("Intel".to_string());
        assert_eq!(stored(&live, &dir).global.gpu.adapter, "Intel");
        // A23: the last source is written at once, the source preferences debounced.
        let last = settings::LastSource {
            exe: "pixel-demo.exe".into(),
            class: "pixel_demo".into(),
            title: "pixel-demo".into(),
        };
        live.remember_source(last.clone());
        assert_eq!(stored(&live, &dir).global.source.last, Some(last));
        live.set_source_prefs(false, 12.0);
        assert!(!live.flush_settings(false));
        assert!(live.flush_settings(true));
        let s = stored(&live, &dir).global.source;
        assert_eq!((s.ask_at_start, s.wait_s), (false, 12.0));
        // Black-bar detection preferences: debounced; a source profile: written at once, keyed by program and
        // class.
        let cfg = crate::bars::Config {
            tolerance: 16,
            min_bar: 4,
            max_crop: 0.4,
            ..crate::bars::Config::default()
        };
        live.set_bars_prefs(cfg);
        assert!(live.flush_settings(true));
        let s = stored(&live, &dir).global.source;
        assert_eq!(s.bars_config(), cfg);
        let mut profile = settings::SourceProfile {
            exe: "snes9x.exe".into(),
            class: "Snes9X".into(),
            ..settings::SourceProfile::default()
        };
        crate::bars::set_manual_crop(
            &mut Params::default(),
            &mut profile,
            &mut None,
            crate::scaling::Rect {
                x: 0,
                y: 16,
                w: 768,
                h: 672,
            },
            (768, 704),
        );
        crate::scaling::Multiplier::Manual(3, 3).apply(&mut Params::default(), &mut profile);
        live.save_source_profile(&profile, false);
        let saved = stored(&live, &dir);
        assert_eq!(saved.find_source("SNES9X.EXE", "snes9x"), Some(&profile));
        // A manual limit edit: debounced, written with the next flush.
        profile.crop = Some([2, 16, 770, 688]);
        live.save_source_profile(&profile, true);
        assert_ne!(stored(&live, &dir).find_source("snes9x.exe", "Snes9X"), Some(&profile));
        assert!(live.flush_settings(true));
        assert_eq!(stored(&live, &dir).find_source("snes9x.exe", "Snes9X"), Some(&profile));
        // Without an executable nothing is stored.
        live.save_source_profile(&settings::SourceProfile::default(), false);
        assert_eq!(stored(&live, &dir).sources.len(), 1);
        // The exit writes what is left and stops the writer; nothing is left behind but the file.
        live.set_source_prefs(true, 5.0);
        live.finish(HWND::default());
        assert_eq!(settings::load(&dir).settings.global.source.wait_s, 5.0);
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, [settings::SETTINGS_FILE]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The menu's hotkeys, profile deletions and resets reach the settings file at once; "Reset everything" keeps
    /// a copy of the file first and keeps the per-game profiles.
    #[test]
    fn hotkeys_profiles_and_resets_are_written_at_once() {
        let dir = std::env::temp_dir().join(format!("ucrt-live-resets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let lib = Library::embedded();
        let id = lib.list()[0].id.clone();
        let mut live = Live::new(
            false,
            Settings::default(),
            Some(dir.clone()),
            lib.clone(),
            Some(id.clone()),
            (800, 600),
        );
        // First run: due in memory, written with the next write.
        live.mark_first_run();
        assert!(live.settings().global.ui.first_run_due());
        let mut chords = settings::default_hotkeys();
        chords.insert("toggle_menu".into(), "Ctrl+Alt+M".into());
        live.set_hotkeys(chords.clone());
        let saved = stored(&live, &dir);
        assert_eq!(saved.global.hotkeys, chords);
        assert!(saved.global.ui.first_run_due());
        // Profiles: removal written at once.
        let profile = settings::SourceProfile {
            exe: "game.exe".into(),
            class: "C".into(),
            divisor: Some(2),
            ..settings::SourceProfile::default()
        };
        live.save_source_profile(&profile, false);
        assert_eq!(stored(&live, &dir).sources.len(), 1);
        assert!(!live.remove_source_profile("other.exe", "C"));
        assert!(live.remove_source_profile("GAME.EXE", "c"));
        assert!(stored(&live, &dir).sources.is_empty());
        live.save_source_profile(&profile, false);
        // Look overrides: cleared, quality kept.
        let reference = lib.list()[0].params();
        let mut after = reference;
        after.exposure = 1.7;
        after.quality_tier = crate::params::QUALITY_TIER_FAST;
        live.note_params_edit(&reference, &after, &reference);
        live.flush_settings(true);
        live.reset_look_overrides();
        let o = stored(&live, &dir).global.overrides;
        assert_eq!(o.keys().collect::<Vec<_>>(), ["quality_tier"]);
        // Everything: a copy first, then the defaults; the profile and the preset stay.
        live.set_gpu_adapter("Intel".into());
        let bak = live.reset_everything().expect("a copy of the file");
        let before = settings::Settings::from_toml(&std::fs::read_to_string(&bak).unwrap()).unwrap();
        assert_eq!(before.global.gpu.adapter, "Intel");
        assert_eq!(before.global.hotkeys, chords);
        let after = stored(&live, &dir);
        assert_eq!(after.global.gpu.adapter, settings::GPU_ADAPTER_AUTO);
        assert_eq!(after.global.hotkeys, settings::default_hotkeys());
        assert!(after.global.overrides.is_empty());
        assert_eq!(after.global.preset, id);
        assert_eq!(after.sources, vec![profile]);
        live.finish(HWND::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The settings on disk once the queued writes are done.
    fn stored(live: &Live, dir: &std::path::Path) -> Settings {
        assert_eq!(live.wait_saved(), None);
        settings::load(dir).settings
    }

    /// R-39: the wait is clamped before the comparison (a repeated negative wait marks the settings dirty once);
    /// NaN keeps the stored wait.
    #[test]
    fn source_prefs_are_clamped_before_the_comparison() {
        let mut live = Live::new(false, Settings::default(), None, Library::embedded(), None, (800, 600));
        live.set_source_prefs(true, -1.0);
        assert!(live.dirty_since.is_some());
        assert_eq!(live.settings().global.source.wait_s, 0.0);
        live.dirty_since = None;
        live.set_source_prefs(true, -1.0);
        assert!(live.dirty_since.is_none(), "same clamped value: not dirty again");
        live.set_source_prefs(true, f64::NAN);
        assert!(live.dirty_since.is_none());
        assert_eq!(live.settings().global.source.wait_s, 0.0);
        live.set_source_prefs(false, f64::NAN);
        assert!(live.dirty_since.is_some(), "the other preference still counts");
        assert_eq!(live.settings().global.source.wait_s, 0.0);
    }

    /// R-15: a session that found a newer-schema (or unreadable) file never writes it, at exit included.
    #[test]
    fn a_newer_settings_file_is_never_written() {
        let dir = std::env::temp_dir().join(format!("ucrt-live-newer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let text = format!(
            "version = {}\n[global]\npreset = \"future\"\n",
            settings::SCHEMA_VERSION + 1
        );
        std::fs::write(dir.join(settings::SETTINGS_FILE), &text).unwrap();
        let loaded = settings::load(&dir);
        assert!(!loaded.writable);
        let mut live = Live::new(
            false,
            loaded.settings.clone(),
            writable_dir(Some(&dir), &loaded),
            Library::embedded(),
            None,
            (800, 600),
        );
        live.set_gpu_adapter("Intel".into());
        live.set_source_prefs(false, 3.0);
        live.finish(HWND::default());
        assert_eq!(
            std::fs::read_to_string(dir.join(settings::SETTINGS_FILE)).unwrap(),
            text
        );
        // A valid file stays writable.
        std::fs::write(dir.join(settings::SETTINGS_FILE), "version = 1\n").unwrap();
        assert_eq!(writable_dir(Some(&dir), &settings::load(&dir)), Some(dir.clone()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn placements_are_described_with_the_monitor_index() {
        let m0 = IRect::new(0, 0, 2560, 1440);
        let m1 = IRect::new(2560, 0, 5120, 1440);
        let s = describe(&Placement::Fullscreen { cover: m1 }, &[m0, m1]);
        assert_eq!(s, "fullscreen on monitor 1 (2560x1440 at 2560,0)");
        assert!(describe(&Placement::Fullscreen { cover: m1 }, &[m0]).contains("monitor ?"));
        let w = describe(
            &Placement::Windowed {
                rect: IRect::new(100, 80, 1396, 839),
            },
            &[m0],
        );
        assert_eq!(w, "windowed 1296x759 at 100,80");
    }
}

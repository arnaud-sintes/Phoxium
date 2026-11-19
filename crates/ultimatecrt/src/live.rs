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
    settings_dir: Option<PathBuf>,
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
        for action in output::take_actions() {
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
        // Follows the framed window about once a second, so the rectangle is known when the window is
        // destroyed (Esc, Ctrl+Alt+Q, the close button) and can be written at exit.
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

    fn mark_dirty(&mut self) {
        self.dirty_since = Some(std::time::Instant::now());
    }

    /// Writes the settings when the menu changed them: once no change came for [`SAVE_DEBOUNCE`] (a slider
    /// drag writes once, at its end), or at once with `now` (the menu closed, the app exits). A failure is a
    /// warning, never an error. Returns whether the file was written.
    pub fn flush_settings(&mut self, now: bool) -> bool {
        let Some(since) = self.dirty_since else {
            return false;
        };
        if !now && since.elapsed() < SAVE_DEBOUNCE {
            return false;
        }
        self.dirty_since = None;
        let Some(dir) = &self.settings_dir else {
            return false;
        };
        match settings::save(dir, &self.settings) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("warning: cannot save the settings: {e}");
                false
            }
        }
    }

    /// The menu layout changed (the menu closed after a move, resize or opacity change): kept in the shared
    /// settings and written to the file. A failure is a warning, never an error.
    pub fn set_ui_layout(&mut self, layout: settings::UiState) {
        if layout == self.settings.global.ui {
            return;
        }
        self.settings.global.ui = layout;
        if let Some(dir) = &self.settings_dir {
            if let Err(e) = settings::save(dir, &self.settings) {
                eprintln!("warning: cannot save the menu layout: {e}");
            }
        }
    }

    /// The fullscreen output dropped to a window by itself (T-56): keeps the mode in step.
    pub fn dropped_to_windowed(&mut self) {
        self.mode.force_windowed();
    }

    /// At exit: remembers where the framed window is and writes it to the settings.
    pub fn finish(&mut self, hwnd: HWND) {
        if self.mode.mode == WindowMode::Windowed {
            // The window may be destroyed already (Esc, Ctrl+Alt+Q): then the last remembered rectangle stays.
            if unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(hwnd)) }.as_bool() {
                self.mode.remember(output::outer_rect(hwnd), &output::monitor_irects());
            }
        }
        self.save_window_rect();
        self.flush_settings(true);
    }

    /// Writes `global.output.windowed` when it changed. A failure is a warning, never an error.
    fn save_window_rect(&mut self) {
        let rect = self.mode.windowed.map(IRect::to_array);
        if rect == self.settings.global.output.windowed {
            return;
        }
        self.settings.global.output.windowed = rect;
        if let Some(dir) = &self.settings_dir {
            if let Err(e) = settings::save(dir, &self.settings) {
                eprintln!("warning: cannot save the window position: {e}");
            }
        }
    }
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
        let saved = settings::load(&dir).settings;
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
        let saved = settings::load(&dir).settings;
        assert_eq!(
            (saved.global.preset.as_str(), saved.global.overrides.len()),
            (other.as_str(), 0)
        );
        // The adapter choice is written without waiting.
        live.set_gpu_adapter("Intel".to_string());
        assert_eq!(settings::load(&dir).settings.global.gpu.adapter, "Intel");
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

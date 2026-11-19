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
    /// The settings as loaded; only `global.output.windowed` is written back (on F11 and at exit).
    settings: Settings,
    settings_dir: Option<PathBuf>,
    /// Outer size of the framed window when nothing is remembered (the initial client size plus the frame).
    default_outer: (i32, i32),
    last_follow: std::time::Instant,
}

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
                self.run(command, ui, hwnd, renderer, params, lost);
            }
        }
        if output::take_display_changed() {
            let monitors = output::monitor_irects();
            if let Some(p) = self.mode.display_change(output::outer_rect(hwnd), &monitors) {
                output::apply_placement(hwnd, &p, !ui.menu_open());
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
        ui: &dyn UiHook,
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
                // Non-activating in both modes, except while the settings menu is open (it needs the focus).
                output::apply_placement(hwnd, &placement, !ui.menu_open());
                lost.set_fullscreen(matches!(placement, Placement::Fullscreen { .. }));
                println!("output: {}", describe(&placement, &monitors));
                self.save_window_rect();
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

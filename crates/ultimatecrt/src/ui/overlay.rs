//! The read-only stats overlay (T-52b, OUT-3): capture and render fps, GPU milliseconds, pixels per triad,
//! in a small window at the top right of the output. It is drawn by the same ImGui pass as the menu, but it
//! never takes input or focus: the window has no inputs, no navigation and is never focused, and showing it
//! does not route any message to the UI nor change the output's `WS_EX_NOACTIVATE` (only the menu does). The
//! full content of the overlay is T-55.

use dear_imgui_rs::{Condition, Ui as Frame, WindowFlags};

use super::UiState;

/// ImGui id of the overlay window (no title bar).
pub const TITLE: &str = "###ucrt_overlay";

/// Distance of the overlay from the top-right corner of the output, in pixels.
pub const MARGIN: f32 = 10.0;

/// Background opacity of the overlay window.
pub const BG_ALPHA: f32 = 0.6;

/// The overlay text, one line per entry.
pub fn lines(state: &UiState) -> Vec<String> {
    let mut out = vec![format!(
        "capture {:.0} fps | render {:.0} fps",
        state.capture_fps, state.render_fps
    )];
    match &state.timings {
        Some(t) => {
            let ui = t.stage_ms("ui").map(|ms| format!(" (ui {ms:.3})")).unwrap_or_default();
            out.push(format!("GPU {:.2} ms{ui}", t.frame_ms));
        }
        None => out.push("GPU -".to_string()),
    }
    match &state.placement {
        Some(p) => out.push(format!(
            "{:.2} px per triad | {:.2} px per line",
            p.px_per_triad, p.line_px
        )),
        None => out.push("px per triad -".to_string()),
    }
    out
}

/// Builds the overlay window with its top-right corner `MARGIN` px from the top-right corner of a display
/// of `display` px, given the overlay size of the previous frame (`last_size`, auto-resized by ImGui).
/// Returns the window rectangle (min, max) of this frame.
pub fn build(ui: &Frame, lines: &[String], display: [f32; 2], last_size: [f32; 2]) -> [[f32; 2]; 2] {
    let pos = [(display[0] - last_size[0] - MARGIN).max(0.0), MARGIN];
    ui.window(TITLE)
        .position(pos, Condition::Always)
        .bg_alpha(BG_ALPHA)
        .flags(
            WindowFlags::NO_INPUTS
                | WindowFlags::NO_TITLE_BAR
                | WindowFlags::ALWAYS_AUTO_RESIZE
                | WindowFlags::NO_SAVED_SETTINGS
                | WindowFlags::NO_FOCUS_ON_APPEARING
                | WindowFlags::NO_BRING_TO_FRONT_ON_FOCUS
                | WindowFlags::NO_NAV
                | WindowFlags::NO_MOVE,
        )
        .build(|| {
            for l in lines {
                ui.text(l);
            }
            let (p, s) = (ui.window_pos(), ui.window_size());
            [p, [p[0] + s[0], p[1] + s[1]]]
        })
        .unwrap_or([pos, pos])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu::{FrameTimings, StageTiming};

    #[test]
    fn lines_show_fps_gpu_ms_and_px_per_triad() {
        let empty = lines(&UiState::default());
        assert_eq!(empty.len(), 3);
        assert!(empty[1].contains('-') && empty[2].contains('-'));
        let state = UiState {
            capture_fps: 59.9,
            render_fps: 60.0,
            timings: Some(FrameTimings {
                frames: 1,
                frame_ms: 1.25,
                stages: vec![StageTiming {
                    name: "ui",
                    ms: Some(0.125),
                    bypassed: false,
                }],
            }),
            placement: Some(crate::placement::readout(
                &crate::params::Params::default(),
                (320, 240),
                (1920, 1080),
                false,
            )),
            ..UiState::default()
        };
        let l = lines(&state);
        assert_eq!(l[0], "capture 60 fps | render 60 fps");
        assert_eq!(l[1], "GPU 1.25 ms (ui 0.125)");
        assert!(l[2].contains("px per triad"), "{}", l[2]);
    }
}

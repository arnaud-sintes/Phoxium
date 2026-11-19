//! The read-only stats overlay (T-52b, OUT-3): capture and render fps, GPU milliseconds, pixels per triad,
//! in a small window at the top right of the output. It is drawn by the same ImGui pass as the menu, but it
//! never takes input or focus: the window has no inputs, no navigation and is never focused, and showing it
//! does not route any message to the UI nor change the output's `WS_EX_NOACTIVATE` (only the menu does). The
//! full content of the overlay is T-55.
//!
//! Look (`super::theme`): two columns, labels in small capitals on the left, values right-aligned in tabular
//! digits on the right; the GPU total in the accent colour.

use dear_imgui_rs::{Condition, Ui as Frame, WindowFlags};

use super::theme::{self, Fonts, Rgba, METRICS, PALETTE};
use super::UiState;

/// ImGui id of the overlay window (no title bar).
pub const TITLE: &str = "###ucrt_overlay";

/// Distance of the overlay from the top-right corner of the output, in pixels.
pub const MARGIN: f32 = 10.0;

/// Background opacity of the overlay window.
pub const BG_ALPHA: f32 = 0.75;

/// One overlay row: label and value.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub label: &'static str,
    pub value: String,
    /// Highlight the value (the GPU total).
    pub strong: bool,
}

fn row(label: &'static str, value: String) -> Row {
    Row {
        label,
        value,
        strong: false,
    }
}

/// The overlay rows. A missing readout shows "-".
pub fn lines(state: &UiState) -> Vec<Row> {
    let mut out = vec![
        row("Capture", format!("{:.0} fps", state.capture_fps)),
        row("Render", format!("{:.0} fps", state.render_fps)),
    ];
    match &state.timings {
        Some(t) => {
            out.push(Row {
                strong: true,
                ..row("GPU", format!("{:.2} ms", t.frame_ms))
            });
            if let Some(ms) = t.stage_ms("ui") {
                out.push(row("UI", format!("{ms:.3} ms")));
            }
        }
        None => out.push(row("GPU", "-".to_string())),
    }
    match &state.placement {
        Some(p) => {
            out.push(row("Triad", format!("{:.2} px", p.px_per_triad)));
            out.push(row("Line", format!("{:.2} px", p.line_px)));
        }
        None => out.push(row("Triad", "-".to_string())),
    }
    out
}

/// Builds the overlay window with its top-right corner `MARGIN` px (scaled) from the top-right corner of a
/// display of `display` px, given the overlay size of the previous frame (`last_size`, auto-resized by ImGui).
/// Returns the window rectangle (min, max) of this frame.
pub fn build(ui: &Frame, rows: &[Row], fonts: &Fonts, display: [f32; 2], last_size: [f32; 2]) -> [[f32; 2]; 2] {
    let s = theme::scale_of(ui);
    let margin = MARGIN * s;
    let pos = [(display[0] - last_size[0] - margin).max(0.0), margin];
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
            // Fixed columns (an auto-resized window has no stable right edge to align to).
            let label_w = theme::with_font(ui, fonts.strong, METRICS.font_small, || {
                rows.iter()
                    .map(|r| ui.calc_text_size(r.label.to_uppercase())[0])
                    .fold(0.0, f32::max)
            });
            let value_w = theme::with_font(ui, fonts.body, METRICS.font_body, || {
                rows.iter().map(|r| ui.calc_text_size(&r.value)[0]).fold(0.0, f32::max)
            });
            let x0 = ui.cursor_pos()[0];
            let gap = 18.0 * s;
            for r in rows {
                let y = ui.cursor_pos()[1];
                theme::with_font(ui, fonts.strong, METRICS.font_small, || {
                    // Small capitals centred on the body line.
                    let dy = (METRICS.font_body - METRICS.font_small) * 0.5 * s;
                    ui.set_cursor_pos([x0, y + dy]);
                    ui.text_colored(PALETTE.text_muted, r.label.to_uppercase());
                });
                theme::with_font(ui, fonts.body, METRICS.font_body, || {
                    let colour: Rgba = if r.strong { PALETTE.accent } else { PALETTE.text };
                    let w = ui.calc_text_size(&r.value)[0];
                    ui.set_cursor_pos([x0 + label_w + gap + value_w - w, y]);
                    ui.text_colored(colour, &r.value);
                });
            }
            let (p, sz) = (ui.window_pos(), ui.window_size());
            [p, [p[0] + sz[0], p[1] + sz[1]]]
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
        assert_eq!(empty.len(), 4);
        assert_eq!(empty[2].value, "-");
        assert_eq!(empty[3].value, "-");
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
        assert_eq!(l[0], row("Capture", "60 fps".into()));
        assert_eq!(l[1], row("Render", "60 fps".into()));
        assert_eq!((l[2].label, l[2].value.as_str(), l[2].strong), ("GPU", "1.25 ms", true));
        assert_eq!(l[3], row("UI", "0.125 ms".into()));
        assert_eq!(l[4].label, "Triad");
        assert!(l[4].value.ends_with(" px"), "{:?}", l[4]);
        assert_eq!(l[5].label, "Line");
    }
}

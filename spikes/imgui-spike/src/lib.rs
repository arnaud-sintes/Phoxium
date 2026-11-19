//! T-08 (TEC-5) spike: Dear ImGui through `dear-imgui-rs` / `dear-imgui-ash` on our own ash 0.38
//! Vulkan 1.3 device, dynamic rendering only, input fed by hand from raw Win32 messages.
//! Throw-away code (branch `spike/T-08`, never merged); see `RESULTS.md`.

pub mod gpu;
pub mod input;
pub mod overlay;

pub type Res<T> = Result<T, Box<dyn std::error::Error>>;

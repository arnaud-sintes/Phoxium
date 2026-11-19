# T-08 (TEC-5) ImGui binding spike: results of the build part

Date 2026-11-19, dev machine (Windows 11 build 26300, NVIDIA RTX A2000 8GB Laptop GPU, Rust 1.98.1, MSVC).
Branch `spike/T-08`, throw-away code, never merged. This file and `QUESTIONS.md` TEC-5 carry the facts to T-13.

Scope of this run: everything that needs no window. No window was opened, no hotkey registered, no input
synthesised. The live part (window focus hand-off, F11/F12) is prepared in `src/bin/live.rs` and
`scripts/run-t08-live.ps1` and still has to be run on a hands-off desktop (section 6).

## 1. Crate candidates

| Set | Versions (pinned) | Builds from crates.io | Licence | ash | Dynamic rendering | Verdict |
|---|---|---|---|---|---|---|
| A: `dear-imgui-rs` + `dear-imgui-sys` + `dear-imgui-ash` | `=0.22.0` each, Dear ImGui 1.93.1 WIP (docking) via cimgui | yes, no git submodule, no network, no libclang; needs the MSVC C++ compiler | MIT OR Apache-2.0 (all three); whole tree passes `cargo deny check licenses` with the project `deny.toml` | `^0.38`, resolves to our `ash 0.38.0+1.3.281` | yes: feature `dynamic-rendering` (default `render-pass` turned off); verified headless, zero validation messages | **passes the build part** |
| B (fallback): `imgui` + `imgui-rs-vulkan-renderer` | `imgui =0.12.0` (Dear ImGui 1.89, last release 2024-05-05), `imgui-rs-vulkan-renderer =1.16.0` (third party, adrien-ben) | yes (build only, scratch crate, not run); the renderer fails to compile alone (`ash::util::read_spv` missing) because it takes `ash` with `default-features = false`; it compiles once `ash` with its default `std` feature is in the tree, as in our crate | imgui MIT OR Apache-2.0, renderer MIT, tree permissive (adds ultraviolet, wide, safe_arch, bytemuck: Zlib OR Apache-2.0 OR MIT) | `0.38` | has a `dynamic-rendering` feature (not exercised) | not needed; older Dear ImGui, no 1.92 texture protocol |
| C (fallback): own ash renderer over `dear-imgui-sys` | n/a | n/a | n/a | n/a | n/a | not needed; feasible: `dear-imgui-rs` exposes a renderer-agnostic frame and texture-request API (its example `custom_renderer_headless.rs`), so an own renderer would only replace `dear-imgui-ash` |

How set A builds (read from `dear-imgui-sys-0.22.0/build.rs` and the build log): the crate package vendors
cimgui and Dear ImGui under `third-party/cimgui` (no submodule), uses its checked-in
`bindings_pregenerated_windows.rs` (no bindgen, no libclang) and compiles the C++ with `cc` (MSVC `cl.exe`,
`/MD`, C++17). Log line: `NATIVE_MODE=source`. A prebuilt download happens only with the `prebuilt` feature
or the `IMGUI_SYS_*` prebuilt environment variables; neither is used. The Vulkan renderer embeds its SPIR-V
(`src/shaders/*.spv`), so no `glslc` is needed.

Dependency tree of set A (normal and build): `ash`, `bitflags`, `parking_lot`, `thiserror`, `mint`, `smallvec`,
`cc`, `find-msvc-tools`, `shlex`, `dear-imgui-build-support` (+ `serde`, `serde_json`, `itoa`, `memchr`, `zmij`,
proc-macro crates), `windows` (spike only). Licences (cargo metadata): MIT OR Apache-2.0 for nearly all; `libloading`
ISC; `memchr` Unlicense OR MIT; `mint`, `zmij`, `redox_syscall` MIT; `unicode-ident` (MIT OR Apache-2.0) AND
Unicode-3.0. All are in the `deny.toml` allow list; `cargo deny --locked check licenses`: `licenses ok`.

Licence files: the four `dear-imgui-*` crate packages ship **no LICENSE file** (only the `license` field), so a
release package has to take the MIT/Apache texts from the upstream repository (not fetched here). The vendored
C/C++ carries its own notices: Dear ImGui `LICENSE.txt` (MIT, Omar Cornut), cimgui `LICENSE` (MIT, Stephan
Dilly), and `THIRD_PARTY_NOTICES.md` (MIT, stack-layout shim, compiled only with the `stack-layout` feature,
which we do not use). `scripts/package.ps1` must add these notices to the release zip (T-52a or T-62).

## 2. Build cost (set A, dev machine)

| Measure | Value |
|---|---|
| Cold debug build of the dependencies (C++ at `/Od`) | 31.4 s |
| Cold release build of the whole spike (`live`, thin LTO) | 53.8 s |
| `dear-imgui-sys` alone, release rebuild (the C++ part) | 16.0 s |
| Warm release relink after touching `live.rs` | 4.7 s |
| `live.exe` release (ash, windows, swapchain, ImGui with the stats and menu windows) | 2,324,992 bytes |
| `dear_imgui.lib` static archive (before linker stripping) | 6,827,708 bytes |

The binary size delta on `ultimatecrt` is not measured (the spike has no build without ImGui); `live.exe` as a
whole, 2.3 MB, is an upper bound for what the UI adds to the existing exe minus ash and windows.

## 3. Headless test (`tests/headless.rs`), result: PASS

`cargo test --locked` with `UCRT_REQUIRE_GPU=1`: 6 unit tests and 1 GPU test, all pass. Surface-less Vulkan 1.3
device (copy of `gpu/context.rs`: counting debug messenger, synchronization validation on), `dear-imgui-ash` in
dynamic-rendering mode (no `VkRenderPass` anywhere), offscreen `R8G8B8A8_UNORM` 320x240 target, readback,
32 frames. Validation layer and synchronization validation enabled: **0 validation messages**.

Checked:
- Non-empty: more than 10,000 pixels differ from the clear colour.
- Window background at a known pixel (15, 205) equals the style colour (51, 102, 153) within 1 code; a pixel
  outside the window keeps the clear colour.
- Text renders: more than 100 pixels of the pure-green text colour; setting the text field to "imgui spike"
  adds more than 30 green pixels.
- Slider grab (pure red) mean x moves by more than 100 px between values 0 and 1.
- Input through the Win32 translation (synthetic message numbers, no window): mouse press at 5 % of the
  slider gives 0.000, drag to 90 % and release gives 1.000; click into the text field, `WM_CHAR` h, i, !,
  `WM_KEYDOWN`/`UP` Backspace gives "hi"; click on the button increments its counter; click on the combo then
  Down, Down, Enter selects entry 2 (needs `ConfigFlags::NAV_ENABLE_KEYBOARD`, set in `Overlay::new`).
- Overlay on a background image (gradient uploaded by copy, attachment load op LOAD), window colour
  (0, 0, 255) at style alpha 0.5 (128/255 after packing): three pixels equal
  `src * a + dst * (1 - a)` exactly (error 0 codes, tolerance 1), alpha stays 255; outside the window the
  background is unchanged.
- GPU time of the overlay pass (timestamps around the dynamic-rendering scope): median 0.009 ms, max 0.012 ms
  at 320x240. The live harness measures it at the window size.

## 4. Input plumbing (`src/input.rs`), unit tests: PASS

Pure functions, no window: `Win32Input::translate(msg, wparam, lparam)` -> `UiEvent`s, `apply` -> `Io`.
Covered by 6 tests: mouse position (signed coordinates), left/right/middle/X buttons, double clicks as presses,
mouse leave; wheel and horizontal wheel sign and scale (fractions of `WHEEL_DELTA`); key down/up mapping of
Tab, Enter, keypad Enter (extended bit), Backspace, arrows, Delete, Home, End, Escape, letters, digits, F12,
keypad, OEM keys; modifiers with left/right from the extended bit and the right-Shift scan code, `Mod*` events
before the key, no duplicate on auto-repeat, all released on focus loss; `WM_CHAR` with UTF-16 surrogate pairs
and control characters dropped (keys carry them); `WM_DPICHANGED` -> DPI scale (dpi / 96).

## 5. Integration notes for T-52a (from the code and the headless run)

- Frame flow: `imgui.begin_frame()`, build the UI, `frame.render(renderer.renderer_consumer()?)` gives a
  `PendingFrame`; `renderer.prepare_frame(pending)` uploads texture requests (font atlas) and gives a
  `ReconciledFrame`; `unsafe renderer.cmd_draw(cb, frame)` inside our own `cmd_begin_rendering` scope on the
  swapchain image (load op LOAD after `encode`, or in the same scope since the format is the same).
- `prepare_frame` submits its own upload command buffers to the queue we pass (own command pool, own fences).
  It must be the render queue, used from the render thread.
- `Options::in_flight_frames` must equal the pipeline's `FRAMES_IN_FLIGHT` (2): the renderer cycles one mesh
  buffer per frame and relies on our fence wait before reuse.
- Colour: `framebuffer_srgb = false` (default): ImGui colours are written as they are into the UNORM swapchain,
  which is what we want (the overlay is not part of the linear CRT pipeline).
- `AshRenderer::shutdown(&mut imgui)` is mandatory before the ImGui context drops: without it Dear ImGui asserts
  ("Forgot to shutdown Renderer backend?") and aborts the process (seen during the spike when a test assertion
  unwound). The spike's `Overlay` does it in `Drop`.
- Dear ImGui keeps the current context in a process global: one context per process, GPU tests that use ImGui
  run in one test function (or `--test-threads=1`).
- ImGui trickles input events (one conflicting event per frame), so a press and a move arrive on successive
  frames: fine at 60 fps, but tests must render one frame per event.
- No ini file (`set_ini_filename(None)`), no log file.
- API churn: 0.20, 0.21 and 0.22 in three weeks, and 0.22 removed the positional renderer constructor
  (`AshRendererConfig` now). Keep the exact pins and expect edits on every upgrade.

## 6. Live harness (prepared, not run)

Files: `src/bin/live.rs`, `scripts/run-t08-live.ps1` (in the spike folder: the plan limits T-08 to
`spikes/imgui-spike/**` and TEC-5).

Run, on a hands-off desktop, from the spike folder: `powershell -ExecutionPolicy Bypass -File
scripts\run-t08-live.ps1` (options `-Cycles 10`, `-FgMethod auto|plain|alt|attach`, `-NoBuild`). It builds
`pixel-demo` and `live` in release, then for windowed and fullscreen: starts pixel-demo (the source), starts
`scripts/poll-foreground.ps1` (T-06) as an independent poll in a background job, runs `live --selftest 10`,
and checks: F11 and F12 registered; 2 F11 toggles with the source focused; 10 opens that activated the output
window (method logged); "t08" typed with SendInput reached the focused ImGui text field 10 times; 10 closes
that returned the foreground to pixel-demo; no foreground change outside these hand-offs (live CSV, 10 ms poll)
and exactly 20 changes in the independent poll; zero validation messages. PASS/FAIL per mode, exit 0 when both
pass. Results in `live-results/` (git-ignored; CSVs hold window titles, delete after reading).

`live` itself: raw Win32 window (`WS_EX_TOPMOST | WS_EX_TOOLWINDOW`, plus `WS_EX_NOACTIVATE` while the menu is
closed, `MA_NOACTIVATE` on click), shown with `SW_SHOWNOACTIVATE`; F11 switches `WS_POPUP` over the monitor and
`WS_OVERLAPPEDWINDOW` with `SWP_NOACTIVATE`; Vulkan swapchain FIFO `B8G8R8A8_UNORM`, one frame in flight;
read-only stats overlay (`NO_INPUTS`) always drawn, settings menu while open. F12 open: remember
`GetForegroundWindow()`, drop `WS_EX_NOACTIVATE`, then `--fg-method auto` tries `SetForegroundWindow`, then a
synthetic Alt tap + `SetForegroundWindow`, then `AttachThreadInput` to the foreground thread +
`SetForegroundWindow`/`BringWindowToTop`/`SetFocus`, logging which one worked; input messages then go through
`Win32Input` into ImGui and the text field gets the keyboard focus. F12 close: restore `WS_EX_NOACTIVATE`,
give the foreground back to the remembered window with the same method chain, log the result. Summary file:
counts, methods per cycle, overlay GPU ms median, validation count.

Open questions only the live run answers:
- Whether plain `SetForegroundWindow` from the `WM_HOTKEY` handler is allowed (the hotkey should count as the
  last input event), or one of the workarounds is needed; run once with `-FgMethod plain` to know.
- Whether `RegisterHotKey(VK_F12)` succeeds: Microsoft's RegisterHotKey documentation reserves F12 for the
  debugger (not checked on this machine). If it fails, T-54 needs another default key or a modifier.
- Whether the synthetic Alt tap has side effects in the game (menu bar activation in Win32 apps).
- Whether toggling `WS_EX_NOACTIVATE` at run time is enough without `SWP_FRAMECHANGED`.
- Real typing, dragging and clicking by a person (the self-test types with SendInput only), the behaviour with a
  real game instead of pixel-demo (T-06 game, cursor clip), and the overlay GPU ms at 2560x1440.

Deviation, spike only: DESIGN.md forbids input synthesis; the self-test and the `alt` method call SendInput.
The product keeps only what the live run proves necessary.

## 7. Recommendation (decision at T-13)

Set A, pinned exactly: `dear-imgui-rs = "=0.22.0"`, `dear-imgui-sys = "=0.22.0"`,
`dear-imgui-ash = { version = "=0.22.0", default-features = false, features = ["dynamic-rendering"] }`, with our
own Win32 input module (`src/input.rs`) and no platform backend crate. Fallback order unchanged: own ash renderer
over `dear-imgui-sys`, then set B.

## 8. Risks

- Single-maintainer binding with fast API churn (three minor releases in three weeks, breaking renderer API).
- Build needs the MSVC C++ toolchain (already needed by `cc`-based crates on this machine; to document in ENV).
- Missing LICENSE files in the crate packages: the release zip must carry the upstream notices.
- The renderer submits its own upload work on our queue; with a future second queue or thread this must stay
  on the render queue.
- Focus hand-off and F12 registration are unverified until the live run.

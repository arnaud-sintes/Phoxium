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

## 6. Live harness (run 2026-11-19, results in section 9)

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
- F12 can be held by another program (error 1409 on the dev machine): the menu key must be configurable and registration failure handled (section 9.2). Focus hand-off verified with plain `SetForegroundWindow` (section 9.3).

## 9. Live run 2026-11-19

Branch `task/T-08live`, dev machine (Windows 11 build 26300, RTX A2000 Laptop, Dell P2418D 2560x1440), owner away,
hands-off desktop (idle log below). Source window: `pixel-demo` 256x240 at 3x. Every run: 2 F11 toggles with the
source focused, then N cycles of menu key (open), `t08` typed with SendInput, menu key (close). Validation layer
and synchronization validation on in every run: 0 messages. No pixel-demo/live/poll process left after any run.

### 9.1 Results per run

| Run | Mode | Menu key | fg method | Cycles | opens ok | text ok | closes to source | Poll fg changes (expected) | Verdict |
|---|---|---|---|---|---|---|---|---|---|
| 1 | windowed | F12 | auto | 10 | 0/10 | 0/10 | 0/10 | 0 (20) | FAIL: `RegisterHotKey(F12)` error 1409 |
| 1 | fullscreen | F12 | auto | 10 | 0/10 | 0/10 | 0/10 | 0 (20) | FAIL: same |
| 1b | windowed | Ctrl+Alt+F12 | auto | 10 | 10/10 | 10/10 | 10/10 | 20 (20) | PASS |
| 1b | fullscreen | Ctrl+Alt+F12 | auto | 10 | 10/10 | 10/10 | 10/10 | 20 (20) | PASS (see note a) |
| 2 | windowed | F10 | plain | 10 | 10/10 | 10/10 | 10/10 | 20 (20) | PASS |
| 2 | fullscreen | F10 | plain | 10 | 10/10 | 10/10 | 10/10 | 20 (20) | PASS (see note b) |
| 3 | windowed, long | F10 | auto | 28 (94 s) | 28/28 | 28/28 | 28/28 | 56 (56) | PASS (see 9.4) |

Note a: the script flagged one "unexpected foreground change" at t=6786 ms: the live poll thread logged the
restore to the source 1 ms before the main loop logged its `close` row (race inside the harness, not a focus
leak). Checker fixed (window of -200 ms to +1000 ms around the hand-off row); re-evaluated on the saved CSVs: 0
unexpected changes in both modes. Run 1b was not repeated after the fix.
Note b: one flagged change in the live CSV at t=14726 ms with an empty foreground title while the menu was open
(cycle 3), most likely the foreground being NULL for a moment during the switch. The independent 50 ms poll saw
exactly 20 changes, and all counters are 10/10. The CSV was deleted before its detail was read (not repeated),
so the cause is not proven.

F11: registered in every run, 2 toggles with the source focused, no focus change.

### 9.2 Hotkey registration

- `RegisterHotKey(hwnd, F12, MOD_NOREPEAT)` fails in our window with "Hot key is already registered."
  (ERROR_HOTKEY_ALREADY_REGISTERED, 1409), in both modes, with no other process of ours running. A separate
  PowerShell process confirms: plain F12 gives 1409 there too, while F10, Ctrl+Alt+F12 and F11 register
  (MOD_NOREPEAT). So another program of the owner's desktop holds F12 system-wide; it is not our window and not
  the debugger reservation (we did not identify the holder). On a machine without that program F12 may well
  register. The product must not depend on F12 registering: handle the failure (log, tell the user in the
  window title or log) and offer a configurable menu key; the F12 default of CLAUDE.md stays but needs a fallback.
- `F10` and `Ctrl+Alt+F12` register and work. F10 caveat for the product: a global F10 steals the menu-bar key
  of every application; Ctrl+Alt+F12 is less intrusive. A game that itself grabs F12 (Steam screenshots, NVIDIA
  overlay) is the likely source of the clash.

### 9.3 Foreground method

`auto` (plain, then Alt, then attach) used `plain` for all 10+10+28 opens and for all closes: no Alt tap and no
AttachThreadInput was ever needed. Run 2 forced `plain`: identical result. Caveat that decides the design: in
the self-test the hotkey is itself a synthesised key press, which counts as the last input event, so
`SetForegroundWindow` from the `WM_HOTKEY` handler is allowed. A real key press does the same. The close hand-back
also worked with `plain`, 48 of 48 (while the menu window is foreground, our process is the one that last
received input). Not covered: hand-off triggered by something that is not an input event (a timer, a
tray/command), and a real game as the source. The Alt trick is not needed, so the product needs no synthetic
input for the focus hand-off.

Typed text: "t08" sent with SendInput (`KEYEVENTF_UNICODE`) reached the ImGui text field in every cycle (48 of 48).
Real typing by a person, dragging and clicking were not tested (nobody present).

### 9.4 Long run and the external activation at about 25 s

Run 3: 28 cycles (94 s), windowed, topmost non-activating window while closed. T-07 found that Windows activates
any new topmost window about 25 s after the process starts. In the live log the menu was open at 24.4 to 26.1 s
(cycle 6) and during every other 25 s +/- 2 s moment of the window life; the live CSV has exactly 56 `fg` rows
(2 per cycle, each within 20 to 60 ms of an `open` or `close` row), and the independent poll counted 56 changes,
the expected number. No additional activation, no lost hand-off, no focus change at 25 s or at any later time.
The T-07 external activation was therefore not reproduced here: this spike window is created with
`SW_SHOWNOACTIVATE` and `WS_EX_NOACTIVATE`, and (not isolated) `WS_EX_TOOLWINDOW`; which of these, or the later
`SWP_NOACTIVATE` topmost, avoids it was not tested. The product logic that hands the foreground back (T-07) is not
in the spike and was not exercised.

### 9.5 Overlay GPU cost (median of per-frame timestamps, stats overlay always on, menu open 3 s of ~4.2 s per cycle)

| Mode | Window | Overlay GPU median |
|---|---|---|
| windowed | about 1280x720 (default window; size not logged) | 0.145 to 0.160 ms (4 runs) |
| fullscreen | 2560x1440 (the monitor) | 0.265 to 0.270 ms (3 runs) |

The headless figure (0.009 ms at 320x240, no menu) is not comparable: the live figure includes the menu window
part of the time and the GPU runs at its idle clock (a nearly empty frame: the passthrough is a clear). Order of
magnitude: 0.15 to 0.27 ms, 1.6 % of a 60 Hz frame at most.

### 9.6 Changes to the harness (spike only)

- `live.rs`: new `--menu-key f12|f10|ctrl-alt-f12` (default f12) for hotkey and self-test; registration result is logged.
- `run-t08-live.ps1`: `-MenuKey`, `-Modes` (windowed only for the long run), the independent poll result is now
  read (Write-Host output of the job was lost, "poll gave no result"), and the unexpected-change check tolerates
  the 200 ms log-order race described in note a.

### 9.7 Idle log (GetLastInputInfo; every value consistent with no input other than our own injections)

294 s at 10:38:14, 324 s at 10:38:45 (run 1 start), 21 s at 10:40:43 (after run 1, own), 64 s at 10:41:26, 74 s at
10:41:37 (run 1b start), 20 s at 10:43:35 (after 1b, own), 52 s at 10:44:06 (run 2 start), 20 s at 10:46:04 (after
run 2, own), 33 s at 10:46:16 (run 3 start), 38 s at 10:48:32 (after run 3, own). Idle was never lower than the
time since our last injection.

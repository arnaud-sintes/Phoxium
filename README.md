# ultimateCRT

A physically based CRT shader for Windows. It captures a window, runs it through a virtual CRT tube on the GPU and shows the result in a separate output window. See `CLAUDE.md` for the vision, `CRT.md` for the physics and `QUESTIONS.md` for decisions and open questions.

Current state: the physical chain runs on the GPU in linear light (FP16): source decode, video amplifier, gun law and white balance, automatic beam limiting, scanline beam with spot growth, geometry and convergence, aperture grille, slot mask or delta dot mask, phosphor saturation and persistence, and an output transform (tube colour to sRGB, shoulder, dither), with 10 tube presets and a settings file. Not there yet: the ImGui settings menu and the F11 and F12 hotkeys (parameters are changed through `--preset`, `--set` of `render`, and the settings file), the optics stage (halation, glare), the optional composite, interlace, ageing and burn-in modules, and any validation on a real game. See `ARCHITECTURE.md` for the stage table and `CHANGELOG.md` for the detail.

## Requirements

- Windows 11 x64, a Vulkan 1.3 GPU (developed on an NVIDIA RTX A2000 Laptop)
- Rust 1.98.1 (pinned in `rust-toolchain.toml`, MSVC toolchain)
- Vulkan SDK with `glslc` (`VULKAN_SDK` set, or `glslc` on `PATH`)

Setup, tests, packaging and releases are in `ENV.md`. Architecture is in `ARCHITECTURE.md`, coding rules in `DESIGN.md`.

## Build

```
cargo build --release
```

## How to run

1. Start the demo source: `target\release\pixel-demo.exe`
   - Space cycles the contents, 1 to 9 and 0 set the integer scale 1 to 10, M cycles the scale mode, B the filter, A the alignment, F toggles fullscreen, C the frame counter, Esc quits. The window is resizable and re-lays out the picture on every size change; it handles `WM_DPICHANGED` (an exact-size window stays exact on a monitor with another scaling).
   - Universal test source (T-95), all options also on the command line (`pixel-demo --help`):
     - `--native WxH` (alias `--res`, default 256x240; 320x240, 640x480, 1280x720, 1920x1080, any 16 to 8192) is the native image, `--image file.png` shows a pixel-art image (its size is the native size unless `--native` is given), `--par N:M` is the pixel aspect ratio (for example `8:7`).
     - `--mode native` shows the image 1:1 (PAR ignored), `--mode integer --scale N` an integer upscale x1 to x10 (nearest, `--scale N` alone keeps meaning that, default 3), `--mode fit` the largest integer scale that fits the client area (1 to 10), `--mode fit-free` a non-integer fit that keeps the aspect, with `--filter nearest|bilinear|bicubic` to emulate a smoothed upscaler (for the smoothed-source warning of the application). With a PAR the integer modes scale the height by N and the width by N times the PAR, floored.
     - The picture is centred in the client area (`--align center`, or `top-left`) with black bars around it, `--bg #rrggbb` colours the bars. Without `--window-size WxH` the initial client area is exactly the picture (x3 for the fit modes).
     - Windows: `--borderless`, `--maximised`, `--fullscreen` (F toggles at run time), `--no-round-corners` (Windows 11: the bottom corner pixels of a normal window are darkened by the rounded corners, this asks DWM not to round them).
     - `--content card|bars|ramp|checker|motion|text|noise|scene|image` (alias `--scene`, default `scene`). `motion` is a moving bar and box on a grid. `--counter` (default on for `motion`, `--no-counter` to disable) draws a 32 bit binary frame number in the top-left corner (a sync cell, 32 cells of at most 8 native pixels, a closing cell; it needs a native width of at least 34). `pixel-demo --decode capture.png <same layout flags>` prints the frame number found in a client-area capture (exit code 1 if none), which gives a capture-based latency and cadence test. `--fixed-seed` makes the `noise` content deterministic (frame N is always the same pixels).
     - `--fps N` (1 to 240, default 60) is the update rate. The loop waits on a high-resolution waitable timer armed for each frame deadline instead of `WM_TIMER` (which was limited to about 57 distinct frames per second), and `--stats` prints the achieved painted rate and skipped frames every second (also in the title). Frames are painted with GDI, so a capture sees at most the DWM composition rate (the monitor refresh rate); during a window drag or resize Windows runs a modal loop and the animation pauses. `--exit-after SECS` quits by itself. The achieved rate is not measured yet on this machine, `scripts\check-pixel-demo.ps1` measures it.
   - `scripts\check-pixel-demo.ps1` exercises the modes with `ultimatecrt --save-capture` and the counter decoder (layouts, resize, rounded corners, optional DPI move, rates).
2. Start the CRT app: `target\release\ultimatecrt.exe`
   - It captures the first window whose title starts with `pixel-demo` (else contains it) and shows it in a second window. Esc closes a windowed output.
   - `--fullscreen` covers the source's monitor with a borderless, topmost, non-activating window. It never takes focus, so quit with Ctrl+Alt+Q. `--monitor N` picks another monitor, `--no-activate` makes a windowed output that never takes focus (Esc then cannot reach it).
   - `--window "text"` captures another window, `--list-windows` and `--list-gpus` list candidates, `--gpu N` picks the Vulkan device.
   - `--divisor N` (or `NxM`) tells it the source is an integer upscale by N, `1` meaning native. `--crop x,y,w,h` selects the part of the client area that is the source. Without `--divisor` the factor is detected automatically.
   - `--no-crop` captures the whole window instead of its client area.
   - `--stats` prints capture and render rates once per second, followed by a GPU line: the mean whole-frame GPU time and one value per timed segment (`upload`, `decode`, every chain slot, `encode`) in ms, from Vulkan timestamp queries (`gpu 3.4 ms | upload 0.097 | decode 0.023 | signal - | amp 0.061 (bypass) | ...`). `-` is a segment that did not run in the second (an off or placeholder slot that keeps its domain, an upload without a new frame); `(bypass)` is an off slot that changes the domain (`amp`, `screen`) and runs the cheap resample copy, so its value is that copy's cost. The numbers are read when a frame slot is reused, so they lag the CPU by at most two frames and never stall it. A device without timestamp support prints `gpu timings unavailable: <reason>` once. `--save-capture out.png` saves the first captured frame and exits, `--exit-after SECS` quits by itself, `--integer` restricts to integer scales, `--validation` enables the Vulkan validation layer.
   - `--log file.csv` writes a diagnostics CSV with one row per second: capture fps, min/max/p99 capture interval, render fps and interval stats, gaps over 50 ms (strictly greater, capture and render counted separately), the number of foreground-window changes in that second, the pid and title of the last foreground window when it changed, and `fg_is_source` (1 when the foreground window is the captured one). The GPU timings are appended after those 15 columns (the earlier columns keep their position): `gpu_frame_ms`, then `gpu_<segment>_ms` for `upload`, `decode`, every chain slot and `encode` (the new chain slots add columns at their place in the chain, so read columns by name), each the mean over the second in ms, empty when the segment did not run or the device has no timestamps. The foreground is polled with `GetForegroundWindow` once per loop (no hooks); the first poll is the baseline, not a change. A summary line is printed at exit. Capture intervals are observed from the main loop (a counter jump of k frames over d ms counts as k intervals of d/k), so their resolution is the render cadence; exact capture timestamps come with T-59. The foreground title can contain private content: delete the log when done.
   - `--pattern name[:args]` shows a generated test pattern instead of capturing a window (no capture target needed, divisor 1): `flat[:level]`, `ramp`, `vline[:N]`, `hline[:N]`, `grating[:N]`, `checker[:N]`, `crosshatch[:N]`, `point`, `window[:APL]` (10, 25, 50, 100 % area on black), `bar[:width[:px-per-frame]]` (moving), `pluge`, `moire[:N]`. `--pattern-size WxH` sets its size (default 320x240). Values are 8-bit sRGB codes.
   - `--image file.png` shows a still PNG (any colour type, alpha flattened over black, divisor 1) instead of capturing. It is exclusive with `--pattern`.
   - `--help` prints all options, `--version` the version.
3. Offline render, no window: `ultimatecrt render (--pattern name[:args] [--pattern-size WxH] | --image f.png) [--set id=value]... [--size WxH] --out out.png|out.pfm`. It runs one frame through the same GPU pipeline headless (default transfer settings and parameters of the live path, `--set` overrides a parameter by id: numbers, `a,b` for vectors, `true`/`false`, enum value names). `--size` is the output size (default: the source size). A `.png` is 8-bit RGB (the codes the output window would show); a `.pfm` is 32-bit float RGB (little-endian, no dependency) holding the same encoded values unquantised, in [0, 1], not linear light. `--validation` enables the validation layer. `--preset <id|file.toml>` applies a tube preset before the `--set` overrides. `render` has no `--gpu` option.

## Tube presets and parameters

- `--preset <id>` (live and `render`) starts from a shipped preset, a user preset (a `.toml` file in `%APPDATA%\ultimateCRT\presets\`, whose `id` replaces a shipped one of the same id) or a preset file path. Shipped ids (`presets\tubes\`): `pvm-20m4u` (Sony PVM-20M4U), `bvm-20f1u` (Sony BVM-20F1U), `kv-trinitron-tv` (Sony KV-class Trinitron TV), `gdm-f520` (Sony GDM-F520 PC monitor), `slot-tv-26in` (26 in slot-mask TV class), `tv-13in` (13 in consumer TV class), `cromaclear-17in` (NEC CromaClear 17 in), `arcade-19in` (19 in arcade monitor class), `vga-15in` (15 in delta-dot VGA class), `delta-17in` (17 in delta-dot monitor class). They are starting points built from published data, each tagged with its source, not measurements of a given unit.
- Every parameter (id, unit, default, range, confidence tag, source, description) is declared in `params\*.toml`, one file per stage. `--set id=value` of `render` takes those ids.
- Without `--preset`, the live app starts from the preset named in `%APPDATA%\ultimateCRT\settings.toml` (else the parameter defaults), with the parameter overrides stored in that file on top.

Only the client area of the target window is captured, so a 256×240 source at 3× arrives as exactly 768×720 pixels.

## Source scaling

The CRT model needs the native raster: scanlines are native rows. A game shown at 3× is captured as 768×720, so the app recovers 256×240 with a divisor:
- **Automatic (default):** over the first 30 informative frames (flat frames such as a black screen are skipped) it looks at the run lengths of identical pixels along rows and of identical rows, and keeps the largest factor that explains nearly all of them. If nothing does, the source is native (x1) and nothing is divided. A margin of 16 pixels is ignored because Windows 11 rounds window corners.
- **Manual:** `--divisor` and `--crop`.
- The shader reads the centre of each block, so no extra pass is needed. Detection restarts when the captured size changes.
- Only simple integer nearest-neighbour upscales are handled. Smoothed or non-integer scaling detects as x1.

## Target lifecycle (T-56)

- **Reattach:** when the captured window closes, the app waits for a new window with the same executable and window class (title preferred, then the largest) and captures it. Only windows that appear after the loss are candidates, or the old handle if it comes back: sibling windows of a shared host (every Chrome window, every UWP app) that were already open are ignored. A sibling that was hidden or minimised when the target closed and is shown later counts as new.
- **Lost source:** the console prints `source lost; Ctrl+Alt+Q to quit` at once and every 10 s. A fullscreen output drops to a normal window (taskbar button, Esc closes it) after `--wait-source SECS` (default 30, 0 waits forever); it does not go back to fullscreen by itself. `--exit-on-lost` exits at once instead. Reattaching keeps working while windowed.
- **Black or silent frames:** `protected or not drawing` in the title and the console after 3 s of black frames or 10 s without a frame (a static picture is silent too).
- **Smoothed source (advisory only):** when no integer scale is found, a warning is printed if the window looks like a low-resolution source upscaled with filtering: most edges between flat areas are ramps of a few pixels (bilinear, sharp bilinear, bicubic, Lanczos, integer or not) and the edges lie on a regular grid (period 1.5 to 16 px). Text at 1:1 has ramps too but no grid, nearest-neighbour scaling has no ramps, so neither warns. It cannot tell apart monospaced text with heavy strokes at 1:1 (its stems repeat at the character pitch, a possible false warning), and it misses an upscale whose flat areas are all shorter than the filter support (for example bicubic or Lanczos x2 of art without runs of about three native pixels: no flat area is left to measure). Thresholds are tuned on synthetic images only (`lifecycle.rs` tests); they have not been checked on real captures.
- **Size change:** a new source size restarts the scale detection and clears the persistence history.
- TODO(T-80b): a capture session lost without the window closing (Direct3D 11 device loss, monitor hot-plug) is not a lost target: the frames stop and only the `protected or not drawing: no new frame` message appears, with no restart. T-80b must restart the capture session.

## Known limits of the scaffold

- Frames go GPU to CPU to GPU (CAP-2, CPU copy path). A shared-handle path is a later spike.
- The Direct3D capture device uses the default adapter and the Vulkan device is chosen separately (NVIDIA discrete by default). On hybrid-graphics laptops this may imply a cross-adapter copy (see HW-3 in `QUESTIONS.md`).
- The window is found by title, not by executable or class.

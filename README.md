# ultimateCRT

A physically based CRT shader for Windows. It captures a window, runs it through a virtual CRT tube on the GPU and shows the result in a separate output window. See `CLAUDE.md` for the vision, `CRT.md` for the physics and `QUESTIONS.md` for decisions and open questions.

Current state: the pipeline duplicates the captured window (passthrough shader). The CRT model comes next.

## Requirements

- Windows 11 x64, a Vulkan 1.3 GPU (developed on an NVIDIA RTX A2000 Laptop)
- Rust 1.98.1 (pinned in `rust-toolchain.toml`, MSVC toolchain)
- Vulkan SDK with `glslc` (`VULKAN_SDK` set, or `glslc` on `PATH`)

Setup, tests, packaging and releases are in `ENV.md`. Architecture is in `ARCHITECTURE.md`, coding rules in `DESIGN.md`.

## Build

```
cargo build --release
```

## Try it

1. Start the demo source: `target\release\pixel-demo.exe`
   - Space cycles the scene, the test card and an optional PNG, 1 to 9 sets the integer scale, Esc quits.
   - `--res WxH` sets another source resolution (for example `--res 160x144`), `--image file.png` shows a pixel-art image, `--scale N` sets the initial scale.
2. Start the CRT app: `target\release\ultimatecrt.exe`
   - It captures the first window whose title starts with `pixel-demo` (else contains it) and shows it in a second window. Esc closes a windowed output.
   - `--fullscreen` covers the source's monitor with a borderless, topmost, non-activating window. It never takes focus, so quit with Ctrl+Alt+Q. `--monitor N` picks another monitor, `--no-activate` makes a windowed output that never takes focus (Esc then cannot reach it).
   - `--window "text"` captures another window, `--list-windows` and `--list-gpus` list candidates, `--gpu N` picks the Vulkan device.
   - `--divisor N` (or `NxM`) tells it the source is an integer upscale by N, `1` meaning native. `--crop x,y,w,h` selects the part of the client area that is the source. Without `--divisor` the factor is detected automatically.
   - `--no-crop` captures the whole window instead of its client area.
   - `--stats` prints capture and render rates, `--save-capture out.png` saves the first captured frame and exits, `--exit-after SECS` quits by itself, `--integer` restricts to integer scales, `--validation` enables the Vulkan validation layer.
   - `--help` prints all options, `--version` the version.

Only the client area of the target window is captured, so a 256×240 source at 3× arrives as exactly 768×720 pixels.

## Source scaling

The CRT model needs the native raster: scanlines are native rows. A game shown at 3× is captured as 768×720, so the app recovers 256×240 with a divisor:
- **Automatic (default):** over the first 30 informative frames (flat frames such as a black screen are skipped) it looks at the run lengths of identical pixels along rows and of identical rows, and keeps the largest factor that explains nearly all of them. If nothing does, the source is native (x1) and nothing is divided. A margin of 16 pixels is ignored because Windows 11 rounds window corners.
- **Manual:** `--divisor` and `--crop`.
- The shader reads the centre of each block, so no extra pass is needed. Detection restarts when the captured size changes.
- Only simple integer nearest-neighbour upscales are handled. Smoothed or non-integer scaling detects as x1.

## Known limits of the scaffold

- Frames go GPU to CPU to GPU (CAP-2, CPU copy path). A shared-handle path is a later spike.
- The Direct3D capture device uses the default adapter and the Vulkan device is chosen separately (NVIDIA discrete by default). On hybrid-graphics laptops this may imply a cross-adapter copy (see HW-3 in `QUESTIONS.md`).
- The window is found by title, not by executable or class.

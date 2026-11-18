# ultimateCRT

## Vision

A universal, physically accurate CRT shader for Windows.
It captures the content of any window (typically 1:1 pixel-art game output), runs it through a virtual CRT tube, and displays the result in a separate output window that can go fullscreen.

## Requirements

### Capture and display
- Runs externally to the captured game or software: no injection, no modification of the target application.
- Captures the content of a chosen window (generally 1:1 pixel-art content).
- Transforms it on the GPU and presents the result in a second window, which can be put fullscreen.
- Presenting or interacting with the output window must not steal focus from the captured application.
- Must work with a single screen: the fullscreen output covers the captured window on the same monitor.
- Input resolution and colour count are arbitrary (the model works on the captured RGB frame).
- Output is SDR only. HDR output and HDR colour are out of scope for now.
- Target hardware: NVIDIA GPU (dev machine: hybrid laptop with Intel UHD plus RTX A2000 Laptop, two Dell P2418D 2560×1440 SDR monitors), Windows 11.
- Mask type is a 3-way choice (aperture grille, slot mask, delta dot). All other look parameters are independent, in physical units. Well-known CRT models (up to 10, typical monitor parameters) ship as starting-point presets, data files only, which the user loads and then edits freely.
- Personal project. No GPL shader code is reused (inspiration only).

### Tunability
- All CRT parameters are tunable live through a menu built with ImGui.

### Performance
- As fast as possible, so fully GPU-based.
- Internal computation in linear light with floating point (FP16, FP32 where needed), output SDR.
- Proposed stack (the author's own guess, not a hard constraint; counter-proposals welcome, to be confirmed by spikes SP-1, SP-2):
  - Graphics API: Vulkan.
  - Shaders: GLSL.
  - Core application: Rust.
  - UI: Dear ImGui.

### Physical accuracy (the differentiator)
- The model is physical, not emulated: the source RGB drives three electron beams of a virtual CRT tube, and each phosphor element emits light in proportion to the beam energy it receives (shorthand: each source RGB pixel becomes real RGB light sources).
- When the host display cannot resolve the phosphor mask, the mask is area-averaged (default) or a coarser, resolvable tube preset is offered.
- Take into account everything that matters physically, including:
  - phosphor cell size and pitch
  - cell group (mask) type, depending on the CRT vendor and tube technology
  - electron beam spot profile, blur and focus
  - glow, halation and bloom
  - screen curvature and geometry
  - scanlines
  - ageing (efficiency loss, spot growth) and burn-in (per-colour wear map, persistent between sessions), opt-in
  - composite video (encode and decode artefacts, optional) and interlace (manual field mode, optional), both off by default
  - anything else physically relevant

## Documents in this folder

- `CLAUDE.md`: this file, the summary of the expressed need.
- `CRT.md`: consolidated knowledge about real-life CRT monitors relevant to this project.
- `QUESTIONS.md`: open questions to resolve before the project starts.
- `README.md`: how to build and run the scaffold.
- `ARCHITECTURE.md`: architecture and architectural decisions (languages, API, modules, data flow, target pipeline).
- `DESIGN.md`: coding rules, design patterns, good practices, known deviations.
- `ENV.md`: how to set up, build, test, package, deploy and publish.
- `CHANGELOG.md`: changes per version (Keep a Changelog, Semantic Versioning, one workspace version).

## Conventions

- **"Publish"** means the full release procedure of `ENV.md` section 7: version update, changelog update, build, test, commit and push, package, and publish as a release on GitHub. Anything shorter (a plain commit, a push) is not a publish.
- Commit or push only when asked. Never publish without being asked.

## Repository layout

Cargo workspace, Windows only. Private GitHub repository `arnaud-sintes/ultimateCRT`.
- `crates/pixel-demo`: demo source app (Win32 and GDI), shows a pixel-art scene or test card at an integer scale. It is the capture target.
- `crates/ultimatecrt`: the main app. `sources.rs` finds the window, `capture.rs` captures it (Windows.Graphics.Capture, CPU copy), `scaling.rs` is the pure integer-upscale detection and crop logic (unit tested), `output.rs` is the Win32 output window, `gpu.rs` is the Vulkan renderer (ash).
- `shaders/`: GLSL, compiled to SPIR-V by `crates/ultimatecrt/build.rs` with `glslc`.
- `scripts/`: `package.ps1` (release zip) and `release.ps1` (GitHub release).
- `rust-toolchain.toml`, `rustfmt.toml`: pinned toolchain and formatting.

## Status

Study phase done, scaffold in place (2026-11-18).
`CRT.md` (physics knowledge base) and `QUESTIONS.md` (open questions, blockers, spikes) are written.
Blocking decisions were answered on 2026-11-18, see "Decisions recorded" in `QUESTIONS.md`.
The scaffold captures a window client area exactly, and duplicates it with Vulkan (passthrough shader) in a windowed or fullscreen non-activating output. No CRT model yet.
Source scaling is handled: manual `--crop` and `--divisor`, and automatic integer-divisor detection (details in `README.md`).
First observations on the dev machine: capture on the NVIDIA GPU works, and a fullscreen output covering the source kept receiving about 60 capture frames per second without taking focus. Not yet a full SP-1 or SP-2 result.
Still open: which GPU drives the monitors (hybrid graphics risk), capture rate on an unfocused or occluded real game, latency.
Next step: first CRT pass in `shaders/` (gun law, beam, area-averaged mask), then the remaining parts of spikes SP-1 and SP-2 against Mina the Hollower.

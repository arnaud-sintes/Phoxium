# ultimateCRT

## Vision

A universal, physically accurate CRT shader for Windows.
It captures the content of any window (typically 1:1 pixel-art game output), runs it through a virtual CRT tube, and displays the result in a separate output window that can go fullscreen.

## Requirements

### Capture and display
- Runs externally to the captured game or software: no injection, no modification of the target application.
- Captures the content of a chosen window (generally 1:1 pixel-art content).
- Transforms it on the GPU and presents the result in a second window, which can be put fullscreen.
- Presenting or interacting with the output window must not steal focus from the captured application. One deliberate exception: the ImGui settings menu (F10 by default, reassignable, inside the output window; F12 is held by another program on the dev machine) takes the focus while it is open (accepted by the owner) and returns it to the source on close. F11 (a default binding, reassignable) toggles fullscreen and a resizable windowed output. The quit chord is fixed.
- Capture uses a simple CPU copy, the most universal path (decided 2026-11-19). A zero-copy path (shared handle) is a later optional performance setting.
- Which GPU runs the whole chain is a setting (`[global.gpu] adapter`), default `auto` = the adapter of the monitor the output covers.
- Must work with a single screen: the fullscreen output covers the captured window on the same monitor, at least when the captured game is not in exclusive fullscreen (conditional GO, 2026-11-19; to be debugged on a real game if it fails).
- Input resolution and colour count are arbitrary (the model works on the captured RGB frame).
- Output is SDR only. HDR output and HDR colour are out of scope for now.
- Target hardware: NVIDIA GPU (dev machine: hybrid laptop with Intel UHD plus RTX A2000 Laptop, two Dell P2418D 2560×1440 SDR monitors), Windows 11.
- Mask type is a 3-way choice (aperture grille, slot mask, delta dot). All other look parameters are independent, in physical units. Well-known CRT models (up to 10, typical monitor parameters) ship as starting-point presets, data files only, which the user loads and then edits freely.
- Personal project. No GPL shader code is reused (inspiration only).

### Tunability
- All CRT parameters are tunable live through a menu built with ImGui.

### Performance
- As fast as possible, so fully GPU-based. 60 fps is a target, not a hard requirement: features come first, optimisation after they work, with quality and performance settings. The 4.2 ms frame-share budget is a goal, not a gate.
- Internal computation in linear light with floating point (FP16, FP32 where needed), output SDR.
- Stack, decided 2026-11-19 after the spikes (the owner's "winner combo"):
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
- **Testing:** run the impact plan (`scripts\impact.ps1 -Run`), not the whole suite, for a task. The full gate (`scripts\check.ps1 -Full`) only before a merge to main, a push or a release, or when unsure (`ENV.md` Test policy).

## Repository layout

Cargo workspace, Windows only. Private GitHub repository `arnaud-sintes/ultimateCRT`.
- `crates/pixel-demo`: demo source app (Win32 and GDI), emulates a game presenting a native surface (1:1, integer, autofit, fit, stretch, bars, presets, test contents, corner markers, frame counter; options and keys, see `pixel-demo --help`). It is the capture target.
- `crates/crt-model`: f64 CPU oracle of the physics (gun, beam, mask, geometry, ABL, phosphor, ageing, burn-in, output, tube, units), used by the GPU tests and the placement code.
- `crates/paramgen`: build-time generator of the Rust and GLSL parameter code from `params/*.toml`.
- `crates/ultimatecrt`: the main app. Modules: `ageing` (ageing and burn-in values for the GPU), `app` (live loop), `capture` (Windows.Graphics.Capture, CPU copy), `cli`, `diag` (`--log`), `gpu/{context, present, headless, pipeline, stages, timing}` (Vulkan renderer, ash), `lifecycle` (lost source, reattach), `output` (Win32 output window), `params` (generated parameter table), `patterns`, `placement`, `presets`, `render_cmd` (offline `render`), `scaling` (pure integer-upscale detection and crop logic), `settings`, `source`, `sources` (window lookup), `tube_state` (persistent burn-in wear map), `white`. Tests in `crates/ultimatecrt/tests/`.
- `shaders/`: GLSL (`*.comp`, `*.frag`, `*.vert`, `include/`), compiled to SPIR-V by `crates/ultimatecrt/build.rs` with `glslc`.
- `params/`: the declarative parameter table, one TOML file per stage.
- `presets/tubes/`: the 10 shipped tube presets (data files).
- `spikes/imgui-spike`: own Cargo workspace, the TEC-5 (T-08) Dear ImGui spike, not part of the product build.
- `scripts/`: `check.ps1` (local gate: the impact plan by default, `-Full` for everything), `impact.ps1` (impact analysis: runs only the tests a change can affect, see `ENV.md` Test policy), `clean-targets.ps1` (shared cargo target folder), `check-capture.ps1` (capture fidelity), `poll-foreground.ps1` (focus checks), `package.ps1` (release zip) and `release.ps1` (GitHub release).
- `rust-toolchain.toml`, `rustfmt.toml`: pinned toolchain and formatting.

## Status

Study phase done (2026-11-18); `CRT.md` (physics knowledge base) and `QUESTIONS.md` (open questions, decisions, spikes) are written.
What exists (see `ARCHITECTURE.md` for the detail): window capture with source scaling and target lifecycle, test patterns and image sources, the full physical chain on the GPU with analytic tests against the `crt-model` oracle (source decode, video amplifier, gun law and white balance, automatic beam limiting, beam with spot growth, geometry and convergence, aperture grille, slot mask and delta dot mask, phosphor saturation and persistence, faceplate optics off by default, output transform), the optional ageing and burn-in modules (off by default; burn-in wear map saved per tube preset), the optional composite video module (NTSC or PAL, composite or S-Video, off by default) and interlace module (manual field mode, off by default), 10 tube presets, a settings file, the offline `render` command, diagnostics (`--log`, GPU timestamps with `--stats`), the check and capture-fidelity scripts, and in code (headless and unit tested, not yet validated live): the ImGui settings menu (F10) with its focus hand-off and the read-only stats overlay (Ctrl+Alt+O), the global hotkeys (configurable, with fallbacks; F11 fullscreen and windowed, presets, A/B bypass, Ctrl+Alt+Q quit) and the output modes (fullscreen and framed window, remembered rectangle).
Also built since: the generated parameter panels of the menu (every parameter live, presets, search, GPU and system panel) with a custom look, the GPU choice setting, a pixel-demo that emulates any presentation mode, and the focus fix for the windowed output. Live-checked on 2026-11-19 with the demo app (menu, hotkeys, focus, GPU selection; see `QUESTIONS.md`).
What does not exist yet: the click-through option, any live run of the new menu panels, ageing, burn-in, optics, composite video and interlace, and any live validation of the chain on a real game (SP-1, SP-2 are only partly answered, see `QUESTIONS.md`).
Decision gate done (2026-11-19): stack kept (Rust, Vulkan, GLSL), CPU-copy capture kept, zero-copy capture deferred as an optional performance setting, GPU choice is a setting, one screen is a conditional GO, 60 fps is a target, first-version scope confirmed (see `QUESTIONS.md`, "Decision gate", and `PLAN.md`, D-19 to D-26). Open and non-blocking: the owner's judgement of the early-look renders.
Next step: a live look at the new menu panels, optics, ageing, burn-in, composite video and interlace, then performance and hardening; the owner tests real games once a stable version exists.

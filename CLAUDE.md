# Phoxium

## Vision

A universal, physically accurate CRT shader for Windows.
It captures the content of any window (typically 1:1 pixel-art game output), runs it through a virtual CRT tube, and displays the result in a separate output window that can go fullscreen.

## Requirements

### Capture and display
- Runs externally to the captured game or software: no injection, no modification of the target application.
- Captures the content of a chosen window (generally 1:1 pixel-art content).
- Single-window product with no command line needed (owner requirement 2026-11-20): the output window is the only window of the app; the window to capture is chosen inside it (a mandatory, manual "Choose a window to capture" picker at start, reachable again from the menu and when the source is lost for longer than the wait), then the automatic handling takes over (resolution, integer upscale, crop, scale modes, centring, surround) with manual overrides in the menu. The command line stays a developer and diagnostic convenience only.
- Transforms it on the GPU and presents the result in a second window, which can be put fullscreen.
- Presenting or interacting with the output window must not steal focus from the captured application. One deliberate exception: the ImGui settings menu (F10 by default, reassignable, inside the output window; F12 was already registered by another program on the development PC) takes the focus while it is open (accepted by the owner) and returns it to the source on close. F11 (a default binding, reassignable) toggles fullscreen and a resizable windowed output. The quit chord is fixed.
- Capture uses a simple CPU copy, the most universal path (decided 2026-11-19). A zero-copy path (shared handle) is a later optional performance setting.
- Which GPU runs the whole chain is a setting (`[global.gpu] adapter`), default `auto` = the adapter of the monitor the output covers.
- Must work with a single screen: the fullscreen output covers the captured window on the same monitor, at least when the captured game is not in exclusive fullscreen (conditional GO, 2026-11-19; to be debugged on a real game if it fails).
- Input resolution and colour count are arbitrary (the model works on the captured RGB frame).
- Output is SDR only. HDR output and HDR colour are out of scope for now.
- Target hardware: NVIDIA GPU (development hardware: hybrid laptop with Intel UHD plus RTX A2000 Laptop GPU, two 2560×1440 SDR monitors), Windows 11.
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

## Documents

Every document except this file is in `docs/`.

- `CLAUDE.md`: this file, the summary of the expressed need.
- `docs/CRT.md`: consolidated knowledge about real-life CRT monitors relevant to this project.
- `docs/QUESTIONS.md`: decisions, open questions, spikes and live-test findings, in the order they were made.
- `docs/README.md`: the short user guide (what it is, download, run, keys, main controls, known limits, unsigned binaries, licence).
- `docs/REFERENCE.md`: the detailed user and developer reference (command-line options, every menu section, hotkeys and fallbacks, GPU choice, capture path, presets and parameters, recipes, optional modules, source scaling, target lifecycle).
- `LICENSE`, `docs/THIRD-PARTY.md`: the MIT licence of the project and the notices of the bundled third-party code and fonts (`deny.toml` is the dependency licence policy).
- `docs/PLAN.md`: the execution plan of the AI-assisted build, kept as provenance of the decisions.
- `docs/ARCHITECTURE.md`: architecture and architectural decisions (languages, API, modules, data flow, target pipeline).
- `docs/DESIGN.md`: coding rules, design patterns, good practices, known deviations.
- `docs/ENV.md`: how to set up, build, test, package, deploy and publish.
- `docs/CHANGELOG.md`: changes per version (Keep a Changelog, Semantic Versioning, one workspace version).

## Conventions

- **"Publish"** means the full release procedure of `docs/ENV.md` section 7: version update, changelog update, build, test, commit and push, package, and publish as a release on GitHub. Anything shorter (a plain commit, a push) is not a publish.
- Commit or push only when asked. Never publish without being asked.
- **Testing:** run the impact plan (`scripts\impact.ps1 -Run`), not the whole suite, for a task. The full gate (`scripts\check.ps1 -Full`) only before a merge to main, a push or a release, or when unsure (`docs/ENV.md` Test policy).

## Repository layout

Cargo workspace, Windows only. GitHub repository `arnaud-sintes/Phoxium`, public from v0.2.0, MIT licence (`LICENSE`).
- `crates/pixel-demo`: demo source app (Win32 and GDI), emulates a game presenting a native surface (1:1, integer, autofit, fit, stretch, bars, presets, test contents, corner markers, frame counter; options and keys, see `pixel-demo --help`). It is the capture target of the live checks; it is not shipped in the release zip (since 0.4.0).
- `crates/crt-model`: f64 CPU oracle of the physics (gun, beam, mask, geometry, ABL, phosphor, ageing, burn-in, optics, composite, interlace, colour, output, tube, units), used by the GPU tests and the placement code.
- `crates/paramgen`: build-time generator of the Rust and GLSL parameter code from `params/*.toml`.
- `crates/appres`: application icon and Windows resources (ICO writer and reader, `.res` writer, PE resource reader); the build scripts of `phoxium` and `pixel-demo` use it, and the `gen_icon` example draws the icons.
- `crates/phoxium`: the main app. Modules: `adapters` (GPU selection, stable adapter identity and LUID mapping), `ageing` (ageing and burn-in values for the GPU), `app` (live loop), `bars` (pure black-bar detection and crop edits), `capture` (Windows.Graphics.Capture, CPU copy), `cli`, `controls`, `diag` (`--log`), `gpu/{context, present, headless, pipeline, stages, timing, ui_render}` (Vulkan renderer, ash; `gpu/stages/` has one file per chain stage), `headroom`, `hotkeys` (global hotkeys), `interlace`, `keyboard_layout` (what AltGr types on the installed layouts), `lifecycle` (lost source, reattach), `live` (settings writes and session state), `migrate` (one-time copy of the old `%APPDATA%\ultimateCRT` settings folder), `modes` (output window modes and rectangle), `output` (Win32 output window), `pacing` (frame pacing on the desktop composition), `params` (generated parameter table), `patterns`, `picker` (source selection logic and state machine), `placement`, `presets`, `quality` (quality tiers), `render_cmd` (offline `render`), `scaling` (pure integer-upscale detection and crop logic), `settings`, `source`, `sources` (window lookup), `tube_state` (persistent burn-in wear map), `ui/{menu, panels, picker, overlay, input, theme}` (ImGui menu, generated parameter panels, picker, stats overlay), `white`. Tests in `crates/phoxium/tests/` (test data in `tests/data`).
- `crates/phoxium/assets/fonts`: the embedded Inter subset (SIL OFL 1.1, `OFL.txt`) and the script that makes it. `assets/icon`: the icon sources and outputs.
- `shaders/`: GLSL (`*.comp`, `*.frag`, `*.vert`, `include/`), compiled to SPIR-V by `crates/phoxium/build.rs` with `glslc`.
- `params/`: the declarative parameter table, one TOML file per stage.
- `presets/tubes/`: the 10 shipped tube presets (data files).
- `spikes/imgui-spike`, `spikes/focus-leak`: own Cargo workspaces, the TEC-5 (T-08) Dear ImGui spike and the T-07 focus-leak spike; not part of the product build.
- `scripts/`: `check.ps1` (local gate: the impact plan by default, `-Full` for everything), `impact.ps1` (impact analysis: runs only the tests a change can affect, see `docs/ENV.md` Test policy), `package.ps1` (release zip), `release.ps1` (GitHub release), `gen-third-party.ps1` (licence texts of the dependencies for the zip), `licence-policy.ps1` (GPL-family filter), `shared-paths.ps1` (worktree and shared target folder, derived from the repository location), `clean-targets.ps1` (shared cargo target folder, with safety checks), and the live checks `check-capture.ps1` (capture fidelity), `check-pixel-demo.ps1`, `check-menu.ps1`, `check-hotkeys.ps1`, `check-focus.ps1`, `focus-monitor.ps1`, `poll-foreground.ps1` (these synthesise input and stop processes, see `docs/DESIGN.md` 11).
- `LICENSE`, `docs/THIRD-PARTY.md` (notices of bundled code and fonts), `deny.toml` (licence policy), `docs/PLAN.md` (the execution plan of the AI-assisted build, kept as provenance of the decisions), `rust-toolchain.toml`, `rustfmt.toml`: licence, notices, pinned toolchain and formatting.

## Status

Study phase done (2026-11-18); decision gate done (2026-11-19: stack kept, CPU-copy capture kept, GPU choice is a setting, one screen is a conditional GO, 60 fps is a target, first-version scope confirmed; see `docs/QUESTIONS.md`). The workspace version is 0.6.0, the fifth public release (0.2.0 to 0.5.0 came before; MIT, unsigned binaries, see `docs/README.md` and `docs/ENV.md` section 7).

What exists (see `docs/ARCHITECTURE.md` for the detail): the single-window flow with no command line (the in-window source picker, Change source and Detach in the menu, ask-at-start and the last choice in the settings, a GUI-subsystem release binary); window capture with source scaling, the picture-area detection (Auto, Manual limits) and the target lifecycle; test patterns and image sources; the full physical chain on the GPU with analytic tests against the `crt-model` oracle (source decode, video amplifier, gun law and white balance, automatic beam limiting, beam with spot growth, geometry and convergence, aperture grille, slot mask and delta dot mask, phosphor saturation and persistence, faceplate optics, output transform); the optional modules, all off by default (ageing and burn-in with the wear map saved per tube preset, composite video, interlace); quality tiers and the automatic quality drop; 10 tube presets; a settings file; the offline `render` command; diagnostics (`--log`, GPU timestamps with `--stats`); the ImGui settings menu with every parameter as a generated panel, presets, search and the GPU and system panel (hotkeys rebound in the menu, with a safety chord for the menu key), preset files (rename, delete, duplicate, export, import), the saved per-game profiles, the interface size, the resets, the About section and the first-run hint, and the read-only stats overlay; the global hotkeys (configurable, with fallbacks) and the output modes (fullscreen and framed window, remembered rectangle); the live-check scripts; and the release tooling (package, release, third-party notices, impact analysis).

Checked live: with the demo app on 2026-11-19 (menu, hotkeys, focus, GPU selection; see `docs/QUESTIONS.md`), and the picture-area detection on two real captures (it failed live twice and was fixed, CAP-6 and CAP-7; the fix is covered by tests on a real frame, not yet re-checked live). Built and measured headless only, not yet checked live: the window picker, the newer menu panels (the hotkey re-registration, the Save and Open dialogs, the clipboard copy and the focus after a dialog in particular), optics, ageing, burn-in, composite video, interlace, the quality tiers and their automatic drop. Built since 0.2.0: zero-copy capture (the default, with an automatic fallback to the CPU copy), glow, vignette and chromatic aberration controls, persistence controls and the click-through option. Not proven: the chain on real games (SP-1 and SP-2 are only partly answered, see `docs/QUESTIONS.md`).

Review pass (2026-11-20): a three-reviewer code review, reconciled into five work packages (public hygiene and release scripts, app loop and persistence, GPU robustness, parameter validation, physical time base); what each one fixes is listed under `[Unreleased]` in `docs/CHANGELOG.md`. After the merge: the integration gate (`scripts\check.ps1 -Full -Smoke` with the GPU required, and one manual windowed `--validation` session), the history rewrite that removes the author e-mail, then the publish of v0.2.0 with the full `docs/ENV.md` section 7 procedure.

Next step: a live look at the picture area (Auto on pixel-demo at x1 and x3 in a large window, resizes, dark scenes, the manual limits), the window picker (focus, previews, minimised windows, two monitors, the first-run hint), the new menu panels (hotkey rebinding with a chord another program owns, the new AltGr-safe default chords and the AltGr warning on a European layout, the GPU choice across a reboot, the preset dialogs, the clipboard), optics, ageing, burn-in, composite video and interlace, then a live check of the quality tiers and the automatic drop, the remaining performance items (phosphor history bandwidth, curved-tube `screen`; `docs/QUESTIONS.md` PRF-1) and hardening; tests on real games once a stable version exists.

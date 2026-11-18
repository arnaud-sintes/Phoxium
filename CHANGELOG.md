# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html) (while below 1.0.0, a minor bump may break things).

Every change goes under `## [Unreleased]` as it is made. Publishing renames that section to the new version and date (see `ENV.md`).

## [Unreleased]

### Added
- Gun law and white balance (`gun` slot, `shaders/gun.comp`, `params/gun.toml`): `gun_law` power (default, 2.4), gog (contrast, brightness, per-gun cutoff and drive), bt1886 (Lw, Lb) and bt1886_annex1; optional white balance from the tube primaries and white xy (`gun_white_balance`, default off). Oracle `crt_model::gun`.
- `scripts/poll-foreground.ps1`: polls the foreground window every 100 ms and logs each change to a CSV (title, pid, executable, is-source), with a summary and a `-SelfTest` switch.
- `--log <file.csv>`: per-second diagnostics (capture and render fps, min/max/p99 intervals, gaps over 50 ms, foreground-window changes, "foreground is the source" flag) and a summary at exit.
- `pixel-demo`: demo source window with a procedural NES-style scene, a test card and optional PNG display, at any source resolution and integer scale.
- `ultimatecrt`: window capture (Windows.Graphics.Capture, client area only, CPU copy) and Vulkan 1.3 output through a passthrough GLSL pass, in a windowed or fullscreen non-activating window. Ctrl+Alt+Q always quits.
- Source integer scaling: `--crop`, `--divisor` and automatic detection (x1 detected as native). Flat frames (a black boot screen) are not counted by the detector.
- `--version` flag on both programs, single workspace version.
- `ultimatecrt --help` prints every option. A bad numeric option value names the flag.
- Pinned toolchain (`rust-toolchain.toml`) and formatting rules (`rustfmt.toml`).
- `scripts/package.ps1` (fmt, clippy, build, tests, licence check, zip with checksum and commit record) and `scripts/release.ps1` (GitHub release from the changelog section, with safety checks).
- Documents: `CLAUDE.md`, `CRT.md`, `QUESTIONS.md`, `ARCHITECTURE.md`, `DESIGN.md`, `ENV.md`, `README.md`.
- Declarative parameter table `params/*.toml` (units, soft ranges, confidence tags, sources) and the `paramgen` build-time generator of the Rust `Params` struct, its metadata and std140 bytes, and the matching GLSL uniform block. First entries: `exposure` and the tube primaries and white point.
- Headless renderer (offscreen RGBA16F/RGBA32F render and readback, no window) and GPU tests run by `cargo test`, failing on any validation message; `UCRT_REQUIRE_GPU=1` makes a missing GPU or validation layer a failure instead of a skip.
- `--validation` prints the layer's messages on stderr through a debug messenger, and names the missing layer when the Vulkan SDK is not installed.
- Pipeline core: an FP16 `decode` stage (compute, native raster read at block centres) and an `encode` stage (explicit host EOTF, black surround around the tube rectangle), two frames in flight with per-frame uniform and staging buffers, images reallocated on source or native size change. Headless identity tests: all 256 codes per channel come back exact through the FP16 raster, FP16 subnormals kept.
- Source decode and tube placement (T-30): parameters `source_eotf` (`drive_voltage` default, or `srgb`: decoded to linear light, then drive voltage = L^(1/tube gamma)), `raster_aspect` (`par` default or `fill_tube`), `source_par`, `line_multiplier` (1 or 2), and the tube `tube_visible_height_mm` (290), `tube_aspect` (4:3) and `tube_height_fraction`. The CPU now fills `px_per_mm` and `line_pitch_mm` of the frame constants (PVM-20M4U class on 2560x1440: 4.966 px/mm, 1.208 mm line pitch). With the defaults the picture is placed exactly as before.

### Changed
- A fullscreen output now refuses to start when the Ctrl+Alt+Q hotkey cannot be registered, and an unknown `--monitor` index is an error (it used to fall back to a default rectangle).
- The output window releases its hotkey and window when dropped.
- The swapchain warns when it has to use a format other than `B8G8R8A8_UNORM`.
- The Esc hint is no longer printed for a `--no-activate` output.
- The output is no longer a plain copy: captured codes are linearised with a pure 2.4 power law (stand-in for the gun law until it exists, SRC-5 default) and encoded for a pure 2.2 host display (COL-3 default), so the picture is slightly darker than the source. GPU tests now also report shader-access hazards (synchronization validation with `syncval_shader_accesses_heuristic`).

# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html) (while below 1.0.0, a minor bump may break things).

Every change goes under `## [Unreleased]` as it is made. Publishing renames that section to the new version and date (see `ENV.md`).

## [Unreleased]

### Added
- `pixel-demo`: demo source window with a procedural NES-style scene, a test card and optional PNG display, at any source resolution and integer scale.
- `ultimatecrt`: window capture (Windows.Graphics.Capture, client area only, CPU copy) and Vulkan 1.3 output through a passthrough GLSL pass, in a windowed or fullscreen non-activating window. Ctrl+Alt+Q always quits.
- Source integer scaling: `--crop`, `--divisor` and automatic detection (x1 detected as native). Flat frames (a black boot screen) are not counted by the detector.
- `--version` flag on both programs, single workspace version.
- `ultimatecrt --help` prints every option. A bad numeric option value names the flag.
- Pinned toolchain (`rust-toolchain.toml`) and formatting rules (`rustfmt.toml`).
- `scripts/package.ps1` (fmt, clippy, build, tests, licence check, zip with checksum and commit record) and `scripts/release.ps1` (GitHub release from the changelog section, with safety checks).
- Documents: `CLAUDE.md`, `CRT.md`, `QUESTIONS.md`, `ARCHITECTURE.md`, `DESIGN.md`, `ENV.md`, `README.md`.
- Declarative parameter table `params/*.toml` (units, soft ranges, confidence tags, sources) and the `paramgen` build-time generator of the Rust `Params` struct, its metadata and std140 bytes, and the matching GLSL uniform block. First entries: `exposure` and the tube primaries and white point.

### Changed
- A fullscreen output now refuses to start when the Ctrl+Alt+Q hotkey cannot be registered, and an unknown `--monitor` index is an error (it used to fall back to a default rectangle).
- The output window releases its hotkey and window when dropped.
- The swapchain warns when it has to use a format other than `B8G8R8A8_UNORM`.
- The Esc hint is no longer printed for a `--no-activate` output.

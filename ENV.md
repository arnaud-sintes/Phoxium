# ENV

How to set up, build, test, package, deploy and publish ultimateCRT. Windows 11 x64 only.

## 1. Prerequisites

| Tool | Version | Notes |
|---|---|---|
| Windows | 11 | Developed on an Insider build (10.0.26300). Windows.Graphics.Capture features depend on the build. |
| Rust | 1.98.1 | Pinned in `rust-toolchain.toml` (MSVC host, with rustfmt and clippy). `rustup` installs it on first use. |
| MSVC build tools | Visual Studio 2022 with "Desktop development with C++" | Needed by the MSVC Rust target. |
| Vulkan SDK | 1.4.x | Provides `glslc` and the validation layer. Set `VULKAN_SDK` (the installer does), or put `glslc` on `PATH`. |
| GPU driver | Vulkan 1.3 capable | Developed on NVIDIA driver 596.71. |
| GitHub CLI `gh` | recent | Logged in (`gh auth status`) with the `repo` scope, only needed to publish. |
| PowerShell | Windows PowerShell 5.1 or later | Runs the scripts in `scripts/`. |

Check the setup:

```
rustc --version
glslc --version
cargo run -p ultimatecrt -- --list-gpus
gh auth status
```

## 2. Build

```
cargo build              # debug, target\debug
cargo build --release    # optimised, target\release
```

`crates/ultimatecrt/build.rs` compiles `shaders/*.vert` and `*.frag` with `glslc` (from `%VULKAN_SDK%\Bin`, else `PATH`). If it fails with "cannot run glslc", install the Vulkan SDK or set `VULKAN_SDK`.

On Windows an `.exe` cannot be replaced while it runs: close `pixel-demo` and `ultimatecrt` before rebuilding ("Access is denied" on the exe).

## 3. Static checks and tests

```
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```

`cargo test` runs the unit tests (scaling, patterns, sources, diagnostics, device choice) and the headless GPU tests (`crates/ultimatecrt/tests/gpu_*.rs`, no window is opened). On the dev machine, run them with the GPU required:

```
$env:UCRT_REQUIRE_GPU = '1'; cargo test
```

- The GPU tests create a surface-less Vulkan 1.3 device, run the pipeline into an offscreen RGBA16F, RGBA32F or BGRA8 image and read it back. When `VK_LAYER_KHRONOS_validation` is installed (Vulkan SDK) it is enabled with synchronization validation (including the accesses shaders make through descriptors, `syncval_shader_accesses_heuristic` set through `VK_EXT_layer_settings`: without it layer 1.4.363 reports no hazard between shader stages), every message is printed on stderr, and any validation or performance warning or error fails the test.
- Without a Vulkan 1.3 device a GPU test prints `<test> skipped: ...` (visible with `cargo test -- --nocapture`) and passes. With `UCRT_REQUIRE_GPU=1`, a missing device or a missing validation layer fails the test instead (a count of 0 proves nothing without the layer).
- `gpu_passthrough` checks that every 8-bit code k of a BGRA8 ramp reads back with `round(v * 255) == k`. FP16 cannot hold k/255 exactly, and the NVIDIA driver converts to FP16 toward zero (254 reads 253.879), so the FP16 tolerance is one FP16 step. It runs through the pipeline with identity transfers, so the FP32 target carries the FP16 error of the native raster too.
- `gpu_identity` (T-20a) runs decode then encode with the same transfer on both sides (pure 2.4, pure 2.2, sRGB, identity): every code comes back exact on an RGBA16F and a BGRA8 target, the native raster matches the `crt-model` oracle within FP16 precision, and the FP16 subnormals of the dark codes (1 to 4 at gamma 2.4) are not flushed to zero. It also changes the native size (256x240, 320x240, back), decodes a 2x upscale at block centres, checks the black surround, and submits a burst of frames that overlap on the GPU (two frames in flight) so synchronization validation sees the cross-frame accesses.
- Software Vulkan for a future CI (report only, unverified on Windows): Mesa lavapipe (llvmpipe Vulkan driver, Vulkan 1.3; Windows builds exist from third parties such as mesa-dist-win) and Google SwiftShader are candidates. Installing either as an ICD (`VK_DRIVER_FILES` pointing at its JSON manifest) would let a GPU-less runner run these tests with `UCRT_REQUIRE_GPU=1`. Neither has been tried here; check its Vulkan 1.3, dynamic rendering and synchronization2 support and its licence first.

`scripts/check.ps1` runs all of the above in one go (fmt, clippy `-D warnings`, tests with `UCRT_REQUIRE_GPU=1`, licence check) and exits non-zero on the first failure. `-Smoke` adds a 10 s live run (`--pattern ramp --exit-after 10 --validation`, opens a window) that fails on any validation line on stderr. The licence check runs `cargo deny check licenses` against `deny.toml` when cargo-deny is installed (`cargo install cargo-deny --locked`), else warns, and always runs the declared-licence check shared in policy with `package.ps1`.

There is no automated test of capture or output, and no CI yet: a hosted runner has no GPU and cannot capture windows.

### Manual end-to-end test

1. Start the source: `target\release\pixel-demo.exe` (`--scale 3`, `--res 160x144`, `--scene card`, `--image file.png`; Space cycles content, 1 to 9 sets the scale, Esc quits).
2. Start the CRT app: `target\release\ultimatecrt.exe --stats --validation`.
3. Expect: a second window duplicating the demo, `render` near the display refresh rate, a line `source scaling: detected x3/x3, native 256x240` (or `native (x1)` at scale 1), and no validation messages.

Useful variants:
- `--fullscreen` covers the source's monitor and never takes focus. Quit with `Ctrl+Alt+Q`. Use `--exit-after 10` on the first tries so it ends by itself.
- `--save-capture out.png` saves the first captured frame (check that the client area is exact, for example 768Ã—720 for a 256Ã—240 source at 3Ã—).
- `--list-windows`, `--list-gpus`, `--gpu N`, `--window "text"`, `--divisor N`, `--crop x,y,w,h`, `--integer`.

Tips: launch the demo and the CRT app from the same kind of shell. When a program is started with `Start-Process` from PowerShell, the console window it opens can carry the exe path in its title and be picked as the target. The target match prefers titles that start with the text, so `pixel-demo |` wins. Do not take full-desktop screenshots when checking output (see `DESIGN.md`, testing).

## 4. Versioning

- One version for the whole workspace: `[workspace.package] version` in the root `Cargo.toml`. Both programs print it with `--version`.
- Semantic Versioning. While below 1.0.0, a minor bump may break behaviour. Use a patch bump for fixes, a minor bump for features.
- Release tags are `vX.Y.Z`. `CHANGELOG.md` follows Keep a Changelog: every user-visible change is added under `## [Unreleased]` when it is made.

## 5. Package

```
.\scripts\package.ps1
```

Run it on a committed (clean) tree. It reads the version from `[workspace.package]`, runs `cargo fmt --all --check`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo build --release --locked` and `cargo test --release --locked`, checks the declared licences of the dependencies (a missing licence or a GPL one fails), then creates in `dist\` (ignored by git):
- `ultimatecrt-vX.Y.Z-windows-x64.zip` with one top-level folder `ultimatecrt-vX.Y.Z-windows-x64\` containing `ultimatecrt.exe`, `pixel-demo.exe`, `README.md`, `CHANGELOG.md`, `THIRD-PARTY.md` (dependency licences) and `RUNNING.txt` (how to start);
- `ultimatecrt-vX.Y.Z-windows-x64.zip.sha256` (`<hash>  <file>`, ASCII, LF);
- `ultimatecrt-vX.Y.Z-windows-x64.commit`, the commit the zip was built from, so `release.ps1` can refuse a stale zip.

`-SkipTests` (skips fmt, clippy and tests) and `-AllowDirty` exist for local trials only, never for a publish. A zip built with `-AllowDirty` is recorded as `<hash>-dirty` and `release.ps1` refuses it.

## 6. Deploy

Deployment is a GitHub release of the zip on the private repository `arnaud-sintes/ultimateCRT`. There is no installer: unzip and run. The executables are not code-signed, so Windows SmartScreen and antivirus tools may warn on first run.

```
.\scripts\release.ps1 -DryRun   # runs every check and prints the notes
.\scripts\release.ps1           # creates tag vX.Y.Z and the release with the zip and checksum
```

`release.ps1` refuses to run when `gh` is not logged in, off the `main` branch, on a dirty tree, with a HEAD that is not pushed, with a zip built from another commit than HEAD (or one that does not match its checksum), when the tag exists locally or on `origin`, when the release exists, or without a changelog section with at least one entry for the version. The notes of the release are that changelog section. Any native command that fails stops the script.

### Recovery

`gh release create` creates the tag and the release together. If it fails half way:
- check `gh release view vX.Y.Z` and `git ls-remote --tags origin vX.Y.Z`;
- a release that exists but lacks files: `gh release upload vX.Y.Z dist\<file>`;
- a wrong release to redo: `gh release delete vX.Y.Z --cleanup-tag` (deleting is the owner's decision), then run `release.ps1` again;
- a wrong version already released is never reused: fix forward with the next patch version.

## 7. Publish

"Publish" is the full release procedure, in this order:

1. **Version update:** bump `[workspace.package] version` in `Cargo.toml` (not needed if the current version was never released), then `cargo build` to refresh `Cargo.lock`.
2. **Changelog update:** rename `## [Unreleased]` to `## [X.Y.Z] - YYYY-MM-DD`, add a new empty `## [Unreleased]` above it, and check that the entries cover every user-visible change since the last release (`git log`). Update `README.md` and `ARCHITECTURE.md` if behaviour changed.
3. **Build:** `cargo build --release`.
4. **Test:** `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (the packaging step repeats them), plus the manual end-to-end test of section 3 if the capture, output or Vulkan code changed.
5. **Commit and push:** commit as `Release vX.Y.Z` on `main` and push it. The package must be built from this commit.
6. **Package:** `.\scripts\package.ps1`.
7. **Release:** `.\scripts\release.ps1 -DryRun`, then `.\scripts\release.ps1`.

If a step fails, stop and fix it. A fix after step 5 needs a new commit, a push and a new package.

Check the result with `gh release view vX.Y.Z`.

## 8. Troubleshooting

| Symptom | Cause and fix |
|---|---|
| `cannot run glslc` during build | Vulkan SDK missing or `VULKAN_SDK` unset. |
| `Access is denied` replacing `target\...\*.exe` | A program is still running. Close it (`taskkill /F /IM pixel-demo.exe`). |
| `no visible window with "..." in its title` | The target is minimised or has another title. Use `--list-windows` and `--window`. |
| `no frame captured within 5 seconds` | The window is not drawing, is protected content, or capture is blocked for it. Moving or redrawing the window often helps. |
| `no Vulkan 1.3 device can present to the output window` | Update the GPU driver, or pick a device with `--gpu` (see `--list-gpus`). |
| Output window does not respond to the keyboard | By design in fullscreen: quit with `Ctrl+Alt+Q`. |
| `cannot register the Ctrl+Alt+Q quit hotkey` | Another program owns that shortcut. Close it, or use a windowed output (Esc quits). |
| `no such monitor` | `--monitor` index out of range: see the monitor lines of `--list-windows`. |
| `cargo clippy` or `fmt` fails during packaging | Fix the warning or run `cargo fmt --all`, commit, package again. |
| Capture reports few frames per second on a static window | Windows.Graphics.Capture only delivers a frame when the window content changes. |

### Foreground poll (focus checks)

`scripts/poll-foreground.ps1` polls `GetForegroundWindow` every 100 ms (no hook) and writes one CSV row per foreground change (plus a baseline row): `timestamp,title,pid,exe,is_source`. It prints the number of changes and the share of time the source was in the foreground at the end.

```powershell
.\scripts\poll-foreground.ps1 -SourceTitle "<game title>" -OutFile .\poll.csv -DurationSeconds 600   # 0 = until Ctrl+C
.\scripts\poll-foreground.ps1 -SelfTest                                                               # 3 s, prints PASS or FAIL
```

`-SourceTitle` is a case-insensitive title substring, `-IntervalMs` defaults to 100. Window titles can hold private content (documents, chats): delete the CSV after the run.

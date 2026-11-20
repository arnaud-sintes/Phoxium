# ENV

How to set up, build, test, package, deploy and publish ultimateCRT. Windows 11 x64 only.

## 1. Prerequisites

| Tool | Version | Notes |
|---|---|---|
| Windows | 11 | Developed on an Insider build (10.0.26300). Windows.Graphics.Capture features depend on the build. |
| Rust | 1.98.1 | Pinned in `rust-toolchain.toml` (MSVC host, with rustfmt and clippy). `rustup` installs it on first use. |
| MSVC build tools | Visual Studio 2022 with "Desktop development with C++" | Needed by the MSVC Rust target, and its C++ compiler (`cl.exe`) by `dear-imgui-sys`, which compiles Dear ImGui and cimgui from source for the settings menu (T-52a; about 16 s in release, once per target folder). |
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

`crates/ultimatecrt/build.rs` compiles `shaders/*.vert`, `*.frag` and `*.comp` with `glslc` (after `crates/paramgen` has generated `params.glsl` from `params/*.toml`) (from `%VULKAN_SDK%\Bin`, else `PATH`). If it fails with "cannot run glslc", install the Vulkan SDK or set `VULKAN_SDK`.

On Windows an `.exe` cannot be replaced while it runs: close `pixel-demo` and `ultimatecrt` before rebuilding ("Access is denied" on the exe).

**Disk space:** a Cargo `target` folder (debug plus release, with the test binaries) takes about 1 to 3 GB, and every git worktree or spike workspace has its own. Build with `$env:CARGO_INCREMENTAL = '0'` (no incremental cache, which is a large part of that size) and delete the `target` folders of worktrees and of `spikes\imgui-spike` when done (`Remove-Item -Recurse -Force target`), or share one with `CARGO_TARGET_DIR` (`scripts\impact.ps1` and `check.ps1` do, see Test policy).

## 3. Static checks and tests

```
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```

`cargo test` runs the unit tests (scaling, patterns, sources, diagnostics, lifecycle, presets, settings, placement, device choice) and the headless GPU tests: `crates/ultimatecrt/tests/gpu_*.rs`, the analytic stage tests `stage_*.rs` against the `crt-model` oracle, `pipeline_skip.rs`, `presets.rs` and `render_cmd.rs` (no window is opened). On the dev machine, run them with the GPU required:

```
$env:UCRT_REQUIRE_GPU = '1'; cargo test
```

- The GPU tests create a surface-less Vulkan 1.3 device, run the pipeline into an offscreen RGBA16F, RGBA32F or BGRA8 image and read it back. When `VK_LAYER_KHRONOS_validation` is installed (Vulkan SDK) it is enabled with synchronization validation (including the accesses shaders make through descriptors, `syncval_shader_accesses_heuristic` set through `VK_EXT_layer_settings`: without it layer 1.4.363 reports no hazard between shader stages), every message is printed on stderr, and any validation or performance warning or error fails the test.
- Without a Vulkan 1.3 device a GPU test prints `<test> skipped: ...` (visible with `cargo test -- --nocapture`) and passes. `UCRT_REQUIRE_GPU` is read by `gpu_required()`: any non-empty value except `0` counts as set (`1`, `true`; empty or `0` means not required). When set, a missing device, a missing validation layer or missing synchronization validation fails the test instead (a count of 0 proves nothing without the layer). Never set it on a machine without a GPU.
- `gpu_passthrough` checks that every 8-bit code k of a BGRA8 ramp reads back with `round(v * 255) == k`. FP16 cannot hold k/255 exactly (254/255 is not an FP16 value). The Vulkan spec leaves the rounding of a float to FP16 conversion to the implementation, and the NVIDIA driver converts toward zero (254 reads 253.879, a bias of -2^-10 to 0 per store, different on other vendors), so since T-fp16 every chain stage rounds its value itself before `imageStore` (`round_f16` in `shaders/include/common.glsl`: round to nearest, ties to even, exact under any hardware rounding mode, +-inf from 65520 up, NaN and inf unchanged) and the chain images are the nearest FP16 values on any device; a new stage that stores into an `rgba16f` image must call it. `gpu_fp16_store` checks it bit for bit against a CPU reference for 138765 awkward values (ties, subnormals, 65504, tiny values, every k/255) through `abl`, `gun`, `amp`, `screen` and the real `decode`. What is still device defined is the conversion of the `encode` output into an RGBA16F test target (not a chain store; truncating on this device, below 255 * 2^-11 = 0.1245 of a code), so the RGBA16F identity tolerance is 0.32 of a code (derivation in `gpu_identity.rs`) and the BGRA8 one is exact. The test runs through the pipeline with identity transfers, so the FP32 target carries the FP16 error of the native raster too (now at most half an FP16 step).
- `gpu_identity` (T-20a) runs decode then encode with the same transfer on both sides (pure 2.4, pure 2.2, sRGB, identity): every code comes back exact on an RGBA16F and a BGRA8 target, the native raster matches the `crt-model` oracle within FP16 precision, and the FP16 subnormals of the dark codes (1 to 4 at gamma 2.4) are not flushed to zero. It also changes the native size (256x240, 320x240, back), decodes a 2x upscale at block centres, checks the black surround, and submits a burst of frames that overlap on the GPU (two frames in flight) so synchronization validation sees the cross-frame accesses.
- Software Vulkan for a future CI (report only, unverified on Windows): Mesa lavapipe (llvmpipe Vulkan driver, Vulkan 1.3; Windows builds exist from third parties such as mesa-dist-win) and Google SwiftShader are candidates. Installing either as an ICD (`VK_DRIVER_FILES` pointing at its JSON manifest) would let a GPU-less runner run these tests with `UCRT_REQUIRE_GPU=1`. Neither has been tried here; check its Vulkan 1.3, dynamic rendering and synchronization2 support and its licence first.

`scripts/check.ps1 -Full` runs all of the above in one go (fmt, clippy `-D warnings`, tests with `UCRT_REQUIRE_GPU=1`, licence check) and exits non-zero on the first failure. `-Smoke` adds a 10 s live run (`--pattern ramp --exit-after 10 --validation`, opens a window) that fails on any validation line on stderr. The licence check runs `cargo deny check licenses` against `deny.toml` when cargo-deny is installed (`cargo install cargo-deny --locked`), else warns, and always runs the declared-licence check shared in policy with `package.ps1`.

There is no automated test of capture or output, and no CI yet: a hosted runner has no GPU and cannot capture windows.

### Test policy

Running the whole gate (fmt, clippy, 700 tests in about 30 test programs, many on the GPU; the tests took about 7 minutes alone before 2026-11-20, about 70 s since, see Test cost below) after every change is wasted time when a change touches one module. `scripts\impact.ps1` analyses the change and runs only what it can affect.

| Gate | When |
|---|---|
| The impact plan: `scripts\impact.ps1 -Run` (or `scripts\check.ps1`, which calls it by default) | Every task and every commit. The plan is the gate of the task. |
| The full gate: `scripts\check.ps1 -Full` (or `scripts\impact.ps1 -Full -Run`) | Before a merge to `main`, before a push, before a release (`package.ps1` and `release.ps1` run their own full checks), when the plan says FULL, and whenever you are unsure. |

How to use it:

```
.\scripts\impact.ps1                    # prints the plan only: commands, cost class, reasons
.\scripts\impact.ps1 -Explain           # also why each test program was selected
.\scripts\impact.ps1 -Run               # executes the plan; stops at the first failure (-KeepGoing: runs on)
.\scripts\impact.ps1 -Files a.rs,b.glsl # plan for an explicit list of files
.\scripts\impact.ps1 -Rev <commit>      # plan for one commit (what a merge brought in)
.\scripts\impact.ps1 -Full -Run         # the whole gate
.\scripts\impact.ps1 -SelfTest          # fixture tests of the model itself
.\scripts\impact.ps1 -Validate 5        # plans of the last 5 first-parent commits plus a miss check
```

The changed files are the committed changes since the merge base with `exec/integration` (else `HEAD~1`; on a clean `exec/integration` that is the last commit), plus uncommitted and untracked files. `-Base <ref>` overrides the base.

What the model decides (derived from the repository at every run, nothing hard-coded except the package names and the `gpu/stages` layout):

| Change | Runs |
|---|---|
| Docs (`*.md`, `*.txt`), `.gitignore`, an asset recipe | Format check only. |
| `scripts\*.ps1` | Format check and a PowerShell parse of the changed scripts. No cargo tests. |
| `crates/pixel-demo/**` | `cargo test -p pixel-demo` and clippy on it. Integration tests of `ultimatecrt` only when `options.rs` changed and a test references pixel-demo; the live `scripts\check-*.ps1` are never run by the plan. |
| `crates/crt-model/src/<module>` | `cargo test -p crt-model`, every `ultimatecrt` test file referencing `crt_model::<module>` (or a crt-model module that uses it, depth 2), and the unit tests of the `ultimatecrt` modules that do. |
| `crates/ultimatecrt/src/<module>` | The module's unit tests (`cargo test -p ultimatecrt --lib -- <module>::`), every integration test referencing `ultimatecrt::<module>`, and the dependants found by the `use crate::` graph (depth 2, marked "transitive" in the reasons). |
| `crates/ultimatecrt/src/gpu/stages/<stage>.rs` | The stage's unit tests, the tests naming the stage (`"<stage>"` in `enable_only`/`stage(...)`; an `after: "<stage>"` input injection does not count) and the whole-chain tests (those that render without a stage filter). |
| `shaders/*.comp, *.vert, *.frag` | The same as the owning stage (the stage Rust file holding `<shader>.spv`). |
| `shaders/include/*.glsl` | Every shader that `#include`s it, transitively, then as above, plus the unit tests that read shader sources. |
| `params/*.toml` | `cargo check --workspace --all-targets` (the table is regenerated), the `params`, `settings` and `presets` unit tests, `tests/presets.rs`, and the tests naming a changed parameter id (from the diff: only the changed blocks). |
| `crates/paramgen/**` | `cargo test -p paramgen`, `cargo check --workspace`, the params-related tests. |
| `presets/tubes/*.toml`, assets (fonts) | The module that embeds the file (`include_str!`/`include_bytes!`), the tests referencing it, the tests iterating over the presets. |
| `Cargo.toml`, `Cargo.lock`, `build.rs`, `rust-toolchain.toml`, `rustfmt.toml`, `deny.toml`, `src/lib.rs` | FULL (all of `ultimatecrt` for `lib.rs`). |
| A file kind the model cannot place | FULL, with the reason printed. |

Always: `cargo fmt --all -- --check`. Clippy (`--all-targets -- -D warnings`) runs only on the packages that own a changed Rust file plus the workspace crates that depend on them. The GPU tests it selects run with `UCRT_REQUIRE_GPU=1`.

Reading the plan: each line is `[cost] command`. Cost classes: `cheap` (no GPU), `GPU` (headless Vulkan test), `slow` (clippy, full-chain renders, tests that spawn the binary or render the presets, tests over 900 lines, and any GPU test measured above 20 s). `-Run` records the run time of every integration test it executes in `impact-timings.json` next to the shared target folder; the plan then classifies by the measurement and prints the measured seconds (on 2026-11-19 `ui_menu` took 160 s of the 6 min 43 s full run). The last lines give "Test programs selected: N of M" and the list of programs not run ("skipped by impact analysis"). M counts the integration test files plus one unit-test target per package. `-Explain` lists the reasons per program. The selection is deliberately conservative: a test that merely names a stage or module is included, and a widely used module (`params`, `gpu`) pulls in most tests, as it should.

Limits: the model is textual (module paths, quoted stage names, shader names, parameter ids), not a type analysis. It cannot see a test that reaches a module only through a re-export from another crate item it does not name, or behaviour shared through a data file it does not know. When a change is risky, or the plan looks too small, run `-Full`.

Shared build cache: `impact.ps1` and `check.ps1` set `CARGO_TARGET_DIR` to `C:\projects\personal\ultimateCRT-wt\target-shared` when it is not already set, and `CARGO_INCREMENTAL=0`. Every agent and worktree then reuses the compiled external dependencies (`windows`, `ash`, and the Dear ImGui C++ sources built by `dear-imgui-sys`, about 16 s in release) instead of building them cold. The workspace crates themselves rebuild once per worktree path, which is expected. Cargo takes a file lock on the target folder, so parallel cargo runs on it wait for each other (the "Blocking waiting for file lock" line) instead of corrupting it. Do not delete the shared folder after a task; `scripts\clean-targets.ps1` prints the sizes of the shared and per-worktree `target` folders and deletes the shared one only when the free disk space is under 30 GB (or with `-Force`, and never while cargo runs). An `.exe` in the shared folder cannot be replaced while it runs, as with any target folder.

#### Test cost: normal tier and slow tier

Tiers. The normal tier is every test `cargo test` runs: the gate. The slow tier is the `#[ignore = "..."]` tests, run on purpose with a documented command, never silently dropped. A test goes to the slow tier only when it is redundant with a normal-tier test or a pure long-run measurement or stress run. Today the slow tier is the three GPU-time profiles (print a table, assert at most a cost ratio), unchanged by the 2026-11-20 work:

```
cargo test -p ultimatecrt -- --ignored --nocapture        # the whole slow tier (UCRT_REQUIRE_GPU=1 on the dev machine)
```

(`stage_beam_fast::screen_profile`, `stage_beam_fast::screen_profile_geometry`, `stage_beam_warp::screen_profile_warp`; `UCRT_PROFILE_FRAMES` sets their frame count.) No test was moved to the slow tier by the speed-up, and `presets::all_ten_presets_render_finite_images` left it (about 1 s now).

Budget: the whole normal tier at most about 3 minutes alone, `ui_menu` at most about 40 s, no single normal-tier test above about 20 s. A new test above that budget either gets cheaper (below) or argues its case for the slow tier in its `ignore` reason.

What makes the suite fast (2026-11-20, owner decision "the tests poison the workflow"), and what to keep doing in new tests:

- The `test` profile optimises the CPU-heavy packages (`Cargo.toml`: `crt-model` opt-level 3, `ultimatecrt` 2, the PNG encoder crates 3); debug assertions and overflow checks stay on. The GPU tests were CPU-bound in debug code (f64 oracles over millions of texels, the f16 to f32 readback conversion, per-texel checks in the test files, the PNG encoder of the menu previews). Cost: `cargo build` no longer shares the test build's `ultimatecrt`, a change to the `ultimatecrt` library rebuilds its 27 integration test programs at opt-level 2 (about 60 s instead of about 20 s on the dev machine, measured under load), a change to one test file about 40 s instead of a few seconds. Measured alternatives: `ultimatecrt` at opt-level 0 left `ui_menu` at 137 s and the suite at 321 s; at opt-level 1 the suite took 131 s (`ui_menu` 32 s) for about 42 s of rebuild.
- Read back only what the test looks at: `Headless::set_read_stages(StageReadback::None)` (output only) or `StageReadback::Last` (output and the encode input). By default every routed chain image is read back, converted to `f32` and cloned once per slot that routes it: at 1280 x 1080 that is about 8 million texels and 160 ms of CPU per frame, against 6 ms of GPU.
- A UI test renders the UI over a bypassed chain (`enable_only(&[])`), uploads its background once, and reads back the output only; tests about what the chain shows (`a_slider_changes_its_parameter_and_the_rendered_frame`, the previews) turn the chain back on.
- A per-texel oracle over a full-size image is split over threads (`presets.rs`, `par_sums`): the same arithmetic per texel, the partial sums added in order.
- Use the stage's own contract instead of long loops to reach a state: `clear_history` then one black frame primes the phosphor history to 0, where a flush took a few hundred frames per preset (the flush to exactly 0 is still tested once, `stage_phosphor::step_response`).
- Not done, by measurement: one shared `Headless` per test program. Creating the context and pipeline with validation costs about 0.1 to 0.2 s (the smallest GPU tests take 0.2 s in all), the test threads of one program run in parallel on their own contexts, and a shared renderer behind a lock would serialise them and couple their validation counts. cargo-nextest (one process per test, all at once) would add GPU contention on the one shared GPU without saving work: not adopted.

Measured on the dev machine (RTX A2000 Laptop, validation and synchronization validation on, `UCRT_REQUIRE_GPU=1`), each test program run once, one program at a time, its tests in parallel as `cargo test` runs them; other agents were building and using the GPU at the same time, so take the after column as a range of two runs (s):

| Program | Before | After |
|---|---|---|
| `ui_menu` | 952.4 | 15.2 to 20.4 |
| `presets` | 146.8 | 7.8 to 8.6 |
| `stage_beam_fast` | 19.1 | 4.5 to 4.6 |
| `stage_beam_warp` | 18.3 | 3.9 to 4.2 |
| `stage_beam_vertical` | 16.4 | 3.3 to 3.6 |
| `stage_phosphor` | 11.3 | 2.8 to 2.9 |
| `crt-model` unit tests | 10.3 | 2.1 to 3.0 |
| `stage_beam_horizontal` | 9.3 | 2.7 |
| `stage_mask_grille` | 7.6 | 1.8 to 2.0 |
| `stage_mask_slot_delta` | 7.3 | 3.4 to 3.6 |
| `gpu_timings` | 5.0 | 1.0 to 1.1 |
| `pipeline_skip` | 4.6 | 2.2 to 2.3 |
| `stage_abl` | 4.2 | 1.5 |
| the 17 other programs | 20.5 | 13.9 to 14.7 |
| **Total (30 programs)** | **1233** | **67 to 74** |

Before, `ui_menu` alone was about 160 s on an idle GPU and 950 s under contention; after, its 15 tests take 15.6 s in sequence, the slowest 3.1 s (`menu_preview_images`). The slowest normal-tier test is now `presets::flat_white_through_the_chain_in_every_headroom_mode` (8.3 s: 50 full-chain renders at 2560 x 1440 and their oracle). Per-test times: `-Z unstable-options --report-time --format json` on a test binary, with `RUSTC_BOOTSTRAP=1` set for that run only (stable toolchain).

### Scripts (`scripts/`)

| Script | Use |
|---|---|
| `check.ps1` | The local gate. By default it runs the impact plan of `impact.ps1` (Test policy above); `-Full` runs everything below (the previous behaviour). `-Smoke` adds the 10 s live run, `-SkipTests` skips `cargo test` (full gate). |
| `impact.ps1` | Impact analysis: prints or runs the checks and tests a change needs (Test policy above). `-Full`, `-Explain`, `-Run`, `-DryRun`, `-KeepGoing`, `-Files`, `-Rev`, `-Validate`, `-SelfTest`. |
| `clean-targets.ps1` | Sizes of the shared and per-worktree `target` folders; deletes the shared one only when free disk < 30 GB or with `-Force`. |
| `check-capture.ps1` | Capture fidelity (T-12, CAP-3): runs `pixel-demo --image` at several integer scales, saves a capture with `ultimatecrt --save-capture` and compares it with the known pixels. Parameters `-TargetDir` (default `..\target`, needs release builds of both programs), `-Scales`, `-Occlude`, `-MoveTo "x,y"`, `-KeepFiles`. Opens windows: hands off the keyboard and mouse. |
| `poll-foreground.ps1` | Foreground-window poll for focus checks, see "Foreground poll" below. |
| `package.ps1`, `release.ps1` | Release, sections 5 and 6. |
| `spikes\imgui-spike\scripts\run-t08-live.ps1` | Live run of the T-08 ImGui spike (F12 menu focus hand-off, F11 toggle) in windowed then fullscreen mode, unattended: it synthesises key presses and moves the foreground window, so it needs a hands-off desktop (about 2 minutes) and must not run with another live task. The spike is its own workspace (own `target`, see the disk-space note). |

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

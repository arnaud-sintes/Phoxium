# DESIGN

Coding rules, patterns and practices for Phoxium. Architecture is in `ARCHITECTURE.md`, build and release in `ENV.md`.

Rules marked **(checked)** are verified by a tool, but there is no continuous integration yet: the check runs when someone runs it, and `scripts/package.ps1` runs it before every package. The others are conventions to follow in review. Section 12 lists where the code does not yet follow them.

## 1. Toolchain and static checks

- Rust 2021 edition, the toolchain is pinned in `rust-toolchain.toml` (rustfmt and clippy included). **(checked: rustup installs that version)**
- `cargo fmt --all` with `rustfmt.toml` (`max_width = 120`). **(checked: `cargo fmt --all --check`, run by `package.ps1`)**
- `cargo clippy --all-targets -- -D warnings` must be clean. **(checked, run by `package.ps1`)** An `#[allow(...)]` needs a one-line reason next to it (convention).
- `Cargo.lock` is committed. Packaging builds with `--locked`, which fails if the lock file is out of date. **(checked)**

## 2. Module design

- One responsibility per module (see the table in `ARCHITECTURE.md`). Operating-system code stays in `capture`, `output`, `sources` and `gpu`.
- Pure logic lives in modules with no OS or GPU calls (`scaling`) so it can be unit tested on any machine.
- Public items are the minimum other modules need. Fields are private unless the type is plain data (`Frame`, `View`, `Rect`).
- Graphics code does not know about windows beyond a handle and a client size. Capture does not know about Vulkan. Keep it that way so a second capture or present backend can be added behind an interface (TEC-1).

## 3. Unsafe code

- `unsafe` only where an FFI call or a raw pointer requires it (`windows`, `ash`, mapped memory). Keep each block as small as practical.
- Every `unsafe impl` (such as `Send for Worker`) carries a comment stating the invariant that makes it sound.
- Do not hide `unsafe` behind a safe function that can be misused. If a function has preconditions that the compiler cannot check, mark it `unsafe fn` and document them.

## 4. Resource lifetime

- A type that owns a GPU or OS resource implements `Drop` and releases it, or is destroyed explicitly by the type that owns it (`Texture` and `Presenter` by `Renderer`). Destroy in the reverse order of creation.
- Call `device_wait_idle` before destroying or recreating anything the GPU may still use (swapchain, texture, pipeline).
- Capture sessions and frame pools are closed in `Drop`.

## 5. Error handling

- Application level: `Result<T, Box<dyn Error>>` (`Res<T>`). `main` prints `error: <message>` to stderr and exits with code 1. The demo exits with code 2 on a usage error.
- Windows wrappers return `windows::core::Result` and let `?` propagate.
- Environmental failures (no matching window, no suitable GPU, no frame within 5 seconds) are errors with a message that suggests a remedy (for example "try --list-windows"), never panics.
- `unwrap` and `expect` only for invariants the program itself establishes (a lock that cannot be poisoned in normal use, a value just created) and in tests. Never on OS or I/O results.

## 6. Concurrency

- Share data between the capture callback and the main loop only through `Shared` (`Mutex`, atomics). Keep lock scopes to a copy or a swap.
- The latest frame wins: no queues, no backlog. The consumer swaps buffers instead of copying, and buffers are resized only when the frame size changes.
- Vulkan calls come from the main thread only.

## 7. Vulkan and shaders

- Vulkan 1.3 core only: dynamic rendering and synchronization2. No render passes, no framebuffers, no extension unless needed.
- Every barrier names its source and destination stage and access. Image layouts are tracked explicitly (`Texture.layout`).
- Run with `--validation` before committing any change to `gpu/` or the shaders. The layer must stay silent. **(manual)**
- Shaders are GLSL 450 compiled by `build.rs` with `glslc -O --target-env=vulkan1.3`. A shader error fails the build.
- A push-constant block must be declared identically in every stage that uses it, and its size must match the pipeline layout range (24 bytes for `decode`, 96 for `encode`; the minimum guaranteed limit is 128). Each stage mirrors its block in a `#[repr(C)]` struct with a size test.
- Sampling a captured frame uses `texelFetch` on exact texels. Filtering is a modelled stage, never an accident of the sampler.

## 8. Physical model rules (apply from the first CRT shader)

- Work in linear light with floating point. Conversions to and from gamma or sRGB are explicit, named functions, never implicit in a sampler or a swapchain format.
- Lengths are in millimetres on the virtual tube, converted to host pixels by one documented factor. Time is in milliseconds, voltages and currents keep their units in names (`_mm`, `_ms`, `_kv`).
- The order of stages follows "Suggested physical pipeline order" in `CRT.md` (listed in `ARCHITECTURE.md` section 5): voltage-domain stages before the gun law, linear-light stages after it.
- Each physical constant carries a confidence tag (`[S]` sourced, `[D]` derived, `[K]` standard knowledge, `[E]` estimate) and a source in a comment or in the parameter table. An `[E]` value stays an exposed parameter, not a hidden constant.
- No hidden compensation for the look. Anything non-physical (bloom to recover brightness, enlarged triads) sits in a separate, labelled stage that is off in reference presets.
- Parameters are defined once (the declarative table `params/*.toml`, turned into Rust and GLSL by `paramgen`) and shared with Rust, GLSL and the UI. Never copy a default into two places.
- No randomness without a seed.
- **White scale of the source colour targets** (decided 2026-11-26): a target never rescales white to hide or invent a level. Its white is whatever its sourced model gives for the emulator's full white (255, 255, 255 in, or the emulator's own full white where the data file says the emulator's white is lower, e.g. Mega Drive Genesis Plus GX 238). Where a source states the device's white (the Game Boy Color's 240 of 255, the AGS-101's measured tinted white, the CRT-like set white of a composite model), the target keeps it; where the sources leave the scale open (the Mega Drive's 255 against 238), the file says which it uses and why. Every file of `presets/targets/` states its white (level and tint, per variant when they differ) in a line of its notes that starts with `White scale:` (a test checks it), and `targets::tests::the_white_of_each_target_is_pinned` pins the computed white of variant 1, so a model change that moves a white is seen. The table is in `REFERENCE.md`, "White of each target".

## 9. Patterns in use

| Pattern | Where | Rule |
|---|---|---|
| Producer and consumer with a single-slot mailbox | `Capture` and the main loop | Latest wins, swap buffers. |
| RAII | `Context`, `Renderer`, `Capture`, `Output` | Acquire in `new`, release in `Drop`. The renderer's `Texture` and `Presenter` have no `Drop` of their own (they do not own the device): `Renderer` destroys them after `device_wait_idle`, before its `Context` drops. |
| State machine | `scaling::Detector` | `Default` then `feed` until locked. `reset` on any change of frame size or crop. |
| Plain data handed across modules | `Frame`, `View`, `Rect` | Copyable or swappable, no behaviour. |
| Builder style | `ash` create-info structs | Fill every field you rely on, do not rely on defaults you have not checked. |
| Data-driven configuration | Presets (`presets/tubes/*.toml`), the parameter table (`params/*.toml`), source profiles | Files, not code. |

Avoid adding abstraction before a second implementation exists. Introduce an interface (trait) when the second capture or present backend is real.

## 10. Testing

- Pure logic has unit tests with synthetic data, named after the behaviour (`tolerates_rounded_window_corners`). Run `cargo test`.
- Which tests to run for a change: `scripts\impact.ps1` selects them from the files changed (modules, stages, shaders, parameters, presets); the full gate (`scripts\check.ps1 -Full`) is for a merge to main, a push, a release, or when unsure. See `ENV.md`, Test policy.
- Physics gets analytic tests derived from `CRT.md` (for example 57 % scanline modulation at σ/P = 0.25) before the matching shader is trusted. **(done per stage: the `stage_*` integration tests compare the GPU with the `crt-model` oracle)**
- GPU passes are tested headless (`gpu::Headless`, no window): integration tests in `crates/phoxium/tests/gpu_*.rs` get a renderer from `common::gpu_or_skip`, which enables the validation layer when installed, and end with `common::assert_no_validation_messages` after dropping the renderer. Without a Vulkan 1.3 device they print "skipped" and pass; `PHOXIUM_REQUIRE_GPU=1` turns a missing device or validation layer into a failure; `scripts/package.ps1`, `scripts/check.ps1 -Full` and `scripts/impact.ps1` set it, so a release gate cannot pass by skipping the GPU suite.
- Capture and presentation are checked by hand with `pixel-demo` and `phoxium --stats --validation` (see `ENV.md`). A hosted CI runner (none is set up yet) has no GPU, so it could cover build, format, lint and unit tests only (GPU tests skip).
- Every bug found by hand that can be expressed on pure logic gets a regression test.
- Screenshots used to check output must show only the output monitor or window, never the whole desktop, and are deleted afterwards (they can contain private content).

## 11. Interface, dependencies, privacy, process

- **Command line:** long flags in `--kebab-case`, `--help` and `--version` on every program, every option listed in the `USAGE` text printed by `--help` (`main.rs`) and in `docs/REFERENCE.md`, sensible defaults, unknown flags are errors, a bad value names the flag.
- **Dependencies:** `std`, `windows`, `ash`, `png`, `serde` and `toml` (the parameter table, settings and presets), and Dear ImGui through `dear-imgui-rs`, `dear-imgui-sys` and `dear-imgui-ash` (pinned exactly; the C++ part builds from source with MSVC). New dependencies need a permissive licence (MIT, Apache-2.0, BSD, ISC, Zlib, OFL for fonts). No GPL, LGPL or AGPL: `deny.toml` holds the licence allow list (anything else is denied; an exception would need a named `[[licenses.exceptions]]` entry and a written justification here), `scripts/licence-policy.ps1` applies the same rule to the declared licences (`package.ps1` and `check.ps1` run it, and `cargo deny check licenses` when cargo-deny is installed). `scripts/gen-third-party.ps1` copies the licence texts of every shipped dependency into the release zip and fails when a package has none. Prefer std over a small crate.
- **Privacy and safety:** the product is offline, no telemetry, no network. Capture only the chosen window. The product does no injection, installs no hooks, synthesises no input and uses no low-level keyboard hook (only `RegisterHotKey`). The title of the last captured window is kept in `settings.toml` (the reattach tie-breaker, local only, documented in the README) and `--log` records no window titles. The live-check scripts under `scripts/` are not the product and are not in the release zip: `check-hotkeys.ps1`, `check-menu.ps1` and `check-focus.ps1` send key chords with `SendInput` to whatever has the foreground, and `check-capture.ps1`, `check-pixel-demo.ps1`, `check-hotkeys.ps1`, `check-menu.ps1` and `check-focus.ps1` stop the `phoxium` and `pixel-demo` processes (the spike script `spikes/imgui-spike/scripts/run-t08-live.ps1` does both too). Run them only on a hands-off desktop with no unsaved work in those programs.
- **Commits:** imperative subject up to 72 characters, a body that says why, a `Co-Authored-By` trailer when an AI assisted. Small, buildable commits on `main`. No force pushes to `main`, with one recorded exception: the one-time history rewrite before the public v0.2.0 (it removes the author e-mail from every commit; see the CHANGELOG).
- **Documentation:** a behaviour change updates `README.md` and `ARCHITECTURE.md` in the same commit, adds a line under `## [Unreleased]` in `CHANGELOG.md`, and updates `QUESTIONS.md` if it settles a question.
- **Performance:** measure with `--stats` before optimising. No new CPU readback or per-frame allocation in the render loop without a reason.

## 12. Known deviations (to fix when touched)

- The frame path is GPU, CPU, GPU (decision A3).
- `pixel-demo` uses `unwrap` and `expect` on window-system calls at start-up.
- `capture.rs` unwraps the out-parameters of `D3D11CreateDevice` and the texture creation after the call returned success. The invariant holds but is not commented.
- There is no continuous integration, and the `cargo-deny` licence check is local only (`scripts/check.ps1` and `scripts/package.ps1` run it, skipped with a warning when cargo-deny is missing; the declared-licence filter of `scripts/licence-policy.ps1` always runs).
- There is no automated test of `capture` or `output`; `gpu` has headless tests of device creation, the validation messenger and the pipeline (decode and encode, frames in flight), but none of presentation.
- Synchronization validation is enabled for headless contexts only; the window path (`--validation`) has not been run against it yet.
- **Non-physical headroom by default (against section 8, "no hidden compensation").** ``output_headroom_mode`` defaults to ``auto``, which resolves to ``mask_depth`` where the host resolves the mask (the mask contrast is limited inside the ``screen`` slot, not in a separate stage) and to ``shoulder`` where only the scanlines peak (a soft roll-off above the knee); both are labelled non-physical, and the reference presets 03, 05, 06 and 08 set ``mask_depth = 0.25``. This is an owner decision (T-84b, PLAN D-16), labelled non-physical in the parameter description (``params/output.toml``). The physical choice is ``physical``. Kept on purpose; no code change.
- **Saturation depends on the mask tier.** Phosphor saturation acts on mask-normalised, area-averaged light (``mask.glsl`` divides by the cell width and fill), so its strength depends on the mask tier and on the host resolution, not only on the tube. The approximation is stated in ``phosphor.rs``; the feature is off by default and no preset enables it.
- **Burn-in map aliasing.** The wear map is a box mean of the masked light over a fixed 640 x 480 grid (``wear_accumulate.comp``), so it aliases against a resolved mask and against the scanlines. Burn-in is opt-in.
- **Per-frame time base of the optional modules.** The composite frame phase and the interlace field advance per output frame, not in real time (see ``composite_frame_advance_deg``). The analogue impairments, the tape and the TV flicker follow real time (`FrameConstants::source_frame`, from the sum of the frame times at `impair_field_rate_hz` or `tv_frame_rate_hz`).
- **Phosphor persistence is real time since the review pass (R-10, R-11; a behaviour change, not a deviation).** ``phosphor_frame_rate_hz`` defaults to 0: the trail decays by the time that really passed between two output frames (fit for the measured rate, 10 to 240 Hz; a frame step above ``crt_model::phosphor::MAX_FRAME_STEP_MS``, 250 ms, counts as 250 ms and is left out of the rate estimate). Before, every output frame decayed the trail by a fixed 1/60 s, so the trail was 2.4 times shorter on a 144 Hz display and longer below 60 Hz; now it lasts the same time on every display, and on a non-60 Hz display it no longer looks as it did before the change. A positive value restores the fixed per-frame period (manual override; the stage tests use it). The burn-in frame hours use the same 250 ms clamp.
- **Motion clarity approximations (2026-11-27).** The rolling scan's band boundary is a horizontal cut of the tube image, so with rotation or trapezoid the tilted lines at a boundary may be lit in two refreshes for a few pixels; the stages before the `scan` slot run once per sweep (a parameter change reaches them at the next sweep, up to one source frame later); the sweep follows the output frame counter, not the capture's timing; the burn-in charges the frame-integrated light, not the boosted peak. See ARCHITECTURE 3.7, "Motion clarity".
- **Strict device requirements (R-29).** Device selection refuses a GPU without Vulkan 1.3, a queue family that does both graphics and compute (and presents), ``dynamicRendering``, ``synchronization2``, ``fragmentStoresAndAtomics``, 256 compute invocations per workgroup, a workgroup size of (256, 32, 1) or 28 KiB of compute shared memory, with the missing item named. These are above the Vulkan 1.3 minimums (128 invocations, 16 KiB), so a device that meets only the minimums is refused. The shared memory figure is the largest use, ``composite.comp`` (28 KiB); the beam tile of ``screen.comp`` needs 20 KiB, so a device with 20 to 28 KiB is refused even with composite video off. Checking per stage, or splitting ``composite.comp``, would lower it.
- **Global hotkeys.** The bare F10 (menu) and F11 (fullscreen) chords are registered system-wide (``RegisterHotKey``) while the app runs, so no other program, the captured game included, receives them (both are rebindable, ``[global.hotkeys]``). Owner decision, documented in the README. The other defaults use Ctrl+Alt with a function or navigation key only, never with a character key: Windows sends AltGr as Ctrl+Alt, so such a chord would also fire on AltGr and swallow a character (``@`` on a German keyboard) in every program (review D-01). Never write a hotkey chord in a message or a document as a constant: show the chord in force (``Registration::bound``), or say "by default".
- **Toolchain not pinned for shaders.** The ``glslc`` version is whatever ``VULKAN_SDK`` provides; ``scripts/package.ps1`` records it, with ``rustc -vV``, in ``BUILD-INFO.txt`` of the zip, and ``ENV.md`` lists the version used for a release. A shader whose source was deleted keeps a stale ``.spv`` in ``OUT_DIR`` until a clean build.
- **Planning material in the public tree.** ``PLAN.md``, the spike ``RESULTS.md`` files and the ``QUESTIONS.md`` run logs stay in the repository as provenance of the decisions (scrubbed of narration and personal paths). The ``Co-Authored-By`` trailers of the AI-assisted commits are required by section 11.

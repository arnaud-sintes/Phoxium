# DESIGN

Coding rules, patterns and practices for ultimateCRT. Architecture is in `ARCHITECTURE.md`, build and release in `ENV.md`.

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
- A push-constant block must be declared identically in every stage that uses it, and its size must match the pipeline layout range (24 bytes for `decode`, 8 for `encode`; the minimum guaranteed limit is 128). Each stage mirrors its block in a `#[repr(C)]` struct with a size test.
- Sampling a captured frame uses `texelFetch` on exact texels. Filtering is a modelled stage, never an accident of the sampler.

## 8. Physical model rules (apply from the first CRT shader)

- Work in linear light with floating point. Conversions to and from gamma or sRGB are explicit, named functions, never implicit in a sampler or a swapchain format.
- Lengths are in millimetres on the virtual tube, converted to host pixels by one documented factor. Time is in milliseconds, voltages and currents keep their units in names (`_mm`, `_ms`, `_kv`).
- The order of stages follows "Suggested physical pipeline order" in `CRT.md` (listed in `ARCHITECTURE.md` section 5): voltage-domain stages before the gun law, linear-light stages after it.
- Each physical constant carries a confidence tag (`[S]` sourced, `[D]` derived, `[K]` standard knowledge, `[E]` estimate) and a source in a comment or in the parameter table. An `[E]` value stays an exposed parameter, not a hidden constant.
- No hidden compensation for the look. Anything non-physical (bloom to recover brightness, enlarged triads) sits in a separate, labelled stage that is off in reference presets.
- Parameters are defined once (planned declarative table) and generated or shared with Rust, GLSL and the UI. Never copy a default into two places.
- No randomness without a seed.

## 9. Patterns in use

| Pattern | Where | Rule |
|---|---|---|
| Producer and consumer with a single-slot mailbox | `Capture` and the main loop | Latest wins, swap buffers. |
| RAII | `Context`, `Renderer`, `Capture`, `Output` | Acquire in `new`, release in `Drop`. The renderer's `Texture` and `Presenter` have no `Drop` of their own (they do not own the device): `Renderer` destroys them after `device_wait_idle`, before its `Context` drops. |
| State machine | `scaling::Detector` | `Default` then `feed` until locked. `reset` on any change of frame size or crop. |
| Plain data handed across modules | `Frame`, `View`, `Rect` | Copyable or swappable, no behaviour. |
| Builder style | `ash` create-info structs | Fill every field you rely on, do not rely on defaults you have not checked. |
| Data-driven configuration (planned) | Presets and profiles | Files, not code. |

Avoid adding abstraction before a second implementation exists. Introduce an interface (trait) when the second capture or present backend is real.

## 10. Testing

- Pure logic has unit tests with synthetic data, named after the behaviour (`tolerates_rounded_window_corners`). Run `cargo test`.
- Physics gets analytic tests derived from `CRT.md` (for example 57 % scanline modulation at Ïƒ/P = 0.25) before the matching shader is trusted. **(planned, SCP-3)**
- GPU passes are tested headless (`gpu::Headless`, no window): integration tests in `crates/ultimatecrt/tests/gpu_*.rs` get a renderer from `common::gpu_or_skip`, which enables the validation layer when installed, and end with `common::assert_no_validation_messages` after dropping the renderer. Without a Vulkan 1.3 device they print "skipped" and pass; `UCRT_REQUIRE_GPU=1` turns a missing device or validation layer into a failure (use it on the dev machine).
- Capture and presentation are checked by hand with `pixel-demo` and `ultimatecrt --stats --validation` (see `ENV.md`). A hosted CI runner (none is set up yet) has no GPU, so it could cover build, format, lint and unit tests only (GPU tests skip).
- Every bug found by hand that can be expressed on pure logic gets a regression test.
- Screenshots used to check output must show only the output monitor or window, never the whole desktop, and are deleted afterwards (they can contain private content).

## 11. Interface, dependencies, privacy, process

- **Command line:** long flags in `--kebab-case`, `--help` and `--version` on every program, every option listed in the `USAGE` text printed by `--help` (`main.rs`) and in `README.md`, sensible defaults, unknown flags are errors, a bad value names the flag.
- **Dependencies:** `std`, `windows`, `ash`, `png` today. New dependencies need a permissive licence (MIT, Apache-2.0, BSD). No GPL. `deny.toml` holds the licence allow list (GPL denied); `scripts/check.ps1` runs `cargo deny check licenses` when cargo-deny is installed. Prefer std over a small crate.
- **Privacy and safety:** offline, no telemetry, no network. Capture only the chosen window. No injection, no hooks, no input synthesis, no low-level keyboard hooks (only `RegisterHotKey`).
- **Commits:** imperative subject up to 72 characters, a body that says why, a `Co-Authored-By` trailer when an AI assisted. Small, buildable commits on `main`. No force pushes to `main`.
- **Documentation:** a behaviour change updates `README.md` and `ARCHITECTURE.md` in the same commit, adds a line under `## [Unreleased]` in `CHANGELOG.md`, and updates `QUESTIONS.md` if it settles a question.
- **Performance:** measure with `--stats` before optimising. No new CPU readback or per-frame allocation in the render loop without a reason.

## 12. Known deviations (to fix when touched)

- The frame path is GPU, CPU, GPU (decision A3).
- `pixel-demo` uses `unwrap` and `expect` on window-system calls at start-up.
- `capture.rs` unwraps the out-parameters of `D3D11CreateDevice` and the texture creation after the call returned success. The invariant holds but is not commented.
- There is no continuous integration, and the `cargo-deny` licence check is local only (`scripts/check.ps1`, skipped with a warning when cargo-deny is missing; `package.ps1` only checks the declared licences).
- There is no automated test of `capture` or `output`; `gpu` has headless tests of device creation, the validation messenger and the pipeline (decode and encode, frames in flight), but none of presentation.
- Synchronization validation is enabled for headless contexts only; the window path (`--validation`) has not been run against it yet.

# ARCHITECTURE

How ultimateCRT is built and why. The need is in `CLAUDE.md`, the physics in `CRT.md`, the open questions and decisions in `QUESTIONS.md` (ids such as SRC-2 refer to it). Coding rules are in `DESIGN.md`, build and release in `ENV.md`.

Status legend: **Implemented** means it is in the code today. **Planned** means it is decided or defaulted in `QUESTIONS.md` but not written.

## 1. Context

ultimateCRT runs next to the program it enhances. It never injects into it, hooks it or sends it input.

```
 source program (any window)            ultimatecrt (this project)
 ┌───────────────────┐   Windows.Graphics.Capture   ┌────────────────────────────────────────────┐
 │ game / emulator   │ ───────────────────────────► │ capture ─► scaling ─► Vulkan passes ─► out │
 │ (pixel art, 1:1)  │   GPU texture of the window  │  (CPU copy)  (÷N)      (GLSL)     window   │
 └───────────────────┘                              └────────────────────────────────────────────┘
                                                      output window: borderless, topmost, never takes focus
```

Constraints: Windows 11 x64 only, SDR output, a Vulkan 1.3 GPU (developed on an NVIDIA RTX A2000 Laptop, a hybrid-graphics machine with an Intel iGPU). One screen must work: the fullscreen output covers its own source.

## 2. Repository layout

| Path | Role |
|---|---|
| `crates/pixel-demo` | Demo source app (Win32 and GDI). Procedural NES-style scene, test card, PNG display. It is the capture target for development and tests. |
| `crates/ultimatecrt` | The main application. |
| `shaders/` | GLSL 450 sources, compiled to SPIR-V by `crates/ultimatecrt/build.rs`. |
| `params/` | The declarative parameter table, one TOML file per stage (see 3.6). |
| `crates/paramgen` | Build-time generator: reads `params/*.toml`, writes the Rust and GLSL parameter code (see 3.6). |
| `scripts/` | `package.ps1` (checks, build, tests, release zip), `release.ps1` (GitHub release). |
| `rust-toolchain.toml`, `rustfmt.toml` | Pinned toolchain, formatting rules. |
| `CRT.md`, `QUESTIONS.md` | Physics knowledge base, decisions and open questions. |

## 3. The `ultimatecrt` application

### 3.1 Modules

| Module | Responsibility | Depends on |
|---|---|---|
| `main.rs` | Argument parsing, start-up, main loop, statistics, PNG dump (`--save-capture`). | all below |
| `sources.rs` | Lists top-level windows (`EnumWindows`), finds the target by title (prefix match first, then substring), skips own windows and minimised ones. | Win32 |
| `capture.rs` | Windows.Graphics.Capture session. Crops to the client area, copies to a CPU buffer, publishes the newest frame. | `windows`, Direct3D 11 |
| `scaling.rs` | Pure logic: `Rect`, divisor parsing, `Detector` (integer-upscale detection), `View` (crop, divisor, native size). No OS calls. Unit tested. | none |
| `output.rs` | The output window (raw Win32): windowed or fullscreen non-activating, monitor choice, global quit hotkey, message pump. | Win32 |
| `params.rs` | Generated `Params` struct, parameter metadata, std140 serialisation (see 3.6). | `paramgen` (build time) |
| `gpu/` | Vulkan renderer. `context.rs`: instance, device choice, queue (with or without a window). `present.rs`: `Presenter` (surface, swapchain, per-image semaphores), one per window. `pipeline.rs`: `Pipeline`, the stage chain, its images and frames in flight (see 3.7). `stages/`: one module per stage (`decode`, `encode`). `mod.rs`: `Renderer` facade. `headless.rs`: `Headless`, the same pipeline into an offscreen RGBA16F/RGBA32F/BGRA8 image with readback, without a window (tests). With validation, `context.rs` installs a debug-utils messenger that prints and counts messages (`ValidationCounter`); headless contexts also enable synchronization validation. | `ash`, `scaling::View` |
| `shaders/*.vert, *.frag, *.comp` | `fullscreen.vert` draws one triangle over the target. `decode.comp` and `encode.frag` are the stages of 3.7. `include/common.glsl`: `FrameConstants` block and transfer functions. | built by `build.rs` (every shader, `shaders/include` and the generated `params.glsl` on the include path) |

### 3.2 Per-frame data flow (Implemented)

1. **Capture thread** (thread-pool thread of the free-threaded frame pool). `Worker::on_frame` receives the frame texture, computes the client area inside it (`DwmGetWindowAttribute(EXTENDED_FRAME_BOUNDS)` against `ClientToScreen`), copies that region into a staging texture, maps it and copies the rows into `Shared.frame` (a `Mutex<Frame>`), incrementing a sequence number. If the content size changed, it recreates the frame pool.
2. **Main thread** pumps Win32 messages, then `take_frame` swaps the newest frame's buffer into the renderer's `Frame` (latest frame wins, no queue, no allocation once sizes are stable).
3. `scaling::Detector::feed` analyses the frame until its divisor is locked (30 informative frames, or earlier when the source is native). A flat frame, such as a black boot screen, is not informative and does not count. The result and the optional `--crop` form a `View`.
4. `Renderer::render` waits for its frame slot, copies a new frame into the slot's persistently mapped staging buffer, and records the `Pipeline` (3.7): buffer-to-image copy into the `B8G8R8A8_UNORM` source image, `decode`, `encode` into the swapchain image.
5. `decode.comp` fetches the texel at `origin + p * div + div / 2` for native pixel `p` (the centre of each block), so no separate downscale pass exists.
6. Present with FIFO (vsync). The swapchain is `B8G8R8A8_UNORM`: `encode` writes display code values, encoded explicitly with the host EOTF.

### 3.3 Threading and synchronisation

- Two threads touch frames: the capture callback (producer) and the main loop (consumer). They share only `Shared`: a `Mutex<Frame>`, an `AtomicBool` for the closed-window event and an `AtomicU64` frame counter.
- The Direct3D device is created with multithread protection. The immediate context is used only inside the callback, inside `Worker`, which is moved there (`unsafe impl Send` with that invariant).
- Vulkan is used from the main thread only. There are two frames in flight (3.7): per frame slot one fence, one command buffer, one image-available semaphore; one render-finished semaphore per swapchain image. The swapchain is rebuilt on size change or `OUT_OF_DATE`.

### 3.4 Output window (Implemented)

Raw Win32, not winit, because the behaviour needed (non-activating, topmost, tool window, `MA_NOACTIVATE`, global hotkey, per-monitor DPI v2) is Win32 specific.
- Fullscreen: `WS_POPUP` with `WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`, shown with `SW_SHOWNOACTIVATE` over the monitor of the source window (or `--monitor`). It never takes focus.
- Windowed: normal overlapped window, Esc closes it.
- `Ctrl+Alt+Q` is registered with `RegisterHotKey` and quits. If another program already owns it, a windowed output only warns (Esc still works), while a fullscreen output refuses to start, because it could not be closed from the keyboard. No keyboard hooks are used.
- `Output` unregisters the hotkey and destroys its window in `Drop`.

### 3.5 Device choice

The Direct3D capture device uses the default adapter. The Vulkan device is chosen separately: the requested `--gpu` index, else an NVIDIA discrete GPU, else any discrete GPU, else the first one that is Vulkan 1.3 and can present to the window. On a hybrid-graphics machine this may imply a cross-adapter copy (risk HW-3).

### 3.6 Parameter table (Implemented; `exposure` is read by `encode`)

Every CRT parameter is defined once (DESIGN section 8), in `params/<stage>.toml`: id, stage, type (f32, i32, bool, enum, vec2, vec3, vec4), unit, default, soft range, confidence tag (S, D, K, E), source, description, UI group, enum values. One file per stage, so stage tasks do not edit the same file. The format is documented in `crates/paramgen/src/lib.rs`.

`build.rs` calls `crates/paramgen` (a build dependency, `serde` and `toml`), which validates the files and writes to `OUT_DIR`:
- `params.rs`, included by `src/params.rs`: the `Params` struct with the table defaults, `PARAMS_META` (all metadata, for the UI, presets and documentation), `write_std140` and `to_std140` (the uniform buffer bytes), `get` and `set` by id, one `u32` constant per enum value;
- `params.glsl`: the same constants and a `Params` uniform block (instance `params`, set and binding 0 unless `UCRT_PARAMS_SET` and `UCRT_PARAMS_BINDING` are defined) whose members carry explicit `layout(offset = N)`. `build.rs` compiles a shader that includes it with `glslc`.

The std140 layout is computed once, in declaration order (production files sorted by name), and written into both outputs. Parameters live in a uniform buffer, not push constants (128 bytes minimum limit). A duplicate id, an out-of-range default, an unknown key or a missing field fails the build with a message naming the file and the id. Test-only entries (`params/_test.toml`, every type) are placed after the production entries and exist only in non-release builds (`cfg(ucrt_test_params)`). Stage-specific ids are added by their stage tasks; today the table holds `exposure` (output) and the shared tube colorimetry (`colour.toml`: primaries and white in CIE xy).

### 3.7 Pipeline core (Implemented, T-20a)

`gpu/pipeline.rs` owns the fixed ordered chain of stages (A11, TEC-6), the images between them and the frames in flight. The window `Renderer` and the headless renderer run the same `Pipeline`.

Chain today: upload (captured BGRA8 to the `src` image) -> `decode` (compute) -> `encode` (fragment) -> target (swapchain image or headless image).

| Stage | Kind | Reads | Writes |
|---|---|---|---|
| `decode` | compute, 8x8 groups | `src` (`B8G8R8A8_UNORM`, capture size), at block centres `origin + p * div + div / 2` of the `View` | `native` (`R16G16B16A16_SFLOAT`, native size): `T(code / 255)` with the `source` transfer |
| `encode` | fragment, one triangle over the target | `native` (nearest: T-20a has no screen stage) | target: inside the tube rectangle `exposure * L`, clipped to [0, 1], encoded with the inverse of the `host` transfer; black surround outside |

- **Transfers** (`Transfer`: identity, pure power, piecewise sRGB) are named functions in `shaders/include/common.glsl` (DESIGN 8). Defaults (`Settings`): `source` = power 2.4, `host` = power 2.2. The `source` transfer stands in for the gun law until the `gun` stage exists: T-20b sets it to identity (the native raster then holds code values, drive voltages, as the voltage-domain stages need) and the gun slot carries the tube gamma. Both transfers move into the parameter table with T-30 (`source_eotf`), T-34 (`tube_gamma`) and T-46 (host EOTF).
- **Stage contract** (`gpu/stages/mod.rs`): set 0, binding 0 `Params` (T-10), binding 1 `FrameConstants`, bindings 2 and up the stage's images (inputs as combined image samplers read with `texelFetch`, compute outputs as storage images). Settings that are not yet parameters go in a small push-constant block.
- **`FrameConstants`** (std140, 48 bytes, `common.glsl`; a Rust test checks the offsets): tube rectangle in target pixels (x0, y0, x1, y1), native size, target size, px per mm and line pitch in mm (0 until T-30 places the tube), frame index, time since the previous frame in ms. Until T-30, the tube rectangle is the native raster letterboxed into the target (integer factors with `--integer`).
- **Frames in flight:** 2 slots. Each slot has its own command buffer, fence, staging upload buffer, `FrameConstants` and `Params` uniform buffers and descriptor sets; the CPU writes a slot only after waiting for its fence. The images are shared: each frame records barriers against the previous frame's use (upload after the previous decode reads, decode after the previous encode reads or test readback), which order frames on the single queue. A headless burst test submits overlapping frames under synchronization validation.
- **Reallocation:** `src` and the staging buffers follow the capture size, `native` the native size of the `View`. A change idles the device, recreates the image and rewrites the descriptor sets of every slot. A new image starts `UNDEFINED`: history is cleared on a mode change (SRC-8); later history images follow the same rule.
- **Seam for T-20b:** a new stage is a module in `gpu/stages/` with its shader, created and destroyed by `Pipeline`, recorded between `decode` and `encode` in `Pipeline::record` with a barrier on each image it reads or writes; its image goes between `native` and the `encode` input (binding 2 of `encode`).

## 4. Architectural decisions

| # | Decision | Status | Why | Alternatives and revisit trigger |
|---|---|---|---|---|
| A1 | Rust for the application | Implemented | Safe systems language, strong Windows and Vulkan bindings, single binary. | None considered seriously. |
| A2 | External capture with Windows.Graphics.Capture, client area only | Implemented | No injection (requirement). Works per window and on covered windows (to confirm with real games, CAP-4). | Desktop Duplication cannot isolate one window. |
| A3 | CPU copy from the capture texture to Vulkan | Implemented, temporary | Simplest correct path, cheap at 256×240 sources. | Shared NT handle or GPU-to-GPU copy once measured at larger sizes or on hybrid graphics (CAP-2). |
| A4 | Vulkan 1.3 through `ash`, dynamic rendering and synchronization2 | Implemented | Owner's stack choice, explicit control of present and external memory, no render-pass boilerplate. | Direct3D 12 if SP-1 (interop) or SP-2 (no-focus present) is painful (TEC-1). Slang or HLSL if a backend switch must stay cheap. |
| A5 | GLSL compiled to SPIR-V at build time with `glslc` | Implemented | Fails the build on shader errors, no runtime compiler. | Runtime compilation with hot reload in development builds (TEC-3, planned). |
| A6 | Raw Win32 output window | Implemented | See 3.4. | winit only if it proves able to do the same (TEC-4). |
| A7 | Swapchain `B8G8R8A8_UNORM`, FIFO | Implemented | No implicit conversion: the `encode` stage applies the host EOTF explicitly. Vsync, no tearing (OUT-6). | A 10-bit or FP16 swapchain when SDR banding shows (COL-4). |
| A8 | Source integer scaling by detection or `--divisor`, sampled at block centres in the shader | Implemented | The model needs the native raster (scanlines are native rows). Only integer nearest-neighbour upscales are handled, by owner decision (SRC-2). | A native-resolution entry plus resampling for non-integer sources, not planned. |
| A9 | One workspace version, tags `vX.Y.Z`, Keep a Changelog | Implemented | One product, two binaries released together. | None. |
| A10 | SDR output, linear-light FP16 internal pipeline | Implemented in part (T-20a: FP16 native raster, explicit host EOTF in `encode`; no dither yet, T-46) (COL-1) | Accurate gamma, mask sampling, halation and afterglow need unclamped linear values. HDR output is out of scope. | HDR output later behind the present interface. |
| A11 | Fixed ordered pipeline of physical stages | Planned (TEC-6) | The physics has an order (see 5). | A data-driven pass graph is explicitly not wanted. |
| A12 | Mask type is a 3-way choice, all other look parameters independent, in physical units | Planned (PHY-4, PHY-16) | Freedom of emulation. Each type has its own geometry parameters. Warnings, not hard limits, for impossible tubes. | None. |
| A13 | Presets are data files (up to 10 well-known models), tagged by confidence | Planned (PHY-4) | Starting points the user edits. No code per model. | None. |
| A14 | Area-averaged mask by default, coarser tube preset as an alternative | Planned (PHY-2) | The host (2560×1440) gives 1.5 to 4 px per triad. | Host subpixels (HW-6) deferred. |
| A15 | Optional modules, off by default: composite video, interlace, ageing and burn-in | Planned (PHY-9, SRC-7, PHY-14) | In scope by owner decision. Burn-in needs a persistent per-colour wear map. | After the first usable version. |
| A16 | Headless renderer and analytic tests from `CRT.md` | Headless renderer and GPU test harness implemented (T-09); analytic stage tests planned (TEC-7, SCP-3) | A physical model cannot be validated by eye. | None. |
| A17 | Dear ImGui for live tuning, as a settings menu inside the output window (F12) plus an optional read-only overlay | Planned (OUT-3, TEC-5, owner decision 2026-11-18) | Output does not take focus while it displays; the menu takes it while open and returns it on close. | Another UI toolkit only with the owner's agreement. |
| A18 | No GPL code, permissive dependencies only | Policy (PRJ-2) | Personal project that may become public. Inspiration from existing shaders is allowed, copying is not. | None. |

## 5. Target pipeline (Planned)

Stages in the order physics applies them (details in `CRT.md`).

This follows "Suggested physical pipeline order" in `CRT.md`, the authority if the two differ.

1. Source decode: native raster, source curve to code values, pixel aspect ratio. Optional signal module in the voltage (gamma-encoded) domain: composite encode and decode, amplifier bandwidth with rise and fall asymmetry, peaking, interlace field mode. Filtering horizontally before the gun law is what makes isolated vertical lines dimmer than horizontal ones.
2. Gun law per channel (brightness, contrast, cutoff, gain, power law around 2.4), then automatic beam limiting (gain from the frame's average level, low-passed in time). Gives the beam current per channel.
3. Beam: Gaussian-plus-tail profile in y per scanline, horizontal spot width, both depending on current and position, per-channel convergence offsets, and geometry (curvature, pincushion residuals, blooming scale).
4. Mask: integrate the beam over each phosphor element of the chosen mask type, prefiltered to the host pixel footprint (area-averaged), times element shape and fill factor. Phosphor saturation.
5. Phosphor: temporal decay per colour (frame feedback), ageing and burn-in wear map.
6. Optics: layer diffusion, halation ring, veiling glare, vignetting, glass tint.
7. Output: phosphor primaries and white to the host display primaries in linear light, exposure, SDR encode with a final dither.

Each stage has an enable flag so it can be bypassed or A/B compared.

Cross-cutting plans: one declarative parameter table shared by Rust, GLSL and the UI; three layered text profiles (tube, host display, source) with schema version and confidence fields; a tube-state file for the wear map; GPU timestamps per stage and a stats overlay.

## 6. Known risks

Tracked in `QUESTIONS.md` ("Risks that could invalidate the project"). The ones that shape the architecture today:
- **Hybrid graphics (HW-3):** capture and Vulkan may sit on different adapters.
- **One screen (OUT-1, CAP-4):** a covered source may stop delivering frames. Early test: with the demo, capture kept about 60 fps under a fullscreen output. A real game is not yet tested.
- **No-focus present (OUT-2):** whether the compositor adds latency or disables independent flip is not measured.
- **Unmeasured physics:** most constants in `CRT.md` are estimates and stay tagged until replaced.

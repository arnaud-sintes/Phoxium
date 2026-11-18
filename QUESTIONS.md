# QUESTIONS.md: open questions before ultimateCRT starts

This file reconciles three independent need analyses (Sonnet, Opus, Fable agents, high effort, 2026-11-18).
Each analyst read `CLAUDE.md` and `CRT.md` and listed questions, tensions and spikes.
About 306 raw questions were merged and deduplicated into the catalogue below.
Where the analysts proposed different defaults, the difference is kept visible (see "Where the analysts disagreed").

Nothing here is a design.
It lists what to decide, what to measure and what to prototype before committing to a design.

## Decisions recorded (2026-11-18)

Answers from the project owner to the blocking table. They override the defaults in the entries below, which carry a "DECISION" bullet where they changed.

| Id | Decision |
|---|---|
| HW-1 | Detected on the dev machine: laptop with Intel UHD Graphics plus NVIDIA RTX A2000 8GB Laptop GPU (driver 596.71). Two active Dell P2418D monitors (24 in, 2560×1440, 60 Hz, about 53×30 cm, SDR IPS). |
| COL-1, CAP-3 | Desktop HDR is off: the only active monitors are P2418D, which have no HDR. The owner also says neither the Intel nor the NVIDIA GPU uses HDR. |
| PHY-4 | Mask type is a 3-way choice (aperture grille, slot mask, delta dot), all other look parameters are independent. Well-known CRT models ship as starting-point presets (data files, 3 to 5, user-editable), not code (see PHY-4). Extended to up to 10 on request. |
| PHY-14, SCP-6 | Ageing and burn-in are in scope, opt-in, with a persistent tube state (see PHY-14). |
| PHY-9, SRC-7 | Composite video and interlace are in scope as optional modules, off by default, after the first usable version (see PHY-9, SRC-7). |
| OUT-1, CAP-4 | Must work with one screen. SP-1 and SP-2 become blocking for the whole project. Two monitors is a bonus, not the guaranteed mode. |
| SRC-1, SRC-2 | Test inputs: any NES-style still snapshot, then Mina the Hollower in real conditions. Any input resolution and colour count must work. |
| COL-1 | Output is SDR only, no HDR output and no HDR colour (decided by the assistant, see COL-1). Internal computation is linear-light floating point. |
| PHY-2 | Default is option A (area-averaged mask with a live pixels-per-triad readout). Option D (offer a coarser tube preset that the host can resolve) must exist as an alternative. |
| SCP-1, SCP-2 | No real CRT and no instruments. Physics comes from literature and web research, tagged by confidence. More research passes are wanted on the weakest data. |
| PRJ-1, PRJ-2 | Personal project. No GPL shader code is reused, inspiration only. |
| SCP-5 | The wording "each source RGB pixel becomes RGB light sources" is shorthand for the beam-and-phosphor model. `CLAUDE.md` is updated. |
| TEC-1 | Vulkan, GLSL, Rust and ImGui are the owner's own guess, not hard constraints. Counter-proposals welcome, decided after the spikes. |
| OUT-3, OUT-5 (2026-11-18, planning) | No separate control window. F11 toggles fullscreen and a resizable windowed output. F12 opens and closes the ImGui settings menu inside the output window; the menu takes the focus while open and gives it back to the source on close. Output otherwise stays non-activating. F11 and F12 are global hotkeys, configurable. |
| SRC-1, SRC-2 (2026-11-18, planning) | Mina the Hollower is not on the dev machine. A stand-in pixel-art game is used during development; Mina is the final acceptance test on the owner's other machine (PLAN.md T-83). First usable version keeps all 3 mask types. |

Still open:
- Which GPU drives the monitors: the Intel iGPU or the NVIDIA GPU (see HW-3, new hybrid-graphics finding).
- Nothing else blocking.
- OUT-2, OUT-3 (no-focus window and menu placement): defaults stand, decided by SP-2.
- TIM-1 (latency), PHY-12 (temporal scope): defaults stand.

## How to read this file

Each question has an id, the question, why it matters, who can answer, options when useful, and a recommended default.

| Who | Meaning |
|---|---|
| USER | Product-owner decision: preference, scope, budget, or hardware you own |
| SPIKE | Needs a small prototype or experiment on your real machine (hours to 2 days) |
| RESEARCH | Needs desk research (docs, licences, crate status), no code |
| DATA | Needs measurement data or material that is not yet in `CRT.md` |

Windows capture, presentation, Vulkan and HDR facts are the analysts' own knowledge unless a source is listed in "Feasibility facts to verify".
Every such fact that drives a decision is tied to a spike.

If you want to move fast, answer the "Decisions needed from you first" table and say "accept the defaults" for the rest.

## Decisions needed from you first

These are the questions all three analysts ranked as blocking.
Each has a recommended default so you can reply with exceptions only.

| Id | Question | Recommended default |
|---|---|---|
| HW-1 (partial) | What GPU and which monitor(s) do you play on: resolution, refresh rate, panel type (LCD, mini-LED, WOLED, QD-OLED), HDR peak nits, subpixel layout, VRR, one or two screens? | None: this blocks most model decisions. Please provide it |
| SCP-1, SCP-2 (answered) | What makes the model "physically accurate" (principle by construction, numeric checks, match to a reference tube), and do you own a real CRT or instruments (colorimeter, macro camera, photodiode)? | Physical by construction plus numeric invariants from `CRT.md`. Assume no tube and no instruments, and design validation not to depend on them |
| PHY-2 (answered) | When the host cannot resolve the mask (a PVM mask gets 2.3 to 2.7 px per triad on 4K, anything on 1080p), what happens: area-filter, zoom, enlarge the pitch, or use host subpixels? | Area-integrate by default with a live px-per-triad readout. Enlarged pitch only as an explicitly labelled non-physical option |
| COL-1 (answered) | Is HDR a requirement, an enhancement, or later? How is the 75 to 85 % mask loss handled in SDR? | HDR is the reference, not required. SDR uses an honest exposure control, no hidden bloom compensation |
| OUT-1, CAP-4 (answered) | Is the reference setup one monitor (output covers the game) or two? Does a covered, off-screen or minimised source keep rendering and keep delivering captured frames? | Design for both. Guarantee two monitors first, let a spike decide whether one monitor works |
| OUT-2 | Can a Vulkan window that is topmost, borderless, non-activating (optionally click-through) present fullscreen at full rate, with HDR, without ever taking focus? | Borderless topmost non-activating window, exclusive fullscreen disallowed. Spike before committing |
| OUT-3 | What exactly does "does not steal focus" cover, and where does the ImGui menu live? | DECIDED 2026-11-18: no separate control window. F12 toggles the ImGui settings menu inside the output window (it takes the focus while open, returns it on close); F11 toggles fullscreen and a resizable windowed output. Output otherwise never activates. |
| SRC-1, SRC-2 (answered) | Which content must work (pixel art only, emulators, DOS, modern 3D, video), which 5 to 10 apps form the acceptance set, and what does "1:1" mean in practice? | Low-resolution raster content first. Per-profile crop and integer divisor, with a warning when the capture is not integer-scaled |
| TEC-1 (answered) | Are Vulkan, GLSL, Rust and Dear ImGui hard constraints or preferences? Is cross-platform a real future goal? | Keep them. Isolate capture and presentation behind interfaces so a D3D path stays possible |
| PHY-12 | Temporal scope of the first version: frame-integrated, 60 Hz afterglow, or rolling-scan sub-frames at 120/240 Hz? | Frame-integrated first. Model emission as a function of time so sub-frames can be added later |
| TIM-1 | What added latency (source Present to photons) is acceptable? | 1 to 2 frames at 60 Hz, measured and documented, not promised as "zero" |
| PRJ-1, PRJ-2 (answered) | Personal or public project, which licence, and may GPL shader code be reused? | Personal first, public-quality architecture. Clean-room from `CRT.md` and papers, no GPL code copied |
| SCP-5 (answered) | Is "each source RGB pixel becomes real RGB light sources" literal, or shorthand? | Shorthand. Reword as "source RGB drives three beams; each phosphor element emits in proportion to the beam energy it receives" |
| CAP-2 | Zero-copy D3D11 to Vulkan interop, or CPU copy first? | Measure both in a spike. A CPU copy is cheap for a 320×240 source but not for a 4K capture |

## Scope, definitions and acceptance

### SCP-1 Accuracy contract
- **Q:** What does "physically accurate" mean as a contract, and at which plane: light leaving the virtual faceplate, or light reaching the eye from the host panel?
- **Why:** Without a testable criterion the differentiator cannot be verified and tuning falls back to "looks right", which `CRT.md` criticises in existing shaders.
- **Who:** USER.
- **Options:**
  - Principle: every stage is a physical mechanism in physical units.
  - Principle plus numeric checks of effects that have published references (scanline modulation versus σ/P, mask transmission, halation ring radius, BT.1886 curve, decay residuals).
  - Photometric match to a specific reference tube.
  - Full spectral or light-transport model (out of reach with current data).
- **Default:** Principle plus numeric checks. The honest claim is "physically structured, parameterised, partially measured", because most parameters in `CRT.md` are estimates.

### SCP-2 Ground truth available
- **DECISION (2026-11-18):** no real CRT and no instruments. All parameters come from literature and web research, tagged by confidence. Action: extra research passes on the weakest data (spot size versus current, decay tails, halation amplitude, consumer pitch, Trinitron curvature) before presets are frozen.
- **Q:** Do you own or can you borrow a real CRT (which model), a colorimeter or spectro, a macro-lens camera, a photodiode and oscilloscope, a high-speed camera?
- **Why:** A colorimeter unlocks primaries, white, gamma and uniformity. A photodiode unlocks decay. A macro lens unlocks spot size versus current, the largest data gap.
- **Who:** USER.
- **Default:** Assume none. Validate against `CRT.md` invariants and published photos, and state it. If a single tube is available, one well-photographed tube beats ten estimated presets.

### SCP-3 Per-effect acceptance tests
- **Q:** For each modelled effect, what is the pass criterion and test pattern?
- **Why:** It turns the `CRT.md` tables into executable checks and ends endless tuning by eye.
- **Who:** RESEARCH drafts, USER approves.
- **Examples:** a flat field of 240p lines gives 57 % modulation at σ/P = 0.25; a 1-pixel vertical line is dimmer than a horizontal one by the configured ratio; the halation ring radius equals 1.74 t within one host pixel; brightness is conserved across sub-frames.
- **Default:** Derive about 15 to 20 analytic checks from `CRT.md` before any shader is written.

### SCP-4 Meaning of "universal"
- **Q:** Universal along which axes: any capturable window, any CRT family, any content type, any GPU vendor, any Windows version, any host display?
- **Why:** Each axis multiplies the test matrix, and "universal" collides with "physically accurate" on the output side (see tension T-1).
- **Who:** USER.
- **Default:** Universal over capturable windows and over CRT families (one parameterised model, presets per family). Not universal over host displays: accuracy tiers instead (PHY-2).

### SCP-5 Wording of the core requirement
- **Q:** Is "each source RGB pixel is transformed as real RGB light source" literal or shorthand?
- **Why:** Read literally it describes a pixel-sprite model. `CRT.md` shows a tube has no pixel grid: the beam is a continuous signal sampled by the mask, and pixel boundaries fall at arbitrary mask phases.
- **Who:** USER.
- **Default:** Shorthand. Update `CLAUDE.md` to the beam-and-phosphor wording above.

### SCP-6 Non-goals
- **DECISION (2026-11-18):** ageing and burn-in (see PHY-14), composite video (see PHY-9) and interlace (see SRC-7) are all in scope, removed from the non-goals below. All three are optional modules, off by default, after the first usable version.
- **Q:** Confirm out of scope for the first version, each as its own decision: composite/S-Video/NTSC, interlace field simulation, audio, light-gun and pointer mapping, bezel and cabinet art, room reflections, ageing and burn-in, purity/doming, monochrome and vector tubes, projection CRTs, rotated (TATE) screens, recording and streaming, Linux/macOS, multi-window capture, RetroArch preset import.
- **Why:** "Anything else physically relevant" is unbounded, and each item adds a model, UI and tests.
- **Who:** USER.
- **Default:** All out, except TATE (cheap if planned early, decide with the user) and pointer mapping (OUT-4). `CRT.md` already treats composite as a separate pre-stage.

### SCP-7 Why a new application at all
- **Q:** What concrete result must ultimateCRT show that ShaderGlass plus crt-royale, crt-guest-advanced, Sony Megatron or the Blur Busters beam simulator cannot? Could the physical model first be prototyped as a RetroArch shader?
- **Why:** ShaderGlass already covers capture-and-overlay. If the value is only the physical model, the capture shell is plumbing, and if the value is the shell, the model is secondary.
- **Who:** USER.
- **Default:** Build the application. Three testable differentiators: voltage-domain horizontal response (dim isolated vertical lines), brightness-dependent spot in mm with mask sampling in mm, halation as a ring at the right radius. Build a headless renderer from the start so the model can be validated without the capture shell, rather than prototyping inside RetroArch.

### SCP-8 Physics versus taste
- **Q:** May the product include non-physical controls (bloom to recover mask loss, mask strength, scanline darkness), and who wins when the physical result is dim or ugly on your display?
- **Why:** On SDR the physical result is dark. Every existing shader cheats on brightness, and `CRT.md` cites cgwg's warning that "looks right" corrections are untrustworthy.
- **Who:** USER.
- **Options:** forbid; allow behind a clearly labelled stage after the physical chain; allow freely.
- **Default:** Allow, in a separate, labelled "host compensation" stage, off in reference presets. Exposure is honest (it is the panel's brightness); bloom compensation is not.

### SCP-9 Ideal tube or aged unit
- **Q:** Do defaults represent a perfectly adjusted new tube or a typical aged unit (misconvergence, corner defocus, vignette, geometry residuals, drifted white)?
- **Why:** Faithful defects make the picture objectively worse, and you may reject an accurate result as "ugly". Memories of aged consumer sets differ from a factory-adjusted PVM.
- **Who:** USER.
- **Default:** Factory-adjusted tube within service tolerance, with defects as parameters at the low end of their documented range.

### SCP-10 Where the model stops
- **Q:** Does the model end at the outer glass in a dark room, or include ambient light, specular reflections and viewing geometry?
- **Why:** Raised blacks and reflections dominate real contrast (ANSI 100 to 150:1) but need a room model that is not tube physics.
- **Who:** USER.
- **Default:** Dark room. One ambient-illuminance scalar raises blacks by the diffuse reflectance. No specular reflections in the first version.

## Source content

### SRC-1 Content classes and acceptance set
- **DECISION (2026-11-18):** test inputs are a NES-style still snapshot (any game) fed as an image or test pattern, then Mina the Hollower as the first real-condition target. Any input resolution and colour count must work. The model only sees the captured RGB frame, so colour count has no effect, but resolution does through native-raster recovery (SRC-2). An image-file source type is needed from the start (see UX-5).
- **Q:** Which content must work in the first version (1:1 pixel art, emulators and which output modes, DOSBox, modern pixel-art indies at arbitrary window size, modern 3D, video), and which 5 to 10 applications form the acceptance set?
- **Why:** The model maps source pixels to beam current, which is well defined only for low-resolution raster content. At 1080 source lines a real CRT shows no scanlines either, and window modes, scaling, throttling and cursor use differ per application.
- **Who:** USER.
- **Default:** 240p/288p/480p pixel art, emulators and DOSBox first. 3D and video are not refused, but nothing is tuned for them. You must name the applications.

### SRC-2 Knowing the native raster
- **IMPLEMENTED (2026-11-18, scaffold):** per-run options `--crop x,y,w,h` and `--divisor N|NxM` (1 = native), plus automatic detection by default (run lengths of identical pixels and rows, largest surviving factor, 16 px margin for Windows 11 rounded corners). Only simple integer nearest-neighbour upscales are handled, by decision of the owner. Tested at x1, x3, x4, x5 on `pixel-demo`. Not yet done: per-profile storage, a menu control, a visible warning for smoothed sources.
- **Q:** How does the tool learn native resolution, crop and scale factor when the window is scaled (3×, 4×, non-integer, letterboxed, with emulator UI)?
- **Why:** The whole model starts from source pixels on known lines. A smoothed or non-integer-scaled capture cannot be recovered. This is the largest practical correctness risk, and the need does not mention it.
- **Who:** USER for policy, SPIKE for detection reliability.
- **Options:** require native 1× client area; user enters integer divisor; automatic detection of pixel repetition; user enters native resolution and the tool resamples.
- **Default:** Per-profile crop rectangle and integer divisor, an auto-detect "suggest" button, and a visible warning when the capture is not integer nearest-neighbour. Client area is the default crop.

### SRC-3 Mapping source rows to scanlines
- **IMPLEMENTED (2026-11-18, T-30):** `line_multiplier` (1 or 2, `params/source.toml`): lines = source rows x multiplier, no row resampling; the scanline pitch `line_pitch_mm` of `FrameConstants` = raster height / lines (PVM-20M4U class, 240 lines: 1.208 mm; multiplier 2: 0.604 mm). Fixed modes and a free "lines" value are not offered; the multiplier is not yet part of a tube preset (T-50). The beam stage (T-36a) must draw each native row as `line_multiplier` scanlines.
- **Q:** What defines the raster: one scanline per source row, fixed modes (240, 480, 576), double-scanned VGA 200-line modes, or a free "lines" parameter?
- **Why:** σ/P at 240p and 480p differs by 2, which flips visible scanlines on or off. A 320×200 DOS game on a VGA monitor (paired fine lines, 70 Hz) and the same pixels on a 15 kHz TV are different pictures.
- **Who:** USER.
- **Default:** Raster mode is part of the tube preset, overridable per profile: lines = source rows × an integer line multiplier (1 for 240p, 2 for double-scan), no row resampling.

### SRC-4 Aspect and pixel aspect ratio
- **IMPLEMENTED (2026-11-18, T-30):** `tube_aspect` (4:3 default, any value, 16:9 and 16:10 as later presets), `raster_aspect` (`par` default: picture aspect = width x PAR / height, shrunk to the tube width when wider; `fill_tube`: the raster stretched to the whole visible area as on a real tube) and `source_par` (NES 8:7 gives 1.219 displayed aspect). The default is `par` with PAR 1 so a square-pixel capture keeps its aspect and the pre-T-30 picture is unchanged; `fill_tube` is the real-tube behaviour. PAR does not yet feed the pixel clock of the amplifier model (T-33). No per-profile PAR storage or system presets yet (T-50, T-51 profiles).
- **Q:** How are display aspect and PAR chosen (presets per system: NES 8:7, Genesis H40 32:35, VGA 320×200 5:6), and what happens with 16:9 pixel art (320×180) that never existed on a 4:3 tube? Are 16:9 and 16:10 tubes (BVM-D24, FW900) in scope?
- **Why:** The tube stretches the raster to cabinet aspect regardless of pixel count, so a 1:1 capture is wrong for non-square-PAR systems. PAR also changes triads per source pixel and the pixel clock used by the bandwidth model. A 4:3 image uses only 2880 of 3840 columns on a 4K 16:9 host.
- **Who:** USER.
- **Default:** Selectable tube aspect (4:3 default, 16:9 and 16:10 as later presets), per-profile PAR override.

### SRC-5 Source colour and gamma interpretation
- **IMPLEMENTED (2026-11-18, T-30):** `source_eotf` = `drive_voltage` (default, V = code / 255 into the gun law) or `srgb` (sRGB EOTF to linear light, then V = L^(1/tube gamma), so the tube shows the sRGB luminance: code 20 gives 0.006995). The tube gamma is still the `Settings::source` stand-in in `decode` until T-34 moves it into the gun stage; the V in [0, 1] contract is in `shaders/decode.comp`. Per-profile choice (PLAN D-3: Mina starts on `srgb`) comes with the profile storage (T-51 settings).
- **Q:** What is the default meaning of a captured 8-bit value: CRT drive voltage reinterpreted with tube gamma (consoles), sRGB-authored PC content, or already-processed emulator output?
- **Why:** `CRT.md` rule 9 and the sRGB-versus-2.4 example (level 20/255) show this changes every shadow and saturation. It is a policy choice, not physics.
- **Who:** USER.
- **Default:** Per-profile source EOTF. Default: captured values are gamma-encoded drive voltages, tube gamma 2.4 above cutoff with no linear toe. "Decode as sRGB" as an option.

### SRC-6 Source cadence, virtual field rate and static content
- **Q:** Is the virtual tube's field rate the source rate (59.94, 60.1, 50, 70.09, 30 fps), a fixed value, or the host refresh divided by an integer? How does the model tell "source repeated a frame" from "source did not render", given the capture API only delivers frames when content changes?
- **Why:** It decides flicker, afterglow, whether rolling scan is possible, and what happens when no new frame arrives.
- **Who:** USER, informed by SPIKE SP-3.
- **Default:** The virtual CRT runs its own clock at the configured rate and re-scans the last captured frame. Rolling scan only when the host rate is an integer multiple within tolerance.

### SRC-7 Interlaced sources
- **DECISION (2026-11-18):** interlace is in scope as an optional, manual mode after composite video, off by default. The user declares the field order and that alternate frames are fields (or that a woven frame is split into fields). Fields are shown alternately at the output refresh with the phosphor decay applied between them, which gives line twitter and flicker. Until then, and for any source without a declared field order, 480i and 576i are treated as 480 and 576 progressive lines (σ/P about 0.5). It needs a 60 Hz output matched to the field rate (the dev monitors are 60 Hz). Low value for the first test sources (NES, Mina the Hollower).
- **Q:** Is 480i/576i field simulation in scope, given a window capture only yields woven or deinterlaced progressive frames with no field parity?
- **Why:** Field order and parity cannot be recovered reliably, and faking them may look worse than progressive.
- **Who:** USER.
- **Default:** Out of the first version. Treat as 480 progressive lines (σ/P(480) about 0.5). A manual "alternate frames are fields" mode later.

### SRC-8 Mid-session changes
- **Q:** Must the pipeline adapt live when the source switches mode or resizes (SNES 256 to 512 hi-res, PS1 mode switches, emulator window recreation), and what happens to history buffers (afterglow, ABL state)?
- **Why:** Geometry and line count are properties of the source mode, and resource reallocation can glitch.
- **Who:** USER plus SPIKE.
- **Default:** Hot-rebuild resolution-dependent resources within a frame, clear history on mode change, re-run crop and scale detection.

## Hardware and OS baseline

### HW-1 Your exact setup
- **ANSWERED (2026-11-18, read from the machine):** laptop with Intel UHD Graphics (reports 2560×1440 at 59 Hz) and NVIDIA RTX A2000 8GB Laptop GPU. Two active Dell P2418D monitors (EDID: 2017 and 2018 units, 53×30 cm image area, 24 in 16:9, pixel pitch about 0.233 mm). Panel type and subpixel layout were not read from the machine: the P2418D is an SDR IPS panel, presumably RGB stripe (to verify with a subpixel test pattern, see HW-6).
- **Consequence for PHY-2:** a 4:3 tube image at full height is 1920 px wide on this host. A 20 in pro tube (about 1245 triads) gets about 1.5 px per triad, a 26 in consumer TV (about 660) gets about 2.9, a 19 in arcade slot tube (about 480) gets about 4. Only coarse tubes approach resolvable, so option A (area-average) is the default and option D (coarser preset) matters.
- **Q:** Which GPU(s), CPU, monitors (resolution, size, refresh, panel type, HDR capability and real peak and sustained nits, subpixel layout, VRR) and how many monitors?
- **Why:** This single answer fixes host pixels per triad, HDR headroom (3 to 7×), whether rolling scan is possible and the performance budget.
- **Who:** USER.
- **Default:** None. Blocking.

### HW-2 Windows baseline
- **Q:** Minimum Windows version: Windows 10 22H2, Windows 11 22H2/23H2, or 24H2 and later? Pinned or tracking?
- **Why:** Borderless capture (`IsBorderRequired`, build 20348+), cursor toggle and `MinUpdateInterval` differ by build. The development machine runs an Insider build (10.0.26300), which may hide regressions present on release builds or show fixes that release builds lack.
- **Who:** USER.
- **Default:** Windows 11 only, x64, exact minimum build fixed after SP-1. Test also on a non-Insider build and re-run the spikes as a smoke test after updates.

### HW-3 GPU vendors, floor and hybrid graphics
- **FINDING (2026-11-18):** the dev machine is a hybrid-graphics laptop. Windows reports the Intel UHD Graphics as the adapter with the 2560×1440 desktop, and the NVIDIA RTX A2000 Laptop GPU as a second adapter with no display mode. Which GPU physically drives each Dell P2418D is not known yet. If the Intel GPU drives the monitors, the desktop compositor runs on Intel, the game may render on NVIDIA, and our renderer on NVIDIA would need a cross-adapter copy of every captured frame (no shared-handle handoff). Running the renderer on the Intel iGPU may be too slow. This is a new top risk, to settle in SP-1 (detect which adapter composes, measure the cross-adapter cost) and before choosing the Vulkan device.
- **Q:** Which vendors must work (NVIDIA, AMD, Intel Arc/iGPU)? What is the weakest GPU? Must hybrid-graphics laptops and multi-GPU desktops work, where the captured window, the compositor and the output may sit on different adapters?
- **Why:** External-memory import, HDR swapchain colour spaces and present behaviour are driver-specific. Cross-adapter capture forces a copy and complicates interop (same-adapter requirement).
- **Who:** USER, plus SPIKE if yes.
- **Default:** Your own GPU is the only certified target, others best effort. Single adapter only: select the Vulkan device by the capture adapter's LUID and warn on mismatch.
- **IMPLEMENTED differently (2026-11-18):** the scaffold does not match LUIDs yet. The Direct3D capture device uses the default adapter (NVIDIA on the dev machine) and the Vulkan device is the requested `--gpu`, else an NVIDIA discrete GPU, else any discrete GPU. The two agree on the dev machine, but nothing checks it and nothing warns on a mismatch. Which GPU composes the desktop is still unmeasured.

### HW-4 Host display classes
- **Note (2026-11-18):** with SDR output, HDR display classes drop out of scope. Rolling scan (120 Hz or more) is deferred with it.
- **Q:** Which host classes are first-class and which degraded: SDR LCD, HDR LCD (FALD, mini-LED), WOLED, QD-OLED; 1080p, 1440p, 4K, 8K; 60 to 480 Hz?
- **Why:** On 1080p even a consumer-TV mask gets about 2.2 px per triad. Rolling scan needs 120 Hz or more and fast pixels, and `CRT.md` warns against strobed, FALD and mini-LED panels.
- **Who:** USER.
- **Default:** First-class is your display. Others run in a documented degraded but honest mode.

### HW-5 Host characterisation
- **Q:** Where do the host's physical width, pixel pitch, peak luminance, primaries and EOTF come from: EDID, Windows HDR metadata, ICC profile, user entry, colorimeter?
- **Why:** Millimetre units, absolute luminance and gamut conversion need them, and EDID is often wrong. Windows Auto Color Management may also transform the output.
- **Who:** RESEARCH, then USER.
- **Default:** Read EDID and Windows values as initial guesses, always editable. SDR assumes sRGB unless a profile is given. Document the colour-management interaction.

### HW-6 Host subpixel use
- **Q:** Should the mask be aligned to the host panel's own subpixels (Megatron approach), and is rotated or portrait (TATE) output in scope?
- **Why:** It triples effective horizontal mask resolution for aperture grilles, and is the only way to resolve PVM-class pitch on 4K. But it is panel-specific (RGB, BGR, WOLED, QD-OLED triangle), needs 100 % scaling and 1:1 pixel mapping, is not a physical model of the tube, and breaks in screenshots and streaming. There is no reliable OS API for the layout.
- **Who:** USER plus SPIKE.
- **Default:** Panel-independent area-integrated mask first. Reserve a per-display "subpixel layout" profile field. Revisit once the panel is known.

### HW-7 OLED wear and panel protection
- **Q:** Is the output an OLED, and are burn-in risk from static high-luminance mask and borders, and the panel's own ABL (which changes brightness with picture level and corrupts the CRT ABL and rolling-scan models), acceptable?
- **Why:** A fixed periodic mask at HDR peak is the worst case for OLED wear.
- **Who:** USER plus RESEARCH.
- **Default:** Document the risk. Offer a slow sub-pixel orbit of the mask. Never show a static UI by default.

## Capture

### CAP-1 Capture API
- **Q:** Windows.Graphics.Capture (per window, GPU texture, works occluded), DXGI Desktop Duplication (per monitor), or both? Can the capture border and indicator be disabled, and is the output window reliably excluded from capture (feedback loop)?
- **Why:** It defines latency, what can be captured, cursor handling, border, cost and the supported OS builds. Desktop Duplication cannot isolate one window and captures our own output.
- **Who:** SPIKE (SP-1).
- **Default:** Windows.Graphics.Capture only for the first version. Require a build where the border can be disabled.

### CAP-2 Getting frames into Vulkan
- **Clarification (2026-11-18):** there is always an intermediate copy of the source pixels. Windows.Graphics.Capture hands our process a GPU texture holding the composed pixels of the other application's window, made by the OS. "Zero-copy" and "CPU copy" only describe how that texture reaches our Vulkan renderer:
  - **Shared handle (zero extra copy):** the capture texture lives in a Direct3D 11 device (the capture API requires one in our process), and Vulkan reads the same GPU memory through a shared NT handle. No further copy.
  - **GPU-to-GPU copy:** copy the shared texture into a Vulkan-owned texture. One cheap GPU copy, simpler synchronisation.
  - **CPU round trip:** read back to RAM, then upload. Cheap for a small source, slow at 4K.
  - The captured frame is the window's size in screen pixels. If the game upscales its 1× image (for example 4×) before presenting, the capture is the upscaled image, and the native raster must be recovered by detection (SRC-2).
- **Q:** How does the capture texture (Direct3D 11) reach Vulkan: shared NT handle with fence or keyed mutex, or CPU readback and upload? Is zero-copy reliable on your GPU and worth it?
- **Why:** A CPU copy costs several ms at 4K and defeats "fully GPU". Unreliable interop on one vendor changes the architecture. For a small source the CPU copy is trivial.
- **Who:** SPIKE (SP-1).
- **Default:** Measure both. Sonnet and Opus favour zero-copy with CPU as a diagnostic fallback, Fable favours CPU copy first and shared textures only if latency demands. Pick after measuring the capture size you actually use.
- **IMPLEMENTED (2026-11-18):** the CPU round trip (staging texture, mapped copy, Vulkan staging buffer), as the first working path. Windows.Graphics.Capture only delivers a frame when the window content changes. The shared-handle path is not measured yet.

### CAP-3 Bit-exactness of the captured pixels
- **Note (2026-11-18):** output is SDR only. Desktop HDR is off on the dev machine: the owner says so and both active monitors (Dell P2418D) have no HDR. The registry showed no HDR flag either way, so this rests on the hardware. If HDR were on, the capture would be scRGB with an SDR white level, and original code values would have to be recovered.
- **Q:** Are captured pixels identical to what the application rendered under DPI virtualisation, DWM scaling, HDR desktop mode, Auto HDR, Night Light and ICC colour management? With the desktop in HDR, how are the original SDR code values recovered (FP16 scRGB, SDR white-level slider)?
- **Why:** Any scaling or colour transform before the model silently breaks the 1:1 premise.
- **Who:** SPIKE (SP-5).
- **Default:** Assume the native composed surface and verify with a test-pattern application. Ship a "capture integrity" check in the tool.

### CAP-4 Covered, off-screen, minimised or throttled sources
- **Q:** When the fullscreen output covers the source on the same monitor, or the source is off-screen, minimised or on another virtual desktop, does the source keep rendering and does the capture keep delivering frames?
- **Why:** This is the natural one-monitor scenario. Engines and browsers may throttle or pause when occluded or unfocused, Windows may drop a flip-model game out of independent flip, and some capture implementations refuse minimised windows.
- **Who:** SPIKE (SP-1, blocking), on several sources (emulators on D3D11/Vulkan/OpenGL, a Unity or Godot game, DOSBox, a browser).
- **Default:** None until measured. Fallbacks to evaluate if it fails: keep 1 px of the source visible, move the source to a virtual display, or require a second monitor.

### CAP-5 Exclusive fullscreen, protected content, anti-cheat
- **Q:** What is the policy for exclusive-fullscreen targets, DRM or capture-excluded windows (`WDA_EXCLUDEFROMCAPTURE`) and anti-cheat-protected games, which capture as black or not at all?
- **Why:** No external method can legitimately bypass these. On one monitor an exclusive-fullscreen game cannot coexist with a fullscreen output.
- **Who:** USER.
- **Default:** Windowed and borderless targets only. Detect black or stale frames and show a clear message. No bypass attempts.

### CAP-6 Cursor
- **Q:** Is the source's cursor captured into the picture (and thus curved, masked and blurred), drawn sharp on top, or hidden? How is the host cursor handled over the output?
- **Why:** With a click-through output the system draws the cursor in host space, while the captured cursor appears in tube space: double or misplaced cursors. Many pixel-art games draw their own.
- **Who:** USER plus SPIKE.
- **Default:** Capture without cursor, hide the host cursor over the output where a clean method exists, document the rest. Optional inclusion through the tube.

### CAP-7 Target selection and lifecycle
- **Q:** How is the target chosen (system picker, window list, hotkey on the foreground window, profile match by executable and window class), and what happens on resize, move, DPI change, close, relaunch (new window handle) and crash? What about apps that render into child or separate windows, and how is the capture region defined (client area, numeric crop)?
- **Why:** Robustness and per-game profiles depend on it. Per-window capture of the wrong HWND gives black or UI-only frames.
- **Who:** USER plus SPIKE against your real app list.
- **Default:** Window list in the menu, auto-reattach by executable and window class, client area with a per-profile numeric crop. Test your top 5 sources early.

### CAP-8 Capture latency and delivery rate
- **Q:** How much latency does capture add on your machine, does it deliver every source frame at 60, 120, 144 Hz without drops or duplicates, and are timestamps reliable enough to reconstruct source cadence?
- **Why:** Capture frame delivery is tied to composition. Reports exist of a cap near 50 fps even with `MinUpdateInterval` = 0 on some builds. Drops and duplicates break the temporal model and pacing, and external capture has a latency floor that injection does not.
- **Who:** SPIKE (SP-3, blocking).
- **Default:** None until measured.

### CAP-9 Capture pixel format and precision
- **TESTED (2026-11-18, T-46):** dark-ramp banding check (	ests/stage_output.rs, dark_ramp_banding_with_and_without_dither): source codes 0 to 31, gamma 2.4, exposure 0.5, pure 2.2 host. Without dither the band error is up to 0.55 code (rounding plus the RTX A2000's ROP fuzz of 0.05); with dither no pixel is more than 0.98 code from the exact value and band means are within 0.01 (ordered, noise). The 8-bit source codes themselves are not enough to avoid banding on their own: the dither only removes the output quantisation.
- **Q:** Is 8-bit BGRA capture enough, or is an FP16 path needed (HDR sources)? Does 8-bit quantisation of dark codes before the tube gamma cause visible banding?
- **Why:** Gamma 2.4 and rolling accumulation expose banding in dark tones.
- **Who:** SPIKE.
- **Default:** 8-bit capture, FP16 internal pipeline.

## Output window and input

### OUT-1 Reference topology
- **DECISION (2026-11-18):** one screen must work. The output covers the source on the same monitor, so CAP-4 and SP-1 decide feasibility. Fallbacks to test if a covered source stops delivering frames: keep 1 px of the source visible, move the source to a virtual display, or use a second monitor (which then becomes a bonus, not the target).
- **Q:** Is the primary use case one monitor (output covers the game) or two (game on one, tube on the other)? Must monitors with different refresh rates, HDR states and DPI work together? Can the output follow a window across monitors?
- **Why:** One monitor raises occlusion, cursor and focus problems. Two monitors raise mixed-refresh pacing and possibly cross-adapter copies.
- **Who:** USER.
- **Default:** Two-monitor is the guaranteed mode, one-monitor is the design target if SP-1 and SP-2 show it works. One explicitly chosen output monitor, the target anywhere. (Opus and Fable would make one-monitor the reference case because it is the harder one.)

### OUT-2 No-focus fullscreen presentation
- **Q:** Does a borderless, topmost, monitor-sized, non-activating (`WS_EX_NOACTIVATE`), optionally click-through window present at full refresh with Vulkan, with HDR, without tearing and without forcing extra compositor latency? Does the driver ever try exclusive fullscreen? Does Windows or the driver deprioritise a never-foreground process's GPU work and presents?
- **Why:** This is the defining requirement. Layered or transparent styles can disable independent flip. Exclusive fullscreen normally requires focus. Vulkan on Windows may be layered on DXGI depending on the driver, which changes latency, VRR and HDR. A missed refresh under rolling scan is a visible flash.
- **Who:** SPIKE (SP-2, blocking), on each target vendor if possible.
- **Default:** Borderless topmost non-activating window composed by the compositor, exclusive fullscreen explicitly disallowed. Measure present mode (PresentMon), then decide.

### OUT-3 Meaning of "no focus steal" and where the menu lives
- **Q:** What exactly must hold: output never becomes foreground, clicking it does not activate it, keyboard keeps going to the target, the target is not paused by a deactivate message, the target's cursor clip and raw input stay valid? And where does the ImGui menu live: inside the output window, in a separate control window, or in a toggled edit mode?
- **Why:** The need contradicts itself here (T-3). A non-activating window gets mouse clicks but not keyboard focus (text fields and keyboard navigation fail), a click-through window gets nothing, and games may hide or clip the cursor.
- **Who:** USER, checked by SPIKE (SP-2).
- **Options:** mouse-only menu in the non-activating window; separate control window that takes focus on purpose (can sit on a second monitor); an explicit "edit mode" hotkey that temporarily accepts focus; gamepad or hotkey navigation.
- **Default:** Output never activates. Mouse-only live tweaking in the output plus a separate control window for text entry, and a hotkey toggle for the on-output overlay (hidden by default, excluded from screenshots).

### OUT-4 Mouse, click-through and mouse-driven games
- **Q:** Should clicks pass through the output to the window underneath, and are mouse-driven games in scope, which would need pointer remapping between the curved, scaled picture and the source window?
- **Why:** Click-through only works if the source sits under the picture at the same coordinates. Remapping needs input synthesis, which resembles injection and is an anti-cheat red flag, contradicting "no injection".
- **Who:** USER.
- **Options:** keyboard and gamepad content only; align the source under a non-curved picture; synthesise remapped input (fragile).
- **Default:** Keyboard and gamepad games only. Output is click-through. Mouse remapping later, never input hooks.

### OUT-5 Hotkeys and the escape path
- **Q:** Which global actions need hotkeys (toggle effect, toggle menu, cycle preset, screenshot, bypass, quit), and what is the guaranteed way out when a topmost, possibly click-through window covers the desktop?
- **Why:** Registered hotkeys swallow the key but do not steal focus. Low-level keyboard hooks look like keyloggers to antivirus and anti-cheat. A focusless fullscreen window can lock you out of the desktop visually.
- **Who:** USER.
- **Default:** A small set of registered hotkeys with a configurable, rarely used chord, no low-level hooks, and a fixed panic hotkey that always returns to windowed mode.
- **IMPLEMENTED (2026-11-18), first part:** one registered hotkey, Ctrl+Alt+Q, which quits (it does not return to windowed mode yet). No hooks. A fullscreen output refuses to start if the hotkey cannot be registered.

### OUT-6 Present mode, vsync, tearing and VRR
- **Q:** Default present mode (FIFO, mailbox, immediate with tearing)? Is tearing ever acceptable? What happens when the frame budget is missed? Does VRR engage for an unfocused output window, and is it wanted given rolling scan needs a fixed refresh?
- **Why:** It trades latency against tearing and pacing, and interacts with VRR and the temporal model. VRR is the cleanest answer to cadence mismatch for 60 Hz sources, but is normally tied to the foreground application and can cause OLED flicker.
- **Who:** USER plus SPIKE (SP-10).
- **Default:** Vsync, never tear, immediate only as an opt-in low-latency mode. On overrun, drop to a lower quality tier and report it. Do not depend on VRR.
- **IMPLEMENTED (2026-11-18), first part:** FIFO (vsync) only. No immediate mode, no quality tiers, VRR and latency not measured.

### OUT-7 Placement and scale of the tube on the output
- **IMPLEMENTED (2026-11-18, T-30), first part:** `tube_height_fraction` (default 1.0) of the output height, black surround (the whole output outside the tube rectangle, exactly black), centred, free resize (placement is recomputed every frame), `--integer` = whole host pixels per source row when at least 1. The live px-per-triad readout and the "true physical size" mode (EDID) are not done (UI tasks, T-52 onwards).
- **Q:** How is the virtual tube mapped to the output: fill height, fixed 4:3, true physical size (needs panel size and viewing distance), integer scale, user-set size, black surround; and how does it behave in a resizable window?
- **Why:** It defines pixels per triad, halation radius in pixels and moiré. A small window destroys the host-pixel budget, and a 20 in tube filling a 32 in panel is a magnified tube.
- **Who:** USER.
- **Default:** Fit the tube's visible raster to the output at a user-set fraction of screen height, black surround, free resize with the live px-per-triad figure shown. A "true physical size" mode using EDID with manual override.

### OUT-8 Screenshots, recording, streaming
- **Q:** Are screenshots wanted (PNG and linear EXR), and should OBS or Game Bar be able to capture the output, in SDR or HDR?
- **Why:** Validation needs reproducible captures. HDR and subpixel-aligned masks look wrong when captured to SDR by third parties.
- **Who:** USER.
- **Default:** Built-in screenshot of the linear buffer (EXR) plus tone-mapped PNG. Leave recording to external tools. Output window excluded from capture only on request.

## Timing and latency

### TIM-1 Latency budget
- **Q:** What added end-to-end latency (source Present to photons) is acceptable, in frames or ms, at 60 Hz and at 240 Hz, and how will it be measured?
- **Why:** Capture adds at least about a frame, the compositor adds more, and the beam model adds delay by design (the bottom of the image is later). Retro and rhythm games are latency-sensitive, and rolling scan wants buffered, stable frames.
- **Who:** USER, then DATA (photodiode or high-speed camera filming both windows).
- **Default:** 1 to 2 frames at 60 Hz in "low latency" mode (at most one output refresh above the measured capture floor), plus up to 1 frame more in a "temporal accuracy" mode. State the measured number in the docs.

### TIM-2 Cadence mismatch policy
- **Q:** When source rate (60.1, 59.94, 50, 70, 30 fps, variable) differs from output rate (60, 120, 144, 240), do we accept periodic duplicates and drops, resample in time (non-physical), use VRR, or require the user to match rates?
- **Why:** 60.1 against 60 Hz drops a frame every 10 s. With a 144 Hz host and a 60 Hz source, rolling-scan bands do not align (2.4 sub-frames), and any slip shows as a flash or band.
- **Who:** USER after SPIKE (SP-3).
- **Default:** Latest-frame-wins with duplicate-frame detection and recorded timestamps. Rolling scan only when host = N × source within tolerance. Document integer-multiple refresh as the recommended setup.

## Physical model

### PHY-1 First tube families
- **Q:** Which two or three families come first: aperture grille (Trinitron, PVM/BVM, Diamondtron), consumer slot-mask TV, NEC CromaClear, arcade slot, delta-dot VGA monitor?
- **Why:** Each needs its own mask geometry, vertical structure, data and preset. Aperture grille has no vertical mask structure (no scan moiré) and has sourced PVM pitches, but PVM-class pitch is hardest on the host-pixel budget. Coarse slot TV masks are resolvable at 4K but need the more complex slot geometry.
- **Who:** USER.
- **Default:** Aperture grille first (PVM-20M4U class, 14 in and 20 in), consumer slot TV second, 15 in delta-dot VGA monitor third (DOS content).

### PHY-2 Host-pixel budget policy
- **DECISION (2026-11-18):** default is A, area-averaged mask with a live pixels-per-triad readout. D must also exist: when the chosen tube is too fine for the host, the UI suggests a coarser tube preset that the host can resolve. Enlarged cell (B) and host subpixels (C, see HW-6) were not requested and are deferred. Monitor resolution and panel type are still needed to compute the tiers.
- **Q:** When the host offers fewer than about 3 px per triad (PVM masks on 4K, anything on 1080p), what happens: area-filtered mask, zoomed inspection mode, enlarged virtual cell, host subpixels, supersampling, or refuse? What accuracy statement applies in each regime?
- **Why:** This is the central tension between "physically accurate" and real hardware. It decides the mask code, the UI readout, the look of the flagship presets and the honesty of the claim.
- **Who:** USER.
- **Options:** tier A at least 6 px per triad (resolved); tier B area-integrated (3 to 6); tier C prefiltered average only (under 3); tier D enlarged cell (non-physical).
- **Default:** Automatic tiering by computed px per triad with a visible readout, area-integrated in B and C, zoom as an inspection mode, enlarged cell only on explicit request and labelled non-physical, host subpixels per HW-6.

### PHY-3 Physical scale and units of the virtual tube
- **IMPLEMENTED (2026-11-18, T-30):** the tube is `tube_visible_height_mm` (290 for the PVM-20M4U class, CRT.md reference table) x `tube_aspect`; the CPU converts to host pixels (`px_per_mm`, `FrameConstants`, 4.966 on 2560x1440 at full height), ratios preserved when scaled. Lengths of later stages are tube mm times `px_per_mm`. Viewing distance (curvature perspective) comes with T-37.
- **Q:** Is the tube modelled at its real size with a viewing distance, or scaled to fill the output while keeping triad count? Are parameters stored and shown in physical units (mm, kV, µA or normalised drive, MHz, ms, cd/m², CIE xy) and converted to shader units on the CPU?
- **Why:** Halation radius (17 to 25 mm), spot FWHM and pitch only mean something relative to a tube size. Physical units make presets portable across host displays and traceable to `CRT.md`, while shader units are host-dependent.
- **Who:** USER.
- **Default:** All lengths in tube millimetres, mapped to host pixels through the displayed tube width, ratios preserved when scaled. Viewing distance only for curvature perspective.

### PHY-4 Named models, presets and provenance
- **DECISION (2026-11-18, supersedes the model-preset list below):** the only structural choice is the mask type, a 3-way selector (aperture grille, slot mask, delta dot). Every other element of the look (pitch, spot size and growth, glass, halation, gamma, primaries, white point, geometry, scanline mode) is an independent parameter in physical units, each with a confidence tag. Each mask type carries its own geometry parameters (slot period and bridge for slot mask, hexagonal layout for delta dot, damper wires for aperture grille). Named CRT models are not part of the code. They ship as starting-point presets, data files only (parameter sets typical of well-known monitors, 3 to 5, proposal in the list below), which the user can load and then edit freely. They are labelled "based on published data for model X" and every value keeps its confidence tag. The expert-mode warnings of PHY-16 and the readouts of UX-1 (pixels per triad, σ over line pitch) guard against impossible tubes.
- **DECISION (2026-11-18):** the software must offer 3 to 5 selectable, well-known CRT models. Preset names are descriptive labels ("based on published data for model X"), each parameter keeps its confidence tag. This overrides the generic-names-only default (PRJ-3) for a personal project. Proposed list, drawn from the `CRT.md` starting presets, with the host pixels per triad on the 2560×1440 monitors (4:3 image at full height, 1920 px wide):

  | # | Preset | Why | Mask | Triads across | Host px per triad |
  |---|---|---|---|---|---|
  | 1 | Sony PVM-20M4U (pro monitor, 0.31 mm) | The reference look for clean 240p, deep scanlines | Aperture grille | about 1245 | about 1.5, mask averaged |
  | 2 | Sony Trinitron KV-class consumer TV (about 0.78 mm) | The classic console-on-a-TV look | Aperture grille | about 660 | about 2.9 |
  | 3 | Consumer 26 in slot-mask TV (0.80 mm) | Generic living-room set, brick-pattern mask | Slot mask | about 660 | about 2.9 |
  | 4 | 15 in VGA monitor (0.28 mm dot, delta) | DOS and PC games of the 1990s | Delta dot | about 1157 | about 1.7, mask averaged |
  | 5 | 19 in arcade monitor (about 0.75 mm slot) | Arcade look, the coarsest mask, so the most visible | Slot mask | about 480 | about 4 |

  Presets 2, 3 and 5 are the ones that option D (coarser tube) would suggest when 1 or 4 cannot be resolved.

  **Extended list, up to 10 (owner agreed, 2026-11-18; supersedes the five above).** Pitch and size data come from `CRT.md`. Host px per triad is for a 1920 px wide image on the P2418D monitors.

  | # | Preset | Mask | Pitch (mm) | Host px per triad | Data confidence |
  |---|---|---|---|---|---|
  | 1 | Sony PVM-20M4U (pro monitor) | Aperture grille | 0.31 | about 1.5 | Pitch sourced, spot and halation estimated |
  | 2 | Sony BVM-20F1U (broadcast reference) | Aperture grille | 0.30 | about 1.5 | Pitch sourced, D65, BT.1886 origin |
  | 3 | Sony Trinitron KV-class consumer TV | Aperture grille | about 0.78 | about 2.9 | Mostly estimated |
  | 4 | Sony GDM-F520 (21 in PC monitor) | Aperture grille | 0.22 | about 1.0 | Pitch sourced |
  | 5 | Consumer 26 in slot-mask TV | Slot mask | 0.80 | about 2.9 | Estimated |
  | 6 | 13 in consumer TV (GE-class) | Slot mask | 0.60 | about 4.4 | Pitch measured by a third party |
  | 7 | NEC CromaClear 17 in | Slot mask (elliptical) | 0.25 | about 1.5 | Pitch sourced |
  | 8 | 19 in arcade monitor | Slot mask | about 0.75 | about 4 | Estimated |
  | 9 | 15 in VGA monitor | Delta dot | 0.28 diagonal | about 1.7 | Sourced class values |
  | 10 | 17 in good delta-dot monitor | Delta dot | 0.26 diagonal | about 1.3 | Sourced class values |

  Only presets 3, 5, 6 and 8 are close to resolvable on this host. The rest rely on the area-averaged mask. Final list open for the owner to change.
- **Q:** Are presets generic classes ("20 in professional aperture grille, 0.31 mm") or named real units (PVM-20M4U)? Who creates presets, in what format, and how is provenance stored?
- **Why:** A model name implies measured fidelity the data cannot support for most parameters, and raises a trademark question (PRJ-3). Preset provenance is the trust anchor.
- **Who:** USER.
- **Default:** Parametric tube with class names and "based on published data for model X" where sourced. Seed with the nine starting presets from `CRT.md`. Every parameter carries a confidence tag (S, D, K, E) and citation, shown in the UI.

### PHY-5 Policy for missing data
- **Q:** When data is missing or conflicting (spot size versus current, decay tails, halation amplitude, ABL, consumer pitch, glass thickness, Trinitron curvature radius), what does the product do: expose raw tunables with documented ranges, ship a default tagged by confidence, refuse a preset, or collect its own measurements? Will you invest in data (buying or borrowing a tube, measuring)?
- **Why:** `CRT.md` lists these as the main gaps. The project promises "physical" but most numbers are estimates.
- **Who:** USER, then DATA.
- **Default:** Tagged defaults and sliders. No data purchase until a sensitivity analysis (offline reference renderer, SP-7) shows which unknowns change the picture most at your host resolution. Expected top three: spot size versus current, source transfer function, mask policy.

### PHY-6 Gun law and black-level model
- **Q:** Which gun model: GOG power law with cutoff, BT.1886, BT.1886 Annex 1 (2.6 above V = 0.35, 3.0 below), or pure power? Which default gamma, and which semantics for brightness (offset before the power law) and contrast (gain)?
- **Why:** It defines tonal response, interacts with the source EOTF policy (SRC-5), and with spot growth because contrast raises beam current.
- **Who:** USER plus RESEARCH.
- **Default:** GOG with exponent 2.4 (range 2.2 to 2.6), per-channel cutoff and gain, BT.1886 Annex 1 as an option.

### PHY-7 Beam model scope
- **Q:** Which beam terms are in the first version: Gaussian y-profile, generalised Gaussian with a tail, current-dependent spot, horizontal kernel, position-dependent spot (dynamic focus, corner elongation), per-channel convergence? Is the spot driven by absolute beam current (µA, so datasheet tables apply) or normalised drive with FWHM at mid and peak per preset?
- **Why:** Only one spot-versus-current table exists (a monochrome medical tube), the exponent ranges from 0.29 to 0.5, and each term costs shader time and exposes unvalidated parameters. Absolute current invites false precision.
- **Who:** USER plus RESEARCH.
- **Default:** Gaussian y-profile with current-dependent width (d² = d0² + k·I^(2n)), normalised drive with FWHM at mid and at peak per preset, optional tail. Position dependence and convergence in a second phase.

### PHY-8 Mask formulation
- **Q:** Is the product form L = M × B accepted (valid when the spot covers at least 1.5 pitches), or must the mask integrate beam energy over each aperture? Analytic procedural mask with area integration, or precomputed textures? Are mask-to-screen parallax, pitch magnification toward the edges and variable-pitch grilles (FW900 0.23 to 0.27) in scope?
- **Why:** The product form fails for small spots on fine monitors at low current. Procedural masks scale with any pitch and give correct area integration, textures quantise pitch. Pitch changes at edges matter for moiré.
- **Who:** RESEARCH plus SPIKE (SP-6, SP-7).
- **Default:** Product form with an analytic area prefilter, validated against full integration in the offline reference. Constant pitch first, variable pitch later. Show the validity condition in the UI.

### PHY-9 Signal-domain stage
- **DECISION (2026-11-18):** composite video is in scope as an optional module placed before the tube model, off by default, after the first usable version. The captured RGB is re-encoded to composite and decoded again (dot crawl, rainbow, chroma bleed, artifact colours, with S-Video and RGB as parameter variants such as chroma bandwidth and crosstalk). It needs the source's native pixel clock and exact pixel alignment, so it depends on native-raster recovery (SRC-2). Existing implementations are read for ideas only. Licences are checked before any code is reused, and GPL code is never copied (PRJ-2). The first version keeps the defined insertion point and the asymmetric horizontal filter in the gamma domain.
- **Q:** Is the video-amplifier stage in the first version: horizontal low-pass in the voltage domain, asymmetric rise/fall slew (adjacent-pixel nonlinearity), peaking, cable reflection? How is the source pixel clock defined for a captured window, since bandwidth in MHz needs one? Is composite/NTSC ever in scope?
- **Why:** `CRT.md` rule 1: blurring in linear light is wrong, and one-pixel vertical lines come out 5 to over 50 % dimmer than horizontal ones. The pixel clock is not provided by the capture and must come from the video-mode preset.
- **Who:** USER.
- **Default:** Asymmetric horizontal filter in the gamma domain in the first version, with a per-profile "bandwidth in source pixels" parameter. Composite excluded, with a defined insertion point before the tube model.

### PHY-10 Optical stage
- **Q:** Which optical effects come first: halation ring (radius 1.74 × glass thickness, 17 to 25 mm, hundreds of host pixels), veiling glare, phosphor-layer diffusion, tint and thickness uniformity, vignette, reflections? Which default amplitude within the 0.5 to 5 % uncertainty?
- **Why:** The ring is non-separable and dominates GPU cost, and the amplitude range changes the look.
- **Who:** USER for default and priority, SPIKE for cost (SP-8: FFT versus mip pyramid versus ring sampling).
- **Default:** Halation (low-resolution ring kernel, about 1.5 %), short-range diffusion and vignette in the first optical milestone. Reflections and ambient out.

### PHY-11 Geometry and viewing model
- **Q:** Which geometry is in scope: spherical and cylindrical curvature, flat outer face with curved inner face (refraction, concave look), pincushion and S-linearity residuals, overscan, rounded corners, tilt? Is the mask laid out in tube-surface coordinates with the beam distortion separate? Is curvature a perspective view from a fixed eye point?
- **Why:** Physically the mask is fixed to the tube and geometry errors move the beam, so warping everything with one UV transform gives wrong moiré. Curvature breaks input alignment (OUT-4), worsens host pixels per triad at the edges, and has no sourced radius for cylindrical Trinitrons.
- **Who:** USER plus RESEARCH.
- **Default:** Flat or simple spherical/cylindrical mapping, overscan and rounded corners first. Mask in tube coordinates. Ray-cast curvature through the glass, convergence polynomials and distortion terms later. Fixed centred eye at a stated distance. Radii as tunables.

### PHY-12 Temporal scope and phosphor decay
- **Q:** Which temporal model in the first version: none (frame-integrated), 60 Hz frame-feedback afterglow, or rolling sub-frame scan at 120/240/480 Hz? Which decay model and numbers: the `CRT.md` two-term fit to the Sony patent residuals, the fast or slow literature tables, or measured values? Is flicker at 50 to 60 Hz acceptable?
- **Why:** Faithful rolling scan needs N times the peak luminance on top of mask loss (HDR), FP16 linear history buffers, exact cadence, fast pixels, panel ABL off and no strobe or FALD. At 240 Hz only 4 sub-frames per field exist. Published decay times differ by about 100×, so only measurement settles them, and the green and blue tail drives the ghost trail: at 60 Hz about 5 % of a frame's green and blue energy reaches the next frame, then about 3 % and 2 % the frames after; red is negligible (T-63, Kuhn 2002 P22 fit, CRT.md "Decay and persistence"). Real impulse behaviour is visible flicker and a photosensitivity concern.
- **Who:** USER plus DATA.
- **Default:** Frame-integrated first with an optional colour-dependent afterglow feedback term, `CRT.md` fitted decay (tagged [D]) exposed as sliders. Rolling scan as a later milestone, only on displays that pass a capability check, off by default with a warning.

### PHY-13 Whole-frame dynamics
- **Q:** Are ABL, raster breathing (EHT sag) and DC-restoration errors in the first version?
- **Why:** They need a frame-wide average (a reduction pass, a frame of latency or an estimate), a time constant, and temporal state, with constants from memory, and they fight the host panel's own ABL.
- **Who:** USER.
- **Default:** Not in the first version. The architecture keeps a per-frame global state for them.

### PHY-14 Second-order effects
- **DECISION (2026-11-18):** ageing and burn-in are in scope, opt-in and off by default:
  - **Ageing (tube-wide):** phosphor efficiency loss per colour (blue wears fastest, so the white point drifts warm), lower maximum beam current and cutoff drift from cathode wear, slightly larger spot. Parameters, not a simulation of hours.
  - **Burn-in (local):** a per-colour wear map on the tube surface, accumulating beam energy over time. Each element's efficiency is a decreasing function of its accumulated charge, applied in the phosphor emission stage. A persistent HUD or letterbox bar leaves a ghost, tinted by the colour that wore.
  - **Time:** wear takes thousands of real hours, so the model needs a time-acceleration control, a "burn in now" tool and a reset ("new tube"). The tube state is saved to disk between sessions and is tied to tube coordinates, not to the output window.
  - **Data:** only one sourced figure (15 % loss after 20,000 h at 350 cd/m², one datasheet). The colour ratios and wear curve are estimates [E].
- **Q:** Which of these are in the first version: phosphor saturation (blue and green sulfides), backscatter desaturation, convergence offsets, damper wires, doming, purity patches, black-matrix diffuse reflection?
- **Why:** Each is cheap to add but invites a parameter without data.
- **Who:** USER.
- **Default:** Damper wires (cheap, iconic) and polynomial convergence in the second phase, saturation as a per-channel soft clip behind a toggle, doming and purity out.

### PHY-15 Imperfections and determinism
- **Q:** Are random terms (dot size variation, mask irregularity, jitter, line swim) included, and must they be seeded and reproducible?
- **Why:** Reproducibility for tests and screenshots, and noise affects moiré.
- **Who:** USER.
- **Default:** None in the first version. Any added noise is seeded.

### PHY-16 Independent sliders or coupled physics
- **Q:** Are parameters independent controls, or coupled through the operating point (raising contrast raises beam current, spot size, ABL activity and raster size together)?
- **Why:** "All parameters tunable live" and "physical" pull apart: free independent sliders allow tubes that cannot exist.
- **Who:** USER.
- **Default:** Two layers. Physical tube parameters (preset, rarely touched) and the tube's own user controls (brightness, contrast, focus, colour temperature) that act through the physics. An expert mode unlocks everything.
- **DECISION (2026-11-18):** the owner chose independent parameters (mask type is a 3-way choice, every other look parameter is independent, "impossible tubes" are allowed, possibly with warnings). The two-layer default above is superseded.

## Colour and HDR

### COL-1 HDR requirement and SDR behaviour
- **IMPLEMENTED (2026-11-18, T-46):** exposure, soft shoulder (output_knee, 1 = hard clip) and the clip counters (ClipCounts: over white, out of gamut) are in the encode stage; no hidden normalisation of a non-D65 tube white (its blue can exceed panel white, the counter shows it). See ARCHITECTURE 3.7.
- **DECISION (2026-11-18):** the owner does not want HDR output or HDR colour, and left the internal precision to the assistant. Chosen:
  - **Output:** SDR only (sRGB host, 8-bit, or 10-bit when available, with final dither). HDR output is out of scope. The present stage is kept behind an interface so an HDR path could be added later.
  - **Internal computation:** linear-light FP16 (FP32 where banding shows), with the tube's white at 1.0 and no clamp inside the pipeline. Reason: the gun power law, mask sampling (a lit element is 4 to 6 times brighter than the area average), halation and glow sums, and afterglow accumulation all need unclamped linear values, and 8-bit intermediates band in the dark tones.
  - **Mask loss in SDR:** with the area-averaged mask (PHY-2 option A) the mask loss is averaged out, so there is nothing to compensate. A single exposure control maps tube white to panel white. When the mask is resolved (tier A or B), element peaks above panel white are soft-clipped and a clip indicator is shown. No hidden bloom compensation (SCP-8 stage stays optional and labelled).
  - **Colour:** tube primaries and white converted to sRGB primaries by a 3×3 matrix in linear light, clipped in linear light with a warning (COL-2 default). No wide gamut.
  - **Consequences:** SP-4 (HDR path) is deferred. Rolling scan (PHY-12, M4) is effectively out for SDR, since it needs peak luminance headroom, and stays a later option. HW-7 OLED ABL concerns shrink but static-mask burn-in remains.
- **Q:** Is HDR required for the "reference" experience (as Sony Megatron assumes), at what minimum peak (600 or 1000 nits)? On SDR, where the physical model needs 3 to 7× headroom, do we render physically and accept a dark image (CRT white about 15 to 35 % of panel white), normalise so the mask-averaged white equals panel white (elements clip), or apply a non-physical compensation? Which HDR format: HDR10 PQ or scRGB FP16?
- **Why:** `CRT.md` says a pixel-level mask with visible scanlines needs about 400 to 700 cd/m² for a 100 cd/m² CRT white, and rolling scan multiplies that again. OLEDs only reach peak on small windows (panel ABL), which conflicts with full-field mask patterns. The answer defines whether SDR is a supported target at all.
- **Who:** USER plus SPIKE (SP-4).
- **Default:** HDR recommended, not required. Reference CRT white configurable (default 100 cd/m²). SDR renders physically with a global exposure slider the user sets knowingly, clipping shown. Format chosen after SP-4. HDR is the first major milestone after the first usable version.

### COL-2 Colour fidelity target
- **IMPLEMENTED (2026-11-18, T-46):** the 3x3 matrix from the tube primaries and white to sRGB with D65 (crt_model::output::tube_to_display) and the output_match_srgb toggle (identity). Open for the owner or T-50: the toggle defaults to on (identity) so that the default chain stays an exact identity; COL-2's tube-faithful default is reached by presets that turn it off.
- **Q:** Does the output reproduce the tube's own primaries and white (so game reds look less saturated and D93 looks bluish) or colour-manage to what the artist saw on sRGB? Which primaries and white points ship (SMPTE-C, EBU, measured Trinitron, P22 average, custom xy; D65 or D93 default)? What happens to out-of-gamut colours?
- **Why:** A faithful 9300 K picture looks blue next to a D65 desktop. Console art was authored on real tubes, PC art on sRGB monitors. A real CRT does not colour-convert, and a white-point change is a per-channel gain, not a chromatic adaptation.
- **Who:** USER plus DATA (do you calibrate your display).
- **Default:** Tube-faithful and absolute, tube white selectable per preset (D65 for professional presets, 9300 K for consumer and PC presets), one-click "match sRGB colours" toggle. Linear 3×3 conversion from tube to host primaries, clip in linear light with a warning, proper gamut mapping later.

### COL-3 Absolute or relative luminance and host EOTF
- **IMPLEMENTED (2026-11-18, T-46):** host EOTF inverse in encode: pure power (default 2.2) or piecewise sRGB, from Settings.host; checked against the oracle for all 256 codes (20/255: pure 2.2 host gives code 20 for gamma 2.2 light and 16 for gamma 2.4 light; sRGB host 12 and 7). Still open: moving the host EOTF into the parameter table (settings tasks) and the calibration pattern.
- **Q:** Does the model work in cd/m² (a 100 cd/m² tube white shown at 100 cd/m²) or relative units? Which EOTF is assumed for the host in SDR (piecewise sRGB, pure 2.2, 2.4, calibrated), and is there a calibration step?
- **Why:** Absolute units make ABL, glare and ambient parameters physical but need a characterised host. A host-EOTF mismatch (2.2 versus sRGB) shifts dark levels exactly where the CRT model is sensitive.
- **Who:** USER plus DATA.
- **Default:** Internal absolute units with an output exposure factor so an uncharacterised SDR host still works. A host-gamma parameter (default pure 2.2) and a calibration pattern.

### COL-4 Precision, banding and dithering
- **IMPLEMENTED (2026-11-18, T-46):** final dither of 1 LSB in encode (off, ordered 8x8 Bayer, interleaved gradient noise seeded by the frame index), off by default, never on float targets. Blue noise proper (a texture) is not implemented; the noise mode is blue-noise-like. The presenter records whether a 10-bit SDR surface format is offered (Presenter::ten_bit_offered); the swapchain stays 8-bit, and the dither already scales to 1023 levels.
- **Q:** Which working precision (FP16, FP32 for accumulation) and which output bit depth (8-bit SDR, 10-bit SDR, 10-bit PQ, FP16 scRGB)? Is dithering applied?
- **Why:** Gamma 2.4, a mask that dims 4 to 6×, and a decay tail at 0.5 % (about code 16 of 255) band in 8 bits. Dithering can create moiré, none creates banding. Rolling scan needs exact linear sums across sub-frames. Cross-vendor bit-exactness is not achievable.
- **Who:** SPIKE.
- **Default:** FP16 intermediates (FP32 where tests show banding), 10-bit or FP16 swapchain when available, blue-noise dither only in the final encode, off in test mode. Reproducibility defined as within a tolerance.

## Performance and resources

### PRF-1 Frame-time target
- **Q:** At which output resolution and refresh, on which GPU, in how many ms must it run, with what share left to the game? What is the weakest supported GPU?
- **Why:** "As fast as possible" is not a spec. 4K at 240 Hz is a 4.2 ms budget, shared with the game on the same GPU. The mask integration, ring convolution and multi-scanline beam integration have real cost, and the budget decides the algorithms.
- **Who:** USER (hardware), SPIKE (measure, SP-8).
- **Default:** The full model at the host's native resolution and refresh, using at most 25 % of the frame time (at most half a refresh interval) on your GPU at the default preset.

### PRF-2 Quality tiers
- **Q:** Are quality tiers wanted, what degrades first, and may a lower tier change the look or only the approximation error? Manual, automatic or both?
- **Why:** If "physically accurate" is the differentiator, tiers should change error, not physics.
- **Who:** USER.
- **Default:** Named tiers (reference, standard, fast) that change low-frequency pass resolution, kernel truncation, sub-frame count and mask filtering only. Manual selection with a displayed GPU-time readout. Auto-drop on overrun only per OUT-6.

### PRF-3 Sharing the GPU, power and idle
- **Q:** How much GPU time may the tool take from a game on the same GPU, should it back off, does it render continuously or only on new frames, and do power, heat and battery matter?
- **Why:** A slower game changes the source cadence and corrupts temporal effects. A pixel-art game should not turn the PC into a heater.
- **Who:** USER plus SPIKE.
- **Default:** No automatic back-off in the first version, measure and document the combined load. Re-present the last output when the source is unchanged and no temporal state is evolving. No laptop optimisation.

### PRF-4 Live tunability versus rebuilds
- **Q:** Must every parameter respond within one frame, or may structural parameters (mask geometry, tube size, internal resolution, line multiplier, precomputed kernels) take a short rebuild? Undo, reset to preset and "diff against preset"?
- **Why:** Strict per-frame tunability forbids precomputed tables that would otherwise buy speed.
- **Who:** RESEARCH.
- **Default:** Scalars apply in one frame, structural changes rebuild within about 100 ms with a visible "applying" state, per-parameter reset.

## Tuning UX

### UX-1 Units and derived readouts
- **Q:** Does the UI show physical units with live derived readouts (px per triad, triads per source pixel, σ over line pitch, FWHM over pitch, moiré beat period, headroom used, GPU time per pass), or shader units, or both?
- **Why:** Physical units are the visible proof of the differentiator, and the readouts explain why something looks wrong (budget, moiré).
- **Who:** USER.
- **Default:** Physical units everywhere with tooltips and the confidence tag, derived readouts always visible, shader-unit view behind an advanced toggle.

### UX-2 Presets and profiles
- **Q:** What is the structure: tube presets, host-display profiles, per-source profiles (crop, PAR, video mode, EOTF), global settings, per-executable auto-attach, human-editable files, preset versioning, and sharing?
- **Why:** Mixing tube, host and source settings makes presets non-portable, and display calibration must not live inside a tube preset. Breaking the format later invalidates your work.
- **Who:** USER.
- **Default:** Three layered, versioned, human-editable text files (tube, host, source profile) with schema version, source and confidence fields. Manual attach first, auto-attach by executable and window class later. No RetroArch preset import (compare against ShaderGlass side by side).

### UX-3 Test patterns, A/B and calibration
- **Q:** Which built-in aids are in the first version: test patterns (grey ramp, PLUGE, 1-pixel lines, windows at 10/25/50/100 % APL, checkerboard, flat colour fields, convergence crosshatch, moving bar for decay, moiré grid, subpixel-layout check, HDR peak check), per-stage enable flags, split-screen A/B, full bypass, host calibration (px per triad, banding and gamma check, peak estimate)?
- **Why:** Test patterns double as validation material for SCP-3, remove capture from the loop when tuning, and are required for host calibration.
- **Who:** USER.
- **Default:** Built-in pattern generator as a first-class source type, per-stage flags, split-screen A/B and bypass in the first version. Photo overlay and measurement wizards later.

### UX-4 Parameter count and audience
- **Q:** How is a parameter space of 80 or more values kept usable, and is the UI aimed at a physicist-level tuner or a casual "pick a TV" user?
- **Why:** `CRT.md` criticises crt-guest-advanced for its huge empirical parameter space.
- **Who:** USER.
- **Default:** Preset-first basic view with the tube's own user controls, an expert view grouped by physical stage (signal, gun, beam, mask, phosphor, optics, geometry), bypass and A/B split.

### UX-5 Headless, command line and export
- **Q:** Are a command-line launch (profile and target arguments, no menu), a headless render mode (image or video in, floating-point image out), image and test-pattern inputs, stage-by-stage debug views, and screenshots required?
- **Why:** A launcher workflow and the automated test path both need them, and validation and benchmarking need a repeatable input without a live game.
- **Who:** USER.
- **Default:** Yes: CLI with profile and target arguments, headless render command, image and pattern input from the start, capture recording later.

## Tech stack decisions

### TEC-1 Hard constraints or preferences
- **DECISION (2026-11-18):** all four are the owner's own guess, not constraints. With HDR output dropped (COL-1), the main argument for a Direct3D present path is gone. Assistant recommendation: keep Rust and ImGui. Start with Vulkan on NVIDIA, and switch capture and present to Direct3D 12 only if SP-1 (interop) or SP-2 (no-focus present) is painful. Shaders in GLSL, or in Slang or HLSL compiled to both SPIR-V and DXIL, if a backend switch must stay cheap.
- **Q:** Are Vulkan, GLSL, Rust and Dear ImGui firm requirements or preferences, and why (portability to Linux later, learning, existing skills, shader reuse, performance)? Is a D3D12 or D3D11 path acceptable if Windows-only is permanent?
- **Why:** On Windows the capture API, compositor and present chain are Direct3D and DXGI native. Vulkan adds an interop layer, vendor-dependent present behaviour and less-travelled HDR and VRR paths. If cross-platform is a real goal it must shape the architecture now. The Rust ecosystem's centre of gravity for graphics is elsewhere (WGSL, native GUIs).
- **Who:** USER.
- **Default:** Keep the mandate. Isolate capture and presentation behind interfaces, and allow a D3D11 capture stage and, if SP-4 fails, a DXGI present stage. Confirm interop is acceptable with SP-1 before committing.

### TEC-2 Rust Vulkan binding
- **Q:** ash (raw, explicit), vulkano (safe wrapper), or wgpu (portable, limited extension and present control, prefers WGSL)?
- **Why:** External memory import, HDR swapchain colour spaces, exact present control and exclusive-fullscreen avoidance are extension-level features that an abstraction may hide.
- **Who:** SPIKE (SP-1, SP-4).
- **Default:** ash with a thin in-house layer and a GPU allocator crate.

### TEC-3 Shader toolchain and parameter source of truth
- **Q:** How is GLSL compiled to SPIR-V: build-time glslang or shaderc, runtime shaderc (C++ dependency), or naga (limited GLSL)? How are about 80 parameter definitions (units, ranges, defaults, confidence tags) shared between Rust, GLSL and the menu without three-way drift? Is hot reload required?
- **Why:** It affects build complexity for a single developer, hot-reload productivity on a physics-heavy project, and which GLSL features are usable.
- **Who:** SPIKE (SP-9) plus RESEARCH.
- **Default:** glslang or glslc at build time, runtime compilation with hot reload in development builds, precompiled SPIR-V in release, parameters generated from one declarative table.
- **IMPLEMENTED (2026-11-18), first part:** `glslc` at build time from a fixed shader list in `build.rs`, SPIR-V embedded in the binary. No hot reload and no parameter table yet.

### TEC-4 Windowing library
- **Q:** winit or raw Win32 via the `windows` crate for the output window?
- **Why:** Non-activating, topmost, click-through, capture-excluded windows, mouse-activate handling, per-monitor DPI v2 and two windows need Win32 details a portable library may fight (it may activate the window on show).
- **Who:** SPIKE (SP-2).
- **Default:** Raw Win32 for the output window, winit only if shown to cover all needed behaviour. Per-monitor DPI v2, swapchain at physical pixel size, no OS scaling on the output.
- **IMPLEMENTED (2026-11-18):** raw Win32 through the `windows` crate (non-activating, topmost, tool window, `MA_NOACTIVATE`, per-monitor DPI v2). Click-through and capture exclusion are not done yet.

### TEC-5 ImGui binding
- **Q:** Which Rust binding and Vulkan renderer for Dear ImGui (imgui-rs, dear-imgui-rs, cimgui-based), given maintenance status, and can input be fed manually from a custom Win32 window without focus? Is "ImGui" Dear ImGui specifically, with a native Rust GUI as a fallback only with your agreement?
- **Why:** Bindings lag upstream (imgui-rs 0.12 tracks Dear ImGui 1.89, dear-imgui-rs tracks 1.92 with docking). Binding maintenance is a long-term single-developer risk, and docking or multi-viewport would help the control-window case.
- **Who:** RESEARCH plus SPIKE (SP-2).
- **Default:** Dear ImGui through whichever binding passes a one-day spike with custom input feeding, evaluating dear-imgui-rs first. Own Vulkan renderer if its backend does not fit ash.

### TEC-6 Pass architecture
- **Q:** Fixed ordered pipeline in code, or a data-driven multi-pass graph like slangp? Compute or fragment? Intermediate formats?
- **Why:** Flexibility versus simplicity. A physical pipeline has an order (the seven stages in `CRT.md`) and is not meant to be rearranged. Mask integration, convolution and accumulation suit compute, final present suits fragment.
- **Who:** RESEARCH plus SPIKE.
- **Default:** Fixed ordered stages with compute for model passes and one fragment pass for output encoding and UI composition. No user-defined graphs.

### TEC-7 Shader testing and reference renderer
- **Q:** How are shaders tested and is a high-precision offline reference renderer a first-class deliverable (headless, deterministic, arbitrary supersampling, image in and floating-point out, golden-image diffs with tolerance, analytic tests from `CRT.md`, invariants like energy conservation and symmetry, software Vulkan in CI)? Should it be a slow CPU renderer in double precision, or a headless compute path at high supersampling?
- **Why:** There is no practical way to validate a physical model by eye. Floating-point results differ across GPUs, so pure golden images are flaky. A reference quantifies approximation error of prefiltered masks and truncated tails, and enables the sensitivity analysis in PHY-5.
- **Who:** USER (scope and cost), RESEARCH.
- **Default:** Yes. Headless render command sharing the same shader source, analytic test vectors from SCP-3 as the primary tests, a few golden images per preset with tolerance and only per GPU class, CI on a software Vulkan device if stable. A CPU double-precision crate for the analytic parts (beam, mask integration, halation) is optional, recommended by Sonnet and answered by the headless compute path in Opus and Fable.

### TEC-8 Robustness
- **Q:** What happens on device lost, TDR, monitor hot-plug, HDR toggle, resolution change or capture session loss, and which diagnostics are required (GPU timestamps per stage, frame-time and capture-jitter overlay, logging, RenderDoc compatibility with interop)?
- **Why:** These are frequent in a long-running background tool, and PRF-1 and TIM-2 cannot be settled without measurements.
- **Who:** USER.
- **Default:** Recreate swapchain and capture automatically, full device recreate on device lost, log everything. GPU timestamp queries and an optional stats overlay from day one.

### TEC-9 Build and CI
- **Q:** What build system, pinned toolchain and Vulkan SDK version, and is CI wanted given hosted Windows runners have no GPU and cannot run window capture?
- **Why:** GPU-dependent tests cannot run on hosted runners, and SDK drift changes shader compilation.
- **Who:** USER.
- **Default:** Local-first build with a pinned Rust toolchain and SDK, a local test command, CI limited to build, unit tests and offline-renderer tests on a software rasteriser, cargo-deny for licences.

## Project, legal and process

### PRJ-1 Audience, distribution and licence
- **DECISION (2026-11-18):** personal project. Public-quality architecture stays a goal, licence is not needed now.
- **Q:** Is this strictly personal, shared as binaries, or open source, and under which licence? If binaries are shared, is code signing planned?
- **Why:** It decides the hardware matrix, installer, signing, support, documentation, licensing and how much generality is worth paying for. An unsigned program that captures windows, draws a topmost overlay and registers global hotkeys attracts SmartScreen and antivirus warnings.
- **Who:** USER.
- **Default:** Personal first, public-quality architecture, nothing that prevents release. Source build for personal use, signed release later if ever. MIT/Apache-2.0 dual licence if released.

### PRJ-2 Reuse of third-party code
- **DECISION (2026-11-18):** no GPL shader code is copied. Reading existing shaders for ideas is allowed, implementation is written from physics, papers and permissive sources.
- **Q:** May code be derived from GPL projects (crt-royale, crt-guest-advanced, crt-geom, ShaderGlass, Magpie), or must the implementation be written from physics, papers and permissively licensed sources (the Blur Busters beam simulator is MIT)? Check each source's actual licence.
- **Why:** Deriving from GPL code makes the combined work GPL if distributed. Reading for ideas differs from copying, and formulas and published data are free.
- **Who:** USER plus RESEARCH (licence verification).
- **Default:** Clean-room from `CRT.md` and papers. Adapt permissive code only with attribution. Record every external source consulted and the licence of every dependency.

### PRJ-3 Data licensing and brand names
- **Q:** May pitch tables, datasheet values, hobbyist measurements and photos be embedded in presets and docs, and may presets name vendors and models (Sony, Trinitron, PVM, Diamondtron, CromaClear, Megatron) and mention competing tools?
- **Why:** Facts are generally free, but compiled databases and photos can carry rights, notably in the EU. Naming a preset after a model claims fidelity the data cannot support, and trademark exposure is higher in distribution.
- **Who:** USER, RESEARCH if public.
- **Default:** Store values with citations, do not copy tables wholesale or redistribute third-party photos. Generic class names in the UI, model names only in descriptive citations.

### PRJ-4 Anti-cheat, antivirus and privacy
- **Q:** Do the games you run use anti-cheat (EAC, BattlEye, Vanguard), and is a topmost overlay plus global hotkeys acceptable there? What privacy guarantees apply (captures only the chosen window, offline, no telemetry, frames never leave the GPU unless a screenshot is requested)?
- **Why:** External capture without injection is the best stance, but overlays and low-level hooks are what anti-cheat and antivirus heuristics look for. Capture tools are sensitive.
- **Who:** USER plus SPIKE (SP-11).
- **Default:** Anti-cheat titles are out of scope. No hooks, no injection, no input synthesis. Offline, no telemetry.

### PRJ-5 Time budget and maintenance
- **Q:** How many hours per week, for how long, and who maintains it as Windows, drivers and ImGui bindings change? How much will be AI-written and user-reviewed, and how are AI-sourced facts verified (several `CRT.md` values are tagged [E])?
- **Why:** The physics scope in `CRT.md` can absorb years. The tool sits on OS behaviours that change between builds. Unverified numbers can silently become "physical constants" in presets.
- **Who:** USER.
- **Default:** Timeboxed phases of 2 to 4 weeks with a usable tool after each. Every [E] value stays tagged in the preset until replaced by a sourced value. Pin supported Windows builds and re-run the spikes after updates.

### PRJ-6 First usable version and milestones
- **Q:** What is the smallest version you would use daily, and are the proposed milestones acceptable?
- **Why:** It orders the work and keeps physics from starting before the capture and presentation path is proven.
- **Who:** USER.
- **Default first version:** capture one window, native-resolution recovery, no-focus borderless fullscreen output, one aperture-grille tube with gun law, amplifier stage, current-dependent beam, area-filtered mask and phosphor colour, SDR output, menu with save and load. No HDR, no temporal model, no halation in the first cut.
- **Default milestones:** M0 spikes. M1 first usable version. M2 optical stage (halation, vignette) (HDR output dropped, see COL-1). M3 slot and delta masks, geometry. M4 temporal rolling scan. M5 validation against a reference tube and polish.

### PRJ-7 Documentation as a deliverable
- **Q:** Is a model specification (equations, units, parameter list, provenance) kept separate from `CRT.md` and in sync with the code, and is a user guide needed?
- **Why:** "Physically accurate" is only credible with traceability from each shader parameter to an equation and a source.
- **Who:** USER.
- **Default:** Yes. One model document per stage with provenance tags, one parameter table with units, range, default and tag, and a short user guide.

## Tensions inside the need

These contradictions or conflicts were raised by at least two analysts.
Resolving them is part of answering the questions above.

| Id | Tension | Related questions |
|---|---|---|
| T-1 | "Physically accurate" against the host display. PVM-class masks need 3 to 8 host px per triad and 3 to 7× HDR headroom, which 1080p and SDR outputs cannot give. "Universal" and "physically accurate" collide on the output side | PHY-2, COL-1, HW-4 |
| T-2 | "Each source RGB pixel becomes real RGB light sources" against the physics: the beam is continuous, the mask samples it, and a pixel is a voltage sample after amplifier filtering | SCP-5 |
| T-3 | "Interacting with the output must not steal focus" against an ImGui menu that needs keyboard and mouse focus, a fullscreen window that is normally foreground, and games that hide or clip the cursor | OUT-3, OUT-4 |
| T-4 | "Universal" against "generally 1:1 pixel art". The scanline model needs a known low line count, and a scaled or smoothed capture breaks it | SRC-1, SRC-2 |
| T-5 | "As fast as possible" against a heavy physical model (non-separable halation ring, area-integrated masks, up to 4 sub-frames at 240 Hz) sharing the GPU with the game, with no budget or priority order | PRF-1, PRF-2, PHY-10 |
| T-6 | "As fast as possible" and low latency against "no injection": external capture has a latency floor, and rolling scan adds intentional delay | TIM-1, CAP-8 |
| T-7 | "Physical, not emulated" against the data: the structure can be physical, but most parameters are estimates, and named vendor presets imply precision that does not exist | PHY-4, PHY-5, SCP-1 |
| T-8 | Temporal accuracy against brightness and panel behaviour: sub-frame scanning multiplies peak luminance on top of mask loss, while panels have ABL, local dimming and response time. Temporal accuracy also needs a perfectly paced output from a never-foreground process | PHY-12, COL-1, OUT-2 |
| T-9 | "All parameters tunable live" against "physical": independent sliders allow impossible tubes, and live tunability conflicts with precomputation and parameters that change resource sizes | PHY-16, PRF-4 |
| T-10 | Vulkan against the D3D-native Windows capture and present chain: an interop layer is mandatory, and HDR and pacing control are less direct. GLSL and Dear ImGui on a Rust core add binding-maintenance risk | TEC-1, TEC-2, TEC-5 |
| T-11 | "Runs externally, no injection" against exclusive-fullscreen games, protected content, mouse alignment with curvature, and input forwarding to a covered window (which resembles injection) | CAP-5, OUT-4, PRJ-4 |
| T-12 | Fullscreen output on the same monitor as the captured window: the output covers its own source, and the pointer lives in the source's coordinates, not the picture's | OUT-1, CAP-4 |
| T-13 | Source fidelity against compositor reality: the capture is already a composited 8-bit frame, possibly scaled or colour-managed, and an HDR desktop changes the encoding. The model wants raw DAC code values. Interlace, composite artefacts and sub-frame timing are lost before the model starts | CAP-3, SRC-7 |
| T-14 | Physical colour fidelity against art intent: tube primaries and D93 make colours differ from what an sRGB-authored game shows | COL-2 |
| T-15 | OLED as the ideal output (fast, HDR, true black) against static high-contrast mask patterns (burn-in) and panel ABL fighting the CRT ABL model | HW-7 |
| T-16 | A 4:3 tube against 16:9 content and hosts: much modern pixel art is 16:9, and a 4:3 image uses only 2880 of 3840 columns on 4K | SRC-4 |

## Risks that could invalidate the project

Each risk is retired or confirmed by the listed spike.

| Risk | What would happen | Retired by |
|---|---|---|
| The one-monitor scenario is infeasible: covered sources throttle or stop delivering frames | A second monitor becomes the only supported setup | SP-1 |
| The no-focus window is always compositor-composed (no independent flip), adding a frame and no VRR | Total latency of 2 to 3 frames, unpleasant for action games | SP-2, SP-3 |
| The differentiator is not visible: with an area-filtered mask and a frame-integrated temporal model, much of the physics collapses to blur, gamma and glare | The project loses its reason to exist beyond learning | SP-7 |
| The faithful model looks dimmer, softer or more moiré-prone than tuned-by-eye shaders on your display | Fidelity versus appeal must be decided (SCP-8) | SP-6, SP-7 |
| Key constants stay estimates (spot growth, decay tails, consumer pitch, halation amplitude) | "Physical" claim must be worded honestly | SP-13 |
| (Retired 2026-11-18: output is SDR only) HDR on the host is worse than SDR (aggressive ABL, FALD blooming on fine patterns, unstable EOTF) | HDR cannot be the reference | SP-4 |
| Windows capture or present behaviour changes across builds (the development machine is an Insider build) | Breakage after updates | HW-2 policy, re-run spikes |
| Hybrid graphics: the Intel iGPU composes the desktop while the game and our renderer run on the NVIDIA GPU | Cross-adapter copy of every captured frame, extra latency, no shared-handle handoff, or a renderer too slow on the iGPU | SP-1 (new, see HW-3) |
| Game plus tool exceed the GPU budget, dropping the source cadence | Temporal effects and pacing corrupted | SP-8 |

## Spikes and prototypes

Each spike is small (hours to 2 days), throw-away, and retires named risks and questions.
Recommended order: SP-1 and SP-2 first (they can kill the capture and no-focus premises), then SP-3 and SP-4, with SP-7 in parallel with your answers to the decisions table.

| Id | Spike | Retires | Exit criterion |
|---|---|---|---|
| SP-1 | Capture one window with Windows.Graphics.Capture from Rust and show it in a Vulkan window, with both CPU-copy and shared-texture (NT handle plus fence) handoff. Test windowed, borderless, covered, off-screen, minimised, exclusive-fullscreen, DPI-scaled and HDR-desktop targets, the cursor option, border removal and self-exclusion, on your top 5 sources | CAP-1, CAP-2, CAP-4, CAP-5, CAP-6, CAP-7, CAP-9, HW-2, TEC-2 | Frames arrive from a fully covered window at source rate. Handoff cost measured at 1080p and 4K. Minimum Windows build known |
| SP-2 | Raw Win32 borderless topmost non-activating window (and a click-through variant) with a Vulkan swapchain over a focused target on one monitor. Log the foreground window, check game pause on deactivation, raw-input and cursor-lock games, gamepad, Alt-Tab, flip mode (PresentMon), missed presents under game load. Include a menu window and an in-output overlay with manual ImGui input feeding, hotkeys and the panic exit | OUT-2, OUT-3, OUT-4, OUT-5, TEC-4, TEC-5 | The game never loses focus or throttles. Present mode identified. No missed refresh over 10 minutes |
| SP-3 | End-to-end latency and cadence: a frame-counter source filmed with a high-speed camera or photodiode next to the output. Log capture timestamps for 60, 60.1, 50 and 70 Hz sources against 60, 120, 240 Hz and VRR outputs. Count duplicates, drops and jitter. Try frame-locked versus free-running field phase | CAP-8, TIM-1, TIM-2, SRC-6 | Latency floor in ms. Jitter histogram. Pacing policy chosen |
| SP-4 (deferred: HDR output out of scope, replace by an SDR output precision and banding test) | HDR path: HDR10 and scRGB Vulkan swapchains in the borderless window on your panel. Measure luminance of known patches and fine 1-pixel stripes at peak, panel ABL at small window sizes, and test brightness compensation for mask loss. Render a full-field mask pattern at 100 cd/m² CRT white in SDR and HDR | COL-1, COL-3, HW-4, HW-7, TEC-1 | A known code value gives a known cd/m² within tolerance. Panel ABL and dimming characterised |
| SP-5 | Source-fidelity test: a test-pattern application with known pixels captured with scaling, DPI virtualisation, Auto HDR, Night Light and colour management on and off, diffed bit for bit. Native-raster and integer-scale auto-detection on emulator frames at 3×/4× with bilinear filtering | CAP-3, SRC-2 | Captured values proven bit-exact, or the conditions that break it listed. Detection reliability known |
| SP-6 | Mask budget visual test: analytic aperture grille, slot and delta masks viewed at the real tube-to-pixel scale on your display, comparing area-integrated, enlarged-cell, zoomed and host-subpixel variants. Photograph the result | PHY-1, PHY-2, PHY-8, HW-6, OUT-7 | You pick a mask policy after seeing it on your own panel |
| SP-7 | Offline reference renderer for one aperture-grille tube (gun law, amplifier, beam, mask, halation) with heavy supersampling, plus the analytic test table from `CRT.md` as unit tests (scanline modulation, mask pitch, ring radius). Side-by-side with ShaderGlass plus an existing shader. Sensitivity analysis of the uncertain parameters | SCP-1, SCP-3, SCP-7, PHY-5, PHY-8, TEC-7 | Analytic checks pass. You confirm a visible difference and know which unknowns matter most |
| SP-8 | Cost study at 4K on your GPU: halation ring (FFT versus mip pyramid versus sparse ring sampling), multi-scanline beam integration and mask integration, FP16 history buffers, and rolling scan at 240 Hz with 4 sub-frames while a game runs | PRF-1, PRF-2, PRF-3, PHY-10 | Frame time and headroom known |
| SP-9 | Toolchain: GLSL to SPIR-V in Rust with reflection-driven parameter metadata, hot reload, and a headless software-Vulkan render for tests | TEC-3, TEC-6, TEC-9 | A parameter defined once appears in shader, Rust and menu. A headless test runs without a GPU |
| SP-10 | VRR behaviour of the unfocused output window and its interaction with a fixed-refresh rolling scan | OUT-6, TIM-2 | VRR engages or is ruled out |
| SP-11 | Antivirus and anti-cheat smoke test: an unsigned build capturing and overlaying with a registered hotkey, run beside your real games | PRJ-4, OUT-5 | No flags, or the offenders identified |
| SP-12 | Rolling-scan feasibility: port the MIT Blur Busters beam-simulator logic into the spike pipeline at your maximum refresh and check banding, flicker and brightness loss with a 60 Hz test source | PHY-12 | Feasible or ruled out on your display |
| SP-13 | Reference data feasibility: photograph one real CRT (or use published photos) and fit a scanline profile and pitch. Test whether a home measurement of primaries or phosphor decay is practical | SCP-2, PHY-5, PHY-7 | You know what you can measure yourself |

## Where the analysts disagreed

| Topic | Positions | How it is handled here |
|---|---|---|
| Reference topology | Sonnet: two-monitor as the supported primary mode, one-monitor only if the spike works. Opus and Fable: one-monitor is the design case because it is the hard one | OUT-1: two-monitor guaranteed, one-monitor as the design target pending SP-1 and SP-2. Needs your answer |
| Frame handoff to Vulkan | Sonnet and Opus: zero-copy interop by default. Fable: CPU copy first because small sources make it cheap | CAP-2: decide after measuring the capture size you actually use |
| Minimum Windows build | Sonnet: Windows 11 only. Opus: Windows 11 24H2+. Fable: Windows 11 22H2+ | HW-2: Windows 11 only, exact build after SP-1 |
| Temporal scope in the first version | Sonnet: frame-integrated plus optional afterglow. Opus: frame afterglow and ABL in the first version. Fable: frame-integrated, ABL later | PHY-12 and PHY-13: frame-integrated first, ABL and rolling scan later |
| Compensating mask loss in SDR | Sonnet: separate "host compensation" group. Opus: allow in a separate non-physical stage. Fable: honest exposure control, no hidden bloom | SCP-8 and COL-1: exposure slider plus an optional labelled compensation stage |
| Reference renderer implementation | Sonnet: slow double-precision CPU crate. Opus and Fable: headless compute path at high supersampling | TEC-7: headless compute path first, CPU crate optional |
| Vulkan binding for ImGui | Opus: evaluate dear-imgui-rs first. Sonnet and Fable: whichever passes a spike | TEC-5: spike first, dear-imgui-rs evaluated first |
| TATE (rotated) output | Opus: in scope early because rotation is cheap if planned. Sonnet: decide explicitly | SCP-6: user decision |
| Tube quality default | Fable: factory-adjusted at the low end of defect ranges. Opus: factory-adjusted reference with ageing sliders | SCP-9: factory-adjusted, defects as parameters |
| Same data, different spot sizes | Opus used much smaller spot sizes than Sonnet and Fable in the physical tables | Already resolved in `CRT.md` (see its "Resolved conflicts") |

## Feasibility facts to verify

These came from the Opus analyst's web checks or the analysts' own knowledge.
None of them is verified on your machine, which is why they are tied to spikes.

- Borderless capture (`IsBorderRequired = false`) needs Windows build 20348 or later and an access request. [Microsoft Learn: GraphicsCaptureSession.IsBorderRequired]
- `MinUpdateInterval` and cursor-capture toggles arrived in later builds, and a frame-rate cap near 50 fps was reported on some builds even with `MinUpdateInterval` = 0. [Win32CaptureSample issue 82]
- OBS skips minimised (iconic) windows when capturing with the WinRT API, and libwebrtc's capture session drops frames for minimised windows. [OBS winrt-capture source, libwebrtc wgc_capture_session]
- `imgui` 0.12 (imgui-rs) tracks Dear ImGui 1.89. `dear-imgui-rs` 0.9 tracks 1.92 with docking. [docs.rs]
- The development machine reports Windows build 10.0.26300, which Opus identified as an Insider build. No analyst checked which capture features that build has.
- Not yet checked by anyone: behaviour of Vulkan swapchains on Windows when a non-activating topmost window is presented (layered on DXGI or not), VRR for an unfocused window, HDR swapchain colour spaces per vendor, D3D11 to Vulkan external memory support per vendor, and whether hosted CI has any usable software Vulkan implementation.

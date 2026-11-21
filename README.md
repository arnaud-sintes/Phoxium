# Phoxium

![Phoxium icon](assets/icon/icon-256.png)

Phoxium is a CRT shader for Windows. It captures the window of a game or a program, runs the picture through a virtual CRT tube on the GPU and shows the result in its own window, which can go fullscreen over the game. It runs beside the captured program: nothing is injected into it and nothing in it is changed. The output is SDR.

The model is physical, not a filter: each source pixel drives the three electron beams of a virtual tube, and each phosphor element of the mask emits light in proportion to the beam energy it receives.

Phoxium was called **ultimateCRT** up to v0.3.0. It is a personal project, below 1.0.0 (a minor version may still change behaviour). Bug reports and feedback are welcome in the [GitHub issues](https://github.com/arnaud-sintes/Phoxium/issues).

## Requirements

- Windows 11 x64.
- A GPU and driver with Vulkan 1.3.
- Nothing to install: the zip holds one plain executable, `phoxium.exe`.

## Download and run

1. Download `phoxium-vX.Y.Z-windows-x64.zip` and its `.sha256` file from the [Releases page](https://github.com/arnaud-sintes/Phoxium/releases). The releases made before the rename (v0.2.0, v0.3.0) are named `ultimatecrt-vX.Y.Z-windows-x64.zip`.
2. Check the download against the hash (see "Unsigned binaries" below), then unzip it into a folder you own.
3. Start the game or program you want to capture.
4. Double-click `phoxium.exe`. It opens one window with the list of windows you can capture: type to search, Up and Down to choose, Enter or a double-click to capture.

That is all: no command line is needed. The integer upscale of the game and the black bars around its picture are detected automatically, and the picture is scaled and centred for you. Every automatic choice can be overridden in the menu.

## First use

| Action | Default key |
|---|---|
| Settings menu | `F10` |
| Fullscreen / framed window | `F11` |
| Quit | `Ctrl+Alt+End` (or the close button of the framed window) |
| Stats overlay | `Ctrl+Alt+F9` |
| A/B: bypass the whole CRT chain | `Ctrl+Alt+F7` |
| Next / previous preset | `Ctrl+Alt+PageDown` / `Ctrl+Alt+PageUp` |
| Click-through on / off | `Ctrl+Alt+F6` |

- Once a window is captured, the output never takes the keyboard focus from it, except while the settings menu is open (it gives the focus back when it closes).
- These keys are **global**: while Phoxium runs, F10 and F11 reach no other program, the game included. Every key, Quit included, can be changed in the menu (GPU and system, Hotkeys).
- Click-through sends the mouse to the window under the output (the game under a fullscreen output).

## The menu, presets and main controls

F10 opens the settings menu inside the output window. Every parameter of the tube is there, in panels by topic; changes apply at once and are saved. Hover a label for what it does.

- **Presets:** ten tube presets ship with the app (Sony PVM and BVM monitors, a Trinitron TV, the GDM-F520 PC monitor, a slot-mask TV, a consumer TV, an NEC CromaClear, an arcade monitor, VGA and delta-dot monitors). They are starting points built from published data, not measurements of one unit. Pick one, change anything, save your own.
- **Mask:** aperture grille, slot mask or delta dot, and its pitch in millimetres.
- **Beam and scanlines:** the spot width and how it grows with brightness, which shapes the scanlines.
- **Geometry:** curvature, overscan, pincushion, convergence and colour fringes.
- **Phosphor:** saturation and the afterglow (persistence) behind moving objects.
- **Optics:** glow (bloom) around bright areas and the vignette; turn on its **Enable** switch first.
- **Source:** what was detected, **Change source**, the picture area (Auto or manual limits), the scaling multiplier and the saved profile of each game.
- **Performance:** the quality tier (how much work the model costs, never the look) and an optional automatic drop when the GPU cannot keep up.
- Optional and off by default: ageing and burn-in (burn-in deliberately damages the picture and is kept between sessions), composite video and interlace.

## Capture

By default the captured picture stays on the GPU (**zero-copy**). If the PC cannot do that, Phoxium falls back to a **CPU copy** by itself, which works everywhere. You can choose the CPU copy in the menu (Source, Capture) if the picture looks wrong, stutters or freezes.

Only windows can be captured: a game in exclusive fullscreen cannot; run it in a window or borderless fullscreen. Protected content is captured black, and a minimised window sends no frames until it is restored.

## Settings and privacy

Everything Phoxium keeps is in `%APPDATA%\Phoxium\` (settings, your presets, game profiles, burn-in state). Delete that folder to go back to the defaults. On its first start Phoxium copies the folder of the old name, `%APPDATA%\ultimateCRT\`, if there is one; the old folder is left untouched.

The program has no network code and sends nothing anywhere. The settings file keeps the title of the last captured window (to find it again after a restart); look at the file before you attach it to a bug report.

## Known limits

- Checked live mostly with a test source of the repository (`pixel-demo`, not part of the download) and a few windows on one PC, not yet with a wide range of real games.
- Developed and tested on one NVIDIA laptop (RTX A2000, with an Intel GPU beside it). AMD and Intel GPUs and other PCs are untested: please report what you see.
- Zero-copy capture is not yet validated with a wide range of games and drivers (the CPU copy is the safe choice).
- Several newer parts are covered by automated tests but not yet checked by hand; [QUESTIONS.md](QUESTIONS.md) and the [CHANGELOG](CHANGELOG.md) say which.
- SDR output only; no HDR.
- Opening the menu deactivates the game for the moment (some games pause).

The full list, with the technical detail, is in [docs/REFERENCE.md](docs/REFERENCE.md#known-limits).

## Unsigned binaries

The executables are **not code-signed** (a personal project without a certificate). Windows SmartScreen may say "Windows protected your PC" on the first run: choose "More info", then "Run anyway", only after checking the hash. Antivirus tools may also warn about an unsigned program that captures windows and registers hotkeys.

Each release gives the SHA-256 of the zip in its notes and in the attached `.sha256` file. Compare it with yours:

```
PowerShell:  (Get-FileHash .\phoxium-vX.Y.Z-windows-x64.zip -Algorithm SHA256).Hash
cmd:         certutil -hashfile phoxium-vX.Y.Z-windows-x64.zip SHA256
```

The hash proves that your copy is the published file; it does not replace trust in the project. The source is public and the executables can be rebuilt from it.

## Building from source

Rust (pinned in `rust-toolchain.toml`), the Vulkan SDK (for `glslc`) and the MSVC C++ build tools, then:

```
cargo build --release
```

The executables are `target\release\phoxium.exe` and `target\release\pixel-demo.exe`. Setup, tests, packaging and releases: [ENV.md](ENV.md).

## More documentation

- [docs/REFERENCE.md](docs/REFERENCE.md): the detailed reference (developer command-line options, every menu section, hotkeys and their fallbacks, GPU choice, capture path, presets and parameters, recipes for glow, vignette and afterglow, the optional modules, source scaling, the target lifecycle).
- [ENV.md](ENV.md): build, test, package and publish. [ARCHITECTURE.md](ARCHITECTURE.md) and [DESIGN.md](DESIGN.md): how it is built, coding rules.
- [CRT.md](CRT.md): what real CRT monitors do and the data the model uses. [QUESTIONS.md](QUESTIONS.md): decisions, open questions, live-test findings. [CHANGELOG.md](CHANGELOG.md): changes per version.

## Licence

MIT: see [LICENSE](LICENSE). The release zip also carries `THIRD-PARTY.md`, the licences of the third-party code and of the Inter font built into the executables. Only permissive licences are used (no GPL, LGPL or AGPL).

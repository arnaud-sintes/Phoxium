# TODO

Remaining work, most important first. Details live in `QUESTIONS.md` (ids in brackets) and `CLAUDE.md` ("Next step"). Updated 2026-11-25 (nothing left that Claude can do alone or at the reference PC; the second-PC session is closed).

**Hardware constraint.** One reference PC (an NVIDIA RTX A2000 Laptop GPU and an Intel UHD 770, Windows 11, a laptop camera, no capture card); a second laptop can run a test session now and then. The sections below are ordered by what they need: work Claude can do alone on the reference PC (none left), work that needs the owner at the reference PC (none left), and **tests requiring other hardware** that is not available (each entry says the device and the expected result, so it can be run as soon as the device is there).

## Doable on the reference PC, alone (GPU session, no screen feedback needed unless said)

Nothing is left. The optics layout is fixed, the capture robustness is fixed, the FP16 history and the prefix reuse were dropped, and the small speed-ups and the permanent GPU timestamp were judged not worth doing (decisions and measurements: `QUESTIONS.md`, REV-2 notes).

## Tests requiring other hardware (not available)

Not worked on. Each entry gives the device and the result that closes it.

- [ ] **A capture card (or a camera driver) with no signal**: check the video hang fix (code done 2026-11-24, `QUESTIONS.md` REV-2); a laptop camera switched off is only an approximation. Device: any capture card or virtual camera whose driver blocks in `ReadSample` when nothing is plugged in. Expected: the open runs on its worker with a message (8 s wait), the drop shuts the source down and waits at most 1.5 s, the output window never freezes, and the picture of a live card is unchanged.
- [ ] **A video device that omits `MF_MT_DEFAULT_STRIDE`**: the code reads a missing stride bottom-up with the sample's own pitch (unit tested); a laptop's cameras may or may not be such devices. Device: a capture card or camera that delivers RGB32 without the attribute. Expected: the picture is the right way up; if the device turns out to deliver it top-down, keep the top-down reading for it.
- [ ] **An AMD GPU** (README known limit): run the live checks (`check-capture`, `check-pixel-demo`, `check-menu`, `check-hotkeys`, `check-focus`) and `check.ps1 -Full -Smoke` on it, and compare a `--pattern ramp --snapshot` with the NVIDIA one (the Intel UHD 770 differed by 1 of 255 at most).
- [ ] **A Windows 10 PC** (confirmation, the owner expects it to work): the README now says Windows 11 is the tested system and Windows 10 2004 or later is expected to work, untested; ShaderGlass supports Windows 10 2004 and later (`QUESTIONS.md`, comparison). Windows.Graphics.Capture needs version 2004 or later. Does the release start, capture a window and present on Windows 10? A pass lets the README say "tested" instead of "expected". Both PCs run Windows 11, so neither can answer.
- [ ] **An HDR monitor** (README known limit): capture "Whole screen N" on it and the Manual picture-area limits; the output is SDR only, so check that nothing is clipped or washed out. HDR on the second laptop (Windows HDR on, whole-screen capture, Auto HDR content) was reported fine without numbers; a real HDR monitor stays the better test.

## Potential ShaderGlass gaps to fulfill

From the comparison of 2026-11-25 (`QUESTIONS.md`, "Comparison with ShaderGlass": its public pages only, nothing measured). Each entry is tagged **gap** (ShaderGlass does it, Phoxium does not), **check** (probably covered or a doubt to verify), **decision** (the owner chooses a direction) or **by design** (not planned, to confirm). The project's differentiator is the physical model, not the shader library, so most of these are the owner's call.

- [ ] **gap, decision: an overlay without choosing a window (Desktop Glass).** A transparent window that shades whatever is behind it. Phoxium captures a window or a whole monitor and has click-through; the pieces of an overlay exist (the output window is already hidden from screen captures when a whole monitor is the source, the click-through option passes the mouse), what is missing is a region source: capture the screen area behind the output window, with the output excluded from its own capture. Decide whether it is a goal; if so, a design note first (the focus rules, the moving and resizing of the region, the cost of capturing the monitor).
- [ ] **gap: motion clarity at high refresh (black frame insertion, a rolling beam scan).** ShaderGlass had black frame insertion in its alpha builds and moved it, with a CRT beam simulator, to a separate app (ShaderBeam). Phoxium models the afterglow (persistence) but not the scan: on a 240 or 300 Hz panel a rolling scan of the beam at the refresh rate would be a physical feature, not an effect. First look at what a 300 Hz panel shows with the current persistence (the 300 Hz results in `QUESTIONS.md`; the 240 Hz mode was reported fine without a stats run), then decide.
- [ ] **gap: hardware and system reach.** ShaderGlass needs a DirectX 11 GPU and Windows 10 2004 or later (and runs on Linux through Wine); Phoxium needs Vulkan 1.3 and tests on Windows 11 (Windows 10 expected, untested). The second-PC session of 2026-11-25 answered part of it: a GeForce driver on an RTX 3060 works, and an Intel UHD 630 with a 2020 driver (Vulkan 1.2) is not usable, as Phoxium says. What is left: Windows 10 and AMD (both under other hardware); a newer Intel driver was reported to work (no details); relax the README only on what was tested. A DirectX back end is not planned (Vulkan is decided).
- [ ] **gap: a video device from the command line.** ShaderGlass takes `-input`; Phoxium has no flag to choose a capture card or a camera at start (only the picker). Check how the picker names devices, then add `--video <name>` (the matching rule of `--window`) so a launcher can start a capture-card session without the picker.
- [ ] **check: the output scale below 100 % and a stretch (new in ShaderGlass 1.3.2).** Probably covered: `--aspect fill` or a forced ratio stretches the picture, `--scale-mode manual` with a fractional `source_pixel_size` and any output window size scale it down. Verify on the reference PC (a 640 x 480 source in a 300 x 225 window, a stretch to 16:9) and note the answer in `REFERENCE.md`.
- [ ] **check: a dedicated GPU that is not recognised, a multi-monitor mode (their #309 and #310).** Phoxium chooses the adapter of the monitor the output covers or the `[global.gpu]` setting. Done on the reference PC and on the second laptop (2026-11-25): in both its hybrid and discrete modes `auto` picked the NVIDIA GPU, the one that owns the panel in Phoxium's view; nothing was misrecognised. Closed unless a laptop is found whose panel is driven by an Intel GPU with Vulkan 1.3.
- [ ] **check: the mouse pointer stuck on the screen (their #299).** Hand-check the pointer after F10, F11, Alt+Tab, a quit, a crash of the source, and in click-through (Phoxium hides the host cursor over the output while the menu is closed). About 10 minutes at the reference PC.
- [ ] **decision: reach and distribution.** ShaderGlass is on Steam and as a zip, with a large user base that finds the problems; Phoxium is a GitHub zip, unsigned (the SmartScreen warning). A Steam page or itch.io would give reach and a way past the warning; it ties to signing (Very low) and to the owner's wish for users.
- [ ] **by design, to confirm: importing external shaders (the 1200+ libretro slang shaders).** Phoxium is one physical model with parameters in real units; a shader player is another product. Not planned. Also not to take: code or shaders (ShaderGlass is GPL-3.0, its shaders carry their own licences; the policy is inspiration only).

## Source colour targets (new feature, one line per platform)

The idea (owner, 2026-11-25): the chain stays as it is; a step at its head turns the colours of the source into what the device it came from would have shown, as accurately as the sources allow, before the beam and the mask render it. **Built:** the slot `target` (`source_colour_target`, per-game profile fields `colour_target`, `colour_palette`), four transform kinds (`matrix`, `curve`, `palette`, `dac`), the data-file format with a provenance per value (`presets/targets/`, `src/targets.rs`), the oracle (`crt_model::target`) and a GPU test per target, the 5-bit codes recovered exactly from expanded pixels. **The research:** every platform is audited in `QUESTIONS.md` ("Audit of the colour targets": data, verdict, transform kind, licence), seven independent audits; no target ships without a named source for its numbers. Licence rule: numbers and measurements with attribution, never code or tables from GPL, LGPL or AGPL repositories.

Shipped (an estimate each; the audit says what the sources disagree on):

- [x] **Game Boy Color** (`game_boy_color`): the emulator correction (Gambatte "fast" weights with the near.sh clip at 960). Open: a neutral-white and a no-clip option (white is 240 of 255 here, 255 in SameBoy and Gambatte); the colorimetric alternatives are their own lines below.
- [x] **Game Boy Advance** (`game_boy_advance`): Talarubi's formula as in near.sh (red weight 255). Open: a 240 variant (current ares), a neutral-white option (the libretro and mGBA shaders keep white neutral), the measured AGS-101 profile (below).
- [x] **SNES, bsnes "deep black boost"** (`super_nintendo_black_boost`): an emulator look, not the console's output; for a real SNES on the tube the setting is none. Nothing else to build; keep the warning.
- [x] **Super Game Boy** (`super_game_boy`, `source_colour_palette` 1 to 32): the 32 built-in palettes, a grey source. Open: verify the table against an SGB ROM dump (provenance is a claim of the rustyboi emulator, cross-checked with two sources); a green emulator palette as an input (a named source palette).
- [x] **Sega Mega Drive / Genesis** (`sega_mega_drive`, `source_colour_input`): the measured DAC levels, behind Genesis Plus GX, Kega Fusion or Regen. Open: the measured shadow and highlight levels (a pixel in those modes is a few levels off), the white scale (255 here, 238 if 4.7 V is 0.9 V at the encoder), the input steps of Mesen, Mednafen, ares, MAME and FBNeo (not read), nothing for BlastEm (it shows the measured levels already).

To build, in the order the audit proposes (stop after each, owner decides the next):

- [ ] **Commodore 64** (VIC-II 6569 and 6567 revisions): luma from Mäkelä's oscilloscope table (6569R3: black 700, white 1850 mV and so on), chroma angles from Pepto (multiples of 22.5 degrees), a PAL delay line; through the `composite` model; the emulator palette (VICE defaults to Pepto PAL) must be recognised to give a colour index. A design note first. Unknown: the chroma amplitude per colour, the 8565 chip.
- [ ] **NES / Famicom**: nesdev's measured signal levels (sync 48 mV, burst 148 to 524, low levels 228, 312, 552, 880, high 616, 840, 1100 mV, emphasis about 0.816, 12 phases), the 2C02E and 2C02G phase distortion, SMPTE 170M decode constants (a choice); through the `composite` model; the emulator palette (Mesen2, FCEUX, Nestopia differ) must be recognised. A design note shared with the C64.
- [ ] **IBM CGA (RGBI)**: the measured IBM 5153 palette (Hugo Holden via VileR: 06 C47E00, 07 C4C4C4, 08 4E4E4E, 09 4E4EDC, 11 4EF3F3 ...) in place of the canonical 0x55, 0xAA, 0xFF palette DOSBox shows; a 16-entry remap kind (a named source palette). The composite CGA colours have no table yet.
- [ ] **Game Boy Color, colorimetric** (alternatives to the formula): the Pokefan531 matrices (libretro gbc-color: R = 0.82 R + 0.24 G - 0.06 B and so on, luminance 0.94; read the gamma handling in the shader file first), and the GBCC 8 x 8 x 8 table (MIT, a photographed table; a 3D table kind). Both are eyeballed; the instrument readings of the Handheld-Color-Space-Project have no licence.
- [ ] **Game Boy Advance AGS-101, measured**: primaries R (0.619, 0.348), G (0.330, 0.606), B (0.144, 0.084), white (0.309, 0.313), gamma about 2.4 to 2.5, from a colorimeter session; a colorimetric kind (gamma, primaries, white). Ask the data's author for permission first (the repository has no licence).

Partial data, not before more is found:

- [ ] **Sega Master System and SG-1000**: a measured 4-level table (0, 90, 173, 255 on the SMS1, 0, 89, 174, 255 on the SMS2, 0, 85, 170, 255 linear), a blue offset of the first VDP (about 22 to 23 percent, derived blue 0, 104, 170, 255); the scope reading of red and green conflicts with "pretty much linear". The SG-1000's TMS9918 palette is disputed. 4-entry curves.
- [ ] **Atari 2600** (NTSC, PAL, SECAM): the structure (15 hue phases, 8 luma steps) but no measured voltage; the phase step is disputed (23 to 28 degrees); SECAM is an exact 8-colour table. Composite model with user parameters.
- [ ] **Atari 8-bit (GTIA) and 5200**: Altirra's reference manual (delay about 24 degrees, 16 luma steps, a PAL phase table); the luma table is only in GPL code, re-derive it from the Atari documentation. **Atari 7800**: a derived formula, "best guess".
- [ ] **Amstrad CPC**: 27 colours from three levels (127 or 128, about 40 percent measured on one scope per a snippet); the CPCWiki and Grimware pages could not be read. A 27-entry remap with a level parameter.
- [ ] **ZX Spectrum**: nominal levels only (normal at 85 percent of bright), emulators use 0xC0, 0xCD, 0xD7 or 0xD8, 0xEE; the real values are unknown. Remap, or a PAL decode.
- [ ] **MSX (TMS9918A) and V9938**: the TMS has datasheet Y and colour-difference levels confirmed on a scope (SC-3000); the V9938's DAC levels are missing. Decode from Y and colour differences.
- [ ] **Apple II**: a derived composite model (lo-res Y, I, Q, 14.318 MHz), no measurement; the emulator frame already holds the artefacts, so it needs a decode of the pixel bit pattern, not a remap.
- [ ] **IBM CGA composite**: no numeric table found; composite decode.
- [ ] **Game Boy (DMG) and Pocket**: only photograph-based or unmeasured palettes (BGB reality, SameBoy, ares and Gambatte disagree, the Pocket is green in one and grey in another); a 4-shade mono palette, labelled convention; the BGB photo values are images only.
- [ ] **Virtual Boy**: four red levels (on-times 25, 50, 100 percent), the LED hue unmeasured. **WonderSwan and WonderSwan Color**: ares formulas borrowed from the Pocket and the GBC, nothing measured.
- [ ] **NEC PC Engine / TurboGrafx-16**: a silicon-dumped RGB-to-YUV table exists (furrtek) but its values were not readable; a 512-entry table. **SNK Neo Geo**: a computed resistor model, the dark-bit weight disputed.

No data at all (wait for a measurement of a real unit):

- [ ] **Commodore Amiga (OCS, ECS, AGA)**, **Atari ST and STE** (only the STE bit-order quirk is known: level index ((n and 7) << 1) or (n >> 3)), **Atari Lynx**, **Neo Geo Pocket Color**, **Sega Game Gear**.

Across the targets:

- [ ] **Menu and command line**: the menu shows the three target settings as plain choices; show the data file's label, notes and the provenance (the tag of every value) next to the choice, a named combo for the palette and the emulator, and a `--source-colour` flag (only `--set source_colour_target=...` exists).
- [ ] **A live look on a real emulator window for every shipped target**, with the target on and off (nothing but the headless renders and the GPU tests has run).
- [ ] **A palette-remap kind** (a named source palette, the emulator's, matched pixel by pixel) for the CGA, CPC, Spectrum and the fixed-palette consoles; **a composite-driven kind** for the C64, NES, Atari and Apple II. Both need a design note before code.
- [ ] **One white-scale policy** for the targets (the Game Boy Color keeps 240 of 255, the Game Boy Advance a tinted white, the Mega Drive full white at 255 against an open 238): decide, then say it in each file.

## Very low

- [ ] Sign the Windows executable (code-signing certificate) to avoid the SmartScreen and antivirus warnings.
- [ ] Potential Linux port (Vulkan and Rust already portable; capture, output window, hotkeys, picker are Windows-only).

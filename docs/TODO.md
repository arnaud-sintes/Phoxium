# TODO

Remaining work, most important first. Details live in `QUESTIONS.md` (ids in brackets) and `CLAUDE.md` ("Next step"). Updated 2026-11-25 (nothing left that Claude can do alone).

**Hardware constraint.** Only the development PC is available: an RTX A2000 Laptop GPU and an Intel UHD 770 (one monitor on each, one of them at 125 % scaling), Windows 11, a laptop camera, no capture card. The sections below are ordered by what they need: work Claude can do alone on this PC (none left), work that needs the owner at this PC, and a separate category, **Tests requiring other hardware**, that is set aside until that hardware exists (each entry says the device and the expected result, so it can be run as soon as the device is there).

## Doable on this PC, alone (GPU session, no screen feedback needed unless said)

Nothing is left. The optics layout is fixed, the capture robustness is fixed, the FP16 history and the prefix reuse were dropped, and the small speed-ups and the permanent GPU timestamp were judged not worth doing (decisions and measurements: `QUESTIONS.md`, REV-2 notes).

## Needs the owner at this PC (manual, keyboard and screen)

- [ ] The manual checklist in the header of `scripts/check-menu.ps1`: hotkey rebinding with a chord another program owns, the Save and Open dialogs and the clipboard from the menu, the AltGr warning on a European layout (the French layout of this PC will do). About 20 minutes; the automated part passes.
- [ ] A session on a real game (QUESTIONS SP-1 and SP-2: windowed and borderless, the picture area, the scale, a long session), the only checks of the chain on a real game; everything so far ran on `pixel-demo` and a few windows.

## Tests requiring other hardware (set aside until the hardware exists)

Not worked on. Each entry gives the device and the result that closes it.

- [ ] **A capture card (or a camera driver) with no signal**: check the video hang fix (code done 2026-11-24, `QUESTIONS.md` REV-2). Device: any capture card or virtual camera whose driver blocks in `ReadSample` when nothing is plugged in. Expected: the open runs on its worker with a message (8 s wait), the drop shuts the source down and waits at most 1.5 s, the output window never freezes, and the picture of a live card is unchanged.
- [ ] **A video device that omits `MF_MT_DEFAULT_STRIDE`**: the code reads a missing stride bottom-up with the sample's own pitch (unit tested). Device: a capture card or camera that delivers RGB32 without the attribute. Expected: the picture is the right way up; if the device turns out to deliver it top-down, keep the top-down reading for it.
- [ ] **Native file dialogs on a multithreaded-COM thread** (F, suspected): a probe on 2026-11-24 opened and closed both the Open and the Save dialog without a hang on this PC. A hang would depend on the third-party shell extensions of another PC. Device: a PC with such extensions (a cloud-drive or archive-manager shell extension, for instance). Expected: both dialogs open and close from the menu. If one hangs, move the dialogs to a dedicated STA thread (the owner window must then be handled across threads: the main thread must not block on the join, and the dialog must not hide under a topmost fullscreen output).
- [ ] **An AMD GPU** (README known limit): run the live checks (`check-capture`, `check-pixel-demo`, `check-menu`, `check-hotkeys`, `check-focus`) and `check.ps1 -Full -Smoke` on it, and compare a `--pattern ramp --snapshot` with the NVIDIA one (the Intel UHD 770 differed by 1 of 255 at most).
- [ ] **An HDR monitor** (README known limit): capture "Whole screen N" on it and the Manual picture-area limits; the output is SDR only, so check that nothing is clipped or washed out.
- [ ] **Another PC, another driver**: the zero-copy path across drivers and games, and the automatic fallback to the CPU copy on a failure.

## low

- [ ] list pros/cons/diffs vs Shader Glass project https://github.com/mausimus/ShaderGlass

## Very low

- [ ] Sign the Windows executable (code-signing certificate) to avoid the SmartScreen and antivirus warnings.
- [ ] Potential Linux port (Vulkan and Rust already portable; capture, output window, hotkeys, picker are Windows-only).

# TODO

Remaining work, most important first within each section. Details live in `QUESTIONS.md` (ids in brackets) and `CLAUDE.md` ("Next step"). Cleaned on 2026-11-27: everything done was removed (the history is in `CHANGELOG.md`, `QUESTIONS.md` and git).

**Hardware constraint.** One reference PC (an NVIDIA RTX A2000 Laptop GPU and an Intel UHD 770, Windows 11, a laptop camera, no capture card); a second laptop (60, 240 and 300 Hz) can run a test session now and then. Nothing is left that Claude can do alone at the reference PC. The sections below are ordered by what they need: **tests requiring other hardware** that is not available (each entry says the device and the expected result), and the very low items. What the owner tests live is reported as feedback, not listed here.

## Tests requiring other hardware (not available)

Not worked on. Each entry gives the device and the result that closes it.

- [ ] **A capture card (or a camera driver) with no signal**: check the video hang fix (code done 2026-11-24, `QUESTIONS.md` REV-2); a laptop camera switched off is only an approximation. Device: any capture card or virtual camera whose driver blocks in `ReadSample` when nothing is plugged in. Expected: the open runs on its worker with a message (8 s wait), the drop shuts the source down and waits at most 1.5 s, the output window never freezes, and the picture of a live card is unchanged.
- [ ] **A video device that omits `MF_MT_DEFAULT_STRIDE`**: the code reads a missing stride bottom-up with the sample's own pitch (unit tested); a laptop's cameras may or may not be such devices. Device: a capture card or camera that delivers RGB32 without the attribute. Expected: the picture is the right way up; if the device turns out to deliver it top-down, keep the top-down reading for it.
- [ ] **An AMD GPU** (README known limit): run the live checks (`check-capture`, `check-pixel-demo`, `check-menu`, `check-hotkeys`, `check-focus`) and `check.ps1 -Full -Smoke` on it, and compare a `--pattern ramp --snapshot` with the NVIDIA one (the Intel UHD 770 differed by 1 of 255 at most).
- [ ] **A Windows 10 PC** (confirmation, the owner expects it to work): the README now says Windows 11 is the tested system and Windows 10 2004 or later is expected to work, untested; ShaderGlass supports Windows 10 2004 and later (`QUESTIONS.md`, comparison). Windows.Graphics.Capture needs version 2004 or later. Does the release start, capture a window and present on Windows 10? A pass lets the README say "tested" instead of "expected". Both PCs run Windows 11, so neither can answer.
- [ ] **An HDR monitor** (README known limit): capture "Whole screen N" on it and the Manual picture-area limits; the output is SDR only, so check that nothing is clipped or washed out. HDR on the second laptop (Windows HDR on, whole-screen capture, Auto HDR content) was reported fine without numbers; a real HDR monitor stays the better test.

## Very low

- [ ] Sign the Windows executable (code-signing certificate) to avoid the SmartScreen and antivirus warnings.
- [ ] deploy Phoxium to Steam
- [ ] Potential Linux port (Vulkan and Rust already portable; capture, output window, hotkeys, picker are Windows-only).

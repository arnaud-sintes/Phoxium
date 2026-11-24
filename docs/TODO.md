# TODO

Remaining work, most important first. Details live in `QUESTIONS.md` (ids in brackets) and `CLAUDE.md` ("Next step"). Updated 2026-11-24 (night, after 0.9.2).

**Hardware constraint.** Only the development PC is available: an RTX A2000 Laptop GPU and an Intel UHD 770 (one monitor on each, one of them at 125 % scaling), Windows 11, a laptop camera, no capture card. The sections below are ordered by what they need: work Claude can do alone on this PC (including unattended GPU sessions), work that needs the owner at this PC, and a separate category, **Tests requiring other hardware**, that is set aside until that hardware exists (each entry says the device and the expected result, so it can be run as soon as the device is there).

## Doable on this PC, alone (GPU session, no screen feedback needed unless said)

No decision of the owner is open (2026-11-24: the FP16 history is dropped, weak GPUs get a documented limit, the prefix reuse goes after the optics layout fix; `QUESTIONS.md`, REV-2 notes). In this order:

From the code review of 2026-11-24 (`QUESTIONS.md`, REV-2); ids in brackets are the reviewers' agreement (O = Opus, F = Fable, S = Sonnet).

- [ ] The optics stage declares 22 storage images per shader stage (21 in its set 1, the bloom lobes and the pyramid levels, plus the chain output of set 0) against `maxPerStageDescriptorStorageImages` 16 of the Intel UHD 770: the validation layer prints an error at pipeline layout creation. The picture matched the NVIDIA one on that GPU (largest difference 1 of 255 on a ramp pattern, `--gpu 1` against `--gpu 0`), so the driver accepts the layout, but it is a spec violation and a risk on another driver. Found 2026-11-24 (`check.ps1 -SmokeGpu` pins the device of the smoke run). Fix: fewer bindings (a 2D array image for the pyramid levels and the bloom lobes), checked by byte-comparing offline renders of the optics tests and the ten presets, then `--gpu 1 --validation` must print nothing. About half a day.
- [ ] **Prefix reuse (decided 2026-11-24: go, after the optics layout fix above).** Skip the decode-to-`screen` chain on refreshes with no new source frame (a static menu, a 30 fps game on a 60 Hz screen) when the time-varying stages are off (average power and heat). `screen` is the dominant cost, so it is the largest speed-up left; it must stay bit-exact and the cache must be invalidated exactly (a parameter, a resize, a tier or preset change, a new source frame), so it starts with a written plan and a byte-comparison test on repeated refreshes, before any code.
- [ ] A permanent GPU timestamp between `screen_lines` and `screen_warp` (the pass split was measured on 2026-11-24 with a temporary skip patch: pass 1 is 85 % of `screen`), then the shared-memory staging of the sigma and the boundary terms in pass 1 of `screen` (it does not touch the per-sample loop that dominates, so a small gain at best).
- [ ] Performance candidates from PRF-2 (`QUESTIONS.md`), not taken yet. Check each bit-exact change by byte-comparing offline `render` outputs before and after (defaults and the ten presets); drop any that gains under about 0.2 ms: `amp` early-out on flat windows (bit-exact); a static mask lookup table (slot presets); optics weights in shared memory; the zero-copy path's full-frame readback every 100 ms; and, listed apart below, the prefix reuse.

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

## Very low

- [ ] Sign the Windows executable (code-signing certificate) to avoid the SmartScreen and antivirus warnings.
- [ ] Potential Linux port (Vulkan and Rust already portable; capture, output window, hotkeys, picker are Windows-only).

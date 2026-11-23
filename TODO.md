# TODO

Remaining work, most important first. Details live in `QUESTIONS.md` (ids in brackets) and `CLAUDE.md` ("Next step"). Updated 2026-11-23.

## Medium

- [ ] Hold-to-bypass key (today the bypass is a toggle).
- [ ] Whole-monitor or fixed-region capture.
- [ ] Composite demodulator realism (per-line and per-frame carrier phase, dot crawl, decoder chroma matrix).
- [ ] Analogue impairments, off by default: hum bar, scanline jitter and crawl, ghosting, noise and snow, line tracking.
- [ ] TATE (rotated tube).

## Low

- [ ] Sign the Windows executable (code-signing certificate) to avoid the SmartScreen and antivirus warnings.
- [ ] Primaries and white-point presets (EBU, SMPTE-C, NTSC-J 9300K, Trinitron).
- [ ] Cabinet or bezel overlay with reflection on the glass.
- [ ] TV knobs (tint, black and white level, sharpness), refresh flicker, vsync roll, VHS and tape module, source pre-filters (sinc, lanczos).
- [ ] Flip, manual pixel size, keep-PC-awake, hotkey for click-through, default profile, still image or capture card as source.

## Very low

- [ ] Performance, only if a target host needs it: FP16 phosphor history in the `low` quality tier (PRF-1 re-measure 2026-11-23: -0.9 ms of 4.4 at 2560x1440 on preset 01; needs a shader variant and the tier error re-measured).
- [ ] Potential Linux port (Vulkan and Rust already portable; capture, output window, hotkeys, picker are Windows-only).

## Finalization

- [ ] In-depth code review (3 sub-agents - Opus, Fable, Sonnet - high effort)
- [ ] In-depth performance analysis (3 sub-agents - Opus, Fable, Sonnet - high effort)

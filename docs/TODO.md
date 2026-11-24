# TODO

Remaining work, most important first. Details live in `QUESTIONS.md` (ids in brackets) and `CLAUDE.md` ("Next step"). Updated 2026-11-24 (evening).

## Medium

From the code review of 2026-11-24 (`QUESTIONS.md`, REV-2); ids in brackets are the reviewers' agreement (O = Opus, F = Fable, S = Sonnet).

- [ ] Check the video hang fix with a capture card that has no signal (code done 2026-11-24, `QUESTIONS.md` REV-2): the open runs on its worker with a message (8 s wait), the drop shuts the source down and waits at most 1.5 s, the picture of a live card is unchanged. Only a device blocked in `ReadSample` shows the real behaviour.
- [ ] Larger sources miss the frame budget (measured 2026-11-24, headless, 2560x1440, preset 01, game content): a 320x240 source costs 3.8 to 4.4 ms, 640x480 about 10 ms (`screen` 6 ms, `amp` 1.5 ms) and 1280x720 about 31 to 33 ms (`screen` 23 to 25 ms, `amp` 4.7 ms). The amp-domain images grow with native width x K (K = 16 on six of the ten presets) and `screen` with raster lines x columns. Design note in `QUESTIONS.md` (REV-2 notes, 2026-11-24): the cost is linear in source pixels from 640x480 (about 0.033 ms per kpx), `screen` is about 75 % of it, so the K rule alone gains about 3.5 ms of 32; first measure what the `fast` and `low` tiers already give, then split `screen`. Candidates: the `fast` tier's K rule (K = ceil(host px per source px), at least 4) by default when a source pixel is below about 0.25 host px, a coarser line grid for wide spots, work skipped for sources that do not change.

## Low

Performance follow-ups from the analysis of 2026-11-24 (`QUESTIONS.md`, PRF-2). Check each bit-exact change by byte-comparing offline `render` outputs before and after (defaults and the ten presets); the approximate ones with `tests/quality_tiers.rs`; for the history of `phosphor` use `tests/phosphor_dump.rs`.

- [ ] CPU-copy capture: read back into a buffer of the capture thread and take the frame mutex only to swap (the render thread blocks on it during the whole readback today). Needs a live capture to check.
- [ ] `phosphor` history, the second step: FP16 history with stochastic rounding (a hash of pixel and frame) in the `fast` and `low` tiers, about 0.5 ms more at 1440p (not bit-exact). The accuracy study is done on the CPU (`QUESTIONS.md` PHY-12, note of 2026-11-24: unbiased, worst pixel about 1 LSB of 8 bits at rho 0.99, round-to-nearest unusable); what is left is the shader change and its test against the oracle.
- [ ] A GPU timestamp between `screen_lines` and `screen_warp` (the `screen` segment covers both today), then the shared-memory staging of the sigma and the boundary terms in pass 1 of `screen`.
- [ ] Further candidates from PRF-2, not taken yet: reuse the decode-to-`screen` prefix on refreshes with no new source frame when the time-varying stages are off (average power and heat, bit-exact); a static mask lookup table (slot presets); `amp` early-out on flat windows (bit-exact); the zero-copy path's full-frame readback every 100 ms; optics weights in shared memory.

Minor findings of the code review (`QUESTIONS.md`, REV-2):

- [ ] Video stride: done in code (a missing `MF_MT_DEFAULT_STRIDE` is read bottom-up with the sample's own pitch, unit tested); check with a device that omits the attribute, and keep the top-down reading if one turns out to deliver it.
- [ ] Capture robustness, what is left (the `FrameArrived` panic is caught since 2026-11-24): after a zero-copy import failure a static source stays blank until the window redraws (F: the CPU path needs a frame to copy; keep the last texture to re-copy); the first use of an imported slot transitions from `UNDEFINED` (F, suspected; the headless zero-copy tests read the right pixels). Both need a live capture to check.
- [ ] Native file dialogs are called on a multithreaded-COM thread (F, suspected): check live, or use a dedicated STA thread.

## Very low

- [ ] Sign the Windows executable (code-signing certificate) to avoid the SmartScreen and antivirus warnings.
- [ ] Potential Linux port (Vulkan and Rust already portable; capture, output window, hotkeys, picker are Windows-only).

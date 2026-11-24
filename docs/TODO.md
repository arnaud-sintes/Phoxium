# TODO

Remaining work, most important first. Details live in `QUESTIONS.md` (ids in brackets) and `CLAUDE.md` ("Next step"). Updated 2026-11-24.

## Medium

From the code review of 2026-11-24 (`QUESTIONS.md`, REV-2); ids in brackets are the reviewers' agreement (O = Opus, F = Fable, S = Sonnet).

- [ ] Video device that stops delivering hangs the app (O, F, S): `Drop for VideoCapture` joins a worker blocked in a synchronous `ReadSample` (a card with no signal; also at Detach, a source switch and exit); opening a device blocks the render thread up to 8 s without a message pump. Bound the join (shut the source down or flush the reader from the dropping thread), open off the render thread, and drop `video` before the window is destroyed. Needs a capture card with no signal to check.
- [ ] Larger sources miss the frame budget (measured 2026-11-24, headless, 2560x1440, preset 01, game content): a 320x240 source costs 3.8 to 4.4 ms, 640x480 about 10 ms (`screen` 6 ms, `amp` 1.5 ms) and 1280x720 about 31 to 33 ms (`screen` 23 to 25 ms, `amp` 4.7 ms). The amp-domain images grow with native width x K (K = 16 on six of the ten presets) and `screen` with raster lines x columns. Candidates: the `fast` tier's K rule (K = ceil(host px per source px), at least 4) by default when a source pixel is below about 0.25 host px, a coarser line grid for wide spots, work skipped for sources that do not change.

## Low

Performance follow-ups from the analysis of 2026-11-24 (`QUESTIONS.md`, PRF-2). Check each bit-exact change by byte-comparing offline `render` outputs before and after (defaults and the ten presets); the approximate ones with `tests/quality_tiers.rs`; for the history of `phosphor` use `tests/phosphor_dump.rs`.

- [ ] CPU-copy capture: read back into a buffer of the capture thread and take the frame mutex only to swap (the render thread blocks on it during the whole readback today). Needs a live capture to check.
- [ ] `phosphor` history, the second step: FP16 history with stochastic rounding (a hash of pixel and frame) in the `fast` and `low` tiers, about 0.5 ms more at 1440p (not bit-exact, needs the PHY-12 accuracy study: a long static-then-black sequence against the f64 oracle).
- [ ] A GPU timestamp between `screen_lines` and `screen_warp` (the `screen` segment covers both today), then the shared-memory staging of the sigma and the boundary terms in pass 1 of `screen`.
- [ ] Further candidates from PRF-2, not taken yet: reuse the decode-to-`screen` prefix on refreshes with no new source frame when the time-varying stages are off (average power and heat, bit-exact); a static mask lookup table (slot presets); `amp` early-out on flat windows (bit-exact); the zero-copy path's full-frame readback every 100 ms; optics weights in shared memory.

Minor findings of the code review (`QUESTIONS.md`, REV-2):

- [ ] Video stride: a missing `MF_MT_DEFAULT_STRIDE` falls back to top-down, while Media Foundation's RGB convention is bottom-up (O, S, suspected); the hardware test checks only the frame size. Needs a device that omits the attribute.
- [ ] Capture robustness: a panic in the `FrameArrived` callback probably aborts the process (no `catch_unwind`, F); after a zero-copy import failure a static source stays blank until the window redraws (F); the first use of an imported slot transitions from `UNDEFINED` (F, suspected). All need a live capture to check.
- [ ] Native file dialogs are called on a multithreaded-COM thread (F, suspected): check live, or use a dedicated STA thread.
- [ ] Only a lost device should end the session, not any render error (F): `render_source` errors propagate with `?` out of the loop.
- [ ] `release.ps1` compares draft asset names and sizes only and checks the tag after publishing; no single-instance guard (two instances share `settings.toml` and the wear maps); the bezel is not drawn while there is no source frame.

## Very low

- [ ] Sign the Windows executable (code-signing certificate) to avoid the SmartScreen and antivirus warnings.
- [ ] Potential Linux port (Vulkan and Rust already portable; capture, output window, hotkeys, picker are Windows-only).

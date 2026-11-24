# TODO

Remaining work, most important first. Details live in `QUESTIONS.md` (ids in brackets) and `CLAUDE.md` ("Next step"). Updated 2026-11-24 (evening).

## Medium

From the code review of 2026-11-24 (`QUESTIONS.md`, REV-2); ids in brackets are the reviewers' agreement (O = Opus, F = Fable, S = Sonnet).

- [ ] Check the video hang fix with a capture card that has no signal (code done 2026-11-24, `QUESTIONS.md` REV-2): the open runs on its worker with a message (8 s wait), the drop shuts the source down and waits at most 1.5 s, the picture of a live card is unchanged. Only a device blocked in `ReadSample` shows the real behaviour.
- [ ] Larger sources miss the frame budget at the standard tier (measured 2026-11-24, `QUESTIONS.md` REV-2 notes, 2560x1440, game content): 640x480 costs 8.9 to 13.7 ms and 1280x720 30 to 48 ms (presets 01 flat to 05), against 5 to 6.7 ms and 12 to 19 ms at `fast`; pass 1 of `screen` is 85 % of it and linear in K. The fast K rule is already built and moves game content by about 0.1 % MAE (1.2 % at worst on a 1-px checker). Decided 2026-11-24: `quality_auto` is on by default (the picture changes only when frames are late); the live check of the drop (2026-11-24, unattended) found a flapping, a raise every 10 s on a source whose standard tier costs twice the period, fixed by predicting the cost of the tier above; afterwards one drop to `fast` after 3 s and 60 fps for 70 s (1280x720, slot TV preset, 1904x1011 output).
- [ ] The optics stage declares 22 storage images per shader stage (21 in its set 1, the bloom lobes and the pyramid levels, plus the chain output of set 0) against `maxPerStageDescriptorStorageImages` 16 of an Intel UHD 770: the validation layer prints an error at pipeline layout creation. The picture matched the NVIDIA one on that GPU (largest difference 1 of 255 on a ramp pattern, `--gpu 1` against `--gpu 0`), so the driver accepts the layout, but it is a spec violation and a risk on another driver. Found 2026-11-24, when the monitor layout of the PC made the smoke run choose the Intel adapter (`check.ps1 -SmokeGpu` now pins the device). Fix: fewer bindings (a 2D array image for the pyramid levels and the bloom lobes), checked by byte-comparing offline renders of the optics tests.

## Low

Performance follow-ups from the analysis of 2026-11-24 (`QUESTIONS.md`, PRF-2). Check each bit-exact change by byte-comparing offline `render` outputs before and after (defaults and the ten presets); the approximate ones with `tests/quality_tiers.rs`; for the history of `phosphor` use `tests/phosphor_dump.rs`.

- [ ] `phosphor` history, the second step: FP16 history with stochastic rounding (a hash of pixel and frame) in the `fast` and `low` tiers, about 0.5 ms more at 1440p (not bit-exact). The accuracy study is done on the CPU (`QUESTIONS.md` PHY-12, note of 2026-11-24: unbiased, worst pixel about 1 LSB of 8 bits at rho 0.99, round-to-nearest unusable); the gain is measured (0.5 ms at 1440p, `QUESTIONS.md` REV-2 notes); what is left is the shader change (stochastic rounding from a hash of pixel and frame, as `crt_model::fp16`) and its test against the oracle.
- [ ] A permanent GPU timestamp between `screen_lines` and `screen_warp` (the pass split was measured on 2026-11-24 with a temporary skip patch: pass 1 is 85 % of `screen`, see the Medium item), then the shared-memory staging of the sigma and the boundary terms in pass 1 of `screen` (it does not touch the per-sample loop that dominates, so a small gain at best).
- [ ] Further candidates from PRF-2, not taken yet: reuse the decode-to-`screen` prefix on refreshes with no new source frame when the time-varying stages are off (average power and heat, bit-exact); a static mask lookup table (slot presets); `amp` early-out on flat windows (bit-exact); the zero-copy path's full-frame readback every 100 ms; optics weights in shared memory.

Minor findings of the code review (`QUESTIONS.md`, REV-2):

- [ ] Video stride: done in code (a missing `MF_MT_DEFAULT_STRIDE` is read bottom-up with the sample's own pitch, unit tested); check with a device that omits the attribute, and keep the top-down reading if one turns out to deliver it.
- [ ] Capture robustness, what is left (the `FrameArrived` panic is caught since 2026-11-24): after a zero-copy import failure a static source stays blank until the window redraws (F: the CPU path needs a frame to copy; keep the last texture to re-copy); the first use of an imported slot transitions from `UNDEFINED` (F, suspected; the headless zero-copy tests read the right pixels, and a live windowed run with the validation layer on, zero-copy and a tier change printed no validation message on 2026-11-24). The first one needs a live capture and a forced import failure.
- [ ] Native file dialogs are called on a multithreaded-COM thread (F, suspected): a probe on 2026-11-24 opened and closed both the Open and the Save dialog on such a thread without a hang on the development PC; a hang would depend on the shell extensions of another PC. A dedicated STA thread needs the owner window handled across threads (the main thread would block on the join), so it waits for a PC that shows the problem.

## Very low

- [ ] Sign the Windows executable (code-signing certificate) to avoid the SmartScreen and antivirus warnings.
- [ ] Potential Linux port (Vulkan and Rust already portable; capture, output window, hotkeys, picker are Windows-only).

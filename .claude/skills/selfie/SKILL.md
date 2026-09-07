---
name: selfie
description: Screenshot the *running* Ship of Tools frontend window (real current state). Windows-only live-window grab via DWM, NOT the headless `--capture` flag (a fresh separate FE). Use for "selfie", "screenshot the running FE", "look at the live window".
---

# selfie — screenshot the running FE

> **Windows only — check `uname` before anything else.** On Linux/macOS or
> any headless session, STOP and say so: there is no FE window on those
> boxes to grab. From a headless context, use `sot --capture <path>` (a
> fresh instance, not the live window) or ask the FE session to take the
> selfie over sot-comm.

Grabs the **actual on-screen** `sot` window (mode, cursor, preview,
REPL/LLM panes, terminal drawer) and Reads it — distinct from `--capture`,
which renders a fresh headless instance and never reflects the user's live
state. Works because this `claude` runs inside the FE's own Terminal
drawer, so the FE window is on the same desktop; the wgpu window grabs fine
through the DWM composite.

## Steps

1. **Grab the window + save the full PNG**:

   ```powershell
   powershell -File scripts/selfie.ps1
   ```

   Prints `saved <path> <w>x<h> rect=(...)`. Fails with `no FE window` if
   `sot.exe` has no visible main window (minimized, or not running).

2. **Read the full PNG once** for gross layout (which pane is where).

3. **Crop into tiles and Read each** — the Read tool downscales hard, so a
   full 3440-wide grab is unreadable for pane text. Re-run with `-Crop`
   specs (`x,y,w,h,name.png`, cropped from the saved full grab — you'll need
   a second invocation once you know the window size, or pass specs derived
   from the layout below):

   ```powershell
   powershell -File scripts/selfie.ps1 -Crop "0,0,720,1440,selfie-nav.png","560,0,900,900,selfie-preview.png"
   ```

   The Ship of Tools layout is roughly: nav column at the left edge
   (~0–560px at 3440 wide), preview pane to its right (~560–1400),
   LLM/orchestrator pane on the right half, terminal drawer across the
   bottom. Adjust to the actual window width from step 1's output.

## Notes

- Shows the **user's real state** — to analyze a feature, ask the user to
  park the FE on the case first (you can't inject keystrokes into the live
  window).
- The window must be visible (not minimized); the relaunch path focuses the
  FE to the foreground, so a fresh post-relaunch window grabs fine.
- Name crops by intent so visual deltas across grabs are diffable.
- For deterministic verification of a code change (independent of live
  state), use `sot.exe --capture <png> ...` instead.

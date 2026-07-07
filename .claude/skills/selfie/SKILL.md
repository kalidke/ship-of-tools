---
name: selfie
description: Take a screenshot of the *running* Ship of Tools frontend window (its real current state — mode, selection, preview, drawer) and read it. This is the live-window grab via Windows DWM (PowerShell + System.Drawing.CopyFromScreen on the FE window rect), NOT the headless `--capture` flag (which spawns a fresh separate FE). Use when the user says "selfie", "take a self screenshot", "screenshot the running FE", "look at the live window", or wants the actual on-screen state analyzed.
---

# selfie — screenshot the running FE

> **Windows only — check `uname` BEFORE anything else.** This is PowerShell +
> Windows DWM (`CopyFromScreen` on the FE window rect). On Linux/macOS or any
> headless session (a remote backend host), STOP and say so —
> there is no FE window on those boxes to grab. From a headless context the
> alternatives are `sot --capture <path>` (fresh instance, not the
> live window) or asking the FE session over sot-comm to take the selfie.

Grabs the **actual on-screen** `sot` window (current mode, cursor,
preview, REPL/LLM panes, terminal drawer) and Reads it. Distinct from
`--capture`, which renders a *fresh headless instance* and never reflects the
user's live state. Use `selfie` when the user means "what's on my screen right
now"; use `--capture` to verify a code change deterministically. See the
`feedback_capture_workflow` memory for the two-modes distinction.

Requires interactive-desktop access — works because this `claude` runs inside
the FE's own Terminal drawer, so the FE window is on the same session's desktop.
The wgpu window grabs fine through the DWM composite (not black).

## Steps

1. **Grab the window rect + save a full PNG.** Get `sot`'s
   `MainWindowHandle`, read its rect with user32 `GetWindowRect`, and
   `CopyFromScreen` that rect (window-only, not the whole virtual desktop):

   ```powershell
   Add-Type -AssemblyName System.Drawing
   Add-Type -TypeDefinition @"
   using System; using System.Runtime.InteropServices;
   public class SelfieWin {
     [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
     [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
   }
   "@
   $p = Get-Process sot,sot -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
   if (-not $p) { Write-Output "no FE window"; exit 1 }
   $r = New-Object SelfieWin+RECT
   [void][SelfieWin]::GetWindowRect($p.MainWindowHandle, [ref]$r)
   $w = $r.Right - $r.Left; $h = $r.Bottom - $r.Top
   $bmp = New-Object System.Drawing.Bitmap $w, $h
   $g = [System.Drawing.Graphics]::FromImage($bmp)
   $g.CopyFromScreen($r.Left, $r.Top, 0, 0, (New-Object System.Drawing.Size($w, $h)))
   $out = "$env:LOCALAPPDATA\sot\selfie.png"
   $bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png); $g.Dispose(); $bmp.Dispose()
   Write-Output "saved $out ${w}x${h} rect=($($r.Left),$($r.Top))"
   ```

   Note: on multi-monitor, `rect.Left/Top` can be negative (virtual-coordinate
   space); `CopyFromScreen` uses those same coordinates, so it lands on the
   right monitor without adjustment.

2. **Read the full PNG once** for gross layout (which pane is where).

3. **Crop into tiles and Read each** — the Read tool downscales hard, so a full
   3440-wide grab is unreadable for pane text. Crop the region(s) you care about
   (nav/module column, preview pane, LLM pane, drawer) and Read those. The Ship of Tools
   layout is roughly: nav column at the left edge (~0–560px at 3440 wide),
   preview pane to its right (~560–1400), LLM/orchestrator pane on the right half,
   terminal drawer across the bottom. Adjust to the actual window width.

   ```powershell
   Add-Type -AssemblyName System.Drawing
   $src = [System.Drawing.Image]::FromFile("$env:LOCALAPPDATA\sot\selfie.png")
   function Crop($x,$y,$cw,$ch,$name){
     $b = New-Object System.Drawing.Bitmap $cw,$ch
     $g = [System.Drawing.Graphics]::FromImage($b)
     $g.DrawImage($src, (New-Object System.Drawing.Rectangle 0,0,$cw,$ch),
                        (New-Object System.Drawing.Rectangle $x,$y,$cw,$ch),
                        [System.Drawing.GraphicsUnit]::Pixel)
     $o = "$env:LOCALAPPDATA\sot\$name"
     $b.Save($o, [System.Drawing.Imaging.ImageFormat]::Png); $g.Dispose(); $b.Dispose()
     Write-Output $o
   }
   Crop 0   0 720 1440 "selfie-nav.png"      # nav / module column (mode + selection)
   Crop 560 0 900 900  "selfie-preview.png"  # preview pane (anchor it from its true left edge)
   $src.Dispose()
   ```

## Notes

- This shows the **user's real state** — if they navigated to a specific
  mode/item, the grab reflects it. To analyze a feature, ask the user to park
  the FE on the case first (you can't inject keystrokes into the live window).
- The window must be visible (not minimized). The relaunch path focuses the FE
  to the foreground, so a fresh post-relaunch window grabs fine.
- Name crops by intent so visual deltas across grabs are diffable.
- For deterministic verification of a code change (independent of live state),
  use `sot.exe --capture <png> ...` instead — see
  `feedback_capture_workflow`.

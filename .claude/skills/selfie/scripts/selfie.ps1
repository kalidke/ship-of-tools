<#
.SYNOPSIS
  Grab the running Ship of Tools frontend window (live on-screen state) via
  Windows DWM (user32 GetWindowRect + System.Drawing.CopyFromScreen), and
  optionally crop tiles out of the grab. See ../SKILL.md for when to use
  this vs. `sot --capture`.

.PARAMETER OutDir
  Where to save the full grab + crops. Default: $env:LOCALAPPDATA\sot

.PARAMETER Crop
  Zero or more "x,y,w,h,filename.png" specs, cropped from the full grab.

.EXAMPLE
  powershell -File selfie.ps1
  powershell -File selfie.ps1 -Crop "0,0,720,1440,selfie-nav.png","560,0,900,900,selfie-preview.png"
#>
param(
    [string]$OutDir = "$env:LOCALAPPDATA\sot",
    [string[]]$Crop = @()
)

Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @"
using System; using System.Runtime.InteropServices;
public class SelfieWin {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
}
"@

# DPI: this PowerShell process is DPI-unaware by default, so on a scaled
# display GetWindowRect reports virtualized (shrunken) coordinates and
# CopyFromScreen grabs only the top-left fraction of the window (a 200%
# box returned 1440x900 of a 2880x1800 window). Opt in before any
# window/screen call so both see physical pixels.
[void][SelfieWin]::SetProcessDPIAware()

$p = Get-Process sot -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if (-not $p) { Write-Output "no FE window"; exit 1 }

$r = New-Object SelfieWin+RECT
[void][SelfieWin]::GetWindowRect($p.MainWindowHandle, [ref]$r)
$w = $r.Right - $r.Left
$h = $r.Bottom - $r.Top
$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp)
# Multi-monitor: rect.Left/Top can be negative (virtual-coordinate space);
# CopyFromScreen uses the same coordinates, so this lands on the right
# monitor with no adjustment needed.
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, (New-Object System.Drawing.Size($w, $h)))
$g.Dispose()

if (-not (Test-Path $OutDir)) { New-Item -ItemType Directory -Path $OutDir -Force | Out-Null }
$out = Join-Path $OutDir "selfie.png"
$bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
Write-Output "saved $out ${w}x${h} rect=($($r.Left),$($r.Top))"

foreach ($spec in $Crop) {
    $parts = $spec -split ','
    if ($parts.Count -ne 5) { Write-Output "skip bad crop spec: $spec (want x,y,w,h,name)"; continue }
    $cx = [int]$parts[0]; $cy = [int]$parts[1]; $cw = [int]$parts[2]; $ch = [int]$parts[3]; $name = $parts[4]
    $cb = New-Object System.Drawing.Bitmap $cw, $ch
    $cg = [System.Drawing.Graphics]::FromImage($cb)
    $cg.DrawImage($bmp, (New-Object System.Drawing.Rectangle 0, 0, $cw, $ch),
                        (New-Object System.Drawing.Rectangle $cx, $cy, $cw, $ch),
                        [System.Drawing.GraphicsUnit]::Pixel)
    $cg.Dispose()
    $cout = Join-Path $OutDir $name
    $cb.Save($cout, [System.Drawing.Imaging.ImageFormat]::Png)
    $cb.Dispose()
    Write-Output $cout
}
$bmp.Dispose()

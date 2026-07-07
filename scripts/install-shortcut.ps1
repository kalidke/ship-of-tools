# install-shortcut.ps1 — create a "Ship of Tools" shortcut on the desktop
# that launches the chrome. First time: run this, then right-click the shortcut
# and choose "Pin to taskbar" (modern Windows blocks pinning programmatically).
# Every re-run AFTER that also re-syncs the existing taskbar pin to the launcher,
# so the pin never drifts into launching a bare sot.exe ("naive" FE).
#
# Re-run any time the repo path changes or the launcher script moves.

$ErrorActionPreference = 'Stop'

$repo = Resolve-Path -Path (Join-Path $PSScriptRoot '..')
$launcher = Join-Path $repo 'scripts\launch-sot.ps1'
$frontendExe = Join-Path $repo 'rust\target\release\sot.exe'
$logoIcon = Join-Path $repo 'logo.ico'
$shortcutPath = Join-Path $env:USERPROFILE 'Desktop\Ship of Tools.lnk'

if (-not (Test-Path $launcher)) {
    Write-Error "launcher not found: $launcher"
    exit 1
}

$wsh = New-Object -ComObject WScript.Shell
$sc = $wsh.CreateShortcut($shortcutPath)
$sc.TargetPath = "$env:WINDIR\System32\WindowsPowerShell\v1.0\powershell.exe"
$sc.Arguments = "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$launcher`""
$sc.WorkingDirectory = $repo.Path
# Prefer the Ship of Tools logo icon; fall back to the frontend exe's icon
# when it's been built, otherwise the PowerShell icon stays (still distinct).
if (Test-Path $logoIcon) {
    $sc.IconLocation = "$logoIcon,0"
} elseif (Test-Path $frontendExe) {
    $sc.IconLocation = "$frontendExe,0"
}
$sc.WindowStyle = 7 # Minimized
$sc.Description = 'Ship of Tools — concept-explorer dev environment'
$sc.Save()

# Stamp the explicit AUMID so the running window (sot.exe, which sets the same
# id via SetCurrentProcessExplicitAppUserModelID — see rust/frontend/src/main.rs)
# merges into THIS shortcut's taskbar button instead of opening a second one.
# Without it, the shortcut launches powershell.exe and Windows groups by that
# identity, not sot.exe's. Keep 'ShipOfTools.Sot' in sync with main.rs.
& (Join-Path $PSScriptRoot 'set-shortcut-aumid.ps1') -LnkPath $shortcutPath -Aumid 'ShipOfTools.Sot'

Write-Host "Created: $shortcutPath"

# --- Keep the taskbar pin in sync -------------------------------------------
# The taskbar pin is a SEPARATE .lnk from the desktop shortcut, so it drifts
# whenever this script updates the launcher: a stale pin then launches a bare
# sot.exe with no tunnel/backend — a "naive" FE (maintainer note, 2026-07-03). Windows
# blocks *creating* a pin programmatically, but an EXISTING pinned .lnk can be
# rewritten — so repoint any Ship of Tools pin to the launcher + stamp the same
# AUMID, matching the desktop shortcut. Match by name OR by a telltale target
# (bare sot.exe, or args already referencing the launcher).
$pinDir = Join-Path $env:APPDATA 'Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar'
$syncedPin = $false
if (Test-Path $pinDir) {
    Get-ChildItem -Path $pinDir -Filter *.lnk -ErrorAction SilentlyContinue | ForEach-Object {
        $p = $wsh.CreateShortcut($_.FullName)
        $isSot = ($_.Name -match 'sot|ship') -or ($p.TargetPath -match 'sot\.exe$') -or ($p.Arguments -match 'launch-sot\.ps1')
        if ($isSot) {
            $p.TargetPath = "$env:WINDIR\System32\WindowsPowerShell\v1.0\powershell.exe"
            $p.Arguments = "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$launcher`""
            $p.WorkingDirectory = $repo.Path
            if (Test-Path $logoIcon) { $p.IconLocation = "$logoIcon,0" }
            $p.WindowStyle = 7
            $p.Description = 'Ship of Tools — concept-explorer dev environment'
            $p.Save()
            & (Join-Path $PSScriptRoot 'set-shortcut-aumid.ps1') -LnkPath $_.FullName -Aumid 'ShipOfTools.Sot'
            Write-Host "Synced taskbar pin -> launcher: $($_.FullName)"
            $syncedPin = $true
        }
    }
}
if ($syncedPin) {
    Write-Host "(Windows caches pinned-icon metadata; restart Explorer or re-login if the pin still shows the old icon.)"
} else {
    Write-Host ""
    Write-Host "To pin to the taskbar:"
    Write-Host "  1. Right-click the Ship of Tools shortcut on the desktop"
    Write-Host "  2. Choose 'Pin to taskbar' (Windows 11 may bury it under 'Show more options')"
    Write-Host "  (Re-run this script after pinning; it will then keep the pin in sync.)"
}

# install-shortcut.ps1 — create a "Ship of Tools" shortcut on the desktop
# that launches the chrome. First time: run this, then right-click the shortcut
# and choose "Pin to taskbar" (modern Windows blocks pinning programmatically).
# Every re-run AFTER that also re-syncs the existing taskbar pin to the launcher,
# so the pin never drifts into launching a bare sot.exe ("naive" FE).
#
# Re-run any time the repo path changes or the launcher script moves. Also
# re-run (idempotent) any time repo\current\scripts\launch-sot.ps1 changes
# hands -- the shortcut/pin target below follows it via
# Get-SotLauncherTarget. See docs/adr/0030-versioning-release-and-auto-
# update.md's 2026-09-17 amendment for why the target is repo\current, not
# the clone. launch-sot.ps1's own migration handover re-runs this script
# once a pinned launcher first appears, so an existing shortcut needs no
# by-hand fix.

param(
    # The hub's ssh alias (the box whose hosts.toml is canonical). Recorded in
    # install.json and used for this box's FIRST topology sync: the launcher
    # can refresh a hosts.toml on every launch but cannot create the first one
    # without knowing the hub (a fresh box has no copy to read a hub from).
    # Same job as install.sh --hub on Linux/macOS.
    [string]$Hub
)
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

# Get-SotLauncherTarget -- shared with launch-sot.ps1; see that file's
# header and scripts/sot-install-layout.ps1's own header.
. (Join-Path $PSScriptRoot 'sot-install-layout.ps1')

# No hosts.toml check here: the file is never repo-local (it lives in
# <config dir>/hosts.toml, fetched from the hub by `sotd topology sync`,
# which the launcher now runs on every launch) -- see
# docs/adr/0015-hosts-targeting.md.

# Copy the icon to the install prefix and point shortcuts THERE, not into the
# clone. A .lnk stores an absolute IconLocation, so a shortcut aimed at
# <repo>\logo.ico silently falls back to a generic icon the moment the repo is
# moved or renamed -- which is why this script's header has always said to
# re-run it when the repo path changes. %LOCALAPPDATA%\sot is where the
# binaries already live and does not move.
$prefixDir = Join-Path $env:LOCALAPPDATA 'sot'
New-Item -ItemType Directory -Force -Path $prefixDir | Out-Null
$stableIcon = Join-Path $prefixDir 'logo.ico'
if (Test-Path $logoIcon) {
    Copy-Item -Path $logoIcon -Destination $stableIcon -Force
    $logoIcon = $stableIcon
} elseif (Test-Path $stableIcon) {
    # Repo copy missing but a previous install left one behind - still better
    # than falling through to the PowerShell icon.
    $logoIcon = $stableIcon
}

# The pinned launcher once sot-apply.ps1 (or a first Initialize-InstallLayout
# run) has created one, else this clone's own launcher -- see the header
# above and Get-SotLauncherTarget's own doc comment.
$launcherTarget = Get-SotLauncherTarget -Prefix $prefixDir -ClonePath $repo.Path

$wsh = New-Object -ComObject WScript.Shell
$sc = $wsh.CreateShortcut($shortcutPath)
$sc.TargetPath = "$env:WINDIR\System32\WindowsPowerShell\v1.0\powershell.exe"
$sc.Arguments = "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$launcherTarget`""
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

# Write <prefix>\install.json. Without it the frontend's startup self-check
# bails at its "not a release install" guard and Windows never checks for
# updates at all (install.sh, the only other writer, refuses to run here).
# No-ops with an explanation on a -dev source build.
& (Join-Path $PSScriptRoot 'install-manifest.ps1') -Prefix $prefixDir -Repo $repo.Path -Hub $Hub

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
            $p.Arguments = "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$launcherTarget`""
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

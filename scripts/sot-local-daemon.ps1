# sot-local-daemon.ps1 -- ensure/stop the per-user LOCAL sotd (ADR 0042 L1c).
#
# "Local is just another host": every launch of the Windows frontend ensures
# a per-user sotd is running on a FIXED named pipe, so the frontend (today
# via -Local; from L2 on, alongside the remote connection) always has a
# local daemon to talk to. Idempotent: if the pipe already answers, this is
# a no-op. Detached: the daemon outlives this script, the launcher, and every
# frontend relaunch -- it is a persistent per-host daemon exactly like the
# remote one, not a per-session spawn (contrast the old GUID-pipe -Local path
# this replaces).
#
# Binary resolution, in order: the release install's <prefix>\bin\sotd.exe,
# then the dev checkout's <DevBinDir>\sotd.exe. This is the OPPOSITE priority
# from the frontend's own dev-build-first resolution in launch-sot.ps1 --
# deliberately: the local daemon is meant to sit untouched across many
# launches (idempotent), so it should run the binary sot-apply.ps1 already
# keeps current, not whatever a dev's last `cargo build` happened to leave
# behind. `sot-capsule.exe` (ADR 0042 L1a's capsule workspace runtime) MUST
# be a sibling of whichever sotd.exe is chosen -- rust/backend/src/
# capsule_workspace.rs resolves it via `current_exe().parent()`, so a
# daemon started from a directory missing that file can create workspaces
# that immediately fail to spawn. Refuse to start rather than run degraded.
#
# Pipe naming: sotd's own `--label` auto-derivation
# (rust/backend/src/paths.rs::session_socket_path) is a Unix runtime-dir
# scheme with no Windows branch -- on Windows it degrades to a POSIX-shaped
# path, not a `\\.\pipe\...` name. The one real Windows pipe-name precedent
# in this repo is the hand-authored `hosts.toml` example
# `[host.local] socket = "\\.\pipe\sot-local"` (rust/frontend/src/hosts.rs).
# This script follows that exact shape, made per-user (`sot-<user>-local`)
# since the daemon's own private-socket security model is per-user
# ownership, not a shared name. `--label local` is passed too (harmless --
# `--socket` already wins over label-derivation) so the daemon's Sessions-
# mode metadata identifies it as "local", mirroring the backend host's own
# `--label sot`.
#
# --project-root: the user's home (`$env:USERPROFILE`), the same convention
# the backend host uses (deploy/sotd.service, scripts/install.sh: `sotd
# --project-root $HOME --label sot`) -- a persistent daemon's default
# workspace root is the user's home, not this repo checkout.
#
# -Stop: sotd installs no signal/console-control handler on either platform
# (grepped: no SIGTERM/ctrl_c handling in rust/backend/src/*.rs) and exposes
# no clean-stop IPC op. Linux's own "graceful" stop is systemd's UNHANDLED
# default SIGTERM (deploy/sotd.service has no ExecStop/KillSignal override)
# -- there is no gentler mechanism to reach for on either platform, so
# Stop-Process is what "clean stop" already reduces to here. Capsule
# supervisors (sot-capsule.exe) are never touched by -Stop: they are
# separate DETACHED processes (spawned with CREATE_BREAKAWAY_FROM_JOB) and
# the one authority over a workspace's live state; the daemon re-adopts them
# via `--resume` on its next start (ADR 0042 L1a). Matched for -Stop by
# process name `sotd.exe` AND the pipe name in its command line, so this
# never touches an unrelated sotd (a different label/pipe) on the same box.
#
# Standalone + parameterized (mirrors scripts/sot-apply.ps1's own -Prefix
# test-override convention) so this is independently testable without the
# rest of launch-sot.ps1/shutdown-sot.ps1 -- see
# scripts/tests/test-local-daemon.ps1. Called from launch-sot.ps1 (every
# launch mode, ensure-started) and shutdown-sot.ps1 (last, after the FE and
# tunnel are down, -Stop).
#
# ASCII ONLY in string literals (see the same note in launch-sot.ps1): this
# file has no BOM, so Windows PowerShell 5.1 decodes it as cp1252 and a
# non-ASCII byte inside a string literal can mojibake into a phantom quote
# and fail the whole parse.
#
# Exit codes: 0 = the daemon is confirmed answering on the pipe (already was,
# or was just started) -- or, for -Stop, confirmed stopped/was not running.
# 1 = not ready (binary missing, sot-capsule.exe missing, or it did not come
# up / go down within the bound) -- callers treat this as fail-open: log and
# continue, never abort the whole launch over it.

[CmdletBinding()]
param(
    [switch]$Stop,
    # Install prefix override (tests). Default: %LOCALAPPDATA%\sot, matching
    # sot-apply.ps1's own default and the install layout (ADR 0030 Sec 4).
    [string]$Prefix,
    # Dev checkout's binary dir override (tests; production callers pass the
    # real one they already computed). Default: <repo>\rust\target\release.
    [string]$DevBinDir,
    # --project-root override (tests). Default: $env:USERPROFILE.
    [string]$ProjectRoot,
    # Named-pipe basename override (tests, so a test run never collides with
    # a real per-user daemon). Default: sot-$env:USERNAME-local.
    [string]$PipeName
)

$ErrorActionPreference = 'Continue'

if (-not $Prefix) { $Prefix = Join-Path $env:LOCALAPPDATA 'sot' }
if (-not $DevBinDir) {
    $repo = Resolve-Path -Path (Join-Path $PSScriptRoot '..')
    $DevBinDir = Join-Path $repo 'rust\target\release'
}
if (-not $ProjectRoot) { $ProjectRoot = $env:USERPROFILE }
if (-not $PipeName) { $PipeName = "sot-$env:USERNAME-local" }
$PipePath = '\\.\pipe\' + $PipeName

function Write-LocalDaemonLog {
    param([string]$Message)
    Write-Host "sot-local-daemon: $Message"
    try {
        $logDir = Join-Path $Prefix 'logs'
        New-Item -ItemType Directory -Force -Path $logDir | Out-Null
        "$(Get-Date -Format o)  pid=$PID  $Message" |
            Out-File -FilePath (Join-Path $logDir 'sotd-local.log') -Append -Encoding utf8
    } catch { }
}

# Windows named pipes appear as entries under the \\.\pipe\ pseudo-directory
# for as long as a server instance is listening -- listing it (rather than
# attempting a client connect) is a non-blocking existence check with no
# risk of hanging on a pipe at its max-instance count.
function Test-SotPipeOpen {
    param([string]$Path)
    try {
        return ([System.IO.Directory]::GetFiles('\\.\pipe\')) -contains $Path
    } catch {
        return $false
    }
}

function Get-LocalDaemonProcess {
    Get-CimInstance Win32_Process -Filter "Name='sotd.exe'" |
        Where-Object { $_.CommandLine -and $_.CommandLine.Contains($PipeName) }
}

if ($Stop) {
    $procs = @(Get-LocalDaemonProcess)
    if ($procs.Count -eq 0) {
        Write-LocalDaemonLog "stop: not running (no sotd.exe with pipe $PipeName)"
        exit 0
    }
    foreach ($p in $procs) {
        Write-LocalDaemonLog "stop: killing pid=$($p.ProcessId)"
        Stop-Process -Id $p.ProcessId -Force -ErrorAction SilentlyContinue
    }
    # Bounded confirmation the pipe actually went away -- never an unbounded
    # wait; a stubborn process just gets reported, not retried forever.
    $down = $false
    for ($i = 0; $i -lt 8; $i++) {
        Start-Sleep -Milliseconds 250
        if (-not (Test-SotPipeOpen $PipePath)) { $down = $true; break }
    }
    if ($down) {
        Write-LocalDaemonLog "stop: confirmed down ($PipePath no longer listed)"
        exit 0
    } else {
        Write-LocalDaemonLog "stop: pipe $PipePath still listed after 2s - may still be tearing down"
        exit 1
    }
}

# ---- ensure-started (default mode) ------------------------------------------

if (Test-SotPipeOpen $PipePath) {
    Write-LocalDaemonLog "already running on $PipePath"
    exit 0
}

$installExe = Join-Path $Prefix 'bin\sotd.exe'
$devExe = Join-Path $DevBinDir 'sotd.exe'
$daemonExe = $null
if (Test-Path $installExe) {
    $daemonExe = $installExe
} elseif (Test-Path $devExe) {
    $daemonExe = $devExe
}
if (-not $daemonExe) {
    Write-LocalDaemonLog "REFUSED: no sotd.exe found (checked $installExe and $devExe)"
    exit 1
}

$capsuleExe = Join-Path (Split-Path $daemonExe -Parent) 'sot-capsule.exe'
if (-not (Test-Path $capsuleExe)) {
    Write-LocalDaemonLog "REFUSED: sot-capsule.exe not found beside $daemonExe (ADR 0042 L1a resolves it there; a local daemon started without it cannot run capsule workspaces)"
    exit 1
}

$daemonArgs = @('--socket', $PipePath, '--project-root', $ProjectRoot, '--label', 'local')
Write-LocalDaemonLog "starting: $daemonExe $($daemonArgs -join ' ')"
try {
    $proc = Start-Process -FilePath $daemonExe -ArgumentList $daemonArgs -WindowStyle Hidden -PassThru
} catch {
    Write-LocalDaemonLog "REFUSED: failed to start $daemonExe - $($_.Exception.Message)"
    exit 1
}
Write-LocalDaemonLog "spawned pid=$($proc.Id)"

# Bounded wait for the pipe to come up -- same 20x250ms=5s shape the remote
# path already uses (launch-sot.ps1's socket-wait loop) for an analogous
# "did the daemon we just started actually bind" check.
$up = $false
for ($i = 0; $i -lt 20; $i++) {
    Start-Sleep -Milliseconds 250
    if (Test-SotPipeOpen $PipePath) { $up = $true; break }
}
if ($up) {
    Write-LocalDaemonLog "pipe=$PipePath"
    exit 0
} else {
    Write-LocalDaemonLog "did not come up on $PipePath within 5s"
    exit 1
}

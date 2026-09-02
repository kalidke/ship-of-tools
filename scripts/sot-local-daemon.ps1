# sot-local-daemon.ps1 -- ensure/stop the per-user LOCAL sotd (ADR 0042 L1c,
# L2b: the pipe name is queried from the daemon itself, and every launch
# mode calls this, not just -Local).
#
# "Local is just another host": every launch ensures a per-user sotd is
# running on a fixed named pipe, so the frontend always has a local daemon
# to talk to. Idempotent: if the pipe already answers, this is a no-op.
# Detached: the daemon outlives this script, the launcher, and every
# frontend relaunch -- it is a persistent per-host daemon exactly like the
# remote one, not a per-session spawn (contrast the old GUID-pipe -Local
# path this replaces).
#
# Binary+capsule resolution: the COMPLETE DEV pair (sotd.exe AND
# sot-capsule.exe both present in <DevBinDir>) wins over the COMPLETE
# install pair (<prefix>\bin\); refuse to START only when NEITHER pair is
# complete. This MATCHES the frontend's own dev-build-first resolution in
# launch-sot.ps1, deliberately: a dev checkout's frontend and this daemon
# (and its capsule runtime) must come from the SAME origin, or a dev
# frontend ends up talking to an older installed sot-capsule.exe -- exactly
# the skew ADR 0041's "same release" rule warns about. "Complete" matters: a
# partial dev build (sotd.exe without a matching sot-capsule.exe, e.g. built
# before that binary existed) is not preferred over a complete install pair
# -- `sot-capsule.exe` MUST be a sibling of whichever sotd.exe is chosen
# (rust/backend/src/capsule_workspace.rs resolves it via
# `current_exe().parent()`), so a daemon started from a directory missing
# that file can create workspaces that immediately fail to spawn. Refuse to
# start rather than run degraded.
#
# -Stop is different (codex follow-up): it only needs a resolvable
# sotd.exe, not the complete pair, to query the pipe name from -- stopping
# never spawns a capsule, so a missing/moved sot-capsule.exe is irrelevant
# to it, and the daemon being stopped is presumably already running with
# its own sotd.exe still resolvable at that same location regardless.
#
# Pipe naming (ADR 0042 L2b design C): resolved FIRST (the section above),
# THEN queried from the daemon itself -- `& $daemonExe session-socket-path
# local` -- which prints exactly what `rust/protocol`'s
# `session_socket::session_socket_path("local")` derives (the SAME function
# `sotd --label local` uses for its own `--socket` default), giving
# `\\.\pipe\sot-<user>-local`. This script no longer constructs that name
# itself: a hand-authored copy of a naming rule that also lives in the
# daemon is exactly the kind of two-sources-of-truth bug ADR 0042 L2b design
# A closes. Used for the probe, the spawn argv, AND the `-Stop` match, so
# all three can never diverge. `-PipeName` (tests) skips the query entirely
# -- a `-Stop`-only test doesn't need a real binary on disk to know what
# it's stopping.
#
# --project-root: the user's home (`$env:USERPROFILE`), the same convention
# the backend host uses (deploy/sotd.service, scripts/install.sh: `sotd
# --project-root $HOME --label sot`) -- a persistent daemon's default
# workspace root is the user's home, not this repo checkout.
#
# Liveness = a bounded named-pipe CONNECT probe, not a namespace listing. A
# pipe NAME persists under \\.\pipe\ while any dead client still holds a
# handle to it (and is listed even when every real instance is busy) --
# presence there is not health. Connecting (then immediately closing) is
# what proves a server is actually there to accept; the daemon treats an
# early close as a normal EOF (server.rs), so this probe is harmless to a
# live daemon. Used for the idempotency check, the readiness wait, AND stop
# confirmation (probe fails AND the matched process is gone -- a process
# that's still exiting can leave the pipe briefly unconnectable without
# actually being gone yet).
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
# via `--resume` on its next start (ADR 0042 L1a). Matched for -Stop by an
# EXACT process match: process name `sotd.exe` AND a `--socket` token
# followed by exactly this pipe path at a token boundary in its command
# line -- not a bare substring match, which could also hit an unrelated
# sotd whose pipe name happens to contain this one.
#
# ADR 0042 L2b consequence, not a regression covered elsewhere: because the
# pipe name is now DERIVED (queried from a resolved binary) rather than
# constructed from `$env:USERNAME` alone, `-Stop` with no `-PipeName`
# override now ALSO needs a resolvable dev-or-install pair to know what to
# stop -- previously it could compute the pipe name with no binary present
# at all. In practice this is a non-issue: a running daemon pins its own
# `sotd.exe` as a mapped image while it runs, so if the daemon is up, ITS
# binary is still resolvable at its original location.
#
# Spawn hygiene: stdout/stderr are redirected to
# <prefix>\logs\sotd-local.{stdout,stderr}.log -- Start-Process TRUNCATES
# these on every (re)start, same as the frontend's own logs (see
# launch-sot.ps1's header); this is fine here because a start only happens
# when the pipe was NOT already answering, i.e. rarely. The daemon's own
# private log (rust/backend/src/main.rs::open_private_log_file, via
# paths::state_dir()) is a SEPARATE, HOME-derived path that today has no
# Windows branch -- a known gap, not fixed here (see the ADR amendment). If
# the spawned process exits before the pipe comes up, that's logged with its
# exit code and the wait stops early rather than spinning out the full
# bound; if the pipe never comes up within the bound, the process we just
# spawned is stopped so a hung daemon can't accumulate across retries.
#
# Standalone + parameterized (mirrors scripts/sot-apply.ps1's own -Prefix
# test-override convention) so this is independently testable without the
# rest of launch-sot.ps1/shutdown-sot.ps1 -- see
# scripts/tests/test-local-daemon.ps1. Called from EVERY launch-sot.ps1
# mode now (ADR 0042 L2b design D; -Local ensures + errors hard on failure,
# the default mode ensures + fails open) and shutdown-sot.ps1 (last, after
# the FE and tunnel are down, -Stop).
#
# ASCII ONLY in string literals (see the same note in launch-sot.ps1): this
# file has no BOM, so Windows PowerShell 5.1 decodes it as cp1252 and a
# non-ASCII byte inside a string literal can mojibake into a phantom quote
# and fail the whole parse.
#
# Exit codes: 0 = the daemon is confirmed answering on the pipe (already was,
# or was just started) -- or, for -Stop, confirmed stopped/was not running.
# 1 = not ready (no complete binary pair to resolve or query, it did not
# come up, or it did not go down within the bound) -- callers treat this as
# their own error path (launch-sot.ps1's -Local shows its existing "not
# found" dialog class; the default mode logs and continues; shutdown-sot.ps1
# logs a WARNING).

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
    # a real per-user daemon, and so a -Stop-only test needs no real binary
    # on disk). Default: queried from the resolved sotd.exe (see header).
    [string]$PipeName
)

$ErrorActionPreference = 'Continue'

if (-not $Prefix) { $Prefix = Join-Path $env:LOCALAPPDATA 'sot' }
if (-not $DevBinDir) {
    $repo = Resolve-Path -Path (Join-Path $PSScriptRoot '..')
    $DevBinDir = Join-Path $repo 'rust\target\release'
}
if (-not $ProjectRoot) { $ProjectRoot = $env:USERPROFILE }

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

# Bounded connect probe (500ms) -- see the header for why this replaces a
# namespace listing. Always closes/disposes, so a live daemon just sees one
# harmless connect-then-EOF.
function Test-SotPipeOpen {
    param([string]$Name)
    $client = New-Object System.IO.Pipes.NamedPipeClientStream('.', $Name, [System.IO.Pipes.PipeDirection]::InOut)
    try {
        $client.Connect(500)
        return $true
    } catch {
        return $false
    } finally {
        $client.Dispose()
    }
}

function Test-CompletePair {
    param([string]$Dir)
    (Test-Path (Join-Path $Dir 'sotd.exe')) -and (Test-Path (Join-Path $Dir 'sot-capsule.exe'))
}

function Find-SotdExe {
    param([string]$Dir)
    $exe = Join-Path $Dir 'sotd.exe'
    if (Test-Path $exe) { return $exe }
    return $null
}

# ---- resolve the binary FIRST (unchanged dev-then-install preference) ------
# Codex follow-up: -Stop only needs a resolvable sotd.exe to query the pipe
# name from -- sot-capsule.exe is a START requirement (a daemon that can't
# spawn capsule workspaces should never be started fresh), not a stop one.
# The daemon -Stop is trying to reach is presumably already running, with
# whatever sotd.exe it started from still resolvable at that same location
# (a running process pins its own binary as a mapped image) -- requiring
# the CURRENT resolution to also find a sibling sot-capsule.exe would
# refuse to stop a daemon whose capsule binary was since removed/moved,
# for no safety benefit (stopping never spawns a capsule).
$installBinDir = Join-Path $Prefix 'bin'
$daemonExe = $null
if ($Stop) {
    $daemonExe = Find-SotdExe $DevBinDir
    if (-not $daemonExe) { $daemonExe = Find-SotdExe $installBinDir }
} else {
    if (Test-CompletePair $DevBinDir) {
        $daemonExe = Join-Path $DevBinDir 'sotd.exe'
    } elseif (Test-CompletePair $installBinDir) {
        $daemonExe = Join-Path $installBinDir 'sotd.exe'
    }
}

# ---- pipe path: queried from the daemon, not constructed here (design C) ---
# $queriedRaw tracks whether a query was actually attempted and what it
# came back with, verbatim -- so the refusal below can tell "no binary to
# ask" apart from "asked a resolved sotd.exe, got something that is not a
# \\.\pipe\ path" (a STALE pre-0.6 sotd.exe, which predates the Windows
# branch in session_socket_path -- ADR 0042 L1c/L2b -- and so answers with
# a Unix-shaped socket path instead, or nothing at all). A field report hit
# exactly this: a COMPLETE but WEEKS-STALE dev pair took the absent-pair
# branch below, blaming absence when the pair was merely stale.
$queriedRaw = $null
if ($PipeName) {
    $PipePath = '\\.\pipe\' + $PipeName
} elseif ($daemonExe) {
    # try/catch + 2>$null: a stale or unexecutable sotd.exe must yield an
    # EMPTY answer (the diagnostic branch below), not a NativeCommandFailed
    # record -- under a caller whose ErrorActionPreference is Stop (the test
    # harness, any strict launcher) that record is terminating on 5.1 and
    # kills the caller instead of reaching the message.
    $queried = try { (& $daemonExe session-socket-path local 2>$null | Select-Object -First 1) } catch { $null }
    $queriedRaw = if ($queried) { $queried.ToString().Trim() } else { '' }
    $PipePath = if ($queriedRaw) { $queriedRaw } else { $null }
    if ($PipePath -and $PipePath.StartsWith('\\.\pipe\')) {
        $PipeName = $PipePath.Substring(9)
    } else {
        $PipePath = $null
    }
} else {
    $PipePath = $null
}

if (-not $PipePath -or -not $PipeName) {
    if ($null -ne $queriedRaw) {
        # $daemonExe resolved and answered the query, but not with a
        # \\.\pipe\ path -- distinct from "no pair found" below, and the
        # message the field report needed: the pair is PRESENT, just too
        # old to derive a Windows pipe name.
        Write-LocalDaemonLog "REFUSED: $daemonExe session-socket-path local returned '$queriedRaw' (not a \\.\pipe\ path) - stale sotd.exe (pre-0.6, no Windows pipe derivation) - rebuild the pair"
    } elseif ($Stop) {
        Write-LocalDaemonLog "REFUSED: no sotd.exe found to derive the pipe name (checked dev $DevBinDir and install $installBinDir)"
    } else {
        Write-LocalDaemonLog "REFUSED: no complete sotd.exe+sot-capsule.exe pair found to derive the pipe name (checked dev $DevBinDir and install $installBinDir)"
    }
    exit 1
}

function Get-LocalDaemonProcess {
    $pat = '(?i)--socket\s+"?' + [regex]::Escape($PipePath) + '"?(\s|$)'
    Get-CimInstance Win32_Process -Filter "Name='sotd.exe'" |
        Where-Object { $_.CommandLine -and ($_.CommandLine -match $pat) }
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
    # Bounded confirmation the pipe actually went away AND the process is
    # actually gone -- never an unbounded wait; a stubborn process just gets
    # reported, not retried forever.
    $down = $false
    for ($i = 0; $i -lt 8; $i++) {
        Start-Sleep -Milliseconds 250
        if ((-not (Test-SotPipeOpen $PipeName)) -and ((@(Get-LocalDaemonProcess)).Count -eq 0)) {
            $down = $true
            break
        }
    }
    if ($down) {
        Write-LocalDaemonLog "stop: confirmed down (pipe unreachable, process gone)"
        exit 0
    } else {
        Write-LocalDaemonLog "stop: still not confirmed down after 2s - may still be tearing down"
        exit 1
    }
}

# ---- ensure-started (default mode) ------------------------------------------

if (Test-SotPipeOpen $PipeName) {
    Write-LocalDaemonLog "already running on $PipePath"
    exit 0
}

if (-not $daemonExe) {
    Write-LocalDaemonLog "REFUSED: no complete sotd.exe+sot-capsule.exe pair found (checked dev $DevBinDir and install $installBinDir)"
    exit 1
}

$logDir = Join-Path $Prefix 'logs'
New-Item -ItemType Directory -Force -Path $logDir | Out-Null
$daemonStdout = Join-Path $logDir 'sotd-local.stdout.log'
$daemonStderr = Join-Path $logDir 'sotd-local.stderr.log'

# Single pre-quoted argument STRING, not an array: PowerShell 5.1's
# Start-Process joins an -ArgumentList array with spaces and drops the
# quotes it would otherwise add around an element containing a space, so a
# project root or pipe path with a space silently splits into stray argv.
# A single string is passed through to CreateProcess's command line as-is,
# quotes intact.
$daemonArgLine = '--socket "{0}" --project-root "{1}" --label local' -f $PipePath, $ProjectRoot
Write-LocalDaemonLog "starting: $daemonExe $daemonArgLine"
try {
    $proc = Start-Process -FilePath $daemonExe -ArgumentList $daemonArgLine `
        -RedirectStandardOutput $daemonStdout -RedirectStandardError $daemonStderr `
        -WindowStyle Hidden -PassThru
} catch {
    Write-LocalDaemonLog "REFUSED: failed to start $daemonExe - $($_.Exception.Message)"
    exit 1
}
Write-LocalDaemonLog "spawned pid=$($proc.Id)"

# Bounded wait for the pipe to come up -- same 20x250ms=5s shape the remote
# path already uses (launch-sot.ps1's socket-wait loop) for an analogous
# "did the daemon we just started actually bind" check. Stops early (a) if
# the process already exited (nothing to wait for) or (b) once the pipe
# answers.
$up = $false
for ($i = 0; $i -lt 20; $i++) {
    Start-Sleep -Milliseconds 250
    $proc.Refresh()
    if ($proc.HasExited) {
        Write-LocalDaemonLog "REFUSED: $daemonExe exited during startup (code=$($proc.ExitCode))"
        exit 1
    }
    if (Test-SotPipeOpen $PipeName) { $up = $true; break }
}
if ($up) {
    Write-LocalDaemonLog "pipe=$PipePath"
    exit 0
} else {
    Write-LocalDaemonLog "did not come up on $PipePath within 5s - stopping pid=$($proc.Id) so it cannot accumulate"
    Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
    exit 1
}

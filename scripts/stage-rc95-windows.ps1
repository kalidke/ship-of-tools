# stage-rc95-windows.ps1 — get this Windows box onto v0.6.6-rc9.5 in one run.
#
# Replaces the five-step manual page. Every command in here is quoted from the
# version this box actually runs (v0.6.6-rc9.3). It checks the expected output
# after every step and stops with a plain message if a step does not match.
#
#   .\stage-rc95-windows.ps1            full run
#   .\stage-rc95-windows.ps1 -Verify    only re-check whether staging landed
#
# WHY THIS EXISTS: this box cannot install a candidate normally, because our own
# file watcher holds a handle on the directory the updater renames into place and
# Windows refuses the rename. The fix ships IN rc9.5 but cannot reach the box,
# because the broken watcher is what blocks it landing. This is the one-time
# route around that: run the daemon with its watching capped while the stage
# completes. The cap lives only as long as that daemon process, and the normal
# launch at the end restarts it uncapped.

[CmdletBinding()]
param(
    [switch]$Verify
)

$ErrorActionPreference = 'Continue'

$TARGET_DIR  = 'v0.6.6-rc9.5-windows-x86_64'
$TMP_DIR     = 'tmp-v0.6.6-rc9.5-windows-x86_64'
$SOT_EXE     = Join-Path $env:LOCALAPPDATA 'sot\bin\sot.exe'
$DAEMON_PS1  = Join-Path $env:LOCALAPPDATA 'sot\repo\current\scripts\sot-local-daemon.ps1'
$LOG_DAEMON  = Join-Path $env:LOCALAPPDATA 'sot\logs\sotd-local.stderr.log'
$LOG_FE      = Join-Path $env:LOCALAPPDATA 'sot\logs\frontend.stderr.log'
$LOG_FE_PREV = Join-Path $env:LOCALAPPDATA 'sot\logs\frontend.stderr.log.prev'

function Fail([string]$msg) {
    Write-Host ''
    Write-Host "STOPPED: $msg"
    Write-Host ''
    Write-Host 'Nothing further was changed. Send this whole message back and we will take it from here.'
    exit 1
}
function Step([string]$n) { Write-Host ''; Write-Host "== $n" }
function Good([string]$msg) { Write-Host "   ok: $msg" }

function Test-StageLanded([string]$updatesRoot) {
    Step 'Checking whether the stage landed'

    $lines = @()
    foreach ($f in @($LOG_DAEMON, $LOG_FE, $LOG_FE_PREV)) {
        if (Test-Path $f) {
            $lines += @(Select-String -Path $f -Pattern 'update staged|staging update failed|fe self-update: staging failed' -ErrorAction SilentlyContinue | Select-Object -Last 10)
        }
    }
    foreach ($h in $lines) { Write-Host ('   ' + $h.Line) }

    $pending = Join-Path $updatesRoot 'pending-windows-x86_64.json'
    $staged = @($lines | Where-Object { $_.Line -match 'update staged' }).Count -gt 0
    $failed = @($lines | Where-Object { $_.Line -match 'staging update failed|staging failed' }).Count -gt 0

    if ($failed -and -not $staged) {
        Fail 'staging failed. The reason is carried in the failure line printed above, on that same line - send it back whole.'
    }
    if (-not $staged) {
        Fail 'no "update staged" line in either log yet. If the frontend stages on this box, start Ship of Tools, leave it up about three minutes, then run:  .\stage-rc95-windows.ps1 -Verify'
    }
    if (-not (Test-Path $pending)) {
        Fail "a stage was logged but $pending is not there, so nothing is armed. Send this back."
    }

    Good 'staged, and the pending pointer is armed'

    Write-Host ''
    Write-Host '== Done here. One thing is left and it is yours:'
    Write-Host ''
    Write-Host '   Launch Ship of Tools normally, from the desktop shortcut.'
    Write-Host '   The launcher stops the daemon itself before swapping the binaries,'
    Write-Host '   so the swap runs with no watcher alive.'
    Write-Host ''
    Write-Host '   Then confirm with:'
    Write-Host '     & "$env:LOCALAPPDATA\sot\bin\sot.exe" --version'
    Write-Host ''
    Write-Host '   It should report 0.6.6-rc9.5.'
    Write-Host ''
}

Write-Host ''
Write-Host 'NOTE: this script cannot close the Ship of Tools window and cannot relaunch it for you -- the launcher has to take the lock itself, and closing the window would end whatever you have typed in its Terminal drawer. It will tell you when to do each by hand.'

# --- preflight -------------------------------------------------------------

Step 'Preflight'

if (-not (Test-Path $SOT_EXE))    { Fail "no sot.exe at $SOT_EXE -- this does not look like an installed box." }
if (-not (Test-Path $DAEMON_PS1)) { Fail "no sot-local-daemon.ps1 at $DAEMON_PS1 -- cannot stop or start the daemon." }
Good 'installed layout found'

if (-not $Verify) {
    $running = @(Get-Process -Name 'sot' -ErrorAction SilentlyContinue)
    if ($running.Count -gt 0) {
        Fail "the Ship of Tools window is still open (sot.exe, pid $($running[0].Id)). Close it, then run this again. If you leave it open the update silently does nothing: only one launcher runs at a time, so a second one sees the first, concludes there is nothing to do, and exits without applying anything and without telling you."
    }
    Good 'Ship of Tools window is closed'
}

# --- step 0: who stages on this box ---------------------------------------

Step 'Step 0 - asking the box which process stages here (changes nothing)'

$status = & $SOT_EXE --update-status 2>&1 | Out-String
if ([string]::IsNullOrWhiteSpace($status)) {
    Fail '--update-status printed nothing. That command does exist in this version, so an empty answer is itself the problem - send it back.'
}
Write-Host $status.TrimEnd()

$updatesRoot = $null
if ($status -match '(?m)^\s*updates root\s+(.+?)\s*$') { $updatesRoot = $Matches[1].Trim() }
if (-not $updatesRoot) { $updatesRoot = Join-Path $env:LOCALAPPDATA 'sot\updates' }

if (($status -match 'staged') -and ($status -match 'armed')) {
    $stager = 'frontend'
} elseif ($status -match 'backend') {
    $stager = 'daemon'
} else {
    Fail 'could not tell from --update-status whether the frontend or the daemon stages on this box. Its output is printed above - send it back.'
}
Good "the $stager stages on this box"
Good "updates root: $updatesRoot"

if ($Verify) {
    Test-StageLanded $updatesRoot
    exit 0
}

# --- step 1: stop the local daemon ----------------------------------------

Step 'Step 1 - stopping the local daemon (capsule sessions survive this)'

$stopOut = & $DAEMON_PS1 -Stop 2>&1 | Out-String
Write-Host $stopOut.TrimEnd()
if (($stopOut -notmatch 'confirmed down') -and ($stopOut -notmatch 'not running')) {
    Fail 'the daemon did not confirm it stopped. Expected a line containing "confirmed down" or "not running". Its output is above.'
}
Good 'daemon is down'

# --- step 2: finish a stage already on disk -------------------------------

Step 'Step 2 - finishing a stage that is already on disk, if there is one'

$tmp = Join-Path $updatesRoot $TMP_DIR
$dst = Join-Path $updatesRoot $TARGET_DIR

if (-not (Test-Path $tmp)) {
    Good "no $TMP_DIR present - nothing to finish, the download happens after step 3"
} elseif (-not (Test-Path (Join-Path $tmp 'manifest.json'))) {
    Good 'a partial stage is present but unfinished - leaving it; it will be redone and the download reused'
} else {
    New-Item -ItemType Directory -Force -Path $dst | Out-Null
    Move-Item -Path (Join-Path $tmp '*') -Destination $dst -ErrorAction SilentlyContinue
    if (-not (Test-Path (Join-Path $dst 'manifest.json'))) {
        Copy-Item -Path (Join-Path $tmp '*') -Destination $dst -Recurse -ErrorAction SilentlyContinue
    }
    if (-not (Test-Path (Join-Path $dst 'manifest.json'))) {
        Fail "could not finish the stage into $dst - neither a move nor a copy put manifest.json there."
    }
    Good "finished the stage into $TARGET_DIR"
}

# --- step 3: restart the daemon with watching capped ----------------------

Step 'Step 3 - restarting the daemon with its watching capped (this is what unblocks the rename)'

$env:SOT_WATCH_BUDGET = '1'
$startOut = & $DAEMON_PS1 2>&1 | Out-String
Write-Host $startOut.TrimEnd()
if ($startOut -notmatch 'spawned pid=') {
    Fail 'the daemon did not report "spawned pid=". Its output is above.'
}
Good 'daemon started'

Start-Sleep -Seconds 5
$watch = @(Select-String -Path $LOG_DAEMON -Pattern 'file watcher registered' -ErrorAction SilentlyContinue | Select-Object -Last 5)
if ($watch.Count -eq 0) {
    Fail 'the daemon has logged no "file watcher registered" line yet. Give it a few seconds and run this again.'
}
$bad = @($watch | Where-Object { $_.Line -notmatch 'watched=1(\D|$)' })
if ($bad.Count -gt 0) {
    foreach ($h in $watch) { Write-Host ('   ' + $h.Line) }
    Fail 'the watch cap did not reach the daemon - at least one line above shows a number other than watched=1. Run this whole script again in a fresh window.'
}
Good 'watch cap confirmed: every recent line reads watched=1'

# --- step 4: let it stage, then check -------------------------------------

Step 'Step 4 - waiting for the stage'

if ($stager -eq 'frontend') {
    Write-Host ''
    Write-Host 'The FRONTEND stages on this box, and this script cannot start it for you.'
    Write-Host ''
    Write-Host 'Do this now, in order:'
    Write-Host '  1. Start Ship of Tools normally, from the desktop shortcut.'
    Write-Host '  2. Leave it up about three minutes so it downloads.'
    Write-Host '  3. Come back here and run:  .\stage-rc95-windows.ps1 -Verify'
    Write-Host ''
    exit 0
}

Write-Host '   the daemon stages on this box - waiting 3 minutes'
Start-Sleep -Seconds 180

Test-StageLanded $updatesRoot

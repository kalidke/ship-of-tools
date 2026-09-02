# test-local-daemon.ps1 -- regression harness for scripts/sot-local-daemon.ps1
# (ADR 0042 L1c: -Local starts a local sotd, idempotently and detached, and
# shutdown-sot.ps1 stops it last; L2b design C: the pipe name is queried
# from the daemon itself -- section 6 -- and every launch mode ensures it).
#
# Section 0 syntax-parses every .ps1 this unit touched -- this repo's CI has
# no PSScriptAnalyzer (grepped: zero hits repo-wide), only the
# `[System.Management.Automation.Language.Parser]::ParseFile` gate in
# .github/workflows/rust.yml's "Parse PowerShell scripts" step, which globs
# `scripts/*.ps1` WITHOUT -Recurse -- it never reaches scripts/tests/*.ps1,
# so this file re-does that check for itself and its siblings.
#
# Sections 1-2 exercise the pure logic (binary/capsule resolution, refusal,
# append-only logging) with placeholder files, matching test-sot-apply.ps1's
# own "fake binaries are never executed" convention -- sot-capsule.exe is
# NEVER executed by anything sot-local-daemon.ps1 does (only Test-Path'd), so
# a placeholder file is exactly as good as a real one for every case here.
#
# Sections 3-6 need a REAL, runnable sotd.exe (it must actually bind a named
# pipe, and section 6 must actually answer `session-socket-path`) -- sourced
# from rust\target\debug\sotd.exe, which the SAME CI job already builds one
# step earlier (`cargo build --workspace --locked`, no --release =>
# target\debug; see .github/workflows/rust.yml). Falls back to a release
# build for a local dev run. On CI ($env:CI, set by GitHub) a missing real
# sotd.exe is a FAIL -- that job already built one, so its absence means
# something upstream broke, not "nothing to test here"; off CI (a dev box
# that hasn't built anything) it SKIPs instead, so this file stays runnable
# without a build. Section 6 additionally only runs ON CI even when a real
# sotd.exe IS present -- see its own comment for why.
#
# Before touching a REAL sotd.exe, sections 3-5 redirect HOME/USERPROFILE/
# LOCALAPPDATA/XDG_STATE_HOME/XDG_CONFIG_HOME at directories under the test
# root: the spawned daemon reads LOCALAPPDATA via sot_log::state_dir and
# HOME/XDG via rust/backend/src/paths.rs for its OWN state (workspace
# registry, capsule resume-scan) -- without this it would read/write the
# REAL developer state and try to resume real capsule workspaces against the
# fake sot-capsule.exe this file plants. Restored, along with every process
# this file spawns, in ONE outer try/finally so a terminating error midway
# through sections 3-5 cannot leak a process or leave the environment
# pointed at the fixture.
#
# Run under WINDOWS POWERSHELL 5.1 specifically -- same reason as
# test-sot-apply.ps1 (the .lnk launcher's host; 5.1 decodes a BOM-less .ps1
# as cp1252 where pwsh 7 decodes it as UTF-8).
#
# ASCII ONLY (see the same note in sot-local-daemon.ps1 / launch-sot.ps1).
#
# Every wait below is BOUNDED (Wait-Pipe/Wait-PipeGone poll with a timeout,
# never an unbounded loop).
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\tests\test-local-daemon.ps1

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$script = Join-Path $repo 'scripts\sot-local-daemon.ps1'
$root = Join-Path $env:TEMP ("sot-local-daemon-test-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
$pass = 0; $fail = 0

function Check([string]$name, [bool]$ok, [string]$detail) {
    if ($ok) { $script:pass++; Write-Host "  PASS  $name" -ForegroundColor Green }
    else { $script:fail++; Write-Host "  FAIL  $name -- $detail" -ForegroundColor Red }
}
function Note-Skip([string]$name, [string]$why) {
    Write-Host "  SKIP  $name -- $why" -ForegroundColor Yellow
}

function New-Fixture {
    param([string]$Prefix, [switch]$WithCapsule, [string]$SotdSource)
    Remove-Item -LiteralPath $Prefix -Recurse -Force -ErrorAction SilentlyContinue
    $bin = Join-Path $Prefix 'bin'
    New-Item -ItemType Directory -Force -Path $bin | Out-Null
    if ($SotdSource) {
        Copy-Item -LiteralPath $SotdSource -Destination (Join-Path $bin 'sotd.exe') -Force
    } else {
        Set-Content -LiteralPath (Join-Path $bin 'sotd.exe') -Value 'FAKE-SOTD-NEVER-EXECUTED' -NoNewline
    }
    if ($WithCapsule) {
        Set-Content -LiteralPath (Join-Path $bin 'sot-capsule.exe') -Value 'FAKE-CAPSULE-NEVER-EXECUTED' -NoNewline
    }
}

function New-TestPipeName { 'test-sot-ld-' + [guid]::NewGuid().ToString('N').Substring(0, 8) }
function Get-PipePath([string]$Name) { '\\.\pipe\' + $Name }

# Bounded connect probe (500ms), matching sot-local-daemon.ps1's own
# Test-SotPipeOpen exactly -- a namespace listing is not a health check (see
# that script's header for why), so the test must observe the same fact
# production does, not a weaker proxy for it. try/catch even though
# $ErrorActionPreference is 'Stop' at file scope, so a transient failure
# here fails one Check, not the whole suite.
function Test-PipeAnswering([string]$Name) {
    try {
        $client = New-Object System.IO.Pipes.NamedPipeClientStream('.', $Name, [System.IO.Pipes.PipeDirection]::InOut)
        try {
            $client.Connect(500)
            return $true
        } finally {
            $client.Dispose()
        }
    } catch {
        return $false
    }
}
function Wait-Pipe([string]$Name, [int]$TimeoutMs = 5000) {
    $elapsed = 0
    while ($elapsed -lt $TimeoutMs) {
        if (Test-PipeAnswering $Name) { return $true }
        Start-Sleep -Milliseconds 200
        $elapsed += 200
    }
    return $false
}
function Wait-PipeGone([string]$Name, [int]$TimeoutMs = 5000) {
    $elapsed = 0
    while ($elapsed -lt $TimeoutMs) {
        if (-not (Test-PipeAnswering $Name)) { return $true }
        Start-Sleep -Milliseconds 200
        $elapsed += 200
    }
    return $false
}

# Exact match, mirroring sot-local-daemon.ps1's own Get-LocalDaemonProcess:
# a --socket token followed by exactly this pipe PATH at a token boundary --
# not a bare substring match, which could also hit an unrelated sotd whose
# pipe name happens to contain this one.
function Get-DaemonProcs([string]$PipePath) {
    # Callers wrap the result in @(...): a single CimInstance unrolled on
    # return answers .Count with $null (adapted-object property lookup wins
    # over the scalar Count intrinsic on 5.1), which failed CI as "found ".
    $pat = '(?i)--socket\s+"?' + [regex]::Escape($PipePath) + '"?(\s|$)'
    Get-CimInstance Win32_Process -Filter "Name='sotd.exe'" |
        Where-Object { $_.CommandLine -and ($_.CommandLine -match $pat) }
}

$realSotd = Join-Path $repo 'rust\target\debug\sotd.exe'
if (-not (Test-Path $realSotd)) { $realSotd = Join-Path $repo 'rust\target\release\sotd.exe' }
$haveRealSotd = Test-Path $realSotd

$fakeSup = $null
$envSaved = $null
$testPipePrefix = 'test-sot-ld-'

try {
    Write-Host "`n=== 0. syntax parse of every .ps1 this unit touches ===" -ForegroundColor Cyan
    foreach ($f in @(
            (Join-Path $repo 'scripts\sot-local-daemon.ps1'),
            (Join-Path $repo 'scripts\launch-sot.ps1'),
            (Join-Path $repo 'scripts\shutdown-sot.ps1'),
            (Join-Path $repo 'scripts\tests\test-local-daemon.ps1')
        )) {
        $errs = $null
        [void][System.Management.Automation.Language.Parser]::ParseFile($f, [ref]$null, [ref]$errs)
        $detail = ($errs | ForEach-Object { "$($_.Extent.StartLineNumber): $($_.Message)" }) -join '; '
        Check "parses: $(Split-Path $f -Leaf)" ($errs.Count -eq 0) $detail
    }

    Write-Host "`n=== 1. refusal when sot-capsule.exe is missing ===" -ForegroundColor Cyan
    $p1 = Join-Path $root 'p1'
    New-Fixture -Prefix $p1
    $pipe1 = New-TestPipeName
    $out1 = & $script -Prefix $p1 -DevBinDir 'C:\sot-test-does-not-exist' -PipeName $pipe1 -ProjectRoot $root 6>&1 2>&1
    $exit1 = $LASTEXITCODE
    Check 'exit code 1' ($exit1 -eq 1) "got $exit1; log: $out1"
    Check 'refused for the right reason' ((($out1 -join ' ')) -match 'REFUSED.*sot-capsule') "log was: $out1"
    Check 'pipe never opened' (-not (Wait-Pipe $pipe1 -TimeoutMs 500)) 'pipe opened despite refusal'
    $log1 = Join-Path $p1 'logs\sotd-local.log'
    Check 'log file written' (Test-Path $log1) 'no log file'
    $lines1 = if (Test-Path $log1) { (Get-Content $log1).Count } else { 0 }

    Write-Host "`n=== 2. log file is append-only across invocations ===" -ForegroundColor Cyan
    $null = & $script -Prefix $p1 -DevBinDir 'C:\sot-test-does-not-exist' -PipeName $pipe1 -ProjectRoot $root 6>&1 2>&1
    $lines2 = (Get-Content $log1).Count
    Check 'log grew, was not truncated' ($lines2 -gt $lines1) "was $lines1 lines, now $lines2"

    Write-Host "`n=== 2b. stale/unanswerable sotd.exe -- distinct diagnostic from an absent pair ===" -ForegroundColor Cyan
    # A field report hit a COMPLETE but WEEKS-STALE dev pair taking the
    # ABSENT-pair refusal (section 1's message) -- blaming absence when the
    # pair was merely too old to answer `session-socket-path local` with a
    # Windows pipe path. This exercises the fix: a COMPLETE pair (both
    # files present, -WithCapsule, so $daemonExe resolves and the query is
    # actually attempted) but a PLACEHOLDER sotd.exe.
    #
    # No -PipeName override here -- that would skip the query entirely and
    # defeat the point. This file has no existing pattern for an
    # EXECUTABLE fake (every fake binary elsewhere in this suite, including
    # sot-capsule.exe, is Test-Path'd only, per the file header -- never
    # invoked), so introducing a .cmd/.ps1 stand-in would be new machinery
    # for one case. Smaller: a placeholder whose query FAILS is enough to
    # exercise the "not a \\.\pipe\ path" branch -- sot-local-daemon.ps1
    # runs the query under $ErrorActionPreference = 'Continue', so invoking
    # a non-executable placeholder is a non-terminating error there; the
    # query result stays empty, which the fix treats exactly like a
    # non-pipe-shaped answer (verbatim-logged, not silently dropped).
    $p2b = Join-Path $root 'p2b'
    New-Fixture -Prefix $p2b -WithCapsule
    $pipe2b = New-TestPipeName
    $out2b = & $script -Prefix $p2b -DevBinDir 'C:\sot-test-does-not-exist' -ProjectRoot $root 6>&1 2>&1
    $exit2b = $LASTEXITCODE
    Check 'exit code 1' ($exit2b -eq 1) "got $exit2b; log: $out2b"
    Check 'refused as stale, not as absent' ((($out2b -join ' ')) -match 'REFUSED.*stale sotd\.exe') "log was: $out2b"
    Check 'pipe never opened' (-not (Wait-Pipe $pipe2b -TimeoutMs 500)) 'pipe opened despite refusal'

    # Same distinct diagnostic under -Stop (the fix applies before the
    # $Stop branch, so both paths share it).
    $outStop2b = & $script -Stop -Prefix $p2b -DevBinDir 'C:\sot-test-does-not-exist' -ProjectRoot $root 6>&1 2>&1
    $exitStop2b = $LASTEXITCODE
    Check '-Stop: exit code 1' ($exitStop2b -eq 1) "got $exitStop2b; log: $outStop2b"
    Check '-Stop: refused as stale, not as absent' ((($outStop2b -join ' ')) -match 'REFUSED.*stale sotd\.exe') "log was: $outStop2b"

    if (-not $haveRealSotd) {
        if ($env:CI) {
            Check '3-6. real sotd.exe present for process-behavior tests' $false 'no rust\target\{debug,release}\sotd.exe on a CI leg that already built the workspace -- upstream build gap, not a skip'
        } else {
            Note-Skip '3. start when absent' 'no rust\target\{debug,release}\sotd.exe built on this box'
            Note-Skip '4. no second start when the pipe already answers' 'no rust\target\{debug,release}\sotd.exe built on this box'
            Note-Skip '5. shutdown stops the daemon and leaves a fake supervisor alone' 'no rust\target\{debug,release}\sotd.exe built on this box'
            Note-Skip '6. derive pipe name from sotd session-socket-path' 'no rust\target\{debug,release}\sotd.exe built on this box'
        }
    } else {
        # Isolate the REAL daemon's own state from the real developer
        # environment -- see the file header. Restored in the outer finally.
        $fixtureHome = Join-Path $root 'home'
        $fixtureLocalAppData = Join-Path $root 'localappdata'
        New-Item -ItemType Directory -Force -Path $fixtureHome | Out-Null
        New-Item -ItemType Directory -Force -Path $fixtureLocalAppData | Out-Null
        $envSaved = @{
            HOME            = $env:HOME
            USERPROFILE     = $env:USERPROFILE
            LOCALAPPDATA    = $env:LOCALAPPDATA
            XDG_STATE_HOME  = $env:XDG_STATE_HOME
            XDG_CONFIG_HOME = $env:XDG_CONFIG_HOME
        }
        $env:HOME = $fixtureHome
        $env:USERPROFILE = $fixtureHome
        $env:LOCALAPPDATA = $fixtureLocalAppData
        $env:XDG_STATE_HOME = Join-Path $fixtureLocalAppData 'xdg-state'
        $env:XDG_CONFIG_HOME = Join-Path $fixtureLocalAppData 'xdg-config'

        # A project root WITH A SPACE -- proves the single pre-quoted
        # -ArgumentList string actually survives Start-Process's 5.1
        # array-join-and-drop-quotes behavior. A broken split would hand
        # sotd's arg parser a stray extra token and it would bail
        # (unrecognised argument) instead of binding the pipe, which the
        # exit-code and pipe-answers checks below would catch.
        $spacedProjectRoot = Join-Path $fixtureHome 'a project root'
        New-Item -ItemType Directory -Force -Path $spacedProjectRoot | Out-Null

        Write-Host "`n=== 3. start when absent (also proves --project-root quoting through a space) ===" -ForegroundColor Cyan
        $p3 = Join-Path $root 'p3'
        New-Fixture -Prefix $p3 -WithCapsule -SotdSource $realSotd
        $pipe3 = New-TestPipeName
        $pipePath3 = Get-PipePath $pipe3
        $out3 = & $script -Prefix $p3 -DevBinDir 'C:\sot-test-does-not-exist' -PipeName $pipe3 -ProjectRoot $spacedProjectRoot 6>&1 2>&1
        $exit3 = $LASTEXITCODE
        Check 'exit code 0 (space in --project-root did not break argv)' ($exit3 -eq 0) "got $exit3; log: $out3"
        Check 'pipe answers' (Wait-Pipe $pipe3) 'pipe never opened'
        $procs3 = @(Get-DaemonProcs $pipePath3)
        Check 'exactly one sotd.exe on this pipe' ($procs3.Count -eq 1) "found $($procs3.Count)"

        Write-Host "`n=== 4. no second start when the pipe already answers ===" -ForegroundColor Cyan
        $out4 = & $script -Prefix $p3 -DevBinDir 'C:\sot-test-does-not-exist' -PipeName $pipe3 -ProjectRoot $spacedProjectRoot 6>&1 2>&1
        $exit4 = $LASTEXITCODE
        Check 'exit code 0 (already running is success)' ($exit4 -eq 0) "got $exit4"
        Check 'said already running' ((($out4 -join ' ')) -match 'already running') "log was: $out4"
        $procs4 = @(Get-DaemonProcs $pipePath3)
        Check 'still exactly one sotd.exe (no second spawn)' ($procs4.Count -eq 1) "found $($procs4.Count)"
        if ($procs3.Count -eq 1 -and $procs4.Count -eq 1) {
            Check 'same pid (not restarted)' ($procs4[0].ProcessId -eq $procs3[0].ProcessId) 'pid changed'
        }

        Write-Host "`n=== 5. shutdown stops the daemon and leaves a fake supervisor alone ===" -ForegroundColor Cyan
        # Stand-in for a capsule supervisor: any long-lived NON-sotd.exe
        # process. Proves -Stop's exact match (Name='sotd.exe' + this exact
        # --socket token) never widens to anything else running alongside it.
        $fakeSup = Start-Process -FilePath 'powershell.exe' `
            -ArgumentList @('-NoProfile', '-Command', 'Start-Sleep -Seconds 60') `
            -WindowStyle Hidden -PassThru
        $out5 = & $script -Stop -Prefix $p3 -PipeName $pipe3 6>&1 2>&1
        $exit5 = $LASTEXITCODE
        Check 'stop exit code 0' ($exit5 -eq 0) "got $exit5; log: $out5"
        Check 'pipe gone' (Wait-PipeGone $pipe3) 'pipe still answering'
        $procs5 = @(Get-DaemonProcs $pipePath3)
        Check 'sotd.exe process gone' ($procs5.Count -eq 0) "still found $($procs5.Count)"
        Start-Sleep -Milliseconds 300
        $fakeSup.Refresh()
        Check 'fake supervisor left alone' (-not $fakeSup.HasExited) 'fake supervisor was killed too'

        Write-Host "`n=== 6. pipe name comes from 'sotd session-socket-path local', not a hardcoded guess ===" -ForegroundColor Cyan
        # ADR 0042 L2b design C: no -PipeName override here -- the script
        # must resolve $daemonExe itself and query IT for the pipe path,
        # exactly the path every real launch takes. CI-only: this exercises
        # the REAL per-user pipe (`\\.\pipe\sot-<the CI user>-local`, since
        # only HOME/USERPROFILE/LOCALAPPDATA/XDG_* are redirected above, not
        # USERNAME) -- safe on an ephemeral CI runner, but skipped on a dev
        # box where it could collide with a genuinely running local daemon.
        # (Also the only section that can exercise the -Stop/complete-pair
        # split below: that needs the daemon actually listening on the
        # SAME pipe -Stop will derive, which -PipeName-isolated sections
        # deliberately avoid.)
        if (-not $env:CI) {
            Note-Skip '6. derive pipe name from sotd session-socket-path' 'only run on CI -- exercises the REAL per-user pipe name'
        } else {
            $expectedPipe = (& $realSotd session-socket-path local | Select-Object -First 1)
            if ($expectedPipe) { $expectedPipe = $expectedPipe.ToString().Trim() }
            Check 'sotd itself derives a Windows named-pipe path' ($expectedPipe -like '\\.\pipe\sot-*-local') "got: $expectedPipe"
            $pipeName6 = $expectedPipe.Substring(9)   # strip '\\.\pipe\'
            $p6 = Join-Path $root 'p6'
            New-Fixture -Prefix $p6 -WithCapsule -SotdSource $realSotd
            try {
                $out6 = & $script -Prefix $p6 -DevBinDir 'C:\sot-test-does-not-exist' -ProjectRoot $spacedProjectRoot 6>&1 2>&1
                $exit6 = $LASTEXITCODE
                Check 'exit code 0 (started via the derived pipe)' ($exit6 -eq 0) "got $exit6; log: $out6"
                Check 'log names the derived pipe path' ((($out6 -join ' ')) -match [regex]::Escape($expectedPipe)) "log was: $out6"
                Check 'derived pipe answers' (Wait-Pipe $pipeName6) 'pipe never opened'
                $procs6 = @(Get-DaemonProcs $expectedPipe)
                Check 'exactly one sotd.exe on the derived pipe' ($procs6.Count -eq 1) "found $($procs6.Count)"

                # ADR 0042 L2b codex follow-up: sot-capsule.exe is a START
                # requirement, not a -Stop one. Remove it AFTER the daemon
                # is already running -- the "already running, capsule
                # binary since moved" case -- and confirm -Stop (still no
                # -PipeName override, so it re-derives $daemonExe and
                # queries it exactly as above) still succeeds despite the
                # now-incomplete pair.
                Remove-Item -LiteralPath (Join-Path $p6 'bin\sot-capsule.exe') -Force -ErrorAction SilentlyContinue
                Check 'sot-capsule.exe removed (pair now incomplete)' `
                    (-not (Test-Path (Join-Path $p6 'bin\sot-capsule.exe'))) 'removal did not take'
            } finally {
                # -Stop's OWN derive path, under test here too -- and the
                # guaranteed cleanup for this section regardless of which
                # Check above failed. Exit code asserted BEFORE the
                # belt-and-suspenders Stop-Process cleanup below, so a
                # nonzero -Stop can't hide behind that cleanup finishing
                # the job anyway.
                $stopOut6 = & $script -Stop -Prefix $p6 -DevBinDir 'C:\sot-test-does-not-exist' -ProjectRoot $spacedProjectRoot 6>&1 2>&1
                $stopExit6 = $LASTEXITCODE
                Check '-Stop exit code 0 (derived path, despite the incomplete pair)' ($stopExit6 -eq 0) "got $stopExit6; log: $stopOut6"
                Start-Sleep -Milliseconds 200
                Get-DaemonProcs $expectedPipe | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
            }
            Check 'derived pipe gone after stop' (Wait-PipeGone $pipeName6) 'pipe still answering after -Stop'
        }
    }
} finally {
    # ONE place for every cleanup this file owes, so a terminating error
    # anywhere above (not just a failed Check, which never throws) still
    # restores the environment and kills whatever got spawned.
    if ($envSaved) {
        foreach ($k in $envSaved.Keys) {
            if ($null -eq $envSaved[$k]) { Remove-Item "Env:\$k" -ErrorAction SilentlyContinue }
            else { Set-Item "Env:\$k" $envSaved[$k] }
        }
    }
    if ($fakeSup -and -not $fakeSup.HasExited) {
        try { Stop-Process -Id $fakeSup.Id -Force -ErrorAction SilentlyContinue } catch {}
    }
    Get-CimInstance Win32_Process -Filter "Name='sotd.exe'" |
        Where-Object { $_.CommandLine -and $_.CommandLine.Contains($testPipePrefix) } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "`n================ $pass passed, $fail failed ================" -ForegroundColor $(if ($fail) { 'Red' } else { 'Green' })
if ($fail) { exit 1 }

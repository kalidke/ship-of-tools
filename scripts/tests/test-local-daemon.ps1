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
# without a build. Section 6 runs under a USERNAME of its own, so the pipe
# it derives is never this box's own daemon's -- see its own comment.
# Sections 9-11 and 16 live in test-launcher-leases.ps1.
# Sections 4b, 4c and 5b2 live in test-local-daemon-own.ps1, dot-sourced after 5b in this scope.
# Section 5c's cases (iii)-(viii), the session pipe under load, live in test-local-daemon-pipe.ps1, dot-sourced there.
# Sections 2c and 7b, the local daemon's one binary, live in test-local-daemon-binary.ps1, dot-sourced after the fake.
#
# Before touching a REAL sotd.exe, sections 3-5 redirect HOME/USERPROFILE/
# LOCALAPPDATA/XDG_STATE_HOME/XDG_CONFIG_HOME at directories under the test
# root: the spawned daemon reads LOCALAPPDATA via sot_log::host::state_dir and
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

. (Join-Path $PSScriptRoot 'test-local-daemon-support.ps1')

try {
    try {
    Write-Host "`n=== 0. syntax parse of every .ps1 this unit touches ===" -ForegroundColor Cyan
    foreach ($f in @(
            (Join-Path $repo 'scripts\sot-local-daemon.ps1'),
            (Join-Path $repo 'scripts\launch-sot.ps1'),
            (Join-Path $repo 'scripts\sot-freshness.ps1'),
            (Join-Path $repo 'scripts\sot-lease.ps1'),
            (Join-Path $repo 'scripts\shutdown-sot.ps1'),
            (Join-Path $repo 'scripts\tests\pipe-request.ps1'),
            (Join-Path $repo 'scripts\tests\test-local-daemon-pipe.ps1'),
            (Join-Path $repo 'scripts\tests\test-local-daemon-own.ps1'),
            (Join-Path $repo 'scripts\tests\test-local-daemon-binary.ps1'),
            (Join-Path $repo 'scripts\tests\test-local-daemon.ps1')
        )) {
        $errs = $null
        [void][System.Management.Automation.Language.Parser]::ParseFile($f, [ref]$null, [ref]$errs)
        $detail = ($errs | ForEach-Object { "$($_.Extent.StartLineNumber): $($_.Message)" }) -join '; '
        Check "parses: $(Split-Path $f -Leaf)" ($errs.Count -eq 0) $detail
    }

    } catch { Check '0: section ran' $false $_.Exception.Message }
    try {
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

    } catch { Check '1: section ran' $false $_.Exception.Message }
    try {
    Write-Host "`n=== 2. log file is append-only across invocations ===" -ForegroundColor Cyan
    $null = & $script -Prefix $p1 -DevBinDir 'C:\sot-test-does-not-exist' -PipeName $pipe1 -ProjectRoot $root 6>&1 2>&1
    $lines2 = (Get-Content $log1).Count
    Check 'log grew, was not truncated' ($lines2 -gt $lines1) "was $lines1 lines, now $lines2"

    } catch { Check '2: section ran' $false $_.Exception.Message }
    try {
    Write-Host "`n=== 2b. unlaunchable placeholder sotd.exe -- distinct diagnostic from an absent pair ===" -ForegroundColor Cyan
    # A field report hit a COMPLETE but WEEKS-STALE dev pair taking the
    # ABSENT-pair refusal (section 1's message) -- blaming absence when the
    # pair was merely too old to answer `session-socket-path local` with a
    # Windows pipe path. This exercises the OTHER half of that fix: a
    # COMPLETE pair (both files present, -WithCapsule, so $daemonExe
    # resolves and the query is actually attempted) but a PLACEHOLDER
    # sotd.exe.
    #
    # No -PipeName override here -- that would skip the query entirely and
    # defeat the point. This file has no existing pattern for an
    # EXECUTABLE fake (every fake binary elsewhere in this suite, including
    # sot-capsule.exe, is Test-Path'd only, per the file header -- never
    # invoked), so introducing a .cmd/.ps1 stand-in would be new machinery
    # for one case. A placeholder is enough on its own: PowerShell cannot
    # LAUNCH it at all (not a valid Win32 application) -- confirmed on CI,
    # where this raised a terminating NativeCommandFailed before the
    # try/catch fix -- so it exercises the "could not execute" diagnostic,
    # NOT the stale-pipe one (that needs a process that actually RAN and
    # printed a non-pipe value; this file has no fake-executable pattern to
    # produce one). The query's SUCCESS path, and a real pre-0.6-shaped
    # answer, are both out of scope here -- section 6 below covers a real
    # sotd.exe answering with a genuine \\.\pipe\ path.
    $p2b = Join-Path $root 'p2b'
    New-Fixture -Prefix $p2b -WithCapsule
    $out2b = & $script -Prefix $p2b -DevBinDir 'C:\sot-test-does-not-exist' -ProjectRoot $root 6>&1 2>&1
    $exit2b = $LASTEXITCODE
    Check 'exit code 1' ($exit2b -eq 1) "got $exit2b; log: $out2b"
    Check 'refused as unlaunchable, not as absent' ((($out2b -join ' ')) -match 'REFUSED: could not execute') "log was: $out2b"

    # Same distinct diagnostic under -Stop (the fix applies before the
    # $Stop branch, so both paths share it).
    $outStop2b = & $script -Stop -Prefix $p2b -DevBinDir 'C:\sot-test-does-not-exist' -ProjectRoot $root 6>&1 2>&1
    $exitStop2b = $LASTEXITCODE
    Check '-Stop: exit code 1' ($exitStop2b -eq 1) "got $exitStop2b; log: $outStop2b"
    Check '-Stop: refused as unlaunchable, not as absent' ((($outStop2b -join ' ')) -match 'REFUSED: could not execute') "log was: $outStop2b"
    } catch { Check '2b: section ran' $false $_.Exception.Message }

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
        # No SOT_* variable of the runner's reaches a daemon this suite starts: a SOT_COMM_HOME would beat the
        # redirected HOME and open the live comm home. Restored by the same outer finally.
        foreach ($v in @(Get-ChildItem Env:SOT_*)) {
            $envSaved[$v.Name] = $v.Value
            Remove-Item "Env:\$($v.Name)" -ErrorAction SilentlyContinue
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

        try {
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

        } catch { Check '3: section ran' $false $_.Exception.Message }
        try {
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

        } catch { Check '4: section ran' $false $_.Exception.Message }
        try {
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

        } catch { Check '5: section ran' $false $_.Exception.Message }
        try {
        Write-Host "`n=== 5b. the launcher's lease against a real daemon of its own: its handoff hello is admitted, the lease granted, and its end shuts the daemon down (ADR 0049) ===" -ForegroundColor Cyan
        # Open-SotLease (scripts/sot-lease.ps1) writes the hello the daemon admits every connection by and the lease
        # line in one write. This is the launcher's lease against the real admission, which no fake can vouch for. The
        # daemon is a fresh one on its own pipe (a handover would keep it for its 60 s bound), and the lease's plain end
        # departs as a Close, which shuts it down.
        . (Join-Path $PSScriptRoot '..\sot-lease.ps1')
        $LeaseReplyWaitMs = 5000
        $HandoverBoundSeconds = 60
        $global:SotLeases = @()
        $script:supLines5b = @()
        function Write-SupLog { param([string]$Message) $script:supLines5b += $Message }
        $pipe5b = New-TestPipeName
        $out5b = & $script -Prefix $p3 -DevBinDir 'C:\sot-test-does-not-exist' -PipeName $pipe5b -ProjectRoot $spacedProjectRoot 6>&1 2>&1
        Check '5b: the daemon starts' (Wait-Pipe $pipe5b) "pipe never opened; log: $out5b"
        $streams5b = @(Open-SotLease (Get-PipePath $pipe5b) $realSotd)
        Check '5b: the real daemon grants the launcher a lease' ($streams5b.Count -eq 1) "got $($streams5b.Count); log: $($script:supLines5b -join ' | ')"
        # Only a granted lease has an end that shuts the daemon down.
        if ($streams5b.Count -eq 1) {
            foreach ($c in $streams5b) { try { $c.Dispose() } catch { } }
            Check '5b: the lease ending shuts the daemon down' (Wait-PipeGone $pipe5b) 'pipe still answering'
        }
        Get-DaemonProcs (Get-PipePath $pipe5b) | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }

        } catch { Check '5b: section ran' $false $_.Exception.Message }
        . (Join-Path $PSScriptRoot 'test-local-daemon-own.ps1')
        try {
        Write-Host "`n=== 5c. a raw pipe client (scripts\tests\pipe-request.ps1) against a real daemon of its own: a refused hello is printed and exits 1, an accepted one answers the request, and a request up to the envelope cap is answered, not stalled (ADR 0049) ===" -ForegroundColor Cyan
        # A raw pipe client (scripts\tests\pipe-request.ps1) matches a reply by its op. A refused hello is a reply to the hello, so the transport
        # must hand it over (and fail) rather than wait out its bound and report a silent daemon: the caller names the
        # refusal. (ii) is the same request with an accepted hello, so the transport's own answer path is exercised too.
        # This daemon closes after refusing, so the transport reads on past a protocol refusal to the end of the
        # connection, says so on stderr and exits 1; the stderr line is the transport's, not a failure of this section.
        . (Join-Path $PSScriptRoot '..\sot-lease.ps1')
        $pipe5c = New-TestPipeName
        try {
        $out5c = & $script -Prefix $p3 -DevBinDir 'C:\sot-test-does-not-exist' -PipeName $pipe5c -ProjectRoot $spacedProjectRoot 6>&1 2>&1
        Check '5c: the daemon starts' (Wait-Pipe $pipe5c) "pipe never opened; log: $out5c"
        $sid5c = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
        $host5c = ConvertTo-SotJsonString (Get-SotHelloHost)
        $request5c = '{"v":3,"id":1,"kind":"req","op":"version.query","payload":{}}'
        $old5c = '{"v":3,"id":0,"kind":"req","op":"hello","payload":{"client_id":"t-cli","protocol":2,"app_version":"t","host":' + $host5c + ',"os_user":"' + $sid5c + '","role":"cli"}}'
        $new5c = '{"v":3,"id":0,"kind":"req","op":"hello","payload":{"client_id":"t-cli","protocol":3,"app_version":"t","host":' + $host5c + ',"os_user":"' + $sid5c + '","role":"cli"}}'
        $r5c = Invoke-PipeTransport $pipe5c version.query @($old5c, $request5c)
        Check '5c: the transport ends on its own after a refused hello' (-not $r5c.Hung) "still running after 20 s; stdout: $($r5c.Out -join ' | ') stderr: $($r5c.Err)"
        $refused5c = $r5c.Out
        $refusedExit5c = $r5c.Exit
        Check '5c: a refused hello prints exactly its own reply' ($refused5c.Count -eq 1) "got $($refused5c.Count) lines: $($refused5c -join ' | ')"
        if ($refused5c.Count -eq 1) {
            $reply5c = $refused5c[0] | ConvertFrom-Json
            Check '5c: the reply is the hello refusal, with the daemon''s code' (($reply5c.op -eq 'hello') -and ($reply5c.payload.code -eq 'protocol_mismatch')) "reply was: $($refused5c[0])"
        }
        Check '5c: a refused hello exits 1' ($refusedExit5c -eq 1) "got $refusedExit5c"
        $r5c = Invoke-PipeTransport $pipe5c version.query @($new5c, $request5c)
        $served5c = $r5c.Out
        $servedExit5c = $r5c.Exit
        Check '5c: an accepted hello gets the request answered' ((-not $r5c.Hung) -and ($served5c.Count -eq 1) -and (($served5c[0] | ConvertFrom-Json).op -eq 'version.query')) "hung: $($r5c.Hung) stdout: $($served5c -join ' | ') stderr: $($r5c.Err)"
        Check '5c: an accepted hello exits 0' ($servedExit5c -eq 0) "got $servedExit5c"
        . (Join-Path $PSScriptRoot 'test-local-daemon-pipe.ps1')
        } finally {
            $stop5c = & $script -Stop -Prefix $p3 -PipeName $pipe5c 6>&1 2>&1
            Get-DaemonProcs (Get-PipePath $pipe5c) | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
        }

        } catch { Check '5c: section ran' $false $_.Exception.Message }
        try {
        Write-Host "`n=== 5d. a named pipe's inbound buffer is a limit charged while data waits in it, never memory set aside per instance (P18) ===" -ForegroundColor Cyan
        # Both ends live in this process, so whichever end Windows charges shows in this process's nonpaged pool. The
        # control first: 1 MiB written into a 2 MiB inbound buffer that nobody reads must complete and must show in the
        # counter, or the counter cannot see pipe buffers and the section fails instead of passing. Then sixteen more
        # connected, idle instances must grow it by less than one buffer: a reservation would add 32 MiB.
        $name5d = New-TestPipeName
        $self5d = Get-Process -Id $PID
        $ends5d = @()
        $held5d = 0
        try {
            $self5d.Refresh()
            $base5d = $self5d.NonpagedSystemMemorySize64
            for ($i5d = 0; $i5d -lt 17; $i5d++) {
                $server5d = New-Object System.IO.Pipes.NamedPipeServerStream($name5d, [System.IO.Pipes.PipeDirection]::InOut, 17, [System.IO.Pipes.PipeTransmissionMode]::Byte, [System.IO.Pipes.PipeOptions]::Asynchronous, 2097152, 512)
                $ends5d += $server5d
                $accept5d = $server5d.WaitForConnectionAsync()
                $client5d = New-Object System.IO.Pipes.NamedPipeClientStream('.', $name5d, [System.IO.Pipes.PipeDirection]::InOut, [System.IO.Pipes.PipeOptions]::Asynchronous)
                $ends5d += $client5d
                $client5d.Connect(3000)
                $null = $accept5d.Wait(3000)
                if ($i5d -eq 0) {
                    $mib5d = New-Object byte[] 1048576
                    $wrote5d = $client5d.WriteAsync($mib5d, 0, $mib5d.Length).Wait(5000)
                    $self5d.Refresh()
                    $held5d = $self5d.NonpagedSystemMemorySize64 - $base5d
                    Check '5d: 1 MiB written into a 2 MiB inbound buffer that nobody reads completes' $wrote5d 'the write blocked: the buffer does not hold it'
                    Check '5d: the waiting 1 MiB shows in this process''s nonpaged pool' ($held5d -ge 524288) "it grew $held5d bytes: this counter cannot see pipe buffers"
                }
            }
            $self5d.Refresh()
            $idle5d = $self5d.NonpagedSystemMemorySize64 - $base5d - $held5d
            Check '5d: sixteen more idle instances set no buffer aside' ($idle5d -lt 1048576) "they grew it $idle5d bytes"
        } finally {
            foreach ($e in $ends5d) { try { $e.Dispose() } catch { } }
        }
        } catch { Check '5d: section ran' $false $_.Exception.Message }
        try {
        Write-Host "`n=== 6. pipe name comes from 'sotd session-socket-path local', not a hardcoded guess ===" -ForegroundColor Cyan
        # ADR 0042 L2b design C: no -PipeName override here -- the script
        # must resolve $daemonExe itself and query IT for the pipe path,
        # exactly the path every real launch takes. That path is the per-user
        # pipe `\\.\pipe\sot-<USERNAME>-local`, this box's own daemon's, and
        # only USERNAME moves it, so the section runs under a USERNAME of its
        # own, named as this suite names its pipes (New-TestPipeName), by which
        # the outer cleanup also finds its daemon. Nothing starts unless the
        # derived pipe differs from the one this box's own daemon derives and
        # no sotd.exe serves it yet. (Also the only section that can exercise
        # the -Stop/complete-pair split below: that needs the daemon actually
        # listening on the SAME pipe -Stop will derive, which
        # -PipeName-isolated sections deliberately avoid.)
        $livePipe = (& $realSotd session-socket-path local | Select-Object -First 1)
        if ($livePipe) { $livePipe = $livePipe.ToString().Trim() }
        $savedUser6 = $env:USERNAME
        $env:USERNAME = New-TestPipeName
        try {
            $expectedPipe = (& $realSotd session-socket-path local | Select-Object -First 1)
            if ($expectedPipe) { $expectedPipe = $expectedPipe.ToString().Trim() }
            Check 'sotd itself derives a Windows named-pipe path' ($expectedPipe -like '\\.\pipe\sot-*-local') "got: $expectedPipe"
            $served6 = @(Get-DaemonProcs $expectedPipe).Count
            $own6 = ($expectedPipe -like '\\.\pipe\sot-*-local') -and ($livePipe -like '\\.\pipe\sot-*-local') -and ($expectedPipe -ne $livePipe) -and ($served6 -eq 0)
            Check "6: the derived pipe is this section's own" $own6 "derived '$expectedPipe', this box's own daemon's '$livePipe', sotd.exe already on it: $served6"
            if (-not $own6) { throw "6: the derived pipe is not this section's own; nothing was started" }
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
                Write-Host ("  6: own daemon pid {0} on {1}" -f (($procs6 | ForEach-Object { $_.ProcessId }) -join ','), $expectedPipe)

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
        } finally {
            $env:USERNAME = $savedUser6
        }
        } catch { Check '6: section ran' $false $_.Exception.Message }
    }

    . (Join-Path $PSScriptRoot 'test-local-daemon-fake.ps1')
    . (Join-Path $PSScriptRoot 'test-local-daemon-binary.ps1')

    if ($compiled) {
        try {
        Write-Host "`n=== 7. EnsureKeepsWaitingDaemon: a late bind is waited for, never killed ===" -ForegroundColor Cyan
        Clear-FakeEnv
        $env:FAKE_SOTD_BIND_DELAY_MS = '7000'
        $p7 = New-FakePrefix 'p7'
        $pipe7 = New-TestPipeName
        $sw7 = [System.Diagnostics.Stopwatch]::StartNew()
        try {
            $out7 = & $script -Prefix $p7 -DevBinDir 'C:\sot-test-does-not-exist' -PipeName $pipe7 -ProjectRoot $root 6>&1 2>&1
            $exit7 = $LASTEXITCODE
            $sec7 = $sw7.Elapsed.TotalSeconds
            Check '7: exit code 0 after the late bind' ($exit7 -eq 0) "got $exit7; log: $out7"
            Check '7: waited at least 6s' ($sec7 -ge 6) "took $sec7 s"
            Check '7: logged the waiting line' ((($out7 -join ' ')) -match 'waiting for the backend') "log was: $out7"
            Check '7: never stopped the spawn' (-not ((($out7 -join ' ')) -match 'stopping pid')) "log was: $out7"
            Check '7: the fake is alive after' (@(Get-DaemonProcs (Get-PipePath $pipe7)).Count -eq 1) 'the fake daemon is gone'
        } finally {
            Stop-FakeOn $pipe7
            Clear-FakeEnv
        }

        } catch { Check '7: section ran' $false $_.Exception.Message }
        try {
        Write-Host "`n=== 8. StopWaitsForSelfShutdown: -Stop waits while a shutdown is under way ===" -ForegroundColor Cyan
        $heldPath = Join-Path $fakeLocalAppData 'sot\held.json'
        Remove-Item -LiteralPath $heldPath -Force -ErrorAction SilentlyContinue

        # (a) -FrontendKilled, and the fake exits by itself 6 s after the test starts measuring. Its bind comes 4 s
        # late: setup time that, with a timer started at the fake's own start, shortened the measured wait.
        try {
        Clear-FakeEnv
        $arm8a = Join-Path $root 'exit-arm-8a'
        $env:FAKE_SOTD_EXIT_ARM_FILE = $arm8a
        $env:FAKE_SOTD_BIND_DELAY_MS = '4000'
        $p8a = New-FakePrefix 'p8a'
        $pipe8a = New-TestPipeName
        try {
            $null = & $script -Prefix $p8a -DevBinDir 'C:\sot-test-does-not-exist' -PipeName $pipe8a -ProjectRoot $root 6>&1 2>&1
            Check '8a: the fake started' (@(Get-DaemonProcs (Get-PipePath $pipe8a)).Count -eq 1) 'the fake daemon did not start'
            # The origin: the stopwatch starts first, then the fake's exit timer, once this file appears.
            $sw8a = [System.Diagnostics.Stopwatch]::StartNew()
            Set-Content -LiteralPath $arm8a -Value '6000' -Encoding ASCII
            $out8a = & $script -Stop -FrontendKilled -Prefix $p8a -PipeName $pipe8a 6>&1 2>&1
            $exit8a = $LASTEXITCODE
            $sec8a = $sw8a.Elapsed.TotalSeconds
            Check '8a: exit code 0' ($exit8a -eq 0) "got $exit8a; log: $out8a"
            Check '8a: waited between 3s and 30s' (($sec8a -ge 3) -and ($sec8a -le 30)) "took $sec8a s"
            Check '8a: said the daemon exited by itself' ((($out8a -join ' ')) -match 'exited by itself') "log was: $out8a"
            Check '8a: did not kill' (-not ((($out8a -join ' ')) -match 'killing pid')) "log was: $out8a"
        } finally {
            Stop-FakeOn $pipe8a
            Clear-FakeEnv
        }
        } catch { Check '8a: section ran' $false $_.Exception.Message }

        # (b) no switch, but held.json says the daemon is closing.
        try {
        Clear-FakeEnv
        $arm8b = Join-Path $root 'exit-arm-8b'
        $env:FAKE_SOTD_EXIT_ARM_FILE = $arm8b
        Set-Content -LiteralPath $heldPath -Value '{"v":1,"holders":[],"handover_until_ms":null,"closing":true,"not_ended":0,"forget":[]}' -Encoding ASCII
        $p8b = New-FakePrefix 'p8b'
        $pipe8b = New-TestPipeName
        try {
            $null = & $script -Prefix $p8b -DevBinDir 'C:\sot-test-does-not-exist' -PipeName $pipe8b -ProjectRoot $root 6>&1 2>&1
            $sw8b = [System.Diagnostics.Stopwatch]::StartNew()
            Set-Content -LiteralPath $arm8b -Value '6000' -Encoding ASCII
            $out8b = & $script -Stop -Prefix $p8b -PipeName $pipe8b 6>&1 2>&1
            $exit8b = $LASTEXITCODE
            $sec8b = $sw8b.Elapsed.TotalSeconds
            Check '8b: exit code 0' ($exit8b -eq 0) "got $exit8b; log: $out8b"
            Check '8b: waited between 3s and 30s' (($sec8b -ge 3) -and ($sec8b -le 30)) "took $sec8b s"
            Check '8b: said the daemon exited by itself' ((($out8b -join ' ')) -match 'exited by itself') "log was: $out8b"
            Check '8b: did not kill' (-not ((($out8b -join ' ')) -match 'killing pid')) "log was: $out8b"
        } finally {
            Stop-FakeOn $pipe8b
            Clear-FakeEnv
            Remove-Item -LiteralPath $heldPath -Force -ErrorAction SilentlyContinue
        }
        } catch { Check '8b: section ran' $false $_.Exception.Message }

        # (c) control: no knob, no switch, no record -- killed at once.
        try {
        Clear-FakeEnv
        $p8c = New-FakePrefix 'p8c'
        $pipe8c = New-TestPipeName
        try {
            $null = & $script -Prefix $p8c -DevBinDir 'C:\sot-test-does-not-exist' -PipeName $pipe8c -ProjectRoot $root 6>&1 2>&1
            $sw8c = [System.Diagnostics.Stopwatch]::StartNew()
            $out8c = & $script -Stop -Prefix $p8c -PipeName $pipe8c 6>&1 2>&1
            $sec8c = $sw8c.Elapsed.TotalSeconds
            Check '8c: killed the daemon' ((($out8c -join ' ')) -match 'killing pid') "log was: $out8c"
            Check '8c: killed at once (under 5s)' ($sec8c -lt 5) "took $sec8c s"
        } finally {
            Stop-FakeOn $pipe8c
            Clear-FakeEnv
        }
        } catch { Check '8c: section ran' $false $_.Exception.Message }
        } catch { Check '8: section ran' $false $_.Exception.Message }
        try {
        Write-Host "`n=== 8d. the pipe transport against a daemon that refuses the hello and goes on serving, as an older one does: the protocol refusal is read past, any other ends it (ADR 0049) ===" -ForegroundColor Cyan
        # The fake answers the hello with FAKE_SOTD_HELLO_REFUSAL as its code and then still answers fe.lease. A refusal
        # for the protocol is followed by the request's own reply, which decides (exit 0); any other code ends the script
        # at once (exit 1), the lease reply that would follow never read.
        $hello8d = '{"v":3,"id":0,"kind":"req","op":"hello","payload":{"client_id":"t-cli","protocol":3,"app_version":"t","host":"t-host","os_user":"t-account","role":"cli"}}'
        $lease8d = '{"v":3,"id":1,"kind":"req","op":"fe.lease","payload":{}}'
        foreach ($code8d in @('protocol_mismatch', 'os_user_conflict')) {
            Clear-FakeEnv
            $env:FAKE_SOTD_HELLO_REFUSAL = $code8d
            $fakeLog8d = Join-Path $root "fake8d-$code8d.log"
            $env:FAKE_SOTD_LOG = $fakeLog8d
            $pipe8d = New-TestPipeName
            $fake8d = Start-Process -FilePath $fakeExe -ArgumentList @('--socket', (Get-PipePath $pipe8d)) -WindowStyle Hidden -PassThru
            try {
                Check "8d ($code8d): the fake pipe is up" (Wait-Pipe $pipe8d) 'pipe never answered'
                $r8d = Invoke-PipeTransport $pipe8d fe.lease @($hello8d, $lease8d)
                $fakeSaw8d = (@(Get-Content -LiteralPath $fakeLog8d -ErrorAction SilentlyContinue) -join ' | ')
                Check "8d ($code8d): the transport ends on its own" (-not $r8d.Hung) "still running after 20 s; stdout: $($r8d.Out -join ' | ') stderr: $($r8d.Err) fake saw: $fakeSaw8d"
                $first8d = $null
                if ($r8d.Out.Count -ge 1) { $first8d = $r8d.Out[0] | ConvertFrom-Json }
                Check "8d ($code8d): the refused hello's reply is printed first" (($null -ne $first8d) -and ($first8d.op -eq 'hello') -and ($first8d.payload.code -eq $code8d)) "stdout: $($r8d.Out -join ' | ')"
                if ($code8d -eq 'protocol_mismatch') {
                    $second8d = $null
                    if ($r8d.Out.Count -eq 2) { $second8d = $r8d.Out[1] | ConvertFrom-Json }
                    Check "8d ($code8d): the request's own reply follows, and decides" (($null -ne $second8d) -and ($second8d.op -eq 'fe.lease')) "stdout: $($r8d.Out -join ' | ')"
                    Check "8d ($code8d): exit 0" ($r8d.Exit -eq 0) "got $($r8d.Exit)"
                } else {
                    Check "8d ($code8d): nothing else is printed" ($r8d.Out.Count -eq 1) "stdout: $($r8d.Out -join ' | ')"
                    Check "8d ($code8d): exit 1" ($r8d.Exit -eq 1) "got $($r8d.Exit)"
                }
            } finally {
                if ($fake8d -and -not $fake8d.HasExited) { Stop-Process -Id $fake8d.Id -Force -ErrorAction SilentlyContinue }
                Clear-FakeEnv
            }
        }
        } catch { Check '8d: section ran' $false $_.Exception.Message }
    }

try {
    if ($compiled) {
        Write-Host "`n=== 12. SuccessorKeepsOldLogs: a spawn over a held stdout log does not lose its content ===" -ForegroundColor Cyan
        # Diagnostic: the ensure can run while the previous daemon still holds
        # its stdout log open. Hold it here with the share mode a child would
        # (no FileShare.Delete), put a known line in it, spawn through the real
        # path, and look for the line in every stdout log.
        Clear-FakeEnv
        $p12 = New-FakePrefix 'p12'
        $pipe12 = New-TestPipeName
        $log12dir = Join-Path $p12 'logs'
        New-Item -ItemType Directory -Force -Path $log12dir | Out-Null
        $out12 = Join-Path $log12dir 'sotd-local.stdout.log'
        $held12 = $null
        try {
            $held12 = New-Object System.IO.FileStream($out12, [System.IO.FileMode]::Create, [System.IO.FileAccess]::Write, [System.IO.FileShare]::ReadWrite)
            $bytes12 = [System.Text.Encoding]::ASCII.GetBytes("known-line-12`r`n")
            $held12.Write($bytes12, 0, $bytes12.Length); $held12.Flush()
            $run12 = & $script -Prefix $p12 -DevBinDir 'C:\sot-test-does-not-exist' -PipeName $pipe12 -ProjectRoot $root 6>&1 2>&1
            $exit12 = $LASTEXITCODE
            Check '12: the spawn exits 0' ($exit12 -eq 0) "got $exit12; log: $run12"
            Check '12: the successor answers' (Wait-Pipe $pipe12) 'pipe never opened'
            Check '12: exactly one sotd is on the pipe' (@(Get-DaemonProcs (Get-PipePath $pipe12)).Count -eq 1) "found $(@(Get-DaemonProcs (Get-PipePath $pipe12)).Count)"
            $kept12 = $false
            foreach ($f12 in @(Get-ChildItem -LiteralPath $log12dir -Filter 'sotd-local.stdout*.log' -ErrorAction SilentlyContinue)) {
                $fs12 = New-Object System.IO.FileStream($f12.FullName, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
                try { if ((New-Object System.IO.StreamReader($fs12)).ReadToEnd() -match 'known-line-12') { $kept12 = $true } } finally { $fs12.Dispose() }
            }
            Check '12: the held log line survives the successor spawn' $kept12 'known line found in no sotd-local.stdout*.log file'
            $held12text = ''
            if (Test-Path -LiteralPath $out12) {
                $fs12 = New-Object System.IO.FileStream($out12, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
                try { $held12text = (New-Object System.IO.StreamReader($fs12)).ReadToEnd() } finally { $fs12.Dispose() }
            }
            Check '12: the held file is still there and still holds the line' ($held12text -match 'known-line-12') "held file text: '$held12text'"
        } finally {
            if ($held12) { $held12.Dispose() }
            Stop-FakeOn $pipe12
            Clear-FakeEnv
        }
    }
} catch { Check '12: section ran' $false $_.Exception.Message }
try {
    if ($compiled) {
        Write-Host "`n=== 13. LogsStayBounded: a spawn prunes old logs to the cap and never touches a held one ===" -ForegroundColor Cyan
        # The bounds are read from the script. Old stamped files of one size,
        # so the newest $LogKeep fit under the cap and all of them do not; the
        # old fixed name is held the way section 12 holds it, and an unheld
        # leftover of the old rotation is the oldest of all.
        $ld13 = Get-Content -LiteralPath $script -Raw
        $keep13 = [int]([regex]::Match($ld13, '(?m)^\$LogKeep\s*=\s*(\d+)').Groups[1].Value)
        $cap13 = [int64]([regex]::Match($ld13, '(?m)^\$LogCapBytes\s*=\s*(\d+)MB').Groups[1].Value) * 1MB
        Check '13: the bounds are named in the script' (($keep13 -gt 0) -and ($cap13 -gt 0)) "keep=$keep13 cap=$cap13"
        Clear-FakeEnv
        $p13 = New-FakePrefix 'p13'
        $pipe13 = New-TestPipeName
        $log13dir = Join-Path $p13 'logs'
        New-Item -ItemType Directory -Force -Path $log13dir | Out-Null
        $size13 = [int64][math]::Floor($cap13 / ($keep13 + 1))
        $old13 = @()
        for ($i13 = 0; $i13 -lt $keep13 + 3; $i13++) {
            $n13 = Join-Path $log13dir ('sotd-local.stdout.20200101-0000{0:d2}-000Z-{1}.log' -f $i13, (1000 + $i13))
            $fs13 = [System.IO.File]::Create($n13)
            try { $fs13.SetLength($size13) } finally { $fs13.Dispose() }
            $old13 += $n13
        }
        $tmp13 = Join-Path $log13dir 'sotd-local.stdout.log.rotating.4242.tmp'
        [System.IO.File]::WriteAllText($tmp13, "leftover-13`r`n")
        $out13 = Join-Path $log13dir 'sotd-local.stdout.log'
        $held13 = $null
        try {
            $held13 = New-Object System.IO.FileStream($out13, [System.IO.FileMode]::Create, [System.IO.FileAccess]::Write, [System.IO.FileShare]::ReadWrite)
            $bytes13 = [System.Text.Encoding]::ASCII.GetBytes("known-line-13`r`n")
            $held13.Write($bytes13, 0, $bytes13.Length); $held13.Flush()
            $run13 = & $script -Prefix $p13 -DevBinDir 'C:\sot-test-does-not-exist' -PipeName $pipe13 -ProjectRoot $root 6>&1 2>&1
            $exit13 = $LASTEXITCODE
            Check '13: the spawn exits 0' ($exit13 -eq 0) "got $exit13; log: $run13"
            Check '13: the successor answers' (Wait-Pipe $pipe13) 'pipe never opened'
            Check '13: exactly one sotd is on the pipe' (@(Get-DaemonProcs (Get-PipePath $pipe13)).Count -eq 1) "found $(@(Get-DaemonProcs (Get-PipePath $pipe13)).Count)"
            $kept13 = @($old13 | Select-Object -Last $keep13 | Where-Object { (Test-Path -LiteralPath $_) -and ((Get-Item -LiteralPath $_).Length -eq $size13) })
            Check "13: the newest $keep13 files are kept" ($kept13.Count -eq $keep13) "kept $($kept13.Count) of $keep13"
            $gone13 = @(@($tmp13) + $old13 | Where-Object { -not (Test-Path -LiteralPath $_) })
            $want13 = @(@($tmp13) + @($old13 | Select-Object -First 3))
            Check '13: the deleted files are exactly the oldest, in order' (($gone13 -join ',') -ceq ($want13 -join ',')) "deleted: $($gone13 -join ', ')"
            Check '13: the held log is kept with one line naming it' (($run13 | Out-String -Width 4096) -match ('kept log [^\r\n]*[\\/]' + [regex]::Escape((Split-Path -Leaf $out13)) + ': \S')) "log: $run13"
            $held13text = ''
            if (Test-Path -LiteralPath $out13) {
                $fs13 = New-Object System.IO.FileStream($out13, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
                try { $held13text = (New-Object System.IO.StreamReader($fs13)).ReadToEnd() } finally { $fs13.Dispose() }
            }
            Check '13: the held file is kept unchanged' ($held13text -ceq "known-line-13`r`n") "held file text: '$held13text'"
            # A line written through the held handle now reaches the same name,
            # so no delete took the file out from under its holder.
            $after13 = [System.Text.Encoding]::ASCII.GetBytes("after-13`r`n")
            $held13.Write($after13, 0, $after13.Length); $held13.Flush()
            $held13text = ''
            if (Test-Path -LiteralPath $out13) {
                $fs13 = New-Object System.IO.FileStream($out13, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
                try { $held13text = (New-Object System.IO.StreamReader($fs13)).ReadToEnd() } finally { $fs13.Dispose() }
            }
            Check '13: no delete touched the held file' ($held13text -ceq "known-line-13`r`nafter-13`r`n") "held file text: '$held13text'"
            $unheld13 = [int64]0
            foreach ($f13 in @(Get-ChildItem -LiteralPath $log13dir -Filter 'sotd-local.stdout*.log' -ErrorAction SilentlyContinue)) {
                if ($f13.Name -ne 'sotd-local.stdout.log') { $unheld13 += $f13.Length }
            }
            Check '13: the unheld total is at most the cap' ($unheld13 -le $cap13) "total $unheld13, cap $cap13"
        } finally {
            if ($held13) { $held13.Dispose() }
            Stop-FakeOn $pipe13
            Clear-FakeEnv
        }
    }
} catch { Check '13: section ran' $false $_.Exception.Message }
try {
    if ($compiled) {
        Write-Host "`n=== 13b. LogsStayBoundedByCap: within the file count, the cap still prunes, oldest first, and keeps the newest ===" -ForegroundColor Cyan
        # $LogKeep files of half the cap each: the count is within bounds and
        # the unprotected total is over the cap, so only the cap prunes.
        $ld13b = Get-Content -LiteralPath $script -Raw
        $keep13b = [int]([regex]::Match($ld13b, '(?m)^\$LogKeep\s*=\s*(\d+)').Groups[1].Value)
        $cap13b = [int64]([regex]::Match($ld13b, '(?m)^\$LogCapBytes\s*=\s*(\d+)MB').Groups[1].Value) * 1MB
        Clear-FakeEnv
        $p13b = New-FakePrefix 'p13b'
        $pipe13b = New-TestPipeName
        $log13bdir = Join-Path $p13b 'logs'
        New-Item -ItemType Directory -Force -Path $log13bdir | Out-Null
        $size13b = [int64][math]::Floor($cap13b / 2)
        $old13b = @()
        for ($i13b = 0; $i13b -lt $keep13b; $i13b++) {
            $n13b = Join-Path $log13bdir ('sotd-local.stdout.20200101-0000{0:d2}-000Z-{1}.log' -f $i13b, (1000 + $i13b))
            $fs13b = [System.IO.File]::Create($n13b)
            try { $fs13b.SetLength($size13b) } finally { $fs13b.Dispose() }
            $old13b += $n13b
        }
        try {
            $run13b = & $script -Prefix $p13b -DevBinDir 'C:\sot-test-does-not-exist' -PipeName $pipe13b -ProjectRoot $root 6>&1 2>&1
            $exit13b = $LASTEXITCODE
            Check '13b: the spawn exits 0' ($exit13b -eq 0) "got $exit13b; log: $run13b"
            Check '13b: the successor answers' (Wait-Pipe $pipe13b) 'pipe never opened'
            $gone13b = @($old13b | Where-Object { -not (Test-Path -LiteralPath $_) })
            $want13b = @($old13b | Select-Object -First ($keep13b - 3))
            Check '13b: the deleted files are exactly the oldest, in order' (($gone13b -join ',') -ceq ($want13b -join ',')) "deleted: $($gone13b -join ', ')"
            Check '13b: the newest closed log is kept' (Test-Path -LiteralPath $old13b[$old13b.Count - 1]) 'the newest old log is gone'
            $unprot13b = [int64]0
            foreach ($f13b in @($old13b | Select-Object -First ($keep13b - 1))) {
                if (Test-Path -LiteralPath $f13b) { $unprot13b += (Get-Item -LiteralPath $f13b).Length }
            }
            Check '13b: the unprotected total is at most the cap' ($unprot13b -le $cap13b) "total $unprot13b, cap $cap13b"
        } finally {
            Stop-FakeOn $pipe13b
            Clear-FakeEnv
        }
    }
} catch { Check '13b: section ran' $false $_.Exception.Message }
try {
    if ($compiled) {
        Write-Host "`n=== 14. HeldLogAllowsDelete: a held log is kept even when its holder allows delete sharing ===" -ForegroundColor Cyan
        # Fail-first for the exclusive-open delete: a holder that grants
        # FileShare.Delete lets a plain delete succeed. Stamped files in both
        # the earlier and the current name format put the stream over both
        # bounds for either version of the script, so the held old fixed name
        # is a delete candidate either way.
        $ld14 = Get-Content -LiteralPath $script -Raw
        $keep14 = [int]([regex]::Match($ld14, '(?m)^\$LogKeep\s*=\s*(\d+)').Groups[1].Value)
        $cap14 = [int64]([regex]::Match($ld14, '(?m)^\$LogCapBytes\s*=\s*(\d+)MB').Groups[1].Value) * 1MB
        Clear-FakeEnv
        $p14 = New-FakePrefix 'p14'
        $pipe14 = New-TestPipeName
        $log14dir = Join-Path $p14 'logs'
        New-Item -ItemType Directory -Force -Path $log14dir | Out-Null
        $size14 = [int64][math]::Floor($cap14 / $keep14)
        for ($i14 = 0; $i14 -le $keep14; $i14++) {
            foreach ($fmt14 in @('sotd-local.stdout.20200101-0000{0:d2}-{1}.log', 'sotd-local.stdout.20200101-0000{0:d2}-000Z-{1}.log')) {
                $fs14 = [System.IO.File]::Create((Join-Path $log14dir ($fmt14 -f $i14, (1000 + $i14))))
                try { $fs14.SetLength($size14) } finally { $fs14.Dispose() }
            }
        }
        $out14 = Join-Path $log14dir 'sotd-local.stdout.log'
        $held14 = $null
        try {
            $held14 = New-Object System.IO.FileStream($out14, [System.IO.FileMode]::Create, [System.IO.FileAccess]::Write, [System.IO.FileShare]'ReadWrite, Delete')
            $bytes14 = [System.Text.Encoding]::ASCII.GetBytes("known-line-14`r`n")
            $held14.Write($bytes14, 0, $bytes14.Length); $held14.Flush()
            $run14 = & $script -Prefix $p14 -DevBinDir 'C:\sot-test-does-not-exist' -PipeName $pipe14 -ProjectRoot $root 6>&1 2>&1
            $exit14 = $LASTEXITCODE
            Check '14: the spawn exits 0' ($exit14 -eq 0) "got $exit14; log: $run14"
            Check '14: the successor answers' (Wait-Pipe $pipe14) 'pipe never opened'
            $after14 = [System.Text.Encoding]::ASCII.GetBytes("after-14`r`n")
            $held14.Write($after14, 0, $after14.Length); $held14.Flush()
            $text14 = ''
            try {
                $fs14 = New-Object System.IO.FileStream($out14, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]'ReadWrite, Delete')
                try { $text14 = (New-Object System.IO.StreamReader($fs14)).ReadToEnd() } finally { $fs14.Dispose() }
            } catch { $text14 = "unreadable: $($_.Exception.Message)" }
            Check '14: the held file survives and holds its lines' ($text14 -ceq "known-line-14`r`nafter-14`r`n") "held file text: '$text14'"
            Check '14: one line names the kept file and its error' (($run14 | Out-String -Width 4096) -match ('kept log [^\r\n]*[\\/]' + [regex]::Escape((Split-Path -Leaf $out14)) + ': \S')) "log: $run14"
        } finally {
            if ($held14) { $held14.Dispose() }
            Stop-FakeOn $pipe14
            Clear-FakeEnv
        }
    }
} catch { Check '14: section ran' $false $_.Exception.Message }
try {
    Write-Host "`n=== 15. StopWaitMs: how long -Stop waits, from held.json ===" -ForegroundColor Cyan
    # A pin, not fail-first: Get-StopWaitMs is new. Any deadline waits until
    # it plus DAEMON_LOCK_WAIT; closing, or a record that exists but cannot be
    # read or parsed, waits the bound; nothing under way waits 0.
    $ast15 = [System.Management.Automation.Language.Parser]::ParseFile($script, [ref]$null, [ref]$null)
    $fn15 = $ast15.Find({ param($n) ($n -is [System.Management.Automation.Language.FunctionDefinitionAst]) -and $n.Name -eq 'Get-StopWaitMs' }, $true)
    Check '15: Get-StopWaitMs is defined' ($null -ne $fn15) 'function not found'
    if ($fn15) {
        . ([scriptblock]::Create($fn15.Extent.Text))
        $DaemonLockWaitSeconds = [int]([regex]::Match((Get-Content -LiteralPath $script -Raw), '(?m)^\$DaemonLockWaitSeconds\s*=\s*(\d+)').Groups[1].Value)
        $now15 = [int64]1759400000000
        $rows15 = @(
            @('missing', 'missing', '', 0),
            @('unreadable', 'unreadable', '', 150000),
            @('unparsable text', 'read', 'not json', 150000),
            @('closing:true', 'read', '{"v":2,"boot":"","holders":[],"handover_until_ms":null,"hold_until_ms":null,"closing":true,"not_ended":0}', 150000),
            @('closing:false, no deadline field', 'read', '{"v":2,"boot":"","holders":[],"closing":false,"not_ended":0}', 0),
            @('a deadline 40 s ahead', 'read', ('{"v":2,"boot":"","holders":[],"hold_until_ms":' + ($now15 + 40000) + ',"closing":false,"not_ended":0}'), 190000),
            @('a deadline 5 s past', 'read', ('{"v":2,"boot":"","holders":[],"handover_until_ms":' + ($now15 - 5000) + ',"closing":false,"not_ended":0}'), 150000)
        )
        foreach ($r15 in $rows15) {
            $got15 = Get-StopWaitMs $r15[1] $r15[2] $now15
            Check "15: $($r15[0]) waits $($r15[3]) ms" ($got15 -eq $r15[3]) "got $got15"
        }
    }
} catch { Check '15: section ran' $false $_.Exception.Message }
} finally {
    # ONE place for every cleanup this file owes, so a terminating error
    # anywhere above (not just a failed Check, which never throws) still
    # restores the environment and kills whatever got spawned.
    Complete-LocalDaemonTest
}

Write-Host "`n================ $pass passed, $fail failed ================" -ForegroundColor $(if ($fail) { 'Red' } else { 'Green' })
if ($fail) { exit 1 }

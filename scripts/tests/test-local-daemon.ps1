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
        } catch { Check '6: section ran' $false $_.Exception.Message }
    }

    # ---- 7-8: a fake daemon (C#, compiled once) that can bind late and exit by itself ----
    # Add-Type -OutputAssembly is Windows PowerShell 5.1 only, which is what
    # this file runs under (CI step `shell: powershell`). C# 5 syntax only.
    Write-Host "`n=== 7-8 setup. compile the fake daemon ===" -ForegroundColor Cyan
    if ($null -eq $envSaved) { $envSaved = @{} }
    foreach ($k in @('LOCALAPPDATA', 'FAKE_SOTD_EXIT_AFTER_MS', 'FAKE_SOTD_BIND_DELAY_MS', 'FAKE_SOTD_LEASE_OUTCOME', 'FAKE_SOTD_LOG')) {
        if (-not $envSaved.ContainsKey($k)) { $envSaved[$k] = [Environment]::GetEnvironmentVariable($k) }
    }
    $fakeLocalAppData = Join-Path $root 'fakelocal'
    New-Item -ItemType Directory -Force -Path (Join-Path $fakeLocalAppData 'sot') | Out-Null
    $env:LOCALAPPDATA = $fakeLocalAppData
    $fakeSrc = @'
using System;
using System.IO;
using System.IO.Pipes;
using System.Text;
using System.Threading;

public static class FakeSotd
{
    static string logPath;
    static object logLock = new object();
    static string outcome = "granted";

    static void Log(string s)
    {
        if (string.IsNullOrEmpty(logPath)) { return; }
        lock (logLock) { File.AppendAllText(logPath, s + "\n"); }
    }

    static int EnvInt(string name, int dflt)
    {
        string v = Environment.GetEnvironmentVariable(name);
        int n;
        if (!string.IsNullOrEmpty(v) && int.TryParse(v, out n)) { return n; }
        return dflt;
    }

    static void Serve(object o)
    {
        NamedPipeServerStream srv = (NamedPipeServerStream)o;
        bool sent = false;
        try
        {
            StreamReader r = new StreamReader(srv, new UTF8Encoding(false));
            string line;
            while ((line = r.ReadLine()) != null)
            {
                sent = true;
                Log(line);
                if (line.Contains("\"op\":\"fe.lease\""))
                {
                    byte[] b = new UTF8Encoding(false).GetBytes(
                        "{\"v\":2,\"id\":1,\"kind\":\"res\",\"op\":\"fe.lease\",\"payload\":{\"outcome\":\"" + outcome + "\"}}\n");
                    srv.Write(b, 0, b.Length);
                    srv.Flush();
                }
            }
        }
        catch (Exception) { }
        if (sent) { Log("eof"); }
        try { srv.Dispose(); } catch (Exception) { }
    }

    public static int Main(string[] a)
    {
        string name = null;
        for (int i = 0; i + 1 < a.Length; i++)
        {
            if (a[i] == "--socket") { name = a[i + 1]; }
        }
        if (name == null) { return 2; }
        const string prefix = "\\\\.\\pipe\\";
        if (name.StartsWith(prefix)) { name = name.Substring(prefix.Length); }
        logPath = Environment.GetEnvironmentVariable("FAKE_SOTD_LOG");
        string oc = Environment.GetEnvironmentVariable("FAKE_SOTD_LEASE_OUTCOME");
        if (!string.IsNullOrEmpty(oc)) { outcome = oc; }
        int exitAfter = EnvInt("FAKE_SOTD_EXIT_AFTER_MS", -1);
        int bindDelay = EnvInt("FAKE_SOTD_BIND_DELAY_MS", 0);
        if (exitAfter >= 0)
        {
            Thread t = new Thread(delegate () { Thread.Sleep(exitAfter); Environment.Exit(0); });
            t.IsBackground = true;
            t.Start();
        }
        if (bindDelay > 0) { Thread.Sleep(bindDelay); }
        while (true)
        {
            NamedPipeServerStream srv = new NamedPipeServerStream(
                name, PipeDirection.InOut, NamedPipeServerStream.MaxAllowedServerInstances, PipeTransmissionMode.Byte);
            srv.WaitForConnection();
            Thread w = new Thread(Serve);
            w.IsBackground = true;
            w.Start(srv);
        }
    }
}
'@
    $fakeBinDir = Join-Path $root 'fakebin'
    New-Item -ItemType Directory -Force -Path $fakeBinDir | Out-Null
    $fakeExe = Join-Path $fakeBinDir 'sotd.exe'
    $compiled = $false
    try {
        Add-Type -TypeDefinition $fakeSrc -OutputAssembly $fakeExe -OutputType ConsoleApplication
        $compiled = Test-Path -LiteralPath $fakeExe
    } catch { $compileErr = $_.Exception.Message }
    Check 'the fake daemon compiles' $compiled "Add-Type failed: $compileErr"

    function Clear-FakeEnv {
        foreach ($k in @('FAKE_SOTD_EXIT_AFTER_MS', 'FAKE_SOTD_BIND_DELAY_MS', 'FAKE_SOTD_LEASE_OUTCOME', 'FAKE_SOTD_LOG')) {
            Remove-Item "Env:\$k" -ErrorAction SilentlyContinue
        }
    }
    function New-FakePrefix([string]$Name) {
        $p = Join-Path $root $Name
        New-Fixture -Prefix $p -WithCapsule -SotdSource $fakeExe
        return $p
    }
    function Stop-FakeOn([string]$Pipe) {
        Get-DaemonProcs (Get-PipePath $Pipe) | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    }

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

        # (a) -FrontendKilled, and the fake exits by itself after 6 s.
        try {
        Clear-FakeEnv
        $env:FAKE_SOTD_EXIT_AFTER_MS = '6000'
        $p8a = New-FakePrefix 'p8a'
        $pipe8a = New-TestPipeName
        try {
            $null = & $script -Prefix $p8a -DevBinDir 'C:\sot-test-does-not-exist' -PipeName $pipe8a -ProjectRoot $root 6>&1 2>&1
            Check '8a: the fake started' (@(Get-DaemonProcs (Get-PipePath $pipe8a)).Count -eq 1) 'the fake daemon did not start'
            $sw8a = [System.Diagnostics.Stopwatch]::StartNew()
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
        $env:FAKE_SOTD_EXIT_AFTER_MS = '6000'
        Set-Content -LiteralPath $heldPath -Value '{"v":1,"holders":[],"handover_until_ms":null,"closing":true,"not_ended":0,"forget":[]}' -Encoding ASCII
        $p8b = New-FakePrefix 'p8b'
        $pipe8b = New-TestPipeName
        try {
            $null = & $script -Prefix $p8b -DevBinDir 'C:\sot-test-does-not-exist' -PipeName $pipe8b -ProjectRoot $root 6>&1 2>&1
            $sw8b = [System.Diagnostics.Stopwatch]::StartNew()
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
    }

    # ---- 9-11: launch-sot.ps1 order (AST) and the converge lease (C4b) ----
    $launchPath = Join-Path $repo 'scripts\launch-sot.ps1'
    $launchTokens = $null; $launchErrs = $null
    $launchAst = [System.Management.Automation.Language.Parser]::ParseFile($launchPath, [ref]$launchTokens, [ref]$launchErrs)
    function Test-HasLoopAncestor($node) {
        $p = $node.Parent
        while ($p) {
            if ($p -is [System.Management.Automation.Language.LoopStatementAst]) { return $true }
            $p = $p.Parent
        }
        return $false
    }
    function Find-Calls($root, [string]$cmdName) {
        $root.FindAll({ param($n) ($n -is [System.Management.Automation.Language.CommandAst]) -and ($n.GetCommandName() -eq $cmdName) }, $true)
    }
    function Find-Ifs($root, [string]$condText) {
        $root.FindAll({ param($n) ($n -is [System.Management.Automation.Language.IfStatementAst]) -and ($n.Clauses[0].Item1.Extent.Text -like $condText) }, $true)
    }

    try {
    Write-Host "`n=== 9. InitialEnsureBeforeChecks: the first ensure precedes the -Local and nothing-reachable checks ===" -ForegroundColor Cyan
    $ensure9 = @($launchAst.FindAll({ param($n)
        ($n -is [System.Management.Automation.Language.AssignmentStatementAst]) -and
        $n.Left.Extent.Text -eq '$localDaemonReady' -and $n.Right.Extent.Text -eq 'Invoke-LocalDaemonEnsure' }, $true) |
        Where-Object { -not (Test-HasLoopAncestor $_) })
    # Only the -Local branch that CONSUMES the ensure result: a UI step such as
    # `if ($Local) { Stop-Splash }` deliberately runs before the ensure.
    $ifLocal9 = @(Find-Ifs $launchAst '$Local' | Where-Object { ($_.Parent -eq $launchAst.EndBlock) -and ($_.Extent.Text -match '\$localDaemonReady') })
    $ifNone9 = @(Find-Ifs $launchAst '*-not $defaultRemoteOk*' | Where-Object { $_.Parent -eq $launchAst.EndBlock })
    Check '9: one top-level ensure assignment' ($ensure9.Count -eq 1) "found $($ensure9.Count)"
    Check '9: one top-level if ($Local) that reads the ensure' ($ifLocal9.Count -eq 1) "found $($ifLocal9.Count)"
    Check '9: one top-level nothing-reachable check' ($ifNone9.Count -eq 1) "found $($ifNone9.Count)"
    if ($ensure9.Count -eq 1 -and $ifLocal9.Count -eq 1 -and $ifNone9.Count -eq 1) {
        Check '9: ensure precedes if ($Local)' ($ensure9[0].Extent.StartOffset -lt $ifLocal9[0].Extent.StartOffset) 'order is wrong'
        Check '9: ensure precedes the nothing-reachable check' ($ensure9[0].Extent.StartOffset -lt $ifNone9[0].Extent.StartOffset) 'order is wrong'
    }

    } catch { Check '9: section ran' $false $_.Exception.Message }
    try {
    Write-Host "`n=== 10. RespawnEnsuresDaemon / ConvergeLeaseOrder: ensure and lease order in the supervisor loop ===" -ForegroundColor Cyan
    $loop10 = @($launchAst.FindAll({ param($n)
        ($n -is [System.Management.Automation.Language.DoWhileStatementAst]) -and $n.Condition.Extent.Text -eq '$relaunchNext' }, $true))
    Check '10: one do-while on $relaunchNext' ($loop10.Count -eq 1) "found $($loop10.Count)"
    if ($loop10.Count -eq 1) {
        $loop = $loop10[0]
        $first10 = $loop.Body.Statements[0]
        $firstIsIf = $first10 -is [System.Management.Automation.Language.IfStatementAst]
        Check '10: the first body statement is if ($relaunchNext)' ($firstIsIf -and $first10.Clauses[0].Item1.Extent.Text -eq '$relaunchNext') 'first statement is not if ($relaunchNext)'
        if ($firstIsIf) {
            $en10 = @(Find-Calls $first10 'Invoke-LocalDaemonEnsure')
            $ls10 = @(Find-Calls $first10 'Open-SotLease')
            Check '10: that block ensures the daemon' ($en10.Count -eq 1) "found $($en10.Count)"
            Check '10: that block opens a lease' ($ls10.Count -eq 1) "found $($ls10.Count)"
            if ($en10.Count -eq 1 -and $ls10.Count -eq 1) {
                Check '10: the lease follows the ensure' ($en10[0].Extent.StartOffset -lt $ls10[0].Extent.StartOffset) 'order is wrong'
            }
        }
        $allEn = @(Find-Calls $loop 'Invoke-LocalDaemonEnsure')
        Check '10: no other ensure in the loop' ($allEn.Count -eq 1) "found $($allEn.Count)"
        $ifConv = @(Find-Ifs $loop '$convergeRequested')
        $conv10 = @($ifConv | Where-Object { @(Find-Calls $_ 'Invoke-PendingApply').Count -gt 0 })
        Check '10: one if ($convergeRequested) block holds the pending apply' ($conv10.Count -eq 1) "found $($conv10.Count)"
        if ($conv10.Count -eq 1) {
            $lease10 = @(Find-Calls $conv10[0] 'Open-SotLease')
            $apply10 = @(Find-Calls $conv10[0] 'Invoke-PendingApply')
            Check '10: the converge opens its lease' ($lease10.Count -eq 1) "found $($lease10.Count)"
            if ($lease10.Count -eq 1 -and $apply10.Count -ge 1) {
                Check '10: the lease precedes Invoke-PendingApply' ($lease10[0].Extent.StartOffset -lt $apply10[0].Extent.StartOffset) 'order is wrong'
            }
        }
        $spawn10 = @($loop.FindAll({ param($n)
            ($n -is [System.Management.Automation.Language.CommandAst]) -and $n.GetCommandName() -eq 'Start-Process' -and
            $n.Extent.Text -match '-FilePath\s+\$stagedExe' }, $true))
        $close10 = @(Find-Calls $loop 'Close-SotLeases')
        Check '10: one frontend spawn and one Close-SotLeases call' (($spawn10.Count -eq 1) -and ($close10.Count -eq 1)) "spawns $($spawn10.Count), closes $($close10.Count)"
        if ($spawn10.Count -eq 1 -and $close10.Count -eq 1) {
            Check '10: Close-SotLeases follows the frontend spawn' ($close10[0].Extent.StartOffset -gt $spawn10[0].Extent.StartOffset) 'order is wrong'
        }
    }

    } catch { Check '10: section ran' $false $_.Exception.Message }
    if ($compiled) {
        try {
        Write-Host "`n=== 11. ConvergeLeaseHandover: the lease line, the handover line, the refusal warning ===" -ForegroundColor Cyan
        foreach ($fname in @('Get-SotBootId', 'Open-SotLease', 'Close-SotLeases')) {
            $fn = $launchAst.Find({ param($n) ($n -is [System.Management.Automation.Language.FunctionDefinitionAst]) -and $n.Name -eq $fname }, $true)
            Check "11: $fname is defined" ($null -ne $fn) 'function not found'
            if ($fn) { . ([scriptblock]::Create($fn.Extent.Text)) }
        }
        $LeaseReplyWaitMs = 5000
        $HandoverBoundSeconds = 60
        $script:convergeLeases = @()
        $script:supLines = @()
        function Write-SupLog { param([string]$Message) $script:supLines += $Message }
        $haveFns = (Get-Command Open-SotLease -ErrorAction SilentlyContinue) -and (Get-Command Close-SotLeases -ErrorAction SilentlyContinue) -and (Get-Command Get-SotBootId -ErrorAction SilentlyContinue)

        function Start-LeaseFake([string]$Pipe, [string]$LogPath) {
            Clear-FakeEnv
            $env:FAKE_SOTD_LOG = $LogPath
            $proc = Start-Process -FilePath $fakeExe -ArgumentList @('--socket', (Get-PipePath $Pipe)) -WindowStyle Hidden -PassThru
            return $proc
        }

        if ($haveFns) {
            # (a) granted
            $pipe11a = New-TestPipeName
            $log11a = Join-Path $root 'lease11a.log'
            $fake11a = Start-LeaseFake $pipe11a $log11a
            try {
                Check '11a: the fake pipe is up' (Wait-Pipe $pipe11a) 'pipe never answered'
                $script:supLines = @()
                $streams = @(Open-SotLease (Get-PipePath $pipe11a))
                Check '11a: one stream comes back' ($streams.Count -eq 1) "got $($streams.Count)"
                $script:convergeLeases = $streams
                Check '11a: logged the grant' (($script:supLines -join ' ') -match 'lease granted') "log: $($script:supLines -join ' | ')"
                $regOut = (& reg query 'HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Memory Management\PrefetchParameters' /v BootId) -join ' '
                $bootDec = ''
                if ($regOut -match 'BootId\s+REG_DWORD\s+0x([0-9a-fA-F]+)') { $bootDec = [string][Convert]::ToUInt32($Matches[1], 16) }
                $created11 = [System.Diagnostics.Process]::GetCurrentProcess().StartTime.ToFileTimeUtc()
                $golden = '{"v":2,"id":1,"kind":"req","op":"fe.lease","payload":{"boot":"' + $bootDec + '","created":' + $created11 + ',"pid":' + $PID + '}}'
                Start-Sleep -Milliseconds 300
                $lines11a = @(Get-Content -LiteralPath $log11a -ErrorAction SilentlyContinue)
                Check '11a: the first logged line is the golden lease line' (($lines11a.Count -ge 1) -and ($lines11a[0] -ceq $golden)) "got: $($lines11a[0]) want: $golden"
                Close-SotLeases
                Start-Sleep -Milliseconds 500
                $lines11a = @(Get-Content -LiteralPath $log11a -ErrorAction SilentlyContinue)
                $handover = '{"v":2,"id":2,"kind":"req","op":"fe.leaving","payload":{"intent":"handover"}}'
                Check '11a: the handover line follows' (($lines11a.Count -ge 2) -and ($lines11a[1] -ceq $handover)) "lines: $($lines11a -join ' | ')"
                Check '11a: then eof' (($lines11a.Count -ge 3) -and ($lines11a[2] -ceq 'eof')) "lines: $($lines11a -join ' | ')"
                Check '11a: the lease list is empty after Close-SotLeases' ($script:convergeLeases.Count -eq 0) "count $($script:convergeLeases.Count)"
            } finally {
                if ($fake11a -and -not $fake11a.HasExited) { Stop-Process -Id $fake11a.Id -Force -ErrorAction SilentlyContinue }
                Clear-FakeEnv
            }

            # (b) refused
            $pipe11b = New-TestPipeName
            $log11b = Join-Path $root 'lease11b.log'
            Clear-FakeEnv
            $env:FAKE_SOTD_LEASE_OUTCOME = 'foreign'
            $env:FAKE_SOTD_LOG = $log11b
            $fake11b = Start-Process -FilePath $fakeExe -ArgumentList @('--socket', (Get-PipePath $pipe11b)) -WindowStyle Hidden -PassThru
            try {
                Check '11b: the fake pipe is up' (Wait-Pipe $pipe11b) 'pipe never answered'
                $script:supLines = @()
                $streams11b = @(Open-SotLease (Get-PipePath $pipe11b))
                Check '11b: no stream comes back' ($streams11b.Count -eq 0) "got $($streams11b.Count)"
                Check '11b: the warning names the 60 s bound' ((@($script:supLines | Where-Object { $_ -like '*60 s*' })).Count -ge 1) "log: $($script:supLines -join ' | ')"
            } finally {
                if ($fake11b -and -not $fake11b.HasExited) { Stop-Process -Id $fake11b.Id -Force -ErrorAction SilentlyContinue }
                Clear-FakeEnv
            }

            # (c) the boot identity is stable and numeric
            $b1 = Get-SotBootId; $b2 = Get-SotBootId
            Check '11c: Get-SotBootId is stable' ($b1 -ceq $b2) "got '$b1' then '$b2'"
            Check '11c: Get-SotBootId is a decimal number' ($b1 -match '^\d+$') "got '$b1'"
        }
        } catch { Check '11: section ran' $false $_.Exception.Message }
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
        # oldest file, at the old fixed name, is held the way section 12 holds it.
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
            $n13 = Join-Path $log13dir ('sotd-local.stdout.20200101-0000{0:d2}-{1}.log' -f $i13, (1000 + $i13))
            $fs13 = [System.IO.File]::Create($n13)
            try { $fs13.SetLength($size13) } finally { $fs13.Dispose() }
            $old13 += $n13
        }
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

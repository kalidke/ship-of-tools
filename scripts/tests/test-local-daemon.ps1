# test-local-daemon.ps1 -- regression harness for scripts/sot-local-daemon.ps1
# (ADR 0042 L1c: the Windows launcher starts a local sotd, idempotently and
# detached, and shutdown-sot.ps1 stops it last).
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
# Sections 3-5 need a REAL, runnable sotd.exe (it must actually bind a named
# pipe) -- sourced from rust\target\debug\sotd.exe, which the SAME CI job
# already builds one step earlier (`cargo build --workspace --locked`, no
# --release => target\debug; see .github/workflows/rust.yml). Falls back to
# a release build for a local dev run; SKIPs (not FAILs) those three
# sections when neither exists, so this test stays runnable on a leg/box
# that hasn't built anything.
#
# Run under WINDOWS POWERSHELL 5.1 specifically -- same reason as
# test-sot-apply.ps1 (the .lnk launcher's host; 5.1 decodes a BOM-less .ps1
# as cp1252 where pwsh 7 decodes it as UTF-8).
#
# ASCII ONLY (see the same note in sot-local-daemon.ps1 / launch-sot.ps1).
#
# Every wait below is BOUNDED (Wait-Pipe/Wait-PipeGone poll with a timeout,
# never an unbounded loop) and every spawned test process is torn down
# before this script exits, success or failure.
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

function Test-PipeListed([string]$Name) {
    # try/catch even though $ErrorActionPreference is 'Stop' at file scope --
    # a transient enumeration failure here must fail one Check, not crash the
    # whole suite (mirrors sot-local-daemon.ps1's own Test-SotPipeOpen).
    try {
        $path = '\\.\pipe\' + $Name
        return ([System.IO.Directory]::GetFiles('\\.\pipe\')) -contains $path
    } catch {
        return $false
    }
}
function Wait-Pipe([string]$Name, [int]$TimeoutMs = 5000) {
    $elapsed = 0
    while ($elapsed -lt $TimeoutMs) {
        if (Test-PipeListed $Name) { return $true }
        Start-Sleep -Milliseconds 200
        $elapsed += 200
    }
    return $false
}
function Wait-PipeGone([string]$Name, [int]$TimeoutMs = 5000) {
    $elapsed = 0
    while ($elapsed -lt $TimeoutMs) {
        if (-not (Test-PipeListed $Name)) { return $true }
        Start-Sleep -Milliseconds 200
        $elapsed += 200
    }
    return $false
}
function Get-DaemonProcs([string]$PipeName) {
    @(Get-CimInstance Win32_Process -Filter "Name='sotd.exe'" |
        Where-Object { $_.CommandLine -and $_.CommandLine.Contains($PipeName) })
}

$realSotd = Join-Path $repo 'rust\target\debug\sotd.exe'
if (-not (Test-Path $realSotd)) { $realSotd = Join-Path $repo 'rust\target\release\sotd.exe' }
$haveRealSotd = Test-Path $realSotd

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

if (-not $haveRealSotd) {
    Note-Skip '3. start when absent' 'no rust\target\{debug,release}\sotd.exe built on this leg'
    Note-Skip '4. no second start when the pipe already answers' 'no rust\target\{debug,release}\sotd.exe built on this leg'
    Note-Skip '5. shutdown stops the daemon and leaves a fake supervisor alone' 'no rust\target\{debug,release}\sotd.exe built on this leg'
} else {
    Write-Host "`n=== 3. start when absent ===" -ForegroundColor Cyan
    $p3 = Join-Path $root 'p3'
    New-Fixture -Prefix $p3 -WithCapsule -SotdSource $realSotd
    $pipe3 = New-TestPipeName
    $out3 = & $script -Prefix $p3 -DevBinDir 'C:\sot-test-does-not-exist' -PipeName $pipe3 -ProjectRoot $root 6>&1 2>&1
    $exit3 = $LASTEXITCODE
    Check 'exit code 0' ($exit3 -eq 0) "got $exit3; log: $out3"
    Check 'pipe answers' (Wait-Pipe $pipe3) 'pipe never opened'
    $procs3 = Get-DaemonProcs $pipe3
    Check 'exactly one sotd.exe on this pipe' ($procs3.Count -eq 1) "found $($procs3.Count)"

    Write-Host "`n=== 4. no second start when the pipe already answers ===" -ForegroundColor Cyan
    $out4 = & $script -Prefix $p3 -DevBinDir 'C:\sot-test-does-not-exist' -PipeName $pipe3 -ProjectRoot $root 6>&1 2>&1
    $exit4 = $LASTEXITCODE
    Check 'exit code 0 (already running is success)' ($exit4 -eq 0) "got $exit4"
    Check 'said already running' ((($out4 -join ' ')) -match 'already running') "log was: $out4"
    $procs4 = Get-DaemonProcs $pipe3
    Check 'still exactly one sotd.exe (no second spawn)' ($procs4.Count -eq 1) "found $($procs4.Count)"
    if ($procs3.Count -eq 1 -and $procs4.Count -eq 1) {
        Check 'same pid (not restarted)' ($procs4[0].ProcessId -eq $procs3[0].ProcessId) 'pid changed'
    }

    Write-Host "`n=== 5. shutdown stops the daemon and leaves a fake supervisor alone ===" -ForegroundColor Cyan
    # Stand-in for a capsule supervisor: any long-lived NON-sotd.exe process.
    # Proves -Stop's match (Name='sotd.exe' + this pipe) never widens to
    # anything else running alongside it.
    $fakeSup = Start-Process -FilePath 'powershell.exe' `
        -ArgumentList @('-NoProfile', '-Command', 'Start-Sleep -Seconds 60') `
        -WindowStyle Hidden -PassThru
    try {
        $out5 = & $script -Stop -Prefix $p3 -PipeName $pipe3 6>&1 2>&1
        $exit5 = $LASTEXITCODE
        Check 'stop exit code 0' ($exit5 -eq 0) "got $exit5; log: $out5"
        Check 'pipe gone' (Wait-PipeGone $pipe3) 'pipe still listed'
        $procs5 = Get-DaemonProcs $pipe3
        Check 'sotd.exe process gone' ($procs5.Count -eq 0) "still found $($procs5.Count)"
        Start-Sleep -Milliseconds 300
        $fakeSup.Refresh()
        Check 'fake supervisor left alone' (-not $fakeSup.HasExited) 'fake supervisor was killed too'
    } finally {
        if ($fakeSup -and -not $fakeSup.HasExited) {
            try { Stop-Process -Id $fakeSup.Id -Force -ErrorAction SilentlyContinue } catch {}
        }
    }
}

Write-Host "`n================ $pass passed, $fail failed ================" -ForegroundColor $(if ($fail) { 'Red' } else { 'Green' })

# Defensive cleanup: any real sotd.exe left running under a test-only pipe
# name (an assertion failing mid-run must not leak a process past this test).
Get-CimInstance Win32_Process -Filter "Name='sotd.exe'" |
    Where-Object { $_.CommandLine -and $_.CommandLine.Contains('test-sot-ld-') } |
    ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
if ($fail) { exit 1 }

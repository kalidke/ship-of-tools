# test-local-daemon-support.ps1 -- shared setup for test-local-daemon.ps1 and test-launcher-leases.ps1: Check, the fixture and pipe helpers, the test root. Dot-sourced.

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

# The outer finally's cleanup: restore the environment, stop the spawned processes, remove the test root.
function Complete-LocalDaemonTest {
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

# agents\comm-pipe-request.ps1 as its own process, as the shell clients run it: $Lines on its stdin through a file, its
# stdout and stderr to files, and a 20 s bound on the wait, so a transport that hangs fails its section with what it
# wrote instead of stalling the job.
function Invoke-PipeTransport([string]$Pipe, [string]$Op, [string[]]$Lines) {
    $transport = Join-Path $repo 'agents\comm-pipe-request.ps1'
    $base = Join-Path $root ('transport-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
    [System.IO.File]::WriteAllText("$base.in", ($Lines -join "`n") + "`n", (New-Object System.Text.UTF8Encoding($false)))
    $argv = '-NoProfile -ExecutionPolicy Bypass -File "' + $transport + '" -PipeName ' + $Pipe + ' -Op ' + $Op + ' -TimeoutSec 10'
    $proc = Start-Process -FilePath 'powershell.exe' -ArgumentList $argv -RedirectStandardInput "$base.in" -RedirectStandardOutput "$base.out" -RedirectStandardError "$base.err" -WindowStyle Hidden -PassThru
    $null = $proc.Handle   # Windows PowerShell 5.1 reads ExitCode as empty unless the handle was taken while the process ran
    $hung = -not $proc.WaitForExit(20000)
    if ($hung) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    [pscustomobject]@{
        Hung = $hung
        Out  = @(Get-Content -LiteralPath "$base.out" -ErrorAction SilentlyContinue | Where-Object { $_ -ne '' })
        Err  = (@(Get-Content -LiteralPath "$base.err" -ErrorAction SilentlyContinue) -join ' ')
        Exit = $(if ($hung) { -1 } else { $proc.ExitCode })
    }
}

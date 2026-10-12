# test-sot-within.ps1 -- scripts/sot-hosts.ps1's Invoke-SotWithin: a native command under a wall-clock bound.
# A stalled command ends at its bound with Code 124 and a line naming it, and is killed; a command that finishes
# returns its output and exit code; stdin reaches the command.
# Run under WINDOWS POWERSHELL 5.1: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\tests\test-sot-within.ps1

$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$pass = 0; $fail = 0
function Check([string]$name, [bool]$ok, [string]$detail) {
    if ($ok) { $script:pass++; Write-Host "  PASS  $name" -ForegroundColor Green }
    else { $script:fail++; Write-Host "  FAIL  $name -- $detail" -ForegroundColor Red }
}

. (Join-Path $repoRoot 'scripts\sot-hosts.ps1')

try {
    Write-Host "`n=== a stalled command ends at its bound ===" -ForegroundColor Cyan
    $marker = Join-Path $env:TEMP ("sot-within-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    $r = Invoke-SotWithin 2 'powershell' @('-NoProfile', '-Command', "Set-Content -LiteralPath '$marker' -Value `$PID; Start-Sleep 600")
    $clock.Stop()
    Check 'the code is 124' ($r.Code -eq 124) "Code=$($r.Code)"
    Check 'the output names the command and the bound' ($r.Output -ceq 'powershell did not finish within 2 s') "Output='$($r.Output)'"
    Check 'it ends within 5 s' ($clock.Elapsed.TotalSeconds -lt 5) "took $($clock.Elapsed.TotalSeconds) s"
    $childPid = 0
    if (Test-Path -LiteralPath $marker) { $childPid = [int](Get-Content -LiteralPath $marker | Select-Object -First 1) }
    Check 'the child recorded its pid' ($childPid -gt 0) 'the child never started'
    Check 'the child is gone' ($childPid -gt 0 -and -not (Get-Process -Id $childPid -ErrorAction SilentlyContinue)) "pid $childPid still runs"
    Remove-Item -LiteralPath $marker -Force -ErrorAction SilentlyContinue

    Write-Host "`n=== a command that finishes returns its output ===" -ForegroundColor Cyan
    $r = Invoke-SotWithin 30 'powershell' @('-NoProfile', '-Command', 'Write-Output hello; exit 3')
    Check 'its exit code' ($r.Code -eq 3) "Code=$($r.Code)"
    Check 'its output' ($r.Output.Trim() -ceq 'hello' -and $r.Stdout.Trim() -ceq 'hello') "Output='$($r.Output)'"
    $r = Invoke-SotWithin 30 'powershell' @('-NoProfile', '-Command', '$x = [Console]::In.ReadToEnd(); Write-Output "got:$($x.Trim())"') "line one`nline two`n"
    Check 'stdin reaches the command' ($r.Output -match 'got:line one\s+line two') "Output='$($r.Output)'"
    $threw = $false
    try { Invoke-SotWithin 5 'powershell' @('-NoProfile', '-Command', 'exit 0') ('x' * 4001) | Out-Null } catch { $threw = $true }
    Check 'a stdin over 4000 bytes is refused' $threw 'no error'
} catch { Check 'the suite ran' $false $_.Exception.Message }

Write-Host "`n================ $pass passed, $fail failed ================" -ForegroundColor $(if ($fail) { 'Red' } else { 'Green' })
if ($fail) { exit 1 }
exit 0

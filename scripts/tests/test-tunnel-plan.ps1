# test-tunnel-plan.ps1 -- regression harness for scripts/sot-hosts.ps1's
# Get-SotTopologyPlan (topology plan, lane D: `sotd topology plan --self
# <host>` is the one parser now; this reads its plain-line stdout).
#
# Section 0 syntax-parses every .ps1 this unit touches, same convention as
# test-local-daemon.ps1's own Section 0 (this repo's CI has no
# PSScriptAnalyzer -- only the ParseFile gate in .github/workflows/rust.yml,
# which globs scripts/*.ps1 WITHOUT -Recurse and so never reaches
# scripts/tests/*.ps1).
#
# Sections 1+ exercise Get-SotTopologyPlan against a FAKE `sotd` -- a
# batch stub that echoes fixed lines, so this is pure text processing, no
# real sotd binary, no ssh, no network. rust/protocol/src/topology.rs's
# `plan` doc comment is the contract this stub imitates; a reviewer may
# still adjust that format, so this parses it in ONE function
# (Get-SotTopologyPlan) and nowhere else.
#
# ASCII ONLY (see the same note in sot-hosts.ps1 / launch-sot.ps1).
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\tests\test-tunnel-plan.ps1

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$script = Join-Path $repo 'scripts\sot-hosts.ps1'
$root = Join-Path $env:TEMP ("sot-tunnel-plan-test-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Force -Path $root | Out-Null
$pass = 0; $fail = 0

function Check([string]$name, [bool]$ok, [string]$detail) {
    if ($ok) { $script:pass++; Write-Host "  PASS  $name" -ForegroundColor Green }
    else { $script:fail++; Write-Host "  FAIL  $name -- $detail" -ForegroundColor Red }
}

# A fake sotd: a .cmd stub that just echoes fixed lines, so Get-SotTopologyPlan
# is exercised end-to-end (it really does `& $SotdPath topology plan ...`)
# without a real Rust binary. `%*` swallows `topology plan [--self <host>]`
# unread -- the stub always answers the same fixture, which is all a pure
# parser test needs.
function New-FakeSotd([string]$name, [string[]]$lines) {
    $path = Join-Path $root "$name.cmd"
    $body = "@echo off`r`n" + (($lines | ForEach-Object { "echo $_" }) -join "`r`n") + "`r`n"
    Set-Content -LiteralPath $path -Value $body -Encoding ascii
    return $path
}

try {
    Write-Host "`n=== 0. syntax parse of every .ps1 this unit touches ===" -ForegroundColor Cyan
    foreach ($f in @(
            (Join-Path $repo 'scripts\sot-hosts.ps1'),
            (Join-Path $repo 'scripts\launch-sot.ps1'),
            (Join-Path $repo 'scripts\shutdown-sot.ps1'),
            (Join-Path $repo 'scripts\tests\test-tunnel-plan.ps1')
        )) {
        $errs = $null
        [void][System.Management.Automation.Language.Parser]::ParseFile($f, [ref]$null, [ref]$errs)
        $detail = ($errs | ForEach-Object { "$($_.Extent.StartLineNumber): $($_.Message)" }) -join '; '
        Check "parses: $(Split-Path $f -Leaf)" ($errs.Count -eq 0) $detail
    }

    . $script

    Write-Host "`n=== 1. a well-formed plan ===" -ForegroundColor Cyan
    $goodSotd = New-FakeSotd 'good' @(
        'self myserver',
        'hub hub-box',
        'relay-endpoint tcp:127.0.0.1:18743',
        'dial hub-box tcp:127.0.0.1:18743',
        'dial otherbox tcp:127.0.0.1:18744',
        'tunnel hub-box 18743',
        'tunnel otherbox 18744'
    )
    $plan = Get-SotTopologyPlan -SotdPath $goodSotd -SelfHost myserver
    Check 'no error' (-not $plan.Error) "got: $($plan.Error)"
    Check 'self parsed' ($plan.Self -eq 'myserver') "got $($plan.Self)"
    Check 'hub parsed' ($plan.Hub -eq 'hub-box') "got $($plan.Hub)"
    Check 'relay-endpoint parsed' ($plan.RelayEndpoint -eq 'tcp:127.0.0.1:18743') "got $($plan.RelayEndpoint)"
    Check 'two dials captured, in order' `
        ((($plan.Dials | ForEach-Object { $_.Host }) -join ',') -eq 'hub-box,otherbox') `
        "got $(($plan.Dials | ForEach-Object { $_.Host }) -join ',')"
    Check 'otherbox dial endpoint carried through' `
        (($plan.Dials | Where-Object { $_.Host -eq 'otherbox' }).Endpoint -eq 'tcp:127.0.0.1:18744') `
        'endpoint mismatch'
    Check 'two tunnels captured, ports parsed as int' `
        ((($plan.Tunnels | Where-Object { $_.Host -eq 'hub-box' }).Port -eq 18743) -and
         (($plan.Tunnels | Where-Object { $_.Host -eq 'otherbox' }).Port -eq 18744)) `
        "got $(($plan.Tunnels | ForEach-Object { "$($_.Host)=$($_.Port)" }) -join ',')"

    Write-Host "`n=== 2. an endpoint containing a space (Windows pipe path, verbatim username) ===" -ForegroundColor Cyan
    $spaceSotd = New-FakeSotd 'space' @(
        'self myserver',
        'hub myserver',
        'relay-endpoint pipe:\\.\pipe\sot-My User-sot',
        'dial myserver pipe:\\.\pipe\sot-My User-sot'
    )
    $spacePlan = Get-SotTopologyPlan -SotdPath $spaceSotd -SelfHost myserver
    Check 'relay-endpoint keeps its embedded space intact' `
        ($spacePlan.RelayEndpoint -eq 'pipe:\\.\pipe\sot-My User-sot') `
        "got [$($spacePlan.RelayEndpoint)]"
    Check 'dial endpoint keeps its embedded space intact (host not swallowed into it)' `
        (($spacePlan.Dials.Count -eq 1) -and
         ($spacePlan.Dials[0].Host -eq 'myserver') -and
         ($spacePlan.Dials[0].Endpoint -eq 'pipe:\\.\pipe\sot-My User-sot')) `
        "got host=[$($spacePlan.Dials[0].Host)] endpoint=[$($spacePlan.Dials[0].Endpoint)]"

    Write-Host "`n=== 3. an unknown first word is ignored, not an error ===" -ForegroundColor Cyan
    $futureSotd = New-FakeSotd 'future' @(
        'self myserver',
        'hub hub-box',
        'a-future-fact something new here',
        'dial hub-box tcp:127.0.0.1:18743'
    )
    $futurePlan = Get-SotTopologyPlan -SotdPath $futureSotd -SelfHost myserver
    Check 'no error from the unrecognised line' (-not $futurePlan.Error) "got: $($futurePlan.Error)"
    Check 'self/hub/dial still parsed around it' `
        (($futurePlan.Self -eq 'myserver') -and ($futurePlan.Hub -eq 'hub-box') -and ($futurePlan.Dials.Count -eq 1)) `
        'a field was lost'

    Write-Host "`n=== 4. no sotd binary at all ===" -ForegroundColor Cyan
    $missingPlan = Get-SotTopologyPlan -SotdPath (Join-Path $root 'does-not-exist.exe')
    Check 'missing binary yields an empty, errored plan (not a throw)' `
        ($missingPlan.Error -and $missingPlan.Dials.Count -eq 0 -and $missingPlan.Tunnels.Count -eq 0) `
        "got error=[$($missingPlan.Error)]"
} finally {
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "`n================ $pass passed, $fail failed ================" -ForegroundColor $(if ($fail) { 'Red' } else { 'Green' })
if ($fail) { exit 1 }

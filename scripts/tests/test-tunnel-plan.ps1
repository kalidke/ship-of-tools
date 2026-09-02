# test-tunnel-plan.ps1 -- regression harness for scripts/sot-hosts.ps1's
# Read-SotHosts / Get-TunnelPlan (ADR 0042 L2b design E: one tunnel per
# configured remote, both launchers).
#
# Section 0 syntax-parses every .ps1 this unit touches, same convention as
# test-local-daemon.ps1's own Section 0 (this repo's CI has no
# PSScriptAnalyzer -- only the ParseFile gate in .github/workflows/rust.yml,
# which globs scripts/*.ps1 WITHOUT -Recurse and so never reaches
# scripts/tests/*.ps1).
#
# Sections 1+ exercise Read-SotHosts/Get-TunnelPlan against a FIXTURE
# hosts.toml -- pure text processing, no ssh, no network. This is the
# PowerShell-side sibling of scripts/tests/test-tunnel-plan.sh, which
# exercises the same fixture shape through sot_tunnel_plan.
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

    $fixture = Join-Path $root 'hosts.toml'
    @'
default_host = "myserver"

[host.myserver]
ssh_alias = "myserver"
remote_repo = "/home/me/project"
tcp_port = 18743

[host.otherbox]
ssh_alias = "otherbox"
remote_repo = "/home/me/project"
tcp_port = 18744
remote_socket = "/run/user/1000/sot/sessions/sot.sock"

[host.thirdbox]
ssh_alias = "thirdbox"
remote_repo = "/home/me/project3"
# tcp_port deliberately omitted -- not the default host, so this must error

[host.local]
socket = "\\.\pipe\sot-local"
'@ | Set-Content -LiteralPath $fixture -Encoding utf8

    Write-Host "`n=== 1. Read-SotHosts ===" -ForegroundColor Cyan
    $cfg = Read-SotHosts -Path $fixture
    Check 'default_host parsed' ($cfg.default_host -eq 'myserver') "got $($cfg.default_host)"
    Check 'four hosts captured' ($cfg.hosts.Count -eq 4) "got $($cfg.hosts.Count)"
    Check 'declaration order preserved' (($cfg.order -join ',') -eq 'myserver,otherbox,thirdbox,local') "got $($cfg.order -join ',')"
    Check 'local has no ssh_alias (only socket)' (-not $cfg.hosts['local'].ssh_alias) 'local unexpectedly has ssh_alias'

    Write-Host "`n=== 2. Get-TunnelPlan (default_alias=myserver, default_port=18743) ===" -ForegroundColor Cyan
    $plan = Get-TunnelPlan -Cfg $cfg -DefaultAlias 'myserver' -DefaultPort 18743
    $names = $plan | ForEach-Object { $_.host }
    Check 'local never appears (no ssh_alias)' (-not ($names -contains 'local')) "plan hosts: $($names -join ',')"
    Check 'exactly three remotes planned' ($plan.Count -eq 3) "got $($plan.Count): $($names -join ',')"

    $myserver = $plan | Where-Object { $_.host -eq 'myserver' }
    Check 'myserver keeps its own tcp_port' ($myserver.local_port -eq 18743) "got $($myserver.local_port)"
    Check 'myserver has no error' (-not $myserver.error) "got $($myserver.error)"

    $otherbox = $plan | Where-Object { $_.host -eq 'otherbox' }
    Check 'otherbox keeps its own tcp_port' ($otherbox.local_port -eq 18744) "got $($otherbox.local_port)"
    Check 'otherbox remote_socket override carried through' ($otherbox.remote -eq '/run/user/1000/sot/sessions/sot.sock') "got $($otherbox.remote)"
    Check 'otherbox has no error' (-not $otherbox.error) "got $($otherbox.error)"

    $thirdbox = $plan | Where-Object { $_.host -eq 'thirdbox' }
    Check 'thirdbox (missing tcp_port, not default) has no local_port' (-not $thirdbox.local_port) "got $($thirdbox.local_port)"
    Check 'thirdbox names the host and the missing field' `
        ($thirdbox.error -eq "host 'thirdbox' has no tcp_port (required for a remote tunnel)") `
        "got: $($thirdbox.error)"

    Write-Host "`n=== 3. Get-TunnelPlan with no matching default alias ===" -ForegroundColor Cyan
    $planNoDefault = Get-TunnelPlan -Cfg $cfg -DefaultAlias 'nonexistent-alias' -DefaultPort 18743
    $myserverNoDefault = $planNoDefault | Where-Object { $_.host -eq 'myserver' }
    Check 'myserver still resolves from its OWN tcp_port regardless of default match' `
        ($myserverNoDefault.local_port -eq 18743) "got $($myserverNoDefault.local_port)"
    $thirdboxNoDefault = $planNoDefault | Where-Object { $_.host -eq 'thirdbox' }
    Check 'thirdbox still errors (no fabricated default fallback)' `
        ($thirdboxNoDefault.error -eq "host 'thirdbox' has no tcp_port (required for a remote tunnel)") `
        "got: $($thirdboxNoDefault.error)"

    Write-Host "`n=== 4. empty/missing hosts.toml ===" -ForegroundColor Cyan
    $emptyCfg = Read-SotHosts -Path (Join-Path $root 'does-not-exist.toml')
    Check 'missing file yields empty registry' ($emptyCfg.hosts.Count -eq 0) "got $($emptyCfg.hosts.Count)"
    $emptyPlan = Get-TunnelPlan -Cfg $emptyCfg -DefaultAlias $null -DefaultPort 18743
    Check 'empty registry yields an empty plan' (@($emptyPlan).Count -eq 0) "got $(@($emptyPlan).Count)"
} finally {
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "`n================ $pass passed, $fail failed ================" -ForegroundColor $(if ($fail) { 'Red' } else { 'Green' })
if ($fail) { exit 1 }

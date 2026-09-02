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
# hosts.toml -- pure text processing, no ssh, no network. Same host set
# (names, ports, aliases) as scripts/tests/test-tunnel-plan.sh's own
# fixture -- kept as two copies (a shared file would couple each
# language's test to the other's directory layout for no real gain at this
# size), not one shared file.
#
# Duplicate tcp_port across hosts is NOT tested here (owner ruling, codex
# follow-up round 2): Get-TunnelPlan doesn't detect it -- that check lives
# once, in rust/frontend/src/hosts.rs's resolve_connections (its own test).
# A second `ssh -L` on an already-bound port fails to bind on its own,
# which is already nonfatal, so the launcher needs nothing else here.
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
ssh_alias = "myserver-alias"
remote_repo = "/home/me/project"
# tcp_port omitted -- it is the default host (by KEY, not ssh_alias), so
# it falls back to whatever -DefaultPort Get-TunnelPlan is given

[host.otherbox]
ssh_alias = "otherbox"
remote_repo = "/home/me/project"
tcp_port = 18744
remote_socket = "/run/user/1000/sot/sessions/sot.sock"

[host.thirdbox]
ssh_alias = "thirdbox"
remote_repo = "/home/me/project3"
# tcp_port deliberately omitted -- not the default host, so this must error

[host.badport]
ssh_alias = "badport"
remote_repo = "/home/me/bad"
# An inline comment here demonstrates the real-world way this happens:
# none of the three hosts.toml parsers (Rust, PowerShell, bash) strip an
# inline comment, so it becomes part of the value verbatim.
tcp_port = 18745 # oops, an inline comment

[host.local]
ssh_alias = "myserver-alias"
tcp_port = 18743
'@ | Set-Content -LiteralPath $fixture -Encoding utf8

    Write-Host "`n=== 1. Read-SotHosts ===" -ForegroundColor Cyan
    $cfg = Read-SotHosts -Path $fixture
    Check 'default_host parsed' ($cfg.default_host -eq 'myserver') "got $($cfg.default_host)"
    Check 'five hosts captured' ($cfg.hosts.Count -eq 5) "got $($cfg.hosts.Count)"
    Check 'declaration order preserved' `
        (($cfg.order -join ',') -eq 'myserver,otherbox,thirdbox,badport,local') `
        "got $($cfg.order -join ',')"
    Check 'local carries whatever it was given (filtering is Get-TunnelPlan''s job)' `
        ($cfg.hosts['local'].ssh_alias -eq 'myserver-alias' -and $cfg.hosts['local'].tcp_port -eq '18743') `
        'local did not carry its configured fields'

    Write-Host "`n=== 2. Get-TunnelPlan (default_host=myserver, default_port=18743) ===" -ForegroundColor Cyan
    $plan = Get-TunnelPlan -Cfg $cfg -DefaultHost 'myserver' -DefaultPort 18743
    $names = $plan | ForEach-Object { $_.host }
    Check 'local never appears (socket-only, regardless of ssh_alias/tcp_port on it)' `
        (-not ($names -contains 'local')) "plan hosts: $($names -join ',')"
    Check 'four remotes planned (local excluded)' ($plan.Count -eq 4) "got $($plan.Count): $($names -join ',')"

    $myserver = $plan | Where-Object { $_.host -eq 'myserver' }
    Check 'myserver (identified by KEY) falls back to DefaultPort' ($myserver.local_port -eq 18743) "got $($myserver.local_port)"
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

    $badport = $plan | Where-Object { $_.host -eq 'badport' }
    Check 'badport (inline comment corrupts the port) has no local_port' (-not $badport.local_port) "got $($badport.local_port)"
    Check 'badport names the host and the bad value (comment included), not a thrown error' `
        ($badport.error -eq "host 'badport' has a malformed tcp_port '18745 # oops, an inline comment'") `
        "got: $($badport.error)"

    Write-Host "`n=== 3. Get-TunnelPlan with no matching default host ===" -ForegroundColor Cyan
    $planNoDefault = Get-TunnelPlan -Cfg $cfg -DefaultHost 'nonexistent-key' -DefaultPort 18743
    $myserverNoDefault = $planNoDefault | Where-Object { $_.host -eq 'myserver' }
    Check 'myserver now errors -- no tcp_port and no default match' `
        ($myserverNoDefault.error -eq "host 'myserver' has no tcp_port (required for a remote tunnel)") `
        "got: $($myserverNoDefault.error)"
    $thirdboxNoDefault = $planNoDefault | Where-Object { $_.host -eq 'thirdbox' }
    Check 'thirdbox still errors (no fabricated default fallback)' `
        ($thirdboxNoDefault.error -eq "host 'thirdbox' has no tcp_port (required for a remote tunnel)") `
        "got: $($thirdboxNoDefault.error)"

    Write-Host "`n=== 4. -DefaultHost matches by KEY only, never by ssh_alias ===" -ForegroundColor Cyan
    $planByAlias = Get-TunnelPlan -Cfg $cfg -DefaultHost 'myserver-alias' -DefaultPort 18743
    $myserverByAlias = $planByAlias | Where-Object { $_.host -eq 'myserver' }
    Check 'myserver-alias (the ssh_alias, not the key) does NOT count as the default' `
        ($myserverByAlias.error -eq "host 'myserver' has no tcp_port (required for a remote tunnel)") `
        "got: $($myserverByAlias.error)"

    Write-Host "`n=== 5. empty/missing hosts.toml ===" -ForegroundColor Cyan
    $emptyCfg = Read-SotHosts -Path (Join-Path $root 'does-not-exist.toml')
    Check 'missing file yields empty registry' ($emptyCfg.hosts.Count -eq 0) "got $($emptyCfg.hosts.Count)"
    $emptyPlan = Get-TunnelPlan -Cfg $emptyCfg -DefaultHost $null -DefaultPort 18743
    Check 'empty registry yields an empty plan' (@($emptyPlan).Count -eq 0) "got $(@($emptyPlan).Count)"
} finally {
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "`n================ $pass passed, $fail failed ================" -ForegroundColor $(if ($fail) { 'Red' } else { 'Green' })
if ($fail) { exit 1 }

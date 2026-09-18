# test-install-layout.ps1 -- regression harness for scripts/sot-install-
# layout.ps1's Test-SotPinnedCheckout and Get-SotLauncherTarget: the one
# predicate that replaces the four disagreeing dev-vs-release heuristics
# launch-sot.ps1 used to have (docs/adr/0030-versioning-release-and-auto-
# update.md's 2026-09-17 amendment).
#
# Section 0 syntax-parses every .ps1 this unit touches, same convention as
# test-tunnel-plan.ps1's own Section 0 (this repo's CI has no
# PSScriptAnalyzer -- only the ParseFile gate in .github/workflows/rust.yml,
# which globs scripts/*.ps1 WITHOUT -Recurse and so never reaches
# scripts/tests/*.ps1).
#
# Sections 1+ build a REAL temp git repo (tagged v9.9.9), a detached
# worktree of it, and a plain branch clone, so Test-SotPinnedCheckout is
# exercised against actual git state -- no fake, no network.
#
# Run under WINDOWS POWERSHELL 5.1 (see the same note in test-sot-apply.ps1
# and sot-install-layout.ps1's own header -- BOM-less .ps1, ASCII-only
# string literals).
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\tests\test-install-layout.ps1

$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$script = Join-Path $repoRoot 'scripts\sot-install-layout.ps1'
$root = Join-Path $env:TEMP ("sot-install-layout-test-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Force -Path $root | Out-Null
$pass = 0; $fail = 0

function Check([string]$name, [bool]$ok, [string]$detail) {
    if ($ok) { $script:pass++; Write-Host "  PASS  $name" -ForegroundColor Green }
    else { $script:fail++; Write-Host "  FAIL  $name -- $detail" -ForegroundColor Red }
}

try {
    Write-Host "`n=== 0. syntax parse of every .ps1 this unit touches ===" -ForegroundColor Cyan
    foreach ($f in @(
            (Join-Path $repoRoot 'scripts\sot-install-layout.ps1'),
            (Join-Path $repoRoot 'scripts\launch-sot.ps1'),
            (Join-Path $repoRoot 'scripts\install-shortcut.ps1'),
            (Join-Path $repoRoot 'scripts\tests\test-install-layout.ps1')
        )) {
        $errs = $null
        [void][System.Management.Automation.Language.Parser]::ParseFile($f, [ref]$null, [ref]$errs)
        $detail = ($errs | ForEach-Object { "$($_.Extent.StartLineNumber): $($_.Message)" }) -join '; '
        Check "parses: $(Split-Path $f -Leaf)" ($errs.Count -eq 0) $detail
    }

    . $script

    Write-Host "`n=== 1. no .git at all: not pinned ===" -ForegroundColor Cyan
    $noGit = Join-Path $root 'no-git'
    New-Item -ItemType Directory -Force -Path $noGit | Out-Null
    Check 'a plain directory with no .git is never pinned' `
        (-not (Test-SotPinnedCheckout -Repo $noGit)) 'reported pinned with no git metadata at all'

    Write-Host "`n=== 2. a real clone on a branch: not pinned ===" -ForegroundColor Cyan
    $clone = Join-Path $root 'clone'
    New-Item -ItemType Directory -Force -Path $clone | Out-Null
    Push-Location $clone
    git init -q 2>$null | Out-Null
    git config user.email t@t 2>$null; git config user.name t 2>$null
    Set-Content -LiteralPath (Join-Path $clone 'f.txt') -Value 'one'
    git add -A 2>$null | Out-Null
    git commit -qm init 2>$null | Out-Null
    git tag v9.9.9 2>$null | Out-Null
    Pop-Location
    Check 'a checkout on a branch is not pinned' `
        (-not (Test-SotPinnedCheckout -Repo $clone)) 'a branch clone reported pinned'

    Write-Host "`n=== 3. a detached worktree at a tag: pinned ===" -ForegroundColor Cyan
    $current = Join-Path $root 'repo-current'
    Push-Location $clone
    git worktree add --detach $current v9.9.9 2>&1 | Out-Null
    Pop-Location
    Check 'a detached worktree at a tag is pinned' `
        (Test-SotPinnedCheckout -Repo $current) 'a detached worktree reported NOT pinned'
    Check 'Get-SotPinnedTag reads the tag back off the detached HEAD' `
        ((Get-SotPinnedTag -Repo $current) -eq 'v9.9.9') "got '$(Get-SotPinnedTag -Repo $current)'"

    Write-Host "`n=== 4. Get-SotLauncherTarget: pinned launcher when present, else the clone's ===" -ForegroundColor Cyan
    $prefix = Join-Path $root 'prefix'
    New-Item -ItemType Directory -Force -Path $prefix | Out-Null
    $noPinnedTarget = Get-SotLauncherTarget -Prefix $prefix -ClonePath $clone
    Check 'no repo\current at all: falls back to the clone launcher' `
        ($noPinnedTarget -eq (Join-Path $clone 'scripts\launch-sot.ps1')) "got '$noPinnedTarget'"

    $repoCurrentDir = Join-Path $prefix 'repo\current'
    New-Item -ItemType Directory -Force -Path (Join-Path $repoCurrentDir 'scripts') | Out-Null
    $pinnedLauncherFile = Join-Path $repoCurrentDir 'scripts\launch-sot.ps1'
    Set-Content -LiteralPath $pinnedLauncherFile -Value '# stand-in for the pinned launcher'
    $pinnedTarget = Get-SotLauncherTarget -Prefix $prefix -ClonePath $clone
    Check 'repo\current\scripts\launch-sot.ps1 present: that is the target' `
        ($pinnedTarget -eq $pinnedLauncherFile) "got '$pinnedTarget'"

    Write-Host "`n=== 5. Set-SotJunction (moved from launch-sot.ps1, unchanged) still works ===" -ForegroundColor Cyan
    # Write-SupLog is a caller-provided function on the real launch path
    # (dot-sourcing puts Set-SotJunction in launch-sot.ps1's own scope,
    # where it always exists) -- stub it here so this unit stays hermetic.
    function Write-SupLog { param([string]$Message) }
    $junctionLink = Join-Path $prefix 'repo\junction-check'
    $ok = Set-SotJunction $junctionLink $current
    Check 'junction created' $ok 'Set-SotJunction returned false'
    Check 'junction resolves to the target' `
        ((Get-Item -LiteralPath $junctionLink -Force).Target -contains $current) 'junction target mismatch'
} finally {
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "`n================ $pass passed, $fail failed ================" -ForegroundColor $(if ($fail) { 'Red' } else { 'Green' })
if ($fail) { exit 1 }
exit 0

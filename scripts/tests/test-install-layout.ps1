# test-install-layout.ps1 -- regression harness for scripts/sot-install-
# layout.ps1's Test-SotPinnedCheckout and Get-SotLauncherTarget: the one
# predicate that replaces the four disagreeing dev-vs-release heuristics
# launch-sot.ps1 used to have (docs/adr/0030-versioning-release-and-auto-
# update.md's 2026-09-17 amendment). Test-SotPinnedCheckout is a path
# identity (is this script running FROM repo\current), not a git-state
# check, so no git fixture is needed here -- CI's ParseFile gate
# (.github/workflows/rust.yml) already syntax-covers launch-sot.ps1 and
# install-shortcut.ps1, so this file's own Section 0 only re-parses ITSELF
# and the file it dot-sources.
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
    Write-Host "`n=== 0. syntax parse ===" -ForegroundColor Cyan
    foreach ($f in @(
            (Join-Path $repoRoot 'scripts\sot-install-layout.ps1'),
            (Join-Path $repoRoot 'scripts\tests\test-install-layout.ps1')
        )) {
        $errs = $null
        [void][System.Management.Automation.Language.Parser]::ParseFile($f, [ref]$null, [ref]$errs)
        $detail = ($errs | ForEach-Object { "$($_.Extent.StartLineNumber): $($_.Message)" }) -join '; '
        Check "parses: $(Split-Path $f -Leaf)" ($errs.Count -eq 0) $detail
    }

    . $script

    Write-Host "`n=== 1. Test-SotPinnedCheckout: a path identity ===" -ForegroundColor Cyan
    $prefix = Join-Path $root 'prefix'
    New-Item -ItemType Directory -Force -Path $prefix | Out-Null
    $repoCurrentDir = Join-Path $prefix 'repo\current'
    New-Item -ItemType Directory -Force -Path $repoCurrentDir | Out-Null
    $clone = Join-Path $root 'clone'
    New-Item -ItemType Directory -Force -Path $clone | Out-Null
    Check 'the clone path is not pinned' `
        (-not (Test-SotPinnedCheckout -RepoPath $clone -Prefix $prefix)) 'the clone reported pinned'
    Check 'repo\current itself is pinned' `
        (Test-SotPinnedCheckout -RepoPath $repoCurrentDir -Prefix $prefix) 'repo\current reported NOT pinned'
    Check 'a same-named sibling directory is not pinned (path identity, not a name match)' `
        (-not (Test-SotPinnedCheckout -RepoPath (Join-Path $root 'other\repo\current') -Prefix $prefix)) `
        'a different prefix''s repo\current reported pinned'

    Write-Host "`n=== 2. Get-SotLauncherTarget: pinned launcher when present, else the clone's ===" -ForegroundColor Cyan
    $noPinnedTarget = Get-SotLauncherTarget -Prefix $prefix -ClonePath $clone
    Check 'no repo\current\scripts\launch-sot.ps1 yet: falls back to the clone launcher' `
        ($noPinnedTarget -eq (Join-Path $clone 'scripts\launch-sot.ps1')) "got '$noPinnedTarget'"

    New-Item -ItemType Directory -Force -Path (Join-Path $repoCurrentDir 'scripts') | Out-Null
    $pinnedLauncherFile = Join-Path $repoCurrentDir 'scripts\launch-sot.ps1'
    Set-Content -LiteralPath $pinnedLauncherFile -Value '# stand-in for the pinned launcher'
    $pinnedTarget = Get-SotLauncherTarget -Prefix $prefix -ClonePath $clone
    Check 'repo\current\scripts\launch-sot.ps1 present: that is the target' `
        ($pinnedTarget -eq $pinnedLauncherFile) "got '$pinnedTarget'"
} finally {
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "`n================ $pass passed, $fail failed ================" -ForegroundColor $(if ($fail) { 'Red' } else { 'Green' })
if ($fail) { exit 1 }
exit 0

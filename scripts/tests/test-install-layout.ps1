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

    Write-Host "`n=== 3. Get-SotLauncherCodeId: the launcher code on disk, seen through repo\current ===" -ForegroundColor Cyan
    try {
        function New-ScriptsDir([string]$Dir, [string]$HostsText) {
            New-Item -ItemType Directory -Force -Path $Dir | Out-Null
            Set-Content -LiteralPath (Join-Path $Dir 'launch-sot.ps1') -Value '# launcher stand-in' -Encoding ascii
            Set-Content -LiteralPath (Join-Path $Dir 'sot-hosts.ps1') -Value $HostsText -Encoding ascii
            Set-Content -LiteralPath (Join-Path $Dir 'sot-install-layout.ps1') -Value '# layout stand-in' -Encoding ascii
        }
        $tagA = Join-Path $root 'tags\A'
        $tagB = Join-Path $root 'tags\B'
        New-ScriptsDir (Join-Path $tagA 'scripts') '# hosts A'
        New-ScriptsDir (Join-Path $tagB 'scripts') '# hosts B'
        $idA = Get-SotLauncherCodeId -ScriptsDir (Join-Path $tagA 'scripts')
        $idB = Get-SotLauncherCodeId -ScriptsDir (Join-Path $tagB 'scripts')
        Check '3: the id is three SHA-256 hashes joined by -' ($idA -cmatch '^[0-9A-F]{64}-[0-9A-F]{64}-[0-9A-F]{64}$') "got '$idA'"
        Check '3: the same files give the same id' ($idA -ceq (Get-SotLauncherCodeId -ScriptsDir (Join-Path $tagA 'scripts'))) 'the id changed between two reads'
        Check '3: a change to a dot-sourced file changes the id' ($idA -cne $idB) 'tags A and B share an id'
        $cur = Join-Path $root 'current'
        $curScripts = Join-Path $cur 'scripts'
        New-Item -ItemType Junction -Path $cur -Target $tagA | Out-Null
        Check '3: through the junction, the id is its target''s' ((Get-SotLauncherCodeId -ScriptsDir $curScripts) -ceq $idA) 'the junction id differs from tag A'
        cmd /c rmdir "$cur" | Out-Null
        New-Item -ItemType Junction -Path $cur -Target $tagB | Out-Null
        Check '3: after the junction flips, the same path string gives the new id' ((Get-SotLauncherCodeId -ScriptsDir $curScripts) -ceq $idB) 'the flip did not show through the unchanged path'
        cmd /c rmdir "$cur" | Out-Null
        Remove-Item -LiteralPath (Join-Path $tagB 'scripts\sot-hosts.ps1') -Force
        Check '3: a missing file gives an empty id' ((Get-SotLauncherCodeId -ScriptsDir (Join-Path $tagB 'scripts')) -ceq '') 'a partial scripts dir got an id'
    } catch { Check '3: section ran' $false $_.Exception.Message }

    Write-Host "`n=== 4. Set-SotFolderTrust: published only when settings.toml does not exist ===" -ForegroundColor Cyan
    try {
        # Ordered equality: same length, then every byte in order.
        function Test-SameBytes([byte[]]$A, [byte[]]$B) {
            if ($A.Length -ne $B.Length) { return $false }
            for ($k = 0; $k -lt $A.Length; $k++) { if ($A[$k] -ne $B[$k]) { return $false } }
            return $true
        }
        $homeDir = 'C:\Users\someone'
        $cfgNew = Join-Path $root 'cfg-new'
        Check '4: no file: returns declared' ((Set-SotFolderTrust -ConfigDir $cfgNew -HomeDir $homeDir) -ceq 'declared') 'did not return declared'
        $f = Join-Path $cfgNew 'settings.toml'
        $bytes = [System.IO.File]::ReadAllBytes($f)
        $text = [System.Text.Encoding]::UTF8.GetString($bytes)
        Check '4: root_prefix is the home with / separators' ($text -match '(?m)^root_prefix = "C:/Users/someone"\r?$') $text
        Check '4: no BOM' (-not ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF)) 'file starts with a BOM'
        Check '4: no temp file left' (@(Get-ChildItem -LiteralPath $cfgNew -Filter '*.tmp').Count -eq 0) 'a .tmp file remains'
        Check '4: a second call returns kept' ((Set-SotFolderTrust -ConfigDir $cfgNew -HomeDir $homeDir) -ceq 'kept') 'did not return kept'
        Check '4: a second call leaves the file byte-identical' (Test-SameBytes $bytes ([System.IO.File]::ReadAllBytes($f))) 'bytes changed'
        $utf8 = New-Object System.Text.UTF8Encoding($false)
        $cases = [ordered]@{
            'UTF-8 without [trust]'                    = $utf8.GetBytes("[display]`nx = 1`n")
            'UTF-16 with BOM, no trust header'         = ([System.Text.Encoding]::Unicode.GetPreamble() + [System.Text.Encoding]::Unicode.GetBytes("[display]`r`nx = 1`r`n"))
            '[trust] # comment and a commented key'    = $utf8.GetBytes("[trust] # mine`n# root_prefix = `"C:/x`"`n")
        }
        $i = 0
        foreach ($name in $cases.Keys) {
            $i++
            $cfg = Join-Path $root "cfg-own$i"
            New-Item -ItemType Directory -Force -Path $cfg | Out-Null
            $own = Join-Path $cfg 'settings.toml'
            [System.IO.File]::WriteAllBytes($own, [byte[]]$cases[$name])
            $before = [System.IO.File]::ReadAllBytes($own)
            $r = Set-SotFolderTrust -ConfigDir $cfg -HomeDir $homeDir
            Check "4: existing file ($name): returns no-header" ($r -ceq 'no-header') "returned $r"
            Check "4: existing file ($name): byte-identical" (Test-SameBytes $before ([System.IO.File]::ReadAllBytes($own))) 'the owner''s file was changed'
        }
        # The race: the destination appears after the temp file is closed, before the move.
        $cfgRace = Join-Path $root 'cfg-race'
        $raceFile = Join-Path $cfgRace 'settings.toml'
        $r = Set-SotFolderTrust -ConfigDir $cfgRace -HomeDir $homeDir -BeforePublish { [System.IO.File]::WriteAllText($raceFile, 'owner') }
        Check '4: race: returns kept' ($r -ceq 'kept') "returned $r"
        Check '4: race: the owner''s file is untouched' (([System.IO.File]::ReadAllText($raceFile)) -ceq 'owner') 'the winner''s file was replaced'
        Check '4: race: no temp file left' (@(Get-ChildItem -LiteralPath $cfgRace -Filter '*.tmp').Count -eq 0) 'a .tmp file remains'
    } catch { Check '4: section ran' $false $_.Exception.Message }
} finally {
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "`n================ $pass passed, $fail failed ================" -ForegroundColor $(if ($fail) { 'Red' } else { 'Green' })
if ($fail) { exit 1 }
exit 0

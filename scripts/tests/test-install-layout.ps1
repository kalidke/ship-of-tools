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

param([string]$SotdPath)

$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
if (-not $SotdPath) {
    $testTarget = $env:CARGO_TARGET_DIR
    if (-not $testTarget) { $testTarget = Join-Path $repoRoot 'rust\target' }
    $SotdPath = Join-Path $testTarget 'debug\sotd.exe'
}
if (-not (Test-Path -LiteralPath $SotdPath)) { throw 'W1 real offline declaration binary is required' }
$SotdPath = (Resolve-Path -LiteralPath $SotdPath).Path
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
            Set-Content -LiteralPath (Join-Path $Dir 'sot-freshness.ps1') -Value '# freshness stand-in' -Encoding ascii
            Set-Content -LiteralPath (Join-Path $Dir 'sot-lease.ps1') -Value '# lease stand-in' -Encoding ascii
        }
        $tagA = Join-Path $root 'tags\A'
        $tagB = Join-Path $root 'tags\B'
        New-ScriptsDir (Join-Path $tagA 'scripts') '# hosts A'
        New-ScriptsDir (Join-Path $tagB 'scripts') '# hosts B'
        $idA = Get-SotLauncherCodeId -ScriptsDir (Join-Path $tagA 'scripts')
        $idB = Get-SotLauncherCodeId -ScriptsDir (Join-Path $tagB 'scripts')
        Check '3: the id is five SHA-256 hashes joined by -' ($idA -cmatch '^[0-9A-F]{64}(-[0-9A-F]{64}){4}$') "got '$idA'"
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

    $homeDir = Join-Path $root 'race-scope'
        # The race: the destination appears after the temp file is closed, before the move.
        $cfgRace = Join-Path $root 'cfg-race'
        $raceFile = Join-Path $cfgRace 'settings.toml'
        $r = Set-SotFolderTrust -ConfigDir $cfgRace -HomeDir $homeDir -BeforePublish { [System.IO.File]::WriteAllText($raceFile, 'owner') }
        Check '4: race: returns kept' ($r -ceq 'kept') "returned $r"
        Check '4: race: the owner''s file is untouched' (([System.IO.File]::ReadAllText($raceFile)) -ceq 'owner') 'the winner''s file was replaced'
        Check '4: race: no temp file left' (@(Get-ChildItem -LiteralPath $cfgRace -Filter '*.tmp').Count -eq 0) 'a .tmp file remains'

    Write-Host "`n=== 4. executed offline trust delegation ===" -ForegroundColor Cyan
    $trustKeys = @('HOME', 'USERPROFILE', 'LOCALAPPDATA', 'XDG_CONFIG_HOME', 'XDG_STATE_HOME', 'CLAUDE_CONFIG_DIR')
    $savedTrustEnv = @{}
    foreach ($key in $trustKeys) { $savedTrustEnv[$key] = [Environment]::GetEnvironmentVariable($key, 'Process') }
    $savedSot = @{}
    Get-ChildItem Env: | Where-Object { $_.Name.StartsWith('SOT_') } | ForEach-Object {
        $savedSot[$_.Name] = $_.Value
        [Environment]::SetEnvironmentVariable($_.Name, $null, 'Process')
    }
    try {
        function Test-SameBytes([byte[]]$A, [byte[]]$B) {
            if ($A.Length -ne $B.Length) { return $false }
            for ($k = 0; $k -lt $A.Length; $k++) { if ($A[$k] -ne $B[$k]) { return $false } }
            return $true
        }
        function Write-SupLog([string]$Text) { $script:trustLog.Add($Text) | Out-Null }
        function Invoke-TrustFixture {
            $script:trustLog = New-Object 'System.Collections.Generic.List[string]'
            Initialize-InstallLayout
        }
        $homeDir = Join-Path $root ("home # ' " + [char]0x03BC)
        New-Item -ItemType Directory -Force -Path $homeDir | Out-Null
        $env:HOME = $homeDir; $env:USERPROFILE = $homeDir
        $env:LOCALAPPDATA = Join-Path $root 'local'
        $env:XDG_CONFIG_HOME = Join-Path $root 'config'
        $env:XDG_STATE_HOME = Join-Path $root 'state'
        [Environment]::SetEnvironmentVariable('CLAUDE_CONFIG_DIR', $null, 'Process')
        $prefixDir = Join-Path $env:LOCALAPPDATA 'sot'
        $cfg = Join-Path $prefixDir 'config'
        New-Item -ItemType Directory -Force -Path $cfg | Out-Null
        $f = Join-Path $cfg 'settings.toml'
        $repoCurrent = Join-Path $prefixDir 'repo\current'
        $repo = Join-Path $root 'fixture-checkout'
        $backendExe = $SotdPath
        $utf8 = New-Object System.Text.UTF8Encoding($false)
        $headerless = $utf8.GetBytes("# retained`n[layout]`npreset = 'auto'`n")
        [System.IO.File]::WriteAllBytes($f, $headerless)
        Invoke-TrustFixture
        $after = [System.IO.File]::ReadAllBytes($f)
        $text = $utf8.GetString($after)
        Check 'W1 C4 Windows headerless declaration' ($text -match '(?m)^root_prefix = ') 'owner added no declaration'
        Check 'W1 C4 Windows dev return follows declaration' (($script:trustLog -join "`n") -match 'folder trust declared' -and ($script:trustLog -join "`n") -match 'dev box') 'declaration did not precede dev return'
        Check 'W1 C4 Windows byte prefix retained' (Test-SameBytes $headerless ([byte[]]$after[0..($headerless.Length - 1)])) 'original byte prefix changed'
        Check 'W1 C4 Windows no BOM' (-not ($after.Length -ge 3 -and $after[0] -eq 0xEF -and $after[1] -eq 0xBB -and $after[2] -eq 0xBF)) 'BOM added'
        foreach ($table in @('[trust] # kept', 'trust = { root_prefix = "/kept" }', 'trust.root_prefix = "/kept"', '[ trust ]')) {
            $before = $utf8.GetBytes($table + "`n")
            [System.IO.File]::WriteAllBytes($f, $before)
            Invoke-TrustFixture
            Check 'W1 C4 Windows table form kept' (Test-SameBytes $before ([System.IO.File]::ReadAllBytes($f))) 'existing trust answer changed'
            Check 'W1 C4 Windows Kept reported' (($script:trustLog -join "`n") -match 'folder trust kept') 'Kept was not reported'
        }
        foreach ($invalid in @($utf8.GetBytes('[layout'), ([System.Text.Encoding]::Unicode.GetPreamble() + [System.Text.Encoding]::Unicode.GetBytes('[layout]')))) {
            [System.IO.File]::WriteAllBytes($f, [byte[]]$invalid)
            Invoke-TrustFixture
            Check 'W1 C4 Windows invalid bytes unchanged' (Test-SameBytes ([byte[]]$invalid) ([System.IO.File]::ReadAllBytes($f))) 'invalid document changed'
            Check 'W1 C4 Windows failure visible' (($script:trustLog -join "`n") -match 'folder trust not declared.*exit') 'native failure was not reported'
        }
        $backendExe = Join-Path $root 'missing-sotd.exe'
        Invoke-TrustFixture
        Check 'W1 C4 Windows missing binary failure visible' (($script:trustLog -join "`n") -match 'folder trust not declared') 'missing binary was quiet'
        $backendExe = ''
        New-Item -ItemType Directory -Force -Path (Join-Path $prefixDir 'bin') | Out-Null
        Copy-Item -LiteralPath $SotdPath -Destination (Join-Path $prefixDir 'bin\sotd.exe')
        $kernel = Join-Path $repoCurrent 'julia\kernel'
        New-Item -ItemType Directory -Force -Path $kernel | Out-Null
        [System.IO.File]::WriteAllText((Join-Path $kernel 'Manifest.toml'), '# fixture')
        [System.IO.File]::WriteAllBytes($f, $headerless)
        Invoke-TrustFixture
        Check 'W1 C4 Windows staged binary choice' (($script:trustLog -join "`n") -match 'folder trust declared') 'staged owner was not invoked'
        $old = Join-Path $root 'older-sotd.exe'
        Add-Type -TypeDefinition 'public class W1OldBinary { public static int Main(string[] args) { System.Console.Error.WriteLine("unknown subcommand trust"); return 64; } }' -OutputAssembly $old -OutputType ConsoleApplication
        $backendExe = $old
        $before = [System.IO.File]::ReadAllBytes($f)
        Invoke-TrustFixture
        Check 'W1 C4 Windows older binary failure visible' (($script:trustLog -join "`n") -match 'folder trust not declared \(exit 64\)') 'older binary failure was not reported'
        Check 'W1 C4 Windows older binary bytes unchanged' (Test-SameBytes $before ([System.IO.File]::ReadAllBytes($f))) 'older binary caused a declaration'
        Check 'W1 C4 Windows caller restores error preference' ($ErrorActionPreference -ceq 'Stop') 'error preference changed'
        if ($fail -eq 0) { Write-Host 'W1 C4 Windows delegation PASS: real owner; dev/staged choices; preservation; encoding; visible compatibility failures' }
    } catch { Check 'W1 C4 Windows section ran' $false $_.Exception.Message }
    finally {
        foreach ($key in $trustKeys) { [Environment]::SetEnvironmentVariable($key, $savedTrustEnv[$key], 'Process') }
        foreach ($key in $savedSot.Keys) { [Environment]::SetEnvironmentVariable($key, $savedSot[$key], 'Process') }
    }

} finally {
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "`n================ $pass passed, $fail failed ================" -ForegroundColor $(if ($fail) { 'Red' } else { 'Green' })
if ($fail) { exit 1 }
exit 0

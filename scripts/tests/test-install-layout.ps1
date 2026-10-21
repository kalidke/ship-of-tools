# test-install-layout.ps1 -- regression harness for scripts/sot-install-
# layout.ps1's Test-SotPinnedCheckout and Get-SotLauncherTarget: the one
# predicate that replaces the four disagreeing dev-vs-release heuristics
# launch-sot.ps1 used to have (docs/history/adr/0030-versioning-release-and-auto-
# update.md's 2026-09-17 amendment). Test-SotPinnedCheckout is a path
# identity (is this script running FROM repo\current), not a git-state
# check, so no git fixture is needed here -- CI's ParseFile gate
# (.github/workflows/rust.yml) already syntax-covers launch-sot.ps1 and
# install-shortcut.ps1, so this file's own Section 0 only re-parses ITSELF
# and the files it dot-sources.
#
# Run under WINDOWS POWERSHELL 5.1 (see the same note in test-sot-apply.ps1
# and sot-install-layout.ps1's own header -- BOM-less .ps1, ASCII-only
# string literals).
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\tests\test-install-layout.ps1
#
# Section 7 (test-install-entry.ps1) runs scripts\install.ps1, the bootstrap, with its network replaced, in a scratch profile.
# Section 5 runs sot-freshness.ps1's Invoke-FreshnessPass for its comm decision
# (a git fixture there: the stamp is compared with the checkout's commit).

param([string]$SotdPath)

# Runs in a scratch profile of its own and never in the caller's (scratch-guard.ps1 says how); Stop, so a missing guard
# file ends the suite rather than letting it run.
$ErrorActionPreference = 'Stop'
$sotSuite = $PSCommandPath; $sotSuiteArgs = $PSBoundParameters
. (Join-Path $PSScriptRoot 'scratch-guard.ps1')
if ($null -ne $sotScratchExit) { exit $sotScratchExit }

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
            (Join-Path $repoRoot 'scripts\sot-freshness.ps1'),
            (Join-Path $repoRoot 'scripts\tests\test-install-layout.ps1'),
            (Join-Path $repoRoot 'scripts\install.ps1'),
            (Join-Path $repoRoot 'scripts\tests\test-install-entry.ps1')
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

    # An 8.3-spelled prefix (a hosted runner's TEMP) against the long spelling a script's own path reports.
    try {
        $shortPrefix = ([string](New-Object -ComObject Scripting.FileSystemObject).GetFolder($prefix).ShortPath).TrimEnd('\')
        if ($shortPrefix -ieq $prefix) { Write-Host '  SKIP  1: this volume creates no 8.3 names' -ForegroundColor Yellow }
        else {
            Check 'repo\current under the long spelling is pinned for the prefix spelled 8.3' `
                (Test-SotPinnedCheckout -RepoPath $repoCurrentDir -Prefix $shortPrefix) 'the same folder, spelled two ways, reported NOT pinned'
            Check 'the 8.3 spelling of a folder and its long spelling are one comparable path' `
                ((Get-SotComparablePath (Join-Path $shortPrefix 'repo\current')) -ieq (Get-SotComparablePath $repoCurrentDir)) 'the two spellings differ'
            Check 'a sibling is still not pinned under the 8.3 prefix' `
                (-not (Test-SotPinnedCheckout -RepoPath (Join-Path $root 'other\repo\current') -Prefix $shortPrefix)) 'a different prefix''s repo\current reported pinned'
        }
        Check 'a path that is not there compares as written (trailing separator trimmed)' `
            ((Get-SotComparablePath (Join-Path $root 'no\such\place\')) -ceq (Join-Path $root 'no\such\place')) 'the missing path was changed'
    } catch { Check '1: the 8.3 checks ran' $false $_.Exception.Message }

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

    Write-Host "`n=== 5. Invoke-FreshnessPass: comm follows the checkout's commit by its stamp ===" -ForegroundColor Cyan
    # A Windows field box, 2026-10-16: an apply made outside the launcher left ~/.sot-comm at the previous tag's
    # scripts, because the pass ran update_comm only when THIS launch had applied. The stamp update_comm writes last
    # (VERSION, the short commit with -dirty for a changed tree) decides now. update_comm, the instantiate and the log
    # are stand-ins.
    $savedComm5 = $env:SOT_COMM_HOME
    $savedEAP5 = $ErrorActionPreference
    try {
        . (Join-Path $repoRoot 'scripts\sot-freshness.ps1')
        function Initialize-InstallLayout { }
        function Invoke-CommUpdate { $script:comm5 += @('update') }
        function Write-SupLog([string]$Message) { $script:comm5 += @($Message) }
        $co5 = Join-Path $root 'co5'
        New-Item -ItemType Directory -Force -Path $co5 | Out-Null
        $ErrorActionPreference = 'Continue'
        git -C $co5 init -q 2>&1 | Out-Null
        git -C $co5 -c user.name=t -c user.email=t@t commit -q --allow-empty -m c 2>&1 | Out-Null
        $sha5 = "$(git -C $co5 rev-parse --short=9 HEAD 2>$null)".Trim()
        $ErrorActionPreference = $savedEAP5
        Check '5: the fixture checkout has a commit' ($sha5.Length -ge 9) "got '$sha5'"
        $repo = Resolve-Path $co5
        $env:SOT_COMM_HOME = Join-Path $root 'comm5'
        New-Item -ItemType Directory -Force -Path (Join-Path $env:SOT_COMM_HOME 'bin') | Out-Null
        $script:sotPinned = $true
        $NoUpdate = $false
        $Local = $false
        foreach ($row in @(@('b9766d678-dirty', 1), @("$sha5-dirty", 0), @($sha5, 0))) {
            Set-Content -LiteralPath (Join-Path $env:SOT_COMM_HOME 'VERSION') -Value $row[0] -NoNewline
            $script:comm5 = @()
            Invoke-FreshnessPass
            $ran5 = @($script:comm5 | Where-Object { $_ -ceq 'update' }).Count
            Check "5: a stamp of $($row[0]) at checkout $sha5 runs update_comm $($row[1]) time(s)" ($ran5 -eq $row[1]) "log: $($script:comm5 -join ' | ')"
        }
    } catch { Check '5: section ran' $false $_.Exception.Message }
    finally {
        $ErrorActionPreference = $savedEAP5
        $env:SOT_COMM_HOME = $savedComm5
    }

    Write-Host "`n=== 5b. a dev launch decides comm by the same stamp: a matching stamp starts no Julia, even after a pull ===" -ForegroundColor Cyan
    $savedComm5b = $env:SOT_COMM_HOME
    $savedRebuild5b = $env:SOT_LAUNCH_REBUILD
    $savedEAP5b = $ErrorActionPreference
    try {
        . (Join-Path $repoRoot 'scripts\sot-freshness.ps1')
        function Initialize-InstallLayout { $script:comm5 += @('layout') }
        function Invoke-CommUpdate { $script:comm5 += @('update') }
        function Write-SupLog([string]$Message) { $script:comm5 += @($Message) }
        function Set-LaunchStatus([string]$Message) { }
        # The dev branch's rebuild needs a toolchain: a box without cargo takes its "nothing to rebuild" line.
        function Get-Command { param([string]$Name, $ErrorAction) if ($Name -eq 'cargo') { return $null }; Microsoft.PowerShell.Core\Get-Command -Name $Name -ErrorAction SilentlyContinue }
        $co5b = Join-Path $root 'co5b'
        New-Item -ItemType Directory -Force -Path $co5b | Out-Null
        $ErrorActionPreference = 'Continue'
        git -C $co5b init -q 2>&1 | Out-Null
        git -C $co5b -c user.name=t -c user.email=t@t commit -q --allow-empty -m c 2>&1 | Out-Null
        $sha5b = "$(git -C $co5b rev-parse --short=9 HEAD 2>$null)".Trim()
        $ErrorActionPreference = $savedEAP5b
        $repo = Resolve-Path $co5b
        $env:SOT_COMM_HOME = Join-Path $root 'comm5b'
        New-Item -ItemType Directory -Force -Path (Join-Path $env:SOT_COMM_HOME 'bin') | Out-Null
        $script:sotPinned = $false
        $NoUpdate = $false
        $Local = $false
        foreach ($rebuild in @('', '1')) {
            foreach ($row in @(@('b9766d678', 1), @("$sha5b-dirty", 0), @($sha5b, 0))) {
                Set-Content -LiteralPath (Join-Path $env:SOT_COMM_HOME 'VERSION') -Value $row[0] -NoNewline
                if ($rebuild) { $env:SOT_LAUNCH_REBUILD = $rebuild } else { Remove-Item Env:\SOT_LAUNCH_REBUILD -ErrorAction SilentlyContinue }
                $script:comm5 = @()
                Invoke-FreshnessPass
                $ran5b = @($script:comm5 | Where-Object { $_ -ceq 'update' }).Count
                Check "5b: a dev launch (after a pull: '$rebuild') with a stamp of $($row[0]) at checkout $sha5b runs update_comm $($row[1]) time(s)" ($ran5b -eq $row[1]) "log: $($script:comm5 -join ' | ')"
                Check "5b: a dev launch never instantiates the checkout's Julia environments" (@($script:comm5 | Where-Object { $_ -ceq 'layout' }).Count -eq 0) "log: $($script:comm5 -join ' | ')"
            }
        }
    } catch { Check '5b: section ran' $false $_.Exception.Message }
    finally {
        $ErrorActionPreference = $savedEAP5b
        $env:SOT_COMM_HOME = $savedComm5b
        if ($null -eq $savedRebuild5b) { Remove-Item Env:\SOT_LAUNCH_REBUILD -ErrorAction SilentlyContinue } else { $env:SOT_LAUNCH_REBUILD = $savedRebuild5b }
        Remove-Item function:Get-Command -ErrorAction SilentlyContinue
    }

    Write-Host "`n=== 5c. Invoke-CommUpdate: a warning block of a successful update_comm (the closing summary) reaches the log ===" -ForegroundColor Cyan
    # The install's output is Write-SupLog's lines, so a successful update_comm that warns (a settings file left without
    # the hooks) must log that block; its info lines and a run with no warning add nothing. julia and its run are stand-ins.
    $savedEAP5c = $ErrorActionPreference
    $origNative5c = ${function:Invoke-SotNative}
    try {
        . (Join-Path $repoRoot 'scripts\sot-freshness.ps1')
        function Write-SupLog([string]$Message) { $script:comm5c += @($Message) }
        function Get-Command { param([string]$Name, $ErrorAction) if ($Name -eq 'julia') { return 'julia' }; Microsoft.PowerShell.Core\Get-Command -Name $Name -ErrorAction SilentlyContinue }
        function Invoke-SotNative { param($Exe, $ArgList) [pscustomobject]@{ Code = 0; All = $script:out5c } }
        $repo = [pscustomobject]@{ Path = $root }
        $script:launchNotices = New-Object System.Collections.ArrayList
        # A Julia warning block starts at its "Warning:" line and ends at its "@ Module file:line" line.
        $warn5c = @('+ Warning: The comm hooks are NOT in these Claude settings:', '|   C:\acct\settings.json', '+ @ ShipTools install.jl:80')
        foreach ($case in @(
                @('a warning block', (@('[ Info: Installed comm scripts') + $warn5c + @('[ Info: sot-comm ready')), 2),
                @('no warning', @('[ Info: sot-comm ready'), 0))) {
            $script:out5c = $case[1]
            $script:comm5c = @()
            Invoke-CommUpdate
            $got = @($script:comm5c | Where-Object { $_ -like '*settings.json*' -or $_ -like '*NOT in these*' }).Count
            Check "5c: $($case[0]): the summary lines are logged" ($got -eq $case[2]) "log: $($script:comm5c -join ' | ')"
            Check "5c: $($case[0]): info lines are not" (@($script:comm5c | Where-Object { $_ -like '*Info*' }).Count -eq 0) "log: $($script:comm5c -join ' | ')"
        }
    } catch { Check '5c: section ran' $false $_.Exception.Message }
    finally {
        $ErrorActionPreference = $savedEAP5c
        Remove-Item function:Get-Command -ErrorAction SilentlyContinue
        Set-Item function:Invoke-SotNative $origNative5c
    }

    Write-Host "`n=== 6. Invoke-PendingApply: a pointer the installed sotd refuses costs no daemon stop ===" -ForegroundColor Cyan
    # UPDATER-ARMS-INSTALLED: the daemon is stopped before an apply only when the apply will happen. The prefix holds the
    # real sotd (it answers `update pointer`), an install at 0.7.0 and a pointer; the local-daemon and apply scripts are
    # stand-ins that record that they ran. A refused pointer runs the apply drop-only.
    $savedEAP6 = $ErrorActionPreference
    try {
        . (Join-Path $repoRoot 'scripts\sot-freshness.ps1')
        function Write-SupLog([string]$Message) { }
        function Set-LaunchStatus([string]$Message) { }
        $prefixDir = Join-Path $root 'prefix6'
        $updates6 = Join-Path $prefixDir 'updates'
        New-Item -ItemType Directory -Force -Path (Join-Path $prefixDir 'bin'), $updates6 | Out-Null
        Copy-Item -LiteralPath $SotdPath -Destination (Join-Path $prefixDir 'bin\sotd.exe')
        $utf8 = New-Object System.Text.UTF8Encoding($false)
        [System.IO.File]::WriteAllText((Join-Path $prefixDir 'install.json'), '{"schema":1,"prefix":"x","version":"0.7.0","commit":"aaa"}', $utf8)
        $calls6 = Join-Path $root 'calls6.txt'
        $sotLocalDaemon = Join-Path $root 'daemon6.ps1'
        Set-Content -LiteralPath $sotLocalDaemon -Value "Add-Content -LiteralPath '$calls6' -Value 'stop'"
        $sotApply = Join-Path $root 'apply6.ps1'
        Set-Content -LiteralPath $sotApply -Value "param([switch]`$DropOnly); Add-Content -LiteralPath '$calls6' -Value `$(if (`$DropOnly) { 'drop' } else { 'apply' })"
        $applyMarker = Join-Path $updates6 'just-applied-windows-x86_64'
        $NoUpdate = $false
        $ptrPath6 = Join-Path $updates6 'pending-windows-x86_64.json'
        function New-Pointer6([string]$version) {
            '{"schema":1,"repo":"kalidke/ship-of-tools","tag":"v' + $version + '","version":"' + $version + '","target":"windows-x86_64","asset":"sot-' + $version + '-windows-x86_64.zip","asset_sha256":"' + ('ab' * 32) + '","checkout":"C:\\x","commit":"cc","armed_at":1}'
        }
        # Every Rust writer records its prepare in the stage before it arms, and the sotd refuses a pointer whose stage
        # has no record: the pointer above the install gets one (the other two are refused before the record is read).
        $stage6 = Join-Path $updates6 'v0.8.0-windows-x86_64'
        New-Item -ItemType Directory -Force -Path $stage6 | Out-Null
        [System.IO.File]::WriteAllText((Join-Path $stage6 'prepared.json'), ((New-Pointer6 '0.8.0') -replace '"armed_at":1', '"julia_instantiated":false,"mathjax_deps":false,"prepared_at":1'), $utf8)
        $cases6 = @(
            @{ name = 'a pointer above the install'; text = (New-Pointer6 '0.8.0'); want = 'stop apply' },
            @{ name = 'a pointer below the install'; text = (New-Pointer6 '0.6.0'); want = 'drop' },
            @{ name = 'a pointer that cannot be read'; text = '{not json'; want = 'drop' }
        )
        foreach ($c in $cases6) {
            [System.IO.File]::WriteAllText($ptrPath6, $c.text, $utf8)
            Remove-Item -LiteralPath $calls6 -Force -ErrorAction SilentlyContinue
            Invoke-PendingApply
            $got = if (Test-Path -LiteralPath $calls6) { (@(Get-Content -LiteralPath $calls6) -join ' ') } else { '' }
            Check "6: $($c.name) runs '$($c.want)'" ($got -ceq $c.want) "ran: '$got'"
        }
    } catch { Check '6: section ran' $false $_.Exception.Message }
    finally { $ErrorActionPreference = $savedEAP6 }

    # Section 7, install.ps1 (the Windows bootstrap), lives in test-install-entry.ps1, dot-sourced here in this scope.
    . (Join-Path $PSScriptRoot 'test-install-entry.ps1')

} finally {
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "`n================ $pass passed, $fail failed ================" -ForegroundColor $(if ($fail) { 'Red' } else { 'Green' })
if ($fail) { exit 1 }
exit 0

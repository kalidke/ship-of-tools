# install-smoke.ps1 -- install.ps1 against the artifacts a release run built (Windows): release.yml's Windows install jobs
# call it, and it runs off CI the same way. Windows PowerShell 5.1, ASCII only.
#
#   install-smoke.ps1 -Scenario fresh|update-entry|update-apply -Artifacts <folder> -Checkout <folder> [-Before <tag>]
#
# Artifacts holds the run's sot-<version>-windows-x86_64.zip; Checkout is the tree whose scripts\install.ps1 is the entry
# under test. Scenarios, as install-smoke.sh's:
#   fresh         install.ps1 -Archive <pack>: the PASS line, then a repair run
#   update-entry  -Before installed first, then install.ps1 -Archive <pack> (PASS), then a repair run
#   update-apply  -Before installed, the run's release staged and armed by its own sotd, the installed sot-apply.ps1 run
#                 as the launcher's start runs it: the staged release's own apply must run and leave the launcher code
#                 and install.json at the new release; then the entry's repair run (it also brings comm to the commit)
# The pack -Archive takes is made here from the artifacts: SHA256SUMS, COMMIT and the zip, which keeps its name when that
# carries the binaries' own version and is repacked under it otherwise (a branch run names its artifacts 0.0.0-ci.<sha>).
# -Before is installed by its own installer when its tag carries scripts\install.ps1 (the entry's prelude fetches it);
# until a published release does, by that tag's docs\INSTALL-AGENT.md section 2b: the zip's three binaries into
# bin\, a clone at the tag with its install-shortcut.ps1, and the launcher's first-start layout run by the tag's own
# sot-install-layout.ps1 (no window can start here). Delete that branch (Install-BeforeByHand) once the release before
# carries install.ps1.
#
# One scratch profile (USERPROFILE, APPDATA, LOCALAPPDATA, SOT_COMM_HOME) and a USERNAME of its own hold everything.
# The verdict of each entry run is judged: PASS, or, in a session with no desk (session 0), the entry's own "start Ship
# of Tools at the desk" step, after which this script applies the armed update as that start does.
# Environment: SOT_SMOKE_HOME (the scratch profile folder; default a fresh one under TEMP), SOT_SMOKE_REPO (owner/name;
# default GITHUB_REPOSITORY), SOT_SMOKE_COMMIT (default HEAD of Checkout), SOT_SMOKE_KEEP=1 (keep the scratch folders),
# SOT_SMOKE_RAW_BASE and SOT_SMOKE_RELEASE_BASE (URL bases for a tag's files and a tag's release assets; default GitHub's).
param(
    [Parameter(Mandatory = $true)][ValidateSet('fresh', 'update-entry', 'update-apply')][string]$Scenario,
    [Parameter(Mandatory = $true)][string]$Artifacts,
    [Parameter(Mandatory = $true)][string]$Checkout,
    [string]$Before = ''
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

function Say([string]$Text) { Write-Host "install-smoke: $Text" }
function Stop-Smoke([string]$Why) { [Console]::Error.WriteLine("install-smoke: FAIL: $Why"); $script:Failed = $true; throw 'install-smoke-stop' }
function Expect-Eq([string]$What, [string]$Got, [string]$Want) { if ($Got -cne $Want) { Stop-Smoke "$What is '$Got', not '$Want'" } }

$script:Failed = $false
$script:Smoke = ''
$script:Rc = 0
$script:Last = ''
$script:Out = @()

function Remove-Scratch([string]$Path) {
    if (-not $Path -or -not (Test-Path -LiteralPath $Path)) { return }
    # Junctions first, link only: a recursive delete must never walk into a version folder through repo\current.
    foreach ($r in @(Get-ChildItem -LiteralPath $Path -Recurse -Force -Attributes ReparsePoint -ErrorAction SilentlyContinue)) {
        cmd /c rmdir "`"$($r.FullName)`"" 2>&1 | Out-Null
    }
    cmd /c rmdir /s /q "`"$Path`"" 2>&1 | Out-Null
}

function Get-JsonString([string]$Path, [string]$Key) {
    try {
        $t = [System.IO.File]::ReadAllText($Path)
        if ($t -match ('"' + $Key + '"\s*:\s*"([^"]*)"')) { return $Matches[1] }
    } catch { }
    return ''
}
function Write-Lf([string]$Path, [string]$Text) { [System.IO.File]::WriteAllText($Path, $Text, (New-Object System.Text.UTF8Encoding($false))) }
function Get-Sha([string]$Path) { return (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant() }

# A command's lines (both streams, in order) and exit code, without 5.1 turning native stderr into an error.
function Invoke-Logged([string]$Label, [string]$Exe, [string[]]$ArgList) {
    Say "--- ${Label}: $Exe $($ArgList -join ' ')"
    $eap = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    $lines = New-Object System.Collections.ArrayList
    try {
        & $Exe @ArgList 2>&1 | ForEach-Object { $l = "$_"; Write-Host $l; [void]$lines.Add($l) }
        $script:Rc = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $eap
    }
    if ($null -eq $script:Rc) { $script:Rc = 0 }
    $script:Out = @($lines)
    $nonEmpty = @($script:Out | Where-Object { $_.Trim() })
    $script:Last = $(if ($nonEmpty.Count) { $nonEmpty[-1] } else { '' })
}
function Test-OutHas([string]$Pattern) { return (@($script:Out | Where-Object { $_ -match $Pattern }).Count -gt 0) }

# ---- the scratch profile ---------------------------------------------------------------------------------------------
$script:Checkout = (Resolve-Path -LiteralPath $Checkout).Path
$script:Entry = Join-Path $script:Checkout 'scripts\install.ps1'
if (-not (Test-Path -LiteralPath $script:Entry)) { throw "$script:Checkout holds no scripts\install.ps1" }
if ($Scenario -ne 'fresh' -and -not $Before) { throw "$Scenario needs -Before (scripts/tests/release-before.sh names it)" }

# Nothing of the caller's own reaches the install: its SOT_ variables (a pinned tag, an origin) would send it elsewhere.
# SOT_JULIA_BIN stays: a box with no julia on PATH names its julia there.
foreach ($e in @(Get-ChildItem env: | Where-Object { ($_.Name -like 'SOT_*' -and $_.Name -notlike 'SOT_SMOKE_*' -and $_.Name -ne 'SOT_JULIA_BIN') -or $_.Name -in @('JULIA_LOAD_PATH', 'JULIA_PROJECT') })) {
    Remove-Item ("env:" + $e.Name)
}
$tmpBase = $(if ($env:TEMP) { $env:TEMP } else { [System.IO.Path]::GetTempPath() })
$script:Smoke = Join-Path $tmpBase ('sot-smoke-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Force -Path $script:Smoke | Out-Null
$h = $(if ($env:SOT_SMOKE_HOME) { $env:SOT_SMOKE_HOME } else { Join-Path $script:Smoke 'home' })
foreach ($d in @($h, "$h\Desktop", "$h\AppData\Roaming", "$h\AppData\Local", "$h\comm")) { New-Item -ItemType Directory -Force -Path $d | Out-Null }
$h = (Resolve-Path -LiteralPath $h).Path
$env:USERNAME = 'sot-scratch-smoke'
$env:USERPROFILE = $h; $env:HOME = $h
$env:APPDATA = "$h\AppData\Roaming"; $env:LOCALAPPDATA = "$h\AppData\Local"
$env:SOT_COMM_HOME = "$h\comm"
# The profile above is this suite's own scratch one; refuse if it is not (scratch-guard.ps1), e.g. SOT_SMOKE_HOME outside TEMP.
. (Join-Path $PSScriptRoot 'scratch-guard.ps1')
$prefix = Join-Path $env:LOCALAPPDATA 'sot'
$repoName = $(if ($env:SOT_SMOKE_REPO) { $env:SOT_SMOKE_REPO } elseif ($env:GITHUB_REPOSITORY) { $env:GITHUB_REPOSITORY } else { 'kalidke/ship-of-tools' })
$env:SOT_INSTALL_REPO = $repoName
$rawBase = $(if ($env:SOT_SMOKE_RAW_BASE) { $env:SOT_SMOKE_RAW_BASE } else { "https://raw.githubusercontent.com/$repoName" })
$releaseBase = $(if ($env:SOT_SMOKE_RELEASE_BASE) { $env:SOT_SMOKE_RELEASE_BASE } else { "https://github.com/$repoName/releases/download" })

function Invoke-Entry([string]$Label, [string[]]$ArgList) {
    Invoke-Logged $Label 'powershell.exe' (@('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $script:Entry) + $ArgList)
}

# ---- the pack -Archive takes, made from the artifacts ----------------------------------------------------------------
function New-Pack {
    $zips = @(Get-ChildItem -LiteralPath $Artifacts -Filter 'sot-*-windows-x86_64.zip' -File)
    if ($zips.Count -ne 1) { Stop-Smoke "$Artifacts must hold exactly one sot-<version>-windows-x86_64.zip (it holds $($zips.Count))" }
    $x = Join-Path $script:Smoke 'x'
    Expand-Archive -LiteralPath $zips[0].FullName -DestinationPath $x -Force
    $tops = @(Get-ChildItem -LiteralPath $x -Directory | Where-Object { $_.Name -like 'sot-*-windows-x86_64' })
    if ($tops.Count -ne 1) { Stop-Smoke "$($zips[0].Name) must hold exactly one sot-<version>-windows-x86_64 folder" }
    $top = $tops[0].Name
    foreach ($e in @('sot.exe', 'sotd.exe', 'sot-capsule.exe')) {
        if (-not (Test-Path -LiteralPath (Join-Path $tops[0].FullName $e))) { Stop-Smoke "$($zips[0].Name) holds no $e" }
    }
    $commit = $(if ($env:SOT_SMOKE_COMMIT) { $env:SOT_SMOKE_COMMIT } else { ((git -C $script:Checkout rev-parse HEAD) | Select-Object -First 1).Trim() })
    if ($commit -notmatch '^[0-9a-f]{40}$') { Stop-Smoke "the commit '$commit' is not a 40-digit id" }
    $sotd = Join-Path $tops[0].FullName 'sotd.exe'
    $line = (& $sotd --version | Select-Object -First 1)
    if ("$line" -notmatch '^sotd (\S+) \(([0-9a-f]+) ') { Stop-Smoke "$top\sotd.exe says '$line'" }
    $ver = $Matches[1]; $sha = $Matches[2]
    if (-not $commit.StartsWith($sha)) { Stop-Smoke "$top\sotd.exe says commit $sha, which is not the commit of $script:Checkout ($($commit.Substring(0, 9)))" }
    $name = "sot-$ver-windows-x86_64"
    $pack = Join-Path $script:Smoke 'pack'
    New-Item -ItemType Directory -Force -Path $pack | Out-Null
    if ($top -ceq $name) {
        Copy-Item -LiteralPath $zips[0].FullName -Destination (Join-Path $pack "$name.zip")
    } else {
        Say "repacking $top as $name (the binaries' version)"
        Rename-Item -LiteralPath $tops[0].FullName -NewName $name
        # Entries are written with / as release.yml's pwsh does: Windows PowerShell 5.1's Compress-Archive writes \, which the updater refuses.
        Add-Type -AssemblyName System.IO.Compression
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $zip = Join-Path $pack "$name.zip"
        $archive = New-Object System.IO.Compression.ZipArchive([System.IO.File]::Create($zip), [System.IO.Compression.ZipArchiveMode]::Create)
        try {
            foreach ($f in Get-ChildItem -LiteralPath (Join-Path $x $name) -File) {
                [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, $f.FullName, "$name/$($f.Name)")
            }
        } finally { $archive.Dispose() }
    }
    Write-Lf (Join-Path $pack 'COMMIT') "$commit`n"
    Write-Lf (Join-Path $pack 'SHA256SUMS') ((@("$name.zip", 'COMMIT') | ForEach-Object { "$(Get-Sha (Join-Path $pack $_))  $_`n" }) -join '')
    Say "pack of $ver at $($commit.Substring(0, 9)) in $pack"
    return [pscustomobject]@{ Dir = $pack; Version = $ver; Commit = $commit; Short = $commit.Substring(0, 9); Name = $name; Sotd = (Join-Path (Join-Path $x $name) 'sotd.exe') }
}

# ---- judging a verdict ------------------------------------------------------------------------------------------------
function Test-Verdict([string]$Label, $Pack, [string]$Ver = '', [string]$Short = '') {
    if (-not $Ver) { $Ver = $Pack.Version; $Short = $Pack.Short }
    $want = "PASS: Ship of Tools $Ver at $Short on this computer"
    if ($script:Last.StartsWith($want, [System.StringComparison]::Ordinal)) {
        if ($script:Rc -ne 0) { Stop-Smoke "${Label}: PASS with exit $($script:Rc)" }
        Say "${Label}: $($script:Last -replace ';.*$', '')"
        return 'pass'
    }
    if ($script:Last.StartsWith('ONE STEP LEFT: ')) {
        $step = $script:Last.Substring('ONE STEP LEFT: '.Length)
        if ($step.StartsWith('start Ship of Tools at the desk;') -and ([System.Diagnostics.Process]::GetCurrentProcess().SessionId -eq 0)) {
            Say "${Label}: this runner's session is 0, which has no desk: the update stays armed by design ($step)"
            return 'desk'
        }
        Stop-Smoke "${Label}: ONE STEP LEFT that this scenario does not allow: $step"
    }
    Stop-Smoke "${Label}: the last line is '$($script:Last)' (exit $($script:Rc)), not PASS for $Ver at $Short"
}

# ---- the release before, laid by its own installer ---------------------------------------------------------------------
# 5.1 turns a native command's stderr into a terminating error under 'Stop': curl says why it failed there.
function Test-RemoteFile([string]$Url) {
    $eap = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
    try { & curl.exe --connect-timeout 15 -fsSI -o NUL $Url 2>&1 | Out-Null; $rc = $LASTEXITCODE } finally { $ErrorActionPreference = $eap }
    # 22 is an HTTP error (the file is absent) and 37 a file:// base that lacks it; any other failure is the network.
    if ($rc -ne 0 -and $rc -ne 22 -and $rc -ne 37) { Stop-Smoke "could not ask whether $Url exists (curl exit $rc)" }
    return ($rc -eq 0)
}
function Get-ByUrl([string]$Url, [string]$Path) {
    $eap = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
    try { & curl.exe --connect-timeout 15 --speed-limit 1000 --speed-time 60 -fsSL -o $Path $Url 2>&1 | ForEach-Object { Write-Host "$_" }; $rc = $LASTEXITCODE } finally { $ErrorActionPreference = $eap }
    if ($rc -ne 0) { Stop-Smoke "could not download $Url (curl exit $rc)" }
}
function Install-BeforeByHand([string]$Tag) {
    $vb = $Tag.TrimStart('v')
    $dl = Join-Path $script:Smoke 'before'
    New-Item -ItemType Directory -Force -Path $dl | Out-Null
    # Section 2b step 1: download, verify, the three binaries into bin\.
    foreach ($f in @('SHA256SUMS', "sot-$vb-windows-x86_64.zip")) { Get-ByUrl "$releaseBase/$Tag/$f" (Join-Path $dl $f) }
    $sums = @{}
    foreach ($l in [System.IO.File]::ReadAllLines((Join-Path $dl 'SHA256SUMS'))) { if ($l -match '^([0-9a-fA-F]{64}) [ *](.+)$') { $sums[$Matches[2]] = $Matches[1].ToLowerInvariant() } }
    $zipName = "sot-$vb-windows-x86_64.zip"
    if ($sums[$zipName] -ne (Get-Sha (Join-Path $dl $zipName))) { Stop-Smoke "$zipName does not match its SHA256SUMS entry" }
    Expand-Archive -LiteralPath (Join-Path $dl $zipName) -DestinationPath (Join-Path $dl 'x') -Force
    New-Item -ItemType Directory -Force -Path (Join-Path $prefix 'bin') | Out-Null
    foreach ($e in @('sot.exe', 'sotd.exe', 'sot-capsule.exe')) { Copy-Item -LiteralPath (Join-Path $dl "x\sot-$vb-windows-x86_64\$e") -Destination (Join-Path $prefix "bin\$e") -Force }
    # Step 5: a clone at the tag, and that tag's own install-shortcut.ps1 (it writes the shortcut and install.json).
    $clone = Join-Path $h 'ship-of-tools'
    $eap = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
    & git clone --quiet --filter=blob:none --branch $Tag "https://github.com/$repoName" $clone 2>&1 | ForEach-Object { Write-Host "$_" }
    $cloneRc = $LASTEXITCODE
    $ErrorActionPreference = $eap
    if ($cloneRc -ne 0) { Stop-Smoke "git clone of $repoName at $Tag ended with exit $cloneRc" }
    Push-Location $clone
    try { Invoke-Logged 'before-shortcut' 'powershell.exe' @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', 'scripts\install-shortcut.ps1') } finally { Pop-Location }
    if ($script:Rc -ne 0) { Stop-Smoke "$Tag's install-shortcut.ps1 ended with exit $($script:Rc)" }
    # The launcher's first start finishes the layout (repo\versions, the repo\current junction, the Julia environments,
    # comm). No window can start here, so the tag's own functions run in its place, as its launcher calls them.
    $layout = {
        param($prefixDir, $repo)
        $ErrorActionPreference = 'Stop'
        $script:launchNotices = New-Object System.Collections.ArrayList
        $logDir = Join-Path $env:LOCALAPPDATA 'sot\logs'; New-Item -ItemType Directory -Force -Path $logDir | Out-Null
        function Write-SupLog { param([string]$Message) Write-Host "  $Message"; "$(Get-Date -Format o)  pid=$PID  install-smoke: $Message" | Out-File -FilePath (Join-Path $logDir 'supervisor.log') -Append -Encoding utf8 }
        function Set-LaunchStatus { param([string]$Message) Write-Host "  $Message" }
        function Limit-NoticeText { param($t) $t }
        $repoCurrent = Join-Path $prefixDir 'repo\current'
        $backendExe = $null; $NoUpdate = $false; $Local = $false
        $sd = Join-Path $repo 'scripts'
        . (Join-Path $sd 'sot-install-layout.ps1')
        . (Join-Path $sd 'sot-freshness.ps1')
        # The launcher relaxes 'Stop' around native tools: Julia writes to stderr.
        $ErrorActionPreference = 'Continue'
        Initialize-InstallLayout
        $repo = [pscustomobject]@{ Path = $repoCurrent }
        if (Get-Command Invoke-FreshnessPass -ErrorAction SilentlyContinue) { Invoke-FreshnessPass } else { Invoke-CommUpdate }
        foreach ($n in @($script:launchNotices)) { Write-Host "  note: $n" }
    }
    & $layout $prefix $clone
    Expect-Eq 'before: install.json version' (Get-JsonString (Join-Path $prefix 'install.json') 'version') $vb
}
function Install-Before([string]$Tag) {
    $vb = $Tag.TrimStart('v')
    if (Test-RemoteFile "$rawBase/$Tag/scripts/install.ps1") {
        Say "before: $Tag carries scripts\install.ps1; the entry's prelude fetches it"
        Invoke-Entry 'before' @('-Version', $Tag)
        if ($script:Rc -ne 0) { Stop-Smoke "before: $Tag's installer ended with exit $($script:Rc); its last line: $($script:Last)" }
        Expect-Eq 'before: install.json version' (Get-JsonString (Join-Path $prefix 'install.json') 'version') $vb
    } else {
        Say "before: $Tag carries no scripts\install.ps1; laid by its docs\INSTALL-AGENT.md section 2b"
        Install-BeforeByHand $Tag
    }
    Say "before: $Tag is installed"
}

# ---- what the new release leaves ---------------------------------------------------------------------------------------
function Get-LauncherCode([string]$ScriptsDir) {
    . (Join-Path $ScriptsDir 'sot-install-layout.ps1')
    return (Get-SotLauncherCodeId -ScriptsDir $ScriptsDir)
}
function Test-StateNew([string]$Label, $Pack, [switch]$Comm) {
    $ij = Join-Path $prefix 'install.json'
    Expect-Eq "${Label}: install.json version" (Get-JsonString $ij 'version') $Pack.Version
    Expect-Eq "${Label}: install.json commit" (Get-JsonString $ij 'commit') $Pack.Commit
    $target = ''
    try { $target = [string]@((Get-Item -LiteralPath (Join-Path $prefix 'repo\current') -Force -ErrorAction Stop).Target)[0] } catch { }
    if ($target -notlike "*repo\versions\v$($Pack.Version)") { Stop-Smoke "${Label}: repo\current points at '$target', not repo\versions\v$($Pack.Version)" }
    if ((& (Join-Path $prefix 'bin\sotd.exe') --version | Select-Object -First 1) -notlike "sotd $($Pack.Version) ($($Pack.Short) *") { Stop-Smoke "${Label}: bin\sotd.exe is not $($Pack.Version) at $($Pack.Short)" }
    # The launcher the shortcut runs is this release's own: the id of the installed scripts is the checkout's.
    Expect-Eq "${Label}: the launcher code id of repo\current" (Get-LauncherCode (Join-Path $prefix 'repo\current\scripts')) (Get-LauncherCode (Join-Path $script:Checkout 'scripts'))
    if ($Comm) {
        $stamp = ''
        try { $stamp = ([System.IO.File]::ReadAllText((Join-Path $env:SOT_COMM_HOME 'VERSION'))).Trim() -replace '-dirty$', '' } catch { }
        Expect-Eq "${Label}: the comm stamp" $stamp $Pack.Short
    }
}
function Invoke-RepairRun($Pack) {
    Invoke-Entry 'repair' @('-Archive', $Pack.Dir)
    [void](Test-Verdict 'repair' $Pack)
    if (-not (Test-OutHas 'a repair run')) { Stop-Smoke 'repair: the run did not say it is a repair run' }
    Test-StateNew 'repair' $Pack -Comm
}
# The installed apply, as the launcher's start runs it: the one in repo\current\scripts, which is the release before's.
function Invoke-InstalledApply($Pack) {
    Invoke-Logged 'apply' 'powershell.exe' @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', (Join-Path $prefix 'repo\current\scripts\sot-apply.ps1'), '-Prefix', $prefix)
    if ($script:Rc -ne 0) { Stop-Smoke "apply: the installed sot-apply.ps1 ended with exit $($script:Rc)" }
    if (-not (Test-OutHas 'sot-apply: re-executing the staged sot-apply')) { Stop-Smoke "apply: the installed apply did not hand off to the staged release's own" }
    if (-not (Test-OutHas ('sot-apply: APPLIED v' + [regex]::Escape($Pack.Version)))) { Stop-Smoke "apply: the staged apply did not say APPLIED v$($Pack.Version)" }
    if (Test-Path -LiteralPath (Join-Path $prefix 'updates\pending-windows-x86_64.json')) { Stop-Smoke 'apply: the pointer is still armed' }
}

try {
    # Hosted question 2 (does the runner have a desk) is answered by this line in every leg's log.
    Say "session $([System.Diagnostics.Process]::GetCurrentProcess().SessionId) ($Scenario)"
    $pack = New-Pack
    switch ($Scenario) {
        'fresh' {
            Invoke-Entry 'fresh' @('-Archive', $pack.Dir)
            [void](Test-Verdict 'fresh' $pack)
            # The shortcut step's install-manifest.ps1 call finds the record install.ps1 wrote for a pack of the line and keeps it.
            if ($pack.Version.Contains('+') -and (Test-OutHas 'no manifest written')) { Stop-Smoke 'fresh: the run printed "no manifest written" for a pack of the line, whose record install.ps1 had written' }
            Test-StateNew 'fresh' $pack -Comm
            Invoke-RepairRun $pack
        }
        'update-entry' {
            Install-Before $Before
            Invoke-Entry 'update' @('-Archive', $pack.Dir)
            $how = Test-Verdict 'update' $pack
            if ($how -eq 'desk') {
                if (-not (Test-Path -LiteralPath (Join-Path $prefix 'updates\pending-windows-x86_64.json'))) { Stop-Smoke 'update: the desk step was named, but no update is armed' }
                Invoke-InstalledApply $pack
            }
            Test-StateNew 'update' $pack
            Invoke-RepairRun $pack
        }
        'update-apply' {
            Install-Before $Before
            Invoke-Logged 'stage' $pack.Sotd @('update', 'stage', $pack.Dir, '--prefix', $prefix)
            if ($script:Rc -ne 0) { Stop-Smoke "stage: sotd update stage ended with exit $($script:Rc); its last line: $($script:Last)" }
            if (-not (Test-Path -LiteralPath (Join-Path $prefix 'updates\pending-windows-x86_64.json'))) { Stop-Smoke 'stage: no pointer is armed' }
            Invoke-InstalledApply $pack
            Test-StateNew 'apply' $pack
            Invoke-RepairRun $pack
        }
    }
    Say "PASS ${Scenario}: $($pack.Version) at $($pack.Short) on windows-x86_64"
} catch {
    if ($_.Exception.Message -ne 'install-smoke-stop') {
        [Console]::Error.WriteLine("install-smoke: FAIL: $($_.Exception.Message)")
        $script:Failed = $true
    }
} finally {
    if ($env:SOT_SMOKE_KEEP -ne '1') { Remove-Scratch $script:Smoke }
}
if ($script:Failed) { exit 1 }
exit 0

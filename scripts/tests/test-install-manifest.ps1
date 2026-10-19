# test-install-manifest.ps1 -- regression harness for scripts/install-manifest.ps1's pending-pointer rule
# (UPDATER-ARMS-INSTALLED): writing an install.json for a NEW install (another version or another commit) drops the
# pointer armed over the install it replaces; a rewrite of the SAME version and commit (the launcher's refresh after an
# apply) keeps it; with no pointer nothing breaks.
#
# The script reads its version from `<prefix>\bin\sot.exe --version`, so the fixture builds a stand-in sot.exe with the
# .NET Framework's csc.exe (on every Windows): it prints the first line of version.txt beside it, after a line on stderr
# when warn.txt is beside it. The commit comes from
# a scratch git repo with two tags. No network, no hub: install-manifest.ps1 syncs topology only with -Hub, and case p8 gives it a stand-in sotd.
#
# Runs under Windows PowerShell 5.1 and pwsh 7. ASCII ONLY (see the same note in sot-apply.ps1). Self-ending: every
# step is local and bounded, and the scratch folder is removed at the end.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\tests\test-install-manifest.ps1
#   pwsh -NoProfile -File scripts\tests\test-install-manifest.ps1

# Runs in a scratch profile of its own and never in the caller's (scratch-guard.ps1 says how); Stop, so a missing guard
# file ends the suite rather than letting it run.
$ErrorActionPreference = 'Stop'
$sotSuite = $PSCommandPath; $sotSuiteArgs = $PSBoundParameters
. (Join-Path $PSScriptRoot 'scratch-guard.ps1')
if ($null -ne $sotScratchExit) { exit $sotScratchExit }
# Continue, not Stop: Windows PowerShell 5.1 turns a native command's stderr into a terminating error under Stop, and
# git writes hints there. Every step that must not fail is checked below.
$ErrorActionPreference = 'Continue'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$script = Join-Path $repo 'scripts\install-manifest.ps1'
$root = Join-Path $env:TEMP ("sot-install-manifest-test-" + [guid]::NewGuid().ToString('N').Substring(0,8))
$pass = 0; $fail = 0
function Check([string]$name, [bool]$ok, [string]$detail) {
    if ($ok) { $script:pass++; Write-Host "  PASS  $name" -ForegroundColor Green }
    else { $script:fail++; Write-Host "  FAIL  $name -- $detail" -ForegroundColor Red }
}

New-Item -ItemType Directory -Force -Path $root | Out-Null
try {
    # A stand-in sot.exe: prints the first line of version.txt beside it, as `sot --version` does.
    $csc = Join-Path $env:WINDIR 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
    if (-not (Test-Path -LiteralPath $csc)) { $csc = Join-Path $env:WINDIR 'Microsoft.NET\Framework\v4.0.30319\csc.exe' }
    if (-not (Test-Path -LiteralPath $csc)) {
        Write-Host '  SKIP  no csc.exe to build the stand-in sot.exe' -ForegroundColor Yellow
        return
    }
    $src = Join-Path $root 'stub.cs'
    Set-Content -LiteralPath $src -Encoding ASCII -Value @'
class P {
    static void Main() {
        string d = System.AppDomain.CurrentDomain.BaseDirectory;
        if (System.IO.File.Exists(System.IO.Path.Combine(d, "warn.txt"))) System.Console.Error.WriteLine("warning: stand-in sot warns on stderr");
        System.Console.WriteLine(System.IO.File.ReadAllLines(System.IO.Path.Combine(d, "version.txt"))[0]);
    }
}
'@
    $stub = Join-Path $root 'sot-stub.exe'
    $built = & $csc /nologo /out:$stub $src 2>&1
    if (-not (Test-Path -LiteralPath $stub)) { throw "could not build the stand-in sot.exe: $built" }

    # A scratch clone with two release tags on two commits: v0.6.0 and v0.7.0.
    $clone = Join-Path $root 'clone'
    New-Item -ItemType Directory -Force -Path $clone | Out-Null
    Push-Location $clone
    try {
        git init -q 2>&1 | Out-Null
        git config user.email t@t 2>&1 | Out-Null
        git config user.name t 2>&1 | Out-Null
        Set-Content -LiteralPath (Join-Path $clone 'f.txt') -Value 'one'
        git add -A 2>&1 | Out-Null
        git commit -qm one 2>&1 | Out-Null
        git tag v0.6.0 2>&1 | Out-Null
        $commit060 = (git rev-parse HEAD).Trim()
        Set-Content -LiteralPath (Join-Path $clone 'f.txt') -Value 'two'
        git commit -qam two 2>&1 | Out-Null
        git tag v0.7.0 2>&1 | Out-Null
    } finally { Pop-Location }

    $ptrName = 'pending-windows-x86_64.json'

    # A prefix whose sot.exe reports `version`, with an install.json naming (oldVersion, oldCommit) when given and a
    # pointer armed when asked.
    function New-Prefix([string]$name, [string]$version, [string]$oldVersion, [string]$oldCommit, [bool]$armed) {
        $prefix = Join-Path $root $name
        $bin = Join-Path $prefix 'bin'
        $updates = Join-Path $prefix 'updates'
        New-Item -ItemType Directory -Force -Path $bin, $updates | Out-Null
        Copy-Item -LiteralPath $stub -Destination (Join-Path $bin 'sot.exe')
        Set-Content -LiteralPath (Join-Path $bin 'version.txt') -Encoding ASCII -Value "sot $version (abc 2026-01-01)"
        if ($oldVersion) {
            $ij = "{`n  `"schema`": 1,`n  `"version`": `"$oldVersion`",`n  `"tag`": `"v$oldVersion`",`n  `"commit`": `"$oldCommit`"`n}`n"
            [System.IO.File]::WriteAllText((Join-Path $prefix 'install.json'), $ij, (New-Object System.Text.UTF8Encoding($false)))
        }
        if ($armed) { Set-Content -LiteralPath (Join-Path $updates $ptrName) -Value '{}' }
        return $prefix
    }
    # The script ends with an explicit `exit 0`, so $LASTEXITCODE after a run is its own answer, not a native command's.
    function Invoke-Manifest([string]$prefix, [string]$commit) {
        if ($commit) { & $script -Prefix $prefix -Repo $clone -Commit $commit 6>&1 2>&1 | Out-Null }
        else { & $script -Prefix $prefix -Repo $clone 6>&1 2>&1 | Out-Null }
    }
    # The lines a run prints (information stream included), as one string.
    function Get-ManifestOutput([string]$prefix, [string]$commit) {
        if ($commit) { return ((& $script -Prefix $prefix -Repo $clone -Commit $commit 6>&1 2>&1) | ForEach-Object { "$_" }) -join "`n" }
        return ((& $script -Prefix $prefix -Repo $clone 6>&1 2>&1) | ForEach-Object { "$_" }) -join "`n"
    }
    function Check-Exit([string]$name) { Check "${name}: exit code 0" ($LASTEXITCODE -eq 0) "got $LASTEXITCODE" }
    function Pointer([string]$prefix) { return (Test-Path -LiteralPath (Join-Path $prefix "updates\$ptrName")) }
    function Manifest([string]$prefix) { return (Get-Content -LiteralPath (Join-Path $prefix 'install.json') -Raw) }
    # The record is written through a temp file and a rename: nothing is left beside it, and it parses, with no BOM.
    function Check-Whole([string]$name, [string]$prefix) {
        $left = @(Get-ChildItem -LiteralPath $prefix -Filter 'install.json*' | Where-Object { $_.Name -ne 'install.json' })
        Check "${name}: no temp file left beside install.json" ($left.Count -eq 0) (($left | ForEach-Object { $_.Name }) -join ', ')
        $bytes = [System.IO.File]::ReadAllBytes((Join-Path $prefix 'install.json'))
        $whole = $false
        try { $whole = [bool]((Manifest $prefix) | ConvertFrom-Json).version } catch {}
        Check "${name}: the record is whole JSON with no BOM" ($whole -and $bytes[0] -ne 0xEF) "first byte $($bytes[0])"
    }

    Write-Host "`n=== 1. a changed version drops the armed pointer ===" -ForegroundColor Cyan
    $p1 = New-Prefix 'p1' '0.6.0' '0.5.0' 'aaaa' $true
    Invoke-Manifest $p1
    Check-Exit 'p1'
    Check 'install.json now names the new version' ((Manifest $p1) -match '"version":\s*"0\.6\.0"') (Manifest $p1)
    Check 'pointer dropped' (-not (Pointer $p1)) 'pointer kept'
    Check-Whole 'a replaced record (p1)' $p1

    Write-Host "`n=== 2. the same version at another commit drops it ===" -ForegroundColor Cyan
    $p2 = New-Prefix 'p2' '0.6.0' '0.6.0' 'not-the-tag-commit' $true
    Invoke-Manifest $p2
    Check-Exit 'p2'
    Check 'install.json now names the tag commit' ((Manifest $p2) -match [regex]::Escape($commit060)) (Manifest $p2)
    Check 'pointer dropped' (-not (Pointer $p2)) 'pointer kept'

    Write-Host "`n=== 3. the same version and commit (the launcher's refresh) keeps it ===" -ForegroundColor Cyan
    $p3 = New-Prefix 'p3' '0.6.0' '0.6.0' $commit060 $true
    Invoke-Manifest $p3
    Check-Exit 'p3'
    Check 'install.json still names the version' ((Manifest $p3) -match '"version":\s*"0\.6\.0"') (Manifest $p3)
    Check 'pointer kept' (Pointer $p3) 'pointer dropped by a same-install rewrite'

    Write-Host "`n=== 4. a first install with no pointer: nothing breaks ===" -ForegroundColor Cyan
    $p4 = New-Prefix 'p4' '0.7.0' $null $null $false
    Invoke-Manifest $p4
    Check-Exit 'p4'
    Check 'install.json written' ((Manifest $p4) -match '"version":\s*"0\.7\.0"') 'no manifest'
    Check 'still no pointer' (-not (Pointer $p4)) 'a pointer appeared'
    Check-Whole 'a first record (p4)' $p4
    Check 'the record says this box runs its local daemon and a window (every Windows box does)' ((Manifest $p4) -match '"daemon":\s*true,\s*"frontend":\s*true') (Manifest $p4)

    Write-Host "`n=== 5. a pointer with no install.json yet is dropped too ===" -ForegroundColor Cyan
    $p5 = New-Prefix 'p5' '0.7.0' $null $null $true
    Invoke-Manifest $p5
    Check-Exit 'p5'
    Check 'pointer dropped' (-not (Pointer $p5)) 'pointer kept'

    Write-Host "`n=== 6. a '+' build: recorded only when its caller hands the pack's commit ===" -ForegroundColor Cyan
    $packCommit = ('ab12' * 10)
    $p6 = New-Prefix 'p6' '0.6.6-DEV+abcdef123' $null $null $false
    Invoke-Manifest $p6 $packCommit
    Check-Exit 'p6'
    $m6 = if (Test-Path -LiteralPath (Join-Path $p6 'install.json')) { Manifest $p6 } else { '' }
    Check 'a + build with the commit writes a record naming the + version, its tag and the commit' (($m6 -match '"version":\s*"0\.6\.6-DEV\+abcdef123"') -and ($m6 -match '"tag":\s*"v0\.6\.6-DEV\+abcdef123"') -and ($m6 -match $packCommit) -and ($m6 -match '"daemon":\s*true,\s*"frontend":\s*true')) $m6
    $p7 = New-Prefix 'p7' '0.6.6-DEV+abcdef123' $null $null $false
    Invoke-Manifest $p7
    Check-Exit 'p7'
    Check 'a + build with no commit (a source checkout) writes none' (-not (Test-Path -LiteralPath (Join-Path $p7 'install.json'))) 'a record was written'
    Invoke-Manifest $p6
    Check 'and a later call with no commit leaves the record it finds' ((Manifest $p6) -match $packCommit) (Manifest $p6)
    # The line says what happened: a record for this version and a commit is kept, and a source checkout has none to keep.
    $out6 = Get-ManifestOutput $p6
    Check 'a + build whose record is already there says it keeps that record, not that none is written' (($out6 -match 'keeping the record that names it') -and ($out6 -notmatch 'no manifest written')) $out6
    $out7 = Get-ManifestOutput $p7
    Check 'a source checkout keeps the line: not a release build, no manifest written' (($out7 -match 'is not a release build - no manifest written') -and ($out7 -notmatch 'keeping the record that names it')) $out7
    # The sync is its own concern: a + build with no commit writes no record, and still syncs the hub it is given.
    $srcD = Join-Path $root 'stubd.cs'
    Set-Content -LiteralPath $srcD -Encoding ASCII -Value @'
class D { static int Main(string[] a) { System.IO.File.WriteAllText(System.IO.Path.Combine(System.AppDomain.CurrentDomain.BaseDirectory, "synced.txt"), string.Join(" ", a)); return 0; } }
'@
    $stubD = Join-Path $root 'sotd-stub.exe'
    [void](& $csc /nologo /out:$stubD $srcD 2>&1)
    $p8 = New-Prefix 'p8' '0.6.6-DEV+abcdef123' $null $null $false
    Copy-Item -LiteralPath $stubD -Destination (Join-Path $p8 'bin\sotd.exe')
    & $script -Prefix $p8 -Repo $clone -Hub rig-hub 6>&1 2>&1 | Out-Null
    Check-Exit 'p8'
    $syncedFile = Join-Path $p8 'bin\synced.txt'
    Check 'a + build with no commit writes no record but still runs the hub sync' ((-not (Test-Path -LiteralPath (Join-Path $p8 'install.json'))) -and (Test-Path -LiteralPath $syncedFile) -and ((Get-Content -LiteralPath $syncedFile -Raw) -match 'topology sync --hub rig-hub')) 'record or no sync'
    Write-Host "`n=== 9. a record that cannot be read keeps its pin (only an installer run decides a pin) ===" -ForegroundColor Cyan
    $p9 = New-Prefix 'p9' '0.6.0' '0.5.0' 'aaaa' $false
    [System.IO.File]::WriteAllText((Join-Path $p9 'install.json'), "{`n  `"schema`": 1,`n  `"version`": `"0.5.0`",`n  `"tag`": `"v0.5.0`",`n  `"pinned`": tr", (New-Object System.Text.UTF8Encoding($false)))
    Invoke-Manifest $p9
    Check-Exit 'p9'
    $j9 = $null; try { $j9 = (Manifest $p9) | ConvertFrom-Json } catch {}
    Check 'the rewritten record parses, names the new version and is pinned' (($null -ne $j9) -and $j9.version -eq '0.6.0' -and $j9.pinned -eq $true) (Manifest $p9)
    Check-Whole 'a replaced cut record (p9)' $p9
    Check 'a record that reads and is unpinned stays unpinned' (-not ((Manifest $p1) -match 'pinned')) (Manifest $p1)

    Write-Host "`n=== 11. a sot that warns on stderr does not end the script, whatever the caller redirects (#49) ===" -ForegroundColor Cyan
    $p11 = New-Prefix 'p11' '0.7.0' $null $null $false
    Set-Content -LiteralPath (Join-Path $p11 'bin\warn.txt') -Value 'x'
    $threw11 = ''
    try { Invoke-Manifest $p11 } catch { $threw11 = $_.Exception.Message }
    Check 'p11: the run is not ended by the stderr line' ($threw11 -eq '') $threw11
    Check-Exit 'p11'
    Check 'the version is read past the stderr line and the record written' ((Test-Path -LiteralPath (Join-Path $p11 'install.json')) -and ((Manifest $p11) -match '"version":\s*"0\.7\.0"')) 'no record'

    Write-Host "`n=== 12. a tag the clone lacks records unknown, never git's echoed argument ===" -ForegroundColor Cyan
    $p12 = New-Prefix 'p12' '0.9.9' $null $null $false
    Invoke-Manifest $p12
    Check-Exit 'p12'
    Check 'the commit is unknown' ((Manifest $p12) -match '"commit":\s*"unknown"') (Manifest $p12)

    Write-Host "`n=== 10. the record is replaced by a rename: a handle opened before the run still reads the old bytes ===" -ForegroundColor Cyan
    $p10 = New-Prefix 'p10' '0.6.0' '0.5.0' 'aaaa' $false
    $file10 = Join-Path $p10 'install.json'
    $old10 = [System.IO.File]::ReadAllText($file10)
    $held = [System.IO.File]::Open($file10, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, ([System.IO.FileShare]'ReadWrite,Delete'))
    $still = ''
    try {
        Invoke-Manifest $p10
        Check-Exit 'p10'
        $held.Position = 0
        $still = (New-Object System.IO.StreamReader($held)).ReadToEnd()
    } finally { $held.Dispose() }
    Check 'the handle opened before the run reads the old record (an in-place write would show the new one)' ($still -eq $old10) $still
    Check 'and the file now names the new version' ((Manifest $p10) -match '"version":\s*"0\.6\.0"') (Manifest $p10)
} finally {
    Write-Host "`n================ $pass passed, $fail failed ================" -ForegroundColor $(if ($fail) { 'Red' } else { 'Green' })
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}
if ($fail) { exit 1 }
exit 0

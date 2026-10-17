# test-install-manifest.ps1 -- regression harness for scripts/install-manifest.ps1's pending-pointer rule
# (UPDATER-ARMS-INSTALLED): writing an install.json for a NEW install (another version or another commit) drops the
# pointer armed over the install it replaces; a rewrite of the SAME version and commit (the launcher's refresh after an
# apply) keeps it; with no pointer nothing breaks.
#
# The script reads its version from `<prefix>\bin\sot.exe --version`, so the fixture builds a stand-in sot.exe with the
# .NET Framework's csc.exe (on every Windows): it prints the first line of version.txt beside it. The commit comes from
# a scratch git repo with two tags. No network, no sotd, no hub: install-manifest.ps1 syncs topology only with -Hub.
#
# Runs under Windows PowerShell 5.1 and pwsh 7. ASCII ONLY (see the same note in sot-apply.ps1). Self-ending: every
# step is local and bounded, and the scratch folder is removed at the end.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\tests\test-install-manifest.ps1
#   pwsh -NoProfile -File scripts\tests\test-install-manifest.ps1

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
    function Invoke-Manifest([string]$prefix) {
        & $script -Prefix $prefix -Repo $clone 6>&1 2>&1 | Out-Null
        return $LASTEXITCODE
    }
    function Pointer([string]$prefix) { return (Test-Path -LiteralPath (Join-Path $prefix "updates\$ptrName")) }
    function Manifest([string]$prefix) { return (Get-Content -LiteralPath (Join-Path $prefix 'install.json') -Raw) }

    Write-Host "`n=== 1. a changed version drops the armed pointer ===" -ForegroundColor Cyan
    $p1 = New-Prefix 'p1' '0.6.0' '0.5.0' 'aaaa' $true
    $rc = Invoke-Manifest $p1
    Check 'exit code 0' ($rc -eq 0 -or $null -eq $rc) "got $rc"
    Check 'install.json now names the new version' ((Manifest $p1) -match '"version":\s*"0\.6\.0"') (Manifest $p1)
    Check 'pointer dropped' (-not (Pointer $p1)) 'pointer kept'

    Write-Host "`n=== 2. the same version at another commit drops it ===" -ForegroundColor Cyan
    $p2 = New-Prefix 'p2' '0.6.0' '0.6.0' 'not-the-tag-commit' $true
    Invoke-Manifest $p2 | Out-Null
    Check 'install.json now names the tag commit' ((Manifest $p2) -match [regex]::Escape($commit060)) (Manifest $p2)
    Check 'pointer dropped' (-not (Pointer $p2)) 'pointer kept'

    Write-Host "`n=== 3. the same version and commit (the launcher's refresh) keeps it ===" -ForegroundColor Cyan
    $p3 = New-Prefix 'p3' '0.6.0' '0.6.0' $commit060 $true
    Invoke-Manifest $p3 | Out-Null
    Check 'install.json still names the version' ((Manifest $p3) -match '"version":\s*"0\.6\.0"') (Manifest $p3)
    Check 'pointer kept' (Pointer $p3) 'pointer dropped by a same-install rewrite'

    Write-Host "`n=== 4. a first install with no pointer: nothing breaks ===" -ForegroundColor Cyan
    $p4 = New-Prefix 'p4' '0.7.0' $null $null $false
    $rc = Invoke-Manifest $p4
    Check 'exit code 0' ($rc -eq 0 -or $null -eq $rc) "got $rc"
    Check 'install.json written' ((Manifest $p4) -match '"version":\s*"0\.7\.0"') 'no manifest'
    Check 'still no pointer' (-not (Pointer $p4)) 'a pointer appeared'

    Write-Host "`n=== 5. a pointer with no install.json yet is dropped too ===" -ForegroundColor Cyan
    $p5 = New-Prefix 'p5' '0.7.0' $null $null $true
    Invoke-Manifest $p5 | Out-Null
    Check 'pointer dropped' (-not (Pointer $p5)) 'pointer kept'
} finally {
    Write-Host "`n================ $pass passed, $fail failed ================" -ForegroundColor $(if ($fail) { 'Red' } else { 'Green' })
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}
if ($fail) { exit 1 }

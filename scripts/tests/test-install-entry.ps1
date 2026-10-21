# test-install-entry.ps1 -- scripts/install.ps1, the bootstrap of sotd install on Windows ("update sot from the repo"),
# with its network replaced: the fake release is a folder, served by Get-Url and Get-UrlText, which this file
# redefines after it dot-sources install.ps1. Dot-sourced by test-install-layout.ps1 after its last section, in its
# scope ($root, $repoRoot, $SotdPath, Check), in the scratch profile scratch-guard.ps1 made. Windows PowerShell 5.1,
# ASCII only. The guarantees: a tag's own installer runs (the channel picks the tag), every file of a pack matches
# SHA256SUMS before anything of it runs, and every exit ends in one verdict line. The pack's sotd.exe is the real one
# (SotdPath), so a refusal it gives is sotd install's own; the install itself is the install smokes' (install-smoke.ps1).

$saved7 = @{}
foreach ($n in @('SOT_INSTALL_TAG', 'SOT_INSTALL_IMPLICIT', 'SOT_INSTALL_REPO')) { $saved7[$n] = [Environment]::GetEnvironmentVariable($n) }
try {
    Write-Host "`n=== 7. install.ps1: the bootstrap of sotd install ===" -ForegroundColor Cyan
    . (Join-Path $repoRoot 'scripts\install.ps1')
    $env:SOT_INSTALL_REPO = 'o/r'
    Remove-Item Env:SOT_INSTALL_TAG, Env:SOT_INSTALL_IMPLICIT -ErrorAction SilentlyContinue
    $rel7 = Join-Path $root 'rel7'
    New-Item -ItemType Directory -Force -Path $rel7 | Out-Null
    $script:Net7 = New-Object System.Collections.ArrayList
    $script:Lines7 = New-Object System.Collections.ArrayList
    $script:Latest7 = 'v2.0.0'; $script:Newest7 = 'v2.1.0-rc1'
    function Write-EntryLine([string]$Text) { [void]$script:Lines7.Add($Text) }
    # A tag's installer, fetched by the prelude, says how it was run; a release asset is a file of $rel7\<tag>.
    function Get-Url([string]$Url, [string]$Path) {
        [void]$script:Net7.Add($Url)
        if ($Url -match '/([^/]+)/scripts/install\.ps1$') {
            $tag = $Matches[1]
            [System.IO.File]::WriteAllText($Path, "Write-Host ('tag-installer ' + `$env:SOT_INSTALL_TAG + ' ' + `$env:SOT_INSTALL_IMPLICIT + ' ' + (`$args -join ' ')); Write-Host 'PASS: Ship of Tools $tag at x on this computer'; exit 0")
            return 0
        }
        if ($Url -match '/releases/download/([^/]+)/(.+)$') {
            $f = Join-Path (Join-Path $rel7 $Matches[1]) $Matches[2]
            if (Test-Path -LiteralPath $f) { Copy-Item -LiteralPath $f -Destination $Path; return 0 }
            return 22
        }
        return 6
    }
    function Get-UrlText([string[]]$CurlArgs) {
        $url = $CurlArgs[-1]; [void]$script:Net7.Add($url)
        if ($url -like '*/releases/latest') { return [pscustomobject]@{ Code = 0; Lines = @("https://github.com/o/r/releases/tag/$script:Latest7") } }
        if ($url -like '*api.github.com*') { return [pscustomobject]@{ Code = 0; Lines = @("[{`"tag_name`": `"$script:Newest7`"}]", '200') } }
        return [pscustomobject]@{ Code = 6; Lines = @() }
    }
    function Invoke-Run7([string[]]$Argv) {
        $script:Lines7.Clear(); $script:Net7.Clear()
        $script:Code7 = Invoke-InstallEntry $Argv
        $script:Verdicts7 = @($script:Lines7 | Where-Object { $_ -match '^(PASS|FAIL|ONE STEP LEFT): ' }).Count
        $script:Last7 = "$(@($script:Lines7 | Where-Object { "$_".Trim() })[-1])"
    }
    function Test-Said7([string]$Text) { return [bool]@($script:Lines7 | Where-Object { $_.Contains($Text) }).Count }
    $prefix7 = Join-Path $env:LOCALAPPDATA 'sot'
    function Set-Installed7([string]$Version) {
        New-Item -ItemType Directory -Force -Path $prefix7 | Out-Null
        [System.IO.File]::WriteAllText((Join-Path $prefix7 'install.json'), "{`n  `"schema`": 1,`n  `"version`": `"$Version`"`n}`n")
    }

    # ---- the prelude: the tag's own installer ------------------------------------------------------------------------
    Remove-Item -LiteralPath (Join-Path $prefix7 'install.json') -Force -ErrorAction SilentlyContinue
    Invoke-Run7 @('-Hub', 'hub')
    Check '7: no install: the latest stable release, its own installer run as an implicit run with the same arguments' ((Test-Said7 'tag-installer v2.0.0 1 -Hub hub') -and $script:Code7 -eq 0 -and $script:Verdicts7 -eq 1) ($script:Lines7 -join ' | ')
    Set-Installed7 '1.9.0-rc2'
    Invoke-Run7 @()
    Check '7: a pre-release: the newest published release of any kind' (Test-Said7 'tag-installer v2.1.0-rc1 1') ($script:Lines7 -join ' | ')
    Set-Installed7 '2.0.1-dev+abc123'
    Invoke-Run7 @()
    Check '7: a build of the line: the newest published release too' (Test-Said7 'tag-installer v2.1.0-rc1 1') ($script:Lines7 -join ' | ')
    Invoke-Run7 @('-Version', 'v1.5.0')
    Check '7: a named version: that tag, told it was named, and no listing' ((Test-Said7 'tag-installer v1.5.0 0 -Version v1.5.0') -and -not @($script:Net7 | Where-Object { $_ -like '*api.github.com*' }).Count) ($script:Lines7 -join ' | ')
    Invoke-Run7 @('-Version')
    Check '7: an option with no value: one FAIL, exit 2' ($script:Code7 -eq 2 -and $script:Verdicts7 -eq 1 -and $script:Last7 -eq 'FAIL: options: -Version needs a value') "$($script:Code7): $($script:Last7)"

    # ---- the pack, checked before anything of it runs ----------------------------------------------------------------
    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    function New-Pack7([string]$Tag, [string]$Sotd) {
        $v = $Tag.TrimStart('v'); $dir = Join-Path $rel7 $Tag; $top = "sot-$v-windows-x86_64"
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
        $zip = Join-Path $dir "$top.zip"
        Remove-Item -LiteralPath $zip -Force -ErrorAction SilentlyContinue
        $a = New-Object System.IO.Compression.ZipArchive([System.IO.File]::Create($zip), [System.IO.Compression.ZipArchiveMode]::Create)
        try { [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($a, $Sotd, "$top/sotd.exe") } finally { $a.Dispose() }
        [System.IO.File]::WriteAllText((Join-Path $dir 'COMMIT'), "0123456789abcdef0123456789abcdef01234567`n")
        $sums = foreach ($f in @("$top.zip", 'COMMIT')) { "$((Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $dir $f)).Hash.ToLowerInvariant())  $f`n" }
        [System.IO.File]::WriteAllText((Join-Path $dir 'SHA256SUMS'), (-join $sums))
        return $dir
    }
    $pack7 = New-Pack7 'v3.0.0' $SotdPath
    $env:SOT_INSTALL_TAG = 'v3.0.0'
    Invoke-Run7 @('-Frob')
    Check '7: a named release is fetched, checked and its own sotd install runs, whose refusal is the one line' ($script:Code7 -eq 2 -and $script:Verdicts7 -eq 1 -and $script:Last7 -eq 'FAIL: options: unknown option: -Frob') "$($script:Code7): $($script:Lines7 -join ' | ')"
    [System.IO.File]::AppendAllText((Join-Path $pack7 'sot-3.0.0-windows-x86_64.zip'), 'tampered')
    Invoke-Run7 @('-Frob')
    Check '7: an archive that does not match SHA256SUMS: FAIL before its sotd runs' ($script:Code7 -eq 1 -and $script:Last7 -like 'FAIL: verify: sot-3.0.0-windows-x86_64.zip does not match its SHA256SUMS entry*') "$($script:Code7): $($script:Last7)"
    $pack7 = New-Pack7 'v3.0.0' $SotdPath
    $zipLine = @([System.IO.File]::ReadAllLines((Join-Path $pack7 'SHA256SUMS')) | Where-Object { $_ -like '*.zip' })[0]
    [System.IO.File]::WriteAllText((Join-Path $pack7 'SHA256SUMS'), "$zipLine`n")
    Invoke-Run7 @('-Frob')
    Check '7: a COMMIT SHA256SUMS does not list is refused' ($script:Last7 -eq 'FAIL: verify: COMMIT is not listed in SHA256SUMS; every file of the pack must be checksummed') $script:Last7
    Remove-Item Env:SOT_INSTALL_TAG -ErrorAction SilentlyContinue
    $pack7 = New-Pack7 'v3.0.0' $SotdPath
    $archive7 = Join-Path $root 'archive7'
    New-Item -ItemType Directory -Force -Path $archive7 | Out-Null
    Copy-Item -Path (Join-Path $pack7 '*') -Destination $archive7
    Invoke-Run7 @('-Archive', $archive7, '-Frob')
    Check '7: -Archive runs this bootstrap with the folder''s pack and fetches nothing' ($script:Last7 -eq 'FAIL: options: unknown option: -Frob' -and $script:Net7.Count -eq 0) "$($script:Last7); fetched $($script:Net7 -join ' ')"
} catch {
    Check '7: section ran' $false $_.Exception.Message
} finally {
    foreach ($n in $saved7.Keys) { [Environment]::SetEnvironmentVariable($n, $saved7[$n]) }
}

# install.ps1 -- install or update Ship of Tools on this Windows computer (scripts/CLAUDE.md, "Installers"): the
# bootstrap of `sotd install`.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File install.ps1 [-Version vX.Y.Z | -Archive <folder>] [-Hub <alias>]
#                                                                    [-Prefix <dir>] [-RestartDaemon]
#
# The prelude fetches the target tag's own install.ps1 and runs it: the tag -Version names, else the channel the install
# runs (none or a stable release: the latest stable release; a pre-release or a build of the line: the newest published
# release), told it is an implicit run (SOT_INSTALL_TAG, SOT_INSTALL_IMPLICIT). The body downloads that release's
# SHA256SUMS, zip and COMMIT (or copies them from the -Archive folder, which skips the prelude), checks every file against
# SHA256SUMS, unpacks the zip and runs its own sotd.exe install with the same arguments, which does the install and
# prints the one verdict: PASS, ONE STEP LEFT (exit 0) or FAIL (exit 1, 2 for a refusal), then an explicit exit.
# Arguments are read by hand from $args, so an option error ends in the same one verdict line as every other exit.
#
# ASCII ONLY in string literals: Windows PowerShell 5.1 decodes a BOM-less .ps1 as Windows-1252.

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$script:Repo = $(if ($env:SOT_INSTALL_REPO) { $env:SOT_INSTALL_REPO } else { 'kalidke/ship-of-tools' })
$script:CurlBound = @('--connect-timeout', '15', '--speed-limit', '1000', '--speed-time', '60')
$script:Done = $false

# Every line the bootstrap prints goes through here (the suite replaces it to read a run's lines in order).
function Write-EntryLine([string]$Text) { Write-Host $Text }
function Write-Verdict([string]$Line) {
    if ($script:Done) { return }
    $script:Done = $true
    Write-EntryLine $Line
}
function Stop-Entry([string]$Check, [string]$Detail, [int]$Code = 1) {
    $script:Rc = $Code
    Write-Verdict "FAIL: ${Check}: $Detail"
    throw 'sot-install-stop'
}

# A native command's exit code and its lines, in order, never thrown on: Windows PowerShell 5.1 turns native stderr into
# terminating errors under 'Stop' when the caller redirects. -Show writes each line as it comes.
function Invoke-Native([string]$Exe, [string[]]$ArgList, [switch]$Show) {
    $eap = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    $lines = New-Object System.Collections.ArrayList
    $code = 127
    try {
        & $Exe @ArgList 2>&1 | ForEach-Object { $l = "$_"; if ($Show) { Write-EntryLine $l }; [void]$lines.Add($l) }
        $code = $LASTEXITCODE
        if ($null -eq $code) { $code = 0 }
    } catch {
        [void]$lines.Add($_.Exception.Message)
    } finally {
        $ErrorActionPreference = $eap
    }
    return [pscustomobject]@{ Code = $code; Lines = @($lines) }
}
# The network, in two calls the suite replaces (test-install-entry.ps1): a file to a path, and a text with curl's exit code.
function Get-Url([string]$Url, [string]$Path) {
    return (Invoke-Native 'curl.exe' (@($script:CurlBound) + @('-fsSL', '-o', $Path, $Url))).Code
}
function Get-UrlText([string[]]$CurlArgs) {
    return (Invoke-Native 'curl.exe' (@($script:CurlBound) + $CurlArgs))
}

# The version the install runs, for the channel: its install.json's, else its sotd.exe's own.
function Get-InstalledVersion([string]$Prefix) {
    try { if ([System.IO.File]::ReadAllText((Join-Path $Prefix 'install.json')) -match '"version"\s*:\s*"([^"]+)"') { return $Matches[1] } } catch { }
    $sotd = Join-Path $Prefix 'bin\sotd.exe'
    if (Test-Path -LiteralPath $sotd) {
        $r = Invoke-Native $sotd @('--version')
        if ($r.Code -eq 0 -and "$($r.Lines | Select-Object -First 1)" -match '^\s*\S+\s+(\S+)') { return $Matches[1] }
    }
    return ''
}
# The newest published release of any kind. The listing is an API call: it carries GITHUB_TOKEN (or GH_TOKEN) when one is
# set, since GitHub limits anonymous calls per network address.
function Get-NewestRelease {
    $tok = $(if ($env:GITHUB_TOKEN) { $env:GITHUB_TOKEN } else { $env:GH_TOKEN })
    $a = @('-fsSL', '-w', '\n%{http_code}', '-H', 'Accept: application/vnd.github+json')
    if ($tok) { $a += @('-H', "Authorization: Bearer $tok") }
    $r = Get-UrlText ($a + "https://api.github.com/repos/$($script:Repo)/releases?per_page=1")
    $http = "$(@($r.Lines)[-1])".Trim()
    if ($r.Code -ne 0) {
        $why = ''
        if ($http -eq '403' -or $http -eq '429') {
            $why = $(if ($tok) { ": GitHub refused the request (HTTP $http, its rate limit) even with the token in GITHUB_TOKEN or GH_TOKEN; retry in an hour, or name the release with -Version <tag>, which needs no listing" }
                else { ": GitHub refused the anonymous request (HTTP $http, its rate limit for this network address); retry in an hour, set GITHUB_TOKEN to send a token, or name the release with -Version <tag>, which needs no listing" })
        }
        Stop-Entry 'target' "could not list the releases of $($script:Repo)$why"
    }
    if (($r.Lines -join "`n") -match '"tag_name"\s*:\s*"([^"]*)"') { return $Matches[1] }
    return ''
}

# The prelude: run the target tag's own install.ps1 with the same arguments and end with its verdict. A child that ends
# without one is a FAIL here, so a run never ends silent.
function Invoke-Prelude([string]$Tag, [bool]$Implicit, [string[]]$Argv, [string]$Scratch) {
    $url = "https://raw.githubusercontent.com/$($script:Repo)/$Tag/scripts/install.ps1"
    $tmp = Join-Path $Scratch 'install-target.ps1'
    if ((Get-Url $url $tmp) -ne 0) { Stop-Entry 'target' "$Tag has no scripts/install.ps1 (or $url could not be fetched): install that release by its own procedure, docs/INSTALL-AGENT.md at $Tag" }
    $env:SOT_INSTALL_TAG = $Tag
    $env:SOT_INSTALL_IMPLICIT = $(if ($Implicit) { '1' } else { '0' })
    # The child prints the one verdict, so Ctrl+C while it runs must not make this run print a second.
    $script:Done = $true
    try {
        $r = Invoke-Native 'powershell.exe' (@('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $tmp) + $Argv) -Show
    } finally {
        Remove-Item Env:SOT_INSTALL_TAG, Env:SOT_INSTALL_IMPLICIT -ErrorAction SilentlyContinue
    }
    if (-not @($r.Lines | Where-Object { $_ -match '^(PASS|FAIL|ONE STEP LEFT): ' }).Count) {
        $script:Done = $false
        Stop-Entry 'prelude' "the installer of $Tag ended without a verdict (exit $($r.Code))"
    }
    return [int]$r.Code
}

# The pack in $Scratch\pack: SHA256SUMS, the zip and COMMIT, every one listed and matching, the zip unpacked beside them.
function Get-Pack([string]$Archive, [string]$Tag, [string]$Scratch) {
    $dir = Join-Path $Scratch 'pack'
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    if ($Archive) {
        if (-not (Test-Path -LiteralPath $Archive -PathType Container)) { Stop-Entry 'fetch' "-Archive: no such folder: $Archive" 2 }
        $from = (Resolve-Path -LiteralPath $Archive).Path
        foreach ($f in @('SHA256SUMS')) { Copy-Item -LiteralPath (Join-Path $from $f) -Destination $dir -ErrorAction Stop }
    } else {
        if ($Tag -notmatch '^v[0-9]') { Stop-Entry 'fetch' "-Version takes a release tag (vX.Y.Z), not '$Tag'" 2 }
        $base = "https://github.com/$($script:Repo)/releases/download/$Tag"
        if ((Get-Url "$base/SHA256SUMS" (Join-Path $dir 'SHA256SUMS')) -ne 0) { Stop-Entry 'fetch' "could not download $base/SHA256SUMS" }
    }
    $sums = @{}
    foreach ($l in [System.IO.File]::ReadAllLines((Join-Path $dir 'SHA256SUMS'))) {
        if ($l -match '^([0-9a-fA-F]{64}) [ *](.+)$') { $sums[$Matches[2]] = $Matches[1].ToLowerInvariant() }
    }
    # Names are compared as text, never as patterns: a version carries a + .
    $zips = @($sums.Keys | Where-Object { $_.StartsWith('sot-') -and $_.EndsWith('-windows-x86_64.zip') })
    if ($zips.Count -ne 1) { Stop-Entry 'fetch' "SHA256SUMS must name exactly one sot-<version>-windows-x86_64.zip (it names $($zips.Count))" }
    $zip = $zips[0]
    $ver = $zip.Substring(4, $zip.Length - 4 - '-windows-x86_64.zip'.Length)
    if ($ver -notmatch '^[0-9A-Za-z.+-]+$') { Stop-Entry 'fetch' "unsafe version '$ver' in SHA256SUMS" }
    foreach ($f in @($zip, 'COMMIT')) {
        if (-not $sums.ContainsKey($f)) { Stop-Entry 'verify' "$f is not listed in SHA256SUMS; every file of the pack must be checksummed" }
        $p = Join-Path $dir $f
        if ($Archive) {
            try { Copy-Item -LiteralPath (Join-Path $from $f) -Destination $p -ErrorAction Stop } catch { Stop-Entry 'fetch' "cannot copy $f from ${from}: $($_.Exception.Message)" }
        } elseif ((Get-Url "$base/$f" $p) -ne 0) { Stop-Entry 'fetch' "could not download $base/$f" }
        $h = (Get-FileHash -Algorithm SHA256 -LiteralPath $p).Hash.ToLowerInvariant()
        if ($h -ne $sums[$f]) { Stop-Entry 'verify' "$f does not match its SHA256SUMS entry (got $h)" }
    }
    Expand-Archive -LiteralPath (Join-Path $dir $zip) -DestinationPath $dir -Force
    $sotd = Join-Path $dir "sot-$ver-windows-x86_64\sotd.exe"
    if (-not (Test-Path -LiteralPath $sotd)) { Stop-Entry 'unpack' "$zip holds no sotd.exe" }
    $help = Invoke-Native $sotd @('install', '--help')
    if (-not (($help.Lines -join "`n") -match 'Usage: sotd install')) { Stop-Entry 'unpack' "release $ver predates sotd install: install it with its own installer (the one-liner without SOT_INSTALL_TAG, or that tag's scripts/install.ps1)" }
    return [pscustomobject]@{ Dir = $dir; Sotd = $sotd }
}

function Invoke-Entry([string[]]$Argv, [string]$Scratch) {
    $o = @{ Version = ''; Archive = ''; Prefix = '' }
    for ($i = 0; $i -lt $Argv.Count; $i++) {
        if ([string]$Argv[$i] -match '^-(Version|Archive|Prefix)$') {
            if ($i + 1 -ge $Argv.Count -or ([string]$Argv[$i + 1]).StartsWith('-')) { Stop-Entry 'options' "$($Argv[$i]) needs a value" 2 }
            $o[$Matches[1]] = [string]$Argv[$i + 1]
        }
    }
    $prefix = $(if ($o.Prefix) { $o.Prefix } else { Join-Path $env:LOCALAPPDATA 'sot' })
    if ($env:OS -ne 'Windows_NT') { Stop-Entry 'preflight' 'this installer is for Windows; on Linux and macOS run scripts/install.sh' 2 }
    if (-not (Get-Command curl.exe -ErrorAction SilentlyContinue)) { Stop-Entry 'preflight' 'curl.exe is not on PATH (it ships with Windows 10 version 1803 and later); update Windows or put curl.exe on PATH' 2 }
    if (-not $env:SOT_INSTALL_TAG -and -not $o.Archive) {
        $tag = $o.Version; $implicit = $false
        if (-not $tag) {
            $implicit = $true
            $installed = Get-InstalledVersion $prefix
            if ($installed.Contains('+') -or $installed.Contains('-')) { $tag = Get-NewestRelease }
            else {
                $r = Get-UrlText @('-fsSLI', '-o', 'NUL', '-w', '%{url_effective}', "https://github.com/$($script:Repo)/releases/latest")
                if ($r.Code -ne 0) { Stop-Entry 'target' "could not resolve the latest release of $($script:Repo)" }
                $tag = ((($r.Lines -join '') -split '/')[-1]).Trim()
            }
        }
        if ($tag -notmatch '^v[0-9]') { Stop-Entry 'target' "could not resolve a release tag (got '$tag')" }
        return (Invoke-Prelude $tag $implicit $Argv $Scratch)
    }
    $pack = Get-Pack $o.Archive $(if ($o.Version) { $o.Version } else { $env:SOT_INSTALL_TAG }) $Scratch
    # sotd install prints the verdict, a Ctrl+C included (exit 130); any other end without exit 0, 1 or 2 is named here.
    $script:Done = $true
    $r = Invoke-Native $pack.Sotd (@('install', '--pack', $pack.Dir) + $Argv) -Show
    if (@(0, 1, 2, 130) -notcontains $r.Code) {
        $script:Done = $false
        Stop-Entry 'install' "sotd install ended with exit $($r.Code) and no verdict"
    }
    return [int]$r.Code
}

# One run: the entry in a scratch folder of its own, removed at the end, and the one verdict whatever ends it.
function Invoke-InstallEntry([string[]]$Argv) {
    $script:Rc = 1; $script:Done = $false
    $scratch = Join-Path $env:TEMP ('sot-install-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
    $code = 1
    try {
        New-Item -ItemType Directory -Force -Path $scratch | Out-Null
        $code = [int](@(Invoke-Entry $Argv $scratch)[-1])
    } catch {
        if ($_.Exception.Message -ne 'sot-install-stop') { $script:Rc = 1; Write-Verdict "FAIL: install: $($_.Exception.Message)" }
        $code = $script:Rc
    } finally {
        if (-not $script:Done) { Write-Verdict 'FAIL: install: stopped' }
        Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
    }
    return $code
}

# The suite dot-sources this file to replace the network; a run is any other invocation.
if ($MyInvocation.InvocationName -ne '.') {
    $entryCode = Invoke-InstallEntry $args
    # Explicit, never whatever the last native command left in $LASTEXITCODE.
    exit $entryCode
}

# install-manifest.ps1 -- write <prefix>\install.json for a Windows install.
#
# The Windows counterpart to the manifest block at the end of scripts/install.sh
# (schema 1 -- keep the three in sync: that block, rust/updater/src/manifest.rs,
# and this file).
#
# WHY THIS EXISTS: install.sh refuses to run on Windows (it dies on
# MINGW*/MSYS*/CYGWIN*), and it was the ONLY writer of install.json. Without
# that file `InstallManifest::for_current_exe()` returns None, so
# `spawn_startup_selfcheck()` in rust/frontend/src/selfupdate.rs returns at its
# "not a release install" guard and the frontend never even CHECKS for updates.
# That is why Windows had no auto-update -- not a missing updater, just a
# missing 10-line file the updater looks for.
#
# Called by install-shortcut.ps1 (the step INSTALL-AGENT.md section 2b already
# tells the agent to run), so a Windows install gets it without a new step.
#
# ASCII ONLY, deliberately: Windows PowerShell 5.1 decodes a BOM-less .ps1 as
# Windows-1252, so a non-ASCII character inside a STRING literal (an em-dash,
# say) is mangled into bytes that can break the parse. Comments survive it;
# strings do not. Keep this file 7-bit.

[CmdletBinding()]
param(
    # Install prefix. Must match where the frontend exe actually lives:
    # the manifest is found at <exe>\..\..\install.json, and the launcher
    # stages to <prefix>\bin\sot.exe.
    [string]$Prefix,
    # Install role, as understood by the updater: local | remote | be-only.
    # Windows is a frontend host talking to a remote backend (section 2b), and
    # `remote` is also what gates the FE-side self-update: any other role means
    # "the backend on this machine owns updates", which is never true here.
    [ValidateSet('local', 'remote', 'be-only')]
    [string]$Role = 'remote',
    # Repo clone that supplies the launcher + config (not the binaries).
    [string]$Repo,
    # Write the manifest even if the frontend reports a -dev version.
    [switch]$AllowDev
)

$ErrorActionPreference = 'Stop'

if (-not $Prefix) { $Prefix = Join-Path $env:LOCALAPPDATA 'sot' }
if (-not $Repo)   { $Repo = Resolve-Path -Path (Join-Path $PSScriptRoot '..') | Select-Object -ExpandProperty Path }

$exe = Join-Path $Prefix 'bin\sot.exe'
if (-not (Test-Path -LiteralPath $exe)) {
    Write-Host "install-manifest: no frontend at $exe - nothing to describe yet."
    Write-Host "  Extract the release zip's sot.exe there (INSTALL-AGENT section 2b step 1), then re-run."
    exit 0
}

# Version comes from the binary itself, never from a hardcoded string: the
# manifest must describe what is actually installed. `sot --version` prints
# "sot X.Y.Z (<sha> <date>)" or "sot X.Y.Z-dev+<sha> (...)".
$versionLine = & $exe --version 2>&1 | Select-Object -First 1
if ($versionLine -notmatch '^\s*sot\s+(\S+)') {
    Write-Warning "install-manifest: could not read a version from '$versionLine' - skipping manifest."
    exit 0
}
$version = $Matches[1]

# A source build is stamped -dev and is hard-guarded from self-updating in
# selfupdate.rs regardless of what we write here. Claiming it is a release
# install would make install.json describe something untrue, so skip it and
# say why -- dev machines update with git pull + cargo build, which is what
# the launcher's freshness prelude already does.
if ($version -match '-dev' -and -not $AllowDev) {
    Write-Host "install-manifest: $version is a source build (-dev) - no manifest written."
    Write-Host "  Dev machines update via the launcher's git pull + cargo rebuild, not the release updater."
    exit 0
}

$tag = "v$version"
$config = Join-Path $env:APPDATA 'sot'
$commit = ''
try {
    $commit = (& git -C $Repo rev-parse HEAD 2>$null | Select-Object -First 1)
} catch {}
if (-not $commit) { $commit = 'unknown' }

# Minimal JSON string escape, matching install.sh's json_str(): an exotic
# prefix must not be able to produce a manifest that parses wrong, because a
# broken manifest silently redirects the updater's staging root.
function ConvertTo-JsonStr([string]$s) { return ($s -replace '\\', '\\' -replace '"', '\"') }

New-Item -ItemType Directory -Force -Path $Prefix | Out-Null
$manifest = Join-Path $Prefix 'install.json'
$body = @"
{
  "schema": 1,
  "role": "$Role",
  "prefix": "$(ConvertTo-JsonStr $Prefix)",
  "config": "$(ConvertTo-JsonStr $config)",
  "service": "none",
  "version": "$version",
  "tag": "$tag",
  "commit": "$commit",
  "installed_at": "$( (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ') )"
}
"@
# UTF8 without BOM: serde_json rejects a leading BOM, and Set-Content -Encoding
# utf8 on PowerShell 5.1 writes one.
[System.IO.File]::WriteAllText($manifest, $body, (New-Object System.Text.UTF8Encoding($false)))

Write-Host "install-manifest: wrote $manifest (schema 1, role=$Role, version=$version)"
Write-Host "  The frontend will now check for updates at startup and stage them for sot-apply.ps1."

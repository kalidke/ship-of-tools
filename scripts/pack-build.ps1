# pack-build.ps1 -- pack this checkout's Windows build the way release.yml's Windows job does, for `sotd update stage`.
#
#   scripts\pack-build.ps1 -Out <folder>
#
# Builds the workspace from this checkout and writes into <folder> sot-<version>-windows-x86_64.zip, COMMIT (this
# checkout's HEAD) and SHA256SUMS (the zip and COMMIT; lowercase hex, two spaces, LF, no BOM). <version> is the built
# sotd's own, so a build of the line packs as `0.6.6-rc9.14-dev+<sha>`. Refuses a dirty tree and a HEAD no
# remote-tracking branch holds (a staged dev build prepares its checkout from origin). Windows only. Build output goes
# in $env:CARGO_TARGET_DIR, else rust\target.
param([Parameter(Mandatory = $true)][string]$Out)

$ErrorActionPreference = 'Stop'

# Invoke-SotNative: every native call below goes through it, so the script does not depend on how its caller redirects.
. (Join-Path $PSScriptRoot 'sot-install-layout.ps1')

function Stop-Pack([string]$Why) {
    [Console]::Error.WriteLine("pack-build: $Why")
    exit 1
}

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if (-not [Environment]::Is64BitOperatingSystem) { Stop-Pack 'x86_64 Windows only' }
if (@((Invoke-SotNative 'git' @('status', '--porcelain')).Out).Count -gt 0) { Stop-Pack 'the working tree is not clean; commit or stash first' }
if (@((Invoke-SotNative 'git' @('branch', '-r', '--contains', 'HEAD')).Out).Count -eq 0) { Stop-Pack 'HEAD is on no remote-tracking branch; push the line first' }

# A version is the release artifact iff it has no `+`: an ambient SOT_BUILD_ORIGIN=ci must not make a local pack one.
Remove-Item Env:SOT_BUILD_ORIGIN -ErrorAction SilentlyContinue

New-Item -ItemType Directory -Force -Path $Out | Out-Null
$out = (Resolve-Path -LiteralPath $Out).Path
$commit = ((Invoke-SotNative 'git' @('rev-parse', 'HEAD')).Out -join '').Trim()
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'rust\target' }
New-Item -ItemType Directory -Force -Path $target | Out-Null
$target = (Resolve-Path -LiteralPath $target).Path

# The same command as release.yml's "Build release binaries".
Push-Location (Join-Path $root 'rust')
try {
    $env:CARGO_TARGET_DIR = $target
    $build = Invoke-SotNative 'cargo' @('build', '--release', '--workspace', '--locked') -Echo
    if ($build.Code -ne 0) { Stop-Pack "cargo build failed ($($build.Code))" }
} finally {
    Pop-Location
}

$bin = Join-Path $target 'release'
$v = ((Invoke-SotNative (Join-Path $bin 'sotd.exe') @('--version')).Out -join '' -split ' ')[1]
if (-not $v) { Stop-Pack 'sotd --version printed no version' }
$stage = "sot-$v-windows-x86_64"
$work = Join-Path ([IO.Path]::GetTempPath()) ("sot-pack-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path (Join-Path $work $stage) | Out-Null
try {
    foreach ($exe in 'sot.exe', 'sotd.exe', 'sot-capsule.exe') {
        Copy-Item (Join-Path $bin $exe) (Join-Path $work $stage)
    }
    $got = (Invoke-SotNative (Join-Path $work "$stage\sot.exe") @('--version')).Out -join ''
    if (-not "$got".StartsWith("sot $v (")) { Stop-Pack "VERSION MISMATCH for sot: $got (sotd says $v)" }
    $zip = Join-Path $out "$stage.zip"
    Remove-Item -LiteralPath $zip -ErrorAction SilentlyContinue
    # Entries are written with `/` as release.yml's pwsh does: Windows PowerShell 5.1's Compress-Archive writes `\`, which
    # the updater refuses.
    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = New-Object IO.Compression.ZipArchive([IO.File]::Create($zip), [IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($f in Get-ChildItem -LiteralPath (Join-Path $work $stage) -File) {
            [void][IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, $f.FullName, "$stage/$($f.Name)")
        }
    } finally {
        $archive.Dispose()
    }
} finally {
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}

$plain = New-Object System.Text.UTF8Encoding($false)
[IO.File]::WriteAllText((Join-Path $out 'COMMIT'), "$commit`n", $plain)
$lines = foreach ($name in "$stage.zip", 'COMMIT') {
    $digest = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $out $name)).Hash.ToLowerInvariant()
    "$digest  $name`n"
}
[IO.File]::WriteAllText((Join-Path $out 'SHA256SUMS'), (-join $lines), $plain)
Write-Output "packed $v at $out"

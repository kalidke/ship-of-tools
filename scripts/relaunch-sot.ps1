# relaunch-sot.ps1 — build the frontend, then ask the running supervisor
# to relaunch into the fresh binary (ADR 0017).
#
# Run this from inside the Ship of Tools Terminal drawer (or any shell) while the app
# is live. Because the frontend runs from a staged copy under
# %LOCALAPPDATA%\sot\bin, `cargo build --release` can overwrite
# rust\target\release without hitting the running-exe file lock. On a green
# build we drop the relaunch sentinel; the frontend's watcher notices it and
# exits with code 75, and launch-sot.ps1 re-stages + respawns with
# --relaunched (reopening the Terminal drawer and running the resume command,
# default `claude --continue`).
#
# Build failures leave the app running untouched — nothing is signalled.

[CmdletBinding()]
param(
    # Skip the build and just request a relaunch of whatever is already built.
    [switch]$NoBuild
)

$ErrorActionPreference = 'Stop'
$repo = Resolve-Path -Path (Join-Path $PSScriptRoot '..')

if (-not $NoBuild) {
    Write-Host 'Building sot-frontend (release)...' -ForegroundColor Cyan
    Push-Location (Join-Path $repo 'rust')
    try {
        cargo build --release -p sot-frontend
        $buildExit = $LASTEXITCODE
    } finally {
        Pop-Location
    }
    if ($buildExit -ne 0) {
        Write-Host "Build failed (exit $buildExit) - not relaunching." -ForegroundColor Red
        exit $buildExit
    }
    Write-Host 'Build OK.' -ForegroundColor Green
}

$sentinelDir = Join-Path $env:LOCALAPPDATA 'sot'
New-Item -ItemType Directory -Force -Path $sentinelDir | Out-Null
$sentinel = Join-Path $sentinelDir 'relaunch.request'
Set-Content -Path $sentinel -Value (Get-Date -Format o) -Encoding utf8

Write-Host 'Relaunch requested - the supervisor will restage and respawn the frontend.' -ForegroundColor Green

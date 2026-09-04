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
# default `claude --permission-mode auto --continue /sot-fe-session-start`).
#
# Build failures leave the app running untouched — nothing is signalled.
#
# Sentinel content picks the exit code (ADR 0017's 76 amendment): a bare
# timestamp -> 75 (plain relaunch, this file's default). `-Converge` writes
# `converge` instead -> the frontend's watcher (rust/frontend/src/gpu.rs)
# reads that back and exits 76, and the supervisor (launch-sot.ps1's
# do/while loop) re-runs its self-update prelude (git pull + classify) and
# freshness pass (cargo rebuild + `ShipTools.update_comm()`) BEFORE
# respawning -- the only way today to converge a resident supervisor to
# main without a fresh shortcut launch. `-Converge` skips the local build
# below: it would build the CURRENT, not-yet-pulled tree, which the
# supervisor's own freshness-pass rebuild immediately supersedes after the
# pull -- wasted work against the wrong source. `-NoBuild` and `-Converge`
# both skip the build, for different reasons; passing both is fine (same
# effect as `-Converge` alone).

[CmdletBinding()]
param(
    # Skip the build and just request a relaunch of whatever is already built.
    [switch]$NoBuild,
    # Converge: ask the resident supervisor to pull origin/main, rebuild,
    # reinstall the comm layer, and respawn -- not just respawn on today's
    # tree. See the sentinel-content note above and ADR 0017's 76 amendment.
    [switch]$Converge
)

$ErrorActionPreference = 'Stop'
$repo = Resolve-Path -Path (Join-Path $PSScriptRoot '..')

if (-not $NoBuild -and -not $Converge) {
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
$sentinelValue = if ($Converge) { "converge`n$(Get-Date -Format o)" } else { Get-Date -Format o }
Set-Content -Path $sentinel -Value $sentinelValue -Encoding utf8

if ($Converge) {
    Write-Host 'Converge requested - the supervisor will pull, rebuild, reinstall comm, then respawn the frontend.' -ForegroundColor Green
} else {
    Write-Host 'Relaunch requested - the supervisor will restage and respawn the frontend.' -ForegroundColor Green
}

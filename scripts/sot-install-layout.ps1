# sot-install-layout.ps1 -- shared "is this launch pinned to the installed
# checkout" predicate + shortcut-target rule. Dot-sourced by launch-sot.ps1
# (which gates its self-update prelude on Test-SotPinnedCheckout) and
# install-shortcut.ps1 (which uses Get-SotLauncherTarget to decide what the
# shortcut/pin should point at). Same shape as sot-hosts.ps1: one
# dot-sourceable file, no privileged access either caller doesn't already
# have.
#
# See docs/adr/0030-versioning-release-and-auto-update.md's 2026-09-17
# amendment for why this predicate exists and what it replaced.
#
# ASCII ONLY in string literals (see the same note in launch-sot.ps1 /
# sot-hosts.ps1): this file has no BOM, and Windows PowerShell 5.1 decodes
# a non-ASCII byte inside a string literal into a phantom quote that fails
# the whole parse.

# Pinned iff this script is running FROM repo\current -- a path identity,
# not a git-state one: it is exactly the invariant Linux's `sot-launch`
# wrapper enforces by construction (execs repo/current/scripts/
# launch-sot.sh, never the base clone), and it cannot disagree with which
# file is actually executing the way a git-state predicate could.
function Test-SotPinnedCheckout {
    param(
        [Parameter(Mandatory)][string]$RepoPath,
        [Parameter(Mandatory)][string]$Prefix
    )
    return ($RepoPath -eq (Join-Path $Prefix 'repo\current'))
}

# The shortcut/pin target, in priority order: the pinned launcher once
# sot-apply.ps1 (or Initialize-InstallLayout's own first run) has created
# repo\current, else the clone's own launcher (bootstrap only, before any
# layout exists). Idempotent and safe to call on every install-shortcut.ps1
# run: a box that migrates gets repointed once and every later run finds
# the same target and rewrites nothing that changed.
function Get-SotLauncherTarget {
    param(
        [Parameter(Mandatory)][string]$Prefix,
        [Parameter(Mandatory)][string]$ClonePath
    )
    $pinned = Join-Path $Prefix 'repo\current\scripts\launch-sot.ps1'
    if (Test-Path -LiteralPath $pinned) { return $pinned }
    return (Join-Path $ClonePath 'scripts\launch-sot.ps1')
}

# sot-install-layout.ps1 -- shared "which checkout is this launch pinned
# to" + first-launch layout helpers. Dot-sourced by launch-sot.ps1 (which
# runs Initialize-InstallLayout every launch and gates its self-update
# prelude on Test-SotPinnedCheckout) and install-shortcut.ps1 (which uses
# Get-SotLauncherTarget to decide what the shortcut/pin should point at).
# Same shape as sot-hosts.ps1: one dot-sourceable file, no privileged
# access either caller doesn't already have.
#
# Root cause this file exists to answer in ONE place (docs/adr/
# 0030-versioning-release-and-auto-update.md's 2026-09-17 amendment): a
# version is binaries + resources + SCRIPTS, but a shortcut aimed at a
# branch clone runs whatever the clone's `scripts\launch-sot.ps1` currently
# is, pulled fresh on every launch -- so a launcher change can break every
# release box between the push and the next tag. The fix is a shortcut
# that targets the pinned checkout instead (mirrors Linux's `sot-launch`
# wrapper, scripts/install.sh), plus the one question every dev-vs-release
# heuristic in launch-sot.ps1 used to answer four different, sometimes
# disagreeing ways: is the script actually running right now PINNED to the
# installed tag, or sitting on a branch?
#
# ASCII ONLY in string literals (see the same note in launch-sot.ps1 /
# sot-hosts.ps1): this file has no BOM, and Windows PowerShell 5.1 decodes
# a non-ASCII byte inside a string literal into a phantom quote that fails
# the whole parse.

# A pinned checkout is a detached worktree: `git worktree add --detach
# $checkout $tag` (Initialize-InstallLayout below) leaves HEAD pointing
# straight at the tag's commit, no branch. A dev clone (or a release
# install that has not migrated onto the pinned shortcut yet -- see
# install-shortcut.ps1 and launch-sot.ps1's migration handover) is always
# ON a branch. No `.git` at all (a hand-copied tree, no git metadata)
# is neither -- treated as not pinned, same as today's dev-box fallback.
function Test-SotPinnedCheckout {
    param([Parameter(Mandatory)][string]$Repo)
    if (-not (Test-Path -LiteralPath (Join-Path $Repo '.git'))) { return $false }
    $savedEAP = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        git -C $Repo symbolic-ref -q HEAD 2>&1 | Out-Null
        return ($LASTEXITCODE -ne 0)
    } finally {
        $ErrorActionPreference = $savedEAP
    }
}

# Cosmetic only (the one log line a pinned launch prints) -- `git describe`
# rather than reading install.json, since a pinned worktree's HEAD *is* the
# tag by construction and this avoids a second source of truth for what to
# print. Never throws: an unexpected repo state just prints '(unknown tag)'
# rather than blocking the launch this is only logging about.
function Get-SotPinnedTag {
    param([Parameter(Mandatory)][string]$Repo)
    $savedEAP = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $t = git -C $Repo describe --tags --exact-match 2>$null
        if ($LASTEXITCODE -eq 0 -and "$t".Trim()) { return "$t".Trim() }
        return '(unknown tag)'
    } finally {
        $ErrorActionPreference = $savedEAP
    }
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

# Twin of sot-apply.ps1's OWN Set-Junction -- kept as two separately named
# functions, not shared, on purpose: sot-apply.ps1 is fail-open by
# contract on the launch path, and a file it would have to dot-source in
# order to run at all is a new way for it not to run. This copy is shared
# only between launch-sot.ps1 and install-shortcut.ps1 (which does not
# call it today, only Get-SotLauncherTarget), never with sot-apply.ps1.
#
# A directory JUNCTION, never a symlink: a symlink needs Developer Mode or
# an elevated shell, a junction needs neither, and every reader only
# traverses it as a directory.
function Set-SotJunction {
    param([string]$Link, [string]$Target)
    try {
        if (Test-Path -LiteralPath $Link) {
            # Remove the LINK, never its contents: Remove-Item -Recurse on a
            # junction can follow into the target on older PowerShell.
            [System.IO.Directory]::Delete($Link)
        }
        $parent = Split-Path -Parent $Link
        if ($parent -and -not (Test-Path -LiteralPath $parent)) {
            New-Item -ItemType Directory -Path $parent -Force | Out-Null
        }
        New-Item -ItemType Junction -Path $Link -Target $Target -ErrorAction Stop | Out-Null
        return $true
    } catch {
        Write-SupLog "install layout: junction $Link -> $Target failed: $($_.Exception.Message)"
        return $false
    }
}

# First-launch install layout (field report, a fresh hand-installed frontend
# box on v0.6.0-rc.15). A hand install (docs/INSTALL-AGENT.md 2b) extracts the
# release binaries into <prefix>\bin and clones the repo for the launcher +
# config -- but NOTHING creates <prefix>\repo\current, and that junction is
# how the local daemon finds its Julia resources: resource_dir
# (rust/backend/src/paths.rs) tries $SOT_RESOURCE_ROOT, then
# <exe>\..\repo\current\<rel>, then <exe>\..\julia\current\<rel>, then the
# COMPILE-TIME CARGO_MANIFEST_DIR. With the junction absent, a release sotd
# fell all the way through to the CI runner's build path and every REPL verb
# died with "repl project missing". sot-apply.ps1 is the only other creator of
# that junction and it only runs on a staged update -- whose prepare step
# needs a resource checkout to prepare FROM, so a fresh box could neither
# resolve resources nor update its way out.
#
# scripts/install.sh does this at install time on Linux (its section 4, the
# ADR 0030 clone-install amendment). Windows has no installer, so the
# launcher -- which runs on every launch and already knows the clone it runs
# from -- lays the same layout down once, here:
#   repo\versions\<tag>  a detached worktree of THIS clone, pinned at the tag
#   repo\current         the junction the daemon resolves through
# Same paths the installer and the updater use, so sot-apply.ps1's flip and
# its prune of old version dirs keep working unchanged. julia\current is
# deliberately NOT created: it is the retired bundle's mount point, read only
# by pre-clone binaries, and resource_dir reaches repo\current first.
#
# The checkout is only SOURCE until its Julia environments are instantiated,
# so this step does that too (install.sh's own instantiate); see the second
# half of Initialize-InstallLayout for why a frontend box needs them.
#
# Placed BEFORE the staged-update apply below on purpose: sot-apply.ps1
# records the pre-apply junction target as its rollback checkout, so a box
# whose first update lands right after this gets a rollback-able previous
# version instead of the empty one its own comment calls out.
#
# Fail-open like every other step on this path: each failure logs, notices,
# and the launch continues.
#
# Pinned checkouts (Test-SotPinnedCheckout $repo -eq $true) never reach the
# create-a-checkout branch below by CONSTRUCTION, not by an extra check
# here: $repoCurrent already exists (it is where this very script lives),
# so `-not (Test-Path -LiteralPath $repoCurrent)` is already false. The
# "no staged sotd.exe" guard right below decides a different question --
# is there a distributable binary to derive a version and layout from at
# ALL -- which stays load-bearing for a genuine dev box (never staged one)
# and is never spuriously true on a pinned checkout (a release install
# always has one staged). The Julia-env instantiate half further down
# keeps running unconditionally through repo\current either way: sot-
# apply.ps1's version flip does not instantiate the new tag's envs itself.
function Initialize-InstallLayout {
    $stagedSotd = Join-Path $prefixDir 'bin\sotd.exe'
    if (-not (Test-Path -LiteralPath $stagedSotd)) {
        # A pure dev box: its daemon runs out of rust\target\release, where
        # resource_dir's compile-time fallback IS the correct answer, and its
        # Julia envs are the checkout's own. Nothing to create.
        Write-SupLog 'install layout: no staged sotd.exe - dev box, leaving the layout alone'
        return
    }
    # Relax 'Stop' -> 'Continue' around native git/julia and the version probe:
    # their stderr under 'Stop' + 2>&1 throws in PS 5.1. Gate on $LASTEXITCODE.
    $savedEAP = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        if (-not (Test-Path -LiteralPath $repoCurrent)) {
            if (-not (Test-Path (Join-Path $repo '.git'))) {
                Write-SupLog "install layout: $repo is not a git clone - cannot pin a version checkout"
                return
            }
            Set-LaunchStatus 'Creating install layout...'
            # The version comes from the binary itself, never a hardcoded
            # string: the layout must describe what is actually installed. The
            # line is "sotd X.Y.Z (<sha> <date>)"; a marked build adds "+src"
            # or "-dev+<sha>[-dirty]" (rust/protocol/src/lib.rs app_version)
            # and the release tag is the bare X.Y.Z[-pre] underneath both.
            $versionLine = & $stagedSotd --version 2>&1 | Select-Object -First 1
            if ("$versionLine" -notmatch '^\s*sotd\s+(\S+)') {
                Write-SupLog "install layout: could not read a version from '$versionLine'"
                return
            }
            $rawVersion = $Matches[1]
            $version = ($rawVersion -replace '\+.*$', '') -replace '-dev$', ''
            # Strict shape, because this string becomes a directory name and a
            # git ref: X.Y.Z with an optional alnum-led prerelease, nothing else.
            if ($version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z][0-9A-Za-z.]*)?$') {
                Write-SupLog "install layout: '$rawVersion' is not a version this can pin - leaving the layout alone"
                return
            }
            $tag = "v$version"
            $checkout = Join-Path $prefixDir "repo\versions\$tag"
            if (-not (Test-Path -LiteralPath $checkout)) {
                if (-not ((git -C $repo tag -l $tag) -join '')) {
                    if ($NoUpdate) {
                        # -NoUpdate is documented as the offline path; no network here.
                        Write-SupLog "install layout: tag $tag not in the local clone and -NoUpdate forbids fetching"
                    } else {
                        Write-SupLog "install layout: tag $tag not in the local clone - fetching tags"
                        $fetchOut = git -C $repo fetch --tags --quiet 2>&1
                        foreach ($l in @($fetchOut)) { if ("$l".Trim()) { Write-SupLog "install layout: git fetch -> $l" } }
                    }
                }
                if (-not ((git -C $repo tag -l $tag) -join '')) {
                    # No tag means no honest resource tree to pin. Say it once
                    # and leave the daemon on resource_dir's own fallback rather
                    # than junctioning a tree that is not this version.
                    Write-SupLog "install layout: tag $tag unavailable - repo\current NOT created"
                    $script:launchNotices.Add("install layout: release tag $tag is not in the local clone - the local daemon runs without a resource checkout") | Out-Null
                    return
                }
                # An externally deleted worktree stays registered; prune first
                # so the add cannot die on "missing but already registered"
                # (the same order scripts/install.sh and the updater's prepare
                # step use).
                git -C $repo worktree prune 2>&1 | Out-Null
                $addOut = git -C $repo worktree add --detach $checkout $tag 2>&1
                $addExit = $LASTEXITCODE
                foreach ($l in @($addOut)) { if ("$l".Trim()) { Write-SupLog "install layout: git worktree -> $l" } }
                if ($addExit -ne 0 -or -not (Test-Path -LiteralPath $checkout)) {
                    Write-SupLog "install layout: worktree add for $tag failed (exit $addExit) - repo\current NOT created"
                    $script:launchNotices.Add("install layout: could not create the $tag resource checkout - see supervisor.log") | Out-Null
                    return
                }
            }
            if (-not (Set-SotJunction $repoCurrent $checkout)) {
                $script:launchNotices.Add('install layout: could not create the repo\current junction - see supervisor.log') | Out-Null
                return
            }
            # install.json is what makes this box a release install to the
            # updater (rust/updater/src/manifest.rs). install-shortcut.ps1
            # already writes it through install-manifest.ps1 at hand-install
            # time, so this only fires for a box that missed that step -- and
            # it DELEGATES rather than re-deriving the schema: that script is
            # the Windows authority for it and refuses non-release builds on
            # its own.
            $installJson = Join-Path $prefixDir 'install.json'
            $installManifest = Join-Path $PSScriptRoot 'install-manifest.ps1'
            if (-not (Test-Path -LiteralPath $installJson) -and (Test-Path $installManifest)) {
                try {
                    $manOut = & $installManifest -Prefix $prefixDir -Repo "$repo" 6>&1 2>&1
                    foreach ($l in @($manOut)) { if ("$l".Trim()) { Write-SupLog "install layout: $l" } }
                } catch {
                    Write-SupLog "install layout: install-manifest.ps1 failed: $($_.Exception.Message)"
                }
            }
            Write-SupLog "install layout: created repo\current -> $checkout"
            $script:launchNotices.Add("install layout created: repo\current -> repo\versions\$tag") | Out-Null
        }

        # ---- the checkout's Julia environments (scripts/install.sh's own
        # instantiate step). A checkout is only SOURCE until its envs are
        # instantiated: the daemon spawns the REPL child on
        # <checkout>\julia\repl and it died at `using JSON3` on the fresh box.
        # install.sh skips this for role=remote -- a box whose backend lives
        # elsewhere -- but a Windows frontend runs its OWN local daemon (ADR
        # 0042 L2b: local is just another host) and that daemon serves local
        # REPLs, so this box does need them.
        #
        # Everything below addresses the envs THROUGH repo\current rather than
        # through $checkout, so the one code path serves both a layout this
        # call just created and one that was already there. Gated on the
        # kernel env's manifest, not on "the junction was just created": a box
        # that got its layout from the first version of this step, or by hand,
        # still needs the envs -- including a pinned checkout right after a
        # version flip, since sot-apply.ps1's junction swap does not
        # instantiate the new tag's envs itself.
        if (Test-Path -LiteralPath (Join-Path $repoCurrent 'julia\kernel\Manifest.toml')) { return }
        if (-not (Get-Command julia -ErrorAction SilentlyContinue)) {
            Write-SupLog 'install layout: no julia on PATH - Julia envs not instantiated'
            $script:launchNotices.Add('local REPL needs Julia on this box: install Julia (juliaup), then relaunch') | Out-Null
            return
        }
        # A previous instantiate's UNTRACKED Manifest.toml can predate a dep
        # added at this tag, and instantiate then dies with "project and
        # manifest out of sync". Drop untracked leftovers only: deleting a file
        # the tag TRACKS (old tags shipped julia/pluto/Manifest.toml) would
        # dirty a checkout that is read-only by convention and break the next
        # update's dirty-tree refusal. Same guard as install.sh's own loop.
        foreach ($envName in @('kernel', 'repl', 'pluto')) {
            $man = Join-Path $repoCurrent "julia\$envName\Manifest.toml"
            if (-not (Test-Path -LiteralPath $man)) { continue }
            git -C $repoCurrent ls-files --error-unmatch "julia/$envName/Manifest.toml" 2>&1 | Out-Null
            if ($LASTEXITCODE -ne 0) {
                Write-SupLog "install layout: dropping stale julia\$envName\Manifest.toml (fresh resolve at this tag)"
                Remove-Item -LiteralPath $man -Force -ErrorAction SilentlyContinue
            }
        }
        Set-LaunchStatus 'Instantiating Julia environments (first launch, a few minutes)...'
        # The same three commands in the same order as install.sh. julia\pluto
        # also precompiles and loads Pluto -- the slowest of the three, and the
        # one whose first use is a user-visible page load.
        foreach ($step in @(
                @{ Env = 'kernel'; Code = 'using Pkg; Pkg.instantiate()' },
                @{ Env = 'repl';   Code = 'using Pkg; Pkg.instantiate()' },
                @{ Env = 'pluto';  Code = 'using Pkg; Pkg.instantiate(); Pkg.precompile(); using Pluto' }
            )) {
            $project = Join-Path $repoCurrent "julia\$($step.Env)"
            if (-not (Test-Path -LiteralPath $project)) {
                Write-SupLog "install layout: julia\$($step.Env) is not in this checkout - skipping"
                continue
            }
            Write-SupLog "install layout: instantiating julia\$($step.Env)"
            # One fully-quoted argument, not a bare+quoted concatenation --
            # unambiguous when the prefix contains a space, which is normal here.
            $projectArg = "--project=$project"
            $julOut = julia $projectArg -e $step.Code 2>&1
            $julExit = $LASTEXITCODE
            if ($julExit -ne 0) {
                # Head-bounded, because Julia writes ERROR and the failing file
                # FIRST. Not routed through Get-FailureExcerpt: that helper is
                # defined in launch-sot.ps1 itself, further down, so it does
                # not exist yet at this point in the run.
                foreach ($l in @($julOut | Select-Object -First 30)) { if ("$l".Trim()) { Write-SupLog "install layout: julia -> $l" } }
                Write-SupLog "install layout: julia\$($step.Env) instantiate FAILED (exit $julExit)"
                $script:launchNotices.Add("julia $($step.Env) env did not instantiate - local REPL/Pluto will not start (see supervisor.log)") | Out-Null
            } else {
                Write-SupLog "install layout: julia\$($step.Env) instantiated"
            }
        }
    } catch {
        Write-SupLog "install layout: unexpected failure - $($_.Exception.Message)"
    } finally {
        $ErrorActionPreference = $savedEAP
    }
}

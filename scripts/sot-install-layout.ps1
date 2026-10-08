# sot-install-layout.ps1 -- shared "is this launch pinned to the installed
# checkout" predicate + shortcut-target rule, and the home of the launcher code
# id (Get-SotLauncherCodeId), which launch-sot.ps1 uses on a converge. Dot-sourced by launch-sot.ps1
# (which gates its self-update prelude on Test-SotPinnedCheckout) and
# install-shortcut.ps1 (which uses Get-SotLauncherTarget to decide what the
# shortcut/pin should point at). Same shape as sot-hosts.ps1: one
# dot-sourceable file, no privileged access either caller doesn't already
# have.
# Also the first-launch install layout (Set-SotJunction, Initialize-InstallLayout), called only by launch-sot.ps1.
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

# The identity of the launcher code in a scripts directory: the SHA-256 of
# launch-sot.ps1 and of the four files it dot-sources, joined with '-', or ''
# when any of them cannot be read. launch-sot.ps1 logs its own at start (the
# "supervisor start" line) and, on a converge, re-invokes the launcher when the
# files on disk have another id (ADR 0017's 0.6.6 amendment). Read through
# repo\current, a flip by sot-apply.ps1 shows here with the path unchanged.
function Get-SotLauncherCodeId {
    param([Parameter(Mandatory)][string]$ScriptsDir)
    try {
        $ids = foreach ($f in @('launch-sot.ps1', 'sot-hosts.ps1', 'sot-install-layout.ps1', 'sot-freshness.ps1', 'sot-lease.ps1')) {
            (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $ScriptsDir $f) -ErrorAction Stop).Hash
        }
        return ($ids -join '-')
    } catch {
        return ''
    }
}

# ---------------------------------------------------------------------------
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
# ---------------------------------------------------------------------------

# Twin of sot-apply.ps1's Set-Junction -- keep the two in step. A directory
# JUNCTION, never a symlink: a symlink needs Developer Mode or an elevated
# shell, a junction needs neither, and every reader only traverses it as a
# directory. Not shared by dot-sourcing: sot-apply.ps1 is fail-open by
# contract on the launch path, and a file it would have to source in order to
# run at all is a new way for it not to run.
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

# A pinned checkout never reaches the create-a-checkout branch below (Test-
# SotPinnedCheckout is true only once repo\current already exists, by
# construction) -- see docs/adr/0030-versioning-release-and-auto-update.md's
# 2026-09-17 amendment.
function Initialize-InstallLayout {
    $stagedSotd = Join-Path $prefixDir 'bin\sotd.exe'
    # Both installer paths delegate folder-trust declaration to the agents-owned offline command;
    # a failure is reported and never described as a successful declaration.
    # Before the dev-box return: its daemon prepares rows too.
    $trustBinary = $stagedSotd
    if ($backendExe -and (Test-Path -LiteralPath $backendExe)) { $trustBinary = $backendExe }
    $trustEAP = $ErrorActionPreference
    try {
        if (-not [System.IO.Path]::IsPathRooted($trustBinary) -or -not (Test-Path -LiteralPath $trustBinary)) {
            Write-SupLog 'install layout: folder trust not declared - no absolute backend binary'
        } else {
            $ErrorActionPreference = 'Continue'
            $trustOutput = & $trustBinary trust declare $env:USERPROFILE 2>&1
            $trustExit = $LASTEXITCODE
            if ($trustExit -ne 0) {
                Write-SupLog "install layout: folder trust not declared (exit $trustExit)"
                foreach ($line in @($trustOutput)) { if ("$line".Trim()) { Write-SupLog "install layout: trust -> $line" } }
            } elseif (("$trustOutput").Trim() -ceq 'Declared') {
                Write-SupLog 'install layout: folder trust declared'
            } elseif (("$trustOutput").Trim() -ceq 'Kept') {
                Write-SupLog 'install layout: folder trust kept'
            } else {
                Write-SupLog 'install layout: folder trust not declared - unexpected command outcome'
            }
        }
    } catch {
        Write-SupLog "install layout: folder trust not declared - $($_.Exception.Message)"
    } finally {
        $ErrorActionPreference = $trustEAP
    }
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
            # or "-dev+<sha>[-dirty]" (rust/protocol/src/version.rs app_version)
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
                # defined further down this file, so it does not exist yet at
                # this point in the run.
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

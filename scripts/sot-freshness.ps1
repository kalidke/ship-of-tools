# sot-freshness.ps1 -- what brings this box's code up to date before a window starts: the armed update, the
# dev pull's rebuild, the comm install. Dot-sourced by launch-sot.ps1; ASCII ONLY in string literals.

# ADR 0042 L2b design D: a running local daemon pins its sotd.exe/
# sot-capsule.exe as mapped images (Windows) -- stop it BEFORE anything
# that might replace those files out from under it. Checked, never
# unconditional: only when an update is actually about to land --
# sot-apply.ps1's own staged-update pointer (about to be consumed by the
# apply call right below) -- never on every launch, which would pay a
# stop/restart on every idle click. -NoUpdate skips it, same as the apply
# step it guards.
#
# Codex follow-up: SOT_LAUNCH_REBUILD (set by the self-update prelude on a
# successful git pull) used to also trigger this -- deleted, and still
# correctly excluded: the dev freshness rebuild block SOT_LAUNCH_REBUILD
# guards used to be `cargo build --release -p sot-frontend` ONLY, so
# stopping the local daemon for it bought nothing but a pointless
# stop/restart on every launch that pulled fresh source. That block now
# ALSO rebuilds the sotd.exe/sot-capsule.exe pair (2026-09-02 field report
# -- see its own comment further below), but it stops the daemon itself,
# right before ITS OWN pair rebuild -- gating THIS earlier stop on
# SOT_LAUNCH_REBUILD too would just double the stop/restart for no benefit.
#
# A function, not inline, so an exit-76 CONVERGE respawn (see the do/while
# loop) can call it too: a PINNED install's ONLY apply path used to be this
# top-level call, so an update armed while the supervisor was already
# resident (relaunch-sot.ps1 -Converge, or the frontend's own update
# banner) sat unapplied until the box's next full process start --
# Invoke-SelfUpdatePrelude is a no-op for a pinned install ("updates arrive
# via sot-apply, no pull"), so nothing else in the converge path ever
# invoked sot-apply.ps1 (2026-09-18 field report).
# -Handover marks the ONE pass that is finishing an update a previous pass
# already applied: it skips the apply, and with it the daemon stop that exists
# only to serve the apply. It is a parameter and not a read of $script:reexecd
# because "is this a handover" is a fact about a CALL, while the supervisor
# outlives the pass and calls this again on every converge. Reading the latch
# here made a converge inherit the first pass's answer and never apply; a
# consumed latch instead freed the handover gate below to re-exec forever.
# Neither failure is reachable once the caller states which kind of call it is.
function Invoke-PendingApply {
    param([switch]$Handover)

    # The stop exists ONLY to keep the daemon from pinning a stale binary while
    # sot-apply swaps it, so a pass that will not apply must not bounce it.
    if (-not $NoUpdate -and -not $Handover -and (Test-Path $sotLocalDaemon)) {
        $updatePending = Test-Path (Join-Path $prefixDir 'updates\pending-windows-x86_64.json')
        if ($updatePending) {
            Write-SupLog 'local daemon: stopping before apply so it does not pin a stale binary'
            $stopOut = & $sotLocalDaemon -Stop 6>&1 2>&1
            foreach ($l in @($stopOut)) { if ("$l".Trim()) { Write-SupLog "$l" } }
        }
    }

    # A re-exec'd pass (post-apply handover) keeps the marker the first pass
    # left and does not run sot-apply again: $script:sotJustApplied below
    # then reads true from that marker, so the comm update and the
    # crash-loop window follow the apply on the fresh-launch path too.
    if (-not $NoUpdate -and -not $Handover -and (Test-Path $sotApply)) {
        Remove-Item -Path $applyMarker -Force -ErrorAction SilentlyContinue
        Set-LaunchStatus 'Applying update...'
        $applyOut = & $sotApply 6>&1 2>&1
        foreach ($l in @($applyOut)) { if ("$l".Trim()) { Write-SupLog "$l" } }
    }
    # Read ONCE, right after the apply attempt above -- Invoke-FreshnessPass
    # and the migration/handover block below both need "did an update just
    # land", and the marker FILE itself stays present for the rest of this
    # launch (the crash-loop read further down needs that), so a $script:
    # flag is the one-shot signal that gets CONSUMED (see
    # Invoke-FreshnessPass) instead.
    $script:sotJustApplied = Test-Path -LiteralPath $applyMarker
}

# Dev-freshness (maintainer note, 2026-07-06: "launcher should always update to newest
# build on startup" — the maintainer's FE booted a stale 0.2.1-dev). Pull + rebuild the
# FRONTEND before staging. FAIL-OPEN at every step: pull failure (offline,
# conflict) or build failure (broken main) logs to the supervisor log and
# launches the existing staged/dev binary — a broken update path must never
# brick the launcher. Every non-fatal tool failure below logs and notices
# Get-FailureExcerpt's head-anchored excerpt (the tool's actual message),
# never a raw stderr tail. -NoUpdate skips. -Local skips too (2026-09-02 Codex
# round): the self-update prelude only ever SETS SOT_LAUNCH_REBUILD when
# -not $Local, but the var is process environment, not a fresh local --
# an inherited '1' from an earlier invocation in the same shell must not
# make a later -Local run rebuild (and stop the daemon for it); -Local
# is documented as a freshness-free debug path with no exception.
#
# Invoke-FreshnessPass wraps the WHOLE pass below as one function, its own
# SOT_LAUNCH_REBUILD/-NoUpdate/-Local gate included, so an exit-76 CONVERGE
# respawn can re-run it right after Invoke-SelfUpdatePrelude -- the same two
# calls the very first launch makes below, just repeated mid-loop. One code
# path, no copy. See the do/while loop and relaunch-sot.ps1 -Converge.
# ---------------------------------------------------------------------------
#
# Get-FailureExcerpt -- a failing tool's MESSAGE, not its trace tail, reaches
# the log and the notice. `Select-Object -Last 3` used to feed both, and for
# Julia the last 3 lines of stderr are the BOTTOM of a stack trace (bare
# frame numbers, "@ Base .\client.jl:550") -- never the exception message or
# the failing file name, which Julia writes FIRST. A real update_comm()
# failure (2026-09-04, a script deleted mid-install) was diagnosable only
# from file mtimes because of this. One rule per tool family:
#   julia   -- from the first '^ERROR:' line (or line 1 if none matches)
#              forward, bounded to $Lines lines.
#   cargo   -- every line starting with 'error' (cargo's own marker: plain
#              'error:' or 'error[E....]'), bounded to $Lines lines.
#   generic -- first 5 non-empty lines plus the last 2 (head carries the
#              message, tail carries where it stopped); also the fallback
#              when a julia/cargo output has no line the rule above matches.
# Returns the excerpt as a string array for the log; callers take element
# [0] (through Limit-NoticeText) for the launch notice.
function Get-FailureExcerpt {
    param(
        [Parameter(Mandatory)] $Output,
        [ValidateSet('julia', 'cargo', 'generic')][string]$Kind = 'generic',
        [int]$Lines = 6
    )
    $text = @($Output | ForEach-Object { "$_" })
    $excerpt = @()
    if ($Kind -eq 'julia' -and $text.Count -gt 0) {
        $start = 0
        for ($i = 0; $i -lt $text.Count; $i++) {
            if ($text[$i] -match '^ERROR:') { $start = $i; break }
        }
        $excerpt = @($text[$start..([Math]::Min($text.Count, $start + $Lines) - 1)])
    } elseif ($Kind -eq 'cargo') {
        $excerpt = @($text | Where-Object { $_ -match '^error' } | Select-Object -First $Lines)
    }
    if (-not $excerpt -or $excerpt.Count -eq 0) {
        $nonEmpty = @($text | Where-Object { "$_".Trim() })
        $excerpt = @($nonEmpty | Select-Object -First 5) + @($nonEmpty | Select-Object -Last 2)
    }
    if (-not $excerpt -or $excerpt.Count -eq 0) { $excerpt = @('(no output captured)') }
    return $excerpt
}

# Bounds a notice message to ~160 chars -- $script:launchNotices is joined
# into one SOT_LAUNCH_NOTICE env var for the frontend; one runaway line must
# not crowd out every other notice.
function Limit-NoticeText {
    param([string]$Text, [int]$MaxLength = 160)
    $t = "$Text".Trim()
    if ($t.Length -le $MaxLength) { return $t }
    return $t.Substring(0, $MaxLength) + '...'
}
# Comm layer install/refresh (converge follow-up, 2026-09-03): a converged
# box carries fresh Rust binaries but a STALE ~/.sot-comm/bin + Claude/Codex
# skill set if this step is skipped -- ShipTools.update_comm() is the one
# place that deploys comm/ scripts and skills (scripts/install.sh's own
# julia_run call does the same thing at install time; see docs/src/start/
# install.md). Shared by both of Invoke-FreshnessPass's branches below (a
# dev pull and a pinned release install) -- the trigger differs, the call
# does not. Non-fatal: no julia on PATH is the NORMAL state for a pure
# FE-client box (sot-setup SKILL.md's no-Julia fallback) and only logs;
# julia present but the command itself failing is unusual enough to also
# join the launch notice.
function Invoke-CommUpdate {
    $juliaCmd = Get-Command julia -ErrorAction SilentlyContinue
    if (-not $juliaCmd) {
        Write-SupLog "freshness: no julia on PATH - skipping comm install (FE-client box, this is normal)"
        return
    }
    Write-SupLog "freshness: julia -e ShipTools.update_comm()"
    # One fully-quoted argument, not an adjacent bare+quoted concatenation --
    # unambiguous if $repo.Path ever contains a space. $repo IS repo\current
    # on a pinned checkout (PSScriptRoot resolves through it), so comm
    # scripts and skills follow the installed tag with no separate wiring.
    $juliaProjectArg = "--project=$($repo.Path)"
    $commOut = julia $juliaProjectArg -e 'using ShipTools; ShipTools.update_comm()' 2>&1
    if ($LASTEXITCODE -ne 0) {
        $commExcerpt = @(Get-FailureExcerpt -Output $commOut -Kind 'julia')
        Write-SupLog "freshness: ShipTools.update_comm() FAILED (non-fatal). message: $($commExcerpt -join ' / ')"
        $script:launchNotices.Add("comm install failed: $(Limit-NoticeText $commExcerpt[0])") | Out-Null
    } else {
        Write-SupLog "freshness: ShipTools.update_comm() ok"
    }
}

function Invoke-FreshnessPass {
    if ($NoUpdate -or $Local) { return }
    if ($script:sotPinned) {
        # Nothing here to pull or rebuild. Comm still needs to follow the
        # tag (ADR 0030's deferred "update_comm on auto-apply" item, closed
        # by the 2026-09-17 amendment): triggered by an update this launch
        # just applied or a comm install that is simply missing, never by
        # "a pull succeeded". $script:sotJustApplied is CONSUMED (read then
        # cleared) here, not re-read from the marker file: an exit-76
        # converge calls this function again later in the SAME launch, and
        # that is still the one apply, not a second update -- Julia must
        # not run twice for it.
        $commHome = if ($env:SOT_COMM_HOME) { $env:SOT_COMM_HOME } else { Join-Path $env:USERPROFILE '.sot-comm' }
        $commMissing = -not (Test-Path -LiteralPath (Join-Path $commHome 'bin'))
        $justApplied = $script:sotJustApplied
        $script:sotJustApplied = $false
        if ($justApplied -or $commMissing) {
            # A converge whose apply left the launcher code unchanged does
            # not re-invoke the launcher (the converge block in the do/while
            # loop re-invokes only on a code id change), so a tag that just
            # landed here via sot-apply.ps1 can still be uninstantiated: the
            # Windows updater's prepare step never runs Pkg.instantiate()
            # (rust/frontend/src/selfupdate.rs's PrepareSpec always sets
            # julia_bin: None), so prepared.json carries
            # julia_instantiated: false for every Windows tag. Without this,
            # Invoke-CommUpdate below ran `using ShipTools` against an
            # uninstantiated checkout, and its stderr -- turned into
            # terminating errors by $ErrorActionPreference = 'Stop' + 2>&1,
            # same footgun as the dev-checkout branch below -- killed the
            # launcher silently right here. Initialize-InstallLayout is
            # idempotent (it returns immediately once
            # julia\kernel\Manifest.toml exists), so calling it again is the
            # converge-path equivalent of the fresh-path handover: free on
            # the common already-instantiated case, and it does the missing
            # instantiate here otherwise.
            Initialize-InstallLayout
            # Relax 'Stop' -> 'Continue' around update_comm() itself too --
            # belt-and-suspenders with the instantiate above, and the same
            # relaxation the dev-checkout branch below already uses. Never
            # fatal: log and move on, don't let a freshness step brick the
            # launch.
            $savedEAP = $ErrorActionPreference
            $ErrorActionPreference = 'Continue'
            try {
                Invoke-CommUpdate
            } catch {
                Write-SupLog "freshness: ShipTools.update_comm() threw (non-fatal): $($_.Exception.Message)"
            } finally {
                $ErrorActionPreference = $savedEAP
            }
        } else {
            Write-SupLog 'freshness: pinned checkout - comm already installed and nothing just applied, skipping'
        }
        return
    }
    if ($env:SOT_LAUNCH_REBUILD -ne '1') { return }
    # The git pull moved to the self-update prelude at the top; here we only
    # REBUILD, and only when that pull succeeded (the SOT_LAUNCH_REBUILD marker)
    # so exactly one cargo build runs in the final invocation. Consume the marker.
    Remove-Item Env:\SOT_LAUNCH_REBUILD -ErrorAction SilentlyContinue
    # $ErrorActionPreference is 'Stop', but cargo prints "Finished ..." to stderr,
    # which under 'Stop' + 2>&1 in PS 5.1 turns every stderr line into a
    # terminating NativeCommandError (the "taskbar launcher does nothing"
    # regression, f8fdf81). Gate on $LASTEXITCODE, not thrown errors; restore after.
    $savedEAP = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        # Gated the same as the rest of this pass -- "the pull succeeded"
        # (SOT_LAUNCH_REBUILD, checked above) stands in for "the pull
        # changed anything"; update_comm() is idempotent, so running it on
        # a no-op pull is harmless and there is no cheaper signal available
        # here.
        Invoke-CommUpdate

        # Probe for cargo FIRST. Without this the missing-toolchain case is
        # reported as a SUCCESS: PowerShell raises CommandNotFoundException
        # (which 2>&1 captures into $buildOut) but leaves $LASTEXITCODE at 0
        # from the preceding successful `git` call, so the `-ne 0` test below
        # takes the else-branch and logs "frontend rebuilt" having built
        # nothing. This branch only runs on a non-pinned (dev/clone) checkout
        # now -- a pinned release install returns above and never reaches
        # this probe at all -- but a dev checkout can still lack a Rust
        # toolchain (e.g. a box that only ever clicks the shortcut), so this
        # is not an error -- just say so and run whatever binaries exist.
        $cargoCmd = Get-Command cargo -ErrorAction SilentlyContinue
        if (-not $cargoCmd) {
            Write-SupLog "freshness: no cargo on PATH - nothing to rebuild from source here, running the existing binaries (this is normal on a dev checkout without a Rust toolchain)"
            Set-LaunchStatus 'Starting Ship of Tools...'
            $buildOut = $null
        } else {
        Set-LaunchStatus 'Rebuilding frontend...'
        Write-SupLog "freshness: cargo build -p sot-frontend"
        $buildOut = cargo build --release -p sot-frontend --manifest-path (Join-Path $repo 'rust\Cargo.toml') 2>&1
        if ($LASTEXITCODE -ne 0) {
            $buildExcerpt = @(Get-FailureExcerpt -Output $buildOut -Kind 'cargo')
            Set-LaunchStatus 'ERROR: frontend rebuild failed - launching existing build (see supervisor.log)'
            Write-SupLog "freshness: BUILD FAILED - launching existing binary. message: $($buildExcerpt -join ' / ')"
            $script:launchNotices.Add("frontend rebuild failed - running the existing build (see supervisor.log): $(Limit-NoticeText $buildExcerpt[0])") | Out-Null
        } else {
            Write-SupLog "freshness: frontend rebuilt"
        }

        # Backend pair (sotd.exe, sot-capsule.exe) -- a SECOND cargo
        # invocation, always attempted after the frontend one above
        # regardless of its outcome (independent packages), and NON-FATAL:
        # a failure here logs and the launch continues with whatever pair
        # already exists. 2026-09-02 field report: this rebuild used to be
        # frontend-only, so a dev box's local sotd.exe/sot-capsule.exe went
        # stale for weeks -- a COMPLETE but pre-0.6 pair with no Windows
        # pipe derivation, which sot-local-daemon.ps1 then misreported as
        # an ABSENT pair rather than a stale one (see its own header and
        # the diagnostic split there).
        #
        # The pair spans TWO packages, not one: `sotd` is sot-backend's own
        # [[bin]], `sot-capsule` is an auto-discovered src/bin of sot-log --
        # building sot-backend alone is NOT enough to produce sot-capsule.exe.
        #
        # Stop-first is REQUIRED here, not merely prudent: a RUNNING local
        # daemon pins both files as mapped images on Windows -- reproduced
        # on the reporting box, with sotd.exe running from this same
        # target\release, `cargo build --release -p sot-backend` failed
        # with "Access is denied. (os error 5)" and left the old binaries
        # in place. The "Local daemon ensure" step right after this whole
        # freshness block (the single per-launch ensure, see its own comment) restarts
        # it on whatever pair is current once this rebuild is done -- so on
        # a dev box, every launch that rebuilds also restarts the local
        # daemon. A converge respawn (exit 76) re-ensures it too, right
        # after calling this function -- see the do/while loop.
        #
        # Pair rule: the daemon is stopped (it pins sotd.exe) and the FULL
        # pair is always rebuilt. A sot-capsule.exe pinned by live capsule
        # supervisors -- long-lived by design, never stopped here -- is
        # renamed aside first (below); the daemon itself refuses to spawn a
        # capsule from a binary of another build (its pair guard), so a
        # half-updated pair can no longer produce foreign rows silently.
        $devBinDir = Split-Path $backendExe -Parent
        $capsuleSessionsAlive = @(Get-CimInstance Win32_Process -Filter "Name='sot-capsule.exe'" -ErrorAction SilentlyContinue |
            Where-Object {
                ($_.ExecutablePath -and $_.ExecutablePath -like "$devBinDir*") -or
                ($_.CommandLine -and $_.CommandLine -like "*$devBinDir*")
            })
        if (Test-Path $sotLocalDaemon) {
            Write-SupLog 'freshness: stopping local daemon before backend rebuild (a running sotd.exe pins its own image)'
            $stopOut2 = & $sotLocalDaemon -Stop 6>&1 2>&1
            foreach ($l in @($stopOut2)) { if ("$l".Trim()) { Write-SupLog "$l" } }
        }
        # Stale images renamed aside by an earlier pass (below): a mapped
        # one refuses deletion and is left for a later pass; a free one goes.
        Get-ChildItem -Path (Join-Path $devBinDir 'sot-capsule-stale-*.exe'), (Join-Path $devBinDir 'deps\sot_capsule-stale-*.exe') -ErrorAction SilentlyContinue |
            Remove-Item -Force -ErrorAction SilentlyContinue
        if ($capsuleSessionsAlive.Count -gt 0) {
            # Live supervisors pin sot-capsule.exe, so an in-place rebuild
            # fails -- and the old answer ("end those workspaces to update
            # it") was a deadlock: a capsule of another build can neither
            # be attached nor destroyed by the new daemon (field day
            # 2026-09-05). Windows permits RENAMING a mapped image, which
            # frees the canonical path for the pair build; the running
            # supervisors keep executing the renamed file until their
            # workspaces end or their processes are killed (the daemon then
            # respawns them from the new binary, journals recovering).
            $staleStamp = Get-Date -Format 'yyyyMMdd-HHmmss'
            $staleName = "sot-capsule-stale-$staleStamp.exe"
            $renamedAside = $null
            try {
                Rename-Item -Path (Join-Path $devBinDir 'sot-capsule.exe') -NewName $staleName -ErrorAction Stop
                $renamedAside = Join-Path $devBinDir $staleName
                # cargo HARD-LINKS target\release\sot-capsule.exe to
                # target\release\deps\sot_capsule.exe, and the linker writes
                # the deps name first. The live supervisors' mapped image is
                # therefore still reachable -- and locked -- under deps\ after
                # the rename above, and the pair build dies with LNK1104
                # "cannot open file ...deps\sot_capsule.exe" (2026-09-08). Move
                # that name aside too; both stale names are swept above on the
                # next pass once nothing maps them.
                $depsImage = Join-Path $devBinDir 'deps\sot_capsule.exe'
                if (Test-Path $depsImage) {
                    Rename-Item -Path $depsImage -NewName "sot_capsule-stale-$staleStamp.exe" -ErrorAction Stop
                }
                Write-SupLog "freshness: sot-capsule.exe pinned by $($capsuleSessionsAlive.Count) live supervisors - renamed aside as $staleName (and deps\sot_capsule.exe likewise); rebuilding the full pair"
                $script:launchNotices.Add("$($capsuleSessionsAlive.Count) running capsule supervisor(s) are on the previous build and cannot attach to this frontend: end them from a frontend of that build, or kill only their 'sot-capsule supervise' process and attach the row again (the run leg and its agent survive and are adopted)") | Out-Null
            } catch {
                Write-SupLog "freshness: could not rename the pinned sot-capsule.exe aside ($($_.Exception.Message)); the pair build below will refresh sotd.exe only"
                $script:launchNotices.Add("sot-capsule.exe is pinned and could not be renamed aside; kill the running sot-capsule.exe processes and relaunch") | Out-Null
            }
        }
        # The full pair build, unconditionally: the only difference a pinned
        # image makes is the rename above.
        Set-LaunchStatus 'Rebuilding backend pair...'
        Write-SupLog "freshness: cargo build -p sot-backend -p sot-log"
            $capOut = cargo build --release -p sot-backend -p sot-log --manifest-path (Join-Path $repo 'rust\Cargo.toml') 2>&1
            if ($LASTEXITCODE -ne 0) {
                $capExcerpt = @(Get-FailureExcerpt -Output $capOut -Kind 'cargo')
                Write-SupLog "freshness: backend pair rebuild FAILED (non-fatal, continuing with whatever pair exists). message: $($capExcerpt -join ' / ')"
                # A renamed-aside image must come back, or there is no
                # sot-capsule.exe at all and the local daemon cannot start.
                # Copying a mapped image is permitted (only overwrite is not).
                if ($renamedAside -and -not (Test-Path (Join-Path $devBinDir 'sot-capsule.exe'))) {
                    try {
                        Copy-Item -Path $renamedAside -Destination (Join-Path $devBinDir 'sot-capsule.exe') -ErrorAction Stop
                        Write-SupLog "freshness: restored the previous sot-capsule.exe from $staleName after the failed build"
                    } catch {
                        Write-SupLog "freshness: could NOT restore sot-capsule.exe from $staleName ($($_.Exception.Message)) - no capsule binary until the next successful build"
                    }
                }
                $script:launchNotices.Add("backend rebuild failed - running the existing sotd.exe/sot-capsule.exe pair: $(Limit-NoticeText $capExcerpt[0])") | Out-Null
            } else {
                Write-SupLog "freshness: backend pair (sotd.exe, sot-capsule.exe) rebuilt"
                # The supervisors that pinned the old image are now one build
                # behind the pair: the daemon's pair guard refuses them, every
                # attach fails as ForeignPipe, and the rows sit unusable until
                # someone kills the supervisors by hand (2026-09-08, twice in
                # one evening). Do it here, before the daemon ensure: compare
                # the two images' build ids (the same id the pair guard
                # compares), and when they differ end ONLY the 'supervise'
                # processes -- never a run leg or the agent inside it, never
                # an endrun/reset in flight. The legs survive; the first
                # attach of each row spawns a fresh supervisor that adopts.
                if ($renamedAside -and (Test-Path $renamedAside)) {
                    $newId = & (Join-Path $devBinDir 'sot-capsule.exe') build-id 2>$null | Select-Object -First 1
                    $oldId = & $renamedAside build-id 2>$null | Select-Object -First 1
                    if ($newId -and $oldId -and "$newId" -ne "$oldId") {
                        foreach ($cp in $capsuleSessionsAlive) {
                            if (-not $cp.CommandLine -or $cp.CommandLine -notmatch '"\s+supervise\s') { continue }
                            $row = if ($cp.CommandLine -match 'ws-[A-Za-z0-9-]+') { $Matches[0] } else { '?' }
                            try {
                                Stop-Process -Id $cp.ProcessId -Force -ErrorAction Stop
                                Write-SupLog "freshness: ended old-build supervisor pid=$($cp.ProcessId) row=$row ($oldId -> $newId); its leg and agent survive, the first attach adopts them"
                            } catch {
                                Write-SupLog "freshness: could not end old-build supervisor pid=$($cp.ProcessId) row=$row ($($_.Exception.Message))"
                            }
                        }
                    } else {
                        Write-SupLog "freshness: pair build id unchanged ($newId) - live supervisors kept"
                    }
                }
            }
        }
    } finally {
        $ErrorActionPreference = $savedEAP
    }
}

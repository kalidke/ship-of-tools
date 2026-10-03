# launch-sot.ps1 — default launcher: connect to every declared host at once
# (ADR 0042 L2b) -- the local machine's own daemon, always, plus one ssh
# child per dialable host `sotd topology plan --self <host>` names (topology
# plan, lane D; C3, isolation-plan.md §3, opens no port). Pass `-Local` for
# a debug path that skips freshness and dials only the local daemon.
#
# Idempotent on the backend side: each remote's backend is started once via
# `systemctl --user` and survives across launches, so the second click is
# fast. The frontend spawns its own ssh child per remote host and reconnects
# it on its own backoff — this launcher opens nothing on the remote's
# behalf.
#
# ADR 0042 L2b: the local sotd (a fixed per-user named pipe -- "the frontend
# machine runs its own sotd") is ensured on EVERY launch, not just -Local,
# right after the staged-update apply and before either mode's frontend
# launch; see scripts/sot-local-daemon.ps1 and design D in the ADR. Design
# E is `New-RemoteEnsureCommand`, run for the PRIMARY remote
# (`$backendHost`, env vars can still override which host that is): with
# the local connection always present, an unreachable or unconfigured
# default remote is NONFATAL too now (codex follow-up) -- every
# remote-ensure step is skipped with a log line on failure, never a hard
# stop, except the one dialog for "nothing at all could be reached" right
# before the frontend launches.
#
# Overrides (env vars):
#   SOT_HOST_NAME    Which declared host is the PRIMARY (default: the plan's hub)
#   SOT_HOST         Same, pre-topology-plan name (still honoured)
#   SOT_TOKEN        App-level auth token for TCP fallback only
#
# Every OTHER dialable host `sotd topology plan` names is passed to the
# frontend as its own `--dial` entry too (built straight from the plan,
# same as the primary) -- the frontend's own ssh child per host is what
# reaches it, not a per-host tunnel this launcher manages.
#
# Logs land at %LOCALAPPDATA%\sot\logs\ so disconnect / reconnect
# events can be diagnosed without keeping a console window around.

[CmdletBinding()]
param(
    [switch]$Local,
    # Pass --relaunched to the frontend on the *first* launch. The frontend
    # sets this itself across the self-relaunch respawn loop (exit code 75);
    # this switch is for bootstrapping straight into a resumed terminal
    # (e.g. the first migration onto the supervisor). See ADR 0017.
    [switch]$Relaunched,
    # Skip the launch-time FRONTEND freshness pass (git pull + cargo rebuild).
    # For offline starts or when you deliberately want the stale binary.
    [switch]$NoUpdate,
    # Force a full pull+rebuild+restart of the SHARED remote daemon (the
    # canonical scripts/restart-backend.sh). Default launches never restart a
    # running daemon — other FEs' kernels/REPLs die with it. ADR 0030
    # dev-freshness rev 2.
    [switch]$RestartBackend
)

$ErrorActionPreference = 'Stop'

# Captured NOW, at top-level script scope, so Invoke-SelfUpdatePrelude's
# rare self-re-exec branch (the launcher script itself changed under us) can
# pass the ORIGINAL -Local/-Relaunched/-NoUpdate/-RestartBackend switches
# through to the fresh copy. $PSBoundParameters is an automatic variable
# scoped to whatever's currently executing -- read from inside a function it
# means that FUNCTION's own bound params (just -AllowReexec), not the
# script's. Capture the script's copy before any function can shadow it.
$script:LaunchBoundParameters = $PSBoundParameters

# How many launcher copies this process has run: each in-process re-invoke
# (the prelude's, the post-apply handover's, a converge's) nests one. The
# "supervisor start" line logs it with the working set (ADR 0017, 0.6.6).
$global:SotLauncherDepth = 1 + [int]$global:SotLauncherDepth

# AUTHORING GOTCHA (Windows PowerShell 5.1): this file has no BOM, so the 5.1
# parser decodes it as ANSI/cp1252. A UTF-8 non-ASCII char (em-dash, curly
# quote, etc.) is harmless inside a "#" comment (runs to end-of-line) but inside
# a "string literal" its bytes mojibake into a phantom double-quote that corrupts
# parsing and fails the WHOLE launcher to load. Keep STRING LITERALS ASCII-only
# (use '-' not an em-dash in status text); prose em-dashes live in comments only.

$repo = Resolve-Path -Path (Join-Path $PSScriptRoot '..')
# Moved up from its old spot further down (needed by the pinned-checkout
# predicate right below); every other use of it is unchanged.
$prefixDir = Join-Path $env:LOCALAPPDATA 'sot'

# Get-SotTopologyPlan (topology plan, lane D: `sotd topology plan --self
# <host>` is the one parser now) lives in one dot-sourceable file shared
# with shutdown-sot.ps1 -- see that file's own header.
. (Join-Path $PSScriptRoot 'sot-hosts.ps1')

# Test-SotPinnedCheckout / Get-SotLauncherTarget / Get-SotLauncherCodeId --
# shared with install-shortcut.ps1 and the tests; see that file's header.
. (Join-Path $PSScriptRoot 'sot-install-layout.ps1')

# The launcher code this process parsed: launch-sot.ps1 and the two files
# dot-sourced above, read before anything below can change them on disk. The
# "supervisor start" line logs it; a converge compares it with the files on
# disk and re-invokes the launcher when they differ (the do/while loop).
$script:launcherCodeId = Get-SotLauncherCodeId -ScriptsDir $PSScriptRoot

# Computed ONCE, before any self-update/freshness/layout decision below.
# See docs/adr/0030-versioning-release-and-auto-update.md's 2026-09-17
# amendment for what this predicate replaced and why.
$script:sotPinned = Test-SotPinnedCheckout -RepoPath $repo.Path -Prefix $prefixDir

# Logs FIRST — so the progress splash and status writes can come up before any
# slow pull/build/ssh work. Append-only supervisor log: unlike the frontend
# stdout/stderr logs (which Start-Process truncates on every respawn), this
# survives across exit-75 respawns so the relaunch path — frontend exit codes,
# restage, respawn, tunnel flaps — is diagnosable after the fact. ADR 0017.
$logDir = Join-Path $env:LOCALAPPDATA 'sot\logs'
New-Item -ItemType Directory -Force -Path $logDir | Out-Null
$frontendStdout = Join-Path $logDir 'frontend.stdout.log'
$frontendStderr = Join-Path $logDir 'frontend.stderr.log'
$supervisorLog  = Join-Path $logDir 'supervisor.log'
# The frontend's stdout/stderr are TRUNCATED by Start-Process on every spawn,
# which erased the evidence of a pane freeze the moment the user relaunched to
# recover from it. Keep exactly one previous generation as .prev (both spawn
# sites below call this first); a crash-loop still overwrites .prev each time,
# which is the right bound for a log that is only ever read by hand.
function Rotate-FrontendLogs {
    foreach ($f in @($frontendStdout, $frontendStderr)) {
        if (Test-Path $f) { Move-Item -Path $f -Destination "$f.prev" -Force -ErrorAction SilentlyContinue }
    }
}
function Write-SupLog {
    param([string]$Message)
    try {
        "$(Get-Date -Format o)  pid=$PID  $Message" |
            Out-File -FilePath $supervisorLog -Append -Encoding utf8
    } catch { }
}

# ---------------------------------------------------------------------------
# Single-instance lock (2026-09-08 incident): a shortcut launch 90 s into a
# resident supervisor's exit-76 converge ran the WHOLE prelude + freshness
# pass concurrently -- two cargo builds raced on one target dir, the pair link
# failed (LNK1104), the loser found the winner's control port open and opened
# no tunnel, then tore the winner's tunnel down on exit: a frontend with no
# backend at all. One launcher per user: the lock file names the live
# supervisor pid. A second launcher waits briefly for it (a cold relaunch's
# old supervisor is in its last seconds), then exits with a visible message
# -- it never runs a pass of its own. A stale lock (pid gone, or not a
# launcher any more) is simply taken over.
# ---------------------------------------------------------------------------
$launcherLock = Join-Path $logDir 'launcher.pid'
function Get-OtherLauncherPid {
    $other = 0
    try { $other = [int](Get-Content -Path $launcherLock -ErrorAction Stop | Select-Object -First 1) } catch { $other = 0 }
    if ($other -le 0 -or $other -eq $PID) { return 0 }
    $proc = Get-CimInstance Win32_Process -Filter "ProcessId = $other" -ErrorAction SilentlyContinue
    if ($proc -and $proc.CommandLine -and $proc.CommandLine -like '*launch-sot.ps1*') { return $other }
    return 0
}
function Get-LaunchStatusText {
    try { return ([System.IO.File]::ReadAllText((Join-Path $logDir 'launch-status.txt'))).Trim() } catch { return '' }
}
$otherLauncher = Get-OtherLauncherPid
if ($otherLauncher) {
    # The other launcher's status file says what it is doing. DONE means its
    # frontend is up (nothing for us to do); anything else is a launch or a
    # converge in progress -- wait for it to reach DONE or exit, bounded.
    Write-SupLog "another launcher is running (pid $otherLauncher, status: $(Get-LaunchStatusText)) - waiting for it"
    $lockDeadline = (Get-Date).AddMinutes(15)
    while ($otherLauncher -and (Get-Date) -lt $lockDeadline) {
        if ((Get-LaunchStatusText) -eq 'DONE') { break }
        Start-Sleep -Seconds 1
        $otherLauncher = Get-OtherLauncherPid
    }
}
if ($otherLauncher) {
    if ((Get-LaunchStatusText) -eq 'DONE') {
        Write-SupLog "launcher pid $otherLauncher owns the running frontend - nothing to do; exiting"
        exit 0
    }
    Write-SupLog "another launcher is still running (pid $otherLauncher, status: $(Get-LaunchStatusText)) - exiting; it owns the frontend and any converge in progress"
    try {
        Add-Type -AssemblyName System.Windows.Forms
        [System.Windows.Forms.MessageBox]::Show(
            "Ship of Tools is already starting or running (launcher pid $otherLauncher).`nIf it is mid-converge, wait for the window to come back.",
            'Ship of Tools launcher', 'OK', 'Information') | Out-Null
    } catch { }
    exit 2
}
try { [System.IO.File]::WriteAllText($launcherLock, "$PID") } catch { }

# ---------------------------------------------------------------------------
# Launch progress surface (maintainer note, 2026-07-06: "say what it's doing ... or Error").
# The Windows launcher runs hidden, so the dev-freshness pull+rebuild (up to
# ~1-3 min after a big merge) was invisible and read as a dead taskbar click.
# scripts\launch-splash.ps1 is a SEPARATE process (own message pump -> keeps
# animating during the blocking cargo build; a same-thread window would freeze
# to "Not Responding") that renders the current phase from a one-line status
# file. It's spawned FIRST, before any slow work, so there's feedback within
# ~1s of the click. Mirrors the phase text the Linux launcher already echoes to
# its terminal — same vocabulary, per-OS surface. FAIL-OPEN: a splash failure
# never touches the launch. Set-LaunchStatus writes the file (no BOM — the
# splash string-matches DONE/ERROR:) and mirrors to the supervisor log.
# ---------------------------------------------------------------------------
$statusFile = Join-Path $logDir 'launch-status.txt'
function Set-LaunchStatus {
    param([string]$Message)
    try { [System.IO.File]::WriteAllText($statusFile, $Message) } catch { }
    Write-SupLog "status: $Message"
}
Set-LaunchStatus 'Starting Ship of Tools...'
# A function, not a one-shot: an exit-76 converge (see the do/while loop)
# spawns it again, because the splash exits itself on DONE and the converge's
# pull + rebuild + daemon ensure otherwise run for minutes with no window at
# all (2026-09-08: the owner read that as a dead launch and clicked the
# shortcut, which double-ran the converge).
function Start-Splash {
    try {
        $script:splash = Start-Process -FilePath 'powershell.exe' `
            -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-WindowStyle', 'Hidden',
                '-File', (Join-Path $PSScriptRoot 'launch-splash.ps1'), '-StatusFile', $statusFile) `
            -WindowStyle Hidden -PassThru
    } catch { $script:splash = $null }
}
Start-Splash
function Stop-Splash {
    if ($splash -and -not $splash.HasExited) {
        try { Stop-Process -Id $splash.Id -Force -ErrorAction SilentlyContinue } catch { }
    }
}

# ---------------------------------------------------------------------------
# Self-update prelude (ADR 0032 - launcher self-update gap, 2026-07-13).
# A running .ps1 executes its already-parsed AST, so a git pull that adds e.g.
# a new -L forward to THIS script only takes effect on a fresh PARSE - the
# launch that pulls the change still runs the old port set (the 1241 WGL
# connection-refused incident). Fix: pull FIRST, and if this script itself
# changed, re-invoke the fresh copy IN-PROCESS (a re-parse, not a new OS
# process) before any binary/backend/tunnel/FE side effect. Guarded to one
# re-invoke. Fail-open: a failed/absent pull, or a pulled copy that fails the
# parse check, runs the current copy.
#
# One-build handoff: a successful pull sets SOT_LAUNCH_REBUILD so the final
# invocation runs cargo exactly once (the old freshness block, now cargo-only).
# SOT_LAUNCH_REEXEC guards the re-invoke and is cleared just below so neither
# the tunnels nor an exit-75 relaunch inherit it. -Local (a freshness-free debug
# path) and -NoUpdate skip the whole prelude.
#
# $script:sotPinned (computed once near the top of this file) skips ALL of
# the above -- see docs/adr/0030-versioning-release-and-auto-update.md's
# 2026-09-17 amendment.
#
# Refused vs offline (2026-09-03 field report): a pull can fail two different
# ways and they are NOT the same event. OFFLINE means fetch never reached the
# remote - expected on a laptop off wifi, stays quiet (log only). REFUSED means
# git ran against the LOCAL repo and failed - a stale index.lock, a dirty tree,
# a stopped rebase - so this box is silently stuck on an old build while every
# OTHER fail-open step here still reports success. A stale-lock refusal was
# observed logging "Offline or dirty tree" and launching the old binary with
# nothing on screen saying the box never updated - fine for one laptop, a
# false "converged" reading fleet-wide otherwise. A REFUSED pull's first
# error line lands in $script:launchNotices below, joined into
# $env:SOT_LAUNCH_NOTICE (see Set-LaunchNoticeEnv) so the frontend renders
# it at its own startup - offline still only logs, same as before.
# ---------------------------------------------------------------------------
# Invoke-SelfUpdatePrelude is the WHOLE prelude above as one function, so an
# exit-76 CONVERGE respawn (docs/adr/0017-frontend-self-relaunch.md's 76
# amendment; see the do/while loop and relaunch-sot.ps1 -Converge) can re-run
# the identical pull + classification -- one code path, no copy. -AllowReexec
# is passed ONLY by the very first, top-level call below, which re-invokes the
# pulled launcher at once. A converge calls this without it: the converge
# block in the do/while loop decides by code id (Get-SotLauncherCodeId), once
# its apply has run too, and re-invokes the launcher there.
#
# $script:launchNotices collects every line worth surfacing in the frontend's
# one-line startup notice (a refused pull, a failed comm install, a pinned
# capsule -- see Invoke-FreshnessPass and Set-LaunchNoticeEnv below): the
# frontend renders $env:SOT_LAUNCH_NOTICE as a single status string
# (gpu.rs's FeCommand::Notify arm), so multiple problems join with "; "
# rather than teaching it a list. Reset at the start of every prelude call
# (each launch, and each converge) so a resolved problem stops repeating.
$script:launchNotices = [System.Collections.Generic.List[string]]::new()

function Invoke-SelfUpdatePrelude {
    param(
        [switch]$AllowReexec
    )
    $script:launchNotices.Clear()
    if ($script:sotPinned) {
        # See docs/adr/0030-versioning-release-and-auto-update.md's
        # 2026-09-17 amendment: nothing here for a git pull to refresh.
        Write-SupLog "self-update: pinned checkout ($($repo.Path)) - updates arrive via sot-apply, no pull"
        if ($env:SOT_LAUNCH_REEXEC) { Remove-Item Env:\SOT_LAUNCH_REEXEC -ErrorAction SilentlyContinue }
        return
    }
    if (-not $NoUpdate -and -not $Local -and -not $env:SOT_LAUNCH_REEXEC -and (Test-Path (Join-Path $repo '.git'))) {
        # Relax 'Stop' -> 'Continue' around native git: its stderr under 'Stop' + 2>&1
        # throws in PS 5.1. Gate on $LASTEXITCODE, not thrown errors (as below).
        $savedEAP = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        try {
            $selfRel = 'scripts/launch-sot.ps1'
            $before = git -C $repo rev-parse "HEAD:$selfRel" 2>$null
            Set-LaunchStatus 'Checking for updates...'
            Write-SupLog 'self-update: git pull --rebase --autostash'
            $pullOut = git -C $repo pull --rebase --autostash 2>&1
            Write-SupLog "self-update: git -> $($pullOut | Select-Object -Last 1)"
            if ($LASTEXITCODE -eq 0) {
                $env:SOT_LAUNCH_REBUILD = '1'   # pull ok -> final invocation builds once
                $after = git -C $repo rev-parse "HEAD:$selfRel" 2>$null
                if ($after -and $before -and ($after -ne $before)) {
                    # This launcher changed under us. Syntax-check the pulled copy by
                    # blob-OID diff before handing over - a broken-but-successful pull
                    # must not brick the launch (fail-open beats re-parsing garbage).
                    $tokens = $null
                    $parseErrors = $null
                    [System.Management.Automation.Language.Parser]::ParseFile(
                        $PSCommandPath, [ref]$tokens, [ref]$parseErrors) | Out-Null
                    if ($parseErrors -and $parseErrors.Count -gt 0) {
                        Write-SupLog "self-update: pulled launcher has parse errors - staying on current copy: $($parseErrors[0].Message)"
                    } elseif ($AllowReexec) {
                        Write-SupLog 'self-update: launcher changed - re-invoking fresh copy'
                        Stop-Splash   # the fresh invocation spawns its own splash
                        $env:SOT_LAUNCH_REEXEC = '1'
                        # Splat via a plain local copy, not @script:LaunchBoundParameters
                        # directly -- the splat operator's scope-qualifier support is not
                        # worth relying on; a local variable is unambiguous.
                        $reexecParams = $script:LaunchBoundParameters
                        & $PSCommandPath @reexecParams
                        exit $LASTEXITCODE
                    }
                }
            } else {
                # Classify by scanning git's own output text rather than trying to
                # pre-probe the network separately (no second git/ssh round trip,
                # no new failure mode of its own) - fetch failures print a
                # recognizable network-layer message regardless of transport
                # (https or ssh remote); anything else ran against the local repo
                # and is a refusal.
                $pullText = (($pullOut | ForEach-Object { "$_" }) -join "`n")
                $offlinePattern = 'Could not resolve host|Could not read from remote|Connection timed out|Network is unreachable|Could not connect|Operation timed out|Temporary failure in name resolution|No route to host|Connection refused|Host is down|ssh: connect to host'
                if ($pullText -match $offlinePattern) {
                    Set-LaunchStatus 'Offline - launching current build...'
                    Write-SupLog 'self-update: pull failed (offline) - launching existing binary'
                } else {
                    $errLine = ($pullOut | ForEach-Object { "$_" } | Where-Object { $_ -match '^\s*(fatal|error):' } | Select-Object -Last 1)
                    if (-not $errLine) { $errLine = ($pullOut | ForEach-Object { "$_" } | Select-Object -Last 1) }
                    $refusedReason = "$errLine".Trim()
                    Set-LaunchStatus 'Update refused - launching current build...'
                    Write-SupLog "self-update: pull REFUSED - launching existing binary: $refusedReason"
                    $script:launchNotices.Add("self-update: pull refused - running the existing build ($refusedReason)") | Out-Null
                }
            }
        } finally {
            $ErrorActionPreference = $savedEAP
        }
    }
    # The re-invoke guard has served its purpose; clear it so the FE and an
    # exit-75/76 relaunch don't inherit it (a relaunch must self-update afresh).
    if ($env:SOT_LAUNCH_REEXEC) { Remove-Item Env:\SOT_LAUNCH_REEXEC -ErrorAction SilentlyContinue }
}

# Captured ONCE before the prelude clears the env var: a post-apply handover
# (below) re-execs the pinned launcher, and that second pass must not delete
# the just-applied marker or run sot-apply again (the pending pointer is
# already consumed, so sot-apply would exit without rewriting the marker and
# the pass would lose the "an update just landed" fact -- no comm update, no
# crash-loop rollback window; field report, 2026-09-19). It is also the
# guard that stops the handover from looping.
$script:reexecd = [bool]$env:SOT_LAUNCH_REEXEC
Invoke-SelfUpdatePrelude -AllowReexec

Add-Type -AssemblyName System.Windows.Forms   # MessageBox for the fatal dialogs below

$frontendExe = Join-Path $repo 'rust\target\release\sot.exe'
$backendExe = Join-Path $repo 'rust\target\release\sotd.exe'

# Apply any armed pending update BEFORE deciding what to run (ADR 0030 §4).
# This used to be an inline `Move-Item` of a literal
# `updates\pending\sot.exe` — a path NOTHING in the tree has ever written.
# The stager arms `updates\pending-<target>.json` (a pointer) with the bits
# under `<tag>-<target>\`, and sot-apply.ps1 is the consumer that understands
# that contract: it verifies digests + the prepared worktree, swaps binaries
# keeping .prev, flips repo\current, and arms the crash-loop marker. It is
# fail-open by contract, so a broken update path can never brick the launch.
# -NoUpdate skips it, same as the git-pull prelude. ($prefixDir itself now
# lives up near $repo -- the pinned-checkout predicate needs it earlier.)
$applyMarker = Join-Path $prefixDir 'updates\just-applied-windows-x86_64'
$sotApply = Join-Path $PSScriptRoot 'sot-apply.ps1'
$sotLocalDaemon = Join-Path $PSScriptRoot 'sot-local-daemon.ps1'

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
$repoCurrent = Join-Path $prefixDir 'repo\current'

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
Initialize-InstallLayout

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
Invoke-PendingApply -Handover:$script:reexecd

# ---------------------------------------------------------------------------
# Migration + post-apply handover onto the pinned launcher (docs/adr/
# 0030-versioning-release-and-auto-update.md's 2026-09-17 amendment). ONE
# block serves two cases that both need the same in-process re-invoke:
#   - migration: the shortcut/pin still targets the clone even though
#     repo\current now exists (Initialize-InstallLayout above may have just
#     created it) -- hand over so this launch already runs pinned scripts.
#   - post-apply: sot-apply.ps1 just flipped repo\current to a NEW tag, but
#     THIS process is still the OLD tag's in-memory copy -- without handing
#     over, it drives the rest of this launch (backend-pair rebuild, local
#     daemon ensure, frontend spawn) against the new tag's binaries and
#     scripts with the old copy's logic.
#
# Gated on install.json existing -- the updater's own release-install marker
# (rust/updater/src/manifest.rs); a dev clone never writes one. $pinnedLauncher
# -ne $PSCommandPath used to be the ONLY thing that let this fire, on the
# reasoning that a copy already AT $pinnedLauncher has nothing left to hand
# over to -- true for migration, but wrong for the post-apply case: on an
# already-pinned install $PSCommandPath and $pinnedLauncher are the SAME
# junction path string both before and after sot-apply.ps1 flips the
# junction underneath this running process, so the string compare can never
# see the change. That silently skipped the post-apply handover on every
# Windows box past its first migration -- exactly the common case -- and
# left THIS process (helpers dot-sourced once, near the top of the file,
# from the OLD target) driving the rest of the launch against the NEW
# tag's binaries with the old copy's logic (2026-09-18 field report: a
# v0.6.2 launcher that had just applied v0.6.4 died silently later reading
# v0.6.4's sot-hosts.ps1/sot-install-layout.ps1). Fixed by OR-ing in
# $script:sotJustApplied, which is true only for the one launch that just
# ran sot-apply.ps1 -- $pinnedLauncher then resolves through the junction
# to the NEW tag's file even though its string is unchanged.
# Recursion is stopped two ways, not just the path compare: -not
# $env:SOT_LAUNCH_REEXEC (kept as defense in depth even though both
# Invoke-SelfUpdatePrelude paths always clear it before this point), and --
# for the sotJustApplied arm specifically -- Invoke-PendingApply always
# clears $applyMarker before invoking sot-apply.ps1, so the re-exec'd
# process finds nothing pending, sot-apply.ps1 exits without rewriting the
# marker, and $script:sotJustApplied comes back false on the second pass.
# Test-Path $pinnedLauncher guards the obvious case of nothing to hand over
# to yet (Initialize-InstallLayout failed to pin a tag).
# ---------------------------------------------------------------------------
$pinnedLauncher = Join-Path $repoCurrent 'scripts\launch-sot.ps1'
if ((Test-Path -LiteralPath (Join-Path $prefixDir 'install.json')) -and
    -not $env:SOT_LAUNCH_REEXEC -and -not $script:reexecd -and
    (($pinnedLauncher -ne $PSCommandPath) -or $script:sotJustApplied) -and
    (-not $script:sotPinned -or $script:sotJustApplied) -and
    (Test-Path -LiteralPath $pinnedLauncher)) {
    $handoverReason = if ($pinnedLauncher -ne $PSCommandPath) { 'migration' } else { 'post-apply refresh' }
    Write-SupLog "${handoverReason}: handing over to $pinnedLauncher"
    $shortcutScript = Join-Path $PSScriptRoot 'install-shortcut.ps1'
    if (Test-Path -LiteralPath $shortcutScript) {
        try {
            $shortcutOut = & $shortcutScript 2>&1
            foreach ($l in @($shortcutOut)) { if ("$l".Trim()) { Write-SupLog "migration: install-shortcut -> $l" } }
        } catch {
            Write-SupLog "migration: install-shortcut.ps1 failed: $($_.Exception.Message)"
        }
    }
    Stop-Splash   # the pinned launcher spawns its own
    $env:SOT_LAUNCH_REEXEC = '1'
    $reexecParams = $script:LaunchBoundParameters
    & $pinnedLauncher @reexecParams
    exit $LASTEXITCODE
}

# Binary sources, in priority order: an update just applied into the staged
# bin dir, the dev source build, or the already-staged copy from a previous
# run. A machine with no source tree (public install layout) runs on the
# staged copy, which is exactly what sot-apply.ps1 writes.
$alreadyStaged = Join-Path $prefixDir 'bin\sot.exe'
if (-not (Test-Path $frontendExe) -and -not (Test-Path $alreadyStaged)) {
    Set-LaunchStatus 'ERROR: No sot.exe found - build it: cargo build --release -p sot-frontend'
    Stop-Splash
    [System.Windows.Forms.MessageBox]::Show(
        "No sot.exe found (no staged copy at $alreadyStaged, no source build at $frontendExe)`n`nDev machines: cd $repo\rust; cargo build --release -p sot-frontend`nRelease installs: re-extract the release zip into $prefixDir\bin",
        'Ship of Tools launcher',
        'OK', 'Error') | Out-Null
    exit 1
}

# ---------------------------------------------------------------------------
# Everything from here through Update-SotRemoteDial below is DEFAULT-MODE
# ONLY (2026-09-02, ONE-ensure simplification). -Local now
# skips it via this explicit gate, rather than via the old "Local daemon
# ensure" block's early exit that used to sit ABOVE this section -- that
# block moved to run once, after the freshness rebuild below (see its own
# comment there for why). Left NOT reindented on purpose: this wraps the
# pre-existing "Default: SSH-to-remote backend" section as-is, to keep
# the diff reviewable.
if (-not $Local) {
# ---------------------------------------------------------------------------
# Default: SSH-to-remote backend.
#
# Host registry. `sotd topology plan --self <host>` (topology plan, lane
# D) is the one parser for the declared topology now -- Get-SotTopologyPlan
# (scripts/sot-hosts.ps1, dot-sourced above) just reads its plain-line
# stdout. No `.sot\hosts.toml` path, no TOML, here or anywhere else in this
# script.
#
# $backendHost is this box's PRIMARY remote -- the one `New-RemoteEnsureCommand`
# checks/starts on every launch. Default the plan's declared `hub`;
# SOT_HOST_NAME (or the pre-existing SOT_HOST) still overrides it for a box
# that wants a different primary. Every OTHER dialable host in the plan is
# just another `--dial` entry to the frontend (`$frontendArgs` below,
# straight from `$plan.Dials`) -- v2 has no more "the configured default"
# vs. "everyone else" distinction at the topology level, and C3 means
# nothing here opens a port for either kind.
#
# remote_repo/tcp_port/remote_socket per host are GONE (topology plan
# section D): ports are ordinal (the plan names them), a remote daemon is
# never started by path (New-RemoteEnsureCommand below is `systemctl
# --user start sotd` or report it down), and the remote socket is always
# queried (`sotd session-socket-path sot` on the remote), never configured.
#
# ADR 0042 L2a codex review, item I: the state-toml `last_host` read
# (`Read-SotLastHost`, ADR 0015) is DELETED (unrelated to this box's own
# host name -- see state_persistence.rs's field doc for what `last_host`
# means today, frontend-side).
#
# Ordering risk (manager review): a brand-new box has no sotd(.exe) built
# yet at THIS point (before Invoke-FreshnessPass's backend-pair rebuild,
# below) -- an early-only read would leave $frontendArgs built from a
# permanently empty plan on the very first launch, needing a second one to
# pick up any host at all. Update-SotTopologyPlan is called here (for the
# remote-ensure code right below, which does need to run before the
# rebuild) AND AGAIN after every Invoke-FreshnessPass call (both call
# sites) so $plan is fresh by the time $frontendArgs is built, right
# before the frontend actually launches -- a fresh box needs exactly one
# launch, not two.
function Update-SotTopologyPlan {
    $sotdForPlan = if (Test-Path -LiteralPath $backendExe) {
        $backendExe
    } else {
        $stagedSotdForPlan = Join-Path $prefixDir 'bin\sotd.exe'
        if (Test-Path -LiteralPath $stagedSotdForPlan) { $stagedSotdForPlan } else { $null }
    }
    # Self-heal, at EVERY launch -- what .sot\hosts.toml.example promises.
    # SYNC FIRST, then plan -- never the other way around. `plan` needs
    # this box listed in the hosts.toml it reads, and the one file that
    # CANNOT list this box is exactly the stale/never-synced copy the
    # self-heal exists to replace; gating the sync on a successful plan
    # (the old order) starves it of the one thing it exists to fix (a
    # frontend box's file predates this box's own [host.<name>] entry, or
    # or is pre-grammar-v2 -- ADR 0015's 2026-09-17 addendum). The hub for
    # the sync is the env override when set, else the hub install.json
    # recorded at install time (install-shortcut.ps1 -Hub), else `sotd
    # topology sync` derives it from whatever hub the LOCAL copy already
    # names. Only the manifest's hub can create the FIRST copy: sotd reads
    # --hub only while no local copy exists, so a box that never synced
    # stayed local-only until 2026-09-18.
    $syncHub = if ($env:SOT_HOST_NAME) { $env:SOT_HOST_NAME } else { $env:SOT_HOST }
    if (-not $syncHub) {
        try {
            $manifestPath = Join-Path $prefixDir 'install.json'
            if (Test-Path -LiteralPath $manifestPath) {
                $syncHub = [string](Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json).hub
            }
        } catch { $syncHub = $null }
    }
    $sync = Invoke-SotTopologySync -SotdPath $sotdForPlan -Hub $syncHub
    if ($sync.Ok) {
        if ($sync.Output) { Write-SupLog "topology sync: $($sync.Output)" }
    } elseif ($sync.Output) {
        # No hint appended here: sotd's own message already names the fix
        # (e.g. "... pass --hub <alias>") when there is one.
        $msg = "topology sync failed: $($sync.Output)"
        Write-SupLog $msg
        Set-LaunchStatus $msg
    }
    $script:plan = Get-SotTopologyPlan -SotdPath $sotdForPlan
    if ($script:plan.Error) {
        $msg = "topology plan failed: $($script:plan.Error) - continuing with no remote hosts"
        Write-SupLog $msg
        Set-LaunchStatus $msg
    }
    # C10 (isolation-plan.md §3): the resolver now asks `sotd topology
    # relay-endpoint` at call time, on every platform -- there is no
    # value left for this launcher to compute and hand down. Clear the
    # User-scope value ONCE (a no-op once it is already gone) so no later
    # session inherits a stale one this launcher itself set before this
    # change.
    [Environment]::SetEnvironmentVariable('SOT_RELAY_ENDPOINT', $null, 'User')
}
Update-SotTopologyPlan
# Token resolution with registry-scope fallback (a Windows FE box finding, 2026-07-11):
# an ADR-0017 exit-75 respawn reuses THIS supervisor's process env, frozen at
# launch time — a supervisor started from a stale shell/shortcut (no
# $env:SOT_TOKEN) reconnect-looped on token mismatch forever even though the
# token was correctly set at User scope. Fall back to the User/Machine scoped
# values so a fresh supervisor self-heals its token.
$token = $env:SOT_TOKEN
if (-not $token) { $token = [Environment]::GetEnvironmentVariable('SOT_TOKEN', 'User') }
if (-not $token) { $token = [Environment]::GetEnvironmentVariable('SOT_TOKEN', 'Machine') }
# may still be empty (open-config local installs)

# Check/start the remote backend on every launch without restarting a live
# daemon by default. The backend listens on its per-user socket; the
# frontend reaches it by spawning its own ssh child (C3), never a forward
# this launcher opens.
#
# ADR 0042 L2b design E: kept as a function that BUILDS the remote command
# text (not one that also runs ssh) so the same text could feed an `ssh`
# call at more than one site (codex follow-up: the default host is
# nonfatal too now, exactly like
# every other host -- see $defaultRemoteOk below).
#
# ssh options (codex follow-up, item 5, trimmed): BatchMode=yes (never
# prompt for a password/passphrase -- that would hang, not fail, on a
# misconfigured host) and ConnectionAttempts=1 (no silent retries) join
# the existing ConnectTimeout=10 at every remote call site, default host
# included. No separate per-host deadline machinery beyond that -- a wedged
# remote command past the handshake is accepted as today's existing risk,
# not one this slice takes on.
$sshRemoteOpts = @('-o', 'ConnectTimeout=10', '-o', 'BatchMode=yes', '-o', 'ConnectionAttempts=1')

function New-RemoteEnsureCommand {
    param(
        [bool]$Restart
    )
    # ADR 0030 dev-freshness rev 2 - MULTI-FE SAFE. The shared daemon is NEVER
    # restarted by a launcher while running: other FEs' kernels and REPL state
    # die with it. The BE updates on its own cadence - on the backend host the BE
    # session's on-merge deploy keeps it current.
    #
    # Topology plan (lane D, section D deletions): no more remote_repo, no
    # more path-based nohup fallback. The launcher never starts a remote
    # daemon by path any more -- `systemctl --user start sotd` (report it
    # down if that fails), same as every shared-home lab box already runs
    # it. The remote socket is always QUERIED (`sotd session-socket-path
    # sot`, on the remote's own PATH), never configured. Echoes stay
    # paren-free AND semicolon-free - PS 5.1 hands this to ssh unquoted, so
    # bash sees echo text bare: a ';' inside it splits the command and the
    # tail runs as a bogus command whose stderr killed the whole launcher
    # under EAP=Stop (the 2026-07-16 'force: command not found' hang).
    $restartFlag = if ($Restart) { '1' } else { '0' }
    $cmd = @"
export PATH="`$HOME/.local/share/sot/bin:`$HOME/.cargo/bin:`$HOME/.local/bin:`$PATH"
remote_socket="`$(sotd session-socket-path sot 2>/dev/null)"
echo "backend-socket: `$remote_socket"
if [ "$restartFlag" = 1 ]; then
    systemctl --user restart sotd.service && echo "backend: force-restarted via systemd" || echo "backend: force-restart FAILED"
elif systemctl --user is-active --quiet sotd.service 2>/dev/null; then
    echo "backend: running"
else
    systemctl --user reset-failed sotd.service 2>/dev/null || true
    if systemctl --user start sotd.service; then
        echo "backend: was down - started via systemd"
    else
        echo "backend: DOWN and could not be started via systemd - no path-based fallback any more, per the topology plan" >&2
    fi
fi
for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
    [ -S "`$remote_socket" ] && break
    sleep 0.25
    remote_socket="`$(sotd session-socket-path sot 2>/dev/null)"
done
[ -S "`$remote_socket" ] || echo "backend: socket MISSING at `$remote_socket"
"@
    # Normalize to LF — Windows checkouts (autocrlf=true) leave CRLF in the
    # here-string, which becomes literal $'\r' tokens in bash on the remote.
    return ($cmd -replace "`r`n", "`n")
}


# Everything the default remote's dial is made of -- which host it is, its
# socket, and whether it resolved at all -- in ONE function, called at
# first launch and again on every converge (exit 76) right after the plan
# is re-read: a launch whose FIRST plan failed and a later converge that
# finally sees a good plan must both see the SAME resolution logic, not a
# stale one frozen from before the plan existed. Assignments are $script:
# because the "nothing reachable" check further down reads them.
function Update-SotRemoteDial {
$script:backendHost = if ($env:SOT_HOST_NAME) {
    $env:SOT_HOST_NAME
} elseif ($env:SOT_HOST) {
    $env:SOT_HOST
} else {
    $plan.Hub
}
# ADR 0042 L2b codex follow-up (design 3): the default remote is now
# NONFATAL, same as every other host -- the local `--socket` connection
# (item 1: no more unconditional implicit local, but this launcher always
# passes it when the local daemon is up, see $localSocket above) means the
# frontend usually has SOMETHING to show even with no default remote
# configured or reachable at all. No backend host configured just means
# the launch continues without one; $defaultRemoteOk (computed further
# down, after the ssh attempt) gates the one error dialog that remains --
# see the "nothing at all can start" check right before the frontend
# launches.
# Always queried on the remote (New-RemoteEnsureCommand above) -- no more
# config-file/env override; see the host-registry comment above.
$script:remoteSocket = $null
# ADR 0042 L2b codex follow-up (design 3): the default remote is routed
# through the same nonfatal plan every other host uses -- log, continue,
# let the frontend show it unreachable and reconnect. $defaultRemoteOk
# gates (combined with $localDaemonReady, computed further below, after
# the freshness rebuild) the ONE error dialog that remains -- see
# "nothing at all can start" further down.
$script:defaultRemoteOk = $false
if ($backendHost) {
    $remoteCmd = New-RemoteEnsureCommand -Restart $RestartBackend
    # rev 2: default launches only check staleness / start-if-down (never restart a
    # running shared daemon); -RestartBackend forces `systemctl --user restart sotd`.
    Set-LaunchStatus $(if ($RestartBackend) { "Restarting backend on $backendHost..." } else { "Checking backend on $backendHost..." })
    # Relax 'Stop' -> 'Continue' around native ssh (same reason as the git pull
    # above): under 'Stop' + 2>&1 in PS 5.1, ANY remote stderr line throws and
    # kills the launcher silently. Gate on $LASTEXITCODE below instead.
    $savedEAP = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    $remoteStatus = ssh $sshRemoteOpts $backendHost $remoteCmd 2>&1
    $remoteExit = $LASTEXITCODE
    $ErrorActionPreference = $savedEAP
    $remoteStatusText = ($remoteStatus | Out-String)
    if ($remoteExit -ne 0) {
        Write-SupLog "default remote: '$backendHost' unreachable (ssh exit $remoteExit) - continuing without it"
    } elseif ($remoteStatusText -match 'socket MISSING') {
        Write-SupLog "default remote: '$backendHost' backend did not create its socket - continuing without it"
    } else {
        # Surface a remote force-restart failure. rev 2 only ever restarts the daemon on
        # the -RestartBackend force path (the default path never touches a running shared
        # daemon), so this can only fire there. Sticky warning, not a stop -- if a daemon
        # is up the FE still connects; staleness on the default path is expected and silent.
        if ($remoteStatusText -match 'force-restart FAILED') {
            Set-LaunchStatus "ERROR: backend force-restart failed on $backendHost (see 'systemctl --user status sotd' on that box / supervisor.log)"
        }
        if ($remoteStatusText -match 'backend-socket:[ \t]*(\S+)') {
            $script:remoteSocket = $matches[1]
        }
        if ($remoteSocket) {
            $script:defaultRemoteOk = $true
        } else {
            Write-SupLog "default remote: '$backendHost' did not report a socket path - continuing without it"
        }
    }
} else {
    Write-SupLog "default remote: no hub/primary declared (sotd topology plan --self, or SOT_HOST_NAME/SOT_HOST) - continuing without one"
}
}
Update-SotRemoteDial

}   # end: default-mode only (see the -not $Local gate above)

# ---------------------------------------------------------------------------
# Self-relaunch supervisor (ADR 0017).
#
# ---------------------------------------------------------------------------
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
Invoke-FreshnessPass
# Re-read now that the backend pair may have just been built for the first
# time (ordering risk, see Update-SotTopologyPlan's own comment above).
Update-SotTopologyPlan
# ---------------------------------------------------------------------------
# Local daemon ensure (ADR 0042 L2b design D; ONE-ensure simplification
# 2026-09-02): EVERY launch mode ensures the persistent, per-user local
# sotd is running now, not just -Local -- this launcher passes its socket
# explicitly (Get-SotLocalPipePath / $localSocket, item 1: no more
# unconditional implicit local), whether -Local's own connection or one
# row of the default mode's multi-host tree.
#
# Positioned here -- after BOTH steps in this launcher that can replace
# sotd.exe/sot-capsule.exe: the staged-update apply (near the top) and the
# dev freshness rebuild (just above, with its own stop-first guard) -- so
# this is the ONE ensure call per launch, always seeing whatever pair is
# current. It used to run before the SSH remote-ensure section too, needing a
# second post-rebuild call (and a $backendPairStopped gate) to restart the
# daemon on a fresh pair; both are deleted now that there is only one call,
# made once everything that could invalidate an earlier one has run. The
# SSH remote-ensure section above is explicitly gated on `-not $Local` (see its
# own header) so it still never runs for -Local, even though this ensure
# no longer sits before it.
#
# See scripts/sot-local-daemon.ps1 for the binary resolution order, the
# pipe-naming derivation (queried from the daemon itself, ADR 0042 L2b
# design C) and why -Stop reduces to Stop-Process.
#
# Fail-open in the DEFAULT mode: a local daemon that won't come up just
# means the "local" row in Hosts mode shows unreachable -- the remote
# host(s) this mode exists for are unaffected. -Local has nothing else to
# fall back to, so it keeps today's hard error dialog.
# ---------------------------------------------------------------------------
# Invoke-LocalDaemonEnsure is this single call as a function so an exit-76
# CONVERGE respawn (see the do/while loop) can re-ensure the daemon after its
# freshness pass -- a backend-pair rebuild there stops the daemon first (it
# pins its own image on Windows) and only THIS launcher restarts it.
function Invoke-LocalDaemonEnsure {
    if (-not (Test-Path $sotLocalDaemon)) {
        Write-SupLog "local daemon: sot-local-daemon.ps1 missing at $sotLocalDaemon"
        return $false
    }
    Set-LaunchStatus 'Starting local daemon...'
    $localOut = & $sotLocalDaemon -DevBinDir (Split-Path $backendExe -Parent) 6>&1 2>&1
    foreach ($l in @($localOut)) { if ("$l".Trim()) { Write-SupLog "$l" } }
    return ($LASTEXITCODE -eq 0)
}

# The Windows box's own local daemon (label "local", sot-local-daemon.ps1)
# is a frontend-only convenience OUTSIDE the topology plan entirely -- it
# needs no dial, so it never appears in `$plan.Dials`
# (those only ever name DAEMON hosts this box dials over ssh, topology
# plan section C). The frontend now needs an explicit `--socket` for it
# (no more unconditional implicit local, item 1) -- queried the same way
# sot-local-daemon.ps1 itself queries it (`sotd session-socket-path
# local`), not re-derived here, so the two can never disagree.
function Get-SotLocalPipePath {
    $exe = if (Test-Path $backendExe) { $backendExe } else { Join-Path $prefixDir 'bin\sotd.exe' }
    if (-not (Test-Path -LiteralPath $exe)) { return $null }
    $queried = (& $exe session-socket-path local 2>$null | Select-Object -First 1)
    if ($queried) { return $queried.ToString().Trim() }
    return $null
}

if ($Local) { Stop-Splash }   # -Local is a debug path with no other progress UI
$localDaemonReady = Invoke-LocalDaemonEnsure
$localSocket = if ($localDaemonReady) { Get-SotLocalPipePath } else { $null }

if ($Local) {
    if (-not $localDaemonReady) {
        [System.Windows.Forms.MessageBox]::Show(
            "Local sotd is not answering.`n`nSee %LOCALAPPDATA%\sot\logs\sotd-local.log for why (no complete sotd.exe+sot-capsule.exe pair, or it did not come up in time).",
            'Ship of Tools launcher',
            'OK', 'Error') | Out-Null
        exit 1
    }
    Rotate-FrontendLogs
    $localOnlyArgs = if ($localSocket) { @('--socket', $localSocket) } else { @() }
    Start-Process -FilePath $frontendExe `
        -ArgumentList $localOnlyArgs `
        -RedirectStandardOutput $frontendStdout `
        -RedirectStandardError $frontendStderr `
        -WindowStyle Hidden `
        -Wait
    exit 0
}
if (-not $localDaemonReady) {
    Write-SupLog "local daemon: not ready - continuing without it (fail-open; 'local' will show unreachable, or be absent if --socket was never passed)"
}

# The frontend runs from a *staged copy* under %LOCALAPPDATA%\sot\bin so a
# `cargo build --release` can overwrite rust\target\release while the app is
# live — Windows locks a running .exe, so building in place would fail the
# link step. On exit code 75 ("rebuild done, relaunch me") we re-stage the
# fresh binary and respawn it with --relaunched (which reopens the Terminal
# drawer and runs the resume command). Exit code 76 ("converge") does the
# same respawn but FIRST re-runs Invoke-SelfUpdatePrelude + Invoke-
# FreshnessPass + Invoke-LocalDaemonEnsure -- relaunch-sot.ps1 -Converge
# writes `converge` as the sentinel file's content instead of a bare
# timestamp, and the frontend's watcher (rust/frontend/src/gpu.rs) reads
# that back to pick 76 over 75. Any other exit code = real quit. See
# docs/adr/0017-frontend-self-relaunch.md's 76 amendment.
# A converge that finds the launcher code on disk changed re-invokes the
# launcher in this process instead of finishing the pass (ADR 0017, 0.6.6).
#
# SOT_REPO_DIR lets the frontend find the local repo (Terminal cwd for
# `claude --continue`, and the build dir for the relaunch helper).
$RelaunchExitCode = 75
$ConvergeExitCode = 76
# The supervisor holds a lease on the local daemon across every window-less
# stretch of a relaunch (contract 1.8): LEASE_REPLY_WAIT and HANDOVER_BOUND in
# rust/protocol/src/ops.rs.
$LeaseReplyWaitMs = 5000
$HandoverBoundSeconds = 60
# Every lease this supervisor PROCESS holds, as open pipe streams. Global, not
# script, scope: the daemon knows a lease by process (pid and creation time),
# and a converge that re-invokes the launcher runs the new copy in this same
# process, which must hand over the leases its caller opened. The contract
# every launcher version keeps (ADR 0017, 0.6.6): append with +=, never reset;
# only Close-SotLeases empties it, after closing each stream.
if ($null -eq $global:SotLeases) { $global:SotLeases = @() }

# The Windows boot identity: the registry BootId as an unsigned decimal string,
# "" when it cannot be read. A failed read never fails the lease.
function Get-SotBootId {
    try {
        $v = (Get-ItemProperty -Path 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Memory Management\PrefetchParameters' -Name BootId -ErrorAction Stop).BootId
        return [string][BitConverter]::ToUInt32([BitConverter]::GetBytes([int32]$v), 0)
    } catch {
        return ""
    }
}

# Open a lease on the local daemon: emits the open pipe stream when granted and
# NOTHING otherwise (callers collect with @()). Never throws: $ErrorActionPreference
# is Stop and an escaped throw would kill the supervisor.
function Open-SotLease([string]$PipePath) {
    $client = $null
    $why = ''
    try {
        $name = $PipePath -replace '^\\\\\.\\pipe\\', ''
        $created = [System.Diagnostics.Process]::GetCurrentProcess().StartTime.ToFileTimeUtc()
        $line = '{"v":2,"id":1,"kind":"req","op":"fe.lease","payload":{"boot":"' + (Get-SotBootId) + '","created":' + $created + ',"pid":' + $PID + '}}'
        $client = New-Object System.IO.Pipes.NamedPipeClientStream('.', $name, [System.IO.Pipes.PipeDirection]::InOut)
        $client.Connect($LeaseReplyWaitMs)
        $bytes = (New-Object System.Text.UTF8Encoding($false)).GetBytes($line + "`n")
        $client.Write($bytes, 0, $bytes.Length)
        $client.Flush()
        $reader = New-Object System.IO.StreamReader($client, (New-Object System.Text.UTF8Encoding($false)), $false, 1024, $true)
        $task = $reader.ReadLineAsync()
        if (-not $task.Wait($LeaseReplyWaitMs)) { throw 'no reply' }
        $why = (ConvertFrom-Json $task.Result).payload.outcome
        if ($why -eq 'granted') {
            Write-SupLog 'relaunch: lease granted'
            return $client
        }
    } catch {
        $why = $_.Exception.Message
    }
    if ($client) { try { $client.Dispose() } catch { } }
    Write-SupLog "WARNING: relaunch: lease not granted ($why) - this computer's sessions end if no window holds the backend within $HandoverBoundSeconds s"
}

# Hand every lease this process holds over to the frontend just spawned: say so, then drop them.
function Close-SotLeases {
    foreach ($c in @($global:SotLeases)) {
        try {
            $bytes = (New-Object System.Text.UTF8Encoding($false)).GetBytes('{"v":2,"id":2,"kind":"req","op":"fe.leaving","payload":{"intent":"handover"}}' + "`n")
            $c.Write($bytes, 0, $bytes.Length)
            $c.Flush()
        } catch { }
        try { $c.Dispose() } catch { }
    }
    $global:SotLeases = @()
}
$stagedDir = Join-Path $env:LOCALAPPDATA 'sot\bin'
New-Item -ItemType Directory -Force -Path $stagedDir | Out-Null
$stagedExe = Join-Path $stagedDir 'sot.exe'
$env:SOT_REPO_DIR = $repo.Path
# Point the frontend at the project settings file explicitly. The frontend
# runs from the staged copy with an arbitrary cwd (e.g. System32 when the
# supervisor was spawned via WMI), so cwd-relative discovery of
# .sot\settings.toml is unreliable; $SOT_SETTINGS is the highest-
# priority, absolute discovery path. Don't clobber a user-set override.
if (-not $env:SOT_SETTINGS) {
    $env:SOT_SETTINGS = Join-Path $repo '.sot\settings.toml'
}

# ADR 0042 L2b codex follow-up (design 3): the ONE error dialog that
# survives the default remote becoming nonfatal -- nothing at all could be
# reached. Every OTHER remote is still purely informational (the frontend
# shows each as unreachable and reconnects on its own), but if BOTH the
# local daemon and the default remote failed, there is nothing for the
# frontend to usefully show on first paint; fail loud here rather than
# open a window with no connection anywhere and no way back in.
if (-not $localDaemonReady -and -not $defaultRemoteOk) {
    Set-LaunchStatus 'ERROR: nothing reachable - no local daemon and no default remote'
    Stop-Splash
    [System.Windows.Forms.MessageBox]::Show(
        "Nothing reachable: the local sotd did not come up, and $(if ($backendHost) { "the default remote ($backendHost)" } else { 'no default remote is configured' }) could not be used either.`n`nSee %LOCALAPPDATA%\sot\logs\supervisor.log and sotd-local.log for why.",
        'Ship of Tools launcher',
        'OK', 'Error') | Out-Null
    exit 1
}

Set-LaunchStatus 'Connecting...'

if ($token) {
    $env:SOT_TOKEN = $token
}
# Self-update / freshness notice for the frontend - whatever
# $script:launchNotices collected (empty for offline or a fully clean pull;
# see the self-update prelude and Invoke-FreshnessPass above). Set once here,
# before the FIRST Start-Process spawn below: env vars set on this process
# are inherited by every child it spawns, exit-75 respawns included within
# this SAME invocation. A later exit-76 CONVERGE re-runs the prelude and
# freshness pass and calls Set-LaunchNoticeEnv again before its own respawn
# (see the do/while loop), so the notice stays current across converges too.
# The frontend reads it once at its own startup (rust/frontend/src/gpu.rs)
# and renders it through the same status/notify_sticky_until fields
# FeCommand::Notify uses.
function Set-LaunchNoticeEnv {
    Remove-Item Env:\SOT_LAUNCH_NOTICE -ErrorAction SilentlyContinue
    if ($script:launchNotices.Count -gt 0) {
        $env:SOT_LAUNCH_NOTICE = ($script:launchNotices -join '; ')
    }
}
Set-LaunchNoticeEnv
# ADR 0045 decision 1 (Codex review): one FE-instance id per SUPERVISOR
# invocation, set once here (same "env vars set on this process are
# inherited by every child it spawns" rule as SOT_LAUNCH_NOTICE above) —
# every exit-75/-76 respawn inside the do/while loop below inherits the
# SAME value, while a fresh `launch-sot.ps1` invocation (a genuinely
# independent frontend, not a relaunch of this one) mints its own. The
# frontend folds this into its supervisor-lane controller id
# (`gpu.rs`'s `fe_instance_component`) so the durable record can tell
# two frontends on one machine apart even though their comm handle
# (hostname-based) is identical. Respects an existing value so a caller
# can pin one explicitly; never overwritten by a converge/relaunch.
if (-not $env:SOT_FE_INSTANCE) {
    $env:SOT_FE_INSTANCE = [guid]::NewGuid().ToString('N')
}
$relaunchNext = [bool]$Relaunched
# The splash covers the INITIAL launch only. Exit-75 relaunches keep every
# host's ssh child alive inside the frontend process and skip freshness,
# and happen while the user is already in the app, so they get no splash
# — dismiss it exactly once, when the first FE window is up.
$splashDismissed = $false
Write-SupLog "supervisor start (relaunched=$Relaunched) depth=$global:SotLauncherDepth ws=$([int]([System.Diagnostics.Process]::GetCurrentProcess().WorkingSet64 / 1MB))MB code=$(if ($script:launcherCodeId) { $script:launcherCodeId } else { 'unknown' })"
# A supervisor rolls back AT MOST ONCE, matching the Unix supervisor's $ROLLED
# in scripts/install.sh. What it actually guards is narrow: a Remove-Item that
# failed to clear the marker, and a converge that applies again after a
# rollback. It is not protection against walking backwards through releases --
# that cannot happen here, since a rollback clears the marker twice over and
# the supervisor exits after any fast exit it does not roll back.
$rolledBackOnce = $false
try {
    do {
        # Every respawn (exit 75, exit 76 and the crash-loop rollback) ensures the
        # local daemon first and leases it until the new window is up: no frontend is spawned without one. Nothing to do for
        # a remote host's own connection (C3): the OLD frontend process owned every
        # ssh child it spawned, so the NEW one spawns its own set from the same
        # --dial list and the remote backend and session survive regardless.
        if ($relaunchNext) {
            $localDaemonReady = Invoke-LocalDaemonEnsure
            $localSocket = if ($localDaemonReady) { Get-SotLocalPipePath } else { $null }
            if ($localSocket) { $global:SotLeases += @(Open-SotLease $localSocket) }
        }
        # Stage the binary for this launch, priority order:
        #   1. dev source build (the classic path — takes precedence, and a
        #      -dev build never self-updates so it cannot race an apply)
        #   2. keep the already-staged copy, which is where sot-apply.ps1
        #      installed any update it applied above (public install layout)
        #
        # `$appliedUpdate` gates the crash-loop rollback below. sot-apply.ps1
        # drops the just-applied marker only on a SUCCESSFUL apply, so its
        # presence means an apply succeeded — but NOT, on its own, that it was
        # this launch's. The marker is removed only just before an apply runs,
        # so a launch that skips the apply for any other reason (a handover
        # pass, -NoUpdate, sot-apply.ps1 absent, a converge with nothing armed)
        # inherits whatever a PREVIOUS launch left; and this read sits inside
        # the loop, so every exit-75 relaunch re-read it too. A marker from days
        # ago then armed a live rollback window, and that rollback is not a log
        # line: it reverts binaries, the repo\current junction and install.json,
        # and writes a bad-<tag> marker that stops the stager ever re-arming
        # that release. Any unrelated fast exit — a bad config, a driver
        # failure, the user closing the window quickly — could revert a healthy
        # install and ban its version.
        #
        # So the marker has to be RECENT, and the window has to CLOSE once the
        # release has proven itself. That is the rule the Unix supervisor
        # already states in as many words — "Roll back ONLY inside the
        # just-applied health window — an unrelated crash weeks later must not
        # downgrade a healthy release" (scripts/install.sh).
        #
        # The bound is two hours rather than the Unix half-hour, because the
        # Windows timeline differs in one direction: on the just-applied path
        # this launcher runs Initialize-InstallLayout — including the Julia
        # instantiate the updater deliberately skips — and then update_comm,
        # BETWEEN the apply and the first frontend start. A cold instantiate can
        # outrun a half-hour bound, which would close the window before the new
        # release had started even once: protection removed from exactly the
        # slow first boot where a bad update is most likely to bite. Unix has no
        # comparable work in that gap, so the same number does not mean the same
        # thing on the two platforms.
        #
        # The longer bound costs nothing because the window closes on success
        # rather than only on the clock (the closer sits just after the exit is
        # known, above): once any run has lasted a full minute the marker is
        # deleted,
        # so what remains is "a fast crash before this release has ever run
        # healthily, within two hours of its apply" — very nearly the exact
        # event worth rolling back on.
        #
        # The read stays INSIDE the loop on purpose: a converge can apply an
        # update mid-life, and that update deserves the same window as one
        # applied at launch. Hoisting it above the loop would close the window
        # for exactly those.
        # ONE lookup, not Test-Path followed by Get-Item: $ErrorActionPreference
        # is Stop for this script, so a marker that vanished between the two --
        # the closer below removes it, and a rollback removes it twice -- would
        # throw and drop the whole launch into the finally block.
        $appliedUpdate = $false
        $markerItem = Get-Item -LiteralPath $applyMarker -ErrorAction SilentlyContinue
        if ($markerItem) {
            $markerAge = (Get-Date) - $markerItem.LastWriteTime
            $appliedUpdate = $markerAge.TotalMinutes -lt 120
            if (-not $appliedUpdate) {
                Write-SupLog ("stale just-applied marker ({0:N0} min old) - no rollback window" -f $markerAge.TotalMinutes)
            }
        }
        if ($appliedUpdate) { Write-SupLog "first boot after an applied update - rollback window armed" }
        if (Test-Path $frontendExe) {
            Copy-Item -Path $frontendExe -Destination $stagedExe -Force
            Write-SupLog "staged $frontendExe -> $stagedExe (built $((Get-Item $stagedExe).LastWriteTime.ToString('o')))"
        } else {
            Write-SupLog "no source build - running the staged copy at $stagedExe"
        }

        if ($splash -and -not $splashDismissed) { Set-LaunchStatus 'Starting Ship of Tools...' }
        # The dial set (item 1): this box's own local daemon (--socket,
        # outside the topology plan entirely -- see Get-SotLocalPipePath
        # above) plus one --dial per plan.Dials entry, passed UNCONDITIONALLY
        # (same as the old always-pass `--tcp`) -- a tunnel that didn't come
        # up just means the frontend shows that host unreachable and keeps
        # retrying, never a reason to hold an arg back.
        # This box is now DIALABLE in its own right if it declares `daemon`
        # (ADR 0048 widened that predicate), so without this filter it would
        # arrive twice: once as $localSocket and once as a --dial to itself,
        # and the frontend would render its rows under two hosts. The local
        # socket wins -- it is the direct connection, not a dial.
        $frontendArgs = @()
        if ($localSocket) { $frontendArgs += @('--socket', $localSocket) }
        foreach ($d in $plan.Dials) {
            if ($localSocket -and $plan.Self -and $d.Host -eq $plan.Self) { continue }
            $frontendArgs += @('--dial', "$($d.Host)=$($d.Endpoint)")
        }
        if ($relaunchNext) { $frontendArgs += '--relaunched' }
        $feStartedAt = Get-Date
        Rotate-FrontendLogs
        $frontend = Start-Process -FilePath $stagedExe `
            -ArgumentList $frontendArgs `
            -RedirectStandardOutput $frontendStdout `
            -RedirectStandardError $frontendStderr `
            -WindowStyle Hidden `
            -PassThru
        # Cache the OS process handle NOW, while the child is alive. Without
        # this, a Start-Process -PassThru object loses access to the handle
        # once the child exits, so $frontend.ExitCode reads $null afterwards.
        # That made the exit-75 relaunch test ($ExitCode -eq $RelaunchExitCode)
        # always False, silently turning every self-relaunch into a real quit
        # (frontend closed, never reopened). Touching .Handle pins it.
        $null = $frontend.Handle
        Write-SupLog "frontend spawned pid=$($frontend.Id) args=[$($frontendArgs -join ' ')]"
        if ($global:SotLeases.Count -gt 0) { Close-SotLeases }

        # Hold the splash until the FE window is actually up (not merely the
        # process spawned), then dismiss it — avoids a blink of nothing between
        # splash-close and first FE paint. Caps at ~6s so a windowless/edge case
        # still writes DONE and the splash never orphans. One-shot per launch.
        if ($splash -and -not $splashDismissed) {
            for ($w = 0; $w -lt 24; $w++) {
                try { $frontend.Refresh(); if ($frontend.MainWindowHandle -ne 0) { break } } catch { }
                Start-Sleep -Milliseconds 250
            }
            $splashDismissed = $true
        }
        # DONE after EVERY spawn, not only the splashed first one: a converge
        # (exit 76) re-runs the freshness pass, which writes 'Rebuilding
        # frontend...', and nothing else settles the file afterwards.
        Set-LaunchStatus 'DONE'

        # C3 (isolation-plan.md §3): there is no tunnel process left for
        # this launcher to supervise. Each host's ssh child now lives
        # INSIDE the frontend process, respawned there on its own
        # exponential backoff (200ms→5s) exactly the way the old tunnel
        # supervisor above used to respawn ssh -- laptop wake, wifi flap, a
        # bounced sshd, all handled the same way, just one process closer
        # to the thing that needs the reconnect. This loop is now only
        # waiting for the frontend itself to exit.
        while (-not $frontend.HasExited) {
            Start-Sleep -Milliseconds 500
        }

        # Determine whether this was a relaunch request (75), a converge
        # request (76), or a real quit. WaitForExit() guarantees ExitCode is
        # populated after the poll loop.
        $frontend.WaitForExit()
        $feUptime = (Get-Date) - $feStartedAt
        $convergeRequested = ($frontend.ExitCode -eq $ConvergeExitCode)
        $relaunchNext = ($frontend.ExitCode -eq $RelaunchExitCode) -or $convergeRequested
        Write-SupLog "frontend pid=$($frontend.Id) exited code=$($frontend.ExitCode) uptime=$([int]$feUptime.TotalSeconds)s -> relaunchNext=$relaunchNext converge=$convergeRequested"

        # A healthy run closes the crash-loop health window -- the twin of
        # scripts/install.sh's `[ "$RUNTIME" -ge 60 ] && rm -f "$MARKER"`, same
        # number so the two platforms keep one rule. Once this release has run
        # properly once, a later fast exit is not its fault, and nothing may be
        # rolled back on its account.
        #
        # The MARKER is what goes, not the in-memory flag: the flag is
        # re-derived from the marker at the top of every iteration, so clearing
        # it is undone on the next pass, and it dies with the process, which
        # does nothing for the case that spans two launches -- work for twenty
        # minutes, quit, relaunch, fast crash. The marker is the only
        # cross-process state, so the marker is what has to go.
        #
        # Placed HERE, where the fact becomes known, and BEFORE the converge
        # block below, for a reason that is not symmetry: a converge can apply
        # a new update mid-life, and that apply writes a fresh marker. A closer
        # sitting lower in the loop would delete the marker the converge had
        # just written, and the window for a converge-applied update would
        # never arm -- breaking the case this window exists to protect. Here
        # the order is closer, then converge apply, then a fresh marker that
        # the next iteration reads as seconds old.
        if ($feUptime.TotalSeconds -ge 60) {
            if (Test-Path $applyMarker) {
                Write-SupLog "frontend ran $([int]$feUptime.TotalSeconds)s - closing the post-update rollback window"
            }
            Remove-Item -Path $applyMarker -Force -ErrorAction SilentlyContinue
        }

        # Converge (exit 76, relaunch-sot.ps1 -Converge): re-run the SAME
        # self-update prelude + freshness pass the very first launch ran,
        # then re-ensure the local daemon (the freshness pass may have
        # stopped it for a backend-pair rebuild) and refresh the launch
        # notice -- all three are plain function calls now, so this is the
        # only place that repeats them. Each is a no-op under -NoUpdate,
        # matching what a first launch with that switch would do.
        if ($convergeRequested) {
            Write-SupLog 'converge (exit 76): re-running self-update prelude + freshness pass'
            if ($localSocket) { $global:SotLeases += @(Open-SotLease $localSocket) }
            # Visible progress for the whole window-less stretch: the splash
            # renders each step below and exits itself on the DONE write after
            # the respawn, exactly as on the first launch.
            Start-Splash
            $splashDismissed = $false
            # A pinned install's updates arrive ONLY through sot-apply.ps1 --
            # Invoke-SelfUpdatePrelude below is a no-op for one -- so an
            # armed update sitting on an already-resident supervisor was
            # never applied until the next full process start (2026-09-18
            # field report). Gated on the pending pointer actually existing
            # so a converge with nothing armed doesn't pay a sot-apply.ps1
            # spawn every time.
            if (Test-Path (Join-Path $prefixDir 'updates\pending-windows-x86_64.json')) {
                # Never a handover: this supervisor is already running and the
                # armed update is one nobody has applied yet. Before this was
                # stated here, a supervisor born from a handover carried that
                # pass's flag for life and skipped the apply on every converge
                # -- it stopped the daemon, relaunched, read the marker the
                # handover had left, logged that a rollback window was armed,
                # and applied nothing, reporting success the whole way (field
                # report, 2026-09-28).
                Invoke-PendingApply
            }
            Invoke-SelfUpdatePrelude
            # The launcher code on disk is no longer the code this process
            # parsed: the apply above flipped repo\current, or the prelude's
            # pull changed the launcher or a file it dot-sources. Run the
            # launcher that is on disk, in this process, as the fresh path's
            # post-apply handover does: it keeps this process's leases in
            # $global:SotLeases and hands them over once its window is up, and
            # this pass exits when it returns. A copy that does not parse, that
            # predates the lease list, or that cannot be read is never invoked:
            # this pass carries on with the code it has. Every check is caught:
            # a throw here would end this process while it holds the leases,
            # and their end reads as a close.
            $onDiskCodeId = Get-SotLauncherCodeId -ScriptsDir $PSScriptRoot
            if (-not $onDiskCodeId) {
                Write-SupLog 'WARNING: converge: a launcher file on disk cannot be read - this pass carries on with the code it has'
                $script:launchNotices.Add('a launcher file on disk cannot be read; see supervisor.log before quitting') | Out-Null
            }
            if ($onDiskCodeId -and ($onDiskCodeId -ne $script:launcherCodeId)) {
                $refusal = ''
                $notice = ''
                try {
                    foreach ($f in @('launch-sot.ps1', 'sot-hosts.ps1', 'sot-install-layout.ps1')) {
                        $errs = $null
                        [void][System.Management.Automation.Language.Parser]::ParseFile((Join-Path $PSScriptRoot $f), [ref]$null, [ref]$errs)
                        if ($errs -and -not $refusal) {
                            $refusal = "${f} does not parse (line $($errs[0].Extent.StartLineNumber): $($errs[0].Message))"
                            $notice = 'the installed launcher does not parse, so starting again would fail: keep this window open and report it (see supervisor.log)'
                        }
                    }
                    if (-not $refusal -and -not ([System.IO.File]::ReadAllText($PSCommandPath).Contains('$global:SotLeases'))) {
                        $refusal = 'it predates the process lease list'
                        $notice = 'the launcher on disk is older than the running one; quit and start again from the Start menu'
                    }
                } catch {
                    $refusal = "it cannot be read ($($_.Exception.Message))"
                    $notice = 'a launcher file on disk cannot be read; see supervisor.log before quitting'
                }
                if ($refusal) {
                    Write-SupLog "WARNING: converge: not re-invoking the launcher on disk ($onDiskCodeId): $refusal - this pass carries on with the code it has"
                    $script:launchNotices.Add($notice) | Out-Null
                } else {
                    Write-SupLog "converge: launcher code changed on disk ($onDiskCodeId) - re-invoking $PSCommandPath in this process"
                    Stop-Splash   # the re-invoked launcher spawns its own
                    $env:SOT_LAUNCH_REEXEC = '1'
                    $reexecParams = @{}
                    foreach ($k in $script:LaunchBoundParameters.Keys) { $reexecParams[$k] = $script:LaunchBoundParameters[$k] }
                    $reexecParams['Relaunched'] = $true
                    & $PSCommandPath @reexecParams
                    exit $LASTEXITCODE
                }
            }
            Invoke-FreshnessPass
            Update-SotTopologyPlan
            # The re-read plan is only half of it: the default remote's own
            # ensure/resolve step has to be rebuilt from it too, feeding
            # $defaultRemoteOk -- that is exactly the bootstrap case: a
            # first plan that failed (no local topology file), then a
            # sync, then this. There is no tunnel to (re)start here any
            # more (C3): the frontend respawned below builds its own
            # --dial list straight from the freshly re-read plan and
            # spawns its own ssh child per host.
            Update-SotRemoteDial
            Set-LaunchNoticeEnv
        }

        # Crash-loop rollback (ADR 0030 §4): a just-applied update that dies
        # abnormally within 10s is rolled back and the FE respawns on the
        # previous binary. Delegated to sot-apply.ps1 -Rollback so the WHOLE
        # transaction reverts — binaries, the repo\current junction, and
        # install.json's version/tag — not just the exe. It also writes a
        # bad-<tag> marker so the stager never re-arms that release, which is
        # what makes this one-shot (the old inline .prev copy left install.json
        # claiming the broken version, and nothing stopped a re-arm).
        if ($appliedUpdate -and -not $rolledBackOnce -and -not $relaunchNext `
            -and $frontend.ExitCode -ne 0 -and $feUptime.TotalSeconds -lt 10) {
            Write-SupLog "UPDATE CRASH-LOOP: exit=$($frontend.ExitCode) after $([int]$feUptime.TotalSeconds)s - rolling back"
            if (Test-Path $sotApply) {
                $rbOut = & $sotApply -Rollback 6>&1 2>&1
                foreach ($l in @($rbOut)) { if ("$l".Trim()) { Write-SupLog "$l" } }
            } elseif (Test-Path "$stagedExe.prev") {
                Copy-Item -Path "$stagedExe.prev" -Destination $stagedExe -Force
                Write-SupLog "sot-apply.ps1 missing - restored $stagedExe from .prev only"
            }
            $rolledBackOnce = $true
            Remove-Item -Path $applyMarker -Force -ErrorAction SilentlyContinue
            $relaunchNext = $true
        }
    } while ($relaunchNext)
} catch {
    Write-SupLog "supervisor loop failed: $($_.Exception.Message) $($_.InvocationInfo.PositionMessage)"
    throw
} finally {
    Stop-Splash   # safety — normally already closed by the DONE status write
    # C3 (isolation-plan.md §3): there is no separate tunnel process for
    # this launcher to order teardown around any more -- every host's ssh
    # child is now a CHILD OF THE FRONTEND PROCESS itself, so stopping the
    # frontend is the only step left here. This launcher's own
    # `Stop-Process -Force` on the frontend is an abrupt kill, not the
    # frontend's own clean shutdown path (scripts/shutdown-sot.ps1,
    # /sot-fe-shutdown) -- it is not verified here whether Windows reaps
    # the frontend's own ssh children when the frontend itself is killed
    # this way (no job-object containment is set up for them, unlike the
    # capsule containment ADR 0043 gives supervised rows).
    Write-SupLog "supervisor exiting (relaunchNext=$relaunchNext) - stopping the frontend"
    if ($frontend -and -not $frontend.HasExited) {
        try { Stop-Process -Id $frontend.Id -Force -ErrorAction SilentlyContinue } catch {}
    }
    # Release the single-instance lock only if it is still ours.
    try {
        if ((Get-Content -Path $launcherLock -ErrorAction Stop | Select-Object -First 1) -eq "$PID") {
            Remove-Item -Path $launcherLock -Force -ErrorAction SilentlyContinue
        }
    } catch { }
}

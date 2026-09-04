# launch-sot.ps1 — default launcher: connect to every configured host at
# once (ADR 0042 L2b) -- the local machine's own daemon, always, plus one SSH
# tunnel per `[host.<name>]` remote in `.sot/hosts.toml`. Pass `-Local` for a
# debug path that skips freshness and opens no tunnels at all, connecting
# only to the local daemon.
#
# Idempotent on the backend side: each remote's backend is started once via
# `nohup` and survives across launches, so the second click is fast. Every
# SSH local-forward is started fresh each launch and torn down when the
# frontend exits.
#
# ADR 0042 L2b: the local sotd (a fixed per-user named pipe -- "the frontend
# machine runs its own sotd") is ensured on EVERY launch, not just -Local,
# right after the staged-update apply and before either mode's frontend
# launch; see scripts/sot-local-daemon.ps1 and design D in the ADR. Design
# E is the multi-tunnel loop below `New-RemoteEnsureCommand` — one tunnel
# per remote, `$backendHost` (env vars can still override which host that
# is) included: with the local connection always present, an unreachable
# or unconfigured default remote is NONFATAL too now (codex follow-up) --
# every remote-ensure step is skipped with a log line on failure, never a
# hard stop, except the one dialog for "nothing at all could be reached"
# right before the frontend launches.
#
# Overrides (env vars):
#   SOT_HOST         SSH alias for the backend host       (default: none — see .sot/hosts.toml)
#   SOT_REMOTE_REPO  Path to the repo on the remote       (default: none — see .sot/hosts.toml)
#   SOT_TCP_PORT     Local loopback port for the tunnel   (default: 18743)
#   SOT_REMOTE_SOCKET Remote socket path                  (default: query sotd)
#   SOT_TOKEN        App-level auth token for TCP fallback only
#
# Every OTHER configured remote (anything in hosts.toml besides
# $backendHost) is NOT env-var-overridable the way $backendHost is above —
# its ssh_alias/remote_repo/tcp_port/remote_socket come straight from its
# own [host.<name>] section.
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

# AUTHORING GOTCHA (Windows PowerShell 5.1): this file has no BOM, so the 5.1
# parser decodes it as ANSI/cp1252. A UTF-8 non-ASCII char (em-dash, curly
# quote, etc.) is harmless inside a "#" comment (runs to end-of-line) but inside
# a "string literal" its bytes mojibake into a phantom double-quote that corrupts
# parsing and fails the WHOLE launcher to load. Keep STRING LITERALS ASCII-only
# (use '-' not an em-dash in status text); prose em-dashes live in comments only.

$repo = Resolve-Path -Path (Join-Path $PSScriptRoot '..')

# Read-SotHosts (the .sot/hosts.toml parser) and Get-TunnelPlan (ADR 0042
# L2b design E: which remote hosts get their own tunnel, and on which
# port) live in one dot-sourceable file shared with shutdown-sot.ps1 and
# scripts/tests/test-tunnel-plan.ps1 -- see that file's own header.
. (Join-Path $PSScriptRoot 'sot-hosts.ps1')

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
function Write-SupLog {
    param([string]$Message)
    try {
        "$(Get-Date -Format o)  pid=$PID  $Message" |
            Out-File -FilePath $supervisorLog -Append -Encoding utf8
    } catch { }
}

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
try {
    $splash = Start-Process -FilePath 'powershell.exe' `
        -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-WindowStyle', 'Hidden',
            '-File', (Join-Path $PSScriptRoot 'launch-splash.ps1'), '-StatusFile', $statusFile) `
        -WindowStyle Hidden -PassThru
} catch { $splash = $null }
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
# Refused vs offline (2026-09-03 field report): a pull can fail two different
# ways and they are NOT the same event. OFFLINE means fetch never reached the
# remote - expected on a laptop off wifi, stays quiet (log only). REFUSED means
# git ran against the LOCAL repo and failed - a stale index.lock, a dirty tree,
# a stopped rebase - so this box is silently stuck on an old build while every
# OTHER fail-open step here still reports success. A stale-lock refusal was
# observed logging "Offline or dirty tree" and launching the old binary with
# nothing on screen saying the box never updated - fine for one laptop, a
# false "converged" reading fleet-wide otherwise. $selfUpdateRefusedReason
# below carries a REFUSED pull's first error line into $env:SOT_LAUNCH_NOTICE
# (set once, just before the frontend's first spawn) so the frontend renders
# it at its own startup - offline still only logs, same as before.
# ---------------------------------------------------------------------------
$selfUpdateRefusedReason = $null
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
                } else {
                    Write-SupLog 'self-update: launcher changed - re-invoking fresh copy'
                    Stop-Splash   # the fresh invocation spawns its own splash
                    $env:SOT_LAUNCH_REEXEC = '1'
                    & $PSCommandPath @PSBoundParameters
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
                $selfUpdateRefusedReason = "$errLine".Trim()
                Set-LaunchStatus 'Update refused - launching current build...'
                Write-SupLog "self-update: pull REFUSED - launching existing binary: $selfUpdateRefusedReason"
            }
        }
    } finally {
        $ErrorActionPreference = $savedEAP
    }
}
# The re-invoke guard has served its purpose; clear it so the FE and an exit-75
# relaunch don't inherit it (a relaunch must self-update afresh).
if ($env:SOT_LAUNCH_REEXEC) { Remove-Item Env:\SOT_LAUNCH_REEXEC -ErrorAction SilentlyContinue }

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
# -NoUpdate skips it, same as the git-pull prelude.
$prefixDir = Join-Path $env:LOCALAPPDATA 'sot'
$applyMarker = Join-Path $prefixDir 'updates\just-applied-windows-x86_64'
$sotApply = Join-Path $PSScriptRoot 'sot-apply.ps1'
$sotLocalDaemon = Join-Path $PSScriptRoot 'sot-local-daemon.ps1'

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
if (-not $NoUpdate -and (Test-Path $sotLocalDaemon)) {
    $updatePending = Test-Path (Join-Path $prefixDir 'updates\pending-windows-x86_64.json')
    if ($updatePending) {
        Write-SupLog 'local daemon: stopping before apply so it does not pin a stale binary'
        $stopOut = & $sotLocalDaemon -Stop 6>&1 2>&1
        foreach ($l in @($stopOut)) { if ("$l".Trim()) { Write-SupLog "$l" } }
    }
}

if (-not $NoUpdate -and (Test-Path $sotApply)) {
    Remove-Item -Path $applyMarker -Force -ErrorAction SilentlyContinue
    Set-LaunchStatus 'Applying update...'
    $applyOut = & $sotApply 6>&1 2>&1
    foreach ($l in @($applyOut)) { if ("$l".Trim()) { Write-SupLog "$l" } }
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
# Everything from here through the end of the per-remote tunnel loop below
# is DEFAULT-MODE ONLY (2026-09-02, ONE-ensure simplification). -Local now
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
# Host registry. We read the `.sot/hosts.toml` table to figure out which
# remote to tunnel to, looking up the `[host.<name>]` block for
# `hosts.toml`'s `default_host` and setting the existing SOT_HOST /
# SOT_REMOTE_REPO / SOT_TCP_PORT env vars from it. The fallback chain is
# env wins → hosts.toml default_host → error if none configured.
#
# ADR 0042 L2a codex review, item I: the state-toml `last_host` read
# (`Read-SotLastHost`, ADR 0015) is DELETED. Under L2a the frontend holds
# one connection per configured host at once and attributes a
# `--socket`/`--tcp` CLI override to `default_host` specifically (hosts.rs
# `resolve_connections`) — a stale `last_host` here (the field now means
# "active host at QUIT" on the frontend side, an entirely different thing;
# see state_persistence.rs's field doc) would make the launcher tunnel to
# host B while a connection the frontend labels A actually reaches B's
# daemon. Per-host tunnels (routing each configured host's own SSH
# forward, not just the launcher's single one) are ADR 0042 L2b design E,
# below — Read-SotHosts itself moved to scripts/sot-hosts.ps1 (dot-sourced
# above) so Get-TunnelPlan can share its output.
$hostsTomlPath = Join-Path $repo '.sot\hosts.toml'
$hostsCfg = Read-SotHosts -Path $hostsTomlPath
$activeHostName = if ($env:SOT_HOST_NAME) {
    $env:SOT_HOST_NAME
} elseif ($hostsCfg.default_host) {
    $hostsCfg.default_host
} else {
    $null
}
if ($activeHostName -and $hostsCfg.hosts.ContainsKey($activeHostName)) {
    $entry = $hostsCfg.hosts[$activeHostName]
    if (-not $env:SOT_HOST -and $entry.ssh_alias) {
        $env:SOT_HOST = $entry.ssh_alias
    }
    if (-not $env:SOT_REMOTE_REPO -and $entry.remote_repo) {
        $env:SOT_REMOTE_REPO = $entry.remote_repo
    }
    if (-not $env:SOT_TCP_PORT -and $entry.tcp_port) {
        $env:SOT_TCP_PORT = $entry.tcp_port
    }
    if (-not $env:SOT_REMOTE_SOCKET -and $entry.remote_socket) {
        $env:SOT_REMOTE_SOCKET = $entry.remote_socket
    }
}

$backendHost = if ($env:SOT_HOST) { $env:SOT_HOST } else { $null }
$remoteRepo = if ($env:SOT_REMOTE_REPO) {
    $env:SOT_REMOTE_REPO
} else {
    $null
}
# ADR 0042 L2b codex follow-up (design 3): the default remote is now
# NONFATAL, same as every other host -- the implicit local connection
# (hosts::resolve_connections) means the frontend always has SOMETHING to
# show even with no default remote configured or reachable at all. No
# backend host configured (logged below, once $tcpPort/etc. are in scope)
# just means the launch continues without one; $defaultRemoteOk (computed
# further down, after the ssh attempt) gates the one error dialog that
# remains -- see the "nothing at all can start" check right before the
# frontend launches.
$tcpPort = if ($env:SOT_TCP_PORT) { [int]$env:SOT_TCP_PORT } else { 18743 }
$remoteSocket = if ($env:SOT_REMOTE_SOCKET) { $env:SOT_REMOTE_SOCKET } else { $null }
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
# daemon by default. The backend listens on its per-user socket; `$tcpPort`
# below is only the local side of the SSH forward for the native frontend.
#
# ADR 0042 L2b design E: this ensure+resolve step is shared with every OTHER
# configured remote's own tunnel below (New-RemoteEnsureCommand), not
# special-cased to $backendHost -- "resolved as today" means the exact same
# three-tier socket resolution and start-if-down/staleness/force-restart
# logic every remote gets, not a lighter version. Kept as a function that
# BUILDS the remote command text (not one that also runs ssh) so the same
# text feeds an `ssh` call at every call site, default host included
# (codex follow-up: the default host is nonfatal too now, exactly like
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
        [string]$RemoteRepo,
        [string]$RemoteSocketOverride,
        [bool]$Restart
    )
    # ADR 0030 dev-freshness rev 2 - MULTI-FE SAFE. The shared daemon is NEVER
    # restarted by a launcher while running: other FEs' kernels and REPL state
    # die with it. The BE updates on its own cadence - on the backend host the BE
    # session's on-merge deploy keeps it current. This block only: starts a daemon that is
    # DOWN, reports staleness when running, and does the full pull+build+restart
    # ONLY on the explicit -RestartBackend force path. Tradeoff accepted: the old
    # always-restart also cleared a WEDGED-but-accepting daemon; that rare case
    # is now the force path's job. Protocol skew stays loud via the ADR 0030
    # handshake gate. Echoes stay paren-free AND semicolon-free - PS 5.1 hands this
    # to ssh unquoted, so bash sees echo text bare: a ';' inside it splits the
    # command and the tail runs as a bogus command whose stderr killed the whole
    # launcher under EAP=Stop (the 2026-07-16 'force: command not found' hang).
    $restartFlag = if ($Restart) { '1' } else { '0' }
    $cmd = @"
export PATH="`$HOME/.cargo/bin:`$HOME/.local/bin:`$PATH"
remote_socket='$RemoteSocketOverride'
if [ -z "`$remote_socket" ]; then
    cd '$RemoteRepo'
    # Dev checkout first, then a release install's staged sotd — a release BE
    # (install.sh --be-only) has no rust/target build, and without this branch
    # the omitted-remote_socket path dies on exactly the topology
    # INSTALL-AGENT.md 2b prescribes. remote_socket in hosts.toml overrides both.
    if [ -x ./rust/target/release/sotd ]; then
        remote_socket="`$(./rust/target/release/sotd session-socket-path sot)"
    elif [ -x "`$HOME/.local/share/sot/bin/sotd" ]; then
        remote_socket="`$(`$HOME/.local/share/sot/bin/sotd session-socket-path sot)"
    fi
fi
echo "backend-socket: `$remote_socket"
if [ "$restartFlag" = 1 ]; then
    cd '$RemoteRepo'
    scripts/restart-backend.sh && echo "backend: force-restarted at current build" || echo "backend: force-restart FAILED"
elif [ -S "`$remote_socket" ] && { pgrep -x sotd >/dev/null 2>&1 || systemctl --user is-active sotd.service >/dev/null 2>&1; }; then
    cd '$RemoteRepo'
    if scripts/restart-backend.sh --check >/dev/null 2>&1; then
        echo "backend: running and current"
    else
        echo "backend: running but STALE - it updates on its own cadence - force with -RestartBackend"
    fi
else
    if systemctl --user is-enabled sotd.service >/dev/null 2>&1; then
        systemctl --user reset-failed sotd.service 2>/dev/null || true
        systemctl --user start sotd.service
        echo "backend: was down - started via systemd"
    else
        cd '$RemoteRepo'
        # Same two-arm resolution as the socket query above: a release BE with
        # the systemd opt-out has no dev build - fall back to the installed
        # sotd with its matching project root (release-BE + --no-service +
        # daemon-down previously died here on a dev-only path).
        if [ -x ./rust/target/release/sotd ]; then
            nohup ./rust/target/release/sotd --project-root '$RemoteRepo' --label sot >/tmp/sotd.log 2>&1 </dev/null &
            disown
            echo "backend: was down - started nohup dev build, pid=`$!"
        elif [ -x "`$HOME/.local/share/sot/bin/sotd" ]; then
            nohup "`$HOME/.local/share/sot/bin/sotd" --project-root "`$HOME" --label sot >/tmp/sotd.log 2>&1 </dev/null &
            disown
            echo "backend: was down - started nohup release install, pid=`$!"
        else
            echo "backend: DOWN and no sotd found - dev build absent and no release install" >&2
        fi
    fi
fi
for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
    [ -S "`$remote_socket" ] && break
    sleep 0.25
done
[ -S "`$remote_socket" ] || echo "backend: socket MISSING at `$remote_socket"
"@
    # Normalize to LF — Windows checkouts (autocrlf=true) leave CRLF in the
    # here-string, which becomes literal $'\r' tokens in bash on the remote.
    return ($cmd -replace "`r`n", "`n")
}

# ADR 0042 L2b codex follow-up (design 3): the default remote is routed
# through the same nonfatal plan every other host uses -- log, continue,
# let the frontend show it unreachable and reconnect. $defaultRemoteOk
# gates: whether $sshArgs/Start-SotTunnel below is worth building at all,
# and (combined with $localDaemonReady, computed further below, after
# the freshness rebuild) the ONE error dialog that remains -- see
# "nothing at all can start" further down.
$defaultRemoteOk = $false
if ($backendHost -and $remoteRepo) {
    $remoteCmd = New-RemoteEnsureCommand -RemoteRepo $remoteRepo -RemoteSocketOverride $remoteSocket -Restart $RestartBackend
    # rev 2: default launches only check staleness / start-if-down (never restart a
    # running shared daemon); -RestartBackend forces the full restart-backend.sh path.
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
            Set-LaunchStatus "ERROR: backend force-restart failed on $backendHost (see restart-backend.sh output / supervisor.log)"
        }
        if (-not $remoteSocket -and $remoteStatusText -match 'backend-socket:\s*(\S+)') {
            $remoteSocket = $matches[1]
        }
        if ($remoteSocket) {
            $defaultRemoteOk = $true
        } else {
            Write-SupLog "default remote: '$backendHost' did not report a socket path - continuing without it"
        }
    }
} else {
    Write-SupLog "default remote: not configured (.sot/hosts.toml default_host, or SOT_HOST/SOT_REMOTE_REPO) - continuing without one"
}

# SSH local-port-forward. Keepalive tuning so brief wifi flaps and
# laptop-sleep-then-wake don't immediately tear the tunnel down:
#   ServerAliveInterval=30  — probe every 30s (less probe traffic on a
#                             stable link than the old 15s default).
#   ServerAliveCountMax=6   — allow 6 missed probes (~3 min tolerance)
#                             before declaring the connection dead.
#                             Pairs with the supervisor below: a real
#                             dead tunnel still respawns within a
#                             second of detection, but a brief network
#                             blip rides through without reconnecting.
#
# IPQoS was tried but Windows OpenSSH rejects the comma-separated
# `lowdelay,throughput` form with "Bad IPQoS value" (different parse
# from OpenSSH on Linux). Dropped since it was nice-to-have, not
# load-bearing — without it the supervisor was respawning ssh in a
# tight loop and the tunnel never came up.
$plutoPort = if ($env:SOT_PLUTO_PORT) { [int]$env:SOT_PLUTO_PORT } else { 1234 }
$videoPort = if ($env:SOT_VIDEO_PORT) { [int]$env:SOT_VIDEO_PORT } else { 1235 }
$docsPort  = if ($env:SOT_DOCS_PORT)  { [int]$env:SOT_DOCS_PORT }  else { 1236 }
$wglPort   = if ($env:SOT_WGL_PORT)   { [int]$env:SOT_WGL_PORT }   else { 1241 }
$sshCommonArgs = @(
    '-N',
    '-o', 'ExitOnForwardFailure=yes',
    '-o', 'ServerAliveInterval=30',
    '-o', 'ServerAliveCountMax=6'
)
# RETIRED by default (ADR 0035 scheduled this once the proxy had field time).
#
# These fixed-port forwards are how the FE used to reach backend-served pages.
# The daemon proxy replaced them: every backend page now rides the ONE control
# tunnel via `proxy.connect`, and the proxy's allowlist is VERIFIED-BOUND — it
# authorizes only ports this daemon actually bound, never a preferred port some
# OTHER user's daemon happens to hold (rust/backend/src/proxy.rs).
#
# Why retiring matters rather than being mere tidy-up: on a SHARED host these
# fixed ports are frequently owned by a different UNIX user's daemon. Forwarding
# them means an HTTP GET succeeds against a stranger's server and renders their
# content looking entirely normal — silent wrong-content, with no error anywhere.
# Not forwarding them fails closed instead.
#
# Set SOT_LEGACY_FORWARDS=1 to restore them. The one case that needs it is a
# NEW launcher against a PRE-v0.5.0 backend, which advertises no proxy and has
# no other path to these pages. It is opt-in and not a silent fallback, because
# on a shared host the safe default is to forward nothing you cannot prove is
# yours.
$useLegacyForwards = [bool]$env:SOT_LEGACY_FORWARDS
$sshAuxArgs = @()
if ($useLegacyForwards -and $defaultRemoteOk) {
    Write-Host "SOT_LEGACY_FORWARDS=1 - forwarding fixed helper ports $plutoPort/$videoPort/$docsPort(+1..4)/$wglPort." -ForegroundColor Yellow
    Write-Host "  On a shared host these may belong to ANOTHER USER's daemon; pages served over them are not verified as yours." -ForegroundColor Yellow
    $sshAuxArgs += $sshCommonArgs
    $sshAuxArgs += @(
        # H1.2 — the remote Pluto.jl server.
        '-L', "${plutoPort}:127.0.0.1:${plutoPort}",
        # ADR 0018 — the backend's video file server.
        '-L', "${videoPort}:127.0.0.1:${videoPort}",
        # ADR 0024 — the backend's docs site server.
        '-L', "${docsPort}:127.0.0.1:${docsPort}",
        # ADR 0029 Option B — the ROOT-relative site pool, docsPort+1..+4.
        # Keep in sync with site_serve::POOL_SIZE.
        '-L', "$($docsPort+1):127.0.0.1:$($docsPort+1)",
        '-L', "$($docsPort+2):127.0.0.1:$($docsPort+2)",
        '-L', "$($docsPort+3):127.0.0.1:$($docsPort+3)",
        '-L', "$($docsPort+4):127.0.0.1:$($docsPort+4)",
        # ADR 0032 — the WGLMakie/Bonito interactive-figure server.
        '-L', "${wglPort}:127.0.0.1:${wglPort}",
        $backendHost
    )
}
# ADR 0042 L2b codex follow-up: only worth building at all when the
# default remote actually resolved -- see $defaultRemoteOk above. An empty
# $sshArgs makes Start-SotTunnel's own callers no-ops (guarded at each
# call site, "Connecting..." and the supervisor loop below).
$sshArgs = @()
if ($defaultRemoteOk) {
    $sshArgs += $sshCommonArgs
    $sshArgs += @('-L', "${tcpPort}:$remoteSocket")
    # Fold the aux forwards into the MAIN tunnel too -- but only when they exist.
    # With the forwards retired, $sshAuxArgs is empty and `$sshAuxArgs.Count - 1`
    # would be -1, which PowerShell reads as "last element" and would splice
    # garbage into the control tunnel's args.
    if ($sshAuxArgs.Count -gt 0) {
        $auxForwardStart = $sshCommonArgs.Count
        # Count - 2, NOT Count - 1: the last element of $sshAuxArgs is $backendHost,
        # and the destination is appended separately below. Slicing to Count - 1 here
        # would put the host in twice.
        $auxForwardEnd = $sshAuxArgs.Count - 2
        $sshArgs += $sshAuxArgs[$auxForwardStart..$auxForwardEnd]
    }
    # The ssh DESTINATION, always last and never conditional. Before the aux
    # forwards were retired this rode in as the final element of the $sshAuxArgs
    # splice above; once that splice became conditional the control tunnel lost its
    # host entirely and ssh exited instantly ("ssh -N -o ... -L 18743:<sock>" with
    # no destination), leaving the supervisor respawning a doomed tunnel on
    # exponential backoff and 18743 never listening. Observed on a real FE, 2026-07-30.
    $sshArgs += $backendHost
}
function Test-LocalPortOpen {
    param([int]$Port)
    $client = New-Object Net.Sockets.TcpClient
    try {
        $client.Connect('127.0.0.1', $Port)
        return $true
    } catch {
        return $false
    } finally {
        $client.Close()
    }
}
function Start-SotTunnel {
    # No-op when the default remote never resolved ($sshArgs empty -- see
    # $defaultRemoteOk above): nothing to forward to, and every call site
    # already treats a $null return the same way Start-SotAuxTunnel's own
    # no-op is treated.
    if ($sshArgs.Count -eq 0) { return $null }
    Start-Process -FilePath ssh `
        -ArgumentList $sshArgs `
        -WindowStyle Hidden `
        -PassThru
}
function Start-SotAuxTunnel {
    # No-op once the fixed-port forwards are retired (the default). Returns
    # $null so callers that track the process handle see "nothing started"
    # rather than launching a bare `ssh` with no forwards.
    if ($sshAuxArgs.Count -eq 0) { return $null }
    Start-Process -FilePath ssh `
        -ArgumentList $sshAuxArgs `
        -WindowStyle Hidden `
        -PassThru
}

# ---------------------------------------------------------------------------
# Every OTHER configured remote gets its own tunnel too (ADR 0042 L2b design
# E) -- $backendHost's tunnel is $sshArgs/Start-SotTunnel above/below; this
# loop covers every remaining `[host.<name>]` entry with an `ssh_alias`.
# Ensure+resolve reuses New-RemoteEnsureCommand and the same $sshRemoteOpts
# (codex follow-up, item 5) as the default host's own attempt, but every
# failure here is NONFATAL: one log line and the launch continues without
# that host's tunnel. The frontend's own hosts.toml read (independent of the launcher)
# then shows that host unreachable and keeps retrying -- never a reason to
# fail the whole launch. tcp_port is required per remote (Get-TunnelPlan
# names the host + field in `error` when it's missing); $activeHostName
# (the default host's hosts.toml KEY, not its ssh_alias -- codex follow-up
# item 7: identity is the key, since nothing stops two different hosts.toml
# entries from sharing one ssh_alias) may fall back to $tcpPort/SOT_TCP_PORT
# for compatibility, matched here by KEY so it isn't double-tunneled.
# ---------------------------------------------------------------------------
$extraTunnels = @()
$tunnelPlan = Get-TunnelPlan -Cfg $hostsCfg -DefaultHost $activeHostName -DefaultPort $tcpPort
foreach ($item in $tunnelPlan) {
    if ($item.host -eq $activeHostName) { continue }
    if ($item.error) {
        Write-SupLog "tunnel: skipping host '$($item.host)' - $($item.error)"
        continue
    }
    if (-not $item.remote_repo) {
        Write-SupLog "tunnel: skipping host '$($item.host)' - no remote_repo configured"
        continue
    }
    Set-LaunchStatus "Checking backend on $($item.host)..."
    $extraCmd = New-RemoteEnsureCommand -RemoteRepo $item.remote_repo -RemoteSocketOverride $item.remote -Restart $RestartBackend
    $savedEAP2 = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    $extraStatus = ssh $sshRemoteOpts $item.ssh_alias $extraCmd 2>&1
    $extraExit = $LASTEXITCODE
    $ErrorActionPreference = $savedEAP2
    $extraStatusText = ($extraStatus | Out-String)
    if ($extraExit -ne 0) {
        Write-SupLog "tunnel: host '$($item.host)' unreachable (ssh exit $extraExit) - skipping"
        continue
    }
    if ($extraStatusText -match 'socket MISSING') {
        Write-SupLog "tunnel: host '$($item.host)' backend socket missing - skipping"
        continue
    }
    $extraRemoteSocket = $item.remote
    if (-not $extraRemoteSocket -and $extraStatusText -match 'backend-socket:\s*(\S+)') {
        $extraRemoteSocket = $matches[1]
    }
    if (-not $extraRemoteSocket) {
        Write-SupLog "tunnel: host '$($item.host)' did not report a socket path - skipping"
        continue
    }
    $extraArgs = @()
    $extraArgs += $sshCommonArgs
    $extraArgs += @('-L', "$($item.local_port):$extraRemoteSocket", $item.ssh_alias)
    try {
        $proc = Start-Process -FilePath ssh -ArgumentList $extraArgs -WindowStyle Hidden -PassThru
        $extraTunnels += [PSCustomObject]@{
            HostName     = $item.host
            SshAlias     = $item.ssh_alias
            LocalPort    = $item.local_port
            RemoteSocket = $extraRemoteSocket
            Args         = $extraArgs
            Proc         = $proc
            StartedAt    = (Get-Date)
            BackoffSec   = 0
        }
        Write-SupLog "tunnel: host '$($item.host)' forwarding 127.0.0.1:$($item.local_port) -> $extraRemoteSocket (pid=$($proc.Id))"
    } catch {
        Write-SupLog "tunnel: host '$($item.host)' failed to start ssh - $($_.Exception.Message)"
    }
}
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
# brick the launcher. -NoUpdate skips. -Local skips too (2026-09-02 Codex
# round): the self-update prelude only ever SETS SOT_LAUNCH_REBUILD when
# -not $Local, but the var is process environment, not a fresh local --
# an inherited '1' from an earlier invocation in the same shell must not
# make a later -Local run rebuild (and stop the daemon for it); -Local
# is documented as a freshness-free debug path with no exception.
# ---------------------------------------------------------------------------
if ($env:SOT_LAUNCH_REBUILD -eq '1' -and -not $NoUpdate -and -not $Local) {
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
        # Probe for cargo FIRST. Without this the missing-toolchain case is
        # reported as a SUCCESS: PowerShell raises CommandNotFoundException
        # (which 2>&1 captures into $buildOut) but leaves $LASTEXITCODE at 0
        # from the preceding successful `git` call, so the `-ne 0` test below
        # takes the else-branch and logs "frontend rebuilt" having built
        # nothing. That is the normal state on a release install (INSTALL-AGENT
        # §2b needs no Rust toolchain), so it is not an error — just say so and
        # run the staged binary.
        $cargoCmd = Get-Command cargo -ErrorAction SilentlyContinue
        if (-not $cargoCmd) {
            Write-SupLog "freshness: no cargo on PATH - release install, running the staged binary (this is normal)"
            Set-LaunchStatus 'Starting Ship of Tools...'
            $buildOut = $null
        } else {
        Set-LaunchStatus 'Rebuilding frontend...'
        Write-SupLog "freshness: cargo build -p sot-frontend"
        $buildOut = cargo build --release -p sot-frontend --manifest-path (Join-Path $repo 'rust\Cargo.toml') 2>&1
        if ($LASTEXITCODE -ne 0) {
            Set-LaunchStatus 'ERROR: frontend rebuild failed - launching existing build (see supervisor.log)'
            Write-SupLog "freshness: BUILD FAILED - launching existing binary. tail: $($buildOut | Select-Object -Last 3)"
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
        # daemon.
        #
        # Mixed-version pair guard (2026-09-02 Codex round): a locally-spawned
        # capsule supervisor (sot-capsule.exe) is a SEPARATE, detached process
        # from sotd (ADR 0042 L1a) -- the -Stop above only unpins sotd.exe, not
        # a still-running capsule's own mapped image. Rebuilding anyway would
        # publish a fresh sotd.exe next to the OLD sot-capsule.exe, and the
        # ensure right after this whole block would then start that MISMATCHED
        # pair. No versioned target dir to build into instead, so the smallest
        # fix is to skip the rebuild entirely -- both the stop (nothing to
        # unpin if nothing is about to overwrite it) and the cargo build --
        # whenever a sot-capsule.exe is running from this dev bin dir. The
        # durable answer is U4's upgrade transaction.
        $devBinDir = Split-Path $backendExe -Parent
        $capsuleSessionsAlive = @(Get-CimInstance Win32_Process -Filter "Name='sot-capsule.exe'" -ErrorAction SilentlyContinue |
            Where-Object {
                ($_.ExecutablePath -and $_.ExecutablePath -like "$devBinDir*") -or
                ($_.CommandLine -and $_.CommandLine -like "*$devBinDir*")
            })
        if ($capsuleSessionsAlive.Count -gt 0) {
            Write-SupLog "freshness: local capsule sessions alive - pair not rebuilt; the durable answer is U4's upgrade transaction"
        } else {
            if (Test-Path $sotLocalDaemon) {
                Write-SupLog 'freshness: stopping local daemon before backend rebuild (a running sotd.exe pins its own image)'
                $stopOut2 = & $sotLocalDaemon -Stop 6>&1 2>&1
                foreach ($l in @($stopOut2)) { if ("$l".Trim()) { Write-SupLog "$l" } }
            }
            Write-SupLog "freshness: cargo build -p sot-backend -p sot-log"
            $capOut = cargo build --release -p sot-backend -p sot-log --manifest-path (Join-Path $repo 'rust\Cargo.toml') 2>&1
            if ($LASTEXITCODE -ne 0) {
                Write-SupLog "freshness: backend pair rebuild FAILED (non-fatal, continuing with whatever pair exists). tail: $($capOut | Select-Object -Last 3)"
            } else {
                Write-SupLog "freshness: backend pair (sotd.exe, sot-capsule.exe) rebuilt"
            }
        }
        }
    } finally {
        $ErrorActionPreference = $savedEAP
    }
}
# ---------------------------------------------------------------------------
# Local daemon ensure (ADR 0042 L2b design D; ONE-ensure simplification
# 2026-09-02): EVERY launch mode ensures the persistent, per-user local
# sotd is running now, not just -Local -- the frontend always holds a
# "local" connection (hosts::resolve_connections adds it implicitly, ADR
# 0042 L2b design B), whether -Local's own connection or one row of the
# default mode's multi-host tree.
#
# Positioned here -- after BOTH steps in this launcher that can replace
# sotd.exe/sot-capsule.exe: the staged-update apply (near the top) and the
# dev freshness rebuild (just above, with its own stop-first guard) -- so
# this is the ONE ensure call per launch, always seeing whatever pair is
# current. It used to run before the SSH/tunnel section too, needing a
# second post-rebuild call (and a $backendPairStopped gate) to restart the
# daemon on a fresh pair; both are deleted now that there is only one call,
# made once everything that could invalidate an earlier one has run. The
# SSH/tunnel section above is explicitly gated on `-not $Local` (see its
# own header) so it still never runs for -Local, even though this ensure
# no longer sits before it.
#
# See scripts/sot-local-daemon.ps1 for the binary resolution order, the
# pipe-naming derivation (queried from the daemon itself, ADR 0042 L2b
# design C) and why -Stop reduces to Stop-Process.
#
# Fail-open in the DEFAULT mode: a local daemon that won't come up just
# means the "local" row in Hosts mode shows unreachable -- the remote
# tunnel(s) this mode exists for are unaffected. -Local has nothing else to
# fall back to, so it keeps today's hard error dialog.
# ---------------------------------------------------------------------------
if ($Local) { Stop-Splash }   # -Local is a debug path with no other progress UI
$localDaemonReady = $false
if (Test-Path $sotLocalDaemon) {
    $localOut = & $sotLocalDaemon -DevBinDir (Split-Path $backendExe -Parent) 6>&1 2>&1
    foreach ($l in @($localOut)) { if ("$l".Trim()) { Write-SupLog "$l" } }
    $localDaemonReady = ($LASTEXITCODE -eq 0)
} else {
    Write-SupLog "local daemon: sot-local-daemon.ps1 missing at $sotLocalDaemon"
}

if ($Local) {
    if (-not $localDaemonReady) {
        [System.Windows.Forms.MessageBox]::Show(
            "Local sotd is not answering.`n`nSee %LOCALAPPDATA%\sot\logs\sotd-local.log for why (no complete sotd.exe+sot-capsule.exe pair, or it did not come up in time).",
            'Ship of Tools launcher',
            'OK', 'Error') | Out-Null
        exit 1
    }
    # No --socket: the frontend derives the local connection itself
    # (hosts::resolve_connections, ADR 0042 L2b design B) from the exact
    # same function sot-local-daemon.ps1 just used to start it on.
    Start-Process -FilePath $frontendExe `
        -RedirectStandardOutput $frontendStdout `
        -RedirectStandardError $frontendStderr `
        -WindowStyle Hidden `
        -Wait
    exit 0
}
if (-not $localDaemonReady) {
    Write-SupLog "local daemon: not ready - continuing without it (fail-open; the 'local' host will show unreachable)"
}

# The frontend runs from a *staged copy* under %LOCALAPPDATA%\sot\bin so a
# `cargo build --release` can overwrite rust\target\release while the app is
# live — Windows locks a running .exe, so building in place would fail the
# link step. On exit code 75 ("rebuild done, relaunch me") we re-stage the
# fresh binary and respawn it with --relaunched (which reopens the Terminal
# drawer and runs the resume command). Any other exit code = real quit.
#
# SOT_REPO_DIR lets the frontend find the local repo (Terminal cwd for
# `claude --continue`, and the build dir for the relaunch helper).
$RelaunchExitCode = 75
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
$externalControlTunnel = Test-LocalPortOpen -Port $tcpPort
if ($externalControlTunnel) {
    Write-SupLog "control port $tcpPort is already open; starting browser aux-only tunnel"
    $sshTunnel = Start-SotAuxTunnel
} else {
    $sshTunnel = Start-SotTunnel
}
$sshStartedAt = Get-Date
Start-Sleep -Milliseconds 400

if ($token) {
    $env:SOT_TOKEN = $token
}
# Self-update notice for the frontend - a REFUSED pull's reason (empty/unset
# for offline or an ok pull; see the self-update prelude above). Set once
# here, before the FIRST Start-Process spawn below: env vars set on this
# process are inherited by every child it spawns, exit-75 respawns included
# within this SAME invocation - correct, since this invocation never pulled
# again, so the notice stays true until a fresh launch. The frontend reads
# it once at its own startup (rust/frontend/src/gpu.rs) and renders it
# through the same status/notify_sticky_until fields FeCommand::Notify uses.
Remove-Item Env:\SOT_LAUNCH_NOTICE -ErrorAction SilentlyContinue
if ($selfUpdateRefusedReason) {
    $env:SOT_LAUNCH_NOTICE = "self-update: pull refused - running the existing build ($selfUpdateRefusedReason)"
}
$relaunchNext = [bool]$Relaunched
# The splash covers the INITIAL launch only. Exit-75 relaunches keep the tunnel
# and skip freshness, and happen while the user is already in the app, so they
# get no splash — dismiss it exactly once, when the first FE window is up.
$splashDismissed = $false
$tunnelPidLabel = if ($sshTunnel) { $sshTunnel.Id } else { 'none (external control tunnel, aux retired)' }
Write-SupLog "supervisor start (relaunched=$Relaunched, tcpPort=$tcpPort, tunnelPid=$tunnelPidLabel)"
try {
    do {
        # Stage the binary for this launch, priority order:
        #   1. dev source build (the classic path — takes precedence, and a
        #      -dev build never self-updates so it cannot race an apply)
        #   2. keep the already-staged copy, which is where sot-apply.ps1
        #      installed any update it applied above (public install layout)
        #
        # `$appliedUpdate` gates the crash-loop rollback below. sot-apply.ps1
        # drops the just-applied marker only on a SUCCESSFUL apply, and the
        # marker was cleared immediately before we invoked it — so its
        # presence means "this launch is the first boot of new bits".
        $appliedUpdate = Test-Path $applyMarker
        if ($appliedUpdate) { Write-SupLog "first boot after an applied update - rollback window armed" }
        if (Test-Path $frontendExe) {
            Copy-Item -Path $frontendExe -Destination $stagedExe -Force
            Write-SupLog "staged $frontendExe -> $stagedExe (built $((Get-Item $stagedExe).LastWriteTime.ToString('o')))"
        } else {
            Write-SupLog "no source build - running the staged copy at $stagedExe"
        }

        if ($splash -and -not $splashDismissed) { Set-LaunchStatus 'Starting Ship of Tools...' }
        $frontendArgs = @('--tcp', "127.0.0.1:$tcpPort")
        if ($relaunchNext) { $frontendArgs += '--relaunched' }
        $feStartedAt = Get-Date
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

        # Hold the splash until the FE window is actually up (not merely the
        # process spawned), then dismiss it — avoids a blink of nothing between
        # splash-close and first FE paint. Caps at ~6s so a windowless/edge case
        # still writes DONE and the splash never orphans. One-shot per launch.
        if ($splash -and -not $splashDismissed) {
            for ($w = 0; $w -lt 24; $w++) {
                try { $frontend.Refresh(); if ($frontend.MainWindowHandle -ne 0) { break } } catch { }
                Start-Sleep -Milliseconds 250
            }
            Set-LaunchStatus 'DONE'
            $splashDismissed = $true
        }

        # Tunnel supervisor: poll every ssh process every 500ms while the
        # frontend runs. If one exits (laptop wake, wifi flap, backend sshd
        # restart, server kicked us idle), respawn it. Back off on rapid
        # successive failures so a permanent issue (backend unreachable) doesn't
        # hammer the network — 1s → 2s → 4s → ... capped at 30s, resets to
        # 0 as soon as a tunnel stays up for >2s. The frontend's transport
        # task is already retrying against 127.0.0.1:$tcpPort on its own
        # exponential backoff (200ms→5s), so as soon as we restore the
        # listener the frontend reconnects, hello-resumes with its cached
        # (session_id, last_seen_revision), and the daemon replays missed
        # events. No state lost as long as the backend's daemon is alive.
        #
        # Codex follow-up, item 6 (trimmed): respawn happens immediately: no
        # per-tunnel Start-Sleep before it, so a backoff on one tunnel can't
        # stack with -- or delay respawning -- another's. Any backoff a
        # respawn needed THIS pass is only applied once, as the single sleep
        # at the bottom of the loop (the largest one needed, if more than
        # one tunnel flapped this pass), in place of the normal 500ms poll
        # interval for that one iteration.
        $tunnelBackoffSec = 0
        while (-not $frontend.HasExited) {
            $pollSleepSec = 0.5
            # $sshTunnel is $null when the control port is externally held AND
            # the aux forwards are retired (nothing to supervise), or when the
            # default remote never resolved (ADR 0042 L2b codex follow-up,
            # item 3 -- Start-SotTunnel/Start-SotAuxTunnel are no-ops then).
            # Guard it explicitly rather than relying on $null.HasExited being
            # falsy -- that only holds while no one adds Set-StrictMode.
            if ($sshTunnel -and $sshTunnel.HasExited) {
                $uptime = ((Get-Date) - $sshStartedAt).TotalSeconds
                $tunnelBackoffSec = if ($uptime -lt 2) { [Math]::Min(($tunnelBackoffSec * 2 + 1), 30) } else { 0 }
                if ($tunnelBackoffSec -gt $pollSleepSec) { $pollSleepSec = $tunnelBackoffSec }
                $sshTunnel = if ($externalControlTunnel) { Start-SotAuxTunnel } else { Start-SotTunnel }
                $sshStartedAt = Get-Date
                Write-SupLog "tunnel respawned pid=$($sshTunnel.Id) (backoff=${tunnelBackoffSec}s)"
            }
            # Every OTHER configured remote's tunnel (ADR 0042 L2b design E),
            # same respawn-then-backoff shape as $sshTunnel above, one
            # instance of state per host (on the PSCustomObject itself) so
            # one host's flap doesn't reset another's.
            foreach ($et in $extraTunnels) {
                if ($et.Proc -and $et.Proc.HasExited) {
                    $etUptime = ((Get-Date) - $et.StartedAt).TotalSeconds
                    $et.BackoffSec = if ($etUptime -lt 2) { [Math]::Min(($et.BackoffSec * 2 + 1), 30) } else { 0 }
                    if ($et.BackoffSec -gt $pollSleepSec) { $pollSleepSec = $et.BackoffSec }
                    try {
                        $et.Proc = Start-Process -FilePath ssh -ArgumentList $et.Args -WindowStyle Hidden -PassThru
                        $et.StartedAt = Get-Date
                        Write-SupLog "tunnel respawned host=$($et.HostName) pid=$($et.Proc.Id) (backoff=$($et.BackoffSec)s)"
                    } catch {
                        Write-SupLog "tunnel respawn FAILED host=$($et.HostName) - $($_.Exception.Message)"
                    }
                }
            }
            # The one Start-Sleep for this iteration: the normal 500ms poll
            # cadence, stretched to the largest backoff any respawn needed
            # this pass (never more than one sleep per iteration, whether
            # zero, one, or every tunnel flapped).
            Start-Sleep -Milliseconds ([int]($pollSleepSec * 1000))
        }

        # Determine whether this was a relaunch request (75) or a real quit.
        # WaitForExit() guarantees ExitCode is populated after the poll loop.
        $frontend.WaitForExit()
        $feUptime = (Get-Date) - $feStartedAt
        $relaunchNext = ($frontend.ExitCode -eq $RelaunchExitCode)
        Write-SupLog "frontend pid=$($frontend.Id) exited code=$($frontend.ExitCode) uptime=$([int]$feUptime.TotalSeconds)s -> relaunchNext=$relaunchNext"

        # Crash-loop rollback (ADR 0030 §4): a just-applied update that dies
        # abnormally within 10s is rolled back and the FE respawns on the
        # previous binary. Delegated to sot-apply.ps1 -Rollback so the WHOLE
        # transaction reverts — binaries, the repo\current junction, and
        # install.json's version/tag — not just the exe. It also writes a
        # bad-<tag> marker so the stager never re-arms that release, which is
        # what makes this one-shot (the old inline .prev copy left install.json
        # claiming the broken version, and nothing stopped a re-arm).
        if ($appliedUpdate -and -not $relaunchNext -and $frontend.ExitCode -ne 0 `
            -and $feUptime.TotalSeconds -lt 10) {
            Write-SupLog "UPDATE CRASH-LOOP: exit=$($frontend.ExitCode) after $([int]$feUptime.TotalSeconds)s - rolling back"
            if (Test-Path $sotApply) {
                $rbOut = & $sotApply -Rollback 6>&1 2>&1
                foreach ($l in @($rbOut)) { if ("$l".Trim()) { Write-SupLog "$l" } }
            } elseif (Test-Path "$stagedExe.prev") {
                Copy-Item -Path "$stagedExe.prev" -Destination $stagedExe -Force
                Write-SupLog "sot-apply.ps1 missing - restored $stagedExe from .prev only"
            }
            $appliedUpdate = $false
            Remove-Item -Path $applyMarker -Force -ErrorAction SilentlyContinue
            $relaunchNext = $true
        }
        if ($relaunchNext) {
            # Keep the tunnel up across the respawn — the remote backend and
            # session survive, so we only re-stage + relaunch the frontend.
        }
    } while ($relaunchNext)
} finally {
    Stop-Splash   # safety — normally already closed by the DONE status write
    # Teardown ORDER is load-bearing (confirmed against the daemon code): the
    # frontend's socket close (FIN) must reach the daemon over the STILL-OPEN
    # tunnel so it drops the client (connections=N-1) immediately. If the tunnel
    # dies first the FIN can't propagate and the client is stranded as a GHOST
    # until the ADR-0027 keepalive reaper fires (~50s) — the "FE not detaching on
    # close" bug. So: frontend down (or already exited on a real quit) -> brief
    # wait for the FIN to drain -> THEN the tunnel. The deliberate
    # "clean up and shutdown" path is scripts/shutdown-sot.ps1 (/sot-fe-shutdown).
    Write-SupLog "supervisor exiting (relaunchNext=$relaunchNext) - frontend, drain FIN, then tunnel(s)"
    if ($frontend -and -not $frontend.HasExited) {
        try { Stop-Process -Id $frontend.Id -Force -ErrorAction SilentlyContinue } catch {}
    }
    Start-Sleep -Seconds 2
    if ($sshTunnel -and -not $sshTunnel.HasExited) {
        try { Stop-Process -Id $sshTunnel.Id -Force -ErrorAction SilentlyContinue } catch {}
    }
    # Every OTHER configured remote's tunnel (ADR 0042 L2b design E) — same
    # teardown as $sshTunnel above, one per host.
    foreach ($et in $extraTunnels) {
        if ($et.Proc -and -not $et.Proc.HasExited) {
            try { Stop-Process -Id $et.Proc.Id -Force -ErrorAction SilentlyContinue } catch {}
        }
    }
}

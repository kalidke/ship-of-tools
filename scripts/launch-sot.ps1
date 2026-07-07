# launch-sot.ps1 — default launcher: connect to the remote backend
# over an SSH local-port-forwarded TCP socket. Per the
# project's deployment topology (Windows local · Linux remote-in-tmux)
# this is the canonical workflow; pass `-Local` to fall back to a
# locally-spawned backend on a named pipe.
#
# Idempotent on the backend side: the backend is started once via
# `nohup` and survives across launches, so the second click is fast.
# The SSH local-forward is started fresh each launch and torn down
# when the frontend exits.
#
# Overrides (env vars):
#   SOT_HOST         SSH alias for the backend host       (default: none — see .sot/hosts.toml)
#   SOT_REMOTE_REPO  Path to the repo on the remote       (default: none — see .sot/hosts.toml)
#   SOT_TCP_PORT     Loopback port for the tunnel         (default: 18743)
#   SOT_TOKEN        App-level auth token; both sides must match (default: unset, open mode)
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

# Logs FIRST — so the progress splash and status writes can come up before any
# slow pull/build/ssh work. Append-only supervisor log: unlike the frontend
# stdout/stderr logs (which Start-Process truncates on every respawn), this
# survives across exit-75 respawns so the relaunch path — frontend exit codes,
# restage, respawn, tunnel flaps — is diagnosable after the fact. ADR 0017.
$logDir = Join-Path $env:LOCALAPPDATA 'sot\logs'
New-Item -ItemType Directory -Force -Path $logDir | Out-Null
$frontendStdout = Join-Path $logDir 'frontend.stdout.log'
$frontendStderr = Join-Path $logDir 'frontend.stderr.log'
$backendStdout  = Join-Path $logDir 'backend.stdout.log'
$backendStderr  = Join-Path $logDir 'backend.stderr.log'
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

Add-Type -AssemblyName System.Windows.Forms   # MessageBox for the fatal dialogs below

$frontendExe = Join-Path $repo 'rust\target\release\sot.exe'
$backendExe = Join-Path $repo 'rust\target\release\sotd.exe'

# Binary sources, in priority order (ADR 0030 §4): a staged pending UPDATE
# (downloaded by the updater), the dev source build, or the already-staged
# copy from a previous run. A machine with no source tree (public install
# layout) runs entirely on the latter two.
$pendingExe = Join-Path $env:LOCALAPPDATA 'sot\updates\pending\sot.exe'
$alreadyStaged = Join-Path $env:LOCALAPPDATA 'sot\bin\sot.exe'
if (-not (Test-Path $frontendExe) -and -not (Test-Path $pendingExe) -and -not (Test-Path $alreadyStaged)) {
    Set-LaunchStatus 'ERROR: No sot.exe found - build it: cargo build --release -p sot-frontend'
    Stop-Splash
    [System.Windows.Forms.MessageBox]::Show(
        "No sot.exe found (no pending update, no staged copy, no source build at $frontendExe)`n`nDev machines: cd $repo\rust; cargo build --release -p sot-frontend",
        'Ship of Tools launcher',
        'OK', 'Error') | Out-Null
    exit 1
}

# ---------------------------------------------------------------------------
# Local-only mode (-Local): spawn a fresh per-session backend on a
# named pipe and connect via that. Preserved for offline / debugging.
# ---------------------------------------------------------------------------
if ($Local) {
    Stop-Splash   # -Local is a debug path with no freshness phases; skip the splash
    if (-not (Test-Path $backendExe)) {
        [System.Windows.Forms.MessageBox]::Show(
            "sotd.exe not found at $backendExe`n`nBuild it first:`n  cd $repo\rust; cargo build --release",
            'Ship of Tools launcher',
            'OK', 'Error') | Out-Null
        exit 1
    }
    $pipeName = 'sot-' + [Guid]::NewGuid().ToString('N').Substring(0, 12)
    $pipePath = '\\.\pipe\' + $pipeName
    $backend = Start-Process -FilePath $backendExe `
        -ArgumentList @('--socket', $pipePath, '--project-root', $repo.Path) `
        -RedirectStandardOutput $backendStdout `
        -RedirectStandardError $backendStderr `
        -WindowStyle Hidden `
        -PassThru
    Start-Sleep -Milliseconds 300
    try {
        Start-Process -FilePath $frontendExe `
            -ArgumentList @('--socket', $pipePath) `
            -RedirectStandardOutput $frontendStdout `
            -RedirectStandardError $frontendStderr `
            -WindowStyle Hidden `
            -Wait
    } finally {
        if ($backend -and -not $backend.HasExited) {
            try { Stop-Process -Id $backend.Id -Force -ErrorAction SilentlyContinue } catch {}
        }
    }
    exit 0
}

# ---------------------------------------------------------------------------
# Default: SSH-to-remote backend.
#
# ADR 0015 — host registry. We read state-toml's `last_host` plus the
# `.sot/hosts.toml` table to figure out which remote to tunnel to.
# The frontend's `Mode::Hosts` picker writes `last_host`; the launcher
# reads it here, looks up the matching `[host.<name>]` block, and sets
# the existing SOT_HOST / SOT_REMOTE_REPO / SOT_TCP_PORT env
# vars from it. The original env-var-driven fallback chain still works
# (env wins → state-toml → hosts.toml default_host → error if none configured).
function Read-SotLastHost {
    $statePath = Join-Path $env:APPDATA "sot\state-$env:COMPUTERNAME.toml"
    if (-not (Test-Path $statePath)) { return $null }
    foreach ($line in Get-Content $statePath) {
        if ($line -match '^\s*last_host\s*=\s*"?([^"]+?)"?\s*$') {
            return $matches[1]
        }
    }
    return $null
}

function Read-SotHosts {
    param([string]$Path)
    $cfg = @{ default_host = $null; hosts = @{} }
    if (-not (Test-Path $Path)) { return $cfg }
    $currentHost = $null
    foreach ($line in Get-Content $Path) {
        $trim = $line.Trim()
        if (-not $trim -or $trim.StartsWith('#')) { continue }
        if ($trim -match '^\[host\.(.+)\]$') {
            $currentHost = $matches[1].Trim()
            if (-not $cfg.hosts.ContainsKey($currentHost)) {
                $cfg.hosts[$currentHost] = @{}
            }
            continue
        }
        if ($trim -match '^\[(.+)\]$') {
            # Some other section; reset host context.
            $currentHost = $null
            continue
        }
        if ($trim -match '^([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.+)$') {
            $key = $matches[1]
            $val = $matches[2].Trim().Trim('"')
            if ($currentHost) {
                $cfg.hosts[$currentHost][$key] = $val
            } elseif ($key -eq 'default_host') {
                $cfg.default_host = $val
            }
        }
    }
    return $cfg
}

$hostsTomlPath = Join-Path $repo '.sot\hosts.toml'
$hostsCfg = Read-SotHosts -Path $hostsTomlPath
$lastHost = Read-SotLastHost
$activeHostName = if ($env:SOT_HOST_NAME) {
    $env:SOT_HOST_NAME
} elseif ($lastHost) {
    $lastHost
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
}

$backendHost = if ($env:SOT_HOST) { $env:SOT_HOST } else { $null }
$remoteRepo = if ($env:SOT_REMOTE_REPO) {
    $env:SOT_REMOTE_REPO
} else {
    $null
}
if (-not $backendHost -or -not $remoteRepo) {
    Set-LaunchStatus 'ERROR: no backend host configured - run scripts/install.sh or copy .sot/hosts.toml.example'
    Stop-Splash
    [System.Windows.Forms.MessageBox]::Show(
        "No backend host configured.`n`nRun scripts/install.sh, or copy .sot/hosts.toml.example to .sot/hosts.toml and set default_host.",
        'Ship of Tools launcher',
        'OK', 'Error') | Out-Null
    exit 1
}
$tcpPort = if ($env:SOT_TCP_PORT) { [int]$env:SOT_TCP_PORT } else { 18743 }
$token = $env:SOT_TOKEN  # may be empty

# (Re)start the remote backend on every launch. Always-fresh is more
# reliable than "skip if running" — the previous behaviour happily
# reused a stuck backend whose accept loop was up but whose protocol
# task had wedged, producing instant-EOF on every reconnect.
#
# Two critical details:
#   - `pkill -x sotd` (exact program name) not `-f` (full
#     command line). `-f` matches against the *bash wrapper's* command
#     line too — the wrapper that ssh runs to host our heredoc has
#     "sotd" embedded in it as a literal pattern argument,
#     so `pkill -f` cheerfully kills the very shell trying to run
#     the kill, ssh disconnects, exit 255.
#   - `nohup … &` must redirect ALL THREE streams (`>log 2>&1
#     </dev/null`) so the backend fully detaches from ssh's stdin
#     pipe. Without `</dev/null` ssh blocks waiting for the child
#     to finish or leaves the backend half-dead when ssh exits.
$tokenArg = if ($token) { "--token $token" } else { '' }
# Interpolated into the remote script at build time (PS-side switch, bash-side test).
$restartBackendFlag = if ($RestartBackend) { '1' } else { '0' }
$remoteCmd = @"
# ADR 0030 dev-freshness rev 2 - MULTI-FE SAFE. The shared daemon is NEVER
# restarted by a launcher while running: other FEs' kernels and REPL state
# die with it. The BE updates on its own cadence - on the backend host the BE
# session's on-merge deploy keeps it current. This block only: starts a daemon that is
# DOWN, reports staleness when running, and does the full pull+build+restart
# ONLY on the explicit -RestartBackend force path. Tradeoff accepted: the old
# always-restart also cleared a WEDGED-but-accepting daemon; that rare case
# is now the force path's job. Protocol skew stays loud via the ADR 0030
# handshake gate. Echoes stay paren-free - PS 5.1 hands this to ssh unquoted.
export PATH="`$HOME/.cargo/bin:`$HOME/.local/bin:`$PATH"
if [ "$restartBackendFlag" = 1 ]; then
    cd '$remoteRepo'
    scripts/restart-backend.sh && echo "backend: force-restarted at current build" || echo "backend: force-restart FAILED"
elif pgrep -x sotd >/dev/null 2>&1 || systemctl --user is-active sotd.service >/dev/null 2>&1; then
    cd '$remoteRepo'
    if scripts/restart-backend.sh --check >/dev/null 2>&1; then
        echo "backend: running and current"
    else
        echo "backend: running but STALE - it updates on its own cadence; force with -RestartBackend"
    fi
else
    if systemctl --user is-enabled sotd.service >/dev/null 2>&1; then
        systemctl --user reset-failed sotd.service 2>/dev/null || true
        systemctl --user start sotd.service
        echo "backend: was down - started via systemd"
    else
        cd '$remoteRepo'
        nohup ./rust/target/release/sotd --tcp 127.0.0.1:$tcpPort --project-root '$remoteRepo' --label sot $tokenArg >/tmp/sotd.log 2>&1 </dev/null &
        disown
        echo "backend: was down - started nohup, pid=`$!"
    fi
fi
"@
# Normalize to LF — Windows checkouts (autocrlf=true) leave CRLF in the
# here-string, which becomes literal $'\r' tokens in bash on the remote.
$remoteCmd = $remoteCmd -replace "`r`n", "`n"
# rev 2: default launches only check staleness / start-if-down (never restart a
# running shared daemon); -RestartBackend forces the full restart-backend.sh path.
Set-LaunchStatus $(if ($RestartBackend) { "Restarting backend on $backendHost..." } else { "Checking backend on $backendHost..." })
$remoteStatus = ssh -o ConnectTimeout=10 $backendHost $remoteCmd 2>&1
if ($LASTEXITCODE -ne 0) {
    Set-LaunchStatus "ERROR: couldn't reach $backendHost (ssh exit $LASTEXITCODE) - try -Local"
    Stop-Splash
    [System.Windows.Forms.MessageBox]::Show(
        "Couldn't reach $backendHost (exit $LASTEXITCODE):`n`n$remoteStatus`n`nFall back to local with:`n  pwsh -File scripts\launch-sot.ps1 -Local",
        'Ship of Tools launcher',
        'OK', 'Error') | Out-Null
    exit 1
}
# Surface a remote force-restart failure. rev 2 only ever restarts the daemon on
# the -RestartBackend force path (the default path never touches a running shared
# daemon), so this can only fire there. Sticky warning, not a stop — if a daemon
# is up the FE still connects; staleness on the default path is expected and silent.
if ($remoteStatus -match 'force-restart FAILED') {
    Set-LaunchStatus "ERROR: backend force-restart failed on $backendHost (see restart-backend.sh output / supervisor.log)"
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
$sshArgs = @(
    '-N',
    '-o', 'ExitOnForwardFailure=yes',
    '-o', 'ServerAliveInterval=30',
    '-o', 'ServerAliveCountMax=6',
    '-L', "${tcpPort}:127.0.0.1:${tcpPort}",
    # H1.2 — forward the remote Pluto.jl server so `o` on a
    # Pluto-flavored .jl opens in the local browser.
    '-L', "${plutoPort}:127.0.0.1:${plutoPort}",
    # ADR 0018 — forward the backend's video file server so `o` on a
    # video opens it in the local browser (HTML5 <video>, native decode).
    '-L', "${videoPort}:127.0.0.1:${videoPort}",
    # ADR 0024 — forward the backend's docs site server so `W` opens the
    # built Documenter site in the local browser (full CSS/JS/sub-pages).
    '-L', "${docsPort}:127.0.0.1:${docsPort}",
    # ADR 0029 Option B — the dedicated-port pool for ROOT-relative sites
    # (an example project's __site etc.): docsPort+1..+4. Keep in sync with
    # site_serve::POOL_SIZE.
    '-L', "$($docsPort+1):127.0.0.1:$($docsPort+1)",
    '-L', "$($docsPort+2):127.0.0.1:$($docsPort+2)",
    '-L', "$($docsPort+3):127.0.0.1:$($docsPort+3)",
    '-L', "$($docsPort+4):127.0.0.1:$($docsPort+4)",
    $backendHost
)
function Start-SotTunnel {
    Start-Process -FilePath ssh `
        -ArgumentList $sshArgs `
        -WindowStyle Hidden `
        -PassThru
}

# ---------------------------------------------------------------------------
# Self-relaunch supervisor (ADR 0017).
#
# ---------------------------------------------------------------------------
# Dev-freshness (maintainer note, 2026-07-06: "launcher should always update to newest
# build on startup" — the maintainer's FE booted a stale 0.2.1-dev). Pull + rebuild the
# FRONTEND before staging. FAIL-OPEN at every step: pull failure (offline,
# conflict) or build failure (broken main) logs to the supervisor log and
# launches the existing staged/dev binary — a broken update path must never
# brick the launcher. -NoUpdate skips.
# ---------------------------------------------------------------------------
if (-not $NoUpdate -and (Test-Path (Join-Path $repo '.git'))) {
    # $ErrorActionPreference is 'Stop' for the whole script, but this block runs
    # native tools (git, cargo) and captures their stderr with 2>&1. In Windows
    # PowerShell 5.1 that combination turns EVERY stderr line into a terminating
    # NativeCommandError under 'Stop' — cargo always prints "Finished ..." to
    # stderr, and git prints fetch progress there — so the launcher would throw
    # and die mid-freshness, BEFORE the frontend ever spawns (the "taskbar
    # launcher does nothing" regression, f8fdf81). This pass is fail-open and
    # gates on $LASTEXITCODE, not on thrown errors, so relax to 'Continue' here
    # and restore 'Stop' after.
    $savedEAP = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        Set-LaunchStatus 'Checking for updates...'
        Write-SupLog "freshness: git pull --rebase --autostash"
        $pullOut = git -C $repo pull --rebase --autostash 2>&1
        Write-SupLog "freshness: git -> $($pullOut | Select-Object -Last 1)"
        if ($LASTEXITCODE -eq 0) {
            Set-LaunchStatus 'Rebuilding frontend...'
            Write-SupLog "freshness: cargo build -p sot-frontend"
            $buildOut = cargo build --release -p sot-frontend --manifest-path (Join-Path $repo 'rust\Cargo.toml') 2>&1
            if ($LASTEXITCODE -ne 0) {
                Set-LaunchStatus 'ERROR: frontend rebuild failed - launching existing build (see supervisor.log)'
                Write-SupLog "freshness: BUILD FAILED - launching existing binary. tail: $($buildOut | Select-Object -Last 3)"
            } else {
                Write-SupLog "freshness: frontend rebuilt"
            }
        } else {
            Set-LaunchStatus 'Offline or dirty tree - launching current build...'
            Write-SupLog "freshness: pull failed - launching existing binary"
        }
    } finally {
        $ErrorActionPreference = $savedEAP
    }
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

Set-LaunchStatus 'Connecting...'
$sshTunnel = Start-SotTunnel
$sshStartedAt = Get-Date
Start-Sleep -Milliseconds 400

if ($token) {
    $env:SOT_TOKEN = $token
}
$relaunchNext = [bool]$Relaunched
# The splash covers the INITIAL launch only. Exit-75 relaunches keep the tunnel
# and skip freshness, and happen while the user is already in the app, so they
# get no splash — dismiss it exactly once, when the first FE window is up.
$splashDismissed = $false
Write-SupLog "supervisor start (relaunched=$Relaunched, tcpPort=$tcpPort, tunnelPid=$($sshTunnel.Id))"
try {
    do {
        # Stage the binary for this launch, priority order (ADR 0030 §4):
        #   1. pending UPDATE (consumed by MOVE so it applies exactly once;
        #      previous staged copy kept as .prev for crash-loop rollback)
        #   2. dev source build (the classic path)
        #   3. keep the already-staged copy (no-source public install layout)
        $appliedUpdate = $false
        if (Test-Path $pendingExe) {
            if (Test-Path $stagedExe) {
                Copy-Item -Path $stagedExe -Destination "$stagedExe.prev" -Force
            }
            Move-Item -Path $pendingExe -Destination $stagedExe -Force
            $appliedUpdate = $true
            Write-SupLog "APPLIED pending update -> $stagedExe (prev kept for rollback)"
        } elseif (Test-Path $frontendExe) {
            Copy-Item -Path $frontendExe -Destination $stagedExe -Force
            Write-SupLog "staged $frontendExe -> $stagedExe (built $((Get-Item $stagedExe).LastWriteTime.ToString('o')))"
        } else {
            Write-SupLog "no pending update, no source build - running existing staged copy"
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

        # Tunnel supervisor: poll the ssh process every 500ms while the
        # frontend runs. If ssh exits (laptop wake, wifi flap, backend sshd
        # restart, server kicked us idle), respawn it. Back off on rapid
        # successive failures so a permanent issue (backend unreachable) doesn't
        # hammer the network — 1s → 2s → 4s → ... capped at 30s, resets to
        # 0 as soon as a tunnel stays up for >2s. The frontend's transport
        # task is already retrying against 127.0.0.1:$tcpPort on its own
        # exponential backoff (200ms→5s), so as soon as we restore the
        # listener the frontend reconnects, hello-resumes with its cached
        # (session_id, last_seen_revision), and the daemon replays missed
        # events. No state lost as long as the backend's daemon is alive.
        $tunnelBackoffSec = 0
        while (-not $frontend.HasExited) {
            Start-Sleep -Milliseconds 500
            if ($sshTunnel.HasExited) {
                $uptime = ((Get-Date) - $sshStartedAt).TotalSeconds
                if ($uptime -lt 2) {
                    $tunnelBackoffSec = [Math]::Min(($tunnelBackoffSec * 2 + 1), 30)
                    Start-Sleep -Seconds $tunnelBackoffSec
                } else {
                    $tunnelBackoffSec = 0
                }
                $sshTunnel = Start-SotTunnel
                $sshStartedAt = Get-Date
                Write-SupLog "tunnel respawned pid=$($sshTunnel.Id) (backoff=${tunnelBackoffSec}s)"
            }
        }

        # Determine whether this was a relaunch request (75) or a real quit.
        # WaitForExit() guarantees ExitCode is populated after the poll loop.
        $frontend.WaitForExit()
        $feUptime = (Get-Date) - $feStartedAt
        $relaunchNext = ($frontend.ExitCode -eq $RelaunchExitCode)
        Write-SupLog "frontend pid=$($frontend.Id) exited code=$($frontend.ExitCode) uptime=$([int]$feUptime.TotalSeconds)s -> relaunchNext=$relaunchNext"

        # Crash-loop rollback (ADR 0030 §4): a just-applied update that dies
        # abnormally within 10s gets rolled back to .prev and the FE respawns
        # on the previous binary. One-shot by construction — the pending file
        # was consumed at stage time, so nothing re-applies the bad update.
        if ($appliedUpdate -and -not $relaunchNext -and $frontend.ExitCode -ne 0 `
            -and $feUptime.TotalSeconds -lt 10 -and (Test-Path "$stagedExe.prev")) {
            Copy-Item -Path "$stagedExe.prev" -Destination $stagedExe -Force
            Write-SupLog "UPDATE ROLLED BACK: exit=$($frontend.ExitCode) after $([int]$feUptime.TotalSeconds)s - restored previous binary, respawning"
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
    Write-SupLog "supervisor exiting (relaunchNext=$relaunchNext) - frontend, drain FIN, then tunnel"
    if ($frontend -and -not $frontend.HasExited) {
        try { Stop-Process -Id $frontend.Id -Force -ErrorAction SilentlyContinue } catch {}
    }
    Start-Sleep -Seconds 2
    if ($sshTunnel -and -not $sshTunnel.HasExited) {
        try { Stop-Process -Id $sshTunnel.Id -Force -ErrorAction SilentlyContinue } catch {}
    }
}

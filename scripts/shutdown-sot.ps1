# shutdown-sot.ps1 - deterministic local Ship of Tools teardown.
#
# Ordering is load-bearing (confirmed against the daemon code):
#
#   1. Kill the SUPERVISOR first        - so it can't respawn the FE or race us
#      (launch-sot.ps1)                   by tearing the tunnel on FE exit.
#   2. Kill the FRONTEND (sot.exe)      - a Stop-Process on a LIVE FE makes the
#                                          OS send FIN over the STILL-OPEN tunnel;
#                                          the daemon reads EOF and drops the
#                                          client (connections=N-1) immediately.
#   3. WAIT ~2s                         - let that FIN propagate + the daemon
#                                          deregister BEFORE the tunnel dies.
#   4. Kill the TUNNEL (ssh -L :port)   - only now. If the tunnel dies before the
#                                          FIN lands, the client is stranded as a
#                                          GHOST until the ADR-0027 keepalive
#                                          reaper fires (~50s). That ghost is the
#                                          "FE not detaching on close" bug.
#
# The remote `sotd` is LEFT RUNNING on purpose (persistent-backend model, ADR
# 0010/0013): workspaces, tmux sessions, kernel + REPL survive an FE detach so
# `claude --continue` resumes. This tears down only the LOCAL frontend + its
# transport - never remote state.
#
# SCOPE: this kills EVERY local sot.exe and every launch-{sot,devenv}.ps1
# supervisor on this machine - the right scope for "shut down everything here."
# If you ever run two FEs to different hosts from one box, this stops both.
#
# This script kills the FE that hosts the calling `claude` session, so it must
# be launched DETACHED (Start-Process) and its result read from the log
# afterward - the /sot-fe-shutdown skill does exactly that. Standalone use:
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\shutdown-sot.ps1

[CmdletBinding()]
param(
    [int]$TcpPort = 18743,      # loopback port the tunnel forwards (matches SOT_TCP_PORT)
    [string]$SshAlias = $(if ($env:SOT_HOST) { $env:SOT_HOST } else { $null }), # host whose sotd we verify the detach against
    [switch]$SkipDaemonVerify   # skip the journal round-trip (offline / faster)
)

if (-not $SshAlias -and -not $SkipDaemonVerify) {
    Write-Host "no backend host configured (set SOT_HOST or pass -SshAlias) - skipping daemon-detach verification"
    $SkipDaemonVerify = $true
}

$ErrorActionPreference = 'Continue'
$log = Join-Path $env:LOCALAPPDATA 'sot\logs\shutdown.log'
New-Item -ItemType Directory -Force -Path (Split-Path $log) | Out-Null
function W([string]$m) { "$(Get-Date -Format o)  $m" | Tee-Object -FilePath $log -Append | Out-Host }

# Match helpers. The supervisor is powershell running launch-{sot,devenv}.ps1
# (devenv covers an in-memory supervisor started before the sot rename). The
# tunnel match covers BOTH sot tunnel shapes - NOT unrelated ssh sessions:
#   control: "-L <port>:127.0.0.1:<port>" (TCP mode) OR
#            "-L <port>:/run/.../sot.sock" (socket-only mode) - both start
#            "-L <port>:", so match that prefix. The old TCP-shape-only regex
#            never matched socket-mode tunnels: Get-Tun came back empty, nothing
#            was killed, and the post-check printed a FALSE "tunnel=0 / CLEAN"
#            (observed 2026-07-14: a control tunnel survived two "clean"
#            shutdowns and reached age 4 days, silently owning port 18743 so
#            every later launch went aux-only and stacks accreted).
#   aux-only: spawned when the control port was already open; carries the
#            browser forwards and always includes pluto "-L 1234:127.0.0.1:1234"
#            - anchor on that. These are sot-owned and must die with the FE.
$supRe = '-File.*launch-(sot|devenv)\.ps1'
$tunRe = "-L ${TcpPort}:|-L 1234:127\.0\.0\.1:1234"
function Get-Sup  { Get-CimInstance Win32_Process -Filter "Name='powershell.exe'" | Where-Object { $_.CommandLine -match $supRe } }
function Get-FE   { Get-CimInstance Win32_Process -Filter "Name='sot.exe'" }
function Get-Tun  { Get-CimInstance Win32_Process -Filter "Name='ssh.exe'" | Where-Object { $_.CommandLine -match $tunRe } }

W "=== shutdown-sot start (port=$TcpPort, host=$SshAlias) ==="
W ("pre: FE=[{0}] supervisor=[{1}] tunnel=[{2}]" -f `
    ((Get-FE | ForEach-Object ProcessId) -join ','),
    ((Get-Sup | ForEach-Object ProcessId) -join ','),
    ((Get-Tun | ForEach-Object ProcessId) -join ','))

# Record the daemon's most recent 'frontend disconnected' line NOW, so the
# post-kill check can distinguish a NEW disconnect (ours) from a stale one - a
# bare `tail -1` would match an old line and read as a false confirmation.
$preDisc = ''
if (-not $SkipDaemonVerify) {
    try {
        $preDisc = ssh -o ConnectTimeout=8 -o BatchMode=yes $SshAlias `
            "journalctl --user -u sotd.service --no-pager -n 400 | grep -E 'frontend disconnected' | tail -1" 2>$null
        if ($preDisc) { W "daemon last disconnect (pre): $preDisc" } else { W "daemon: no prior disconnect line in tail" }
    } catch { W "daemon pre-check skipped: $($_.Exception.Message)" }
}

# 1. Supervisor first - stop the respawn/race.
foreach ($s in Get-Sup) { W "kill supervisor pid=$($s.ProcessId)"; Stop-Process -Id $s.ProcessId -Force -ErrorAction SilentlyContinue }

# 2. Frontend - FIN over the still-open tunnel detaches the daemon client now.
$feKilled = $false
foreach ($f in Get-FE) { W "kill FE pid=$($f.ProcessId)"; Stop-Process -Id $f.ProcessId -Force -ErrorAction SilentlyContinue; $feKilled = $true }

# 3. Let the FIN propagate + the daemon deregister BEFORE the tunnel dies -
#    only meaningful if an FE was actually alive to send a FIN.
if ($feKilled) { Start-Sleep -Seconds 2 } else { W "no live FE to drain; skipping the 2s wait" }

# 4. Verify the daemon saw a NEW disconnect (not the stale pre-line), THEN tear
#    the tunnel.
if (-not $SkipDaemonVerify -and $feKilled) {
    try {
        $postDisc = ssh -o ConnectTimeout=8 -o BatchMode=yes $SshAlias `
            "journalctl --user -u sotd.service --no-pager -n 60 | grep -E 'frontend disconnected' | tail -1" 2>$null
        if ($postDisc -and $postDisc -ne $preDisc) { W "daemon detach CONFIRMED (new disconnect): $postDisc" }
        elseif ($postDisc -and $postDisc -eq $preDisc) { W "no NEW disconnect line yet (matches pre) - FIN may still be in flight or the client was already gone; ADR-0027 reaper bounds any ghost at ~50s" }
        else { W "daemon detach line not found; ADR-0027 reaper bounds any ghost at ~50s" }
    } catch { W "daemon post-check skipped: $($_.Exception.Message)" }
}

foreach ($t in Get-Tun) { W "kill tunnel pid=$($t.ProcessId)"; Stop-Process -Id $t.ProcessId -Force -ErrorAction SilentlyContinue }

# 5. Confirm the local surface is clean.
Start-Sleep -Milliseconds 500
$feN = (Get-FE | Measure-Object).Count
$supN = (Get-Sup | Measure-Object).Count
$tunN = (Get-Tun | Measure-Object).Count
W "post: FE=$feN supervisor=$supN tunnel=$tunN"
if (($feN + $supN + $tunN) -eq 0) { W "CLEAN - local frontend fully torn down; remote sotd left running by design." }
else { W "WARNING - residue remains (FE=$feN sup=$supN tun=$tunN); inspect manually." }
W "=== shutdown-sot done ==="

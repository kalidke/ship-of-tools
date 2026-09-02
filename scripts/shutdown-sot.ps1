# shutdown-sot.ps1 - deterministic local Ship of Tools teardown.
#
# Ordering is load-bearing (confirmed against the daemon code):
#
#   1. Kill the SUPERVISOR first        - so it can't respawn the FE or race us
#      (launch-sot.ps1)                   by tearing a tunnel on FE exit.
#   2. Kill the FRONTEND (sot.exe)      - a Stop-Process on a LIVE FE makes the
#                                          OS send FIN over every STILL-OPEN
#                                          tunnel; each remote daemon reads
#                                          EOF and drops the client
#                                          (connections=N-1) immediately.
#   3. WAIT ~2s                         - let that FIN propagate + every
#                                          daemon deregister BEFORE the
#                                          tunnels die.
#   4. Kill every TUNNEL (ssh -L :port) - only now, one per configured remote
#                                          (ADR 0042 L2b design E). If a
#                                          tunnel dies before its FIN lands,
#                                          that client is stranded as a
#                                          GHOST until the ADR-0027 keepalive
#                                          reaper fires (~50s). That ghost is
#                                          the "FE not detaching on close" bug.
#   5. Stop the LOCAL sotd (ADR 0042    - LAST, only after the FE (its only
#      L1c) via sot-local-daemon.ps1      possible local client) is already
#      -Stop                              gone. Delegated to that script so
#                                          the pipe-name/process-match logic
#                                          has one home, shared with
#                                          launch-sot.ps1's start path.
#
# The remote `sotd` is LEFT RUNNING on purpose (persistent-backend model, ADR
# 0010/0013): workspaces, tmux sessions, kernel + REPL survive an FE detach so
# `claude --continue` resumes. This tears down only the LOCAL frontend + its
# transport - and, per step 5, the LOCAL sotd - never remote state.
#
# The LOCAL sotd's own capsule WORKSPACES are not affected by step 5: their
# supervisors (sot-capsule.exe) are separate, DETACHED processes and the one
# authority over a workspace's live state (ADR 0042 L1a) - stopping sotd
# does not touch them, and the daemon re-adopts every still-running one via
# `--resume` the next time it starts. Stopping sotd only drops its FE
# attach connections (already gone by step 5) and its own bookkeeping.
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
    # Codex follow-up, item 8: resolved the SAME way launch-sot.ps1 resolves
    # $tcpPort -- $env:SOT_TCP_PORT first, else 18743 -- so an env-overridden
    # default-host tunnel is still matched and killed here without having to
    # pass -TcpPort explicitly every time. An explicit -TcpPort still wins
    # over both (this is only the PARAMETER's default value).
    [int]$TcpPort = $(if ($env:SOT_TCP_PORT) { [int]$env:SOT_TCP_PORT } else { 18743 }),
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
#
# ADR 0042 L2b design E: launch-sot.ps1 opens one tunnel per configured
# remote, not just $TcpPort's -- this script has to know every port it
# might need to kill a tunnel on, or a second remote's tunnel outlives every
# "clean" shutdown exactly the way the 2026-07-14 incident above describes.
# Read-SotHosts/Get-TunnelPlan (shared with launch-sot.ps1) supply that list;
# $TcpPort (env/-TcpPort override) is ALWAYS included even with no
# hosts.toml at all, matching the pre-L2b single-tunnel contract.
. (Join-Path $PSScriptRoot 'sot-hosts.ps1')
$repo = Resolve-Path -Path (Join-Path $PSScriptRoot '..')
$hostsCfg = Read-SotHosts -Path (Join-Path $repo '.sot\hosts.toml')
# Codex follow-up, item 7: Get-TunnelPlan's default-host match is by
# hosts.toml KEY, not ssh_alias -- $SshAlias above is (and stays) an SSH
# destination for the journal-verification ssh calls, a different identity.
# Resolve the KEY the same way launch-sot.ps1's $activeHostName does.
$activeHostName = if ($env:SOT_HOST_NAME) {
    $env:SOT_HOST_NAME
} elseif ($hostsCfg.default_host) {
    $hostsCfg.default_host
} else {
    $null
}
$tunnelPlan = Get-TunnelPlan -Cfg $hostsCfg -DefaultHost $activeHostName -DefaultPort $TcpPort
$tunnelPorts = @($TcpPort) + (
    $tunnelPlan | Where-Object { $_.local_port } | ForEach-Object { $_.local_port }
) | Sort-Object -Unique
$portAlt = ($tunnelPorts | ForEach-Object { "-L ${_}:" }) -join '|'

$supRe = '-File.*launch-(sot|devenv)\.ps1'
$tunRe = "$portAlt|-L 1234:127\.0\.0\.1:1234"
function Get-Sup  { Get-CimInstance Win32_Process -Filter "Name='powershell.exe'" | Where-Object { $_.CommandLine -match $supRe } }
function Get-FE   { Get-CimInstance Win32_Process -Filter "Name='sot.exe'" }
function Get-Tun  { Get-CimInstance Win32_Process -Filter "Name='ssh.exe'" | Where-Object { $_.CommandLine -match $tunRe } }

W "=== shutdown-sot start (ports=$($tunnelPorts -join ','), host=$SshAlias) ==="
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

# 5. Stop the LOCAL sotd (ADR 0042 L1c) - only now, after the FE/tunnel are
#    down. Capsule workspace supervisors are NOT stopped here - they are the
#    point (separate detached processes; sotd re-adopts them on its next
#    start). Delegated to sot-local-daemon.ps1 so the pipe-name/process-match
#    logic has ONE home -- its own -Stop exit code (0 = confirmed down) is
#    what step 6 reports below, not a second CIM query of the same process.
$sotLocalDaemon = Join-Path $PSScriptRoot 'sot-local-daemon.ps1'
$localDaemonDown = $false
if (Test-Path $sotLocalDaemon) {
    $localOut = & $sotLocalDaemon -Stop 6>&1 2>&1
    foreach ($l in @($localOut)) { if ("$l".Trim()) { W "$l" } }
    $localDaemonDown = ($LASTEXITCODE -eq 0)
} else {
    W "sot-local-daemon.ps1 missing at $sotLocalDaemon - cannot stop the local daemon"
}

# 6. Confirm the local surface is clean.
Start-Sleep -Milliseconds 500
$feN = (Get-FE | Measure-Object).Count
$supN = (Get-Sup | Measure-Object).Count
$tunN = (Get-Tun | Measure-Object).Count
$localDaemonN = if ($localDaemonDown) { 0 } else { 1 }
W "post: FE=$feN supervisor=$supN tunnel=$tunN localDaemon=$localDaemonN"
$residue = $feN + $supN + $tunN + $localDaemonN
if ($residue -eq 0) {
    W "CLEAN - local frontend and local daemon fully torn down; remote sotd left running by design."
} else {
    W "WARNING - residue remains (FE=$feN sup=$supN tun=$tunN localDaemon=$localDaemonN); inspect manually."
}
W "=== shutdown-sot done ==="
# Codex follow-up, item 10: a non-zero exit when residue remains, so a
# caller (the /sot-fe-shutdown skill, or anyone scripting this) can tell
# CLEAN from WARNING without parsing the log.
if ($residue -ne 0) { exit 1 }
exit 0

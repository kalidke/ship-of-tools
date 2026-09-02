# sot-hosts.ps1 -- shared .sot/hosts.toml reader + tunnel-plan builder
# (ADR 0042 L2b). Dot-sourced by launch-sot.ps1 (which SSH-ensures and opens
# the tunnels a plan names), shutdown-sot.ps1 (which needs every port it
# must kill a tunnel on), and scripts/tests/test-tunnel-plan.ps1.
#
# Read-SotHosts is the same simple regex parser launch-sot.ps1 used to carry
# inline -- moved here, unchanged, so it has one home instead of a copy per
# script. Same format hosts.rs parses on the Rust side; see that file's own
# doc comment for the format and the "why not a TOML library" rationale.
#
# Get-TunnelPlan is PURE (no ssh, no side effects) so it's unit-testable
# against a fixture hosts.toml without touching the network -- the actual
# ssh-ensure-and-open work stays in launch-sot.ps1 (New-RemoteEnsureCommand
# and its two call sites), which is not pure by nature and not something a
# fixture-driven test should be exercising anyway.
#
# ASCII ONLY in string literals (see the same note in launch-sot.ps1): this
# file has no BOM, so Windows PowerShell 5.1 decodes it as ANSI/cp1252 and a
# non-ASCII byte inside a string literal can mojibake into a phantom quote
# and fail the whole parse.

function Read-SotHosts {
    param([string]$Path)
    $cfg = @{ default_host = $null; hosts = @{}; order = @() }
    if (-not (Test-Path $Path)) { return $cfg }
    $currentHost = $null
    foreach ($line in Get-Content $Path) {
        $trim = $line.Trim()
        if (-not $trim -or $trim.StartsWith('#')) { continue }
        if ($trim -match '^\[host\.(.+)\]$') {
            $currentHost = $matches[1].Trim()
            if (-not $cfg.hosts.ContainsKey($currentHost)) {
                $cfg.hosts[$currentHost] = @{}
                $cfg.order += $currentHost
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

# Get-TunnelPlan: one entry per [host.<name>] section that has an ssh_alias
# (a remote -- a local-socket-only entry, e.g. a hand-written [host.local]
# override, has none and is never tunneled). Each entry is
# { host; ssh_alias; remote_repo; local_port; remote; error }, in
# hosts.toml declaration order, ready for the caller to turn into an
# `ssh -L <local_port>:<remote-or-queried-socket> <ssh_alias>` forward.
#
# tcp_port is REQUIRED per remote -- except the one entry whose ssh_alias
# equals $DefaultAlias (today's single-tunnel `default_host`), which falls
# back to $DefaultPort (SOT_TCP_PORT / 18743) for compatibility with
# hosts.toml files written before this slice. A remote missing tcp_port
# (and not the default) comes back with local_port = $null and a non-empty
# `error` naming the host and the field -- the caller logs it and moves on
# (nonfatal per host, ADR 0042 L2b design E); it is never a hard stop for
# every OTHER host's tunnel.
function Get-TunnelPlan {
    param(
        [hashtable]$Cfg,
        [string]$DefaultAlias,
        [int]$DefaultPort = 18743
    )
    $plan = @()
    foreach ($name in $Cfg.order) {
        $entry = $Cfg.hosts[$name]
        if (-not $entry.ssh_alias) { continue }   # local-socket host, not a tunnel target
        $isDefault = ($DefaultAlias -and $entry.ssh_alias -eq $DefaultAlias)
        $port = $null
        $portErr = $null
        if ($entry.tcp_port) {
            $port = [int]$entry.tcp_port
        } elseif ($isDefault) {
            $port = $DefaultPort
        } else {
            $portErr = "host '$name' has no tcp_port (required for a remote tunnel)"
        }
        $plan += [PSCustomObject]@{
            host        = $name
            ssh_alias   = $entry.ssh_alias
            remote_repo = $entry.remote_repo
            local_port  = $port
            remote      = $entry.remote_socket
            error       = $portErr
        }
    }
    return $plan
}

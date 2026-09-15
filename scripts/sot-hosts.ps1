# sot-hosts.ps1 -- shared `sotd topology plan` reader (topology plan, lane
# D). Dot-sourced by launch-sot.ps1 (which SSH-ensures and opens the
# tunnels a plan names) and shutdown-sot.ps1 (which needs every port it
# must kill a tunnel on).
#
# The old hosts.toml TOML parsing (Read-SotHosts / Get-TunnelPlan) is
# DELETED -- `sot_protocol::topology` (Rust) is the one parser for that
# file now, and `sotd topology plan --self <host>` is the one way anything
# reads it. This file is a reader of THAT command's plain-line stdout,
# nothing else -- no TOML, no `.sot\hosts.toml` path knowledge here at all.
#
# Get-SotTopologyPlan is PURE apart from the one `sotd topology plan` call
# it shells out to (no ssh, no other side effects) -- easy to fake in a
# test by pointing -SotdPath at a stub script that prints fixed lines.
#
# Contract (rust/protocol/src/topology.rs's `plan` doc comment -- read it
# there before changing this; a reviewer may still adjust the format, so
# this parses it in ONE place and nowhere else): one fact per line, first
# word a keyword, then either one value (self/hub/relay-endpoint) or a host
# name followed by a value (dial/tunnel). The value can itself contain a
# space (a Windows pipe path carries the username verbatim), so every
# split below keeps the remainder intact -- `-split ' ', 2` at each level,
# never a fixed field count. A line whose first word isn't recognised is
# ignored, not an error, so a newer sotd can add facts without breaking an
# older launcher (the ignore-unknown-keyword rule is deliberate, not an
# oversight).
#
#   self <host>
#   hub <host>
#   relay-endpoint <endpoint>
#   dial <host> <endpoint>      # one per dialable host (daemon, not frontend --
#                                # D8: a frontend box's daemon is never dialled)
#   tunnel <host> <port>        # one per dialable host except self
#
# ASCII ONLY in string literals (see the same note in launch-sot.ps1): this
# file has no BOM, so Windows PowerShell 5.1 decodes it as ANSI/cp1252 and a
# non-ASCII byte inside a string literal can mojibake into a phantom quote
# and fail the whole parse.

function Get-SotTopologyPlan {
    param(
        # Path to a built sotd(.exe). $null (or missing) is not an error --
        # a box with no daemon binary yet (first-ever launch, nothing built)
        # just gets an empty plan, same as a box with no hosts.toml today.
        [string]$SotdPath,
        # Passed through as `--self <host>`; omitted lets `sotd` derive its
        # own host_name() (the normal case -- launch-sot.ps1 never needs to
        # override this).
        [string]$SelfHost
    )
    $result = [PSCustomObject]@{
        Self          = $null
        Hub           = $null
        RelayEndpoint = $null
        Dials         = @()   # [PSCustomObject]@{ Host; Endpoint }
        Tunnels       = @()   # [PSCustomObject]@{ Host; Port }
        Error         = $null
    }
    if (-not $SotdPath -or -not (Test-Path -LiteralPath $SotdPath)) {
        $result.Error = 'no sotd binary found'
        return $result
    }
    $planArgs = @('topology', 'plan')
    if ($SelfHost) { $planArgs += @('--self', $SelfHost) }
    $savedEAP = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    $output = & $SotdPath @planArgs 2>&1
    $exit = $LASTEXITCODE
    $ErrorActionPreference = $savedEAP
    if ($exit -ne 0) {
        $result.Error = "sotd topology plan failed (exit $exit): $($output | Out-String)".Trim()
        return $result
    }
    foreach ($line in $output) {
        # First split: keyword vs. everything else, remainder intact.
        $head = "$line" -split ' ', 2
        $keyword = $head[0]
        $rest = if ($head.Length -gt 1) { $head[1] } else { '' }
        switch ($keyword) {
            'self'           { $result.Self = $rest }
            'hub'            { $result.Hub = $rest }
            'relay-endpoint' { $result.RelayEndpoint = $rest }
            'dial' {
                # Second split, same rule: host vs. the endpoint (which may
                # itself contain a space on Windows).
                $fields = $rest -split ' ', 2
                if ($fields.Length -eq 2 -and $fields[0]) {
                    $result.Dials += [PSCustomObject]@{ Host = $fields[0]; Endpoint = $fields[1] }
                }
            }
            'tunnel' {
                $fields = $rest -split ' ', 2
                $port = 0
                if ($fields.Length -eq 2 -and $fields[0] -and [int]::TryParse($fields[1], [ref]$port)) {
                    $result.Tunnels += [PSCustomObject]@{ Host = $fields[0]; Port = $port }
                }
            }
            default {
                # Unknown first word -- ignore (forward-compat with a newer
                # sotd; see the module doc above).
            }
        }
    }
    return $result
}

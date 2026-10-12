# sot-hosts.ps1 -- shared `sotd topology plan` reader (topology plan, lane
# D). Dot-sourced by launch-sot.ps1, which spawns the ssh child a plan's
# `dial` line names.
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
# Contract (rust/protocol/src/topology/mod.rs's `plan` doc comment -- read it
# there before changing this; a reviewer may still adjust the format, so
# this parses it in ONE place and nowhere else): one fact per line, first
# word a keyword, then either one value (self/hub/relay-endpoint) or a host
# name followed by a value (dial). The value can itself contain a
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
#   dial <host> <endpoint>      # one per dialable host: unix:/pipe: for this box's
#                                # own daemon, and (on the hub) for another host it
#                                # serves a relay socket for; ssh:<hub> or
#                                # ssh:<hub>/<host> everywhere else
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
            default {
                # Unknown first word -- ignore (forward-compat with a newer
                # sotd; see the module doc above).
            }
        }
    }
    return $result
}

# The ONE side-effecting topology call: `sotd topology sync --hub <alias>`
# refreshes this box's cache of the hub's hosts.toml (the hub is the only
# writer -- see .sot/hosts.toml.example). Deliberately NOT inside
# Get-SotTopologyPlan: that stays pure (a reader, no ssh, no writes), and
# shutdown-sot.ps1, which dot-sources this file only to learn ports, must
# never sync.
#
# Never throws and never fails a launch. A box that is itself the hub, an
# unreachable hub, a fetched file that does not parse, or one that does not
# list this box all leave the local copy untouched and come back as
# Ok=$false with the reason in Output -- the caller logs it and carries on
# with whatever copy it already had.
function Invoke-SotTopologySync {
    param(
        # Path to a built sotd(.exe); missing is not an error, just no sync.
        [string]$SotdPath,
        # The hub's ssh alias, from an env override. Empty is NOT an error:
        # `sotd topology sync` (no --hub) derives the hub from whatever the
        # LOCAL copy already names -- the ordinary case on every launch
        # after the first. Only a box that has never synced AND has no env
        # override fails here, and it fails with sotd's own message.
        [string]$Hub
    )
    $result = [PSCustomObject]@{ Ok = $false; Output = $null }
    if (-not $SotdPath -or -not (Test-Path -LiteralPath $SotdPath)) {
        $result.Output = 'no sotd binary found'
        return $result
    }
    $syncArgs = @('topology', 'sync')
    if ($Hub) { $syncArgs += @('--hub', $Hub) }
    $savedEAP = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    $output = & $SotdPath @syncArgs 2>&1
    $exit = $LASTEXITCODE
    $ErrorActionPreference = $savedEAP
    $result.Ok = ($exit -eq 0)
    $result.Output = ("$($output | Out-String)").Trim()
    return $result
}

# The tree's one ssh option set (rust/protocol/src/topology/ssh_bridge.rs SSH_OPTS): BatchMode never prompts,
# ConnectTimeout bounds the connect and the server's banner, ServerAlive ends a link that stops answering, and no
# multiplexing master can keep the call's pipes open. Every script ssh call passes it and runs under Invoke-SotWithin.
$SotSshOpts = @('-T', '-o', 'BatchMode=yes', '-o', 'ServerAliveInterval=15', '-o', 'ServerAliveCountMax=3',
                '-o', 'ConnectTimeout=10', '-o', 'ControlMaster=no', '-o', 'ControlPath=none', '-o', 'ControlPersist=no')

# Runs a native command under a wall-clock bound: the whole call, so a hang in key exchange or authentication ends
# at the bound as surely as a silent link. Returns @{ Code; Output; Stdout }: Output is stdout then stderr, Stdout
# the stdout alone. The call ends when the command has exited and its output has closed, both within the bound; at the
# bound the command and every process it started are ended (sot-lease.ps1's Stop-SotTree) and Code is 124 with Output
# "<FilePath> did not finish within <Seconds> s"; a command that cannot start is Code 127 with Output "<FilePath>
# could not start: <why>". It starts through sot-lease.ps1's Start-SotRedirected (its caller dot-sources that file
# too), so its input carries only $Stdin (at most 4000 bytes, so its write cannot block on a fresh pipe), written and
# closed. Windows PowerShell 5.1 has no ArgumentList, so each argument is quoted into one string here.
function Invoke-SotWithin {
    param(
        [int]$Seconds,
        [string]$FilePath,
        [string[]]$Arguments = @(),
        [string]$Stdin = ''
    )
    if ([System.Text.Encoding]::UTF8.GetByteCount($Stdin) -gt 4000) {
        throw 'Invoke-SotWithin: stdin is over 4000 bytes'
    }
    $quoted = foreach ($a in $Arguments) {
        if ($a -ne '' -and $a -notmatch '[\s"]') { $a }
        else { '"' + ($a -replace '(\\*)"', '$1$1\"' -replace '(\\+)$', '$1$1') + '"' }
    }
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        $p = Start-SotRedirected $FilePath ($quoted -join ' ')
    } catch {
        return @{ Code = 127; Output = "$FilePath could not start: $($_.Exception.GetBaseException().Message)"; Stdout = '' }
    }
    $out = $p.StandardOutput.ReadToEndAsync()
    $err = $p.StandardError.ReadToEndAsync()
    try {
        if ($Stdin) { $p.StandardInput.Write($Stdin) }
        $p.StandardInput.Close()
    } catch { }   # the process already ended: its exit code says why
    $left = { [int][Math]::Max(0, $Seconds * 1000 - $clock.ElapsedMilliseconds) }
    if (-not ($p.WaitForExit((& $left)) -and [System.Threading.Tasks.Task]::WaitAll([System.Threading.Tasks.Task[]]@($out, $err), (& $left)))) {
        Stop-SotTree $p
        return @{ Code = 124; Output = "$FilePath did not finish within $Seconds s"; Stdout = '' }
    }
    return @{ Code = $p.ExitCode; Output = ($out.Result + $err.Result); Stdout = $out.Result }
}

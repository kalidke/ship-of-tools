# comm-pipe-request.ps1 -- the transport a `pipe:` sot-comm endpoint uses to
# reach a Windows box's LOCAL daemon over its named pipe (ADR 0042
# amendment, decision 5, corrected 2026-09-07: the local daemon listens
# ONLY on \\.\pipe\sot-<user>-local -- the box's loopback SOT_TCP_PORT is
# the SSH tunnel OUT to the backend, never a second local listener -- and
# git-bash cannot open a named pipe itself, so this one small PowerShell
# script is the whole bridge).
#
# Invoked from bash (comm-lib.sh's sot_oneshot_request, comm-relay.sh's
# nc_send/nc_hold) exactly the way those callers already invoke `nc`: the
# lines they would have written to a socket are piped to THIS script's
# stdin instead, never passed on argv (a hello/request line can carry a
# token or arbitrary message text -- shell-quoting that across a
# `powershell.exe -Command` boundary is exactly the hazard this file
# avoids by reading it as data, not code). Only short, identifier-shaped
# values (a pipe name, an op name, a timeout) are real parameters.
#
# Two modes:
#   -Mode Oneshot (default) -- reads exactly two lines from stdin (a hello
#     frame, then one request frame), writes both into the pipe, then reads
#     reply lines until one is a `kind:"res"` frame whose `op` equals
#     -Op (the daemon also broadcasts `kind:"evt"` frames on the same
#     connection -- those are skipped, never matched), or -TimeoutSec
#     elapses. Prints exactly that one matching line to stdout and exits 0;
#     any failure -- a connect timeout, no matching reply, the pipe closing
#     early -- prints ONE line to stderr and exits nonzero. This mirrors
#     sot_oneshot_request's own unix:/tcp: arms, which match a reply by its
#     `op` (not `id`): a request's id and the hello's id can legitimately
#     collide (both commonly id:1), so op is the only unambiguous
#     correlation available without changing the wire protocol.
#   -Mode Hold -- reads exactly one line from stdin (a hello frame), writes
#     it, then relays EVERY line the pipe sends to stdout verbatim for up
#     to -TimeoutSec seconds (no op filtering -- the bash side's own
#     filter_inbound does that, exactly as it does for nc_hold's unix/tcp
#     arms). Used by comm-relay.sh's `ask` (a bounded reply-listening
#     window); there is no unbounded/forever form here on purpose -- a
#     Windows box never runs a persistent bridge loop (it would pin this
#     process open and block update_comm's replace-in-place, the same
#     reason comm-listen.sh starts no bridge on Windows at all).
#
# Connects with NamedPipeClientStream(".", <name>, InOut) -- the same call
# scripts/sot-local-daemon.ps1's Test-SotPipeOpen already uses to probe
# liveness -- so a `pipe:\\.\pipe\sot-<user>-local` or bare
# `pipe:sot-<user>-local` endpoint both reduce to the bare NAME on the
# bash side before this script ever runs (NamedPipeClientStream never
# takes the \\.\pipe\ prefix itself).
#
# ASCII ONLY in string literals (see the same note in sot-local-daemon.ps1
# and sot-hosts.ps1): this file has no BOM, so Windows PowerShell 5.1
# decodes it as cp1252 and a non-ASCII byte in a string literal can
# mojibake into a phantom quote and fail the whole parse.
#
# No `pwsh` was available to syntax-check this file at authoring time (a
# Linux dev box) -- static review only; see the LU6e implementation report
# for what a Windows box must verify.

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$PipeName,

    [ValidateSet('Oneshot', 'Hold')]
    [string]$Mode = 'Oneshot',

    # Required for Oneshot (the reply-matching key); ignored for Hold.
    [string]$Op,

    [int]$TimeoutSec = 10,

    [int]$ConnectTimeoutMs = 2000
)

$ErrorActionPreference = 'Stop'

if ($Mode -eq 'Oneshot' -and -not $Op) {
    [Console]::Error.WriteLine("comm-pipe-request: -Op is required in -Mode Oneshot")
    exit 1
}

# Read-line-with-timeout: StreamReader.ReadLine() has no native deadline,
# so a task-based wait bounds each individual read without needing
# cancellation support from the pipe stream itself (NamedPipeClientStream's
# own ReadTimeout support is not something this script wants to depend on
# across both PowerShell 5.1/.NET Framework and PowerShell 7/.NET). Returns
# a tri-state object so the caller can tell "timed out, try again if there
# is time left" apart from "the stream returned EOF (line is $null) inside
# the budget" -- both would otherwise collapse to the same $null.
function Read-SotPipeLine {
    param(
        [System.IO.StreamReader]$Reader,
        [int]$TimeoutMs
    )
    $task = $Reader.ReadLineAsync()
    if (-not $task.Wait([Math]::Max(1, $TimeoutMs))) {
        return [pscustomobject]@{ TimedOut = $true; Line = $null }
    }
    return [pscustomobject]@{ TimedOut = $false; Line = $task.Result }
}

$client = New-Object System.IO.Pipes.NamedPipeClientStream(
    '.', $PipeName, [System.IO.Pipes.PipeDirection]::InOut)
try {
    try {
        $client.Connect($ConnectTimeoutMs)
    } catch {
        [Console]::Error.WriteLine(
            "comm-pipe-request: could not connect to pipe '$PipeName' within ${ConnectTimeoutMs}ms: $($_.Exception.Message)")
        exit 1
    }

    $utf8NoBom = New-Object System.Text.UTF8Encoding($false)
    $reader = New-Object System.IO.StreamReader($client, $utf8NoBom)
    $writer = New-Object System.IO.StreamWriter($client, $utf8NoBom)
    $writer.NewLine = "`n"
    $writer.AutoFlush = $true

    $stdin = [Console]::In
    $helloLine = $stdin.ReadLine()
    if ([string]::IsNullOrEmpty($helloLine)) {
        [Console]::Error.WriteLine("comm-pipe-request: no hello frame on stdin")
        exit 1
    }
    $writer.WriteLine($helloLine)

    if ($Mode -eq 'Oneshot') {
        $frameLine = $stdin.ReadLine()
        if ([string]::IsNullOrEmpty($frameLine)) {
            [Console]::Error.WriteLine("comm-pipe-request: no request frame on stdin")
            exit 1
        }
        $writer.WriteLine($frameLine)

        $deadline = (Get-Date).AddSeconds($TimeoutSec)
        while ($true) {
            $remainingMs = [int](($deadline - (Get-Date)).TotalMilliseconds)
            if ($remainingMs -le 0) {
                [Console]::Error.WriteLine(
                    "comm-pipe-request: no reply for op '$Op' on pipe '$PipeName' within ${TimeoutSec}s")
                exit 1
            }
            $result = Read-SotPipeLine -Reader $reader -TimeoutMs $remainingMs
            if ($result.TimedOut) { continue }
            if ($null -eq $result.Line) {
                [Console]::Error.WriteLine(
                    "comm-pipe-request: pipe '$PipeName' closed before a matching reply for op '$Op' arrived")
                exit 1
            }
            $line = $result.Line
            if ([string]::IsNullOrWhiteSpace($line)) { continue }
            try {
                $obj = $line | ConvertFrom-Json
            } catch {
                continue   # a garbled/partial line -- keep waiting, never match on it
            }
            if ($obj.kind -eq 'res' -and $obj.op -eq $Op) {
                Write-Output $line
                exit 0
            }
            # kind:"evt" (or a res for some other op, e.g. hello's own
            # reply) -- not what we asked for; keep reading.
        }
    } else {
        # Hold: relay every line verbatim for up to $TimeoutSec seconds.
        # No request frame is sent -- being connected (post-hello) is
        # itself the "subscription"; the daemon broadcasts agent.message
        # frames to every connected, authenticated client.
        $deadline = (Get-Date).AddSeconds($TimeoutSec)
        while ($true) {
            $remainingMs = [int](($deadline - (Get-Date)).TotalMilliseconds)
            if ($remainingMs -le 0) { exit 0 }
            $result = Read-SotPipeLine -Reader $reader -TimeoutMs $remainingMs
            if ($result.TimedOut) { continue }
            if ($null -eq $result.Line) { exit 0 }   # pipe closed -- done holding
            Write-Output $result.Line
        }
    }
} finally {
    if ($client) { $client.Dispose() }
}

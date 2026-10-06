# pipe-request.ps1 -- the local-daemon suites' raw pipe client (test-local-daemon.ps1 5c and 5d). It reads exactly two
# lines from stdin (a hello frame, then one request frame), writes both into the named pipe -PipeName, then reads reply
# lines until one is a `kind:"res"` frame whose `op` equals -Op (`kind:"evt"` frames are skipped), or -TimeoutSec
# elapses. It prints that one line and exits 0; a `kind:"res"` reply to the hello that carries an `error` is printed the
# same way, and the script then exits 1 at once unless the code is `protocol_mismatch`, when it reads on for the answer
# (exit 0) or the end (exit 1). Any other failure prints ONE line to stderr and exits nonzero. Test code: it checks no
# account. The product reaches a pipe only through `sotd stdio-bridge --endpoint` (ADR 0049, User isolation).
#
# ASCII ONLY in string literals: this file has no BOM, so Windows PowerShell 5.1 decodes it as cp1252 and a non-ASCII
# byte in a string literal can mojibake into a phantom quote and fail the whole parse.

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$PipeName,

    # The reply-matching key; required.
    [string]$Op,

    [int]$TimeoutSec = 10,

    [int]$ConnectTimeoutMs = 2000
)

$ErrorActionPreference = 'Stop'

if (-not $Op) {
    [Console]::Error.WriteLine("comm-pipe-request: -Op is required")
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

# The pipe itself is read and written as UTF-8 (below), but the lines this
# script hands BACK go to stdout, and stdout is encoded with
# [Console]::OutputEncoding -- the console's codepage unless something says
# otherwise. A daemon reply is JSON carrying whatever is on a pane's screen,
# and a character with NO mapping in that codepage becomes 0x1A, a raw
# control character that makes the line invalid JSON. The caller's `jq -e .`
# then rejects the whole line ("control characters from U+0000 through U+001F
# must be escaped") and drops it. No reply ever matches on op, the request
# times out, and the caller reports -- honestly -- that the daemon did not
# answer. The daemon answered correctly every time.
#
# It fails INTERMITTENTLY, gated on what the pane happens to be showing. On a
# cp437 console the box-drawing runs map to 0xC4/0xB3 and the middot to 0xFA,
# so those survive and a broken client can look healthy for a long stretch;
# the capture that finally exposed this was spoiled by one right arrow
# (U+2192) in the pane's own text. Measured with stdout redirected to a file,
# no console attached: 2149 invalid bytes against 2610 valid ones, five runs
# out of five.
#
# The quieter half is worse than the parse failure. cp437 maps the prompt
# glyph U+276F to 0x3F, a plain question mark -- valid JSON, silently wrong
# content. Any wake gate that compares a pane line to that glyph for EQUALITY
# can therefore never match on this platform, so a watcher sits correctly
# armed, behind a probe that passed, and never types. Fixing the encoding is a
# PREREQUISITE for such a gate, not a repair of it, and the two currently live
# on different branches.
#
# Guarded: a host with no real console can refuse the assignment, and losing
# the whole transport over that would be worse than the bug.
try { [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false) } catch { }

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
        if ($obj.kind -eq 'res' -and $obj.op -eq 'hello' -and $obj.payload.error) {
            Write-Output $line
            if ($obj.payload.code -ne 'protocol_mismatch') { exit 1 }
            continue
        }
        if ($obj.kind -eq 'res' -and $obj.op -eq $Op) {
            Write-Output $line
            exit 0
        }
        # kind:"evt" (or a res for some other op, e.g. hello's own
        # accepted reply) -- not what we asked for; keep reading.
    }
} finally {
    if ($client) { $client.Dispose() }
}

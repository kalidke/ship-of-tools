# sot-lease.ps1 -- the launcher's leases on this computer's daemon: open one, hand every one over; and the launch
# scripts' redirected children: Start-SotRedirected starts each (the bridge, Start-SotBridge; a bounded run,
# sot-hosts.ps1's Invoke-SotWithin), Stop-SotTree ends one with every process it started. Dot-sourced by launch-sot.ps1,
# which keeps $LeaseReplyWaitMs, $HandoverBoundSeconds, $global:SotLeases and $global:SotLeasesUnanswered, by
# sot-local-daemon.ps1 for its probe, and by shutdown-sot.ps1 for its bounded remote reads. Functions only.
# ASCII ONLY in string literals (Windows PowerShell 5.1; see launch-sot.ps1).

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

# This computer's name as the daemon's hello declares it: $env:SOT_SELF_HOST, else the first label of the machine
# name, lowercased (rust/log/src/host/state_dir.rs host_name). The lease is local on an owner-only pipe, so this
# need not equal the window's own spelling.
function Get-SotHelloHost {
    if ($env:SOT_SELF_HOST) { return $env:SOT_SELF_HOST }
    return ([System.Net.Dns]::GetHostName().Split('.')[0]).ToLowerInvariant()
}

# One JSON string literal, quotes included (host and SID are plain, but a quote or backslash must never break the line).
function ConvertTo-SotJsonString([string]$Text) {
    return '"' + $Text.Replace('\', '\\').Replace('"', '\"') + '"'
}

# Start $FilePath with $Arguments (one command line) and its input, output and errors redirected, and return the
# process: the one way a launch script starts a child it writes to. Its input carries exactly the bytes its caller
# writes. Windows PowerShell 5.1's .NET Framework opens a redirected input as a writer in [Console]::InputEncoding and
# flushes it at once, which writes that encoding's preamble (the UTF-8 BOM, when the console's input encoding has one)
# before the caller writes anything, and the child cannot parse what follows (a daemon's hello, `bash -s`'s first
# line). So while the process starts, the console's input encoding is UTF-8 without a preamble whenever the caller's
# has one, and the caller's is put back after. Its output is read as UTF-8. A program that cannot start, or a console
# whose encoding cannot be set, throws, to the caller's own catch.
function Start-SotRedirected([string]$FilePath, [string]$Arguments) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo($FilePath, $Arguments)
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.StandardOutputEncoding = New-Object System.Text.UTF8Encoding($false)
    $callerInput = [Console]::InputEncoding
    $swap = $callerInput.GetPreamble().Length -gt 0
    if ($swap) { [Console]::InputEncoding = New-Object System.Text.UTF8Encoding($false) }
    try {
        return [System.Diagnostics.Process]::Start($psi)
    } finally {
        if ($swap) { [Console]::InputEncoding = $callerInput }
    }
}

# Start the bridge, `<SotdExe> stdio-bridge --endpoint pipe:<PipePath>`, through Start-SotRedirected: the one way the
# launcher's lease and sot-local-daemon.ps1's probe start it (ADR 0049, User isolation).
function Start-SotBridge([string]$SotdExe, [string]$PipePath) {
    return Start-SotRedirected $SotdExe ('stdio-bridge --endpoint "pipe:{0}"' -f $PipePath)
}

# End $Proc and every process it started, at any depth (a ProxyCommand ssh started, a child holding the output). Windows
# keeps a creator's id on a process after the creator exits, so a child whose parent is gone is still found; a process
# created before its recorded parent is another program that was given a reused id, and is left alone. $Proc is ended
# first, then one snapshot of the processes is walked.
function Stop-SotTree([System.Diagnostics.Process]$Proc) {
    $born = $Proc.StartTime
    try { $Proc.Kill() } catch { }   # it may have exited already; what it started is walked either way
    $all = @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue)
    $tree = @(@{ Id = $Proc.Id; Born = $born })
    for ($i = 0; $i -lt $tree.Count; $i++) {
        foreach ($c in $all) {
            if ($c.ParentProcessId -eq $tree[$i].Id -and $c.CreationDate -ge $tree[$i].Born) {
                $tree += @{ Id = [int]$c.ProcessId; Born = $c.CreationDate }
            }
        }
    }
    foreach ($t in @($tree | Select-Object -Skip 1)) { Stop-Process -Id $t.Id -Force -ErrorAction SilentlyContinue }
}

# Open a lease on the local daemon: emits the open stream when granted or unanswered, NOTHING otherwise (callers collect
# with @()). Never throws: $ErrorActionPreference is Stop and an escaped throw would kill the supervisor. The connection
# is sotd's own (`stdio-bridge --endpoint`, from Start-SotBridge): it opens the pipe at identification level and
# connects only to a pipe this OS account serves (ADR 0049, User isolation). The daemon reads the process that connects,
# so the lease names the bridge child (its pid and creation time) and lasts as long as the child's connection. The
# stream returned is the child's input: what is written to it reaches the daemon, and disposing it ends the child and
# the lease. The first line is the hello the daemon admits every connection by (role handoff, this computer and the OS
# account, the process token's user SID); the lease line follows in the same write and the two replies are read in
# order. A lease not granted is logged with the bridge's own stderr line, when it wrote one. A request the daemon has
# not answered within $LeaseReplyWaitMs ('no reply', thrown only once both lines are written) is never closed: the
# daemon may still grant it, and a granted lease whose connection ends departs as a close (ADR 0050). Its stream is
# emitted like a granted one; its bridge process is kept in $global:SotLeasesUnanswered, and with it the bridge's
# output, which the collector would otherwise close and so end the bridge.
function Open-SotLease([string]$PipePath, [string]$SotdExe) {
    $bridge = $null
    $why = ''
    try {
        $sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
        $hello = '{"v":3,"id":0,"kind":"req","op":"hello","payload":{"client_id":"sot-launcher","protocol":3,"app_version":"launcher","host":' + (ConvertTo-SotJsonString (Get-SotHelloHost)) + ',"os_user":' + (ConvertTo-SotJsonString $sid) + ',"role":"handoff"}}'
        $bridge = Start-SotBridge $SotdExe $PipePath
        $created = $bridge.StartTime.ToFileTimeUtc()
        $line = '{"v":3,"id":1,"kind":"req","op":"fe.lease","payload":{"boot":"' + (Get-SotBootId) + '","created":' + $created + ',"pid":' + $bridge.Id + '}}'
        $in = $bridge.StandardInput.BaseStream
        $bytes = (New-Object System.Text.UTF8Encoding($false)).GetBytes($hello + "`n" + $line + "`n")
        $in.Write($bytes, 0, $bytes.Length)
        $in.Flush()
        $task = $bridge.StandardOutput.ReadLineAsync()
        if (-not $task.Wait($LeaseReplyWaitMs)) { throw 'no reply' }
        if ($null -eq $task.Result) { throw 'the bridge ended' }
        $helloReply = (ConvertFrom-Json $task.Result).payload
        if ($helloReply.error) {
            $why = 'hello refused: ' + $helloReply.code
        } else {
            $task = $bridge.StandardOutput.ReadLineAsync()
            if (-not $task.Wait($LeaseReplyWaitMs)) { throw 'no reply' }
            if ($null -eq $task.Result) { throw 'the bridge ended' }
            $why = (ConvertFrom-Json $task.Result).payload.outcome
            if ($why -eq 'granted') {
                # What the daemon writes later is read and dropped, so it never fills the bridge's output.
                $null = $bridge.StandardOutput.BaseStream.CopyToAsync([System.IO.Stream]::Null)
                Write-SupLog 'relaunch: lease granted'
                return $in
            }
        }
    } catch {
        $why = $_.Exception.Message
    }
    if ($why -eq 'no reply') {
        Write-SupLog "WARNING: relaunch: the backend has not answered the lease in $LeaseReplyWaitMs ms - keeping the request open, since closing it could end this computer's sessions"
        $global:SotLeasesUnanswered += @($bridge)
        return $in
    }
    if ($bridge) {
        try { $bridge.StandardInput.Close() } catch { }
        # The bridge's exit code and its own stderr line say why it ended (a refusal, no daemon, a hello the daemon
        # could not read), whichever step above failed first. They are read once the bridge has exited (its input is
        # closed, so it ends), so a bridge that ended before the lease was written is named too.
        try {
            if ($bridge.WaitForExit($LeaseReplyWaitMs)) {
                $err = $bridge.StandardError.ReadToEnd().Trim()
                $why = "$why; the bridge exited $($bridge.ExitCode)"
                if ($err) { $why = "${why}: $err" }
            } else {
                $why = "$why; the bridge was still running $LeaseReplyWaitMs ms after its input closed"
            }
        } catch { }
        try { $bridge.Dispose() } catch { }
    }
    Write-SupLog "WARNING: relaunch: lease not granted ($why) - this computer's sessions end if no window holds the backend within $HandoverBoundSeconds s"
}

# Hand every lease this process holds over to the frontend just spawned: say so, then drop them. A request still
# unanswered (its bridge in $global:SotLeasesUnanswered) stays open behind its handover line: the daemon reads the
# handover once it grants, and closing it first would read as a close.
function Close-SotLeases {
    $kept = @(foreach ($b in $global:SotLeasesUnanswered) { try { $b.StandardInput.BaseStream } catch { } })
    foreach ($c in @($global:SotLeases)) {
        try {
            $bytes = (New-Object System.Text.UTF8Encoding($false)).GetBytes('{"v":3,"id":2,"kind":"req","op":"fe.leaving","payload":{"intent":"handover"}}' + "`n")
            $c.Write($bytes, 0, $bytes.Length)
            $c.Flush()
        } catch { }
        if ($kept -notcontains $c) {
            try { $c.Dispose() } catch { }
        }
    }
    $global:SotLeases = @()
}

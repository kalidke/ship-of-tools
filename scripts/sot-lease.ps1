# sot-lease.ps1 -- the launcher's leases on this computer's daemon: open one, hand every one over; and Start-SotBridge,
# the one way a launch script starts the bridge. Dot-sourced by launch-sot.ps1, which keeps $LeaseReplyWaitMs,
# $HandoverBoundSeconds and $global:SotLeases, and by sot-local-daemon.ps1 for its probe. Functions only.
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

# Start the bridge, `<SotdExe> stdio-bridge --endpoint pipe:<PipePath>`, with its input, output and errors redirected,
# and return the process: the one way the launcher's lease and sot-local-daemon.ps1's probe start it (ADR 0049, User
# isolation). Its input carries exactly the bytes its caller writes. Windows PowerShell 5.1's .NET Framework opens a
# redirected input as a writer in [Console]::InputEncoding and flushes it at once, which writes that encoding's
# preamble (the UTF-8 BOM, when the console's input encoding has one) before the caller writes anything, and the
# daemon cannot parse a hello behind it. So while the process starts, the console's input encoding is UTF-8 without a
# preamble whenever the caller's has one, and the caller's is put back after. A console whose encoding cannot be set
# throws, to the caller's own catch.
function Start-SotBridge([string]$SotdExe, [string]$PipePath) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo($SotdExe, ('stdio-bridge --endpoint "pipe:{0}"' -f $PipePath))
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

# Open a lease on the local daemon: emits the open stream when granted and NOTHING otherwise (callers collect with
# @()). Never throws: $ErrorActionPreference is Stop and an escaped throw would kill the supervisor. The connection is
# sotd's own (`stdio-bridge --endpoint`, started by Start-SotBridge): it opens the pipe at identification level and
# connects only to a pipe this OS account serves (ADR 0049, User isolation). The daemon reads the process that connects,
# so the lease names the bridge child (its pid and creation time) and lasts as long as the child's connection. The
# stream returned is the child's input: what is written to it reaches the daemon, and disposing it ends the child and
# the lease. The first line is the hello the daemon admits every connection by (role handoff, this computer and the OS
# account, the process token's user SID); the lease line follows in the same write and the two replies are read in
# order. A lease not granted is logged with the bridge's own stderr line, when it wrote one.
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

# Hand every lease this process holds over to the frontend just spawned: say so, then drop them.
function Close-SotLeases {
    foreach ($c in @($global:SotLeases)) {
        try {
            $bytes = (New-Object System.Text.UTF8Encoding($false)).GetBytes('{"v":3,"id":2,"kind":"req","op":"fe.leaving","payload":{"intent":"handover"}}' + "`n")
            $c.Write($bytes, 0, $bytes.Length)
            $c.Flush()
        } catch { }
        try { $c.Dispose() } catch { }
    }
    $global:SotLeases = @()
}

# sot-lease.ps1 -- the launcher's leases on this computer's daemon: open one, hand every one over.
# Dot-sourced by launch-sot.ps1, which keeps $LeaseReplyWaitMs, $HandoverBoundSeconds and $global:SotLeases.
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

# Open a lease on the local daemon: emits the open pipe stream when granted and
# NOTHING otherwise (callers collect with @()). Never throws: $ErrorActionPreference
# is Stop and an escaped throw would kill the supervisor. The first line is the hello the daemon admits every
# connection by (ADR 0049, User isolation): role handoff, this computer and the OS account (the process token's user
# SID); the lease line follows in the same write and the two replies are read in order.
function Open-SotLease([string]$PipePath) {
    $client = $null
    $why = ''
    try {
        $name = $PipePath -replace '^\\\\\.\\pipe\\', ''
        $created = [System.Diagnostics.Process]::GetCurrentProcess().StartTime.ToFileTimeUtc()
        $sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
        $hello = '{"v":3,"id":0,"kind":"req","op":"hello","payload":{"client_id":"sot-launcher","protocol":3,"app_version":"launcher","host":' + (ConvertTo-SotJsonString (Get-SotHelloHost)) + ',"os_user":' + (ConvertTo-SotJsonString $sid) + ',"role":"handoff"}}'
        $line = '{"v":3,"id":1,"kind":"req","op":"fe.lease","payload":{"boot":"' + (Get-SotBootId) + '","created":' + $created + ',"pid":' + $PID + '}}'
        $client = New-Object System.IO.Pipes.NamedPipeClientStream('.', $name, [System.IO.Pipes.PipeDirection]::InOut)
        $client.Connect($LeaseReplyWaitMs)
        $bytes = (New-Object System.Text.UTF8Encoding($false)).GetBytes($hello + "`n" + $line + "`n")
        $client.Write($bytes, 0, $bytes.Length)
        $client.Flush()
        $reader = New-Object System.IO.StreamReader($client, (New-Object System.Text.UTF8Encoding($false)), $false, 1024, $true)
        $task = $reader.ReadLineAsync()
        if (-not $task.Wait($LeaseReplyWaitMs)) { throw 'no reply' }
        $helloReply = (ConvertFrom-Json $task.Result).payload
        if ($helloReply.error) {
            $why = 'hello refused: ' + $helloReply.code
        } else {
            $task = $reader.ReadLineAsync()
            if (-not $task.Wait($LeaseReplyWaitMs)) { throw 'no reply' }
            $why = (ConvertFrom-Json $task.Result).payload.outcome
            if ($why -eq 'granted') {
                Write-SupLog 'relaunch: lease granted'
                return $client
            }
        }
    } catch {
        $why = $_.Exception.Message
    }
    if ($client) { try { $client.Dispose() } catch { } }
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

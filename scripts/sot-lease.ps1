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

# Open a lease on the local daemon: emits the open pipe stream when granted and
# NOTHING otherwise (callers collect with @()). Never throws: $ErrorActionPreference
# is Stop and an escaped throw would kill the supervisor.
function Open-SotLease([string]$PipePath) {
    $client = $null
    $why = ''
    try {
        $name = $PipePath -replace '^\\\\\.\\pipe\\', ''
        $created = [System.Diagnostics.Process]::GetCurrentProcess().StartTime.ToFileTimeUtc()
        $line = '{"v":3,"id":1,"kind":"req","op":"fe.lease","payload":{"boot":"' + (Get-SotBootId) + '","created":' + $created + ',"pid":' + $PID + '}}'
        $client = New-Object System.IO.Pipes.NamedPipeClientStream('.', $name, [System.IO.Pipes.PipeDirection]::InOut)
        $client.Connect($LeaseReplyWaitMs)
        $bytes = (New-Object System.Text.UTF8Encoding($false)).GetBytes($line + "`n")
        $client.Write($bytes, 0, $bytes.Length)
        $client.Flush()
        $reader = New-Object System.IO.StreamReader($client, (New-Object System.Text.UTF8Encoding($false)), $false, 1024, $true)
        $task = $reader.ReadLineAsync()
        if (-not $task.Wait($LeaseReplyWaitMs)) { throw 'no reply' }
        $why = (ConvertFrom-Json $task.Result).payload.outcome
        if ($why -eq 'granted') {
            Write-SupLog 'relaunch: lease granted'
            return $client
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

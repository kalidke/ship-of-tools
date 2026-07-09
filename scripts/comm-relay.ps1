[CmdletBinding(DefaultParameterSetName = 'Send')]
param(
    [Parameter(ParameterSetName = 'Send', Mandatory = $true, Position = 0)]
    [string]$To,

    [Parameter(ParameterSetName = 'Send', Mandatory = $true, Position = 1)]
    [string]$Message,

    [Parameter(ParameterSetName = 'Listen')]
    [switch]$Listen,

    [Parameter(ParameterSetName = 'Listen')]
    [int]$Seconds = 30,

    [string]$From,
    [string]$Endpoint = $env:SOT_RELAY_ENDPOINT,
    [string]$Token = $env:SOT_TOKEN
)

$ErrorActionPreference = 'Stop'

if (-not $Endpoint) {
    $port = if ($env:SOT_TCP_PORT) { $env:SOT_TCP_PORT } else { '18743' }
    $Endpoint = "tcp:127.0.0.1:$port"
}
if ($Endpoint -notmatch '^tcp:([^:]+):(\d+)$') {
    throw "comm-relay.ps1 only supports tcp endpoints on Windows; got '$Endpoint'"
}
$hostName = $matches[1]
$portNum = [int]$matches[2]

if (-not $From) {
    $From = 'win-fe-' + $env:COMPUTERNAME.ToLowerInvariant()
}
if ($To.StartsWith('@')) {
    $To = $To.Substring(1)
}

function ConvertTo-SotJsonLine($Value) {
    $Value | ConvertTo-Json -Compress -Depth 12
}

function Read-SotLine {
    param([IO.StreamReader]$Reader)
    try {
        return $Reader.ReadLine()
    } catch [IO.IOException] {
        return $null
    }
}

$client = [Net.Sockets.TcpClient]::new()
try {
    $client.Connect($hostName, $portNum)
    $stream = $client.GetStream()
    $stream.ReadTimeout = 1000
    $writer = [IO.StreamWriter]::new($stream, [Text.UTF8Encoding]::new($false))
    $writer.NewLine = "`n"
    $writer.AutoFlush = $true
    $reader = [IO.StreamReader]::new($stream, [Text.UTF8Encoding]::new($false))

    $hello = [pscustomobject]@{
        v = 1
        id = 1
        kind = 'req'
        op = 'hello'
        payload = [pscustomobject]@{
            client_id = 'sot-comm-ps'
            last_seen_revision = 0
            protocol = 1
            app_version = 'comm-ps'
            token = if ($Token) { $Token } else { '' }
        }
    }
    $writer.WriteLine((ConvertTo-SotJsonLine $hello))

    if ($PSCmdlet.ParameterSetName -eq 'Send') {
        $send = [pscustomobject]@{
            v = 1
            id = 2
            kind = 'req'
            op = 'agent.send'
            payload = [pscustomobject]@{
                from = $From
                to = $To
                text = $Message
            }
        }
        $writer.WriteLine((ConvertTo-SotJsonLine $send))
    }

    $deadline = (Get-Date).AddSeconds($(if ($PSCmdlet.ParameterSetName -eq 'Listen') { $Seconds } else { 6 }))
    $sawAck = $false
    while ((Get-Date) -lt $deadline) {
        $line = Read-SotLine $reader
        if (-not $line) { continue }
        try { $obj = $line | ConvertFrom-Json } catch { Write-Output $line; continue }
        if ($obj.op -eq 'agent.send' -and $obj.payload.ok) {
            $sawAck = $true
            Write-Output "relayed -> $To via $Endpoint"
            if ($PSCmdlet.ParameterSetName -eq 'Send') { continue }
        } elseif ($obj.op -eq 'agent.message') {
            $p = $obj.payload
            Write-Output ("[{0}] [{1} -> {2}] {3}" -f $p.ts, $p.from, $p.to, $p.text)
        }
    }
    if ($PSCmdlet.ParameterSetName -eq 'Send' -and -not $sawAck) {
        throw "no agent.send ack from $Endpoint"
    }
} finally {
    if ($client) { $client.Close() }
}

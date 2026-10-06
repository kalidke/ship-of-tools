# test-local-daemon-own.ps1 -- sections 4b, 4c and 5b2, own-account pipe/process checks and bridge lease handover
# (ADR 0049, User isolation). Dot-sourced after section 5b by test-local-daemon.ps1 in its scope: $script, $p3,
# $realSotd, $spacedProjectRoot and the support helpers. ASCII only; Windows PowerShell 5.1.

try {
    Write-Host "`n=== 4b. a pipe another OS account serves is not this daemon (ADR 0049) ===" -ForegroundColor Cyan
    $ownAst = [System.Management.Automation.Language.Parser]::ParseFile($script, [ref]$null, [ref]$null)
    foreach ($fname in @('Test-SotPipeOpen', 'Get-LocalDaemonProcess')) {
        $fn = $ownAst.Find({ param($n) ($n -is [System.Management.Automation.Language.FunctionDefinitionAst]) -and $n.Name -eq $fname }, $true)
        if (-not $fn) { throw "function not found: $fname" }
        . ([scriptblock]::Create($fn.Extent.Text))
    }
    $ownProcessFn = $ownAst.Find({ param($n) ($n -is [System.Management.Automation.Language.FunctionDefinitionAst]) -and $n.Name -eq 'Test-SotOwnProcess' }, $true)
    if ($ownProcessFn) { . ([scriptblock]::Create($ownProcessFn.Extent.Text)) }
    $daemonExe = $realSotd
    $pipe4b = New-TestPipeName
    $server4b = New-Object System.IO.Pipes.NamedPipeServerStream($pipe4b, [System.IO.Pipes.PipeDirection]::InOut, 1, [System.IO.Pipes.PipeTransmissionMode]::Byte, [System.IO.Pipes.PipeOptions]::Asynchronous)
    try {
        $accept4b = $server4b.WaitForConnectionAsync()
        $open4b = Test-SotPipeOpen $pipe4b
        if (-not $accept4b.Wait(5000)) { throw 'the own-account pipe fixture was not reached within 5 s' }
        Check '4b: a pipe this account serves is open' $open4b 'the production probe refused the test-owned pipe'
    } finally { $server4b.Dispose() }
    Check '4b: epmapper, served by SYSTEM, is not this daemon' (-not (Test-SotPipeOpen 'epmapper')) 'the production probe opened a foreign pipe'
} catch { Check '4b: section ran' $false $_.Exception.Message }

try {
    Write-Host "`n=== 4c. a process another OS account owns is not this daemon (ADR 0049) ===" -ForegroundColor Cyan
    $PipePath = '\\.\pipe\sot-4c'
    $me = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    function Get-CimInstance { [CmdletBinding()] param([Parameter(Position = 0)] $ClassName, $Filter)
        @([pscustomobject]@{ ProcessId = 101; CommandLine = "sotd.exe --socket `"$PipePath`"" },
          [pscustomobject]@{ ProcessId = 202; CommandLine = "sotd.exe --socket `"$PipePath`"" }) }
    function Invoke-CimMethod { [CmdletBinding()] param($InputObject, $MethodName)
        [pscustomobject]@{ Sid = $(if ($InputObject.ProcessId -eq 101) { $me } else { 'S-1-5-18' }) } }
    try {
        $found4c = @(Get-LocalDaemonProcess | ForEach-Object ProcessId)
        Check '4c: only this account''s sotd.exe is this daemon' (($found4c.Count -eq 1) -and ($found4c[0] -eq 101)) "found: $($found4c -join ', ')"
    } finally { Remove-Item function:Get-CimInstance, function:Invoke-CimMethod }
    # These controls exercise the new helper when present; the parent's finder has no helper yet.
    if ($ownProcessFn) {
        Check '4c: this process is this account''s' (Test-SotOwnProcess (Get-CimInstance Win32_Process -Filter "ProcessId = $PID") $me) 'this process was refused'
        $wininit4c = Get-CimInstance Win32_Process -Filter "Name='wininit.exe'" | Select-Object -First 1
        if (-not $wininit4c) { throw 'the wininit.exe control process is missing' }
        Check '4c: wininit.exe is not this account''s' (-not (Test-SotOwnProcess $wininit4c $me)) 'a SYSTEM process was accepted'
    }
} catch { Check '4c: section ran' $false $_.Exception.Message }

try {
    Write-Host "`n=== 5b2. the launcher's handover through its bridge reaches the daemon (ADR 0049) ===" -ForegroundColor Cyan
    $pipe5b2 = New-TestPipeName
    try {
        $out5b2 = & $script -Prefix $p3 -DevBinDir 'C:\sot-test-does-not-exist' -PipeName $pipe5b2 -ProjectRoot $spacedProjectRoot 6>&1 2>&1
        if (-not (Wait-Pipe $pipe5b2)) { throw "the handover daemon never opened its pipe; log: $out5b2" }
        $global:SotLeases = @(Open-SotLease (Get-PipePath $pipe5b2) $realSotd)
        Check '5b2: the real daemon grants the bridge-held lease' ($global:SotLeases.Count -eq 1) "got $($global:SotLeases.Count); log: $($script:supLines5b -join ' | ')"
        Close-SotLeases
        Start-Sleep -Seconds 5
        Check '5b2: the handover holds the daemon' (Test-PipeAnswering $pipe5b2) 'the daemon ended before the 60 s handover bound'
    } finally {
        Close-SotLeases
        Get-DaemonProcs (Get-PipePath $pipe5b2) | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    }
} catch { Check '5b2: section ran' $false $_.Exception.Message }

# test-local-daemon-decoy.ps1 -- section 4d, -Stop selects the daemon by its pipe's owner (ADR 0050). Dot-sourced after the
# fake daemon is compiled by test-local-daemon.ps1, in its scope: $script, $compiled, $fakeExe, $root and the support
# helpers. ASCII only; Windows PowerShell 5.1.

if ($compiled) {
    try {
    Write-Host "`n=== 4d. -Stop ends the daemon its pipe's owner is, never a decoy naming the pipe or a daemon on another pipe (ADR 0050) ===" -ForegroundColor Cyan
    Clear-FakeEnv
    $p4d = New-FakePrefix 'p4d'
    $pipe4dA = New-TestPipeName
    $pipe4dB = New-TestPipeName
    $decoyDir = Join-Path $root 'decoy4d'
    New-Item -ItemType Directory -Force -Path $decoyDir | Out-Null
    # A sotd.exe by name whose command line carries this daemon's --socket token exactly: the old finder matched it.
    $decoyExe = Join-Path $decoyDir 'sotd.exe'
    Copy-Item -LiteralPath (Join-Path $env:SystemRoot 'System32\cmd.exe') -Destination $decoyExe -Force
    $daemonA = $null; $daemonB = $null; $decoy = $null
    try {
        $daemonA = Start-Process -FilePath $fakeExe -ArgumentList @('--socket', (Get-PipePath $pipe4dA)) -WindowStyle Hidden -PassThru
        $daemonB = Start-Process -FilePath $fakeExe -ArgumentList @('--socket', (Get-PipePath $pipe4dB)) -WindowStyle Hidden -PassThru
        $decoy = Start-Process -FilePath $decoyExe -ArgumentList ('/c ping -n 120 127.0.0.1 >nul & rem --socket ' + (Get-PipePath $pipe4dA)) -WindowStyle Hidden -PassThru
        $null = @(Get-DaemonProcs (Get-PipePath $pipe4dA)); $null = @(Get-DaemonProcs (Get-PipePath $pipe4dB))
        Check '4d: both daemons answer' ((Wait-Pipe $pipe4dA) -and (Wait-Pipe $pipe4dB)) 'a stand-in daemon never opened its pipe'
        $out4d = & $script -Stop -Prefix $p4d -PipeName $pipe4dA 6>&1 2>&1
        $exit4d = $LASTEXITCODE
        Check '4d: stop exit code 0' ($exit4d -eq 0) "got $exit4d; log: $out4d"
        Check '4d: this account''s daemon is stopped' (Wait-PipeGone $pipe4dA) 'its pipe still answers'
        $daemonA.Refresh(); $decoy.Refresh(); $daemonB.Refresh()
        Check '4d: the daemon process is gone' $daemonA.HasExited 'it is still running'
        Check '4d: the decoy naming its pipe lives' (-not $decoy.HasExited) 'the decoy was ended'
        Check '4d: the daemon on the other pipe lives' ((-not $daemonB.HasExited) -and (Test-PipeAnswering $pipe4dB)) 'the other daemon was ended'
    } finally {
        foreach ($q in @($daemonA, $daemonB, $decoy)) {
            if ($q -and -not $q.HasExited) { try { Stop-Process -Id $q.Id -Force -ErrorAction SilentlyContinue } catch { } }
        }
        Clear-FakeEnv
    }
    } catch { Check '4d: section ran' $false $_.Exception.Message }
}

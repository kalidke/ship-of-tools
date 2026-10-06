# test-launcher-leases.ps1 -- launch-sot.ps1's order and lease checks (sections 9-11 and 16 of the old test-local-daemon.ps1).
# Reads launch-sot.ps1 and sot-lease.ps1 as syntax trees; sections 11 and 16 run the lease against the fake sotd.exe.
# Run under WINDOWS POWERSHELL 5.1: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\tests\test-launcher-leases.ps1

. (Join-Path $PSScriptRoot 'test-local-daemon-support.ps1')

try {
    . (Join-Path $PSScriptRoot 'test-local-daemon-fake.ps1')

    # ---- 9-11: launch-sot.ps1 order (AST) and the converge lease (C4b) ----
    $launchPath = Join-Path $repo 'scripts\launch-sot.ps1'
    $launchTokens = $null; $launchErrs = $null
    $launchAst = [System.Management.Automation.Language.Parser]::ParseFile($launchPath, [ref]$launchTokens, [ref]$launchErrs)
    $leasePath = Join-Path $repo 'scripts\sot-lease.ps1'
    $leaseAst = [System.Management.Automation.Language.Parser]::ParseFile($leasePath, [ref]$null, [ref]$null)
    function Test-HasLoopAncestor($node) {
        $p = $node.Parent
        while ($p) {
            if ($p -is [System.Management.Automation.Language.LoopStatementAst]) { return $true }
            $p = $p.Parent
        }
        return $false
    }
    function Find-Calls($root, [string]$cmdName) {
        $root.FindAll({ param($n) ($n -is [System.Management.Automation.Language.CommandAst]) -and ($n.GetCommandName() -eq $cmdName) }, $true)
    }
    function Find-Ifs($root, [string]$condText) {
        $root.FindAll({ param($n) ($n -is [System.Management.Automation.Language.IfStatementAst]) -and ($n.Clauses[0].Item1.Extent.Text -like $condText) }, $true)
    }

    try {
    Write-Host "`n=== 9. InitialEnsureBeforeChecks: the first ensure precedes the -Local and nothing-reachable checks ===" -ForegroundColor Cyan
    $ensure9 = @($launchAst.FindAll({ param($n)
        ($n -is [System.Management.Automation.Language.AssignmentStatementAst]) -and
        $n.Left.Extent.Text -eq '$localDaemonReady' -and $n.Right.Extent.Text -eq 'Invoke-LocalDaemonEnsure' }, $true) |
        Where-Object { -not (Test-HasLoopAncestor $_) })
    # Only the -Local branch that CONSUMES the ensure result: a UI step such as
    # `if ($Local) { Stop-Splash }` deliberately runs before the ensure.
    $ifLocal9 = @(Find-Ifs $launchAst '$Local' | Where-Object { ($_.Parent -eq $launchAst.EndBlock) -and ($_.Extent.Text -match '\$localDaemonReady') })
    $ifNone9 = @(Find-Ifs $launchAst '*-not $defaultRemoteOk*' | Where-Object { $_.Parent -eq $launchAst.EndBlock })
    Check '9: one top-level ensure assignment' ($ensure9.Count -eq 1) "found $($ensure9.Count)"
    Check '9: one top-level if ($Local) that reads the ensure' ($ifLocal9.Count -eq 1) "found $($ifLocal9.Count)"
    Check '9: one top-level nothing-reachable check' ($ifNone9.Count -eq 1) "found $($ifNone9.Count)"
    if ($ensure9.Count -eq 1 -and $ifLocal9.Count -eq 1 -and $ifNone9.Count -eq 1) {
        Check '9: ensure precedes if ($Local)' ($ensure9[0].Extent.StartOffset -lt $ifLocal9[0].Extent.StartOffset) 'order is wrong'
        Check '9: ensure precedes the nothing-reachable check' ($ensure9[0].Extent.StartOffset -lt $ifNone9[0].Extent.StartOffset) 'order is wrong'
    }

    } catch { Check '9: section ran' $false $_.Exception.Message }
    try {
    Write-Host "`n=== 10. RespawnEnsuresDaemon / ConvergeLeaseOrder: ensure and lease order in the supervisor loop ===" -ForegroundColor Cyan
    $loop10 = @($launchAst.FindAll({ param($n)
        ($n -is [System.Management.Automation.Language.DoWhileStatementAst]) -and $n.Condition.Extent.Text -eq '$relaunchNext' }, $true))
    Check '10: one do-while on $relaunchNext' ($loop10.Count -eq 1) "found $($loop10.Count)"
    if ($loop10.Count -eq 1) {
        $loop = $loop10[0]
        $first10 = $loop.Body.Statements[0]
        $firstIsIf = $first10 -is [System.Management.Automation.Language.IfStatementAst]
        Check '10: the first body statement is if ($relaunchNext)' ($firstIsIf -and $first10.Clauses[0].Item1.Extent.Text -eq '$relaunchNext') 'first statement is not if ($relaunchNext)'
        if ($firstIsIf) {
            $en10 = @(Find-Calls $first10 'Invoke-LocalDaemonEnsure')
            $ls10 = @(Find-Calls $first10 'Open-SotLease')
            Check '10: that block ensures the daemon' ($en10.Count -eq 1) "found $($en10.Count)"
            Check '10: that block opens a lease' ($ls10.Count -eq 1) "found $($ls10.Count)"
            if ($en10.Count -eq 1 -and $ls10.Count -eq 1) {
                Check '10: the lease follows the ensure' ($en10[0].Extent.StartOffset -lt $ls10[0].Extent.StartOffset) 'order is wrong'
            }
        }
        $allEn = @(Find-Calls $loop 'Invoke-LocalDaemonEnsure')
        Check '10: no other ensure in the loop' ($allEn.Count -eq 1) "found $($allEn.Count)"
        $ifConv = @(Find-Ifs $loop '$convergeRequested')
        $conv10 = @($ifConv | Where-Object { @(Find-Calls $_ 'Invoke-PendingApply').Count -gt 0 })
        Check '10: one if ($convergeRequested) block holds the pending apply' ($conv10.Count -eq 1) "found $($conv10.Count)"
        if ($conv10.Count -eq 1) {
            $lease10 = @(Find-Calls $conv10[0] 'Open-SotLease')
            $apply10 = @(Find-Calls $conv10[0] 'Invoke-PendingApply')
            Check '10: the converge opens its lease' ($lease10.Count -eq 1) "found $($lease10.Count)"
            if ($lease10.Count -eq 1 -and $apply10.Count -ge 1) {
                Check '10: the lease precedes Invoke-PendingApply' ($lease10[0].Extent.StartOffset -lt $apply10[0].Extent.StartOffset) 'order is wrong'
            }
        }
        $spawn10 = @($loop.FindAll({ param($n)
            ($n -is [System.Management.Automation.Language.CommandAst]) -and $n.GetCommandName() -eq 'Start-Process' -and
            $n.Extent.Text -match '-FilePath\s+\$stagedExe' }, $true))
        $close10 = @(Find-Calls $loop 'Close-SotLeases')
        Check '10: one frontend spawn and one Close-SotLeases call' (($spawn10.Count -eq 1) -and ($close10.Count -eq 1)) "spawns $($spawn10.Count), closes $($close10.Count)"
        if ($spawn10.Count -eq 1 -and $close10.Count -eq 1) {
            Check '10: Close-SotLeases follows the frontend spawn' ($close10[0].Extent.StartOffset -gt $spawn10[0].Extent.StartOffset) 'order is wrong'
        }
    }

    } catch { Check '10: section ran' $false $_.Exception.Message }
    if ($compiled) {
        try {
        Write-Host "`n=== 11. ConvergeLeaseHandover: the hello and lease lines, the handover line, the refusal warnings ===" -ForegroundColor Cyan
        foreach ($fname in @('Get-SotBootId', 'Get-SotHelloHost', 'ConvertTo-SotJsonString', 'Open-SotLease', 'Start-SotBridge', 'Close-SotLeases')) {
            $fn = $leaseAst.Find({ param($n) ($n -is [System.Management.Automation.Language.FunctionDefinitionAst]) -and $n.Name -eq $fname }, $true)
            Check "11: $fname is defined" ($null -ne $fn) 'function not found'
            if ($fn) { . ([scriptblock]::Create($fn.Extent.Text)) }
        }
        $LeaseReplyWaitMs = 5000
        $HandoverBoundSeconds = 60
        $global:SotLeases = @()
        $script:supLines = @()
        function Write-SupLog { param([string]$Message) $script:supLines += $Message }
        $haveFns = (Get-Command Open-SotLease -ErrorAction SilentlyContinue) -and (Get-Command Close-SotLeases -ErrorAction SilentlyContinue) -and (Get-Command Get-SotBootId -ErrorAction SilentlyContinue)

        function Start-LeaseFake([string]$Pipe, [string]$LogPath) {
            Clear-FakeEnv
            $env:FAKE_SOTD_LOG = $LogPath
            $proc = Start-Process -FilePath $fakeExe -ArgumentList @('--socket', (Get-PipePath $Pipe)) -WindowStyle Hidden -PassThru
            return $proc
        }

        if ($haveFns) {
            # (a) granted
            $pipe11a = New-TestPipeName
            $log11a = Join-Path $root 'lease11a.log'
            $fake11a = Start-LeaseFake $pipe11a $log11a
            try {
                Check '11a: the fake pipe is up' (Wait-Pipe $pipe11a) 'pipe never answered'
                $script:supLines = @()
                $streams = @(Open-SotLease (Get-PipePath $pipe11a) $realSotd)
                Check '11a: one stream comes back' ($streams.Count -eq 1) "got $($streams.Count)"
                $global:SotLeases = $streams
                Check '11a: logged the grant' (($script:supLines -join ' ') -match 'lease granted') "log: $($script:supLines -join ' | ')"
                $regOut = (& reg query 'HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Memory Management\PrefetchParameters' /v BootId) -join ' '
                $bootDec = ''
                if ($regOut -match 'BootId\s+REG_DWORD\s+0x([0-9a-fA-F]+)') { $bootDec = [string][Convert]::ToUInt32($Matches[1], 16) }
                $goldenPrefix = '{"v":3,"id":1,"kind":"req","op":"fe.lease","payload":{"boot":"' + $bootDec + '","created":'
                # The hello that precedes it (ADR 0049, User isolation): a handoff naming this computer and the OS account.
                $helloHost11 = if ($env:SOT_SELF_HOST) { $env:SOT_SELF_HOST } else { ([System.Net.Dns]::GetHostName().Split('.')[0]).ToLowerInvariant() }
                $sid11 = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
                $goldenHello = '{"v":3,"id":0,"kind":"req","op":"hello","payload":{"client_id":"sot-launcher","protocol":3,"app_version":"launcher","host":"' + $helloHost11 + '","os_user":"' + $sid11 + '","role":"handoff"}}'
                $wait11a = [System.Diagnostics.Stopwatch]::StartNew()
                do {
                    $lines11a = @(Get-Content -LiteralPath $log11a -ErrorAction SilentlyContinue)
                    if ($lines11a.Count -ge 2) { break }
                    Start-Sleep -Milliseconds 50
                } while ($wait11a.ElapsedMilliseconds -lt 10000)
                Check '11a: at least 2 log lines arrive within 10 s' ($lines11a.Count -ge 2) "lines: $($lines11a -join ' | ')"
                Check '11a: the first logged line is the golden hello line' (($lines11a.Count -ge 1) -and ($lines11a[0] -ceq $goldenHello)) "got: $($lines11a[0]) want: $goldenHello"
                $leaseChild11 = $null
                $heldByChild11 = $false
                if ($lines11a.Count -ge 2 -and $lines11a[1].StartsWith($goldenPrefix) -and $lines11a[1].Contains(',"pid":')) {
                    $payload11 = ($lines11a[1] | ConvertFrom-Json).payload
                    $leaseChild11 = Get-Process -Id $payload11.pid -ErrorAction SilentlyContinue
                    $heldByChild11 = ($payload11.boot -ceq $bootDec) -and ($payload11.pid -ne $PID) -and
                        $leaseChild11 -and ($leaseChild11.ProcessName -eq 'sotd') -and
                        ($leaseChild11.StartTime.ToFileTimeUtc() -eq $payload11.created)
                }
                Check '11a: the second logged line is the lease, held by the bridge child' $heldByChild11 "lines: $($lines11a -join ' | ')"
                # The lease line speaks the protocol its own hello declares: the real daemon's gate holds that hello to
                # PROTOCOL_VERSION, and reads the lease's fields into its FeLeaseReq, granting only the bridge's own
                # boot, pid and creation time (5b, 5b2).
                $sameProtocol11 = $false
                if ($lines11a.Count -ge 2) {
                    $sameProtocol11 = (($lines11a[1] | ConvertFrom-Json).v -eq ($lines11a[0] | ConvertFrom-Json).payload.protocol)
                }
                Check '11a: the lease line speaks the protocol its hello declares' $sameProtocol11 "lines: $($lines11a -join ' | ')"
                Close-SotLeases
                $wait11a = [System.Diagnostics.Stopwatch]::StartNew()
                do {
                    $lines11a = @(Get-Content -LiteralPath $log11a -ErrorAction SilentlyContinue)
                    if ($lines11a.Count -ge 4) { break }
                    Start-Sleep -Milliseconds 50
                } while ($wait11a.ElapsedMilliseconds -lt 10000)
                Check '11a: at least 4 log lines arrive within 10 s' ($lines11a.Count -ge 4) "lines: $($lines11a -join ' | ')"
                $handover = '{"v":3,"id":2,"kind":"req","op":"fe.leaving","payload":{"intent":"handover"}}'
                Check '11a: the handover line follows' (($lines11a.Count -ge 3) -and ($lines11a[2] -ceq $handover)) "lines: $($lines11a -join ' | ')"
                Check '11a: then eof' (($lines11a.Count -ge 4) -and ($lines11a[3] -ceq 'eof')) "lines: $($lines11a -join ' | ')"
                Check '11a: the lease list is empty after Close-SotLeases' ($global:SotLeases.Count -eq 0) "count $($global:SotLeases.Count)"
            } finally {
                if ($fake11a -and -not $fake11a.HasExited) { Stop-Process -Id $fake11a.Id -Force -ErrorAction SilentlyContinue }
                Clear-FakeEnv
            }

            # (b) refused
            $pipe11b = New-TestPipeName
            $log11b = Join-Path $root 'lease11b.log'
            Clear-FakeEnv
            $env:FAKE_SOTD_LEASE_OUTCOME = 'foreign'
            $env:FAKE_SOTD_LOG = $log11b
            $fake11b = Start-Process -FilePath $fakeExe -ArgumentList @('--socket', (Get-PipePath $pipe11b)) -WindowStyle Hidden -PassThru
            try {
                Check '11b: the fake pipe is up' (Wait-Pipe $pipe11b) 'pipe never answered'
                $script:supLines = @()
                $streams11b = @(Open-SotLease (Get-PipePath $pipe11b) $realSotd)
                Check '11b: no stream comes back' ($streams11b.Count -eq 0) "got $($streams11b.Count)"
                Check '11b: the warning names the 60 s bound' ((@($script:supLines | Where-Object { $_ -like '*60 s*' })).Count -ge 1) "log: $($script:supLines -join ' | ')"
            } finally {
                if ($fake11b -and -not $fake11b.HasExited) { Stop-Process -Id $fake11b.Id -Force -ErrorAction SilentlyContinue }
                Clear-FakeEnv
            }

            # (d) a refused hello is named in the warning, and no stream comes back
            $pipe11d = New-TestPipeName
            Clear-FakeEnv
            $env:FAKE_SOTD_HELLO_REFUSAL = 'os_user_conflict'
            $fake11d = Start-Process -FilePath $fakeExe -ArgumentList @('--socket', (Get-PipePath $pipe11d)) -WindowStyle Hidden -PassThru
            try {
                Check '11d: the fake pipe is up' (Wait-Pipe $pipe11d) 'pipe never answered'
                $script:supLines = @()
                $streams11d = @(Open-SotLease (Get-PipePath $pipe11d) $realSotd)
                Check '11d: no stream comes back' ($streams11d.Count -eq 0) "got $($streams11d.Count)"
                Check '11d: the warning names the refused hello and its code' ((@($script:supLines | Where-Object { $_ -like '*hello refused: os_user_conflict*' })).Count -ge 1) "log: $($script:supLines -join ' | ')"
            } finally {
                if ($fake11d -and -not $fake11d.HasExited) { Stop-Process -Id $fake11d.Id -Force -ErrorAction SilentlyContinue }
                Clear-FakeEnv
            }

            # (e) a lease is never opened on a pipe another account serves (ADR 0049)
            $script:supLines = @()
            $streams11e = @(Open-SotLease '\\.\pipe\epmapper' $realSotd)
            Check '11e: no stream comes back' ($streams11e.Count -eq 0) "got $($streams11e.Count)"
            Check '11e: the warning names not connecting' ((@($script:supLines | Where-Object { $_ -like '*not connecting*' })).Count -ge 1) "log: $($script:supLines -join ' | ')"

            # (f) a bridge that ends before the lease is written still says why (ADR 0049)
            Clear-FakeEnv
            $go11f = Join-Path $root 'bridge-go-11f'
            $env:FAKE_SOTD_BRIDGE_EARLY_EXIT = $go11f
            $script:child11f = $null
            $input11f = [Console]::InputEncoding
            $realBoot11f = ${function:Get-SotBootId}
            function Get-SotBootId {
                $bridge11f = Get-Variable -Name bridge -Scope 1 -ValueOnly -ErrorAction SilentlyContinue
                if ($bridge11f) {
                    # A process object of the test's own for the cleanup below: Open-SotLease disposes its own when the
                    # lease is not granted. Its handle is taken now, while the child still runs.
                    $script:child11f = [System.Diagnostics.Process]::GetProcessById($bridge11f.Id)
                    $null = $script:child11f.Handle
                    # Only now may the fake read its input: the test holds its own handle, whatever the input holds.
                    Set-Content -LiteralPath $go11f -Value 'go' -Encoding ASCII
                    $bridge11f.StandardInput.Close()
                    if (-not $bridge11f.WaitForExit(10000)) { throw 'the fake bridge did not exit within 10 s' }
                    if ($bridge11f.ExitCode -ne 1) { throw "the fake bridge exited $($bridge11f.ExitCode), expected 1" }
                }
                & $realBoot11f
            }
            try {
                # 11f is about the warning, not the bytes a bridge reads (7c is): its fixture fails on any input byte,
                # so while it runs the console's input encoding has no preamble, whatever Start-SotBridge does.
                $script:supLines = @()
                $streams11f = @(Open-SotLease (Get-PipePath (New-TestPipeName)) $fakeExe)
                Check '11f: no stream comes back' ($streams11f.Count -eq 0) "got $($streams11f.Count)"
                Check '11f: the warning names the bridge''s own line' ((@($script:supLines | Where-Object { $_ -like '*not connecting: test refusal*' })).Count -ge 1) "log: $($script:supLines -join ' | ')"
            } finally {
                [Console]::InputEncoding = $input11f
                ${function:Get-SotBootId} = $realBoot11f
                Clear-FakeEnv
                if ($script:child11f) {
                    if (-not $script:child11f.HasExited) {
                        $script:child11f.Kill()
                        if (-not $script:child11f.WaitForExit(10000)) { throw 'the fake bridge did not stop within 10 s' }
                    }
                    $script:child11f.Dispose()
                }
                $script:child11f = $null
            }

            # (c) the boot identity is stable and numeric
            $b1 = Get-SotBootId; $b2 = Get-SotBootId
            Check '11c: Get-SotBootId is stable' ($b1 -ceq $b2) "got '$b1' then '$b2'"
            Check '11c: Get-SotBootId is a decimal number' ($b1 -match '^\d+$') "got '$b1'"
        }
        } catch { Check '11: section ran' $false $_.Exception.Message }
    }
try {
    Write-Host "`n=== 16. ConvergeRunsCodeOnDisk: a converge re-invokes a changed launcher in this process, which hands over its caller's leases ===" -ForegroundColor Cyan
    $loop16 = @($launchAst.FindAll({ param($n)
        ($n -is [System.Management.Automation.Language.DoWhileStatementAst]) -and $n.Condition.Extent.Text -eq '$relaunchNext' }, $true))
    $conv16 = @()
    if ($loop16.Count -eq 1) {
        $conv16 = @(Find-Ifs $loop16[0] '$convergeRequested' | Where-Object { @(Find-Calls $_ 'Invoke-PendingApply').Count -gt 0 })
    }
    Check '16a: one converge block' ($conv16.Count -eq 1) "found $($conv16.Count)"
    if ($conv16.Count -eq 1) {
        $c16 = $conv16[0]
        $amp16 = @($c16.FindAll({ param($n)
            ($n -is [System.Management.Automation.Language.CommandAst]) -and
            $n.InvocationOperator -eq [System.Management.Automation.Language.TokenKind]::Ampersand -and
            $n.CommandElements[0].Extent.Text -eq '$PSCommandPath' }, $true))
        $id16 = @(Find-Calls $c16 'Get-SotLauncherCodeId')
        $pre16 = @(Find-Calls $c16 'Invoke-SelfUpdatePrelude')
        $fr16 = @(Find-Calls $c16 'Invoke-FreshnessPass')
        Check '16a: the converge re-invokes $PSCommandPath once' ($amp16.Count -eq 1) "found $($amp16.Count)"
        Check '16a: the converge reads the code id on disk once' ($id16.Count -eq 1) "found $($id16.Count)"
        if ($amp16.Count -eq 1 -and $id16.Count -eq 1 -and $pre16.Count -eq 1 -and $fr16.Count -eq 1) {
            Check '16b: the code id is read after the prelude' ($pre16[0].Extent.StartOffset -lt $id16[0].Extent.StartOffset) 'order is wrong'
            Check '16b: the re-invoke precedes the freshness pass' ($amp16[0].Extent.StartOffset -lt $fr16[0].Extent.StartOffset) 'order is wrong'
            $guard16 = $false
            $p16 = $amp16[0].Parent
            while ($p16 -and -not [object]::ReferenceEquals($p16, $c16)) {
                if (($p16 -is [System.Management.Automation.Language.IfStatementAst]) -and ($p16.Clauses[0].Item1.Extent.Text -match '\$script:launcherCodeId')) { $guard16 = $true }
                $p16 = $p16.Parent
            }
            Check '16b: the re-invoke is guarded by the code id this process parsed' $guard16 'no enclosing if compares with $script:launcherCodeId'
            $exit16 = @($amp16[0].Parent.Parent.FindAll({ param($n) $n -is [System.Management.Automation.Language.ExitStatementAst] }, $false) |
                Where-Object { $_.Extent.StartOffset -gt $amp16[0].Extent.StartOffset })
            Check '16b: an exit follows the re-invoke' ($exit16.Count -ge 1) 'no exit after the re-invoke'
        }
        $io16 = @($c16.FindAll({ param($n)
            ($n -is [System.Management.Automation.Language.InvokeMemberExpressionAst]) -and
            (@('ParseFile', 'ReadAllText') -contains $n.Member.Extent.Text) }, $true))
        $tried16 = @($io16 | Where-Object {
            $q = $_.Parent; $t = $false
            while ($q -and -not [object]::ReferenceEquals($q, $c16)) {
                if (($q -is [System.Management.Automation.Language.TryStatementAst]) -and $q.CatchClauses.Count -ge 1) { $t = $true }
                $q = $q.Parent
            }
            $t })
        Check '16f: the refusal checks run inside a try that catches' (($io16.Count -ge 2) -and ($tried16.Count -eq $io16.Count)) "file reads $($io16.Count), inside a try $($tried16.Count)"
    }
    if ($loop16.Count -eq 1) {
        $first16 = $loop16[0].Body.Statements[0]
        $ls16 = @(Find-Calls $first16 'Open-SotLease')
        $gated16 = @($ls16 | Where-Object {
            $q = $_.Parent; $g = $false
            while ($q -and -not [object]::ReferenceEquals($q, $first16)) {
                if (($q -is [System.Management.Automation.Language.IfStatementAst]) -and ($q.Clauses[0].Item1.Extent.Text -match 'convergeRequested')) { $g = $true }
                $q = $q.Parent
            }
            $g })
        Check '16c: the respawn block leases whatever the exit code' (($ls16.Count -eq 1) -and ($gated16.Count -eq 0)) "leases $($ls16.Count), gated on convergeRequested $($gated16.Count)"
    }
    $old16 = @($launchAst.FindAll({ param($n) ($n -is [System.Management.Automation.Language.VariableExpressionAst]) -and $n.Extent.Text -eq '$script:convergeLeases' }, $true))
    Check '16d: no script-scoped lease list remains' ($old16.Count -eq 0) "found $($old16.Count) uses of `$script:convergeLeases"
    $sets16 = @(foreach ($ast16 in @($launchAst, $leaseAst)) { $ast16.FindAll({ param($n)
        ($n -is [System.Management.Automation.Language.AssignmentStatementAst]) -and
        $n.Operator -eq [System.Management.Automation.Language.TokenKind]::Equals -and
        $n.Left.Extent.Text -eq '$global:SotLeases' }, $true) })
    $bad16 = @($sets16 | Where-Object {
        $q = $_.Parent; $ok = $false
        while ($q) {
            if (($q -is [System.Management.Automation.Language.FunctionDefinitionAst]) -and $q.Name -eq 'Close-SotLeases') { $ok = $true }
            if (($q -is [System.Management.Automation.Language.IfStatementAst]) -and ($q.Clauses[0].Item1.Extent.Text -match '\$null -eq \$global:SotLeases')) { $ok = $true }
            $q = $q.Parent
        }
        -not $ok })
    Check '16d: $global:SotLeases is set only when unset, or by Close-SotLeases' (($sets16.Count -ge 2) -and ($bad16.Count -eq 0)) "assignments $($sets16.Count), unguarded $($bad16.Count): $(@($bad16 | ForEach-Object { $_.Extent.Text }) -join ' | ')"
    if ($compiled) {
        foreach ($fname in @('Get-SotBootId', 'Open-SotLease', 'Start-SotBridge')) {
            $fn = $leaseAst.Find({ param($n) ($n -is [System.Management.Automation.Language.FunctionDefinitionAst]) -and $n.Name -eq $fname }, $true)
            if ($fn) { . ([scriptblock]::Create($fn.Extent.Text)) }
        }
        $closeFn16 = $leaseAst.Find({ param($n) ($n -is [System.Management.Automation.Language.FunctionDefinitionAst]) -and $n.Name -eq 'Close-SotLeases' }, $true)
        $LeaseReplyWaitMs = 5000
        $HandoverBoundSeconds = 60
        function Write-SupLog { param([string]$Message) }
        $pipe16 = New-TestPipeName
        $log16 = Join-Path $root 'lease16.log'
        Clear-FakeEnv
        $env:FAKE_SOTD_LOG = $log16
        $fake16 = Start-Process -FilePath $fakeExe -ArgumentList @('--socket', (Get-PipePath $pipe16)) -WindowStyle Hidden -PassThru
        try {
            Check '16e: the fake pipe is up' (Wait-Pipe $pipe16) 'pipe never answered'
            $global:SotLeases = @()
            $global:SotLeases += @(Open-SotLease (Get-PipePath $pipe16) $realSotd)
            Check '16e: the caller holds one lease' ($global:SotLeases.Count -eq 1) "count $($global:SotLeases.Count)"
            # The re-invoked launcher, reduced to what this test is about: its
            # own Close-SotLeases, run from another script in this process.
            $inner16 = Join-Path $root 'inner16.ps1'
            Set-Content -LiteralPath $inner16 -Encoding ascii -Value ("function Write-SupLog { param([string]`$Message) }`r`n" + $closeFn16.Extent.Text + "`r`nClose-SotLeases`r`n")
            & $inner16
            Start-Sleep -Milliseconds 500
            $lines16 = @(Get-Content -LiteralPath $log16 -ErrorAction SilentlyContinue)
            $handover16 = '{"v":3,"id":2,"kind":"req","op":"fe.leaving","payload":{"intent":"handover"}}'
            Check '16e: the re-invoked copy hands over the caller''s lease' (($lines16.Count -ge 3) -and ($lines16[2] -ceq $handover16)) "lines: $($lines16 -join ' | ')"
            Check '16e: and then closes it (eof)' (($lines16.Count -ge 4) -and ($lines16[3] -ceq 'eof')) "lines: $($lines16 -join ' | ')"
            Check '16e: the caller''s list is empty afterwards' ($global:SotLeases.Count -eq 0) "count $($global:SotLeases.Count)"
        } finally {
            foreach ($s16 in @($global:SotLeases)) { try { $s16.Dispose() } catch { } }
            $global:SotLeases = $null
            if ($fake16 -and -not $fake16.HasExited) { Stop-Process -Id $fake16.Id -Force -ErrorAction SilentlyContinue }
            Clear-FakeEnv
        }
    }
} catch { Check '16: section ran' $false $_.Exception.Message }
} finally { Complete-LocalDaemonTest }

Write-Host "`n================ $pass passed, $fail failed ================" -ForegroundColor $(if ($fail) { 'Red' } else { 'Green' })
if ($fail) { exit 1 }

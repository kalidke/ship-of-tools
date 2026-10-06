# test-local-daemon-binary.ps1 -- sections 2c and 7c, the local daemon's one binary (ADR 0049, User isolation): the
# resolver the daemon, -Stop and the launcher's query and lease share (2c), and the bytes the lease's and the probe's
# bridges read, which are only their callers' (7c). Dot-sourced by test-local-daemon.ps1 right after
# test-local-daemon-fake.ps1, in its scope: $script, $repo, $root, $compiled, $fakeExe, the support helpers and the
# fake's. 7c needs the fake daemon and runs only when it compiled. ASCII only; Windows PowerShell 5.1.

    try {
    Write-Host "`n=== 2c. OneResolver: a lone dev sotd.exe never outranks a complete install pair ===" -ForegroundColor Cyan
    # A lone dev sotd.exe (an older build, no sot-capsule.exe beside it) and a complete install pair: the daemon runs
    # the install's, so launch-sot.ps1's pipe-name query and lease run it too, and so does -Stop. Placeholders only:
    # -Resolve answers before any query, and -Stop's could-not-execute line names the binary it chose.
    $p2c = Join-Path $root 'p2c'
    New-Fixture -Prefix $p2c -WithCapsule
    $installExe2c = Join-Path $p2c 'bin\sotd.exe'
    $dev2c = Join-Path $root 'dev2c'
    New-Item -ItemType Directory -Force -Path $dev2c | Out-Null
    $devExe2c = Join-Path $dev2c 'sotd.exe'
    Set-Content -LiteralPath $devExe2c -Value 'FAKE-LONE-DEV-SOTD-NEVER-EXECUTED' -NoNewline

    # The launcher's own function, read from launch-sot.ps1 and run with the launcher's variables pointed here.
    $launchAst2c = [System.Management.Automation.Language.Parser]::ParseFile((Join-Path $repo 'scripts\launch-sot.ps1'), [ref]$null, [ref]$null)
    $fn2c = $launchAst2c.Find({ param($n) ($n -is [System.Management.Automation.Language.FunctionDefinitionAst]) -and $n.Name -eq 'Get-SotLocalSotdExe' }, $true)
    Check '2c: launch-sot.ps1 defines Get-SotLocalSotdExe' ($null -ne $fn2c) 'function not found'
    $sotLocalDaemon = $script
    $backendExe = $devExe2c
    $prefixDir = $p2c
    if ($fn2c) {
        . ([scriptblock]::Create($fn2c.Extent.Text))
        $got2c = Get-SotLocalSotdExe
        Check '2c: the launcher queries and leases through the install pair' ("$got2c" -eq $installExe2c) "got '$got2c'"
    }
    $stop2c = & $script -Stop -Prefix $p2c -DevBinDir $dev2c -ProjectRoot $root 6>&1 2>&1
    Check '2c: -Stop chose the install pair' ((($stop2c -join ' ')) -match [regex]::Escape("could not execute $installExe2c")) "log was: $stop2c"
    $res2c = @(& $script -Resolve -Prefix $p2c -DevBinDir $dev2c)
    Check '2c: -Resolve names the install pair' (($LASTEXITCODE -eq 0) -and ($res2c.Count -eq 1) -and ("$($res2c[0])" -eq $installExe2c)) "exit $LASTEXITCODE, got: $res2c"

    # Control: a complete dev pair wins, for -Resolve and the launcher alike.
    Set-Content -LiteralPath (Join-Path $dev2c 'sot-capsule.exe') -Value 'FAKE-CAPSULE-NEVER-EXECUTED' -NoNewline
    $res2cDev = @(& $script -Resolve -Prefix $p2c -DevBinDir $dev2c)
    Check '2c: a complete dev pair wins' (($LASTEXITCODE -eq 0) -and ($res2cDev.Count -eq 1) -and ("$($res2cDev[0])" -eq $devExe2c)) "exit $LASTEXITCODE, got: $res2cDev"
    if ($fn2c) { $got2c = Get-SotLocalSotdExe; Check '2c: the launcher follows the complete dev pair' ("$got2c" -eq $devExe2c) "got '$got2c'" }

    # Control: with no complete pair there is no binary to start, never a lone one.
    Remove-Item -LiteralPath (Join-Path $dev2c 'sot-capsule.exe'), (Join-Path $p2c 'bin\sot-capsule.exe') -Force
    $res2cNone = @(& $script -Resolve -Prefix $p2c -DevBinDir $dev2c)
    Check '2c: no complete pair resolves nothing' (($LASTEXITCODE -eq 1) -and ($res2cNone.Count -eq 0)) "exit $LASTEXITCODE, got: $res2cNone"
    if ($fn2c) { $got2c = Get-SotLocalSotdExe; Check '2c: the launcher then has no binary' ($null -eq $got2c) "got '$got2c'" }
    } catch { Check '2c: section ran' $false $_.Exception.Message }

    if ($compiled) {
        try {
        Write-Host "`n=== 7c. BridgeReadsOnlyItsCaller: the lease's and the probe's bridges read only their callers' bytes, in a hidden console like the launcher's ===" -ForegroundColor Cyan
        # Windows PowerShell 5.1 opens a redirected input as a writer in [Console]::InputEncoding and flushes it at
        # once, so an encoding with a preamble (the UTF-8 BOM) reaches the bridge before the caller's first byte. The
        # child is a hidden powershell.exe with a console of its own, as the launcher is. It gives that console an
        # input encoding with a BOM, then runs the production lease (Open-SotLease) and probe (Test-SotPipeOpen,
        # read from sot-local-daemon.ps1) against the fake bridge, which records in hex every byte it reads. The lease's
        # bridge must read the hello first; the probe's must read nothing. This step's own console encoding is printed
        # first: whether it has a preamble decides whether this step's lease and probe (5b, 4b, 5b2) see a BOM.
        try {
            $enc7c = [Console]::InputEncoding
            Write-Host ("  this step's console input encoding: code page {0}, preamble [{1}]" -f $enc7c.CodePage, [BitConverter]::ToString($enc7c.GetPreamble()))
        } catch { Write-Host "  this step's console input encoding: unreadable ($($_.Exception.Message))" }
        Clear-FakeEnv
        $lease7c = Join-Path $root 'fake-stdin-7c-lease.log'
        $probe7c = Join-Path $root 'fake-stdin-7c-probe.log'
        $out7c = Join-Path $root 'bridge-7c.out'
        $child7c = Join-Path $root 'bridge-7c.ps1'
        Remove-Item -LiteralPath $lease7c, $probe7c, $out7c -Force -ErrorAction SilentlyContinue
        Set-Content -LiteralPath $child7c -Encoding ascii -Value @'
param([string]$Scripts, [string]$Exe, [string]$LeaseLog, [string]$ProbeLog, [string]$Out)
try {
    . (Join-Path $Scripts 'sot-lease.ps1')
    $ast = [System.Management.Automation.Language.Parser]::ParseFile((Join-Path $Scripts 'sot-local-daemon.ps1'), [ref]$null, [ref]$null)
    $fn = $ast.Find({ param($n) ($n -is [System.Management.Automation.Language.FunctionDefinitionAst]) -and $n.Name -eq 'Test-SotPipeOpen' }, $true)
    . ([scriptblock]::Create($fn.Extent.Text))
    $LeaseReplyWaitMs = 2000
    $HandoverBoundSeconds = 60
    function Write-SupLog { param([string]$Message) }
    [Console]::InputEncoding = [System.Text.Encoding]::UTF8
    $env:FAKE_SOTD_STDIN_LOG = $LeaseLog
    $leases = @(Open-SotLease '\\.\pipe\sot-test-7c' $Exe)
    $env:FAKE_SOTD_STDIN_LOG = $ProbeLog
    $daemonExe = $Exe
    $open = Test-SotPipeOpen 'sot-test-7c'
    $kept = [Console]::InputEncoding.GetPreamble().Length -eq 3
    Set-Content -LiteralPath $Out -Value ('leases={0} probe={1} restored={2}' -f $leases.Count, $open, $kept)
} catch {
    Set-Content -LiteralPath $Out -Value ('error=' + $_.Exception.Message)
}
'@
        try {
            $args7c = '-NoProfile -ExecutionPolicy Bypass -File "{0}" -Scripts "{1}" -Exe "{2}" -LeaseLog "{3}" -ProbeLog "{4}" -Out "{5}"' -f $child7c, (Join-Path $repo 'scripts'), $fakeExe, $lease7c, $probe7c, $out7c
            $proc7c = Start-Process -FilePath 'powershell.exe' -ArgumentList $args7c -WindowStyle Hidden -PassThru
            if (-not $proc7c.WaitForExit(30000)) { try { $proc7c.Kill() } catch { }; throw 'the hidden child did not finish within 30 s' }
            $report7c = if (Test-Path -LiteralPath $out7c) { "$(Get-Content -LiteralPath $out7c -Raw)".Trim() } else { '<no report>' }
            $gotLease7c = if (Test-Path -LiteralPath $lease7c) { "$(Get-Content -LiteralPath $lease7c -Raw)".Trim() } else { '<nothing recorded>' }
            $gotProbe7c = if (Test-Path -LiteralPath $probe7c) { "$(Get-Content -LiteralPath $probe7c -Raw)".Trim() } else { '<nothing recorded>' }
            $hello7c = [BitConverter]::ToString([System.Text.Encoding]::ASCII.GetBytes('{"v":3,"id":0,"kind":"req","op":"hello"'))
            $head7c = if ($gotLease7c.Length -gt 48) { $gotLease7c.Substring(0, 48) + '...' } else { $gotLease7c }
            Write-Host ("  7c reads: the lease's bridge [{0}], the probe's bridge [{1}]" -f $head7c, $gotProbe7c)
            Check '7c: the child ran the lease and the probe, and its console input encoding was put back' ($report7c -ceq 'leases=0 probe=True restored=True') "child: $report7c"
            Check '7c: the lease''s bridge reads its caller''s hello first' ($gotLease7c.StartsWith($hello7c)) "read [$head7c]"
            Check '7c: the probe''s bridge reads no byte' ($gotProbe7c -ceq '') "read [$gotProbe7c]"
        } finally {
            Clear-FakeEnv
        }
        } catch { Check '7c: section ran' $false $_.Exception.Message }
    }

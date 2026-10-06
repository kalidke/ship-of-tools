# test-local-daemon-binary.ps1 -- sections 2c and 7b, the local daemon's one binary (ADR 0049, User isolation): the
# resolver the daemon, -Stop and the launcher's query and lease share (2c), and the SOTD_BIN the daemon hands the
# sessions it spawns (7b). Dot-sourced by test-local-daemon.ps1 right after test-local-daemon-fake.ps1, in its scope:
# $script, $repo, $root, $compiled, the support helpers and the fake's. 7b needs the fake daemon and runs only when it
# compiled. ASCII only; Windows PowerShell 5.1.

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
        Write-Host "`n=== 7b. DaemonCarriesItsBinary: the daemon's sessions get SOTD_BIN = the sotd.exe it runs, and the caller's own is restored ===" -ForegroundColor Cyan
        # The comm shell in a session the daemon spawns bridges with SOTD_BIN when it names a file (comm-lib-client.sh
        # _sot_windows_sotd_exe). A sentinel in this process's environment proves both halves: the daemon does not
        # inherit it, and this process has it back after the call.
        Clear-FakeEnv
        $envLog7b = Join-Path $root 'fake-env-7b.log'
        Remove-Item -LiteralPath $envLog7b -Force -ErrorAction SilentlyContinue
        $env:FAKE_SOTD_ENV_LOG = $envLog7b
        $sentinel7b = 'C:\sot-test-sentinel\sotd.exe'
        $savedSotdBin7b = [Environment]::GetEnvironmentVariable('SOTD_BIN')
        $env:SOTD_BIN = $sentinel7b
        $p7b = New-FakePrefix 'p7b'
        $pipe7b = New-TestPipeName
        try {
            $out7b = & $script -Prefix $p7b -DevBinDir 'C:\sot-test-does-not-exist' -PipeName $pipe7b -ProjectRoot $root 6>&1 2>&1
            Check '7b: exit code 0' ($LASTEXITCODE -eq 0) "got $LASTEXITCODE; log: $out7b"
            $seen7b = if (Test-Path -LiteralPath $envLog7b) { (Get-Content -LiteralPath $envLog7b -Raw).Trim() } else { '<no env log>' }
            Check '7b: the daemon runs with SOTD_BIN = its own sotd.exe' ($seen7b -eq ('SOTD_BIN=' + (Join-Path $p7b 'bin\sotd.exe').Replace('\', '/'))) "got $seen7b"
            Check '7b: the caller keeps its own SOTD_BIN' ($env:SOTD_BIN -eq $sentinel7b) "got '$env:SOTD_BIN'"
        } finally {
            Stop-FakeOn $pipe7b
            Clear-FakeEnv
            [Environment]::SetEnvironmentVariable('SOTD_BIN', $savedSotdBin7b)
        }
        } catch { Check '7b: section ran' $false $_.Exception.Message }
    }

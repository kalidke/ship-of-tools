# test-sot-within.ps1 -- scripts/sot-hosts.ps1's Invoke-SotWithin: a native command under a wall-clock bound, and
# the one script ssh call it bounds that a test can reach, shutdown-sot.ps1's Get-DaemonDisconnect (read as a syntax
# tree, run against a stand-in ssh.exe). A stalled command ends at its bound with Code 124 and a line naming it, and
# is killed; a command that finishes returns its output and exit code; stdin reaches the command.
# Run under WINDOWS POWERSHELL 5.1: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\tests\test-sot-within.ps1

$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$pass = 0; $fail = 0
function Check([string]$name, [bool]$ok, [string]$detail) {
    if ($ok) { $script:pass++; Write-Host "  PASS  $name" -ForegroundColor Green }
    else { $script:fail++; Write-Host "  FAIL  $name -- $detail" -ForegroundColor Red }
}

. (Join-Path $repoRoot 'scripts\sot-hosts.ps1')

try {
    Write-Host "`n=== a stalled command ends at its bound ===" -ForegroundColor Cyan
    $marker = Join-Path $env:TEMP ("sot-within-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    $r = Invoke-SotWithin 2 'powershell' @('-NoProfile', '-Command', "Set-Content -LiteralPath '$marker' -Value `$PID; Start-Sleep 600")
    $clock.Stop()
    Check 'the code is 124' ($r.Code -eq 124) "Code=$($r.Code)"
    Check 'the output names the command and the bound' ($r.Output -ceq 'powershell did not finish within 2 s') "Output='$($r.Output)'"
    Check 'it ends within 5 s' ($clock.Elapsed.TotalSeconds -lt 5) "took $($clock.Elapsed.TotalSeconds) s"
    $childPid = 0
    if (Test-Path -LiteralPath $marker) { $childPid = [int](Get-Content -LiteralPath $marker | Select-Object -First 1) }
    Check 'the child recorded its pid' ($childPid -gt 0) 'the child never started'
    Check 'the child is gone' ($childPid -gt 0 -and -not (Get-Process -Id $childPid -ErrorAction SilentlyContinue)) "pid $childPid still runs"
    Remove-Item -LiteralPath $marker -Force -ErrorAction SilentlyContinue

    Write-Host "`n=== a command that finishes returns its output ===" -ForegroundColor Cyan
    $r = Invoke-SotWithin 30 'powershell' @('-NoProfile', '-Command', 'Write-Output hello; exit 3')
    Check 'its exit code' ($r.Code -eq 3) "Code=$($r.Code)"
    Check 'its output' ($r.Output.Trim() -ceq 'hello' -and $r.Stdout.Trim() -ceq 'hello') "Output='$($r.Output)'"
    $r = Invoke-SotWithin 30 'powershell' @('-NoProfile', '-Command', '$x = [Console]::In.ReadToEnd(); Write-Output "got:$($x.Trim())"') "line one`nline two`n"
    Check 'stdin reaches the command' ($r.Output -match 'got:line one\s+line two') "Output='$($r.Output)'"
    $threw = $false
    try { Invoke-SotWithin 5 'powershell' @('-NoProfile', '-Command', 'exit 0') ('x' * 4001) | Out-Null } catch { $threw = $true }
    Check 'a stdin over 4000 bytes is refused' $threw 'no error'
    Write-Host "`n=== shutdown-sot.ps1's remote read runs under the bound with the tree's ssh options ===" -ForegroundColor Cyan
    # A stand-in ssh.exe first on PATH (Process.Start finds only .exe by name): it records its argv and stdin, answers
    # one line, or sleeps when SOT_TEST_SSH_HANG is 1.
    $stubDir = Join-Path $env:TEMP ("sot-within-ssh-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
    New-Item -ItemType Directory -Force -Path $stubDir | Out-Null
    $sshLog = Join-Path $stubDir 'ssh.log'
    $savedPath = $env:PATH
    try {
        Add-Type -TypeDefinition @'
public class SshStub {
    public static int Main(string[] a) {
        if (System.Environment.GetEnvironmentVariable("SOT_TEST_SSH_HANG") == "1") System.Threading.Thread.Sleep(600000);
        string stdin = System.Console.In.ReadToEnd();
        System.IO.File.WriteAllText(System.Environment.GetEnvironmentVariable("SOT_TEST_SSH_LOG"), string.Join(" ", a) + "\n--stdin--\n" + stdin);
        System.Console.WriteLine("frontend disconnected stand-in");
        return 0;
    }
}
'@ -OutputAssembly (Join-Path $stubDir 'ssh.exe') -OutputType ConsoleApplication
        $env:PATH = "$stubDir;$env:PATH"
        $env:SOT_TEST_SSH_LOG = $sshLog
        $shutAst = [System.Management.Automation.Language.Parser]::ParseFile((Join-Path $repoRoot 'scripts\shutdown-sot.ps1'), [ref]$null, [ref]$null)
        $fn = @($shutAst.FindAll({ param($n) ($n -is [System.Management.Automation.Language.FunctionDefinitionAst]) -and $n.Name -eq 'Get-DaemonDisconnect' }, $true))
        Check 'shutdown-sot.ps1 defines Get-DaemonDisconnect' ($fn.Count -eq 1) "found $($fn.Count)"
        Invoke-Expression $fn[0].Extent.Text
        $line = Get-DaemonDisconnect 'be-host' 400
        Check 'the line comes back' ($line -ceq 'frontend disconnected stand-in') "got '$line'"
        $logged = Get-Content -LiteralPath $sshLog -Raw
        Check 'ssh ran with ServerAliveInterval' ($logged -match 'ServerAliveInterval=15') "argv: $logged"
        Check 'and ServerAliveCountMax and ConnectTimeout' ($logged -match 'ServerAliveCountMax=3' -and $logged -match 'ConnectTimeout=10') "argv: $logged"
        Check 'the remote command goes on stdin to bash -s' ($logged -match 'be-host bash -s' -and $logged -match '--stdin--\s+journalctl --user -u sotd\.service --no-pager -n 400 ') "argv and stdin: $logged"
        $env:SOT_TEST_SSH_HANG = '1'
        $clock = [System.Diagnostics.Stopwatch]::StartNew()
        $said = ''
        try { Get-DaemonDisconnect 'be-host' 60 2 | Out-Null } catch { $said = $_.Exception.Message }
        $clock.Stop()
        Check 'a hung ssh throws the helper''s words' ($said -ceq 'ssh did not finish within 2 s') "got '$said'"
        Check 'within 5 s' ($clock.Elapsed.TotalSeconds -lt 5) "took $($clock.Elapsed.TotalSeconds) s"
    } catch { Check 'the ssh section ran' $false $_.Exception.Message }
    finally {
        $env:PATH = $savedPath
        Remove-Item Env:SOT_TEST_SSH_HANG -ErrorAction SilentlyContinue
        Remove-Item Env:SOT_TEST_SSH_LOG -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $stubDir -Recurse -Force -ErrorAction SilentlyContinue
    }
} catch { Check 'the suite ran' $false $_.Exception.Message }

Write-Host "`n================ $pass passed, $fail failed ================" -ForegroundColor $(if ($fail) { 'Red' } else { 'Green' })
if ($fail) { exit 1 }
exit 0

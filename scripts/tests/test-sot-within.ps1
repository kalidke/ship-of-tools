# test-sot-within.ps1 -- the PowerShell launch scripts' network bounds. scripts/sot-hosts.ps1's Invoke-SotWithin, a
# native command under a wall-clock bound: a stalled command ends at its bound with Code 124 and a line naming it, and
# is killed; a command that finishes returns its output and exit code; stdin reaches the command. Then each script call
# a test can reach, its stand-in first on a child PowerShell's PATH, never this process's: shutdown-sot.ps1's
# Get-DaemonDisconnect (read as a syntax tree) against a stand-in ssh.exe, and sot-install-layout.ps1's tag fetch
# against a stand-in git.exe.
# Run under WINDOWS POWERSHELL 5.1: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\tests\test-sot-within.ps1

$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$scripts = Join-Path $repoRoot 'scripts'
$work = Join-Path $env:TEMP ("sot-within-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Force -Path $work | Out-Null
$pass = 0; $fail = 0
function Check([string]$name, [bool]$ok, [string]$detail) {
    if ($ok) { $script:pass++; Write-Host "  PASS  $name" -ForegroundColor Green }
    else { $script:fail++; Write-Host "  FAIL  $name -- $detail" -ForegroundColor Red }
}

# Runs the script $Text in a child Windows PowerShell whose environment is this process's with $Env laid over it, so a
# stand-in goes first on the CHILD's PATH and this process's PATH never changes; returns @{ Code; Out; Err }. A child
# still running after $Seconds is killed, and the call throws a line naming $Name and the bound.
function Invoke-Child([string]$Name, [string]$Text, [hashtable]$Env = @{}, [int]$Seconds = 60) {
    $file = Join-Path $work "$Name.ps1"
    Set-Content -LiteralPath $file -Value $Text -Encoding ascii
    $psi = New-Object System.Diagnostics.ProcessStartInfo('powershell.exe', ('-NoProfile -ExecutionPolicy Bypass -File "{0}"' -f $file))
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    foreach ($k in $Env.Keys) { $psi.EnvironmentVariables[$k] = [string]$Env[$k] }
    $p = [System.Diagnostics.Process]::Start($psi)
    $out = $p.StandardOutput.ReadToEndAsync()
    $err = $p.StandardError.ReadToEndAsync()
    if (-not $p.WaitForExit($Seconds * 1000)) {
        try { $p.Kill() } catch { }
        throw "${Name}: the child had not ended after $Seconds s"
    }
    return @{ Code = $p.ExitCode; Out = $out.Result; Err = $err.Result }
}

# A stand-in git.exe: it appends its argv to $env:SOT_TEST_GIT_LOG; `pull` fails with git's stall error, as a transfer
# under http.lowSpeedLimit for http.lowSpeedTime does; `rev-parse` prints one commit id; anything else prints nothing.
$gitDir = Join-Path $work 'stub-git'
New-Item -ItemType Directory -Force -Path $gitDir | Out-Null
Add-Type -TypeDefinition @'
public class GitStub {
    public static int Main(string[] a) {
        System.IO.File.AppendAllText(System.Environment.GetEnvironmentVariable("SOT_TEST_GIT_LOG"), "git " + string.Join(" ", a) + "\n");
        if (System.Array.IndexOf(a, "pull") >= 0) {
            System.Console.Error.WriteLine("error: RPC failed; curl 28 Operation too slow. Less than 1000 bytes/sec transferred the last 60 seconds");
            System.Console.Error.WriteLine("fatal: expected flush after ref listing");
            return 1;
        }
        if (System.Array.IndexOf(a, "rev-parse") >= 0) System.Console.WriteLine("0123456789abcdef0123456789abcdef01234567");
        return 0;
    }
}
'@ -OutputAssembly (Join-Path $gitDir 'git.exe') -OutputType ConsoleApplication

. (Join-Path $scripts 'sot-lease.ps1')
. (Join-Path $scripts 'sot-hosts.ps1')

try {
    try {
        Write-Host "`n=== a stalled command ends at its bound ===" -ForegroundColor Cyan
        $marker = Join-Path $work 'stalled.pid'
        $clock = [System.Diagnostics.Stopwatch]::StartNew()
        $r = Invoke-SotWithin 2 'powershell' @('-NoProfile', '-Command', "Set-Content -LiteralPath '$marker' -Value `$PID; Start-Sleep 60")
        $clock.Stop()
        Check 'the code is 124' ($r.Code -eq 124) "Code=$($r.Code)"
        Check 'the output names the command and the bound' ($r.Output -ceq 'powershell did not finish within 2 s') "Output='$($r.Output)'"
        Check 'it ends within 5 s' ($clock.Elapsed.TotalSeconds -lt 5) "took $($clock.Elapsed.TotalSeconds) s"
        $childPid = 0
        if (Test-Path -LiteralPath $marker) { $childPid = [int](Get-Content -LiteralPath $marker | Select-Object -First 1) }
        Check 'the child recorded its pid' ($childPid -gt 0) 'the child never started'
        Check 'the child is gone' ($childPid -gt 0 -and -not (Get-Process -Id $childPid -ErrorAction SilentlyContinue)) "pid $childPid still runs"
    } catch { Check 'stalled: the section ran' $false $_.Exception.Message }

    try {
        Write-Host "`n=== a command that finishes returns its output ===" -ForegroundColor Cyan
        $r = Invoke-SotWithin 30 'powershell' @('-NoProfile', '-Command', 'Write-Output hello; exit 3')
        Check 'its exit code' ($r.Code -eq 3) "Code=$($r.Code)"
        Check 'its output' ($r.Output.Trim() -ceq 'hello' -and $r.Stdout.Trim() -ceq 'hello') "Output='$($r.Output)'"
        $r = Invoke-SotWithin 30 'powershell' @('-NoProfile', '-Command', '$x = [Console]::In.ReadToEnd(); Write-Output "got:$($x.Trim())"') "line one`nline two`n"
        Check 'stdin reaches the command' ($r.Output -match 'got:line one\s+line two') "Output='$($r.Output)'"
        $threw = $false
        try { Invoke-SotWithin 5 'powershell' @('-NoProfile', '-Command', 'exit 0') ('x' * 4001) | Out-Null } catch { $threw = $true }
        Check 'a stdin over 4000 bytes is refused' $threw 'no error'
    } catch { Check 'finishes: the section ran' $false $_.Exception.Message }

    try {
        Write-Host "`n=== shutdown-sot.ps1's remote read: the tree's ssh options, the command on stdin, a hung ssh named ===" -ForegroundColor Cyan
        # A stand-in ssh.exe (Process.Start finds only .exe by name), first on the child's PATH: it records its argv and
        # stdin and answers one line, or sleeps 60 s, ending itself, when SOT_TEST_SSH_HANG is 1.
        $sshDir = Join-Path $work 'stub-ssh'
        New-Item -ItemType Directory -Force -Path $sshDir | Out-Null
        $sshLog = Join-Path $work 'ssh.log'
        Add-Type -TypeDefinition @'
public class SshStub {
    public static int Main(string[] a) {
        if (System.Environment.GetEnvironmentVariable("SOT_TEST_SSH_HANG") == "1") System.Threading.Thread.Sleep(60000);
        string stdin = System.Console.In.ReadToEnd();
        System.IO.File.WriteAllText(System.Environment.GetEnvironmentVariable("SOT_TEST_SSH_LOG"), string.Join(" ", a) + "\n--stdin--\n" + stdin);
        System.Console.WriteLine("frontend disconnected stand-in");
        return 0;
    }
}
'@ -OutputAssembly (Join-Path $sshDir 'ssh.exe') -OutputType ConsoleApplication
        $readDisconnect = @'
$ErrorActionPreference = 'Stop'
. (Join-Path $env:SOT_TEST_SCRIPTS 'sot-lease.ps1')
. (Join-Path $env:SOT_TEST_SCRIPTS 'sot-hosts.ps1')
$ast = [System.Management.Automation.Language.Parser]::ParseFile((Join-Path $env:SOT_TEST_SCRIPTS 'shutdown-sot.ps1'), [ref]$null, [ref]$null)
$fn = $ast.Find({ param($n) ($n -is [System.Management.Automation.Language.FunctionDefinitionAst]) -and $n.Name -eq 'Get-DaemonDisconnect' }, $true)
. ([scriptblock]::Create($fn.Extent.Text))
$clock = [System.Diagnostics.Stopwatch]::StartNew()
$line = ''; $said = ''
try { $line = Get-DaemonDisconnect 'be-host' ([int]$env:SOT_TEST_LINES) ([int]$env:SOT_TEST_SECONDS) } catch { $said = $_.Exception.Message }
@{ line = "$line"; said = $said; took = $clock.Elapsed.TotalSeconds } | ConvertTo-Json -Compress
'@
        $sshEnv = @{ PATH = "$sshDir;$env:PATH"; SOT_TEST_SCRIPTS = $scripts; SOT_TEST_SSH_LOG = $sshLog; SOT_TEST_LINES = 400; SOT_TEST_SECONDS = 30 }
        $r = ConvertFrom-Json (Invoke-Child 'ssh-read' $readDisconnect $sshEnv).Out
        Check 'the line comes back' ($r.line -ceq 'frontend disconnected stand-in') "got '$($r.line)', said '$($r.said)'"
        $logged = Get-Content -LiteralPath $sshLog -Raw
        Check 'ssh ran with ServerAliveInterval' ($logged -match 'ServerAliveInterval=15') "argv: $logged"
        Check 'and ServerAliveCountMax and ConnectTimeout' ($logged -match 'ServerAliveCountMax=3' -and $logged -match 'ConnectTimeout=10') "argv: $logged"
        Check 'the remote command goes on stdin to bash -s' ($logged -match 'be-host bash -s' -and $logged -match '--stdin--\s+journalctl --user -u sotd\.service --no-pager -n 400 ') "argv and stdin: $logged"
        $sshEnv.SOT_TEST_SSH_HANG = 1; $sshEnv.SOT_TEST_LINES = 60; $sshEnv.SOT_TEST_SECONDS = 2
        $r = ConvertFrom-Json (Invoke-Child 'ssh-hang' $readDisconnect $sshEnv).Out
        Check 'a hung ssh throws the helper''s words' ($r.said -ceq 'ssh did not finish within 2 s') "got '$($r.said)'"
        Check 'within 5 s' ($r.took -lt 5) "took $($r.took) s"
    } catch { Check 'ssh: the section ran' $false $_.Exception.Message }

    try {
        Write-Host "`n=== sot-install-layout.ps1's tag fetch carries the stall bound ===" -ForegroundColor Cyan
        # The stand-in git.exe first on the child's PATH; a stand-in staged sotd.exe reports a version no clone has a tag for,
        # so Initialize-InstallLayout fetches tags and stops at "tag unavailable". The child's homes are empty folders.
        $prefix = Join-Path $work 'prefix'
        New-Item -ItemType Directory -Force -Path (Join-Path $prefix 'bin'), (Join-Path $work 'clone\.git'), (Join-Path $work 'home') | Out-Null
        Add-Type -TypeDefinition 'public class StagedSotd { public static int Main(string[] args) { if (args.Length > 0 && args[0] == "--version") System.Console.WriteLine("sotd 9.9.9 (stand-in)"); return 0; } }' -OutputAssembly (Join-Path $prefix 'bin\sotd.exe') -OutputType ConsoleApplication
        $gitLog = Join-Path $work 'git-layout.log'
        $layout = @'
$ErrorActionPreference = 'Stop'
. (Join-Path $env:SOT_TEST_SCRIPTS 'sot-install-layout.ps1')
function Write-SupLog([string]$Text) { }
function Set-LaunchStatus([string]$Text) { }
$prefixDir = $env:SOT_TEST_PREFIX
$repoCurrent = Join-Path $prefixDir 'repo\current'
$repo = $env:SOT_TEST_CLONE
$backendExe = ''
$NoUpdate = $false
$script:launchNotices = New-Object 'System.Collections.Generic.List[string]'
Initialize-InstallLayout
'@
        $home5 = Join-Path $work 'home'
        $null = Invoke-Child 'layout' $layout @{ PATH = "$gitDir;$env:PATH"; SOT_TEST_SCRIPTS = $scripts; SOT_TEST_GIT_LOG = $gitLog; SOT_TEST_PREFIX = $prefix; SOT_TEST_CLONE = (Join-Path $work 'clone'); HOME = $home5; USERPROFILE = $home5; LOCALAPPDATA = $home5; XDG_CONFIG_HOME = $home5; XDG_STATE_HOME = $home5 }
        $fetchLines = @(Get-Content -LiteralPath $gitLog -ErrorAction SilentlyContinue | Where-Object { $_ -match ' fetch ' })
        Check 'the layout fetched the tags' ($fetchLines.Count -eq 1) "fetch lines: $($fetchLines.Count)"
        Check 'the fetch carries the stall bound' (@($fetchLines | Where-Object { $_ -notmatch '-c http\.lowSpeedLimit=1000 -c http\.lowSpeedTime=60' }).Count -eq 0) "got: $($fetchLines -join '; ')"
    } catch { Check 'layout: the section ran' $false $_.Exception.Message }
} finally { Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue }

Write-Host "`n================ $pass passed, $fail failed ================" -ForegroundColor $(if ($fail) { 'Red' } else { 'Green' })
if ($fail) { exit 1 }
exit 0

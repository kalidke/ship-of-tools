# test-sot-within.ps1 -- the PowerShell launch scripts' network bounds. scripts/sot-hosts.ps1's Invoke-SotWithin, a
# native command under a wall-clock bound, started and ended by sot-lease.ps1's Start-SotRedirected and Stop-SotTree:
# a stalled command ends at its bound with Code 124 and a line naming it, with every process it started; so does one
# whose descendant holds its output after it exits; a command that finishes returns its output and exit code; stdin
# reaches the command, and only the caller's bytes do, whatever the console's input encoding; a command that cannot
# start is Code 127. Then each script call a test can reach, its stand-in first on a child PowerShell's PATH, never
# this process's: shutdown-sot.ps1's Get-DaemonDisconnect (read as a syntax tree) against a stand-in ssh.exe, and
# sot-install-layout.ps1's tag fetch and launch-sot.ps1's self-update pull (its stall bound, and git's stall error read
# as offline) against a stand-in git.exe.
# Run under WINDOWS POWERSHELL 5.1: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\tests\test-sot-within.ps1

# Refuses unless this runs under a scratch profile (scratch-guard.ps1).
. (Join-Path $PSScriptRoot 'scratch-guard.ps1')
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

# The processes still running whose command line holds $Tag, polled for up to 5 s while the ended ones go (a check
# only: nothing here signals them).
function Get-Tagged([string]$Tag) {
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    do {
        $left = @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | Where-Object { "$($_.CommandLine)" -like "*$Tag*" })
        if ($left.Count -eq 0) { break }
        Start-Sleep -Milliseconds 200
    } while ($clock.Elapsed.TotalSeconds -lt 5)
    return $left
}

try {
    try {
        Write-Host "`n=== a stalled command ends at its bound, with every process it started ===" -ForegroundColor Cyan
        # cmd starts a background PowerShell (as ssh starts a ProxyCommand), then waits on a foreground one; both sleep 60 s,
        # ending themselves if a run fails, and both carry the tag in their command lines.
        $tag = 'sot-within-stalled-' + [guid]::NewGuid().ToString('N').Substring(0, 8)
        $clock = [System.Diagnostics.Stopwatch]::StartNew()
        $r = Invoke-SotWithin 5 'cmd.exe' @('/c', "start /b powershell -NoProfile -Command Start-Sleep 60 # $tag & powershell -NoProfile -Command Start-Sleep 60 # $tag")
        $clock.Stop()
        Check 'the code is 124' ($r.Code -eq 124) "Code=$($r.Code)"
        Check 'the output names the command and the bound' ($r.Output -ceq 'cmd.exe did not finish within 5 s') "Output='$($r.Output)'"
        Check 'it ends within 10 s' ($clock.Elapsed.TotalSeconds -lt 10) "took $($clock.Elapsed.TotalSeconds) s"
        $left = @(Get-Tagged $tag)
        Check 'every process the command started is gone' ($left.Count -eq 0) "still running: $(@($left | ForEach-Object { "$($_.ProcessId) $($_.Name)" }) -join ', ')"

    } catch { Check 'stalled: the section ran' $false $_.Exception.Message }

    try {
        Write-Host "`n=== a command whose descendant holds its output after it exits ends at the bound ===" -ForegroundColor Cyan
        # cmd exits at once and leaves a background PowerShell holding the output. Run in a child PowerShell held to 20 s, so
        # a call that waits on that output fails the test instead of holding it.
        $tag = 'sot-within-held-' + [guid]::NewGuid().ToString('N').Substring(0, 8)
        $held = @'
$ErrorActionPreference = 'Stop'
. (Join-Path $env:SOT_TEST_SCRIPTS 'sot-lease.ps1')
. (Join-Path $env:SOT_TEST_SCRIPTS 'sot-hosts.ps1')
$clock = [System.Diagnostics.Stopwatch]::StartNew()
$r = Invoke-SotWithin 2 'cmd.exe' @('/c', "start /b powershell -NoProfile -Command Start-Sleep 60 # $env:SOT_TEST_TAG")
@{ code = $r.Code; output = $r.Output; took = $clock.Elapsed.TotalSeconds } | ConvertTo-Json -Compress
'@
        $r = ConvertFrom-Json (Invoke-Child 'held' $held @{ SOT_TEST_SCRIPTS = $scripts; SOT_TEST_TAG = $tag } 20).Out
        Check 'the code is 124' ($r.code -eq 124) "code=$($r.code) output='$($r.output)'"
        Check 'within 10 s' ($r.took -lt 10) "took $($r.took) s"
        $left = @(Get-Tagged $tag)
        Check 'the holder is gone' ($left.Count -eq 0) "still running: $(@($left | ForEach-Object { "$($_.ProcessId) $($_.Name)" }) -join ', ')"
    } catch { Check 'held: the section ran' $false $_.Exception.Message }

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
        Write-Host "`n=== a command that cannot start is a result, not a throw ===" -ForegroundColor Cyan
        $r = try { Invoke-SotWithin 5 'sot-no-such-program' @() } catch { @{ Code = 'threw'; Output = $_.Exception.Message } }
        Check 'the code is 127' ($r.Code -eq 127) "Code=$($r.Code) Output='$($r.Output)'"
        Check 'the output names the program' ($r.Output -like 'sot-no-such-program could not start: *') "Output='$($r.Output)'"

    } catch { Check 'start: the section ran' $false $_.Exception.Message }

    try {
        Write-Host "`n=== the command reads only its caller's bytes, whatever the console's input encoding ===" -ForegroundColor Cyan
        # A hidden PowerShell with a console of its own, as the launcher runs, sets its input encoding to UTF-8 with a BOM,
        # which Windows PowerShell 5.1 writes into a redirected input before the caller's first byte, and runs a stand-in
        # that prints every byte it reads, in hex.
        $hexExe = Join-Path $work 'stdin-hex.exe'
        Add-Type -TypeDefinition 'public class StdinHex { public static int Main() { var m = new System.IO.MemoryStream(); System.Console.OpenStandardInput().CopyTo(m); System.Console.Write(System.BitConverter.ToString(m.ToArray())); return 0; } }' -OutputAssembly $hexExe -OutputType ConsoleApplication
        $bomOut = Join-Path $work 'bom.out'
        Set-Content -LiteralPath (Join-Path $work 'bom.ps1') -Encoding ascii -Value @'
param([string]$Scripts, [string]$Exe, [string]$Out)
try {
    . (Join-Path $Scripts 'sot-lease.ps1')
    . (Join-Path $Scripts 'sot-hosts.ps1')
    [Console]::InputEncoding = [System.Text.Encoding]::UTF8
    $r = Invoke-SotWithin 30 $Exe @() "x`n"
    $kept = [Console]::InputEncoding.GetPreamble().Length -eq 3
    Set-Content -LiteralPath $Out -Value ('read={0} code={1} restored={2}' -f $r.Stdout, $r.Code, $kept)
} catch {
    Set-Content -LiteralPath $Out -Value ('error=' + $_.Exception.Message)
}
'@
        $child = Start-Process -FilePath 'powershell.exe' -ArgumentList ('-NoProfile -ExecutionPolicy Bypass -File "{0}" -Scripts "{1}" -Exe "{2}" -Out "{3}"' -f (Join-Path $work 'bom.ps1'), $scripts, $hexExe, $bomOut) -WindowStyle Hidden -PassThru
        if (-not $child.WaitForExit(60000)) { try { $child.Kill() } catch { }; throw 'the hidden child did not finish within 60 s' }
        $got = if (Test-Path -LiteralPath $bomOut) { "$(Get-Content -LiteralPath $bomOut -Raw)".Trim() } else { '<no report>' }
        Check 'the command read x and a newline, and nothing before them; the console encoding was put back' ($got -ceq 'read=78-0A code=0 restored=True') "child: $got"
    } catch { Check 'bom: the section ran' $false $_.Exception.Message }

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

    try {
        Write-Host "`n=== launch-sot.ps1's self-update pull: its stall bound, and git's stall error read as offline ===" -ForegroundColor Cyan
        # The stand-in git.exe first on the child's PATH answers the pull with git's error for a transfer under
        # http.lowSpeedLimit; the prelude, read from launch-sot.ps1 as a syntax tree, must launch the current build as offline.
        New-Item -ItemType Directory -Force -Path (Join-Path $work 'dev-clone\.git') | Out-Null
        $gitLog = Join-Path $work 'git-pull.log'
        $pull = @'
$ErrorActionPreference = 'Stop'
$statuses = New-Object 'System.Collections.Generic.List[string]'
function Set-LaunchStatus([string]$Text) { $statuses.Add($Text) | Out-Null }
function Write-SupLog([string]$Text) { }
$ast = [System.Management.Automation.Language.Parser]::ParseFile((Join-Path $env:SOT_TEST_SCRIPTS 'launch-sot.ps1'), [ref]$null, [ref]$null)
$fn = $ast.Find({ param($n) ($n -is [System.Management.Automation.Language.FunctionDefinitionAst]) -and $n.Name -eq 'Invoke-SelfUpdatePrelude' }, $true)
. ([scriptblock]::Create($fn.Extent.Text))
$script:sotPinned = $false
$script:launchNotices = New-Object 'System.Collections.Generic.List[string]'
$repo = $env:SOT_TEST_CLONE
$NoUpdate = $false
$Local = $false
Remove-Item Env:\SOT_LAUNCH_REEXEC -ErrorAction SilentlyContinue
Invoke-SelfUpdatePrelude
@{ statuses = @($statuses) } | ConvertTo-Json -Compress
'@
        $r = ConvertFrom-Json (Invoke-Child 'pull' $pull @{ PATH = "$gitDir;$env:PATH"; SOT_TEST_SCRIPTS = $scripts; SOT_TEST_GIT_LOG = $gitLog; SOT_TEST_CLONE = (Join-Path $work 'dev-clone') }).Out
        $pullLines = @(Get-Content -LiteralPath $gitLog -ErrorAction SilentlyContinue | Where-Object { $_ -match ' pull ' })
        Check 'the prelude pulled once' ($pullLines.Count -eq 1) "pull lines: $($pullLines.Count)"
        Check 'the pull carries the stall bound' (@($pullLines | Where-Object { $_ -notmatch '-c http\.lowSpeedLimit=1000 -c http\.lowSpeedTime=60' }).Count -eq 0) "got: $($pullLines -join '; ')"
        Check 'a stalled pull launches the current build as offline' (@($r.statuses) -contains 'Offline - launching current build...') "statuses: $(@($r.statuses) -join ' | ')"
    } catch { Check 'pull: the section ran' $false $_.Exception.Message }
} finally { Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue }

Write-Host "`n================ $pass passed, $fail failed ================" -ForegroundColor $(if ($fail) { 'Red' } else { 'Green' })
if ($fail) { exit 1 }
exit 0

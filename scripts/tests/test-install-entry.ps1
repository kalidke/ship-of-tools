# test-install-entry.ps1 -- scripts/install.ps1 (the Windows entry: "update sot from the repo") against stand-ins, in a
# scratch profile and prefix. Dot-sourced by test-install-layout.ps1 after its last section, in its scope ($root,
# $repoRoot, $SotdPath, Check); run alone with test-install-layout.ps1's arguments through that file. Windows PowerShell
# 5.1, ASCII only.
#
# The stand-ins: a compiled sot/sotd/sot-capsule per version that prints its own version, answers the daemon questions
# from SOT_FAKE_* and hands every other `sotd` verb (update direction, update stage, trust declare) to the real sotd;
# a git origin holding this tree's scripts at one tag per version; a julia and a jq the comm step can run; the network
# replaced by copies from pack folders. Nothing reaches GitHub, the person's profile, install or daemon.

$entryRoot = Join-Path $root 'entry'
New-Item -ItemType Directory -Force -Path $entryRoot | Out-Null
$savedEnv = @{}
foreach ($n in @('USERPROFILE', 'LOCALAPPDATA', 'APPDATA', 'SOT_COMM_HOME', 'SSH_CONNECTION', 'PATH', 'SOT_INSTALL_ORIGIN', 'SOT_INSTALL_TAG', 'SOT_INSTALL_IMPLICIT', 'SOT_INSTALL_CONVERGE_WAIT_S', 'SOT_INSTALL_DAEMON_WAIT_S', 'SOT_FAKE_DAEMON', 'SOT_FAKE_REPLS', 'SOT_FAKE_REPLS_RC', 'SOT_FAKE_REAL')) {
    $savedEnv[$n] = [Environment]::GetEnvironmentVariable($n)
}
try {
    Write-Host "`n=== 7. install.ps1: the Windows entry ===" -ForegroundColor Cyan
    $entryScript = Join-Path $repoRoot 'scripts\install.ps1'
    $errs = $null
    [void][System.Management.Automation.Language.Parser]::ParseFile($entryScript, [ref]$null, [ref]$errs)
    Check '7: install.ps1 parses under Windows PowerShell 5.1' ($errs.Count -eq 0) (($errs | ForEach-Object { "$($_.Extent.StartLineNumber): $($_.Message)" }) -join '; ')
    . $entryScript

    # ---- the stand-in executable, one per version -------------------------------------------------------------------
    $fakeSrc = @'
using System; using System.Diagnostics; using System.IO;
public class P { public static int Main(string[] a) {
  string name = Path.GetFileNameWithoutExtension(System.Reflection.Assembly.GetExecutingAssembly().Location);
  string ver = "@VER@", sha = "@SHA@";
  if (a.Length > 0 && a[0] == "--version") { Console.WriteLine(name + " " + ver + " (" + sha + " 2026-10-17)"); return 0; }
  string d = Environment.GetEnvironmentVariable("SOT_FAKE_DAEMON") ?? "2";
  if (name == "sotd" && a.Length > 0 && a[0] == "session-socket-path") { Console.WriteLine(@"\\.\pipe\sot-entry-test"); return 0; }
  if (name == "sotd" && a.Length > 0 && a[0] == "same-version") { int c = int.Parse(d); if (c == 3 || c == 4) Console.WriteLine("9.9.0"); if (c == 0) return 0; return c; }
  if (name == "sotd" && a.Length > 1 && a[0] == "update" && a[1] == "repls") {
    string lines = Environment.GetEnvironmentVariable("SOT_FAKE_REPLS") ?? "";
    foreach (string l in lines.Split('|')) if (l.Length > 0) Console.WriteLine(l);
    int rc = int.Parse(Environment.GetEnvironmentVariable("SOT_FAKE_REPLS_RC") ?? "0");
    if (rc == 1) Console.Error.WriteLine("sotd update repls: no answer within 10 s");
    return rc; }
  string real = Environment.GetEnvironmentVariable("SOT_FAKE_REAL");
  if (name == "sotd" && real != null) {
    ProcessStartInfo psi = new ProcessStartInfo(real); psi.UseShellExecute = false;
    foreach (string x in a) psi.Arguments += "\"" + x.Replace("\"", "\\\"") + "\" ";
    Process p = Process.Start(psi); p.WaitForExit(); return p.ExitCode; }
  return 0; } }
'@
    function New-FakeExe([string]$Path, [string]$Ver, [string]$Sha) {
        Add-Type -TypeDefinition ($fakeSrc.Replace('@VER@', $Ver).Replace('@SHA@', $Sha)) -OutputAssembly $Path -OutputType ConsoleApplication
    }

    # ---- a release: an origin commit, a tag and a pack folder (SHA256SUMS, the zip, COMMIT) --------------------------------
    $git = 'git.exe'
    $origin = Join-Path $entryRoot 'origin'
    New-Item -ItemType Directory -Force -Path $origin | Out-Null
    # git writes warnings (line endings) to stderr, which 5.1 turns into terminating errors under 'Stop'.
    $eapGit = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
    & $git -C $origin init -q 2>&1 | Out-Null
    & $git -C $origin config user.email t@example.com; & $git -C $origin config user.name t
    & $git -C $origin config core.autocrlf false; & $git -C $origin config core.safecrlf false
    Copy-Item -Recurse -Force (Join-Path $repoRoot 'scripts') (Join-Path $origin 'scripts')
    foreach ($envName in @('kernel', 'repl', 'pluto')) {
        New-Item -ItemType Directory -Force -Path (Join-Path $origin "julia\$envName") | Out-Null
        Set-Content -LiteralPath (Join-Path $origin "julia\$envName\Project.toml") -Value '# fixture'
    }
    function New-Release([string]$Ver) {
        Set-Content -LiteralPath (Join-Path $origin 'VERSION.txt') -Value $Ver
        & $git -C $origin add -A 2>&1 | Out-Null
        & $git -C $origin commit -q -m "release $Ver" 2>&1 | Out-Null
        & $git -C $origin tag "v$Ver" 2>&1 | Out-Null
        $commit = (& $git -C $origin rev-parse HEAD).Trim()
        $pack = Join-Path $entryRoot "pack-$Ver"
        $inner = Join-Path $entryRoot "stage-$Ver\sot-$Ver-windows-x86_64"
        New-Item -ItemType Directory -Force -Path $pack, $inner | Out-Null
        foreach ($e in @('sot', 'sotd', 'sot-capsule')) { New-FakeExe (Join-Path $inner "$e.exe") $Ver $commit.Substring(0, 9) }
        $zip = Join-Path $pack "sot-$Ver-windows-x86_64.zip"
        # Entries are written with `/` as pack-build.ps1 does: 5.1's CreateFromDirectory writes `\`, which the updater refuses.
        Add-Type -AssemblyName System.IO.Compression
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $archive = New-Object System.IO.Compression.ZipArchive([System.IO.File]::Create($zip), [System.IO.Compression.ZipArchiveMode]::Create)
        try {
            foreach ($f in Get-ChildItem -LiteralPath $inner -File) {
                [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, $f.FullName, "sot-$Ver-windows-x86_64/$($f.Name)")
            }
        } finally { $archive.Dispose() }
        [System.IO.File]::WriteAllText((Join-Path $pack 'COMMIT'), "$commit`n")
        $lines = foreach ($f in @("sot-$Ver-windows-x86_64.zip", 'COMMIT')) { "$((Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $pack $f)).Hash.ToLowerInvariant())  $f" }
        [System.IO.File]::WriteAllText((Join-Path $pack 'SHA256SUMS'), (($lines -join "`n") + "`n"))
        return [pscustomobject]@{ Ver = $Ver; Pack = $pack; Commit = $commit; Short = $commit.Substring(0, 9) }
    }
    $r98 = New-Release '9.9.8'; $r99 = New-Release '9.9.9'
    $ErrorActionPreference = $eapGit
    Check '7: two releases and their packs are built' ((Test-Path (Join-Path $r98.Pack 'SHA256SUMS')) -and (Test-Path (Join-Path $r99.Pack 'COMMIT'))) 'the fixture did not build'

    # ---- a scratch profile with julia and jq stand-ins -------------------------------------------------------------------
    $stubs = Join-Path $entryRoot 'stubs'
    New-Item -ItemType Directory -Force -Path $stubs | Out-Null
    Set-Content -LiteralPath (Join-Path $stubs 'julia.ps1') -Value @'
$a = $args
if (($a -join ' ') -match 'update_comm') {
    $p = ($a | Where-Object { $_ -like '--project=*' } | Select-Object -First 1).Substring(10)
    $home_ = if ($env:SOT_COMM_HOME) { $env:SOT_COMM_HOME } else { Join-Path $env:USERPROFILE '.sot-comm' }
    New-Item -ItemType Directory -Force -Path (Join-Path $home_ 'bin') | Out-Null
    $short = (git -C $p rev-parse --short=9 HEAD).Trim()
    Set-Content -LiteralPath (Join-Path $home_ 'VERSION') -Value $short
} elseif (($a -join ' ') -match 'instantiate') {
    $p = ($a | Where-Object { $_ -like '--project=*' } | Select-Object -First 1).Substring(10)
    New-Item -ItemType Directory -Force -Path $p | Out-Null
    Set-Content -LiteralPath (Join-Path $p 'Manifest.toml') -Value ''
}
exit 0
'@
    Set-Content -LiteralPath (Join-Path $stubs 'julia.cmd') -Value "@echo off`r`npowershell -NoProfile -ExecutionPolicy Bypass -File `"%~dp0julia.ps1`" %*`r`nexit /b 0"
    Set-Content -LiteralPath (Join-Path $stubs 'jq.cmd') -Value "@echo off`r`nexit /b 0"
    $basePath = $savedEnv['PATH']

    function New-Home([string]$Name) {
        $h = Join-Path $entryRoot $Name
        foreach ($d in @('Desktop', 'AppData\Roaming', 'AppData\Local', 'comm')) { New-Item -ItemType Directory -Force -Path (Join-Path $h $d) | Out-Null }
        $env:USERPROFILE = $h; $env:APPDATA = Join-Path $h 'AppData\Roaming'; $env:LOCALAPPDATA = Join-Path $h 'AppData\Local'
        $env:SOT_COMM_HOME = Join-Path $h 'comm'; $env:PATH = "$stubs;$basePath"
        foreach ($n in @('SSH_CONNECTION', 'SOT_INSTALL_TAG', 'SOT_INSTALL_IMPLICIT', 'SOT_FAKE_DAEMON', 'SOT_FAKE_REPLS', 'SOT_FAKE_REPLS_RC')) { [Environment]::SetEnvironmentVariable($n, $null) }
        $env:SOT_INSTALL_ORIGIN = $origin; $env:SOT_FAKE_REAL = $SotdPath
        return [pscustomobject]@{ Home = $h; Prefix = Join-Path $h 'AppData\Local\sot' }
    }
    # The entry as an agent runs it: the output lines (the information stream included) and the exit code.
    function Invoke-Entry([string[]]$ArgList) {
        $out = @(Invoke-InstallEntryCaptured $ArgList)
        return [pscustomobject]@{ Out = $out; Last = $(if ($out.Count) { $out[-1] } else { '' }); Rc = $script:EntryRc; Verdicts = @($out | Where-Object { $_ -match '^(PASS|FAIL|ONE STEP LEFT): ' }).Count }
    }
    # Write-EntryLine is replaced so a run's every line is kept in order, an interrupted one included.
    function Write-EntryLine([string]$Text) { if ($script:Capturing) { $script:Captured += $Text } else { Write-Host $Text } }
    function Invoke-InstallEntryCaptured([string[]]$ArgList) {
        $script:EntryRc = -1; $script:Captured = @(); $script:Capturing = $true
        try { $script:EntryRc = Invoke-InstallEntry $ArgList } catch { $script:EntryRc = 255 } finally { $script:Capturing = $false }
        return @($script:Captured)
    }
    # A frontend-only record, so `sotd update stage` prepares no Julia environment and runs no npm (no network here).
    function Set-FrontendOnly([string]$Prefix) {
        $p = Join-Path $Prefix 'install.json'
        $t = [System.IO.File]::ReadAllText($p)
        if ($t -notmatch '"daemon"') { [System.IO.File]::WriteAllText($p, $t.Replace('"service": "none",', "`"service`": `"none`",`n  `"daemon`": false,")) }
    }
    function Get-Json([string]$Prefix, [string]$Key) { return (Get-JsonString (Join-Path $Prefix 'install.json') $Key) }
    function Get-Tree([string]$Path) { return (@(Get-ChildItem -LiteralPath $Path -Recurse -Force -File -ErrorAction SilentlyContinue | ForEach-Object { "$($_.FullName.Substring($Path.Length)):$($_.Length)" } | Sort-Object) -join '|') }

    # 1. Fresh from a pack folder.
    $h1 = New-Home 'h1'
    $e = Invoke-Entry @('-Archive', $r98.Pack)
    Check '7a: a fresh install from a pack ends in one PASS line, the last' ("$($e.Rc) $($e.Verdicts) $($e.Last)" -ceq "0 1 PASS: Ship of Tools 9.9.8 at $($r98.Short) on this computer") "rc=$($e.Rc) verdicts=$($e.Verdicts) last='$($e.Last)'; $(($e.Out | Select-Object -Last 4) -join ' / ')"
    Set-FrontendOnly $h1.Prefix
    Check '7a: install.json names the version and the commit, and is pinned (a pack names its version)' (((Get-Json $h1.Prefix 'version') -eq '9.9.8') -and ((Get-Json $h1.Prefix 'commit') -eq $r98.Commit) -and ((Get-Content -LiteralPath (Join-Path $h1.Prefix 'install.json') -Raw) -match '"pinned"\s*:\s*true')) (Get-Content -LiteralPath (Join-Path $h1.Prefix 'install.json') -Raw)
    $tgt = ''; try { $tgt = [string]@((Get-Item -LiteralPath (Join-Path $h1.Prefix 'repo\current') -Force).Target)[0] } catch { }
    Check '7a: repo\current is a junction to repo\versions\v9.9.8' ($tgt -like '*repo\versions\v9.9.8') $tgt
    $sc = ''; try { $sc = (New-Object -ComObject WScript.Shell).CreateShortcut((Join-Path $h1.Home 'Desktop\Ship of Tools.lnk')).Arguments } catch { }
    Check '7a: the Desktop shortcut targets repo\current\scripts\launch-sot.ps1' ($sc -like '*repo\current\scripts\launch-sot.ps1*') $sc
    Check '7a: comm is at the installed commit' ((Get-Content -LiteralPath (Join-Path $h1.Home 'comm\VERSION') -ErrorAction SilentlyContinue) -eq $r98.Short) 'the comm stamp'

    # 2. Update from that layout, nothing running, a desktop session: applied here.
    $e = Invoke-Entry @('-Archive', $r99.Pack)
    Check '7b: an update with nothing running applies here and ends in PASS' ("$($e.Rc) $($e.Verdicts) $($e.Last)" -ceq "0 1 PASS: Ship of Tools 9.9.9 at $($r99.Short) on this computer") "rc=$($e.Rc) last='$($e.Last)'; $(($e.Out | Select-Object -Last 6) -join ' / ')"
    $tgt = ''; try { $tgt = [string]@((Get-Item -LiteralPath (Join-Path $h1.Prefix 'repo\current') -Force).Target)[0] } catch { }
    Check '7b: the junction moved, no pointer remains, the previous version folder is kept' (($tgt -like '*repo\versions\v9.9.9') -and -not (Test-Path (Join-Path $h1.Prefix 'updates\pending-windows-x86_64.json')) -and (Test-Path (Join-Path $h1.Prefix 'repo\versions\v9.9.8'))) $tgt

    # 3. A repeat on a current box is a repair run: no stage, no restart, PASS. Naming the same pack stages and applies again.
    $before = Get-Tree (Join-Path $h1.Prefix 'updates')
    $env:SOT_INSTALL_TAG = 'v9.9.9'; $env:SOT_INSTALL_IMPLICIT = '1'
    $fetchLog = New-Object System.Collections.ArrayList
    function Invoke-CurlFile([string]$Url, [string]$Path) { [void]$fetchLog.Add($Url); $f = Split-Path $Url -Leaf; $src = Join-Path $script:servePack $f; if (-not (Test-Path -LiteralPath $src)) { return [pscustomobject]@{ Ok = $false; Code = 22 } }; Copy-Item -LiteralPath $src -Destination $Path -Force; return [pscustomobject]@{ Ok = $true; Code = 0 } }
    $script:servePack = $r99.Pack
    $e = Invoke-Entry @()
    Check '7c: an implicit run at the installed version is a repair run ending in PASS' ("$($e.Rc) $($e.Verdicts)" -ceq '0 1' -and $e.Last -like 'PASS: *9.9.9*' -and ($e.Out -match 'a repair run').Count -eq 1) "rc=$($e.Rc) last='$($e.Last)'"
    Check '7c: it staged nothing' ((Get-Tree (Join-Path $h1.Prefix 'updates')) -ceq $before) 'the updates folder changed'
    Check '7c: and the repair run removed the pin (it named no version)' (-not ((Get-Content -LiteralPath (Join-Path $h1.Prefix 'install.json') -Raw) -match 'pinned')) 'pinned is still recorded'

    # 3b. An explicit run of the installed version (how the lab pins a box) is a repair run too: the pin is written and
    # nothing is staged, applied or restarted, with a window's supervisor up and a daemon answering as the installed version.
    [Environment]::SetEnvironmentVariable('SOT_INSTALL_TAG', $null); [Environment]::SetEnvironmentVariable('SOT_INSTALL_IMPLICIT', $null)
    $env:SOT_FAKE_DAEMON = '0'
    $logDirP = Join-Path $h1.Home 'AppData\Local\sot\logs'
    New-Item -ItemType Directory -Force -Path $logDirP | Out-Null
    $supP = Join-Path $entryRoot 'launch-sot.ps1'
    Set-Content -LiteralPath $supP -Value 'Start-Sleep -Seconds 120'
    $procP = Start-Process -FilePath powershell.exe -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $supP) -PassThru -WindowStyle Hidden
    try {
        Start-Sleep -Seconds 2
        Set-Content -LiteralPath (Join-Path $logDirP 'launcher.pid') -Value "$($procP.Id)"
        Set-Content -LiteralPath (Join-Path $logDirP 'supervisor.log') -Value "2026-10-17T00:00:00  pid=$($procP.Id)  supervisor start (relaunched=False) depth=0 ws=1MB code=abc-def"
        $before = Get-Tree (Join-Path $h1.Prefix 'updates')
        $e = Invoke-Entry @('-Archive', $r99.Pack)
        Check '7c2: an explicit run of the installed version is a repair run ending in PASS' ("$($e.Rc) $($e.Verdicts)" -ceq '0 1' -and $e.Last -like 'PASS: *9.9.9*' -and ($e.Out -match 'a repair run').Count -eq 1) "rc=$($e.Rc) last='$($e.Last)'"
        Check '7c2: it wrote the pin' ((Get-Content -LiteralPath (Join-Path $h1.Prefix 'install.json') -Raw) -match '"pinned"\s*:\s*true') 'no pin recorded'
        Check '7c2: it staged nothing, wrote no sentinel and touched no window' (((Get-Tree (Join-Path $h1.Prefix 'updates')) -ceq $before) -and -not (Test-Path (Join-Path $h1.Home 'AppData\Local\sot\relaunch.request')) -and -not $procP.HasExited) 'something was staged or signalled'
    } finally {
        Stop-Process -Id $procP.Id -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath (Join-Path $logDirP 'launcher.pid') -Force -ErrorAction SilentlyContinue
        [Environment]::SetEnvironmentVariable('SOT_FAKE_DAEMON', $null)
    }

    # 4. An implicit older target is refused before any write; an explicit one proceeds and says older.
    $env:SOT_INSTALL_TAG = 'v9.9.8'; $env:SOT_INSTALL_IMPLICIT = '1'; $script:servePack = $r98.Pack
    $before = Get-Tree $h1.Prefix
    $e = Invoke-Entry @()
    Check '7d: an implicit run that would move back stops (exit 2), one verdict line' ("$($e.Rc) $($e.Verdicts)" -ceq '2 1' -and $e.Last -like 'FAIL: direction: this computer runs 9.9.9, which is newer than 9.9.8*-Version v9.9.8') "rc=$($e.Rc) last='$($e.Last)'"
    Check '7d: nothing under the prefix changed' ((Get-Tree $h1.Prefix) -ceq $before) 'the prefix changed'
    [Environment]::SetEnvironmentVariable('SOT_INSTALL_IMPLICIT', $null)
    Set-FrontendOnly $h1.Prefix   # install-shortcut.ps1's install-manifest.ps1 rewrote the record
    $e = Invoke-Entry @('-Version', 'v9.9.8')
    Check '7d: a named older version says older and goes on' (($e.Out -match '\(older\)').Count -eq 1 -and $e.Last -like 'PASS: *9.9.8*') "rc=$($e.Rc) last='$($e.Last)'"
    Check '7d: and the install is pinned at the person''s choice' ((Get-Content -LiteralPath (Join-Path $h1.Prefix 'install.json') -Raw) -match '"pinned"\s*:\s*true') 'no pin recorded'

    # 5. A build with a + stops, with the line that sends the person to a pack.
    $h5 = New-Home 'h5'
    New-Item -ItemType Directory -Force -Path $h5.Prefix | Out-Null
    [System.IO.File]::WriteAllText((Join-Path $h5.Prefix 'install.json'), '{"schema":1,"prefix":"x","version":"9.9.9-dev+abc","tag":"v9.9.9-dev+abc","commit":"0"}')
    $e = Invoke-Entry @()
    Check '7e: a build with a + stops before any download (exit 2)' ("$($e.Rc) $($e.Verdicts)" -ceq '2 1' -and $e.Last -like 'FAIL: target: this computer runs 9.9.9-dev+abc, a build of the line, not a release: install a pack of the line with -Archive <folder>; a release named with -Version is usually older and moves this computer back; ask the person who runs it which they want') "rc=$($e.Rc) last='$($e.Last)'"

    # 6. Option errors and an interrupt end in one verdict line.
    $e = Invoke-Entry @('-Bogus')
    Check '7f: an unknown option: exit 2, one verdict line, the last' ("$($e.Rc) $($e.Verdicts)" -ceq '2 1' -and $e.Last -like 'FAIL: options: unknown option: -Bogus') "rc=$($e.Rc) last='$($e.Last)'"
    $e = Invoke-Entry @('-Version')
    Check '7f: -Version with no value: exit 2, one verdict line' ("$($e.Rc) $($e.Verdicts)" -ceq '2 1' -and $e.Last -like 'FAIL: options: -Version needs a value') "rc=$($e.Rc) last='$($e.Last)'"
    # Ctrl+C is a stopped pipeline: no catch block sees it, a finally block does. A runspace of its own is stopped
    # mid-run; its lines go to a file so the verdict the finally block prints survives the stop.
    $intLog = Join-Path $entryRoot 'interrupt.log'
    Remove-Item -LiteralPath $intLog -Force -ErrorAction SilentlyContinue
    $ps = [powershell]::Create()
    $null = $ps.AddScript({ param($f, $log)
        . $f
        function Write-EntryLine([string]$Text) { [System.IO.File]::AppendAllText($log, $Text + "`n") }
        function Get-Pack { Start-Sleep -Seconds 60 }
        [void](Invoke-InstallEntry @('-Archive', 'x'))
    }).AddArgument($entryScript).AddArgument($intLog)
    $async = $ps.BeginInvoke()
    Start-Sleep -Seconds 5
    $ps.Stop()
    try { [void]$ps.EndInvoke($async) } catch { }
    $ps.Dispose()
    $outInt = @(if (Test-Path -LiteralPath $intLog) { [System.IO.File]::ReadAllLines($intLog) })
    Check '7f: an interrupt (Ctrl+C) mid-run ends in exactly one verdict line' (@($outInt | Where-Object { $_ -match '^(PASS|FAIL|ONE STEP LEFT): ' }).Count -eq 1) ($outInt -join ' / ')

    # 7. The live-work ask: a daemon of another version with a live REPL stops the run before anything changes.
    $h7 = New-Home 'h7'
    [void](Invoke-Entry @('-Archive', $r98.Pack))
    Set-FrontendOnly $h7.Prefix
    $before = Get-Tree $h7.Prefix
    $env:SOT_FAKE_DAEMON = '3'; $env:SOT_FAKE_REPLS = 'repl Rig PC|pluto server'; $env:SOT_FAKE_REPLS_RC = '3'
    $e = Invoke-Entry @('-Archive', $r99.Pack)
    Check '7g: a live REPL and a Pluto server stop the run (exit 2) and name both' ("$($e.Rc) $($e.Verdicts)" -ceq '2 1' -and $e.Last -like 'FAIL: daemon:*Rig PC*Pluto server is running*') "rc=$($e.Rc) last='$($e.Last)'"
    Check '7g: nothing under the prefix changed' ((Get-Tree $h7.Prefix) -ceq $before) 'the prefix changed'
    $env:SOT_FAKE_REPLS_RC = '1'; $env:SOT_FAKE_REPLS = 'repl Rig PC'
    $e = Invoke-Entry @('-Archive', $r99.Pack)
    Check '7g: a daemon that cannot be asked fails closed with its own reason and the rows it could read' ($e.Rc -eq 2 -and $e.Last -like 'FAIL: daemon: could not ask*Rig PC*no answer within 10 s*') "rc=$($e.Rc) last='$($e.Last)'"

    # 8. The path choice: a stand-in supervisor with and without code=, nothing running over ssh.
    $env:SOT_FAKE_REPLS_RC = '0'; $env:SOT_FAKE_REPLS = ''
    $logDir = Join-Path $h7.Home 'AppData\Local\sot\logs'
    New-Item -ItemType Directory -Force -Path $logDir | Out-Null
    $sup = Join-Path $entryRoot 'launch-sot.ps1'
    Set-Content -LiteralPath $sup -Value 'Start-Sleep -Seconds 120'
    $proc = Start-Process -FilePath powershell.exe -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $sup) -PassThru -WindowStyle Hidden
    try {
        Start-Sleep -Seconds 2
        Set-Content -LiteralPath (Join-Path $logDir 'launcher.pid') -Value "$($proc.Id)"
        Set-Content -LiteralPath (Join-Path $logDir 'supervisor.log') -Value "2026-10-17T00:00:00  pid=$($proc.Id)  supervisor start (relaunched=False) depth=0 ws=1MB"
        $e = Invoke-Entry @('-Archive', $r99.Pack)
        Check '7h: a supervisor with no code= (older than rc9.13) leaves the update armed with the quit-and-start step' ($e.Rc -eq 0 -and $e.Last -like 'ONE STEP LEFT: quit Ship of Tools, choose Keep, and start it from the shortcut; the start applies v9.9.9*') "rc=$($e.Rc) last='$($e.Last)'"
        Check '7h: no sentinel was written and the update is armed' ((-not (Test-Path (Join-Path $h7.Home 'AppData\Local\sot\relaunch.request'))) -and (Test-Path (Join-Path $h7.Prefix 'updates\pending-windows-x86_64.json'))) 'sentinel present or pointer missing'
        Set-Content -LiteralPath (Join-Path $logDir 'supervisor.log') -Value "2026-10-17T00:00:00  pid=$($proc.Id)  supervisor start (relaunched=False) depth=0 ws=1MB code=abc-def"
        $env:SOT_INSTALL_CONVERGE_WAIT_S = '3'
        $e = Invoke-Entry @('-Archive', $r99.Pack)
        Check '7h: a supervisor with code= gets the converge sentinel, and one that never reports ends in FAIL apply with the sentinel removed' ($e.Rc -eq 1 -and ($e.Out -match 'converge requested').Count -eq 1 -and $e.Last -like 'FAIL: apply: *' -and -not (Test-Path (Join-Path $h7.Home 'AppData\Local\sot\relaunch.request'))) "rc=$($e.Rc) last='$($e.Last)'"
    } finally {
        Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath (Join-Path $logDir 'launcher.pid') -Force -ErrorAction SilentlyContinue
        [Environment]::SetEnvironmentVariable('SOT_INSTALL_CONVERGE_WAIT_S', $null)
    }
    $env:SSH_CONNECTION = '10.0.0.1 1 10.0.0.2 22'; $env:SOT_FAKE_DAEMON = '3'
    $e = Invoke-Entry @('-Archive', $r99.Pack, '-RestartDaemon')
    Check '7i: nothing running over ssh leaves the update armed with the desk step, and writes no sentinel' ($e.Rc -eq 0 -and $e.Last -like 'ONE STEP LEFT: start Ship of Tools at the desk; the start applies v9.9.9*' -and -not (Test-Path (Join-Path $h7.Home 'AppData\Local\sot\relaunch.request'))) "rc=$($e.Rc) last='$($e.Last)'"
    Check '7i: the install still runs the old version (nothing was applied)' ((Get-Json $h7.Prefix 'version') -eq '9.9.8') (Get-Json $h7.Prefix 'version')

    # 9. A daemon the converge restarted answers as the target within the bound, else FAIL.
    $h9 = New-Home 'h9'
    [void](Invoke-Entry @('-Archive', $r99.Pack))
    $env:SOT_FAKE_DAEMON = '3'; $env:SOT_INSTALL_DAEMON_WAIT_S = '2'
    $script:Done = $false; $script:ExpectDaemon = $true; $script:Steps = @()
    $verdictText = ''
    try { Test-EndState $h9.Prefix ([pscustomobject]@{ Version = '9.9.9'; Tag = 'v9.9.9'; Commit = $r99.Commit; Bin = '' }) (Join-Path $h9.Prefix 'bin\sotd.exe') '\\.\pipe\sot-entry-test' } catch { }
    Check '7j: a daemon that never answers as the target is a FAIL daemon, not a PASS' ($script:Done -and $script:Rc -eq 1) 'no verdict'
    $script:ExpectDaemon = $false
} catch {
    Check '7: section ran' $false $_.Exception.Message
} finally {
    foreach ($n in $savedEnv.Keys) { [Environment]::SetEnvironmentVariable($n, $savedEnv[$n]) }
}

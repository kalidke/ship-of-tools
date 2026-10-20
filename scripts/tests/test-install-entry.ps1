# test-install-entry.ps1 -- scripts/install.ps1 (the Windows entry: "update sot from the repo") against stand-ins, in a
# scratch profile and prefix. Dot-sourced by test-install-layout.ps1 after its last section, in its scope ($root,
# $repoRoot, $SotdPath, Check); run alone with test-install-layout.ps1's arguments through that file. Windows PowerShell
# 5.1, ASCII only.
#
# The stand-ins: a compiled sot/sotd/sot-capsule per version that prints its own version, answers the daemon questions
# from SOT_FAKE_* and hands every other `sotd` verb (update direction, update stage, trust declare) to the real sotd;
# a git origin holding this tree's scripts at one tag per version; a julia (a compiled one at SOT_JULIA_BIN for the
# stage's prepare, a script for the comm step) and a jq; the network replaced by copies from pack folders. Nothing
# reaches GitHub, the person's profile, install or daemon.

$entryRoot = Join-Path $root 'entry'
New-Item -ItemType Directory -Force -Path $entryRoot | Out-Null
$savedEnv = @{}
foreach ($n in @('USERPROFILE', 'LOCALAPPDATA', 'APPDATA', 'SOT_COMM_HOME', 'SSH_CONNECTION', 'PATH', 'SOT_INSTALL_ORIGIN', 'SOT_INSTALL_TAG', 'SOT_INSTALL_IMPLICIT', 'SOT_INSTALL_CONVERGE_WAIT_S', 'SOT_INSTALL_DAEMON_WAIT_S', 'SOT_FAKE_DAEMON', 'SOT_FAKE_REPLS', 'SOT_FAKE_REPLS_RC', 'SOT_FAKE_REAL', 'SOT_FAKE_SYNC', 'SOT_FAKE_SLEEP', 'SOT_FAKE_NO_INSTALLED', 'SOT_FAKE_DAEMON_FILE', 'SOT_INSTALL_REPO', 'SOT_JULIA_BIN', 'SOT_STUB_JULIA_VER', 'SOT_STUB_BIN_VER', 'JULIAUP_DEPOT_PATH', 'HTTPS_PROXY', 'ALL_PROXY', 'HOME')) {
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
  if (Environment.GetEnvironmentVariable("SOT_FAKE_NO_INSTALLED") != null && Array.IndexOf(a, "--installed") >= 0) { Console.Error.WriteLine("sotd update: unexpected argument --installed"); return 2; }
  if (Environment.GetEnvironmentVariable("SOT_FAKE_SLEEP") != null) { System.Threading.Thread.Sleep(60000); return 0; }
  string d = Environment.GetEnvironmentVariable("SOT_FAKE_DAEMON") ?? "2";
  string df = Environment.GetEnvironmentVariable("SOT_FAKE_DAEMON_FILE");
  if (df != null && File.Exists(df)) d = File.ReadAllText(df).Trim();
  if (name == "sotd" && a.Length > 0 && a[0] == "session-socket-path") { Console.WriteLine(@"\\.\pipe\sot-entry-test"); return 0; }
  if (name == "sotd" && a.Length > 0 && a[0] == "same-version") { int c = int.Parse(d); if (c == 3 || c == 4) Console.WriteLine("9.9.0"); if (c == 2) Console.Error.WriteLine("sotd same-version: no answer within 5 s"); if (c == 0) return 0; return c; }
  if (name == "sotd" && a.Length > 1 && a[0] == "topology" && a[1] == "sync") {
    string sy = Environment.GetEnvironmentVariable("SOT_FAKE_SYNC");
    if (sy == "refuse") { Console.Error.WriteLine("sotd topology sync: hub's hosts.toml does not list this box; add `[host.rig-pc]` on the hub"); return 1; }
    if (sy == "ok") { Console.WriteLine("ok"); return 0; } }
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
    # The kernel's own compat is the floor the install checks, so the fixture carries the real file.
    Copy-Item -LiteralPath (Join-Path $repoRoot 'julia\kernel\Project.toml') -Destination (Join-Path $origin 'julia\kernel\Project.toml') -Force
    function New-Release([string]$Ver) {
        Set-Content -LiteralPath (Join-Path $origin 'VERSION.txt') -Value $Ver
        & $git -C $origin add -A 2>&1 | Out-Null
        & $git -C $origin commit -q -m "release $Ver" 2>&1 | Out-Null
        if (-not $Ver.Contains('+')) { & $git -C $origin tag "v$Ver" 2>&1 | Out-Null }  # a pack of the line has no tag
        $commit = (& $git -C $origin rev-parse HEAD).Trim()
        # A '+@' version takes the commit's own short id, as a real build's does (the stage refuses any other).
        $Ver = $Ver -replace '\+@\d*$', ('+' + $commit.Substring(0, 9))
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
    $r98 = New-Release '9.9.8'; $r99 = New-Release '9.9.9'; $rdev = New-Release '9.9.9-rc9.14-dev+@'; $rdev2 = New-Release '9.9.9-rc9.14-dev+@2'
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
    # A Julia below the kernel's 1.12 floor refuses, as a real 1.11 does.
    $ver = $(if ($env:SOT_STUB_JULIA_VER) { $env:SOT_STUB_JULIA_VER } else { '1.12.0' })
    if ($ver -match '^1\.(\d+)' -and [int]$Matches[1] -lt 12) { [Console]::Error.WriteLine("ERROR: julia version requirement from Project.toml's compat section not satisfied"); exit 1 }
    $p = ($a | Where-Object { $_ -like '--project=*' } | Select-Object -First 1).Substring(10)
    New-Item -ItemType Directory -Force -Path $p | Out-Null
    Set-Content -LiteralPath (Join-Path $p 'Manifest.toml') -Value ''
}
if (($a -join ' ') -match 'print\(VERSION\)') {
    if ($env:SOT_STUB_JULIA_VER -eq 'none') { exit 1 }
    Write-Output $(if ($env:SOT_STUB_JULIA_VER) { $env:SOT_STUB_JULIA_VER } else { '1.12.0' })
}
exit 0
'@
    Set-Content -LiteralPath (Join-Path $stubs 'julia.cmd') -Value "@echo off`r`npowershell -NoProfile -ExecutionPolicy Bypass -File `"%~dp0julia.ps1`" %*`r`nexit /b %ERRORLEVEL%"
    Set-Content -LiteralPath (Join-Path $stubs 'jq.cmd') -Value "@echo off`r`nexit /b 0"
    # The julia the stage's prepare runs (SOT_JULIA_BIN): one that succeeds at everything, one that fails with a line.
    $juliaOk = Join-Path $stubs 'julia-standin.exe'
    Add-Type -TypeDefinition 'public class J { public static int Main(string[] a) { if (System.String.Join(" ", a).Contains("print(VERSION)")) { var v = System.Environment.GetEnvironmentVariable("SOT_STUB_BIN_VER"); if (v == "none") return 1; System.Console.Out.Write(v == null || v == "" ? "1.12.0" : v); } return 0; } }' -OutputAssembly $juliaOk -OutputType ConsoleApplication
    $juliaBad = Join-Path $stubs 'julia-failing.exe'
    Add-Type -TypeDefinition 'public class JB { public static int Main(string[] a) { System.Console.Error.WriteLine("julia stand-in: Pkg.instantiate failed"); return 1; } }' -OutputAssembly $juliaBad -OutputType ConsoleApplication
    # A PATH with no julia of any kind, for a box that has none.
    $stubsNone = Join-Path $entryRoot 'stubs-none'
    New-Item -ItemType Directory -Force -Path $stubsNone | Out-Null
    Set-Content -LiteralPath (Join-Path $stubsNone 'jq.cmd') -Value "@echo off`r`nexit /b 0"
    $pathNone = "$stubsNone;$env:SystemRoot\System32;$env:SystemRoot\System32\WindowsPowerShell\v1.0;$env:SystemRoot;C:\Program Files\Git\cmd;C:\Program Files\Git\mingw64\bin;C:\Program Files\Git\usr\bin"
    $basePath = $savedEnv['PATH']

    # -Short spells the profile in its 8.3 form, as a hosted runner's TEMP is (junctions report the long form).
    function New-Home([string]$Name, [switch]$Short) {
        $h = Join-Path $entryRoot $Name
        foreach ($d in @('Desktop', 'AppData\Roaming', 'AppData\Local', 'comm')) { New-Item -ItemType Directory -Force -Path (Join-Path $h $d) | Out-Null }
        if ($Short) { $h = ([string](New-Object -ComObject Scripting.FileSystemObject).GetFolder($h).ShortPath).TrimEnd('\') }
        $env:USERPROFILE = $h; $env:APPDATA = Join-Path $h 'AppData\Roaming'; $env:LOCALAPPDATA = Join-Path $h 'AppData\Local'
        $env:SOT_COMM_HOME = Join-Path $h 'comm'; $env:PATH = "$stubs;$basePath"; $env:HOME = $h
        foreach ($n in @('SSH_CONNECTION', 'SOT_INSTALL_TAG', 'SOT_INSTALL_IMPLICIT', 'SOT_INSTALL_REPO', 'SOT_FAKE_DAEMON', 'SOT_FAKE_DAEMON_FILE', 'SOT_FAKE_REPLS', 'SOT_FAKE_REPLS_RC', 'SOT_FAKE_SYNC', 'SOT_FAKE_SLEEP', 'SOT_FAKE_NO_INSTALLED', 'HTTPS_PROXY', 'ALL_PROXY', 'SOT_STUB_JULIA_VER', 'SOT_STUB_BIN_VER', 'JULIAUP_DEPOT_PATH')) { [Environment]::SetEnvironmentVariable($n, $null) }
        $env:SOT_INSTALL_ORIGIN = $origin; $env:SOT_FAKE_REAL = $SotdPath; $env:SOT_JULIA_BIN = $juliaOk
        $script:DeskSession = $true
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
    # A desk session is a non-zero session id (an ssh logon runs in session 0); the suite says which it is.
    function Get-EntrySessionId { if ($script:DeskSession) { return 3 } else { return 0 } }
    # The code id the installed launcher has, for the stand-in supervisors' start lines.
    function Get-TestCode([string]$ScriptsDir) { . (Join-Path $ScriptsDir 'sot-install-layout.ps1'); return (Get-SotLauncherCodeId -ScriptsDir $ScriptsDir) }
    function Invoke-Case([string]$Name, [scriptblock]$Body) { try { . $Body } catch { Check "$Name ran" $false $_.Exception.Message } }
    function Get-Pin([string]$Prefix) { return ((Get-Content -LiteralPath (Join-Path $Prefix 'install.json') -Raw) -match '"pinned"\s*:\s*true') }
    function Get-PinTail([string]$Ver) { return "; pinned at ${Ver}: automatic updates stay off until a run that names no version" }
    function Get-Json([string]$Prefix, [string]$Key) { return (Get-JsonString (Join-Path $Prefix 'install.json') $Key) }
    function Get-Tree([string]$Path) { return (@(Get-ChildItem -LiteralPath $Path -Recurse -Force -File -ErrorAction SilentlyContinue | ForEach-Object { "$($_.FullName.Substring($Path.Length)):$($_.Length)" } | Sort-Object) -join '|') }

    # 1. Fresh from a pack folder.
    $h1 = New-Home 'h1'
    $e = Invoke-Entry @('-Archive', $r98.Pack)
    Check '7a: a fresh install from a pack ends in one PASS line, the last, and the PASS line says the install is pinned' ("$($e.Rc) $($e.Verdicts) $($e.Last)" -ceq "0 1 PASS: Ship of Tools 9.9.8 at $($r98.Short) on this computer$(Get-PinTail '9.9.8')") "rc=$($e.Rc) verdicts=$($e.Verdicts) last='$($e.Last)'; $(($e.Out | Select-Object -Last 4) -join ' / ')"
    Check '7a: install.json names the version and the commit, and is pinned (a pack names its version)' (((Get-Json $h1.Prefix 'version') -eq '9.9.8') -and ((Get-Json $h1.Prefix 'commit') -eq $r98.Commit) -and ((Get-Content -LiteralPath (Join-Path $h1.Prefix 'install.json') -Raw) -match '"pinned"\s*:\s*true')) (Get-Content -LiteralPath (Join-Path $h1.Prefix 'install.json') -Raw)
    Check '7a: the record says this box runs its local daemon and a window (so the stage prepares Julia)' ((Get-Content -LiteralPath (Join-Path $h1.Prefix 'install.json') -Raw) -match '"daemon"\s*:\s*true,\s*"frontend"\s*:\s*true') (Get-Content -LiteralPath (Join-Path $h1.Prefix 'install.json') -Raw)
    $tgt = ''; try { $tgt = [string]@((Get-Item -LiteralPath (Join-Path $h1.Prefix 'repo\current') -Force).Target)[0] } catch { }
    Check '7a: repo\current is a junction to repo\versions\v9.9.8' ($tgt -like '*repo\versions\v9.9.8') $tgt
    $sc = ''; try { $sc = (New-Object -ComObject WScript.Shell).CreateShortcut((Join-Path $h1.Home 'Desktop\Ship of Tools.lnk')).Arguments } catch { }
    Check '7a: the Desktop shortcut targets repo\current\scripts\launch-sot.ps1' ($sc -like '*repo\current\scripts\launch-sot.ps1*') $sc
    Check '7a: comm is at the installed commit' ((Get-Content -LiteralPath (Join-Path $h1.Home 'comm\VERSION') -ErrorAction SilentlyContinue) -eq $r98.Short) 'the comm stamp'

    # 2. Update from that layout, nothing running, a desktop session: applied here.
    $e = Invoke-Entry @('-Archive', $r99.Pack)
    Check '7b: an update with nothing running applies here and ends in PASS' ("$($e.Rc) $($e.Verdicts) $($e.Last)" -ceq "0 1 PASS: Ship of Tools 9.9.9 at $($r99.Short) on this computer$(Get-PinTail '9.9.9')") "rc=$($e.Rc) last='$($e.Last)'; $(($e.Out | Select-Object -Last 6) -join ' / ')"
    $tgt = ''; try { $tgt = [string]@((Get-Item -LiteralPath (Join-Path $h1.Prefix 'repo\current') -Force).Target)[0] } catch { }
    Check '7b: the junction moved, no pointer remains, the previous version folder is kept' (($tgt -like '*repo\versions\v9.9.9') -and -not (Test-Path (Join-Path $h1.Prefix 'updates\pending-windows-x86_64.json')) -and (Test-Path (Join-Path $h1.Prefix 'repo\versions\v9.9.8'))) $tgt

    Check '7b: it says once, before the stage, that the stage can take many minutes' (@($e.Out | Where-Object { $_ -ceq 'staging and preparing v9.9.9 (this can take many minutes)' }).Count -eq 1) (($e.Out | Select-Object -First 8) -join ' / ')
    $prepared = @(Get-ChildItem -LiteralPath (Join-Path $h1.Prefix 'updates') -Recurse -Filter 'prepared.json' -ErrorAction SilentlyContinue | Where-Object { $_.FullName -like '*v9.9.9*' })
    Check '7b: the stage prepared the Julia environments (the record names no frontend-only box)' ($prepared.Count -ge 1 -and ([System.IO.File]::ReadAllText($prepared[0].FullName) -match '"julia_instantiated"\s*:\s*true')) "prepared.json files: $($prepared.Count)"

    # 3. A repeat on a current box is a repair run: no stage, no restart, PASS. Naming the same pack stages and applies again.
    $before = Get-Tree (Join-Path $h1.Prefix 'updates')
    $env:SOT_INSTALL_TAG = 'v9.9.9'; $env:SOT_INSTALL_IMPLICIT = '1'
    $fetchLog = New-Object System.Collections.ArrayList
    $serveImpl = { param([string]$Url, [string]$Path) $f = Split-Path $Url -Leaf; $src = Join-Path $script:servePack $f; if (-not (Test-Path -LiteralPath $src)) { return [pscustomobject]@{ Ok = $false; Code = 22 } }; Copy-Item -LiteralPath $src -Destination $Path -Force; return [pscustomobject]@{ Ok = $true; Code = 0 } }
    $script:curlFileImpl = $serveImpl
    $textLog = New-Object System.Collections.ArrayList
    $script:curlTextImpl = { param([string]$Url, [bool]$Head) return [pscustomobject]@{ Ok = $false; Code = 22; Text = '' } }
    function Invoke-CurlFile([string]$Url, [string]$Path) { [void]$fetchLog.Add($Url); return (& $script:curlFileImpl $Url $Path) }
    function Invoke-CurlText([string]$Url, [switch]$Head) { [void]$textLog.Add($Url); return (& $script:curlTextImpl $Url ([bool]$Head)) }
    $script:servePack = $r99.Pack
    $e = Invoke-Entry @()
    Check '7c: an implicit run at the installed version is a repair run ending in PASS' ("$($e.Rc) $($e.Verdicts)" -ceq '0 1' -and $e.Last -like 'PASS: *9.9.9*' -and ($e.Out -match 'a repair run').Count -eq 1) "rc=$($e.Rc) last='$($e.Last)'"
    Check '7c: the three downloads of a release are these URLs, as whole strings' ((@($fetchLog) -join ' ') -ceq 'https://github.com/kalidke/ship-of-tools/releases/download/v9.9.9/SHA256SUMS https://github.com/kalidke/ship-of-tools/releases/download/v9.9.9/sot-9.9.9-windows-x86_64.zip https://github.com/kalidke/ship-of-tools/releases/download/v9.9.9/COMMIT') (@($fetchLog) -join ' ')
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
        # The window's supervisor runs launcher code that is not the installed one: the run is a repair run (the pin is
        # written, nothing is staged or signalled) and says the window still runs the old launcher.
        $e = Invoke-Entry @('-Archive', $r99.Pack)
        Check '7c2: an explicit run of the installed version is a repair run' (($e.Out -match 'a repair run').Count -eq 1 -and $e.Rc -eq 0 -and $e.Verdicts -eq 1) "rc=$($e.Rc) last='$($e.Last)'"
        Check '7c2: it wrote the pin' (Get-Pin $h1.Prefix) 'no pin recorded'
        Check '7c2: it staged nothing, wrote no sentinel and touched no window' (((Get-Tree (Join-Path $h1.Prefix 'updates')) -ceq $before) -and -not (Test-Path (Join-Path $h1.Home 'AppData\Local\sot\relaunch.request')) -and -not $procP.HasExited) 'something was staged or signalled'
        Check '7c2: a supervisor on another launcher code is ONE STEP LEFT, quit and start from the shortcut, with the pin in the line' ($e.Last -ceq "ONE STEP LEFT: quit Ship of Tools, choose Keep, and start it from the shortcut; the window runs an older launcher$(Get-PinTail '9.9.9')") "last='$($e.Last)'"
        $code99 = Get-TestCode (Join-Path $h1.Prefix 'repo\versions\v9.9.9\scripts')
        Set-Content -LiteralPath (Join-Path $logDirP 'supervisor.log') -Value "2026-10-17T00:00:00  pid=$($procP.Id)  supervisor start (relaunched=False) depth=0 ws=1MB code=$code99"
        $e = Invoke-Entry @('-Archive', $r99.Pack)
        Check '7c2: a supervisor on the installed launcher code is PASS, with the pin in the line' ($e.Last -ceq "PASS: Ship of Tools 9.9.9 at $($r99.Short) on this computer$(Get-PinTail '9.9.9')") "rc=$($e.Rc) last='$($e.Last)'"
        Set-Content -LiteralPath (Join-Path $logDirP 'supervisor.log') -Value "2026-10-17T00:00:00  pid=$($procP.Id)  supervisor start (relaunched=False) depth=0 ws=1MB"
        $e = Invoke-Entry @('-Archive', $r99.Pack)
        Check '7c2: a supervisor that logs no code is not the installed launcher either' ($e.Last -like 'ONE STEP LEFT: quit Ship of Tools, choose Keep, and start it from the shortcut; the window runs an older launcher*') "last='$($e.Last)'"
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
    $e = Invoke-Entry @('-Version', 'v9.9.8')
    Check '7d: a named older version says older and goes on' (($e.Out -match '\(older\)').Count -eq 1 -and $e.Last -like 'PASS: *9.9.8*') "rc=$($e.Rc) last='$($e.Last)'"
    Check '7d: and the install is pinned at the person''s choice' ((Get-Content -LiteralPath (Join-Path $h1.Prefix 'install.json') -Raw) -match '"pinned"\s*:\s*true') 'no pin recorded'

    # 5. A build with a + stops, with the line that sends the person to a pack.
    $h5 = New-Home 'h5'
    New-Item -ItemType Directory -Force -Path $h5.Prefix | Out-Null
    [System.IO.File]::WriteAllText((Join-Path $h5.Prefix 'install.json'), '{"schema":1,"prefix":"x","version":"9.9.9-dev+abc","tag":"v9.9.9-dev+abc","commit":"0"}')
    $e = Invoke-Entry @()
    Check '7e: a build with a + stops before any install (exit 2)' ("$($e.Rc) $($e.Verdicts)" -ceq '2 1' -and $e.Last -like 'FAIL: target: this computer runs 9.9.9-dev+abc, a build of the line, not a release: install a pack of the line with -Archive <folder>; could not read the newest release to say which way it moves this computer; ask the person who runs it which they want') "rc=$($e.Rc) last='$($e.Last)'"

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
        Check '7h: the ONE STEP LEFT line ends with the pin' ($e.Last.EndsWith((Get-PinTail '9.9.9'))) "last='$($e.Last)'"
        Set-Content -LiteralPath (Join-Path $logDir 'supervisor.log') -Value "2026-10-17T00:00:00  pid=$($proc.Id)  supervisor start (relaunched=False) depth=0 ws=1MB code=abc-def"
        $env:SOT_INSTALL_CONVERGE_WAIT_S = '3'
        $e = Invoke-Entry @('-Archive', $r99.Pack)
        Check '7h: a supervisor with code= gets the converge sentinel, and one that never reports ends in FAIL apply with the sentinel removed' ($e.Rc -eq 1 -and ($e.Out -match 'converge requested').Count -eq 1 -and $e.Last -like 'FAIL: apply: *' -and -not (Test-Path (Join-Path $h7.Home 'AppData\Local\sot\relaunch.request'))) "rc=$($e.Rc) last='$($e.Last)'"
    } finally {
        Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath (Join-Path $logDir 'launcher.pid') -Force -ErrorAction SilentlyContinue
        [Environment]::SetEnvironmentVariable('SOT_INSTALL_CONVERGE_WAIT_S', $null)
    }
    $script:DeskSession = $false; $env:SOT_FAKE_DAEMON = '3'
    $e = Invoke-Entry @('-Archive', $r99.Pack, '-RestartDaemon')
    Check '7i: nothing running over ssh leaves the update armed with the desk step, and writes no sentinel' ($e.Rc -eq 0 -and $e.Last -like 'ONE STEP LEFT: start Ship of Tools at the desk; the start applies v9.9.9*' -and -not (Test-Path (Join-Path $h7.Home 'AppData\Local\sot\relaunch.request'))) "rc=$($e.Rc) last='$($e.Last)'"
    Check '7i: the install still runs the old version (nothing was applied)' ((Get-Json $h7.Prefix 'version') -eq '9.9.8') (Get-Json $h7.Prefix 'version')
    $script:DeskSession = $true

    # 9. A daemon the converge restarted answers as the target within the bound, else FAIL.
    $h9 = New-Home 'h9'
    [void](Invoke-Entry @('-Archive', $r99.Pack))
    $env:SOT_FAKE_DAEMON = '3'; $env:SOT_INSTALL_DAEMON_WAIT_S = '2'
    $script:Done = $false; $script:ExpectDaemon = $true; $script:Steps = @()
    $verdictText = ''
    try { Test-EndState $h9.Prefix ([pscustomobject]@{ Version = '9.9.9'; Tag = 'v9.9.9'; Commit = $r99.Commit; Bin = '' }) (Join-Path $h9.Prefix 'bin\sotd.exe') '\\.\pipe\sot-entry-test' (Join-Path $h9.Home 'AppData\Local\sot\logs') } catch { }
    Check '7j: a daemon that never answers as the target is a FAIL daemon, not a PASS' ($script:Done -and $script:Rc -eq 1) 'no verdict'
    $script:ExpectDaemon = $false

    # 10. The URLs the entry builds, as whole strings, against recorders: nothing is fetched. The release slug is
    # kalidke/ship-of-tools unless SOT_INSTALL_REPO says another.
    Invoke-Case '7k' {
        $script:curlFileImpl = { param([string]$Url, [string]$Path) return [pscustomobject]@{ Ok = $false; Code = 22 } }
        $hk = New-Home 'k1'
        New-Item -ItemType Directory -Force -Path $hk.Prefix | Out-Null
        [System.IO.File]::WriteAllText((Join-Path $hk.Prefix 'install.json'), '{"schema":1,"prefix":"x","version":"0.6.6-rc9.13","tag":"v0.6.6-rc9.13","commit":"0"}')
        $script:curlTextImpl = { param([string]$Url, [bool]$Head) return [pscustomobject]@{ Ok = $true; Code = 0; Text = '[{"tag_name": "v0.6.6-rc9.14"}]' } }
        $fetchLog.Clear(); $textLog.Clear()
        $e = Invoke-Entry @()
        Check '7k: an implicit run on an rc lists the newest release of the repo and fetches that tag''s install.ps1' (((@($textLog) -join ' ') -ceq 'https://api.github.com/repos/kalidke/ship-of-tools/releases?per_page=1') -and ((@($fetchLog) -join ' ') -ceq 'https://raw.githubusercontent.com/kalidke/ship-of-tools/v0.6.6-rc9.14/scripts/install.ps1')) "text=$(@($textLog) -join ' ') fetch=$(@($fetchLog) -join ' ')"
        Check '7k: and a release with no install.ps1 is a FAIL target with that URL in it' ($e.Rc -eq 1 -and $e.Last -ceq 'FAIL: target: v0.6.6-rc9.14 has no scripts/install.ps1 (or https://raw.githubusercontent.com/kalidke/ship-of-tools/v0.6.6-rc9.14/scripts/install.ps1 could not be fetched): install that release by its own procedure, docs/INSTALL-AGENT.md at v0.6.6-rc9.14') "last='$($e.Last)'"
        # GitHub's anonymous rate limit on the listing: a 403 says what it is, when to retry and that -Version needs no listing.
        $tokSave = @($env:GITHUB_TOKEN, $env:GH_TOKEN); $env:GITHUB_TOKEN = $null; $env:GH_TOKEN = $null
        $script:curlTextImpl = { param([string]$Url, [bool]$Head) return [pscustomobject]@{ Ok = $false; Code = 22; Text = ''; Http = '403' } }
        $e = Invoke-Entry @()
        $env:GITHUB_TOKEN = $tokSave[0]; $env:GH_TOKEN = $tokSave[1]
        Check '7k: a 403 from the release listing is a FAIL target that names the refusal, the retry and -Version' ($e.Rc -eq 1 -and $e.Last -ceq 'FAIL: target: could not list the releases of kalidke/ship-of-tools: GitHub refused the anonymous request (HTTP 403, its rate limit for this network address); retry in an hour, set GITHUB_TOKEN to send a token, or name the release with -Version <tag>, which needs no listing') "rc=$($e.Rc) last='$($e.Last)'"
        $hk2 = New-Home 'k2'
        $script:curlTextImpl = { param([string]$Url, [bool]$Head) return [pscustomobject]@{ Ok = $true; Code = 0; Text = 'https://github.com/kalidke/ship-of-tools/releases/tag/v0.6.6' } }
        $fetchLog.Clear(); $textLog.Clear()
        $e = Invoke-Entry @()
        Check '7k: an implicit run on a fresh box resolves the latest stable release and fetches its install.ps1' (((@($textLog) -join ' ') -ceq 'https://github.com/kalidke/ship-of-tools/releases/latest') -and ((@($fetchLog) -join ' ') -ceq 'https://raw.githubusercontent.com/kalidke/ship-of-tools/v0.6.6/scripts/install.ps1')) "text=$(@($textLog) -join ' ') fetch=$(@($fetchLog) -join ' ')"
        $fetchLog.Clear(); $textLog.Clear()
        $e = Invoke-Entry @('-Version', 'v0.6.6-rc9.14')
        Check '7k: a -Version run asks nothing and fetches that tag''s install.ps1' (($textLog.Count -eq 0) -and ((@($fetchLog) -join ' ') -ceq 'https://raw.githubusercontent.com/kalidke/ship-of-tools/v0.6.6-rc9.14/scripts/install.ps1')) "text=$(@($textLog) -join ' ') fetch=$(@($fetchLog) -join ' ')"
        # The origin a fresh run clones from, with no SOT_INSTALL_ORIGIN.
        $hk3 = New-Home 'k3'
        [Environment]::SetEnvironmentVariable('SOT_INSTALL_ORIGIN', $null)
        $script:cloneOrigin = $null
        function Initialize-Base([string]$Base, [string]$Origin, $Pack) { $script:cloneOrigin = $Origin; Stop-Entry 'checkout' 'recorded' }
        $e = Invoke-Entry @('-Archive', $r98.Pack)
        Check '7k: a fresh run clones from github.com/kalidke/ship-of-tools' ($script:cloneOrigin -ceq 'https://github.com/kalidke/ship-of-tools') "origin='$($script:cloneOrigin)' last='$($e.Last)'"
        # SOT_INSTALL_REPO names another repo.
        $env:SOT_INSTALL_REPO = 'fork/x'
        $slug = @(& powershell.exe -NoProfile -ExecutionPolicy Bypass -Command ". '$entryScript'; `$script:Repo")
        Check '7k: SOT_INSTALL_REPO names the repo the downloads and the clone come from' (($slug -join '') -ceq 'fork/x') ($slug -join '|')
        $script:curlFileImpl = $serveImpl
        $script:curlTextImpl = { param([string]$Url, [bool]$Head) return [pscustomobject]@{ Ok = $false; Code = 22; Text = '' } }
    }

    # 11. The prelude end to end: the parent fetches the target tag's install.ps1 (the real one, served), runs it as a
    # child, relays its lines and its exit code; the child stops at its first network call (a dead proxy, so nothing
    # leaves this computer) and names the URL it built.
    Invoke-Case '7l' {
        $hl = New-Home 'l1'
        $env:HTTPS_PROXY = 'http://127.0.0.1:9'; $env:ALL_PROXY = 'http://127.0.0.1:9'
        $script:curlTextImpl = { param([string]$Url, [bool]$Head) return [pscustomobject]@{ Ok = $true; Code = 0; Text = 'https://github.com/kalidke/ship-of-tools/releases/tag/v9.9.9' } }
        $script:curlFileImpl = { param([string]$Url, [string]$Path) if ($Url -like '*/scripts/install.ps1') { Copy-Item -LiteralPath $entryScript -Destination $Path -Force; return [pscustomobject]@{ Ok = $true; Code = 0 } }; return [pscustomobject]@{ Ok = $false; Code = 22 } }
        $e = Invoke-Entry @()
        Check '7l: the prelude runs the served installer, relays its one verdict and its exit code' ($e.Rc -eq 1 -and $e.Verdicts -eq 1 -and $e.Last -like 'FAIL: fetch: could not download https://github.com/kalidke/ship-of-tools/releases/download/v9.9.9/SHA256SUMS (curl exit *)') "rc=$($e.Rc) verdicts=$($e.Verdicts) last='$($e.Last)'"
        $script:curlFileImpl = $serveImpl
        $script:curlTextImpl = { param([string]$Url, [bool]$Head) return [pscustomobject]@{ Ok = $false; Code = 22; Text = '' } }
        [Environment]::SetEnvironmentVariable('HTTPS_PROXY', $null); [Environment]::SetEnvironmentVariable('ALL_PROXY', $null)
    }
    # Ctrl+C while the prelude's child runs: the child prints the one verdict, so the parent prints none of its own.
    Invoke-Case '7l2' {
        $hl2 = New-Home 'l2'
        $pidFile = Join-Path $entryRoot 'sleeper.pid'
        $sleeper = Join-Path $entryRoot 'prelude-sleeper.ps1'
        Remove-Item -LiteralPath $pidFile -Force -ErrorAction SilentlyContinue
        Set-Content -LiteralPath $sleeper -Value "[System.IO.File]::WriteAllText('$pidFile', `$PID.ToString()); Start-Sleep -Seconds 40"
        $log2 = Join-Path $entryRoot 'interrupt2.log'
        Remove-Item -LiteralPath $log2 -Force -ErrorAction SilentlyContinue
        $ps2 = [powershell]::Create()
        $null = $ps2.AddScript({ param($f, $log, $sleeper)
            . $f
            function Write-EntryLine([string]$Text) { [System.IO.File]::AppendAllText($log, $Text + "`n") }
            function Invoke-CurlText([string]$Url, [switch]$Head) { return [pscustomobject]@{ Ok = $true; Code = 0; Text = 'https://github.com/x/y/releases/tag/v9.9.9' } }
            function Invoke-CurlFile([string]$Url, [string]$Path) { Copy-Item -LiteralPath $sleeper -Destination $Path -Force; return [pscustomobject]@{ Ok = $true; Code = 0 } }
            [void](Invoke-InstallEntry @())
        }).AddArgument($entryScript).AddArgument($log2).AddArgument($sleeper)
        $async2 = $ps2.BeginInvoke()
        $t0 = Get-Date
        while (-not (Test-Path -LiteralPath $pidFile) -and ((Get-Date) - $t0).TotalSeconds -lt 60) { Start-Sleep -Milliseconds 500 }
        Start-Sleep -Seconds 1
        $stopAsync = $ps2.BeginStop($null, $null)
        $kid = 0; try { $kid = [int](Get-Content -LiteralPath $pidFile) } catch { }
        if ($kid -gt 0) { Stop-Process -Id $kid -Force -ErrorAction SilentlyContinue }
        [void]$stopAsync.AsyncWaitHandle.WaitOne(30000)
        try { [void]$ps2.EndInvoke($async2) } catch { }
        $ps2.Dispose()
        $out2 = @(if (Test-Path -LiteralPath $log2) { [System.IO.File]::ReadAllLines($log2) })
        Check '7l2: an interrupt while the prelude''s child runs adds no verdict of the parent''s (the child prints the one)' (@($out2 | Where-Object { $_ -match '^(PASS|FAIL|ONE STEP LEFT): ' }).Count -eq 0) ($out2 -join ' / ')
    }

    # 12. An armed pointer does not survive the pin-in-place run (a repair run drops it): PASS, the pointer gone, the
    # version unchanged by the next start's apply.
    Invoke-Case '7m' {
        $hm = New-Home 'm1'
        [void](Invoke-Entry @('-Archive', $r98.Pack))
        $eap = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
        $arm = @(& $SotdPath update stage $r99.Pack --prefix $hm.Prefix 2>&1); $armRc = $LASTEXITCODE
        $ErrorActionPreference = $eap
        $ptr = Join-Path $hm.Prefix 'updates\pending-windows-x86_64.json'
        Check '7m: fixture: the daily check armed 9.9.9 over 9.9.8' ($armRc -eq 0 -and (Test-Path $ptr)) "rc=$armRc $((@($arm) | Select-Object -Last 2) -join ' / ')"
        $env:SOT_FAKE_DAEMON = '0'
        $e = Invoke-Entry @('-Archive', $r98.Pack)
        Check '7m: the pin-in-place run is PASS with the pointer gone and the pin in the line' ($e.Rc -eq 0 -and $e.Last -ceq "PASS: Ship of Tools 9.9.8 at $($r98.Short) on this computer$(Get-PinTail '9.9.8')" -and -not (Test-Path $ptr) -and (Get-Pin $hm.Prefix)) "rc=$($e.Rc) last='$($e.Last)' pointer=$(Test-Path $ptr)"
        $ErrorActionPreference = 'Continue'
        $ap = @(& powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $hm.Prefix 'repo\versions\v9.9.8\scripts\sot-apply.ps1') -Prefix $hm.Prefix 2>&1)
        $ErrorActionPreference = $eap
        Check '7m: the next start''s apply moves nothing' (((Get-Json $hm.Prefix 'version') -eq '9.9.8') -and -not (@($ap | Where-Object { "$_" -match 'APPLIED' }).Count)) "version=$(Get-Json $hm.Prefix 'version')"
    }

    # 13. A daemon that cannot be told is not "none": a pipe, or a process of bin\sotd.exe, says one runs.
    Invoke-Case '7n' {
        $hn = New-Home 'n1'
        [void](Invoke-Entry @('-Archive', $r98.Pack))
        $binSotd = Join-Path $hn.Prefix 'bin\sotd.exe'
        $pipeN = '\\.\pipe\sot-entry-test'
        $states = foreach ($c in @('0', '3', '4', '2')) { $env:SOT_FAKE_DAEMON = $c; Get-DaemonState $binSotd $pipeN $binSotd }
        Check '7n: same-version 0 is current, 3 and 4 are other (no held state), 2 with no daemon is none' (($states -join ' ') -ceq 'current other other none') ($states -join ' ')
        $env:SOT_FAKE_DAEMON = '2'
        $pack98 = [pscustomobject]@{ Version = '9.9.8'; Tag = 'v9.9.8'; Commit = $r98.Commit; Bin = '' }
        $srv = New-Object System.IO.Pipes.NamedPipeServerStream('sot-entry-test')
        try {
            $st = Get-DaemonState $binSotd $pipeN $binSotd
            Check '7n: a pipe that exists and a daemon that cannot be told is unknown, with sotd''s line kept' ($st -ceq 'unknown' -and $script:DaemonWhy -like '*no answer within 5 s*') "state=$st why='$($script:DaemonWhy)'"
            $script:Done = $false; $script:ExpectDaemon = $false; $script:Steps = @()
            try { Test-EndState $hn.Prefix $pack98 $binSotd $pipeN (Join-Path $hn.Home 'AppData\Local\sot\logs') } catch { }
            Check '7n: the end check is a FAIL daemon beside a daemon it cannot tell, never a PASS' ($script:Done -and $script:Rc -eq 1) "done=$($script:Done) rc=$($script:Rc)"
        } finally { $srv.Dispose() }
        $env:SOT_FAKE_SLEEP = '1'
        $held = Start-Process -FilePath $binSotd -PassThru -WindowStyle Hidden
        [Environment]::SetEnvironmentVariable('SOT_FAKE_SLEEP', $null)
        try {
            Start-Sleep -Seconds 2
            $st = Get-DaemonState $binSotd $pipeN $binSotd
            Check '7n: a process of bin\sotd.exe that cannot be told is unknown too' ($st -ceq 'unknown') "state=$st"
            $before = Get-Tree $hn.Prefix
            $e = Invoke-Entry @('-Archive', $r99.Pack)
            Check '7n: an update beside it fails closed (exit 2) with sotd''s own line, one verdict' ($e.Rc -eq 2 -and $e.Verdicts -eq 1 -and $e.Last -like 'FAIL: daemon: *no answer within 5 s*') "rc=$($e.Rc) last='$($e.Last)'"
            Check '7n: nothing under the prefix changed and the old daemon process runs' (((Get-Tree $hn.Prefix) -ceq $before) -and -not $held.HasExited) 'the prefix changed or the process ended'
            $env:SOT_FAKE_DAEMON = '0'; $env:SOT_FAKE_REPLS = 'repl Rig PC'; $env:SOT_FAKE_REPLS_RC = '3'
            $e = Invoke-Entry @('-Archive', $r99.Pack)
            Check '7n: a daemon that answers as the target is asked too when REPLs are live' ($e.Rc -eq 2 -and $e.Last -like 'FAIL: daemon: *Rig PC*') "rc=$($e.Rc) last='$($e.Last)'"
        } finally { Stop-Process -Id $held.Id -Force -ErrorAction SilentlyContinue }
    }

    # 14. The enroll check: a hub whose hosts.toml does not list this box ends the run, in U1's words.
    Invoke-Case '7o' {
        $ho = New-Home 'o1'
        $env:SOT_FAKE_SYNC = 'refuse'
        $e = Invoke-Entry @('-Archive', $r98.Pack, '-Hub', 'rig-hub')
        Check '7o: a hub that does not list this computer is FAIL enroll naming the command for the hub, and the pin the run wrote' ($e.Rc -eq 1 -and $e.Verdicts -eq 1 -and $e.Last -ceq "FAIL: enroll: the hosts.toml on 'rig-hub' does not list this computer ('rig-pc'), so the window would open with no hosts: on 'rig-hub' run  sotd topology set add rig-pc --frontend  (the sotd of its install), then run this installer again$(Get-PinTail '9.9.8')") "rc=$($e.Rc) last='$($e.Last)'; $(@($e.Out | Where-Object { $_ -match 'sync|topology|hub' }) -join ' / ')"
        $ho2 = New-Home 'o2'
        $env:SOT_FAKE_SYNC = 'ok'
        $e = Invoke-Entry @('-Archive', $r98.Pack, '-Hub', 'rig-hub')
        Check '7o: a hub that lists it is PASS' ($e.Rc -eq 0 -and $e.Last -like 'PASS: *') "rc=$($e.Rc) last='$($e.Last)'"
    }

    # 15. Julia: a box without one is a supported state. The stage skips Julia (the launcher instantiates at its first
    # start) and the run ends ONE STEP LEFT naming winget; a failed stage carries sotd's last line only.
    Invoke-Case '7p' {
        $hp = New-Home 'pj1'
        $env:PATH = $pathNone; [Environment]::SetEnvironmentVariable('SOT_JULIA_BIN', $null)
        $e = Invoke-Entry @('-Archive', $r98.Pack)
        Check '7p: a fresh install on a box with no Julia ends ONE STEP LEFT naming winget' ($e.Rc -eq 0 -and $e.Last -like 'ONE STEP LEFT: install Julia (winget install --id Julialang.Juliaup -e)*') "rc=$($e.Rc) last='$($e.Last)'"
        $e = Invoke-Entry @('-Archive', $r99.Pack)
        Check '7p: an update on a box with no Julia stages without it, applies and ends ONE STEP LEFT naming winget' ($e.Rc -eq 0 -and (Get-Json $hp.Prefix 'version') -eq '9.9.9' -and $e.Last -like 'ONE STEP LEFT: install Julia (winget install --id Julialang.Juliaup -e)*') "rc=$($e.Rc) version=$(Get-Json $hp.Prefix 'version') last='$($e.Last)'"
        $hp2 = New-Home 'pj2'
        [void](Invoke-Entry @('-Archive', $r98.Pack))
        Set-InstallPin (Join-Path $hp2.Prefix 'install.json') $false
        $env:SOT_JULIA_BIN = $juliaBad
        $e = Invoke-Entry @('-Archive', $r99.Pack)
        Check '7p: a stage that fails carries one line of sotd''s (its last), not its tracing' ($e.Rc -eq 1 -and $e.Last -like 'FAIL: stage: *' -and $e.Last -notlike '* / *' -and $e.Last -notlike '*INFO*') "rc=$($e.Rc) last='$($e.Last)'"
        Check '7p: and a stage that fails leaves the pin as it was (the run moved nothing)' (-not (Get-Pin $hp2.Prefix)) 'the failed named run left a pin on an install it did not move'
    }

    # 16. Path (a) to the end: a supervisor that logs code= takes the converge sentinel, applies, restarts the daemon
    # (the stand-in answers as the target after) and logs DONE; the run waits for both and ends PASS.
    Invoke-Case '7q' {
        $hq = New-Home 'q1'
        [void](Invoke-Entry @('-Archive', $r98.Pack))
        $dfile = Join-Path $hq.Home 'daemon.code'
        Set-Content -LiteralPath $dfile -Value '3'
        $env:SOT_FAKE_DAEMON_FILE = $dfile
        $logDirQ = Join-Path $hq.Home 'AppData\Local\sot\logs'
        New-Item -ItemType Directory -Force -Path $logDirQ | Out-Null
        $supQ = Join-Path $entryRoot 'launch-sot.ps1'
        $supLogQ = Join-Path $logDirQ 'supervisor.log'
        $sentQ = Join-Path $hq.Home 'AppData\Local\sot\relaunch.request'
        $applyQ = Join-Path $hq.Prefix 'repo\versions\v9.9.8\scripts\sot-apply.ps1'
        $newScripts = Join-Path $hq.Prefix 'repo\versions\v9.9.9\scripts'
        Set-Content -LiteralPath $supQ -Value @"
`$t0 = Get-Date
while (((Get-Date) - `$t0).TotalSeconds -lt 150) {
    if (Test-Path -LiteralPath '$sentQ') {
        Remove-Item -LiteralPath '$sentQ' -Force
        `$out = & powershell.exe -NoProfile -ExecutionPolicy Bypass -File '$applyQ' -Prefix '$($hq.Prefix)' 2>&1
        foreach (`$l in `$out) { "`$(Get-Date -Format o)  pid=`$PID  `$l" | Out-File -FilePath '$supLogQ' -Append -Encoding utf8 }
        . '$newScripts\sot-install-layout.ps1'
        `$c = Get-SotLauncherCodeId -ScriptsDir '$newScripts'
        "`$(Get-Date -Format o)  pid=`$PID  supervisor start (relaunched=True) depth=1 ws=1MB code=`$c" | Out-File -FilePath '$supLogQ' -Append -Encoding utf8
        Set-Content -LiteralPath '$dfile' -Value '0'
        "`$(Get-Date -Format o)  pid=`$PID  status: DONE" | Out-File -FilePath '$supLogQ' -Append -Encoding utf8
    }
    Start-Sleep -Milliseconds 500
}
"@
        $procQ = Start-Process -FilePath powershell.exe -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $supQ) -PassThru -WindowStyle Hidden
        try {
            Start-Sleep -Seconds 2
            Set-Content -LiteralPath (Join-Path $logDirQ 'launcher.pid') -Value "$($procQ.Id)"
            "2026-10-17T00:00:00  pid=$($procQ.Id)  supervisor start (relaunched=False) depth=0 ws=1MB code=old-code" | Out-File -FilePath $supLogQ -Encoding utf8
            $env:SOT_INSTALL_CONVERGE_WAIT_S = '90'
            $e = Invoke-Entry @('-Archive', $r99.Pack)
            Check '7q: the converge applies, the daemon answers as the target, and the run ends PASS (the sentinel consumed)' ($e.Rc -eq 0 -and $e.Verdicts -eq 1 -and $e.Last -ceq "PASS: Ship of Tools 9.9.9 at $($r99.Short) on this computer$(Get-PinTail '9.9.9')" -and (Get-Json $hq.Prefix 'version') -eq '9.9.9' -and -not (Test-Path $sentQ)) "rc=$($e.Rc) last='$($e.Last)'; $(($e.Out | Select-Object -Last 5) -join ' / ')"
        } finally {
            Stop-Process -Id $procQ.Id -Force -ErrorAction SilentlyContinue
            Remove-Item -LiteralPath (Join-Path $logDirQ 'launcher.pid') -Force -ErrorAction SilentlyContinue
            [Environment]::SetEnvironmentVariable('SOT_INSTALL_CONVERGE_WAIT_S', $null)
        }
    }

    # 17. The contract with the launcher: the four log lines this entry reads are the ones the launcher and the apply
    # write. A wording change on either side fails here, not as a 30-minute wait in the field.
    Invoke-Case '7r' {
        $launch = [System.IO.File]::ReadAllText((Join-Path $repoRoot 'scripts\launch-sot.ps1'))
        $apply = [System.IO.File]::ReadAllText((Join-Path $repoRoot 'scripts\sot-apply.ps1'))
        $entry = [System.IO.File]::ReadAllText($entryScript)
        Check '7r: launch-sot.ps1 logs "supervisor start (relaunched=...) ... code=<id>", which Get-Supervisor reads' ($launch.Contains('Write-SupLog "supervisor start (relaunched=$Relaunched) depth=') -and $launch.Contains(' code=$(if ($script:launcherCodeId)') -and $entry.Contains('supervisor start') -and $entry.Contains("code=(\S+)")) 'a side changed'
        Check '7r: launch-sot.ps1 logs "status: DONE" (Set-LaunchStatus ''DONE''), which the converge wait reads' ($launch.Contains("Set-LaunchStatus 'DONE'") -and $launch.Contains('Write-SupLog "status: $Message"') -and $entry.Contains("status: DONE")) 'a side changed'
        Check '7r: sot-apply.ps1 logs "APPLIED <tag>", which the converge wait reads' ($apply.Contains('Write-ApplyLog "APPLIED $tag') -and $entry.Contains('sot-apply: APPLIED')) 'a side changed'
        Check '7r: launch-sot.ps1 writes launcher.pid in the log folder, which Get-Supervisor reads' ($launch.Contains("Join-Path `$logDir 'launcher.pid'") -and $entry.Contains("'launcher.pid'")) 'a side changed'
    }

    # 18. Smaller: option errors, the prefix, a junction that does not resolve, a named run of the installed version.
    Invoke-Case '7s' {
        $e = Invoke-Entry @('-Version', 'v9.9.8', '-Archive', $r98.Pack)
        Check '7s: -Version with -Archive is refused (exit 2), one verdict' ($e.Rc -eq 2 -and $e.Verdicts -eq 1 -and $e.Last -like 'FAIL: options: *-Archive*-Version*') "rc=$($e.Rc) last='$($e.Last)'"
        $cwd = (Get-Location).Path
        Check '7s: a relative prefix becomes absolute and loses a trailing backslash' (((Resolve-PrefixPath 'x\y\') -ceq (Join-Path $cwd 'x\y')) -and ((Resolve-PrefixPath 'C:\a\b\') -ceq 'C:\a\b')) "got '$(Resolve-PrefixPath 'x\y\')' and '$(Resolve-PrefixPath 'C:\a\b\')'"
        $hs = New-Home 's1'
        [void](Invoke-Entry @('-Archive', $r98.Pack))
        $junction = Join-Path $hs.Prefix 'repo\current'
        [System.IO.Directory]::Delete($junction)
        New-Item -ItemType Directory -Force -Path $junction | Out-Null
        $script:Done = $false; $script:ExpectDaemon = $false; $script:Steps = @()
        try { Test-EndState $hs.Prefix ([pscustomobject]@{ Version = '9.9.8'; Tag = 'v9.9.8'; Commit = $r98.Commit; Bin = '' }) (Join-Path $hs.Prefix 'bin\sotd.exe') '\\.\pipe\sot-entry-test' (Join-Path $hs.Home 'AppData\Local\sot\logs') } catch { }
        Check '7s: a repo\current that is a plain folder (no junction target) is a FAIL checkout' ($script:Done -and $script:Rc -eq 1) "done=$($script:Done) rc=$($script:Rc)"
        # A named run of the version already installed pins without downloading the release.
        $hz = New-Home 'z1'
        [void](Invoke-Entry @('-Archive', $r98.Pack))
        $env:SOT_INSTALL_TAG = 'v9.9.8'
        $fetchLog.Clear(); $script:servePack = $r98.Pack
        $e = Invoke-Entry @('-Version', 'v9.9.8')
        Check '7s: a named run of the installed version downloads no release asset and ends PASS with the pin in the line' ($fetchLog.Count -eq 0 -and $e.Rc -eq 0 -and $e.Last -ceq "PASS: Ship of Tools 9.9.8 at $($r98.Short) on this computer$(Get-PinTail '9.9.8')" -and (Get-Pin $hz.Prefix)) "fetched=$($fetchLog.Count) rc=$($e.Rc) last='$($e.Last)'"
    }
    # 19. A Julia the resolver refused is not a missing Julia: the stage skips Julia only when none is found, and a
    # refusal (a relative override, an app-execution alias) stays the stage's error, naming itself.
    Invoke-Case '7t' {
        $ht = New-Home 't1'
        [void](Invoke-Entry @('-Archive', $r98.Pack))
        $env:SOT_JULIA_BIN = 'julia'
        $e = Invoke-Entry @('-Archive', $r99.Pack)
        Check '7t: SOT_JULIA_BIN=julia (relative) is FAIL stage naming the refusal, never a PASS' ($e.Rc -eq 1 -and $e.Verdicts -eq 1 -and $e.Last -like 'FAIL: stage: *SOT_JULIA_BIN="julia" is not an absolute path*') "rc=$($e.Rc) last='$($e.Last)'"
        Check '7t: and the run moved nothing' ((Get-Json $ht.Prefix 'version') -eq '9.9.8') (Get-Json $ht.Prefix 'version')
        $alias = Join-Path $savedEnv['LOCALAPPDATA'] 'Microsoft\WindowsApps\julia.exe'
        if (Test-Path -LiteralPath $alias) {
            $ht2 = New-Home 't2'
            [void](Invoke-Entry @('-Archive', $r98.Pack))
            $env:SOT_JULIA_BIN = $alias
            $e = Invoke-Entry @('-Archive', $r99.Pack)
            Check '7t: SOT_JULIA_BIN naming this box''s Store alias is FAIL stage naming the alias' ($e.Rc -eq 1 -and $e.Last -like 'FAIL: stage: *app-execution alias*') "rc=$($e.Rc) last='$($e.Last)'"
        } else { Write-Host '  note  7t: this box has no Store julia alias; the alias case is not run' }
    }
    # 19b. The repair cases (an install.json that is missing, cut or version-less), in a sibling file for the line limit.
    . (Join-Path $PSScriptRoot 'test-install-entry-repair.ps1')
    . (Join-Path $PSScriptRoot 'test-install-entry-direction.ps1')
    . (Join-Path $PSScriptRoot 'test-install-entry-julia.ps1')
    . (Join-Path $PSScriptRoot 'test-install-entry-live.ps1')

    # 20. A FAIL says what the run left behind: the pin once written, and an update it staged that is still armed.
    # A supervisor stand-in that logs code= and never takes the sentinel, so a converge times out.
    function Start-IdleSupervisor($Hm) {
        $logDir = Join-Path $Hm.Home 'AppData\Local\sot\logs'
        New-Item -ItemType Directory -Force -Path $logDir | Out-Null
        $sup = Join-Path $entryRoot 'launch-sot.ps1'
        Set-Content -LiteralPath $sup -Value 'Start-Sleep -Seconds 120'
        $proc = Start-Process -FilePath powershell.exe -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $sup) -PassThru -WindowStyle Hidden
        Start-Sleep -Seconds 2
        Set-Content -LiteralPath (Join-Path $logDir 'launcher.pid') -Value "$($proc.Id)"
        Set-Content -LiteralPath (Join-Path $logDir 'supervisor.log') -Value "2026-10-17T00:00:00  pid=$($proc.Id)  supervisor start (relaunched=False) depth=0 ws=1MB code=abc-def"
        return $proc
    }
    function Stop-IdleSupervisor($Hm, $Proc) {
        Stop-Process -Id $Proc.Id -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath (Join-Path $Hm.Home 'AppData\Local\sot\logs\launcher.pid') -Force -ErrorAction SilentlyContinue
        [Environment]::SetEnvironmentVariable('SOT_INSTALL_CONVERGE_WAIT_S', $null); [Environment]::SetEnvironmentVariable('SOT_INSTALL_TAG', $null); [Environment]::SetEnvironmentVariable('SOT_INSTALL_IMPLICIT', $null)
    }
    Invoke-Case '7u' {
        # P7's shape: a named older run whose supervisor never takes the sentinel.
        $hu = New-Home 'u1'
        [void](Invoke-Entry @('-Archive', $r99.Pack))
        $procU = Start-IdleSupervisor $hu
        $ptrU = Join-Path $hu.Prefix 'updates\pending-windows-x86_64.json'
        try {
            $env:SOT_INSTALL_CONVERGE_WAIT_S = '3'
            $env:SOT_INSTALL_TAG = 'v9.9.8'; $script:servePack = $r98.Pack
            $e = Invoke-Entry @('-Version', 'v9.9.8')
            Check '7u: a named run whose converge times out is FAIL apply, saying the update stays armed and the box is pinned' ($e.Rc -eq 1 -and $e.Verdicts -eq 1 -and $e.Last -like 'FAIL: apply: the supervisor did not report v9.9.8 applied within 3 s*' -and $e.Last.EndsWith("; the update to v9.9.8 stays armed: the next start applies it$(Get-PinTail '9.9.8')")) "rc=$($e.Rc) last='$($e.Last)'"
            Check '7u: and the pointer and the pin are what the line says' ((Test-Path $ptrU) -and (Get-Pin $hu.Prefix) -and (Get-Json $hu.Prefix 'version') -eq '9.9.9') "pointer=$(Test-Path $ptrU) pin=$(Get-Pin $hu.Prefix) version=$(Get-Json $hu.Prefix 'version')"
        } finally { Stop-IdleSupervisor $hu $procU }
        # An unpinned run (the channel's own) that times out says it is armed and names no pin.
        $hu2 = New-Home 'u2'
        [void](Invoke-Entry @('-Archive', $r98.Pack))
        $procU2 = Start-IdleSupervisor $hu2
        try {
            $env:SOT_INSTALL_CONVERGE_WAIT_S = '3'
            $env:SOT_INSTALL_TAG = 'v9.9.9'; $env:SOT_INSTALL_IMPLICIT = '1'; $script:servePack = $r99.Pack
            $e = Invoke-Entry @()
            Check '7u: an unpinned run that times out says the update stays armed and names no pin' ($e.Rc -eq 1 -and $e.Last -like 'FAIL: apply: *' -and $e.Last.EndsWith('; the update to v9.9.9 stays armed: the next start applies it') -and -not (Get-Pin $hu2.Prefix)) "rc=$($e.Rc) last='$($e.Last)' pin=$(Get-Pin $hu2.Prefix)"
        } finally { Stop-IdleSupervisor $hu2 $procU2 }
    }

    # 21. A daemon whose image sot-apply.ps1 renamed aside (bin\sotd-stale-*.exe) is still a daemon that runs.
    Invoke-Case '7v' {
        $hv = New-Home 'v1'
        [void](Invoke-Entry @('-Archive', $r98.Pack))
        $binV = Join-Path $hv.Prefix 'bin'
        $staleExe = Join-Path $binV 'sotd-stale-20261017-000000.exe'
        Copy-Item -LiteralPath (Join-Path $binV 'sotd.exe') -Destination $staleExe
        Check '7v: nothing runs: no daemon' (-not (Test-DaemonRuns '' (Join-Path $binV 'sotd.exe'))) 'reported a daemon'
        $env:SOT_FAKE_SLEEP = '1'
        $heldV = Start-Process -FilePath $staleExe -PassThru -WindowStyle Hidden
        [Environment]::SetEnvironmentVariable('SOT_FAKE_SLEEP', $null)
        try {
            Start-Sleep -Seconds 2
            Check '7v: a process of bin\sotd-stale-*.exe is a daemon that runs' (Test-DaemonRuns '' (Join-Path $binV 'sotd.exe')) 'not seen'
        } finally { Stop-Process -Id $heldV.Id -Force -ErrorAction SilentlyContinue }
    }

    # 22. Path (c) with a daemon to stop: the entry runs the installed checkout's sot-local-daemon.ps1 -Stop (a stand-in
    # here, which records its argv and ends the stand-in daemon), then applies.
    Invoke-Case '7w' {
        $hw = New-Home 'w1'
        [void](Invoke-Entry @('-Archive', $r98.Pack))
        $binW = Join-Path $hw.Prefix 'bin\sotd.exe'
        $env:SOT_FAKE_SLEEP = '1'
        $heldW = Start-Process -FilePath $binW -PassThru -WindowStyle Hidden
        [Environment]::SetEnvironmentVariable('SOT_FAKE_SLEEP', $null)
        $argvFile = Join-Path $hw.Home 'stop-argv.txt'
        $dfileW = Join-Path $hw.Home 'daemon.code'
        Set-Content -LiteralPath $dfileW -Value '3'
        $env:SOT_FAKE_DAEMON_FILE = $dfileW
        Set-Content -LiteralPath (Join-Path $hw.Prefix 'repo\versions\v9.9.8\scripts\sot-local-daemon.ps1') -Value @"
[System.IO.File]::WriteAllText('$argvFile', (`$args -join ' '))
Stop-Process -Id $($heldW.Id) -Force
Set-Content -LiteralPath '$dfileW' -Value '2'
Write-Host 'sot-local-daemon: stopped'
"@
        try {
            Start-Sleep -Seconds 2
            $e = Invoke-Entry @('-Archive', $r99.Pack)
            Check '7w: a daemon on another version with no live work is stopped by the installed checkout''s sot-local-daemon.ps1 -Stop' ((Test-Path $argvFile) -and ([System.IO.File]::ReadAllText($argvFile) -ceq '-Stop') -and $heldW.HasExited) "argv=$(if (Test-Path $argvFile) { [System.IO.File]::ReadAllText($argvFile) } else { 'none' }) exited=$($heldW.HasExited)"
            Check '7w: and the apply goes on to PASS at the new version' ($e.Rc -eq 0 -and $e.Last -like 'PASS: *9.9.9*' -and (Get-Json $hw.Prefix 'version') -eq '9.9.9') "rc=$($e.Rc) last='$($e.Last)'"
        } finally { Stop-Process -Id $heldW.Id -Force -ErrorAction SilentlyContinue }
    }

    # 23. A session that cannot traverse repo\current (replaced seam): the shortcut is kept when it already targets the
    # junction, else the run asks for the desk.
    Invoke-Case '7x' {
        $hx = New-Home 'x1'
        [void](Invoke-Entry @('-Archive', $r98.Pack))
        function Test-JunctionTraversable([string]$ScriptPath) { return $false }
        $e = Invoke-Entry @('-Archive', $r98.Pack)
        Check '7x: an untraversable junction keeps a shortcut that already targets repo\current (PASS)' ($e.Rc -eq 0 -and $e.Last -like 'PASS: *' -and ($e.Out -match 'shortcut: kept').Count -eq 1) "rc=$($e.Rc) last='$($e.Last)'"
        Remove-Item -LiteralPath (Join-Path $hx.Home 'Desktop\Ship of Tools.lnk') -Force
        $e = Invoke-Entry @('-Archive', $r98.Pack)
        Check '7x: with no such shortcut it is ONE STEP LEFT to run this again at the desk' ($e.Rc -eq 0 -and $e.Last -like 'ONE STEP LEFT: run this again at the desk, not over ssh*') "rc=$($e.Rc) last='$($e.Last)'"
        function Test-JunctionTraversable([string]$ScriptPath) { return (Test-Path -LiteralPath $ScriptPath) }
    }
    # 24. Fresh from a pack of the line (a + version): the record names it, its tag and the pack's commit.
    $rdevRx = [regex]::Escape($rdev.Ver)
    Invoke-Case '7a2' {
        $hd = New-Home 'hd'
        $e = Invoke-Entry @('-Archive', $rdev.Pack)
        Check '7a2: a fresh install from a pack of the line ends in PASS, pinned' ("$($e.Rc) $($e.Verdicts) $($e.Last)" -ceq "0 1 PASS: Ship of Tools $($rdev.Ver) at $($rdev.Short) on this computer$(Get-PinTail $rdev.Ver)") "rc=$($e.Rc) last='$($e.Last)'; $(($e.Out | Select-Object -Last 4) -join ' / ')"
        $ijd = ''; try { $ijd = Get-Content -LiteralPath (Join-Path $hd.Prefix 'install.json') -Raw } catch { }
        Check '7a2: install.json names the + version, its tag, the pack commit, daemon and frontend, and the pin' (($ijd -match "`"version`":\s*`"$rdevRx`"") -and ($ijd -match "`"tag`":\s*`"v$rdevRx`"") -and ($ijd -match $rdev.Commit) -and ($ijd -match '"daemon":\s*true,\s*"frontend":\s*true') -and ($ijd -match '"pinned":\s*true')) $ijd
        Check '7a2: the layout pinned the pack folder, so repo\current is a junction to it (the version has a second hyphen)' ((Test-Path -LiteralPath (Join-Path $hd.Prefix 'repo\current\scripts\install-manifest.ps1')) -and ((Get-Item -LiteralPath (Join-Path $hd.Prefix 'repo\current')).Target -like "*repo\versions\v$($rdev.Ver)")) "target=$((Get-Item -LiteralPath (Join-Path $hd.Prefix 'repo\current') -ErrorAction SilentlyContinue).Target)"
    }
    # 25. The hub's refusal is read on a pack of the line, fresh and update (the record decision and the sync are separate).
    Invoke-Case '7a3' {
        $hf = New-Home 'hf'
        $env:SOT_FAKE_SYNC = 'refuse'
        $e = Invoke-Entry @('-Archive', $rdev.Pack, '-Hub', 'rig-hub')
        Check '7a3: a fresh pack of the line whose hub does not list this computer is FAIL enroll' ($e.Rc -eq 1 -and $e.Verdicts -eq 1 -and $e.Last -like "FAIL: enroll: the hosts.toml on 'rig-hub' does not list this computer ('rig-pc')*") "rc=$($e.Rc) last='$($e.Last)'"
        $env:SOT_FAKE_SYNC = 'ok'
        $e = Invoke-Entry @('-Archive', $rdev.Pack, '-Hub', 'rig-hub')
        Check '7a3: the same install with a hub that lists it is PASS' ($e.Rc -eq 0 -and $e.Last -like 'PASS: *') "rc=$($e.Rc) last='$($e.Last)'"
        $hu = New-Home 'hu1'
        $env:SOT_FAKE_SYNC = 'ok'
        $e = Invoke-Entry @('-Archive', $rdev.Pack, '-Hub', 'rig-hub')
        $env:SOT_FAKE_SYNC = 'refuse'
        $e = Invoke-Entry @('-Archive', $rdev2.Pack, '-Hub', 'rig-hub')
        Check '7a3: an update to another pack of the line whose hub does not list this computer is FAIL enroll' ($e.Rc -eq 1 -and $e.Verdicts -eq 1 -and $e.Last -like "FAIL: enroll: *" -and (Get-Json $hu.Prefix 'commit') -eq $rdev2.Commit) "rc=$($e.Rc) last='$($e.Last)' commit=$(Get-Json $hu.Prefix 'commit'); $(($e.Out | Select-Object -Last 4) -join ' / ')"
    }
    # 26. A profile spelled in 8.3 (a hosted runner's TEMP is): the junction reports the long spelling of a correct
    # install, which the end check must take for the short one it was given.
    Invoke-Case '7a5' {
        $hs = New-Home 'a-profile-with-a-long-name' -Short
        if ($hs.Prefix -notmatch '~') { Write-Host '  SKIP  7a5: this volume creates no 8.3 names' -ForegroundColor Yellow; return }
        # install.ps1 sees only what its own load of the target checkout's layout file defines: while its three installs run, the
        # two layout functions this suite dot-sourced into script scope answer "unloaded:" and their input, which no checkout path
        # equals, so an install whose own load fails cannot borrow them; they are restored afterward.
        $harness = @{}
        foreach ($f in @('Get-SotComparablePath', 'Get-SotLauncherCodeId')) { $harness[$f] = (Get-Item -LiteralPath "Function:$f").ScriptBlock }
        Set-Item -LiteralPath 'Function:script:Get-SotComparablePath' -Value { param([string]$Path) "unloaded:$Path" }
        Set-Item -LiteralPath 'Function:script:Get-SotLauncherCodeId' -Value { param([Parameter(Mandatory)][string]$ScriptsDir) "unloaded:$ScriptsDir" }
        try {
            $e = Invoke-Entry @('-Archive', $r98.Pack)
            Check '7a5: a fresh install under an 8.3-spelled prefix ends in PASS' ($e.Rc -eq 0 -and $e.Last -like 'PASS: Ship of Tools 9.9.8 at *') "rc=$($e.Rc) last='$($e.Last)'"
            $e = Invoke-Entry @('-Archive', $r99.Pack)
            Check '7a5: and so does an update under it' ($e.Rc -eq 0 -and $e.Last -like 'PASS: Ship of Tools 9.9.9 at *') "rc=$($e.Rc) last='$($e.Last)'"
            $e = Invoke-Entry @('-Archive', $r99.Pack)
            Check '7a5: and a repair run' ($e.Rc -eq 0 -and $e.Last -like 'PASS: *' -and ($e.Out -match 'a repair run').Count -eq 1) "rc=$($e.Rc) last='$($e.Last)'"
        } finally {
            foreach ($f in $harness.Keys) { Set-Item -LiteralPath "Function:script:$f" -Value $harness[$f] }
        }
    }
    # 27. A lone install.ps1 (a downloaded file with nothing beside it) installs a pack of the line fresh: the record script
    # is the target checkout's own. Last, because it dot-sources the lone copy over this scope's functions.
    Invoke-Case '7a4' {
        $lone = Join-Path $entryRoot 'lone'
        New-Item -ItemType Directory -Force -Path $lone | Out-Null
        Copy-Item -LiteralPath $entryScript -Destination (Join-Path $lone 'install.ps1') -Force
        . (Join-Path $lone 'install.ps1')
        function Write-EntryLine([string]$Text) { if ($script:Capturing) { $script:Captured += $Text } else { Write-Host $Text } }
        function Get-EntrySessionId { if ($script:DeskSession) { return 3 } else { return 0 } }
        $hl = New-Home 'hl'
        $e = Invoke-Entry @('-Archive', $rdev.Pack)
        $ijl = ''; try { $ijl = Get-Content -LiteralPath (Join-Path $hl.Prefix 'install.json') -Raw } catch { }
        Check '7a4: a lone install.ps1 installs a pack of the line fresh, PASS with the record' ($e.Rc -eq 0 -and $e.Last -like 'PASS: *' -and ($ijl -match $rdev.Commit)) "rc=$($e.Rc) last='$($e.Last)'"
    }
} catch {
    Check '7: section ran' $false $_.Exception.Message
} finally {
    foreach ($n in $savedEnv.Keys) { [Environment]::SetEnvironmentVariable($n, $savedEnv[$n]) }
}

# test-scratch-guard.ps1 -- scratch-guard.ps1: its rule on values the test passes in (a made-up account whose folders are
# a fixture under this test's own folder), and the whole entry end to end on a probe suite: the suite runs in a home of
# its own with the arguments it was given, the caller's environment is unchanged, the home is gone at the end and a
# sibling folder it did not make is not, two concurrent runs end only their own, and a child under a profile that is not
# a scratch one is refused before the suite's body runs. Not itself guarded: it starts nothing of sot's. It makes one
# folder under TEMP and removes it, by its path. Windows PowerShell 5.1, ASCII only.
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\tests\test-scratch-guard.ps1
$ErrorActionPreference = 'Stop'
$pass = 0; $fail = 0
function Check([string]$name, [bool]$ok, [string]$detail) {
    if ($ok) { $script:pass++; Write-Host "  PASS  $name" -ForegroundColor Green }
    else { $script:fail++; Write-Host "  FAIL  $name -- $detail" -ForegroundColor Red }
}
$guard = Join-Path $PSScriptRoot 'scratch-guard.ps1'
# Set in this scope and left set: it loads the functions only here, and the in-process case below proves a suite run from
# this scope is still guarded.
$sotScratchLibOnly = $true
. $guard

$F = Join-Path $env:TEMP ('sot-scratch-guard-test-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Force -Path $F | Out-Null
try {
    # ---- the rule, on a made-up account -------------------------------------------------------------------------------
    $real = @{
        UserName = 'alice'
        Profile  = "$F\alice"
        Roaming  = "$F\alice\AppData\Roaming"
        Local    = "$F\alice\AppData\Local"
        Temp     = "$F\alice\AppData\Local\Temp"
    }
    New-Item -ItemType Directory -Force -Path $real.Roaming, $real.Local, $real.Temp | Out-Null
    $root = "$($real.Temp)\sot-scratch-a1b2c3d4"
    New-Item -ItemType Directory -Force -Path "$root\home\AppData\Roaming", "$root\home\AppData\Local", "$root\tmp", "$root\home\comm" | Out-Null
    [System.IO.File]::WriteAllText("$root\sot-scratch-root.marker", "sot-scratch-root $root`r`n")
    function New-Vars { @{
        USERNAME = 'sot-scratch-a1b2c3d4'; USERPROFILE = "$root\home"; HOME = "$root\home"
        APPDATA = "$root\home\AppData\Roaming"; LOCALAPPDATA = "$root\home\AppData\Local"
        TEMP = "$root\tmp"; TMP = "$root\tmp"; SOT_COMM_HOME = "$root\home\comm" } }
    function Refusal([hashtable]$Vars) { Get-SotScratchRefusal $Vars $real }

    Check 'a correct scratch profile passes' ($null -eq (Refusal (New-Vars))) (Refusal (New-Vars))
    $v = New-Vars; $v.Remove('HOME'); $v.Remove('SOT_COMM_HOME')
    Check 'HOME and SOT_COMM_HOME may be unset' ($null -eq (Refusal $v)) (Refusal $v)

    # The review's bypass: a scratch USERNAME, TMP at the real profile, the real APPDATA and LOCALAPPDATA.
    $v = New-Vars; $v.TMP = $real.Profile; $v.APPDATA = $real.Roaming; $v.LOCALAPPDATA = $real.Local
    Check 'a scratch USERNAME, TMP at the real profile and the real AppData folders is refused' ([bool](Refusal $v)) 'passed'

    foreach ($var in @('USERPROFILE', 'HOME', 'APPDATA', 'LOCALAPPDATA', 'TEMP', 'TMP', 'SOT_COMM_HOME')) {
        foreach ($rf in @('Profile', 'Roaming', 'Local', 'Temp')) {
            $v = New-Vars; $v[$var] = $real[$rf]
            Check "$var at the real $rf folder is refused" ([bool](Refusal $v)) 'passed'
        }
        $v = New-Vars; $v[$var] = "$root\..\.."
        Check "$var climbing out of the home is refused" ([bool](Refusal $v)) 'passed'
        $v = New-Vars; $v[$var] = "$($real.Temp)\sot-sibling"
        Check "$var in a sibling of the home is refused" ([bool](Refusal $v)) 'passed'
        $v = New-Vars; $v[$var] = $root
        Check "$var at the home itself is refused" ([bool](Refusal $v)) 'passed'
        $v = New-Vars; $v[$var] = 'relative\folder'
        Check "$var as a relative path is refused" ([bool](Refusal $v)) 'passed'
    }
    foreach ($var in @('USERPROFILE', 'APPDATA', 'LOCALAPPDATA', 'TEMP', 'TMP')) {
        $v = New-Vars; $v.Remove($var)
        Check "$var unset is refused" ([bool](Refusal $v)) 'passed'
    }
    $v = New-Vars; $v.USERNAME = 'alice'
    Check 'the real USERNAME is refused' ([bool](Refusal $v)) 'passed'
    $v = New-Vars; $v.USERNAME = 'someone-else'
    Check 'a USERNAME that is not a scratch name is refused' ([bool](Refusal $v)) 'passed'
    $v = New-Vars; $v.USERNAME = ''
    Check 'an empty USERNAME is refused' ([bool](Refusal $v)) 'passed'
    $v = New-Vars; $v.USERNAME = 'sot-scratch-other'
    Check 'a USERNAME that is not the home folder''s name is refused' ([bool](Refusal $v)) 'passed'
    $real2 = @{} + $real; $real2.UserName = 'sot-scratch-a1b2c3d4'
    Check 'a scratch USERNAME that is the account''s real name is refused' ([bool](Get-SotScratchRefusal (New-Vars) $real2)) 'passed'
    Remove-Item -LiteralPath "$root\sot-scratch-root.marker" -Force
    Check 'a home with no marker is refused' ([bool](Refusal (New-Vars))) 'passed'
    [System.IO.File]::WriteAllText("$root\sot-scratch-root.marker", "sot-scratch-root $root-other`r`n")
    Check 'a marker that names another folder is refused' ([bool](Refusal (New-Vars))) 'passed'
    [System.IO.File]::WriteAllText("$root\sot-scratch-root.marker", "sot-scratch-root $root`r`n")
    $deep = "$($real.Temp)\x\sot-scratch-a1b2c3d4"
    New-Item -ItemType Directory -Force -Path "$deep\home\AppData\Roaming", "$deep\home\AppData\Local", "$deep\tmp" | Out-Null
    [System.IO.File]::WriteAllText("$deep\sot-scratch-root.marker", "sot-scratch-root $deep`r`n")
    $v = New-Vars; $v.USERPROFILE = "$deep\home"; $v.HOME = "$deep\home"; $v.APPDATA = "$deep\home\AppData\Roaming"; $v.LOCALAPPDATA = "$deep\home\AppData\Local"; $v.TEMP = "$deep\tmp"; $v.TMP = "$deep\tmp"; $v.SOT_COMM_HOME = "$deep\home\comm"
    Check 'a home that is not directly under the real temp folder is refused' ([bool](Refusal $v)) 'passed'
    $r = Refusal (@{ USERNAME = 'alice'; USERPROFILE = 'C:\x'; APPDATA = 'C:\y'; LOCALAPPDATA = 'C:\z'; TEMP = 'C:\t'; TMP = 'C:\t' })
    Check 'the refusal is one line that names what is wrong' ($r -notmatch "[\r\n]" -and $r -like "*'alice'*" -and $r -like '*scratch-guard: refusing*') $r

    # ---- the child's depot --------------------------------------------------------------------------------------------
    $childHome = "$root\home"
    Check 'a caller depot outside the real temp folder is replaced by one in the home' ((Get-SotChildDepot "$($real.Profile)\.julia" $childHome $real.Temp) -eq "$childHome\.julia") 'kept'
    Check 'an unset depot gets one in the home' ((Get-SotChildDepot '' $childHome $real.Temp) -eq "$childHome\.julia") 'kept'
    Check 'a caller depot inside the real temp folder is kept' ((Get-SotChildDepot "$($real.Temp)\my-depot" $childHome $real.Temp) -eq "$($real.Temp)\my-depot") 'replaced'
    Check 'a list with one entry outside the real temp folder is replaced' ((Get-SotChildDepot "$($real.Temp)\a;$($real.Profile)\.julia" $childHome $real.Temp) -eq "$childHome\.julia") 'kept'
    Check 'the real temp folder itself is not a scratch depot' ((Get-SotChildDepot $real.Temp $childHome $real.Temp) -eq "$childHome\.julia") 'kept'

    # ---- the entry, end to end, on a probe suite ----------------------------------------------------------------------
    $probe = Join-Path $F 'probe.ps1'
    $probeText = @'
param([string]$OutFile, [string]$Name = '', [switch]$Flag, [int]$SleepSec = 0)
$ErrorActionPreference = 'Stop'
$sotSuite = $PSCommandPath; $sotSuiteArgs = $PSBoundParameters
. 'GUARDPATH'
if ($null -ne $sotScratchExit) { exit $sotScratchExit }
Add-Content -LiteralPath $OutFile -Value "RAN"
foreach ($n in @('USERNAME', 'USERPROFILE', 'HOME', 'APPDATA', 'LOCALAPPDATA', 'TEMP', 'TMP', 'SOT_COMM_HOME', 'SOT_PROBE_VAR', 'SOT_SMOKE_X', 'XDG_STATE_HOME', 'XDG_CONFIG_HOME', 'CLAUDE_CONFIG_DIR', 'JULIA_DEPOT_PATH')) {
    Add-Content -LiteralPath $OutFile -Value ("$n=" + [Environment]::GetEnvironmentVariable($n))
}
Add-Content -LiteralPath $OutFile -Value ("RealProfile=" + (Get-SotRealProfile).Profile)
Add-Content -LiteralPath $OutFile -Value ("FolderPathLocal=" + [Environment]::GetFolderPath('LocalApplicationData'))
Add-Content -LiteralPath $OutFile -Value ("Cwd=" + (Get-Location -PSProvider FileSystem).ProviderPath)
$rel = $false
if ($Name) { $rel = Test-Path -LiteralPath $Name }   # Test-Path throws on an empty path, which ended the probes that name none
Add-Content -LiteralPath $OutFile -Value ("RelExists=" + $rel)
Add-Content -LiteralPath $OutFile -Value "Name=$Name"
Add-Content -LiteralPath $OutFile -Value "Flag=$($Flag.IsPresent)"
Set-Content -LiteralPath (Join-Path $env:TEMP 'made-by-probe.txt') -Value 'x'
if ($SleepSec -gt 0) { Start-Sleep -Seconds $SleepSec }
exit 7
'@
    [System.IO.File]::WriteAllText($probe, $probeText.Replace('GUARDPATH', $guard.Replace("'", "''")))

    function Read-Probe([string]$File) {
        $h = @{}
        if (Test-Path -LiteralPath $File) { foreach ($l in @(Get-Content -LiteralPath $File)) { $i = $l.IndexOf('='); if ($i -gt 0) { $h[$l.Substring(0, $i)] = $l.Substring($i + 1) } else { $h[$l] = $true } } }
        return $h
    }
    function Start-Probe([string]$Tag, [string]$ExtraArgs, [string]$Script = $probe) {
        $psi = New-Object System.Diagnostics.ProcessStartInfo
        $psi.FileName = (Get-Process -Id $PID).Path
        $psi.UseShellExecute = $false
        $psi.RedirectStandardOutput = $true; $psi.RedirectStandardError = $true
        $out = Join-Path $F "out-$Tag.txt"
        $psi.Arguments = "-NoProfile -ExecutionPolicy Bypass -File `"$Script`" -OutFile `"$out`" $ExtraArgs"
        return @{ Proc = [System.Diagnostics.Process]::Start($psi); Out = $out }
    }
    $env:SOT_PROBE_VAR = 'from-the-caller'
    $env:SOT_SMOKE_X = 'kept'
    $env:XDG_STATE_HOME = "$F\xdg-state"; $env:XDG_CONFIG_HOME = "$F\xdg-config"; $env:CLAUDE_CONFIG_DIR = "$F\claude"
    $before = @{}
    foreach ($n in @('USERNAME', 'USERPROFILE', 'APPDATA', 'LOCALAPPDATA', 'TEMP', 'TMP')) { $before[$n] = [Environment]::GetEnvironmentVariable($n) }
    $realNow = Get-SotRealProfile
    $env:JULIA_DEPOT_PATH = Join-Path $realNow.Profile '.sot-guard-test-depot'   # outside the real temp folder, never created
    $sibling = Join-Path $realNow.Temp ('sot-scratch-sibling-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
    New-Item -ItemType Directory -Force -Path $sibling | Out-Null

    $a = Start-Probe 'one' '-Name "a b" -Flag'
    $a.Proc.WaitForExit()
    $errA = $a.Proc.StandardError.ReadToEnd()
    $o = Read-Probe $a.Out
    $homeRoot = $(if ($o.TEMP) { Split-Path -Parent $o.TEMP } else { '' })
    Check 'the suite ran, and its exit code is the child''s' ($a.Proc.ExitCode -eq 7 -and $o.RAN) "exit $($a.Proc.ExitCode); $errA"
    Check 'the guard printed one line when it made the home and one when it removed it' (($errA -match "scratch-guard: probe\.ps1 runs in \S+") -and ($errA -match "scratch-guard: removed \S+")) $errA
    Check 'the child''s anchor for the real profile is the account''s own, not the moved USERPROFILE' ($o.RealProfile -ieq $realNow.Profile -and $o.USERPROFILE -ine $o.RealProfile) "anchor $($o.RealProfile); parent's $($realNow.Profile); USERPROFILE $($o.USERPROFILE)"
    Write-Host "    note: GetFolderPath('LocalApplicationData') in the child said $($o.FolderPathLocal) (USERPROFILE $($o.USERPROFILE))"
    Check 'its arguments reached the child' ($o.Name -eq 'a b' -and $o.Flag -eq 'True') "Name=$($o.Name) Flag=$($o.Flag)"
    Check 'its USERNAME is scratch and not the real name' ($o.USERNAME -like 'sot-scratch-*' -and $o.USERNAME -ine $realNow.UserName) $o.USERNAME
    $c = Get-SotCanonicalPath $homeRoot
    Check 'its home is directly under the real temp folder' ($c -and ([System.IO.Path]::GetDirectoryName($c) -ieq (Get-SotCanonicalPath $realNow.Temp))) "home: $homeRoot"
    $inside = $true
    foreach ($n in @('USERPROFILE', 'HOME', 'APPDATA', 'LOCALAPPDATA', 'TEMP', 'TMP', 'SOT_COMM_HOME')) { if (-not (Test-SotInside (Get-SotCanonicalPath $o[$n]) $c)) { $inside = $false; Write-Host "    $n=$($o[$n])" } }
    Check 'all seven variables lay inside that home' $inside "home: $homeRoot"
    Check 'the caller''s other SOT_ variables were cleared, SOT_SMOKE_ ones kept' ((-not $o.SOT_PROBE_VAR) -and $o.SOT_SMOKE_X -eq 'kept') "SOT_PROBE_VAR=$($o.SOT_PROBE_VAR) SOT_SMOKE_X=$($o.SOT_SMOKE_X)"
    Check 'XDG_* and CLAUDE_CONFIG_DIR of the caller did not reach the child' ((-not $o.XDG_STATE_HOME) -and (-not $o.XDG_CONFIG_HOME) -and (-not $o.CLAUDE_CONFIG_DIR)) "XDG_STATE_HOME=$($o.XDG_STATE_HOME) XDG_CONFIG_HOME=$($o.XDG_CONFIG_HOME) CLAUDE_CONFIG_DIR=$($o.CLAUDE_CONFIG_DIR)"
    Check 'a JULIA_DEPOT_PATH outside the real temp folder was replaced by a depot inside the home' ($o.JULIA_DEPOT_PATH -and (Test-SotInside (Get-SotCanonicalPath $o.JULIA_DEPOT_PATH) $c)) "JULIA_DEPOT_PATH=$($o.JULIA_DEPOT_PATH)"
    Check 'the home is gone at the end, with what the suite made in it' ($c -and -not (Test-Path -LiteralPath $c)) "still there: $homeRoot"
    Check 'a sibling folder the run did not make is still there' (Test-Path -LiteralPath $sibling) 'removed'
    $same = $true
    foreach ($n in $before.Keys) { if ([Environment]::GetEnvironmentVariable($n) -ne $before[$n]) { $same = $false } }
    Check 'the caller''s own environment is unchanged' $same 'changed'

    # Two concurrent runs: the second ends while the first sleeps; the first's home must stand until the first ends.
    $long = Start-Probe 'long' '-SleepSec 8'
    $deadline = (Get-Date).AddSeconds(60)
    $lo = @{}
    while ((Get-Date) -lt $deadline -and -not $lo.TMP) { Start-Sleep -Milliseconds 300; $lo = Read-Probe $long.Out }
    $longHome = $(if ($lo.TMP) { Split-Path -Parent $lo.TMP } else { '' })
    $short = Start-Probe 'short' ''
    $short.Proc.WaitForExit()
    $so = Read-Probe $short.Out
    $shortHome = $(if ($so.TMP) { Split-Path -Parent $so.TMP } else { '' })
    Check 'two concurrent runs have two homes' ($longHome -and $shortHome -and $longHome -ne $shortHome) "long $longHome short $shortHome"
    Check 'the run that ended exited with the probe''s code' ($short.Proc.ExitCode -eq 7) "exit $($short.Proc.ExitCode)"
    Check 'the one that ended removed its own home' ($shortHome -and -not (Test-Path -LiteralPath $shortHome)) "still there: $shortHome"
    Check 'and left the other''s home, and what that run made in it' ($longHome -and (Test-Path -LiteralPath (Join-Path $longHome 'tmp\made-by-probe.txt')) -and -not $long.Proc.HasExited) "long home $longHome; long exited: $($long.Proc.HasExited); exit $(if ($long.Proc.HasExited) { $long.Proc.ExitCode })"
    $long.Proc.WaitForExit()
    Check 'the other removed its own at its end' ($longHome -and $long.Proc.ExitCode -eq 7 -and -not (Test-Path -LiteralPath $longHome)) "exit $($long.Proc.ExitCode); still there: $longHome"

    # A suite run in-process from a scope where $sotScratchLibOnly is $true is still guarded, and the child starts in the
    # caller's location (Set-Location does not move the process's own folder), so a relative argument resolves there.
    $wd = Join-Path $F 'wd'
    New-Item -ItemType Directory -Force -Path $wd | Out-Null
    Set-Content -LiteralPath (Join-Path $wd 'rel.txt') -Value 'x'
    $inProc = Join-Path $F 'out-inproc.txt'
    Push-Location -LiteralPath $wd
    try { & $probe -OutFile $inProc -Name 'rel.txt' } finally { Pop-Location }
    $rc = $LASTEXITCODE
    $io = Read-Probe $inProc
    Check 'an in-process run from a scope with the library switch set is still guarded, in its own home' ($rc -eq 7 -and $io.RAN -and $io.USERNAME -like 'sot-scratch-*' -and $sotScratchLibOnly -eq $true) "exit $rc; USERNAME $($io.USERNAME)"
    Check 'the child started in the caller''s location, so a relative argument resolved there' ($io.RelExists -eq 'True' -and $io.Cwd -ieq (Get-SotCanonicalPath $wd)) "Cwd $($io.Cwd); RelExists $($io.RelExists)"
    Check 'and that run changed nothing in this environment' ([Environment]::GetEnvironmentVariable('USERNAME') -eq $before.USERNAME) 'USERNAME changed'

    # The daemon suite's helpers: two concurrent runs each end only the daemons they saw on their own pipes. The stand-in
    # daemon is a copy of powershell.exe named sotd.exe whose command line carries `--socket <this run's pipe>`.
    $dprobe = Join-Path $F 'probe-daemons.ps1'
    $dtext = @'
param([string]$OutFile, [int]$SleepSec = 0)
$ErrorActionPreference = 'Stop'
$sotSuite = $PSCommandPath; $sotSuiteArgs = $PSBoundParameters
. 'GUARDPATH'
if ($null -ne $sotScratchExit) { exit $sotScratchExit }
. 'SUPPORTPATH'
$bin = Join-Path $root 'bin'
New-Item -ItemType Directory -Force -Path $bin | Out-Null
$exe = Join-Path $bin 'sotd.exe'
Copy-Item -LiteralPath (Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe') -Destination $exe
$pipe = Get-PipePath (New-TestPipeName)
$p = Start-Process -FilePath $exe -ArgumentList @('-NoProfile', '-Command', ('"Start-Sleep -Seconds 120 # --socket ' + $pipe + '"')) -WindowStyle Hidden -PassThru
$deadline = (Get-Date).AddSeconds(30)
while ((Get-Date) -lt $deadline -and @(Get-DaemonProcs $pipe).Count -ne 1) { Start-Sleep -Milliseconds 300 }
Add-Content -LiteralPath $OutFile -Value "DaemonPid=$($p.Id)"
if ($SleepSec -gt 0) { Start-Sleep -Seconds $SleepSec }
Complete-LocalDaemonTest
Start-Sleep -Milliseconds 1500
Add-Content -LiteralPath $OutFile -Value ("Alive=" + [bool](Get-Process -Id $p.Id -ErrorAction SilentlyContinue))
exit 7
'@
    [System.IO.File]::WriteAllText($dprobe, $dtext.Replace('GUARDPATH', $guard.Replace("'", "''")).Replace('SUPPORTPATH', (Join-Path $PSScriptRoot 'test-local-daemon-support.ps1').Replace("'", "''")))
    $da = Start-Probe 'da' '-SleepSec 12' $dprobe
    $deadline = (Get-Date).AddSeconds(90)
    $dao = @{}
    while ((Get-Date) -lt $deadline -and -not $dao.DaemonPid) { Start-Sleep -Milliseconds 300; $dao = Read-Probe $da.Out }
    $db = Start-Probe 'db' '' $dprobe
    $db.Proc.WaitForExit()
    $dbo = Read-Probe $db.Out
    $aliveA = $false
    if ($dao.DaemonPid) { $aliveA = [bool](Get-Process -Id ([int]$dao.DaemonPid) -ErrorAction SilentlyContinue) }
    Check 'the run that ended ended its own daemon' ($dbo.DaemonPid -and $dbo.Alive -eq 'False') "B: $($dbo.DaemonPid) alive $($dbo.Alive)"
    Check 'and none of the concurrent run''s' ($dao.DaemonPid -and $aliveA -and -not $da.Proc.HasExited) "A: $($dao.DaemonPid) alive $aliveA"
    $da.Proc.WaitForExit()
    $dao = Read-Probe $da.Out
    Check 'the other ended its own at its end' ($dao.Alive -eq 'False') "A alive $($dao.Alive)"

    $env:JULIA_DEPOT_PATH = Join-Path $realNow.Temp ('sot-scratch-depot-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
    $dk = Start-Probe 'depot' '-Name x'
    $dk.Proc.WaitForExit()
    $dko = Read-Probe $dk.Out
    Check 'a caller depot inside the real temp folder reached the child unchanged' ($dko.JULIA_DEPOT_PATH -eq $env:JULIA_DEPOT_PATH) "child: $($dko.JULIA_DEPOT_PATH); caller: $($env:JULIA_DEPOT_PATH)"

    # A child under a profile that is not a scratch one: the caller sets the child marker by hand at the real profile.
    $env:SOT_SCRATCH_CHILD = $F
    $bad = Start-Probe 'bad' ''
    $bad.Proc.WaitForExit()
    $err = $bad.Proc.StandardError.ReadToEnd().Trim()
    Remove-Item Env:\SOT_SCRATCH_CHILD
    $bo = Read-Probe $bad.Out
    Check 'a child marker at the real profile ends the suite with exit 2 before its body runs, with one line' ($bad.Proc.ExitCode -eq 2 -and -not $bo.RAN -and $err -notmatch "[\r\n]" -and $err -like 'scratch-guard: refusing*') "exit $($bad.Proc.ExitCode); ran: $($bo.RAN); stderr: $err"

    # ---- every guarded suite starts with the four lines ---------------------------------------------------------------
    $lines = [regex]::Escape("`$sotSuite = `$PSCommandPath; `$sotSuiteArgs = `$PSBoundParameters") + '\r?\n' + [regex]::Escape(". (Join-Path `$PSScriptRoot 'scratch-guard.ps1')") + '\r?\n' + [regex]::Escape('if ($null -ne $sotScratchExit) { exit $sotScratchExit }')
    foreach ($s in @('test-sot-apply', 'test-install-manifest', 'test-install-layout', 'test-sot-within', 'test-local-daemon', 'test-launcher-leases', 'test-topology-plan', 'install-smoke')) {
        $text = [System.IO.File]::ReadAllText((Join-Path $PSScriptRoot "$s.ps1"))
        $pre = [regex]::Escape("`$ErrorActionPreference = 'Stop'") + '\r?\n' + $lines
        Check "$s starts with the guard lines, after Stop" ([regex]::IsMatch($text, $pre)) 'the four lines are not there'
        Check "$s ends with an explicit exit 0 after its failure exit" ([regex]::IsMatch($text, '\r?\nexit 0[^\r\n]*\s*\z')) 'no final exit 0'
    }
} finally {
    Remove-Item Env:\SOT_PROBE_VAR -ErrorAction SilentlyContinue
    Remove-Item Env:\SOT_SMOKE_X -ErrorAction SilentlyContinue
    foreach ($n in @('XDG_STATE_HOME', 'XDG_CONFIG_HOME', 'CLAUDE_CONFIG_DIR', 'JULIA_DEPOT_PATH')) { Remove-Item "Env:\$n" -ErrorAction SilentlyContinue }
    Remove-Item Env:\SOT_SCRATCH_CHILD -ErrorAction SilentlyContinue
    if ($sibling) { Remove-Item -LiteralPath $sibling -Recurse -Force -ErrorAction SilentlyContinue }
    Remove-Item -LiteralPath $F -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Host "`n$pass passed, $fail failed"
if ($fail -gt 0) { exit 1 }
exit 0   # explicit: the refusal and exit-7 cases above left $LASTEXITCODE set, and the runner's step would end on it

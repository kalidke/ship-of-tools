# scratch-guard.ps1 -- the first lines of every Windows suite that can start or touch sot state:
#     $ErrorActionPreference = 'Stop'
#     $sotSuite = $PSCommandPath; $sotSuiteArgs = $PSBoundParameters
#     . (Join-Path $PSScriptRoot 'scratch-guard.ps1')
#     if ($null -ne $sotScratchExit) { exit $sotScratchExit }
# The suite never runs in the caller's profile. Run bare, this file makes the suite's OWN home, a folder
# `<the account's real temp folder>\sot-scratch-<id>` with a marker file, points USERNAME (`sot-scratch-<id>`),
# USERPROFILE, HOME, APPDATA, LOCALAPPDATA, TEMP, TMP and SOT_COMM_HOME inside it, clears the other SOT_ variables, runs
# the suite again as a child PowerShell there with the same arguments, waits, removes exactly that home by its path, and
# sets $sotScratchExit to the child's exit code, so the suite's `exit` line ends the parent. The caller's own process
# environment is never changed. In the child (SOT_SCRATCH_CHILD names the home) this file checks the profile and sets
# $sotScratchExit to 2 with one stderr line unless it is a scratch profile; a child never starts another child.
# The check compares the environment with what an environment variable cannot move: the account's real name
# ([Environment]::UserName, from the OS) and its real profile folder from the process token (GetUserProfileDirectoryW;
# not [Environment]::GetFolderPath, which follows the USERPROFILE a process was started with, so in the child it names
# the scratch home). The real temp folder is <profile>\AppData\Local\Temp:
#   - USERNAME begins `sot-scratch`, is not the real name, and names the home's folder; the daemon's pipe,
#     `\\.\pipe\sot-<USERNAME>-local`, then begins `sot-sot-scratch`;
#   - the home is a folder directly under the real temp folder and holds the marker;
#   - USERPROFILE, HOME, APPDATA, LOCALAPPDATA, TEMP and TMP (and SOT_COMM_HOME when set) each lie strictly inside the
#     home, so none is, holds or lies inside a real folder.
# A suite that cannot find this file ends too: the line before it sets $ErrorActionPreference to Stop.
# `$sotScratchLibOnly = $true` set in the dot-sourcing scope itself (not inherited from a caller) loads the functions only
# (test-scratch-guard.ps1 does); a suite's own scope never has it, so a caller's variable cannot turn the guard off.
# Each run prints one line when the home is made and one when it is removed.
# Windows PowerShell 5.1, ASCII only.

if (-not ('SotScratchLongPath' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class SotScratchLongPath {
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern uint GetLongPathNameW(string shortPath, StringBuilder longPath, uint size);
    [DllImport("userenv.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern bool GetUserProfileDirectoryW(IntPtr token, StringBuilder path, ref uint size);
    // The profile folder of the account the process token belongs to; the environment does not move it.
    public static string ProfileOf(IntPtr token) {
        StringBuilder sb = new StringBuilder(1024);
        uint size = (uint)sb.Capacity;
        return GetUserProfileDirectoryW(token, sb, ref size) ? sb.ToString() : null;
    }
    public static string Expand(string path) {
        StringBuilder sb = new StringBuilder(1024);
        uint n = GetLongPathNameW(path, sb, (uint)sb.Capacity);
        return (n > 0 && n < sb.Capacity) ? sb.ToString() : null;
    }
}
'@
}

# A path as one comparable spelling: absolute, dot segments gone, 8.3 names expanded (on the deepest part that exists),
# no trailing backslash. $null for an empty or relative path.
function Get-SotCanonicalPath([string]$Path) {
    if (-not $Path -or -not [System.IO.Path]::IsPathRooted($Path)) { return $null }
    try { $full = [System.IO.Path]::GetFullPath($Path) } catch { return $null }
    $full = $full.TrimEnd('\')
    $tail = ''
    $head = $full
    while ($head -and -not (Test-Path -LiteralPath $head)) {
        $parent = [System.IO.Path]::GetDirectoryName($head)
        if (-not $parent) { break }
        $tail = '\' + [System.IO.Path]::GetFileName($head) + $tail
        $head = $parent
    }
    $long = $null
    if ($head) { $long = [SotScratchLongPath]::Expand($head) }
    if (-not $long) { $long = $head }
    return ($long.TrimEnd('\') + $tail)
}

# Whether $Inner lies strictly inside $Outer (both canonical).
function Test-SotInside([string]$Inner, [string]$Outer) {
    return ($Inner -and $Outer -and $Inner.StartsWith($Outer + '\', [System.StringComparison]::OrdinalIgnoreCase))
}

# The reason this environment is not a scratch profile, or $null when it is. The caller passes the values: $Vars is the
# process environment's USERNAME, USERPROFILE, HOME, APPDATA, LOCALAPPDATA, TEMP, TMP and SOT_COMM_HOME; $Real is what no
# variable moves: UserName and Temp (the account's own temp folder, under the profile of its token).
function Get-SotScratchRefusal([hashtable]$Vars, [hashtable]$Real) {
    $problems = @()
    $name = [string]$Vars.USERNAME
    if (-not ($name -like 'sot-scratch*')) { $problems += "USERNAME '$name' is not a scratch name (sot-scratch*)" }
    elseif ($name -ieq [string]$Real.UserName) { $problems += "USERNAME '$name' is the account's real name" }
    if (-not (("sot-$name-local") -like 'sot-sot-scratch*')) { $problems += "the daemon pipe prefix for '$name' is not scratch" }

    $realTemp = Get-SotCanonicalPath $Real.Temp
    $roots = @()
    foreach ($v in @('USERPROFILE', 'HOME', 'APPDATA', 'LOCALAPPDATA', 'TEMP', 'TMP', 'SOT_COMM_HOME')) {
        $raw = [string]$Vars[$v]
        if (-not $raw) {
            if ($v -eq 'HOME' -or $v -eq 'SOT_COMM_HOME') { continue }
            $problems += "$v is not set"
            continue
        }
        $c = Get-SotCanonicalPath $raw
        if (-not $c) { $problems += "$v '$raw' is not an absolute path"; continue }
        $roots += , @($v, $raw, $c)
    }
    # The scratch root: the parent of USERPROFILE, which must sit directly under the real temp folder and hold the marker.
    $root = $null
    $up = ($roots | Where-Object { $_[0] -eq 'USERPROFILE' } | Select-Object -First 1)
    if ($up) { $root = [System.IO.Path]::GetDirectoryName($up[2]) }
    if (-not $root -or -not $realTemp -or -not ((Test-SotInside $root $realTemp) -and ([System.IO.Path]::GetDirectoryName($root) -ieq $realTemp))) {
        $problems += "no scratch root: USERPROFILE's parent is not a folder directly under the account's temp folder"
    } else {
        if ([System.IO.Path]::GetFileName($root) -ine $name) { $problems += "USERNAME '$name' is not the name of the scratch root '$root'" }
        $marker = Join-Path $root 'sot-scratch-root.marker'
        $line = $null
        if (Test-Path -LiteralPath $marker) { $line = (Get-Content -LiteralPath $marker -TotalCount 1) }
        if (-not $line -or -not $line.StartsWith('sot-scratch-root ') -or ((Get-SotCanonicalPath $line.Substring(17)) -ine $root)) { $problems += "the scratch root '$root' has no marker of this guard" }
        foreach ($e in $roots) {
            if (-not (Test-SotInside $e[2] $root)) { $problems += "$($e[0]) '$($e[1])' is not inside the scratch root '$root'" }
        }
    }
    if ($problems.Count -eq 0) { return $null }
    return 'scratch-guard: refusing to start, this is not a scratch profile: ' + ($problems -join '; ') +
        '.'
}

function Get-SotRealProfile {
    $prof = [SotScratchLongPath]::ProfileOf([Security.Principal.WindowsIdentity]::GetCurrent().Token)
    if (-not $prof) { throw 'scratch-guard: could not read the account''s profile folder from its token' }
    return @{
        UserName = [Environment]::UserName
        Profile  = $prof
        Temp     = (Join-Path $prof 'AppData\Local\Temp')
    }
}

# A new home under the real temp folder: the folders, the marker. Returns its path.
function New-SotScratchHome([hashtable]$Real) {
    $id = [guid]::NewGuid().ToString('N').Substring(0, 8)
    $root = Join-Path $Real.Temp "sot-scratch-$id"
    $hm = Join-Path $root 'home'
    New-Item -ItemType Directory -Force -Path (Join-Path $hm 'AppData\Roaming'), (Join-Path $hm 'AppData\Local'), (Join-Path $root 'tmp'), (Join-Path $hm 'comm') | Out-Null
    $root = Get-SotCanonicalPath $root
    [System.IO.File]::WriteAllText((Join-Path $root 'sot-scratch-root.marker'), "sot-scratch-root $root`r`n")
    return $root
}

# Removes exactly one home, by its absolute path, and only if it is a home New-SotScratchHome made: directly under the
# real temp folder, named sot-scratch-*, holding its own marker. A junction a suite left is removed as a link first.
function Remove-SotScratchHome([string]$Root, [hashtable]$Real) {
    $c = Get-SotCanonicalPath $Root
    $realTemp = Get-SotCanonicalPath $Real.Temp
    $marker = $(if ($c) { Join-Path $c 'sot-scratch-root.marker' } else { '' })
    $ok = $c -and $realTemp -and ([System.IO.Path]::GetDirectoryName($c) -ieq $realTemp) -and ([System.IO.Path]::GetFileName($c) -like 'sot-scratch-*') -and
        (Test-Path -LiteralPath $marker) -and ((Get-Content -LiteralPath $marker -TotalCount 1) -ceq "sot-scratch-root $c")
    if (-not $ok) { [Console]::Error.WriteLine("scratch-guard: not removing '$Root': it is not a scratch home of this guard"); return }
    foreach ($r in @(Get-ChildItem -LiteralPath $c -Recurse -Force -Attributes ReparsePoint -ErrorAction SilentlyContinue)) {
        cmd /c rmdir "`"$($r.FullName)`"" 2>&1 | Out-Null
    }
    Remove-Item -LiteralPath $c -Recurse -Force -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $c) { cmd /c rmdir /s /q "`"$c`"" 2>&1 | Out-Null }
    if (Test-Path -LiteralPath $c) { [Console]::Error.WriteLine("scratch-guard: could not remove '$c'") }
    else { [Console]::Error.WriteLine("scratch-guard: removed $c") }
}

function Format-SotArg([string]$Value) {
    if ($Value.Contains('"')) { throw "scratch-guard: an argument holds a double quote: $Value" }
    return '"' + ($Value -replace '(\\+)$', '$1$1') + '"'
}

# Runs the suite again in a home of its own and returns the child's exit code.
function Invoke-SotScratchChild([string]$Suite, $Bound, [hashtable]$Real) {
    $root = New-SotScratchHome $Real
    try {
        $hm = Join-Path $root 'home'
        $psi = New-Object System.Diagnostics.ProcessStartInfo
        $psi.FileName = (Get-Process -Id $PID).Path
        $psi.UseShellExecute = $false
        $psi.WorkingDirectory = (Get-Location -PSProvider FileSystem).ProviderPath   # Set-Location does not move the process's own folder
        $argv = '-NoProfile -ExecutionPolicy Bypass -File ' + (Format-SotArg $Suite)
        foreach ($k in $Bound.Keys) {
            $v = $Bound[$k]
            if ($v -is [System.Management.Automation.SwitchParameter]) { if ($v.IsPresent) { $argv += " -$k" } }
            elseif ($v -is [string] -or $v -is [int] -or $v -is [long]) { $argv += " -$k " + (Format-SotArg ([string]$v)) }
            else { throw "scratch-guard: the argument -$k of the suite is not a string, number or switch" }
        }
        $psi.Arguments = $argv
        foreach ($k in @($psi.EnvironmentVariables.Keys)) {
            if ($k -like 'SOT_*' -and $k -notlike 'SOT_SMOKE_*' -and $k -ne 'SOT_JULIA_BIN') { $psi.EnvironmentVariables.Remove($k) }
        }
        $ev = $psi.EnvironmentVariables
        $ev['USERNAME'] = [System.IO.Path]::GetFileName($root)
        $ev['USERPROFILE'] = $hm; $ev['HOME'] = $hm
        $ev['APPDATA'] = Join-Path $hm 'AppData\Roaming'
        $ev['LOCALAPPDATA'] = Join-Path $hm 'AppData\Local'
        $ev['TEMP'] = Join-Path $root 'tmp'; $ev['TMP'] = $ev['TEMP']
        $ev['SOT_COMM_HOME'] = Join-Path $hm 'comm'
        $ev['SOT_SCRATCH_CHILD'] = $root
        [Console]::Error.WriteLine("scratch-guard: $([System.IO.Path]::GetFileName($Suite)) runs in $root")
        $p = [System.Diagnostics.Process]::Start($psi)
        $null = $p.Handle   # Windows PowerShell 5.1 reads ExitCode as empty unless the handle was taken while the process ran
        $p.WaitForExit()
        return $p.ExitCode
    } finally {
        Remove-SotScratchHome $root $Real
    }
}

$sotScratchExit = $null
if (-not (Get-Variable -Name sotScratchLibOnly -Scope 0 -ValueOnly -ErrorAction Ignore)) {
    $sotReal = Get-SotRealProfile
    $sotChild = [Environment]::GetEnvironmentVariable('SOT_SCRATCH_CHILD')
    if (-not $sotChild) {
        if (-not $sotSuite) { throw 'scratch-guard: the suite did not set $sotSuite' }
        $sotScratchExit = Invoke-SotScratchChild $sotSuite $sotSuiteArgs $sotReal
    } else {
        $sotEnv = @{}
        foreach ($n in @('USERNAME', 'USERPROFILE', 'HOME', 'APPDATA', 'LOCALAPPDATA', 'TEMP', 'TMP', 'SOT_COMM_HOME')) { $sotEnv[$n] = [Environment]::GetEnvironmentVariable($n) }
        $sotRefusal = Get-SotScratchRefusal $sotEnv $sotReal
        if (-not $sotRefusal -and ((Get-SotCanonicalPath $sotChild) -ine (Get-SotCanonicalPath (Split-Path -Parent $sotEnv.USERPROFILE)))) {
            $sotRefusal = 'scratch-guard: refusing to start: SOT_SCRATCH_CHILD does not name this profile''s home'
        }
        if ($sotRefusal) { [Console]::Error.WriteLine($sotRefusal); $sotScratchExit = 2 }
    }
}

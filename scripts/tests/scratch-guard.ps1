# scratch-guard.ps1 -- dot-sourced first by every Windows suite that can start or touch sot state: the suite refuses to
# start (one line on stderr, the process ends with exit 2) unless it runs under a scratch profile. A scratch profile is a USERNAME that begins
# `sot-scratch` (the daemon's pipe is `\\.\pipe\sot-<USERNAME>-local`, so this is also the pipe prefix) and APPDATA and
# LOCALAPPDATA inside a folder under TEMP. Hosted runs and a person's box get one the same way: `. scratch-profile.ps1`
# first, which makes such a profile. There is no switch that turns the check off. Windows PowerShell 5.1, ASCII only.

# The reason this environment is not a scratch profile, or $null when it is. Pure: the caller passes the values.
function Get-SotScratchRefusal([string]$UserName, [string]$LocalAppData, [string]$AppData, [string[]]$TempRoots) {
    $problems = @()
    if (-not ($UserName -like 'sot-scratch*')) { $problems += "USERNAME '$UserName' is not a scratch name (sot-scratch*)" }
    $roots = @($TempRoots | Where-Object { $_ } | ForEach-Object { [System.IO.Path]::GetFullPath($_).TrimEnd('\') + '\' })
    foreach ($p in @(@('LOCALAPPDATA', $LocalAppData), @('APPDATA', $AppData))) {
        $full = $null
        if ($p[1]) { try { $full = [System.IO.Path]::GetFullPath($p[1]) } catch { $full = $null } }
        $inside = $false
        if ($full -and [System.IO.Path]::IsPathRooted($p[1])) {
            foreach ($r in $roots) { if ($full.StartsWith($r, [System.StringComparison]::OrdinalIgnoreCase)) { $inside = $true } }
        }
        if (-not $inside) { $problems += "$($p[0]) '$($p[1])' is not inside a folder under TEMP" }
    }
    if ($problems.Count -eq 0) { return $null }
    return 'scratch-guard: refusing to start, this is the real profile: ' + ($problems -join '; ') +
        '. Run ". scripts\tests\scratch-profile.ps1" first.'
}

$sotRefusal = Get-SotScratchRefusal $env:USERNAME $env:LOCALAPPDATA $env:APPDATA @($env:TEMP, [System.IO.Path]::GetTempPath())
# Not `exit`: in a dot-sourced file that leaves only the guard and the suite goes on. The process ends.
if ($sotRefusal) { [Console]::Error.WriteLine($sotRefusal); [Environment]::Exit(2) }

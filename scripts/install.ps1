# install.ps1 -- install or update Ship of Tools on this Windows computer: the installer is the update.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File install.ps1 [-Version vX.Y.Z | -Archive <folder>] [-Hub <alias>]
#                                                                    [-Prefix <dir>] [-RestartDaemon]
#
# The interface joins scripts/CLAUDE.md's entry-point paths: the file and the option names never move. Arguments are
# parsed by hand from $args (not a param block) so that an unknown option or a missing value ends in the same single
# verdict line as every other exit, instead of PowerShell's binding error.
#
# With no -Version and no -Archive it follows the channel the install already runs (install.json's version): none
# installed or a stable release takes the latest stable release; a pre-release takes the newest published release of any
# kind; a build with a + stops (it is not a release; a pack of the line goes in with -Archive). It never moves the
# install backwards unless told (an older target found by the channel fails; -Version names it, and is said to be
# older). A run of the version already installed, with no daemon on another version, is a repair run whether or not it
# names the version: it writes the pin if it names one, drops any armed update pointer, and stages, applies and restarts
# nothing (a named one downloads no release asset; the prelude still fetches the tag's install.ps1 unless SOT_INSTALL_TAG
# is set). A run that names what to install pins the install: install.json records "pinned", the PASS or ONE STEP LEFT
# line ends "; pinned at <v>: ...", and the updaters leave it where the person put it until a run names no version.
#
# The prelude fetches the target tag's own install.ps1 and runs it (SOT_INSTALL_TAG, SOT_INSTALL_IMPLICIT); -Archive
# skips it and runs this file, which must be the packing checkout's. A fresh install lays down bin\, repo\base,
# repo\versions\v<v> and the repo\current junction, then comm and the shortcut. An update stages the target with its own
# `sotd update stage` and applies it one way, by what runs here: a supervisor that logs its launcher code id gets the
# converge sentinel (rows keep their sessions); an older supervisor or a window with none gets the update left armed and
# a step; nothing running and a desktop session applies it here; nothing running over ssh leaves it armed. The last
# line is exactly one of PASS, ONE STEP LEFT or FAIL (which also says the pin and any update still armed), then an explicit
# exit. A run can outlast an agent's tool call (the stage prepares Julia, the converge waits up to 30 minutes): the staging
# line says so, and the verdict is the last line.
#
# repo\current is read once by its target and never traversed, except by the one deliberate probe that runs
# install-shortcut.ps1 through it (Invoke-Shortcut): an ssh session on a box with redirection trust enabled cannot
# traverse the junction, so every other path used here is repo\versions\<tag>.
#
# Test seams (the hermetic suite): SOT_INSTALL_REPO (owner/name), SOT_INSTALL_ORIGIN (the clone URL),
# SOT_INSTALL_CONVERGE_WAIT_S, SOT_INSTALL_DAEMON_WAIT_S. The suite dot-sources this file and replaces the network
# functions (Invoke-CurlText, Invoke-CurlFile).
#
# ASCII ONLY in string literals: Windows PowerShell 5.1 decodes a BOM-less .ps1 as Windows-1252.

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$script:StopMarker = 'sot-install-stop'
# The release repo the downloads and the clone come from (install.sh's REPO).
$script:Repo = $(if ($env:SOT_INSTALL_REPO) { $env:SOT_INSTALL_REPO } else { 'kalidke/ship-of-tools' })
$script:launchNotices = New-Object System.Collections.ArrayList

# ---- the one verdict -------------------------------------------------------------------------------------------------
function Write-Verdict([string]$Line) {
    if ($script:Done) { return }
    $script:Done = $true
    Write-EntryLine $(if ($Line.StartsWith('FAIL: ')) { "$Line$(Get-FailTail)" } else { $Line })
}
# What a FAIL leaves behind, said at its end: an update this run staged that is still armed (the next start applies it),
# and the pin once the run wrote it (PASS and ONE STEP LEFT carry the pin themselves).
function Get-FailTail {
    $armed = $script:ArmedTag -and $script:PendingFile -and (Test-Path -LiteralPath $script:PendingFile)
    return "$(if ($armed) { "; the update to $($script:ArmedTag) stays armed: the next start applies it" })$(if ($script:PinWritten) { $script:PinSuffix })"
}
function Stop-Entry([string]$Check, [string]$Detail, [int]$Code = 1) {
    $script:Rc = $Code
    Write-Verdict "FAIL: ${Check}: $Detail"
    throw $script:StopMarker
}
# Every line this entry prints goes through here (the suite replaces it to keep a run's lines in order).
function Write-EntryLine([string]$Text) { Write-Host $Text }
function Say([string]$Text) { Write-EntryLine $Text }
function Add-Step([string]$Text) { $script:Steps += $Text }

# stdout and stderr of a native command, apart, with its exit code; never throws (PowerShell 5.1 turns native stderr into
# terminating errors under 'Stop').
function Invoke-Native([string]$Exe, [string[]]$ArgList) {
    $eap = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    $out = @(); $err = @(); $code = -1
    try {
        foreach ($l in @(& $Exe @ArgList 2>&1)) {
            if ($l -is [System.Management.Automation.ErrorRecord]) { $err += "$l" } else { $out += "$l" }
        }
        $code = $LASTEXITCODE
        if ($null -eq $code) { $code = 0 }
    } catch {
        $err += $_.Exception.Message
    } finally {
        $ErrorActionPreference = $eap
    }
    return [pscustomobject]@{ Code = $code; Out = $out; Err = $err }
}

# ---- the network (replaced by the suite) -----------------------------------------------------------------------------
$script:CurlBound = @('--connect-timeout', '15', '--speed-limit', '1000', '--speed-time', '60')
function Invoke-CurlText([string]$Url, [switch]$Head) {
    $a = @($script:CurlBound) + @('-fsSL')
    if ($Head) { $a = @($script:CurlBound) + @('-fsSLI', '-o', 'NUL', '-w', '%{url_effective}') }
    $a += $Url
    $r = Invoke-Native 'curl.exe' $a
    if ($r.Code -ne 0) { return [pscustomobject]@{ Ok = $false; Code = $r.Code; Text = '' } }
    return [pscustomobject]@{ Ok = $true; Code = 0; Text = ($r.Out -join "`n") }
}
function Invoke-CurlFile([string]$Url, [string]$Path) {
    $r = Invoke-Native 'curl.exe' (@($script:CurlBound) + @('-fsSL', '-o', $Path, $Url))
    return [pscustomobject]@{ Ok = ($r.Code -eq 0); Code = $r.Code }
}

# ---- small readers ---------------------------------------------------------------------------------------------------
function Get-JsonString([string]$Path, [string]$Key) {
    try {
        $t = [System.IO.File]::ReadAllText($Path)
        if ($t -match ('"' + $Key + '"\s*:\s*"([^"]*)"')) { return $Matches[1] }
    } catch { }
    return ''
}
function Write-Utf8NoBom([string]$Path, [string]$Text) {
    [System.IO.File]::WriteAllText($Path, $Text, (New-Object System.Text.UTF8Encoding($false)))
}

# The pin in install.json: a surgical edit, never a parse and reserialize (the schema is additive, and sot-apply.ps1
# rewrites the same file field by field).
function Set-InstallPin([string]$InstallJson, [bool]$Pinned) {
    if (-not (Test-Path -LiteralPath $InstallJson)) { return }
    $t = [System.IO.File]::ReadAllText($InstallJson)
    $new = [regex]::Replace($t, ',\s*"pinned"\s*:\s*(true|false)', '')
    if ($Pinned) { $new = [regex]::Replace($new, '\s*\}\s*$', ",`n  `"pinned`": true`n}") }
    if ($new -cne $t) { Write-Utf8NoBom $InstallJson $new }
    $script:PinWritten = $Pinned
}

# What a binary says it is: "<name> <version> (<sha> <date>)".
function Get-BinaryVersion([string]$Exe) {
    $r = Invoke-Native $Exe @('--version')
    $line = ''
    if ($r.Out.Count -gt 0) { $line = $r.Out[0] }
    if ($r.Code -eq 0 -and $line -match '^\s*\S+\s+(\S+)(\s+\(([0-9a-f]+)\b)?') {
        return [pscustomobject]@{ Version = $Matches[1]; Sha = $Matches[3]; Line = $line }
    }
    return [pscustomobject]@{ Version = ''; Sha = ''; Line = $line }
}

# ---- the channel (E1) ------------------------------------------------------------------------------------------------
# 'stop' (a build with a +), 'newest' (a pre-release) or 'latest' (none installed, or a stable release).
function Get-InstallerChannel([string]$Installed) {
    if (-not $Installed) { return 'latest' }
    if ($Installed.Contains('+')) { return 'stop' }
    if ($Installed.Contains('-')) { return 'newest' }
    return 'latest'
}
function Resolve-ChannelTag([string]$Installed) {
    switch (Get-InstallerChannel $Installed) {
        'stop' {
            Stop-Entry 'target' "this computer runs $Installed, a build of the line, not a release: install a pack of the line with -Archive <folder>; a release named with -Version is usually older and moves this computer back; ask the person who runs it which they want" 2
        }
        'newest' {
            $r = Invoke-CurlText "https://api.github.com/repos/$($script:Repo)/releases?per_page=1"
            if (-not $r.Ok) { Stop-Entry 'target' "could not list the releases of $($script:Repo)" }
            if ($r.Text -match '"tag_name"\s*:\s*"([^"]*)"') { return $Matches[1] }
            return ''
        }
        default {
            $r = Invoke-CurlText "https://github.com/$($script:Repo)/releases/latest" -Head
            if (-not $r.Ok) { Stop-Entry 'target' "could not resolve the latest release of $($script:Repo)" }
            return (($r.Text -split '/')[-1])
        }
    }
}

# ---- options ---------------------------------------------------------------------------------------------------------
function Read-Options([string[]]$Argv) {
    $o = @{ Version = ''; Archive = ''; Hub = ''; Prefix = ''; RestartDaemon = $false }
    $i = 0
    while ($i -lt $Argv.Count) {
        $a = [string]$Argv[$i]
        switch -Regex ($a) {
            '^-(Version|Archive|Hub|Prefix)$' {
                $key = $Matches[1]
                if ($i + 1 -ge $Argv.Count -or ([string]$Argv[$i + 1]).StartsWith('-')) { Stop-Entry 'options' "$a needs a value" 2 }
                $o[$key] = [string]$Argv[$i + 1]; $i++
            }
            '^-RestartDaemon$' { $o.RestartDaemon = $true }
            default { Stop-Entry 'options' "unknown option: $a" 2 }
        }
        $i++
    }
    if ($o.Version -and $o.Archive) { Stop-Entry 'options' '-Archive and -Version are exclusive: the folder names its own version' 2 }
    return $o
}
# An absolute prefix with no trailing backslash (a relative one breaks the junction compare, and a trailing backslash
# breaks native-argument quoting in Windows PowerShell 5.1).
function Resolve-PrefixPath([string]$Path) {
    if (-not [System.IO.Path]::IsPathRooted($Path)) { $Path = Join-Path (Get-Location).Path $Path }
    $full = [System.IO.Path]::GetFullPath($Path)
    if ($full.Length -gt 3) { $full = $full.TrimEnd('\') }
    return $full
}

# ---- preflight (W-1) -------------------------------------------------------------------------------------------------
function Test-Preflight {
    $script:Step = 'preflight'
    if ($env:OS -ne 'Windows_NT') { Stop-Entry 'preflight' 'this installer is for Windows; on Linux and macOS run scripts/install.sh' 2 }
    $arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
    if ($arch -ne 'AMD64') { Stop-Entry 'preflight' "Ship of Tools needs Windows x86_64 (this is $arch)" 2 }
    if (-not (Get-Command git.exe -ErrorAction SilentlyContinue)) { Stop-Entry 'preflight' 'git is not on PATH: winget install --id Git.Git -e' 2 }
    if (-not (Get-Command curl.exe -ErrorAction SilentlyContinue)) { Stop-Entry 'preflight' 'curl.exe is not on PATH (it ships with Windows 10 version 1803 and later); update Windows or put curl.exe on PATH' 2 }
}

# ---- the prelude (E3) ------------------------------------------------------------------------------------------------
# Run the target tag's own install.ps1 with the same arguments and end with its verdict. A child that ends without one is
# a FAIL here, so a run never ends silent.
function Invoke-Prelude([string]$Tag, [bool]$Implicit, [string[]]$Argv, [string]$Scratch) {
    $script:Step = 'prelude'
    $url = "https://raw.githubusercontent.com/$($script:Repo)/$Tag/scripts/install.ps1"
    $tmp = Join-Path $Scratch 'install-target.ps1'
    $r = Invoke-CurlFile $url $tmp
    if (-not $r.Ok) {
        Stop-Entry 'target' "$Tag has no scripts/install.ps1 (or $url could not be fetched): install that release by its own procedure, docs/INSTALL-AGENT.md at $Tag"
    }
    $env:SOT_INSTALL_TAG = $Tag
    $env:SOT_INSTALL_IMPLICIT = $(if ($Implicit) { '1' } else { '0' })
    $verdict = $false; $last = ''
    # The child prints the one verdict, so Ctrl+C while it runs must not make this run print a second.
    $script:Done = $true
    try {
        $eap = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
        & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $tmp @Argv 2>&1 | ForEach-Object {
            $l = "$_"
            Write-EntryLine $l
            if ($l -match '^(PASS|FAIL|ONE STEP LEFT): ') { $verdict = $true }
            $last = $l
        }
        $code = $LASTEXITCODE
        $ErrorActionPreference = $eap
    } finally {
        Remove-Item Env:SOT_INSTALL_TAG -ErrorAction SilentlyContinue
        Remove-Item Env:SOT_INSTALL_IMPLICIT -ErrorAction SilentlyContinue
    }
    if (-not $verdict) { $script:Done = $false; Stop-Entry 'prelude' "the installer of $Tag ended without a verdict (exit $code)" }
    return [int]$code
}

# ---- fetch and verify (W-2) ------------------------------------------------------------------------------------------
# Returns the pack: Dir (SHA256SUMS, the zip, COMMIT), Version, Tag, Commit, Zip.
function Get-Pack([hashtable]$O, [string]$Tag, [string]$Scratch) {
    $script:Step = 'fetch'
    $dir = ''
    if ($O.Archive) {
        if (-not (Test-Path -LiteralPath $O.Archive -PathType Container)) { Stop-Entry 'fetch' "-Archive: no such folder: $($O.Archive)" 2 }
        $dir = (Resolve-Path -LiteralPath $O.Archive).Path
    } else {
        $dir = Join-Path $Scratch 'pack'
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
        $base = "https://github.com/$($script:Repo)/releases/download/$Tag"
        $ver = $Tag.TrimStart('v')
        foreach ($f in @('SHA256SUMS', "sot-$ver-windows-x86_64.zip", 'COMMIT')) {
            $r = Invoke-CurlFile "$base/$f" (Join-Path $dir $f)
            if (-not $r.Ok) { Stop-Entry 'fetch' "could not download $base/$f (curl exit $($r.Code))" }
        }
    }
    $sumsPath = Join-Path $dir 'SHA256SUMS'
    if (-not (Test-Path -LiteralPath $sumsPath)) { Stop-Entry 'fetch' "$dir has no SHA256SUMS" }
    $sums = @{}
    foreach ($l in [System.IO.File]::ReadAllLines($sumsPath)) {
        if ($l -match '^([0-9a-fA-F]{64}) [ *](.+)$') { $sums[$Matches[2]] = $Matches[1].ToLowerInvariant() }
    }
    # Asset names are compared as text, never as patterns: a version carries a + .
    $zips = @($sums.Keys | Where-Object { $_.StartsWith('sot-') -and $_.EndsWith('-windows-x86_64.zip') })
    if ($zips.Count -ne 1) { Stop-Entry 'fetch' "$sumsPath must name exactly one sot-<version>-windows-x86_64.zip (it names $($zips.Count))" }
    $zipName = $zips[0]
    $ver = $zipName.Substring(4, $zipName.Length - 4 - '-windows-x86_64.zip'.Length)
    if ($ver -notmatch '^[0-9A-Za-z.+-]+$') { Stop-Entry 'fetch' "unsafe version '$ver' in $sumsPath" }
    if ($Tag -and $Tag -ne "v$ver") { Stop-Entry 'fetch' "$sumsPath names $ver, not $Tag" }
    if (-not $sums.ContainsKey('COMMIT')) { Stop-Entry 'fetch' "$sumsPath does not cover COMMIT" }
    foreach ($f in @($zipName, 'COMMIT')) {
        $p = Join-Path $dir $f
        if (-not (Test-Path -LiteralPath $p)) { Stop-Entry 'fetch' "$dir has no $f" }
        $h = (Get-FileHash -Algorithm SHA256 -LiteralPath $p).Hash.ToLowerInvariant()
        if ($h -ne $sums[$f]) { Stop-Entry 'fetch' "$f does not match its SHA256SUMS entry (got $h)" }
    }
    $commit = ([System.IO.File]::ReadAllText((Join-Path $dir 'COMMIT'))).Trim()
    if ($commit -notmatch '^[0-9a-f]{40}$') { Stop-Entry 'fetch' "$dir\COMMIT is not a 40-digit commit id" }
    $x = Join-Path $Scratch 'x'
    Expand-Archive -LiteralPath (Join-Path $dir $zipName) -DestinationPath $x -Force
    $inner = Join-Path $x ("sot-$ver-windows-x86_64")
    foreach ($e in @('sot.exe', 'sotd.exe', 'sot-capsule.exe')) {
        if (-not (Test-Path -LiteralPath (Join-Path $inner $e))) { Stop-Entry 'fetch' "$zipName holds no $e" }
    }
    return [pscustomobject]@{ Dir = $dir; Version = $ver; Tag = "v$ver"; Commit = $commit; Bin = $inner }
}

# ---- the daemon (live-work ask) --------------------------------------------------------------------------------------
function Get-DaemonPipe([string]$Sotd) {
    $r = Invoke-Native $Sotd @('session-socket-path', 'local')
    if ($r.Code -eq 0 -and $r.Out.Count -gt 0 -and $r.Out[0].StartsWith('\\.\pipe\')) { return $r.Out[0] }
    return ''
}
# Does a daemon run here: its pipe exists (listed, never opened: a probe must not take an instance), or a process runs
# from this install's bin\sotd.exe or the image sot-apply.ps1 renamed aside (bin\sotd-stale-*.exe).
function Test-DaemonRuns([string]$Pipe, [string]$BinExe) {
    if ($Pipe) {
        $listed = @()
        # When the listing throws, the process leg below decides.
        try { $listed = @([System.IO.Directory]::GetFiles('\\.\pipe\')) } catch { }
        if (@($listed | Where-Object { $_ -ieq $Pipe }).Count -gt 0) { return $true }
    }
    if ($BinExe) {
        $stale = Join-Path (Split-Path $BinExe) 'sotd-stale-*.exe'
        foreach ($p in @(Get-Process -Name 'sotd*' -ErrorAction SilentlyContinue)) {
            $path = ''
            try { $path = $p.Path } catch { }
            if ($path -and (($path -ieq $BinExe) -or ($path -like $stale))) { return $true }
        }
    }
    return $false
}
# 'current' (runs the version of $Sotd), 'other' (another version), 'unknown' (a daemon runs and cannot be told: why is in
# $script:DaemonWhy) or 'none'. A daemon that cannot be told is never taken for none.
function Get-DaemonState([string]$Sotd, [string]$Pipe, [string]$BinExe) {
    $script:DaemonWhy = ''
    if ($Pipe) {
        $r = Invoke-Native $Sotd @('same-version', '--endpoint', "pipe:$Pipe")
        switch ($r.Code) { 0 { return 'current' } 3 { return 'other' } 4 { return 'other' } }
        $script:DaemonWhy = $(if ($r.Err.Count -gt 0) { $r.Err[0] } else { "exit $($r.Code)" })
    }
    if (Test-DaemonRuns $Pipe $BinExe) { return 'unknown' }
    return 'none'
}
function Wait-DaemonCurrent([string]$Sotd, [string]$Pipe, [string]$BinExe, [int]$Seconds) {
    $t0 = Get-Date
    while ($true) {
        $st = Get-DaemonState $Sotd $Pipe $BinExe
        if ($st -eq 'current') { return $st }
        if (((Get-Date) - $t0).TotalSeconds -ge $Seconds) { return $st }
        Start-Sleep -Seconds 1
    }
}
# What a restart of the daemon would end: Rows, Pluto, Unsure (the daemon could not be asked in full; Reason says why).
function Get-LiveWork([string]$Sotd, [string]$Pipe) {
    $r = Invoke-Native $Sotd @('update', 'repls', '--endpoint', "pipe:$Pipe")
    $rows = @(); $pluto = $false
    foreach ($l in $r.Out) {
        if ($l -match '^repl (.+)$') { $rows += $Matches[1] } elseif ($l -match '^pluto server') { $pluto = $true }
    }
    $unsure = ($r.Code -ne 0 -and $r.Code -ne 3)
    $why = ''
    if ($unsure) { $why = $(if ($r.Err.Count -gt 0) { $r.Err[0] } else { "exit $($r.Code)" }) }
    return [pscustomobject]@{ Rows = $rows; Pluto = $pluto; Unsure = $unsure; Reason = $why }
}
function Format-LiveWork($Work, [string]$Lead) {
    $out = ''
    if ($Work.Rows.Count -gt 0) { $out = "$Lead the REPLs of: $($Work.Rows -join ', ')" }
    if ($Work.Pluto) {
        $p = 'the Pluto server is running: restarting the daemon ends every notebook open in it'
        $out = $(if ($out) { "$out; $p" } else { $p })
    }
    return $out
}

# What a restart of the daemon would end, as the ask's reason ('' when nothing live and nothing unsure).
function Get-LiveText([string]$State, [string]$Pipe, $Work) {
    $is = $(if ($State -eq 'current') { 'is running' } else { 'runs another version' })
    if ($State -eq 'unknown') {
        return "a daemon runs on $Pipe and could not be told its version ($script:DaemonWhy)$(if ($Work.Rows.Count) { "; restarting it would end the REPLs of: $($Work.Rows -join ', ')" } else { '; restarting it could end live REPLs and Pluto notebooks' })"
    }
    if ($Work.Unsure) {
        return "could not ask the daemon on $Pipe in full: it $is$(if ($Work.Rows.Count) { ", and restarting it would end the REPLs of: $($Work.Rows -join ', ')" }); sotd update repls: $($Work.Reason)"
    }
    $t = Format-LiveWork $Work 'restarting it would end'
    if ($t) { $t = "the daemon on $Pipe $is, and $t" }
    return $t
}

# ---- what runs here (W-5) --------------------------------------------------------------------------------------------
function Get-ProcessCommandLine([int]$ProcessId) {
    try { return [string](Get-CimInstance Win32_Process -Filter "ProcessId = $ProcessId" -ErrorAction Stop).CommandLine } catch { return '' }
}
# The resident supervisor: its pid (launcher.pid names a live launch-sot.ps1) and whether its last start line carries
# code= (rc9.13 and later; an older one never re-invokes after a converge).
function Get-Supervisor([string]$LogDir) {
    $pidv = 0
    try { $pidv = [int](Get-Content -LiteralPath (Join-Path $LogDir 'launcher.pid') -ErrorAction Stop | Select-Object -First 1) } catch { return $null }
    if ($pidv -le 0 -or (Get-ProcessCommandLine $pidv) -notlike '*launch-sot.ps1*') { return $null }
    $code = ''
    $log = Join-Path $LogDir 'supervisor.log'
    if (Test-Path -LiteralPath $log) {
        $starts = @(Select-String -LiteralPath $log -Pattern "pid=$pidv\s+supervisor start" -ErrorAction SilentlyContinue)
        if ($starts.Count -gt 0 -and $starts[-1].Line -match 'code=(\S+)' -and $Matches[1] -ne 'unknown') { $code = $Matches[1] }
    }
    return [pscustomobject]@{ Pid = $pidv; Code = $code }
}
# An ssh logon runs in session 0, the desk's in a later one (seen on a lab PC); the suite replaces this.
function Get-EntrySessionId { return [System.Diagnostics.Process]::GetCurrentProcess().SessionId }
function Test-WindowRuns([string]$BinDir) {
    $exe = Join-Path $BinDir 'sot.exe'
    foreach ($p in @(Get-Process -Name sot -ErrorAction SilentlyContinue)) {
        $path = ''
        try { $path = $p.Path } catch { }
        if ($path -and ($path -ieq $exe)) { return $true }
    }
    return $false
}

# ---- the layout, comm and shortcut (the launcher's own code) -----------------------------------------------------------
# Dot-sources the TARGET checkout's sot-install-layout.ps1 and sot-freshness.ps1 and calls the same functions the
# launcher calls, so the entry carries no copy of that logic. Runs in one function scope: those functions read the
# caller's variables.
function Invoke-LayoutAndComm([string]$PrefixDir, [string]$Checkout, [string]$BaseRepo) {
    $script:Step = 'layout'
    $logDir = Join-Path $env:LOCALAPPDATA 'sot\logs'
    New-Item -ItemType Directory -Force -Path $logDir | Out-Null
    $supervisorLog = Join-Path $logDir 'supervisor.log'
    function Write-SupLog { param([string]$Message)
        Write-EntryLine "  $Message"
        try { "$(Get-Date -Format o)  pid=$PID  install.ps1: $Message" | Out-File -FilePath $supervisorLog -Append -Encoding utf8 } catch { }
    }
    function Set-LaunchStatus { param([string]$Message) Write-EntryLine "  $Message" }
    $prefixDir = $PrefixDir
    # The junction is read, never traversed (an ssh session on a box with redirection trust cannot): the launcher's
    # functions get the target's path when the junction exists and the junction's own path when they are to create it.
    $repoCurrent = Join-Path $PrefixDir 'repo\current'
    if (Get-Item -LiteralPath $repoCurrent -Force -ErrorAction SilentlyContinue) { $repoCurrent = $Checkout }
    $backendExe = $null
    $NoUpdate = $false
    $Local = $false
    $PSScriptRoot_ = Join-Path $Checkout 'scripts'
    . (Join-Path $PSScriptRoot_ 'sot-install-layout.ps1')
    . (Join-Path $PSScriptRoot_ 'sot-freshness.ps1')
    # Initialize-InstallLayout reads $repo as the base clone; the comm pass reads $repo.Path as the checkout.
    $repo = $BaseRepo
    Initialize-InstallLayout
    $script:Step = 'comm'
    $repo = [pscustomobject]@{ Path = $Checkout }
    $script:sotPinned = $true
    Invoke-FreshnessPass
    foreach ($n in @($script:launchNotices)) { Say "  note: $n" }
}

function Invoke-Script([string]$Step, [string]$Script, [string[]]$ArgList, [string]$Hub) {
    $script:Step = $Step; $r = Invoke-Native 'powershell.exe' (@('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $Script) + $ArgList + $(if ($Hub) { @('-Hub', $Hub) }))
    foreach ($l in @($r.Out + $r.Err)) { if ("$l".Trim()) { Say "  $l" } }
    $joined = (@($r.Out + $r.Err) -join ' ') -replace '\s+', ' '  # the console wraps a long warning
    if ($joined -match 'does not list this box') { $script:EnrollRefusal = $joined }
    if ($r.Code -ne 0) { Stop-Entry $Step "$(Split-Path -Leaf $Script) ended with exit $($r.Code)" }
}

# Can this session read a file through repo\current (the suite replaces this to reach the branch that cannot)?
function Test-JunctionTraversable([string]$ScriptPath) { return (Test-Path -LiteralPath $ScriptPath) }
# The shortcut and pin: install-shortcut.ps1 through repo\current when this session can traverse it (the shortcut's working
# folder is then the junction, not a versions folder a later apply prunes); over ssh on a box that enforces redirection
# trust it is kept when it already targets the junction, else a step for the desk.
function Invoke-Shortcut([string]$PrefixDir, [string]$Hub) {
    $script:Step = 'shortcut'
    $viaJunction = Join-Path $PrefixDir 'repo\current\scripts\install-shortcut.ps1'
    if (Test-JunctionTraversable $viaJunction) {
        Invoke-Script 'shortcut' $viaJunction @('-Prefix', $PrefixDir) $Hub
        return
    }
    $lnk = Join-Path $env:USERPROFILE 'Desktop\Ship of Tools.lnk'
    $target = ''
    try { $target = (New-Object -ComObject WScript.Shell).CreateShortcut($lnk).Arguments } catch { }
    if ($target -like '*repo\current\scripts\launch-sot.ps1*') { Say '  shortcut: kept (repo\current cannot be traversed in this session, and the shortcut already targets it)'; return }
    Add-Step 'run this again at the desk, not over ssh: this session cannot traverse repo\current, so the Desktop shortcut was not written'
}

# ---- the update path (W-5) -------------------------------------------------------------------------------------------
# The checkout's own relaunch-sot.ps1 is the sentinel's one writer: installed version first, else the target's.
function Get-RelaunchScript([string]$Prefix, [string]$InstalledVersion, [string]$TargetTag) {
    foreach ($tag in @("v$InstalledVersion", $TargetTag)) {
        $c = Join-Path $Prefix "repo\versions\$tag\scripts\relaunch-sot.ps1"
        if (Test-Path -LiteralPath $c) { return $c }
    }
    Stop-Entry 'apply' "no relaunch-sot.ps1 under $Prefix\repo\versions to ask the window's supervisor with"
}
function Wait-Converge([string]$LogDir, [string]$Tag, [int]$Seconds, [string]$Relaunch) {
    $script:Step = 'apply'
    $log = Join-Path $LogDir 'supervisor.log'
    $start = 0
    if (Test-Path -LiteralPath $log) { $start = (Get-Item -LiteralPath $log).Length }
    $sentinel = Join-Path $env:LOCALAPPDATA 'sot\relaunch.request'
    $q = Invoke-Native 'powershell.exe' @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $Relaunch, '-Converge')
    if ($q.Code -ne 0) { Stop-Entry 'apply' "relaunch-sot.ps1 -Converge ended with exit $($q.Code): $((@($q.Out + $q.Err) | Select-Object -Last 2) -join ' / ')" }
    Say "converge requested: waiting for the window's supervisor to apply $Tag (up to $Seconds s)"
    $t0 = Get-Date; $applied = $false; $lastNote = $t0
    try {
        while (((Get-Date) - $t0).TotalSeconds -lt $Seconds) {
            Start-Sleep -Seconds 1
            if (Test-Path -LiteralPath $log) {
                $fs = [System.IO.File]::Open($log, 'Open', 'Read', 'ReadWrite')
                try {
                    $fs.Seek([Math]::Min($start, $fs.Length), 'Begin') | Out-Null
                    $text = (New-Object System.IO.StreamReader($fs)).ReadToEnd()
                } finally { $fs.Dispose() }
                if ($text -match "sot-apply: APPLIED $([regex]::Escape($Tag))") { $applied = $true }
                if ($applied -and $text -match 'status: DONE' -and $text.LastIndexOf('status: DONE') -gt $text.LastIndexOf('sot-apply: APPLIED')) { return $true }
                if ($text -match 'restoring previous binaries') { Stop-Entry 'apply' "the supervisor's apply did not complete: see $log" }
            }
            if (((Get-Date) - $lastNote).TotalSeconds -ge 30) { $lastNote = Get-Date; Say "  still waiting ($([int]((Get-Date) - $t0).TotalSeconds) s)" }
        }
    } finally {
        # A sentinel nobody consumed would make the next window converge the moment it starts.
        Remove-Item -LiteralPath $sentinel -Force -ErrorAction SilentlyContinue
    }
    Stop-Entry 'apply' "the supervisor did not report $Tag applied within $Seconds s: see $log"
}

# ---- the end checks (E6) ---------------------------------------------------------------------------------------------
# The id of the launcher code in a scripts folder: the launcher's own function, loaded into this function's scope.
function Get-LauncherCode([string]$ScriptsDir) {
    . (Join-Path $ScriptsDir 'sot-install-layout.ps1')
    return (Get-SotLauncherCodeId -ScriptsDir $ScriptsDir)
}
function Test-EndState([string]$PrefixDir, $Pack, [string]$Sotd, [string]$Pipe, [string]$LogDir) {
    $script:Step = 'check'
    $v = $Pack.Version; $short = $Pack.Commit.Substring(0, 9)
    foreach ($b in @('sotd', 'sot')) {
        $r = Get-BinaryVersion (Join-Path $PrefixDir "bin\$b.exe")
        if ($r.Version -ne $v) { Stop-Entry 'binaries' "bin\$b.exe reports '$($r.Line)', not $v" }
        if ($r.Sha -and -not $Pack.Commit.StartsWith($r.Sha)) { Stop-Entry 'binaries' "bin\$b.exe reports commit $($r.Sha), not $short" }
    }
    $ij = Join-Path $PrefixDir 'install.json'
    if ((Get-JsonString $ij 'version') -ne $v -or (Get-JsonString $ij 'commit') -ne $Pack.Commit) {
        Stop-Entry 'record' "$ij does not name $v at $short"
    }
    $target = $null
    try { $target = (Get-Item -LiteralPath (Join-Path $PrefixDir 'repo\current') -Force -ErrorAction Stop).Target } catch { }
    $want = Join-Path $PrefixDir "repo\versions\$($Pack.Tag)"
    if (-not $target) { Stop-Entry 'checkout' "$PrefixDir\repo\current is missing or is not a junction" }
    if ((@($target)[0] -replace '^\\\?\?\\', '') -ine $want) { Stop-Entry 'checkout' "repo\current points at $(@($target)[0]), not $want" }
    if (-not (Test-Path -LiteralPath (Join-Path $want 'scripts\launch-sot.ps1'))) { Stop-Entry 'checkout' "$want holds no launcher" }
    if (Test-Path -LiteralPath (Join-Path $PrefixDir 'updates\pending-windows-x86_64.json')) { Stop-Entry 'pointer' 'an update pointer is still armed after this run' }
    $stamp = Join-Path $(if ($env:SOT_COMM_HOME) { $env:SOT_COMM_HOME } else { Join-Path $env:USERPROFILE '.sot-comm' }) 'VERSION'
    if (Get-Command julia -ErrorAction SilentlyContinue) {
        $have = ''
        if (Test-Path -LiteralPath $stamp) { $have = ([System.IO.File]::ReadAllText($stamp)).Trim() -replace '-dirty$', '' }
        if ($have -ne $Pack.Commit.Substring(0, 9)) { Stop-Entry 'comm' "agent comm is at '$have', not $($Pack.Commit.Substring(0, 9))" }
        $man = Join-Path $want 'julia\kernel\Manifest.toml'
        if (-not (Test-Path -LiteralPath $man)) { Stop-Entry 'julia' "the kernel environment was never instantiated ($man is missing)" }
    } else {
        Add-Step 'install Julia (winget install --id Julialang.Juliaup -e); agent comm and local REPLs need it, then run this again'
    }
    if (-not (Get-Command jq -ErrorAction SilentlyContinue)) { Add-Step 'install jq (winget install jqlang.jq); agent comm needs it, then run this again' }
    # The window is on this launcher: its supervisor logged the installed launcher's code id (a converge re-invokes the
    # launcher, so one that did not logs an older id, or none).
    $sup = Get-Supervisor $LogDir
    if ($sup) {
        $wantCode = Get-LauncherCode (Join-Path $want 'scripts')
        if ($wantCode -and $sup.Code -ne $wantCode) { Add-Step 'quit Ship of Tools, choose Keep, and start it from the shortcut; the window runs an older launcher' }
    }
    # A daemon the converge restarted is verified by the clock: it answers as the target within the bound, or this is a FAIL.
    $st = Get-DaemonState $Sotd $Pipe $Sotd
    if ($script:ExpectDaemon) {
        $bound = $(if ($env:SOT_INSTALL_DAEMON_WAIT_S) { [int]$env:SOT_INSTALL_DAEMON_WAIT_S } else { 60 })
        $st = Wait-DaemonCurrent $Sotd $Pipe $Sotd $bound
        if ($st -ne 'current') { Stop-Entry 'daemon' "the daemon on $Pipe did not answer as $v within $bound s after the converge (it is '$st')" }
    }
    if ($st -eq 'unknown') { Stop-Entry 'daemon' "a daemon runs on $Pipe and could not be told its version ($script:DaemonWhy)" }
    if ($st -eq 'other') { Stop-Entry 'daemon' "the daemon on $Pipe still runs another version than $v" }
}

# ---- the entry ---------------------------------------------------------------------------------------------------------
# The installed build as a pack, for a named run of the version already installed (nothing to download); $null when the
# record, the binary or the tag do not agree.
function Get-InstalledPack([string]$Prefix, [string]$Tag) {
    $ij = Join-Path $Prefix 'install.json'
    $ver = Get-JsonString $ij 'version'; $commit = Get-JsonString $ij 'commit'
    if (-not $ver -or "v$ver" -ne $Tag -or $commit -notmatch '^[0-9a-f]{40}$') { return $null }
    if ((Get-BinaryVersion (Join-Path $Prefix 'bin\sotd.exe')).Version -ne $ver) { return $null }
    return [pscustomobject]@{ Dir = ''; Version = $ver; Tag = $Tag; Commit = $commit; Bin = (Join-Path $Prefix 'bin') }
}

function Invoke-EntryBody([string[]]$Argv, [string]$Scratch) {
    $script:Step = 'options'
    $o = Read-Options $Argv
    $prefix = $(if ($o.Prefix) { Resolve-PrefixPath $o.Prefix } else { Join-Path $env:LOCALAPPDATA 'sot' })
    $ij = Join-Path $prefix 'install.json'
    Test-Preflight

    $tag = $env:SOT_INSTALL_TAG
    $implicit = ($env:SOT_INSTALL_IMPLICIT -eq '1')
    $named = [bool]$o.Version -or [bool]$o.Archive
    if (-not $tag -and -not $o.Archive) {
        $script:Step = 'target'
        $tag = $o.Version
        if (-not $tag) {
            $implicit = $true
            $tag = Resolve-ChannelTag (Get-JsonString $ij 'version')
        }
        if ($tag -notmatch '^v[0-9]') { Stop-Entry 'target' "could not resolve a release tag (got '$tag')" }
        return (Invoke-Prelude $tag $implicit $Argv $Scratch)
    }
    # A body run on a tag by hand was told what to install, as -Version tells it.
    if ($env:SOT_INSTALL_TAG -and -not $implicit) { $named = $true }
    $script:Pinned = $named
    $script:PendingFile = Join-Path $prefix 'updates\pending-windows-x86_64.json'

    $pack = $null
    $sotd = Join-Path $prefix 'bin\sotd.exe'
    # A named run of the version already installed, with no daemon on another version, needs no download: it only pins.
    if ($named -and -not $o.Archive) {
        $pack = Get-InstalledPack $prefix $tag
        if ($pack -and (Get-DaemonState $sotd (Get-DaemonPipe $sotd) $sotd) -notin @('none', 'current')) { $pack = $null }
    }
    if (-not $pack) { $pack = Get-Pack $o $tag $Scratch }
    $script:PinSuffix = $(if ($script:Pinned) { "; pinned at $($pack.Version): automatic updates stay off until a run that names no version" } else { '' })
    Say "installing Ship of Tools $($pack.Tag) into $prefix"
    $newSotd = Join-Path $pack.Bin 'sotd.exe'
    $nv = Get-BinaryVersion $newSotd
    if ($nv.Version -ne $pack.Version) { Stop-Entry 'binaries' "the packed sotd.exe reports '$($nv.Line)', not $($pack.Version)" }

    $installed = Get-JsonString $ij 'version'
    $fresh = (-not $installed) -or (-not (Test-Path -LiteralPath (Join-Path $prefix 'bin\sotd.exe')))
    $pipe = Get-DaemonPipe $newSotd
    $logDir = Join-Path $env:LOCALAPPDATA 'sot\logs'
    $versionDir = Join-Path $prefix "repo\versions\$($pack.Tag)"
    $base = Join-Path $prefix 'repo\base'
    $origin = $(if ($env:SOT_INSTALL_ORIGIN) { $env:SOT_INSTALL_ORIGIN } else { "https://github.com/$($script:Repo)" })

    if ($fresh) {
        Install-Fresh $prefix $pack $base $versionDir $origin
        # A pack of the line is recorded here, with its commit; a '+' build handed none (a source checkout) gets no record.
        if ($pack.Version.Contains('+')) { Invoke-Script 'record' (Join-Path $versionDir 'scripts\install-manifest.ps1') @('-Prefix', $prefix, '-Repo', $base, '-Commit', $pack.Commit) $o.Hub }
        $installed = $pack.Version
    } else {
        $dir = Invoke-Native $newSotd @('update', 'direction', $pack.Version, '--prefix', $prefix)
        $direction = $(if ($dir.Code -eq 0 -and $dir.Out.Count -gt 0) { $dir.Out[0] } else { 'unknown' })
        $script:Step = 'direction'
        if ($implicit -and $direction -eq 'older') {
            Stop-Entry 'direction' "this computer runs $installed, which is newer than $($pack.Version), the release this run found; it never moves back unless told: name the version you want with -Version $($pack.Tag)" 2
        }
        if ($implicit -and $direction -eq 'unknown') {
            Stop-Entry 'direction' "could not tell which way $($pack.Tag) moves the install at $prefix (it records $installed); a run that names no target never moves it unasked: name the version you want with -Version $($pack.Tag)" 2
        }
        $sameCommit = ((Get-JsonString $ij 'commit') -eq $pack.Commit)
        # The same build with no daemon on another version is a repair run, named or not: an explicit run of the version a
        # box already runs writes the pin and stages, applies and restarts nothing (no sentinel, no window touched).
        $dstate = Get-DaemonState $newSotd $pipe $sotd
        $repair = ($direction -eq 'same' -and $sameCommit -and ($dstate -eq 'none' -or $dstate -eq 'current'))
        if ($repair) {
            Say "this computer already runs $installed at $($pack.Commit.Substring(0, 9)): a repair run"
            # A run that arms no pointer itself drops the one armed over this install, with the pin in one step: the
            # pointer would otherwise move the box the pin was written to hold. The pin goes first: no arm lands between.
            Set-InstallPin $ij $script:Pinned
            Remove-Item -LiteralPath (Join-Path $prefix 'updates\pending-windows-x86_64.json') -Force -ErrorAction SilentlyContinue
        }
        elseif ($direction -eq 'same') { Say "this run moves the install from $installed to $($pack.Version): same version, another build" }
        else { Say "this run moves the install from $installed to $($pack.Version) ($direction)" }
        if (-not $repair) {
            Update-Installed $o $prefix $pack $base $origin $logDir $newSotd $sotd $pipe
        }
    }
    # The layout, comm and the shortcut: the launcher's own code, run from the target checkout. The pin was written before
    # the stage; the last write follows install-manifest.ps1's rewrite of the record.
    if ($script:Pending) { return 0 }
    Invoke-LayoutAndComm $prefix $versionDir $base
    Invoke-Shortcut $prefix $o.Hub
    Set-InstallPin $ij $script:Pinned
    Test-EndState $prefix $pack $sotd $pipe $logDir
    if ($script:EnrollRefusal) {
        $hubName = $(if ($o.Hub) { $o.Hub } else { Get-JsonString $ij 'hub' })
        $me = $(if ($script:EnrollRefusal -match '\[host\.([^\]]+)\]') { $Matches[1] } else { $env:COMPUTERNAME.ToLower() })
        Stop-Entry 'enroll' "the hosts.toml on '$hubName' does not list this computer ('$me'), so the window would open with no hosts: on '$hubName' run  sotd topology set add $me --frontend  (the sotd of its install), then run this installer again"
    }
    $script:PassText = "Ship of Tools $($pack.Version) at $($pack.Commit.Substring(0, 9)) on this computer"
    return 0
}

function Install-Fresh([string]$Prefix, $Pack, [string]$Base, [string]$VersionDir, [string]$Origin) {
    $script:Step = 'binaries'
    $bin = Join-Path $Prefix 'bin'
    New-Item -ItemType Directory -Force -Path $bin | Out-Null
    foreach ($e in @('sot.exe', 'sotd.exe', 'sot-capsule.exe')) {
        try { Copy-Item -LiteralPath (Join-Path $Pack.Bin $e) -Destination (Join-Path $bin $e) -Force }
        catch { Stop-Entry 'binaries' "could not write $bin\${e}: $($_.Exception.Message)" }
    }
    Initialize-Base $Base $Origin $Pack
    New-VersionFolder $Base $VersionDir $Pack
}

# repo\base: a blobless clone that holds the commit (a pre-0.6.0 box has none).
function Initialize-Base([string]$Base, [string]$Origin, $Pack) {
    $script:Step = 'checkout'
    if (-not (Test-Path -LiteralPath (Join-Path $Base '.git'))) {
        New-Item -ItemType Directory -Force -Path (Split-Path $Base) | Out-Null
        $r = Invoke-Native 'git.exe' @('-c', 'http.lowSpeedLimit=1000', '-c', 'http.lowSpeedTime=60', 'clone', '--quiet', '--filter=blob:none', '--no-checkout', $Origin, $Base)
        if ($r.Code -ne 0) { Stop-Entry 'checkout' "could not clone $Origin into ${Base}: $((@($r.Out + $r.Err) | Select-Object -First 3) -join ' / ')" }
    }
    $has = Invoke-Native 'git.exe' @('-C', $Base, 'cat-file', '-e', "$($Pack.Commit)^{commit}")
    if ($has.Code -ne 0) {
        $f = Invoke-Native 'git.exe' @('-c', 'http.lowSpeedLimit=1000', '-c', 'http.lowSpeedTime=60', '-C', $Base, 'fetch', '--quiet', 'origin', $Pack.Commit)
        if ($f.Code -ne 0) { Stop-Entry 'checkout' "$($Pack.Commit) is not in $Base and could not be fetched from $Origin" }
    }
    # install-manifest.ps1 reads the release commit from the tag; a pack of the line has no tag.
    if (-not $Pack.Version.Contains('+') -and -not ((Invoke-Native 'git.exe' @('-C', $Base, 'tag', '-l', $Pack.Tag)).Out -join '')) {
        Invoke-Native 'git.exe' @('-c', 'http.lowSpeedLimit=1000', '-c', 'http.lowSpeedTime=60', '-C', $Base, 'fetch', '--quiet', 'origin', "refs/tags/$($Pack.Tag):refs/tags/$($Pack.Tag)") | Out-Null
    }
}
function New-VersionFolder([string]$Base, [string]$VersionDir, $Pack) {
    if (Test-Path -LiteralPath $VersionDir) {
        $h = Invoke-Native 'git.exe' @('-C', $VersionDir, 'rev-parse', 'HEAD')
        if ($h.Code -eq 0 -and $h.Out[0] -eq $Pack.Commit) { return }
        Stop-Entry 'checkout' "$VersionDir exists but is not at $($Pack.Commit.Substring(0, 9)); remove that folder and run this again"
    }
    Invoke-Native 'git.exe' @('-C', $Base, 'worktree', 'prune') | Out-Null
    $r = Invoke-Native 'git.exe' @('-C', $Base, 'worktree', 'add', '--detach', $VersionDir, $Pack.Commit)
    if ($r.Code -ne 0) { Stop-Entry 'checkout' "could not add the worktree $VersionDir at $($Pack.Commit.Substring(0, 9)): $((@($r.Out + $r.Err) | Select-Object -First 3) -join ' / ')" }
    $h = Invoke-Native 'git.exe' @('-C', $VersionDir, 'rev-parse', 'HEAD')
    if ($h.Out[0] -ne $Pack.Commit) { Stop-Entry 'checkout' "$VersionDir is not at $($Pack.Commit)" }
}

# Stage with the target's own sotd, then apply one way by what runs here. Sets $script:Pending when the update stays armed.
function Update-Installed([hashtable]$O, [string]$Prefix, $Pack, [string]$Base, [string]$Origin, [string]$LogDir, [string]$NewSotd, [string]$Sotd, [string]$Pipe) {
    $script:Step = 'daemon'
    $installedVer = Get-JsonString (Join-Path $Prefix 'install.json') 'version'
    # Every path below that stops or restarts a daemon asks first, whatever its version.
    $state = Get-DaemonState $NewSotd $Pipe $Sotd
    $liveText = ''
    if ($state -ne 'none') {
        $liveText = Get-LiveText $state $Pipe (Get-LiveWork $NewSotd $Pipe)
        if ($liveText -and -not $O.RestartDaemon) {
            Say '       Restarting the daemon ends every live REPL and Pluto notebook (their variables are lost); the rows and their agents keep running. Nothing under the install was changed. Ask whoever uses this computer, then run this again with -RestartDaemon.'
            Stop-Entry 'daemon' $liveText 2
        }
    }
    $script:Step = 'stage'
    if (-not (Test-Path -LiteralPath (Join-Path $Base '.git'))) { Initialize-Base $Base $Origin $Pack }
    # The pin goes in before the stage, so no automatic arm lands between the stage and the pin.
    $ijPath = Join-Path $Prefix 'install.json'
    $wasPinned = ((Get-Content -LiteralPath $ijPath -Raw) -match '"pinned"\s*:\s*true')
    Set-InstallPin $ijPath $script:Pinned
    Say "staging and preparing $($Pack.Tag) (this can take many minutes)"
    $r = Invoke-Native $NewSotd @('update', 'stage', $Pack.Dir, '--prefix', $Prefix)
    foreach ($l in $r.Out) { Say "  $l" }
    if ($r.Code -ne 0) {
        # sotd's last line says why; the lines before it are its progress.
        $said = @($r.Err | Where-Object { "$_".Trim() })
        if (-not $said) { $said = @($r.Out | Where-Object { "$_".Trim() }) }
        $detail = $(if ($said) { "$($said[-1])".Trim() } else { "sotd update stage ended with exit $($r.Code)" })
        if (@($r.Err + $r.Out) -join ' ' -match 'not armed') { $detail = "$detail (remove $Prefix\updates\bad-$($Pack.Tag)-windows-x86_64 if the version was marked bad, then run this again)" }
        Set-InstallPin $ijPath $wasPinned
        $script:PinWritten = $false
        Stop-Entry 'stage' $detail
    }
    $script:ArmedTag = $Pack.Tag
    $sup = Get-Supervisor $LogDir
    $windowOnly = (-not $sup) -and (Test-WindowRuns (Join-Path $Prefix 'bin'))
    $work = $(if ($liveText) { "; $liveText" } else { '' })
    if ($sup -and $sup.Code) {
        $wait = $(if ($env:SOT_INSTALL_CONVERGE_WAIT_S) { [int]$env:SOT_INSTALL_CONVERGE_WAIT_S } else { 1800 })
        Wait-Converge $LogDir $Pack.Tag $wait (Get-RelaunchScript $Prefix $installedVer $Pack.Tag) | Out-Null
        $script:ExpectDaemon = ($state -ne 'none')
        return
    }
    $script:Pending = $true
    if ($sup -or $windowOnly) {
        Add-Step "quit Ship of Tools, choose Keep, and start it from the shortcut; the start applies $($Pack.Tag), then run this again to confirm (a converge here could end the box's sessions)$work"
        return
    }
    if ((Get-EntrySessionId) -eq 0) {
        Add-Step "start Ship of Tools at the desk; the start applies $($Pack.Tag) (nothing was stopped from this session, which is not at the desk)$work"
        return
    }
    # Nothing runs and this is a desktop session: apply here. The daemon, if one runs, is stopped first so it pins no stale image.
    $script:Step = 'apply'
    $applyScript = Join-Path (Join-Path $Prefix "repo\versions\$($Pack.Tag)") 'scripts\sot-apply.ps1'
    if ((Get-DaemonState $NewSotd $Pipe $Sotd) -ne 'none') {
        $oldLd = Join-Path (Join-Path $Prefix "repo\versions\v$installedVer") 'scripts\sot-local-daemon.ps1'
        if (Test-Path -LiteralPath $oldLd) {
            $s = Invoke-Native 'powershell.exe' @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $oldLd, '-Stop')
            foreach ($l in $s.Out) { if ("$l".Trim()) { Say "  $l" } }
        }
    }
    $a = Invoke-Native 'powershell.exe' @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $applyScript, '-Prefix', $Prefix)
    foreach ($l in $a.Out) { if ("$l".Trim()) { Say "  $l" } }
    if (-not ($a.Out -match 'sot-apply: APPLIED')) { Stop-Entry 'apply' "sot-apply.ps1 did not apply $($Pack.Tag): $((@($a.Out + $a.Err) | Select-Object -Last 3) -join ' / ')" }
    $script:Pending = $false
}

function Invoke-InstallEntry([string[]]$Argv) {
    $script:Done = $false; $script:Rc = 1; $script:Step = 'options'; $script:Steps = @(); $script:Pending = $false; $script:Pinned = $false; $script:PinSuffix = ''; $script:PinWritten = $false; $script:ArmedTag = ''; $script:PendingFile = ''; $script:EnrollRefusal = ''; $script:DaemonWhy = ''; $script:ExpectDaemon = $false; $script:PassText = ''
    $scratch = Join-Path $env:TEMP ('sot-install-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
    $rc = 1
    try {
        New-Item -ItemType Directory -Force -Path $scratch | Out-Null
        $rc = [int](@(Invoke-EntryBody $Argv $scratch)[-1])
        if (-not $script:Done) {
            if ($script:Steps.Count -gt 0) { Write-Verdict "ONE STEP LEFT: $($script:Steps -join '; then ')$($script:PinSuffix)" }
            else { Write-Verdict "PASS: $($script:PassText)$($script:PinSuffix)" }
        }
    } catch {
        if ($_.Exception.Message -ne $script:StopMarker) { $script:Rc = 1; Write-Verdict "FAIL: $($script:Step): $($_.Exception.Message)" }
        $rc = $script:Rc
    } finally {
        if (-not $script:Done) { Write-Verdict "FAIL: $($script:Step): stopped" }
        Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
    }
    return $rc
}

if ($MyInvocation.InvocationName -ne '.') {
    $entryCode = Invoke-InstallEntry $args
    # Explicit, never whatever the last native command left in $LASTEXITCODE.
    exit $entryCode
}

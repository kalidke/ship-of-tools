# step3-probe.ps1 -Start <tag|source> -Shell powershell|gitbash [-Offline] -- the runbook's step 3 for Windows,
# verbatim from docs/UPDATE-AGENT.md (probe\sot-update.ps1, probe\run-ps.txt, probe\run-gitbash.txt, extracted by
# extract.py), run twice. -Start <tag>: that tag installed by its own install.ps1, unpinned, from a folder outside any
# checkout. -Start source: a source checkout at v0.6.6 with no Rust toolchain on PATH, no install.json, only v0.5.5's
# sot.exe in %LOCALAPPDATA%\sot\bin and a Desktop shortcut to the checkout's launcher. The script file is copied to
# $HOME\sot-update.ps1 as an agent's file tool writes it; its run line is run from PowerShell (Invoke-Expression) or
# from Git Bash (bash.exe -c), from inside a clone of the repository (the source checkout for -Start source), with a
# dummy GITHUB_TOKEN set. -Offline first runs it with GitHub's names resolving to 127.0.0.1: it must print
# STOP: cannot reach GitHub and change nothing. Exit 0 only when both runs end PASS at the newest release, the second
# changes nothing, neither names a caller clone, sot-update.log ends with the same line, the clone it ran from is as it
# was, and the calling session keeps its folder and environment. Windows PowerShell 5.1, ASCII only.
param(
    [Parameter(Mandatory = $true)][string]$Start,
    [Parameter(Mandatory = $true)][ValidateSet('powershell', 'gitbash')][string]$Shell,
    [switch]$Offline
)
$ErrorActionPreference = 'Continue'
$ProgressPreference = 'SilentlyContinue'
$Repo = 'kalidke/ship-of-tools'
$prefix = Join-Path $env:LOCALAPPDATA 'sot'
$out = Join-Path $env:RUNNER_TEMP 'probe'
$neutral = Join-Path $env:RUNNER_TEMP 'neutral'
New-Item -ItemType Directory -Force -Path $out, $neutral, "$env:USERPROFILE\Desktop", (Join-Path $prefix 'config'), (Join-Path $prefix 'bin') | Out-Null
$summary = Join-Path $out 'summary.txt'
$script:ok = $true
function Say([string]$t) { Write-Host "probe: $t"; Add-Content -LiteralPath $summary -Value $t }
function Bad([string]$t) { Say $t; $script:ok = $false }
foreach ($n in @('GITHUB_TOKEN', 'GH_TOKEN', 'SOT_INSTALL_TAG', 'SOT_INSTALL_IMPLICIT')) { Remove-Item "Env:$n" -ErrorAction SilentlyContinue }
[IO.File]::WriteAllText((Join-Path $prefix 'config\publish-list.txt'), "# synthetic probe policy`nprobe-denied-publication`n")
$probeDir = Join-Path $env:GITHUB_WORKSPACE 'probe'
foreach ($f in @('sot-update.ps1', 'run-ps.txt', 'run-gitbash.txt')) { Say "$f sha256: $((Get-FileHash -LiteralPath (Join-Path $probeDir $f) -Algorithm SHA256).Hash.Substring(0, 16).ToLower())" }
$runLine = [IO.File]::ReadAllText((Join-Path $probeDir $(if ($Shell -eq 'powershell') { 'run-ps.txt' } else { 'run-gitbash.txt' }))).Trim()
$bash = 'C:\Program Files\Git\bin\bash.exe'
$log = Join-Path $HOME 'sot-update.log'

function Get-Field([string]$k) {
    $ij = Join-Path $prefix 'install.json'
    if (-not (Test-Path -LiteralPath $ij)) { return '' }
    if ([IO.File]::ReadAllText($ij) -match ('"' + $k + '"\s*:\s*"?([^",\r\n}]*)"?')) { return $Matches[1] }
    return ''
}
function Show-State([string]$label) {
    $cur = ''
    try { $cur = [string]@((Get-Item -LiteralPath (Join-Path $prefix 'repo\current') -Force -ErrorAction Stop).Target)[0] } catch { }
    $v = ''
    if (Test-Path -LiteralPath (Join-Path $prefix 'bin\sotd.exe')) { $v = (& (Join-Path $prefix 'bin\sotd.exe') --version 2>&1 | Select-Object -First 1) }
    Say "$label install.json: version=$(Get-Field 'version') tag=$(Get-Field 'tag') pinned=$(Get-Field 'pinned') commit=$(Get-Field 'commit')"
    Say "$label sotd.exe --version: $v; repo\current: $cur"
}
function Get-Fingerprint {
    $parts = @()
    $ij = Join-Path $prefix 'install.json'
    if (Test-Path -LiteralPath $ij) { $parts += [IO.File]::ReadAllText($ij) }
    foreach ($f in @(Get-ChildItem -LiteralPath (Join-Path $prefix 'bin') -File -ErrorAction SilentlyContinue)) { $parts += "$($f.Name) $((Get-FileHash -LiteralPath $f.FullName).Hash)" }
    try { $parts += [string]@((Get-Item -LiteralPath (Join-Path $prefix 'repo\current') -Force -ErrorAction Stop).Target)[0] } catch { }
    $parts += @(Get-ChildItem -LiteralPath (Join-Path $prefix 'repo\versions') -Name -ErrorAction SilentlyContinue)
    return ($parts -join "`n")
}
function Get-CloneState([string]$dir) {
    $common = (git -C $dir rev-parse --git-common-dir 2>$null)
    if (-not [IO.Path]::IsPathRooted($common)) { $common = Join-Path $dir $common }
    return ((@(git -C $dir status --porcelain 2>$null) + @(git -C $dir rev-parse HEAD 2>$null) + @(git -C $dir config --get core.hooksPath 2>$null) + @(Get-ChildItem -LiteralPath (Join-Path $common 'hooks') -Force -Name -ErrorAction SilentlyContinue)) -join "`n")
}
# The run line, as an agent types it, from $cwd; sets $script:Last.
function Invoke-RunLine([string]$label, [string]$cwd) {
    $file = Join-Path $out "$label.out"
    $t0 = Get-Date
    $envBefore = "$env:SOT_INSTALL_TAG|$env:GITHUB_TOKEN"
    Push-Location -LiteralPath $cwd
    try {
        if ($Shell -eq 'powershell') { $lines = @(Invoke-Expression $runLine 2>&1 | ForEach-Object { "$_" }) }
        else { $lines = @(& $bash -c $runLine 2>&1 | ForEach-Object { "$_" }) }
        $where = (Get-Location).Path
    } finally { Pop-Location }
    [IO.File]::WriteAllLines($file, [string[]]$lines)
    $lines | ForEach-Object { Write-Host "  $_" }
    $nonEmpty = @($lines | Where-Object { $_.Trim() -ne '' })
    $script:Last = $(if ($nonEmpty.Count) { $nonEmpty[-1] } else { '' })
    Say "${label}: after $([int]((Get-Date) - $t0).TotalSeconds) s, from $cwd by $Shell; last line: $($script:Last)"
    if ($where -ne $cwd) { Bad "${label}: the calling session moved to $where" }
    if ("$env:SOT_INSTALL_TAG|$env:GITHUB_TOKEN" -ne $envBefore) { Bad "${label}: the calling session's environment changed" }
}

# ---- the start ------------------------------------------------------------------------------------------------------
if ($Start -eq 'source') {
    $env:PATH = (($env:PATH -split ';') | Where-Object { $_ -and ($_ -notmatch 'cargo|rustup') }) -join ';'
    Remove-Item Env:CARGO_HOME, Env:RUSTUP_HOME -ErrorAction SilentlyContinue
    $from = Join-Path $env:USERPROFILE 'src\ship-of-tools'
    git clone --quiet "https://github.com/$Repo" $from 2>&1 | Out-Null
    git -C $from checkout --quiet v0.6.6 2>&1 | Out-Null
    $zip = Join-Path $out 'old.zip'
    curl.exe -fsSL -o $zip "https://github.com/$Repo/releases/download/v0.5.5/sot-0.5.5-windows-x86_64.zip"
    Expand-Archive -LiteralPath $zip -DestinationPath (Join-Path $out 'old') -Force
    Copy-Item -LiteralPath @(Get-ChildItem -LiteralPath (Join-Path $out 'old') -Recurse -Filter 'sot.exe')[0].FullName -Destination (Join-Path $prefix 'bin\sot.exe')
    $ws = New-Object -ComObject WScript.Shell
    $lnk = $ws.CreateShortcut("$env:USERPROFILE\Desktop\Ship of Tools.lnk")
    $lnk.TargetPath = "$env:WINDIR\System32\WindowsPowerShell\v1.0\powershell.exe"
    $lnk.Arguments = "-NoProfile -ExecutionPolicy Bypass -File `"$from\scripts\launch-sot.ps1`""
    $lnk.WorkingDirectory = $from
    $lnk.Save()
    Say "start: source checkout $from at $(git -C $from describe --tags --always); cargo on PATH: $([bool](Get-Command cargo -ErrorAction SilentlyContinue)); bin: $((Get-ChildItem -LiteralPath (Join-Path $prefix 'bin') -Name) -join ' '); install.json: $(Test-Path -LiteralPath (Join-Path $prefix 'install.json'))"
} else {
    $from = $env:GITHUB_WORKSPACE
    $startPs = Join-Path $out 'start-install.ps1'
    curl.exe -fsSL -o $startPs "https://raw.githubusercontent.com/$Repo/$Start/scripts/install.ps1"
    $env:SOT_INSTALL_TAG = $Start; $env:SOT_INSTALL_IMPLICIT = '1'
    Push-Location -LiteralPath $neutral
    try { cmd /c "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$startPs`" > `"$out\start.out`" 2>&1 < NUL" } finally { Pop-Location }
    Remove-Item Env:SOT_INSTALL_TAG, Env:SOT_INSTALL_IMPLICIT -ErrorAction SilentlyContinue
    Say "start: $Start's install.ps1: $(@(Get-Content -LiteralPath "$out\start.out" | Where-Object { $_.Trim() -ne '' })[-1])"
}
Show-State 'start'
$cloneBefore = Get-CloneState $from
# The agent's file tool writes the script into the home folder.
Copy-Item -LiteralPath (Join-Path $probeDir 'sot-update.ps1') -Destination (Join-Path $HOME 'sot-update.ps1') -Force

# ---- no network -----------------------------------------------------------------------------------------------------
if ($Offline) {
    $hosts = "$env:WINDIR\System32\drivers\etc\hosts"
    $saved = [IO.File]::ReadAllText($hosts)
    [IO.File]::AppendAllText($hosts, "`r`n127.0.0.1 github.com`r`n127.0.0.1 raw.githubusercontent.com`r`n127.0.0.1 api.github.com`r`n")
    ipconfig /flushdns | Out-Null
    $before = Get-Fingerprint
    Invoke-RunLine 'offline' $from
    if ($script:Last -ne 'STOP: cannot reach GitHub') { Bad 'offline: NOT the STOP line' } else { Say 'offline: STOP as written' }
    if ($before -cne (Get-Fingerprint)) { Bad 'offline: CHANGED' } else { Say 'offline: nothing changed' }
    [IO.File]::WriteAllText($hosts, $saved)
    ipconfig /flushdns | Out-Null
}

# ---- step 3, twice --------------------------------------------------------------------------------------------------
$env:GITHUB_TOKEN = 'probe-dummy-not-a-token'
$results = @()
foreach ($label in @('update', 'again')) {
    if ($label -eq 'again') { $before = Get-Fingerprint }
    Invoke-RunLine $label $from
    $logLast = @([IO.File]::ReadAllLines($log) | Where-Object { $_.Trim() -ne '' })[-1]
    if ($logLast -ne $script:Last) { Bad "${label}: sot-update.log ends '$logLast'" } else { Say "${label}: sot-update.log ends with the same line" }
    if (-not $script:Last.StartsWith('PASS: Ship of Tools 0.7.0 at ')) { Bad "${label}: NOT a PASS at 0.7.0" }
    if ($script:Last -match 'checked caller') { Bad "${label}: the PASS line names a caller clone" } else { Say "${label}: no caller clone named" }
    Show-State $label
    if ($label -eq 'again') { if ($before -cne (Get-Fingerprint)) { Bad 'again: CHANGED' } else { Say 'again: install.json, bin\*, repo\current and repo\versions unchanged' } }
}
if ($Shell -eq 'gitbash') {
    # Steps 1, 2 and 5 from Git Bash, as the runbook writes them for that shell.
    $rec = (& $bash -c 'cat "$LOCALAPPDATA/sot/install.json"' 2>&1 | ForEach-Object { "$_" }) -join ' '
    if ($rec -notmatch '"version":\s*"0\.7\.0"') { Bad "Git Bash: cat `"`$LOCALAPPDATA/sot/install.json`" did not show 0.7.0: $rec" } else { Say 'Git Bash: cat "$LOCALAPPDATA/sot/install.json" shows version 0.7.0' }
    $ver = (& $bash -c '"$LOCALAPPDATA/sot/bin/sotd.exe" --version' 2>&1 | Select-Object -First 1)
    if ("$ver" -notlike 'sotd 0.7.0 *') { Bad "Git Bash: sotd.exe --version said '$ver'" } else { Say "Git Bash: `"`$LOCALAPPDATA/sot/bin/sotd.exe`" --version: $ver" }
    $plan = (& $bash -c '"$LOCALAPPDATA/sot/bin/sotd.exe" topology plan' 2>&1 | ForEach-Object { "$_" }) -join ' | '
    Say "Git Bash: sotd.exe topology plan (no hosts.toml here): $plan"
}
if ((Get-CloneState $from) -cne $cloneBefore) { Bad "the clone it ran from ($from): CHANGED" } else { Say "the clone it ran from ($from): status, HEAD, hooksPath and hooks unchanged" }
if ($Start -eq 'source') {
    $s = (New-Object -ComObject WScript.Shell).CreateShortcut("$env:USERPROFILE\Desktop\Ship of Tools.lnk")
    Say "shortcut: $($s.Arguments)"
    if ($s.Arguments -notmatch [regex]::Escape("$prefix\repo\current\scripts\launch-sot.ps1")) { Bad 'the shortcut does not run the release launcher' }
}
Say "RESULT $(if ($script:ok) { 'PASS' } else { 'FAIL' })"
if ($script:ok) { exit 0 } else { exit 1 }

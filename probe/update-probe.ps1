# update-probe.ps1 -Start <tag> [-Mode plain|pinned|token] -- one case of the update runbook on a Windows runner.
# The start is installed by that tag's own install.ps1; then the runbook's command: main's install.ps1 fetched from
# GitHub and run with GITHUB_TOKEN and GH_TOKEN cleared. Windows PowerShell 5.1, ASCII only. The runner's own profile is
# used (a hosted runner is thrown away after the job).
param(
    [Parameter(Mandatory = $true)][string]$Start,
    [ValidateSet('plain', 'pinned', 'token')][string]$Mode = 'plain'
)
$ErrorActionPreference = 'Continue'
$ProgressPreference = 'SilentlyContinue'
$Repo = 'kalidke/ship-of-tools'
$prefix = Join-Path $env:LOCALAPPDATA 'sot'
$out = Join-Path $env:RUNNER_TEMP 'probe'
New-Item -ItemType Directory -Force -Path $out | Out-Null
$summary = Join-Path $out 'summary.txt'
function Say([string]$t) { Write-Host "probe: $t"; Add-Content -LiteralPath $summary -Value $t }
foreach ($n in @('GITHUB_TOKEN', 'GH_TOKEN', 'SOT_INSTALL_TAG', 'SOT_INSTALL_IMPLICIT')) { Remove-Item "Env:$n" -ErrorAction SilentlyContinue }
New-Item -ItemType Directory -Force -Path "$env:USERPROFILE\Desktop", (Join-Path $prefix 'config') | Out-Null
[IO.File]::WriteAllText((Join-Path $prefix 'config\publish-list.txt'), "# synthetic probe policy`nprobe-denied-publication`n")

function Get-Field([string]$k) {
    $ij = Join-Path $prefix 'install.json'
    if (-not (Test-Path -LiteralPath $ij)) { return '' }
    $t = [IO.File]::ReadAllText($ij)
    if ($t -match ('"' + $k + '"\s*:\s*"?([^",\r\n}]*)"?')) { return $Matches[1] }
    return ''
}
function Show-State([string]$label) {
    $cur = ''
    try { $cur = [string]@((Get-Item -LiteralPath (Join-Path $prefix 'repo\current') -Force -ErrorAction Stop).Target)[0] } catch { }
    $v = ''
    try { $v = (& (Join-Path $prefix 'bin\sotd.exe') --version 2>&1 | Select-Object -First 1) } catch { }
    Say "$label install.json: version=$(Get-Field 'version') tag=$(Get-Field 'tag') pinned=$(Get-Field 'pinned') commit=$(Get-Field 'commit')"
    Say "$label sotd.exe --version: $v"
    Say "$label repo\current: $cur"
}
function Get-Fingerprint {
    $parts = @()
    $ij = Join-Path $prefix 'install.json'
    if (Test-Path -LiteralPath $ij) { $parts += [IO.File]::ReadAllText($ij) }
    foreach ($f in @(Get-ChildItem -LiteralPath (Join-Path $prefix 'bin') -File -ErrorAction SilentlyContinue)) { $parts += "$($f.Name) $((Get-FileHash -LiteralPath $f.FullName).Hash)" }
    try { $parts += [string]@((Get-Item -LiteralPath (Join-Path $prefix 'repo\current') -Force).Target)[0] } catch { }
    $parts += @(Get-ChildItem -LiteralPath (Join-Path $prefix 'repo\versions') -Name -ErrorAction SilentlyContinue)
    return ($parts -join "`n")
}
# Run a PowerShell file as a child, its output to <label>.out; sets $script:Rc and $script:Last.
function Invoke-Ps([string]$label, [string]$file, [string[]]$argList) {
    $log = Join-Path $out "$label.out"
    Say "--- ${label}: $file $($argList -join ' ')"
    $t0 = Get-Date
    $quoted = ($argList | ForEach-Object { '"' + $_ + '"' }) -join ' '
    cmd /c "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$file`" $quoted > `"$log`" 2>&1 < NUL"
    $script:Rc = $LASTEXITCODE
    $lines = @(Get-Content -LiteralPath $log | Where-Object { $_.Trim() -ne '' })
    $script:Last = $(if ($lines.Count) { $lines[-1] } else { '' })
    Get-Content -LiteralPath $log | ForEach-Object { Write-Host "  $_" }
    Say "${label}: exit $($script:Rc) after $([int]((Get-Date) - $t0).TotalSeconds) s; last line: $($script:Last)"
}

# ---- the start ----------------------------------------------------------------------------------------------------
$startPs = Join-Path $out 'start-install.ps1'
curl.exe -fsSL -o $startPs "https://raw.githubusercontent.com/$Repo/$Start/scripts/install.ps1"
if ($LASTEXITCODE -ne 0) { Say "RESULT start-failed (no install.ps1 at $Start)"; exit 1 }
if ($Mode -eq 'pinned') {
    Invoke-Ps 'start' $startPs @('-Version', $Start)
} else {
    $env:SOT_INSTALL_TAG = $Start; $env:SOT_INSTALL_IMPLICIT = '1'
    Invoke-Ps 'start' $startPs @()
    Remove-Item Env:SOT_INSTALL_TAG, Env:SOT_INSTALL_IMPLICIT -ErrorAction SilentlyContinue
}
Show-State 'start'
if (-not (Test-Path -LiteralPath (Join-Path $prefix 'install.json'))) { Say 'RESULT start-failed'; exit 1 }

# ---- the runbook's command ------------------------------------------------------------------------------------------
function Invoke-Runbook([string]$label) {
    $entry = Join-Path $env:TEMP 'sot-install.ps1'
    curl.exe -fsSL -o $entry "https://raw.githubusercontent.com/$Repo/main/scripts/install.ps1"
    Remove-Item Env:GITHUB_TOKEN, Env:GH_TOKEN -ErrorAction SilentlyContinue
    Invoke-Ps $label $entry @()
}
if ($Mode -eq 'token') { $env:GITHUB_TOKEN = 'probe-dummy-not-a-token' }
Invoke-Runbook 'update'
$updateLast = $script:Last
Show-State 'update'
# On a runner with no desk (session 0) the update is armed for the next start; apply it as the launcher's start does.
if ($updateLast.StartsWith('ONE STEP LEFT: start Ship of Tools at the desk')) {
    Say "session $([System.Diagnostics.Process]::GetCurrentProcess().SessionId): applying the armed update as the next start does"
    Invoke-Ps 'apply' (Join-Path $prefix 'repo\current\scripts\sot-apply.ps1') @('-Prefix', $prefix)
    Show-State 'apply'
}
$before = Get-Fingerprint
Invoke-Runbook 'again'
$againLast = $script:Last
$after = Get-Fingerprint
Show-State 'again'
if ($before -ceq $after) { Say 'again: install.json, bin\*, repo\current and repo\versions unchanged' } else { Say 'again: CHANGED install.json, bin\*, repo\current or repo\versions' }
$u = $(if ($updateLast.StartsWith('PASS: Ship of Tools 0.7.0 at ')) { 'PASS' } else { 'NOT-PASS' })
$a = $(if ($againLast.StartsWith('PASS: Ship of Tools 0.7.0 at ')) { 'PASS' } else { 'NOT-PASS' })
Say "RESULT update=$u again=$a"
exit 0

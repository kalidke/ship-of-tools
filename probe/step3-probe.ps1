# step3-probe.ps1 -Start <tag> -- the runbook's step 3 command for Windows, verbatim from docs/UPDATE-AGENT.md
# (probe\step3.ps1), run twice on a computer that runs <tag>. The start is installed by that tag's own install.ps1,
# unpinned. Each run starts from inside a clone of the repository (the workflow's checkout) with a dummy GITHUB_TOKEN set,
# so the command's own Set-Location $HOME and token removal are what is proved. Windows PowerShell 5.1, ASCII only.
# Exit 0 only when both runs end PASS at the newest release, the second changes nothing, and neither names a caller.
param([Parameter(Mandatory = $true)][string]$Start)
$ErrorActionPreference = 'Continue'
$ProgressPreference = 'SilentlyContinue'
$Repo = 'kalidke/ship-of-tools'
$prefix = Join-Path $env:LOCALAPPDATA 'sot'
$out = Join-Path $env:RUNNER_TEMP 'probe'
$neutral = Join-Path $env:RUNNER_TEMP 'neutral'
New-Item -ItemType Directory -Force -Path $out, $neutral, "$env:USERPROFILE\Desktop", (Join-Path $prefix 'config') | Out-Null
$summary = Join-Path $out 'summary.txt'
function Say([string]$t) { Write-Host "probe: $t"; Add-Content -LiteralPath $summary -Value $t }
foreach ($n in @('GITHUB_TOKEN', 'GH_TOKEN', 'SOT_INSTALL_TAG', 'SOT_INSTALL_IMPLICIT')) { Remove-Item "Env:$n" -ErrorAction SilentlyContinue }
[IO.File]::WriteAllText((Join-Path $prefix 'config\publish-list.txt'), "# synthetic probe policy`nprobe-denied-publication`n")
$step3 = Join-Path $env:GITHUB_WORKSPACE 'probe\step3.ps1'
Say "step3.ps1 sha256: $((Get-FileHash -LiteralPath $step3 -Algorithm SHA256).Hash.Substring(0, 16).ToLower())"

function Get-Field([string]$k) {
    $ij = Join-Path $prefix 'install.json'
    if (-not (Test-Path -LiteralPath $ij)) { return '' }
    if ([IO.File]::ReadAllText($ij) -match ('"' + $k + '"\s*:\s*"?([^",\r\n}]*)"?')) { return $Matches[1] }
    return ''
}
function Show-State([string]$label) {
    $cur = ''
    try { $cur = [string]@((Get-Item -LiteralPath (Join-Path $prefix 'repo\current') -Force -ErrorAction Stop).Target)[0] } catch { }
    Say "$label install.json: version=$(Get-Field 'version') tag=$(Get-Field 'tag') pinned=$(Get-Field 'pinned') commit=$(Get-Field 'commit')"
    Say "$label sotd.exe --version: $(& (Join-Path $prefix 'bin\sotd.exe') --version 2>&1 | Select-Object -First 1)"
    Say "$label repo\current: $cur"
}
function Get-Fingerprint {
    $parts = @([IO.File]::ReadAllText((Join-Path $prefix 'install.json')))
    foreach ($f in @(Get-ChildItem -LiteralPath (Join-Path $prefix 'bin') -File)) { $parts += "$($f.Name) $((Get-FileHash -LiteralPath $f.FullName).Hash)" }
    try { $parts += [string]@((Get-Item -LiteralPath (Join-Path $prefix 'repo\current') -Force).Target)[0] } catch { }
    $parts += @(Get-ChildItem -LiteralPath (Join-Path $prefix 'repo\versions') -Name)
    return ($parts -join "`n")
}
function Invoke-Logged([string]$label, [string]$cwd, [string]$file) {
    $log = Join-Path $out "$label.out"
    $t0 = Get-Date
    Push-Location -LiteralPath $cwd
    try { cmd /c "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$file`" > `"$log`" 2>&1 < NUL" } finally { Pop-Location }
    $script:Rc = $LASTEXITCODE
    Get-Content -LiteralPath $log | ForEach-Object { Write-Host "  $_" }
    $lines = @(Get-Content -LiteralPath $log | Where-Object { $_.Trim() -ne '' })
    $script:Last = $(if ($lines.Count) { $lines[-1] } else { '' })
    Say "${label}: exit $($script:Rc) after $([int]((Get-Date) - $t0).TotalSeconds) s, run from $cwd; last line: $($script:Last)"
}

# ---- the start: that tag's own installer, unpinned, from a folder outside any checkout -------------------------------
$startPs = Join-Path $out 'start-install.ps1'
curl.exe -fsSL -o $startPs "https://raw.githubusercontent.com/$Repo/$Start/scripts/install.ps1"
$env:SOT_INSTALL_TAG = $Start; $env:SOT_INSTALL_IMPLICIT = '1'
Invoke-Logged 'start' $neutral $startPs
Remove-Item Env:SOT_INSTALL_TAG, Env:SOT_INSTALL_IMPLICIT -ErrorAction SilentlyContinue
Show-State 'start'

# ---- step 3, verbatim, twice, from inside a clone with a dummy token ---------------------------------------------------
$env:GITHUB_TOKEN = 'probe-dummy-not-a-token'
Invoke-Logged 'update' $env:GITHUB_WORKSPACE $step3
$u = $script:Last
Show-State 'update'
$before = Get-Fingerprint
Invoke-Logged 'again' $env:GITHUB_WORKSPACE $step3
$a = $script:Last
$after = Get-Fingerprint
Show-State 'again'
$ok = $true
foreach ($p in @(@('update', $u), @('again', $a))) {
    if (-not $p[1].StartsWith('PASS: Ship of Tools 0.7.0 at ')) { Say "$($p[0]): NOT a PASS at 0.7.0"; $ok = $false }
    if ($p[1] -match 'checked caller') { Say "$($p[0]): the PASS line names a caller clone"; $ok = $false } else { Say "$($p[0]): no caller clone named" }
    Say "$($p[0]): $(@(Select-String -LiteralPath (Join-Path $out "$($p[0]).out") -Pattern 'caller clone enrolled' | ForEach-Object { $_.Line.Trim() })[0])"
}
if ($before -ceq $after) { Say 'again: install.json, bin\*, repo\current and repo\versions unchanged' } else { Say 'again: CHANGED'; $ok = $false }
Say "RESULT $(if ($ok) { 'PASS' } else { 'FAIL' })"
if ($ok) { exit 0 } else { exit 1 }

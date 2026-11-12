# order-probe.ps1 -- the daemons-before-windows commands of docs/UPDATE-AGENT.md step 2 and step 5 on Windows: the blocks
# are probe\daemons-1.ps1, daemons-2.ps1, step5-1.ps1 (PowerShell) and probe\daemons-1.sh (Git Bash), extracted byte for
# byte. A computer that runs 0.6.6 (installed by that tag's own install.ps1) is a hub that declares itself and one far
# computer; its 0.6.6 daemon runs (started the way the launcher starts it). Then: the release read and the plan and
# status as the runbook runs them from PowerShell, the same from Git Bash, and the release's own sotd.exe (0.7.0, unpacked
# from its archive, never installed) reading the 0.6.6 daemon. Windows PowerShell 5.1, ASCII only.
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
function Bad([string]$t) { Say "FAILED: $t"; $script:ok = $false }
foreach ($n in @('GITHUB_TOKEN', 'GH_TOKEN', 'SOT_INSTALL_TAG', 'SOT_INSTALL_IMPLICIT')) { Remove-Item "Env:$n" -ErrorAction SilentlyContinue }
[IO.File]::WriteAllText((Join-Path $prefix 'config\publish-list.txt'), "# synthetic probe policy`nprobe-denied-publication`n")
$probeDir = Join-Path $env:GITHUB_WORKSPACE 'probe'
foreach ($f in @('daemons-1.ps1', 'daemons-2.ps1', 'daemons-3.ps1', 'step5-1.ps1', 'daemons-1.sh')) { Say "$f sha256: $((Get-FileHash -LiteralPath (Join-Path $probeDir $f) -Algorithm SHA256).Hash.Substring(0, 16).ToLower())" }
$bash = 'C:\Program Files\Git\bin\bash.exe'
$sotd = Join-Path $prefix 'bin\sotd.exe'

$startPs = Join-Path $out 'start-install.ps1'
curl.exe -fsSL -o $startPs "https://raw.githubusercontent.com/$Repo/v0.6.6/scripts/install.ps1"
$env:SOT_INSTALL_TAG = 'v0.6.6'; $env:SOT_INSTALL_IMPLICIT = '1'
Push-Location -LiteralPath $neutral
try { cmd /c "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$startPs`" > `"$out\start.out`" 2>&1 < NUL" } finally { Pop-Location }
Remove-Item Env:SOT_INSTALL_TAG, Env:SOT_INSTALL_IMPLICIT -ErrorAction SilentlyContinue
Say "start: $(@(Get-Content -LiteralPath "$out\start.out" | Where-Object { $_.Trim() -ne '' })[-1])"
Say "start sotd.exe --version: $(& $sotd --version 2>&1 | Select-Object -First 1)"

# the group: this computer is its hub and declares itself and one far computer
$env:SOT_SELF_HOST = 'probehub'
$env:SOT_HOSTS = Join-Path $out 'hosts.toml'
[IO.File]::WriteAllText($env:SOT_HOSTS, "hub = `"probehub`"`n[host.probehub]`ndaemon = true`n[host.probefar]`ndaemon = true`n")
$ld = Join-Path $prefix 'repo\current\scripts\sot-local-daemon.ps1'
# Redirected to a file by cmd, never through a pipeline: the daemon it starts inherits its standard handles, and a pipe
# would not end while the daemon runs.
cmd /c "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$ld`" > `"$out\daemon-start.out`" 2>&1 < NUL"
Say "daemon start script exit: $LASTEXITCODE"
Get-Content -LiteralPath "$out\daemon-start.out" | ForEach-Object { Say "  $_" }

# the runbook's blocks, from PowerShell
Push-Location -LiteralPath $neutral
try {
    $tag = (& powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $probeDir 'daemons-1.ps1') 2>&1 | ForEach-Object { "$_" }) -join ''
    Say "release read (PowerShell): $tag"
    if ($tag -notmatch '/releases/tag/v\d') { Bad 'release read (PowerShell)' }
    $step2 = @(& powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $probeDir 'daemons-2.ps1') 2>&1 | ForEach-Object { "$_" })
    [IO.File]::WriteAllLines((Join-Path $out 'step2.out'), [string[]]$step2)
    Say '--- step 2 plan and status (0.6.6 sotd.exe, from PowerShell)'; $step2 | ForEach-Object { Say $_ }
    if (-not ($step2 -contains 'self probehub' -and $step2 -contains 'hub probehub')) { Bad 'plan has no self/hub line' }
    if (-not ($step2 | Where-Object { $_ -match '^probehub hub,daemon 0\.6\.6 up \(host: probehub\)' })) { Bad 'the self row does not read 0.6.6 up' }
    if (-not ($step2 | Where-Object { $_ -match '^probefar daemon unreachable: ' })) { Bad 'the far row does not read unreachable' }
    # Git Bash: the release read as written, and plan and status with the path the runbook gives for Git Bash
    $gtag = (& $bash -c ((Get-Content -Raw -LiteralPath (Join-Path $probeDir 'daemons-1.sh')) -replace "`r", '') 2>&1 | ForEach-Object { "$_" }) -join ''
    Say "release read (Git Bash): $gtag"
    if ($gtag -notmatch '/releases/tag/v\d') { Bad 'release read (Git Bash)' }
    $gstep = @(& $bash -c '"$LOCALAPPDATA/sot/bin/sotd.exe" topology plan; "$LOCALAPPDATA/sot/bin/sotd.exe" status' 2>&1 | ForEach-Object { "$_" })
    Say '--- step 2 plan and status (0.6.6 sotd.exe, from Git Bash)'; $gstep | ForEach-Object { Say $_ }
    if (-not ($gstep | Where-Object { $_ -match '^probehub hub,daemon 0\.6\.6 up \(host: probehub\)' })) { Bad 'Git Bash: the self row does not read 0.6.6 up' }
} finally { Pop-Location }

# the release's own sotd.exe reading the same daemon
$zip = Join-Path $out 'sot-0.7.0-windows-x86_64.zip'
curl.exe -fsSL -o $zip "https://github.com/$Repo/releases/download/v0.7.0/sot-0.7.0-windows-x86_64.zip"
Expand-Archive -LiteralPath $zip -DestinationPath (Join-Path $out 'b070') -Force
$b = @(Get-ChildItem -LiteralPath (Join-Path $out 'b070') -Recurse -Filter 'sotd.exe')[0].FullName
Say "0.7.0's sotd.exe: $(& $b --version 2>&1 | Select-Object -First 1)"
$s070 = @(& $b status 2>&1 | ForEach-Object { "$_" })
Say "--- 0.7.0's sotd.exe status against the 0.6.6 daemon"; $s070 | ForEach-Object { Say $_ }
if (-not ($s070 | Where-Object { $_ -match '^probehub hub,daemon unreachable: .*protocol mismatch: backend 0\.6\.6 \(protocol 3\) vs frontend 0\.7\.0 \(protocol 4\)' })) { Bad '0.7.0 does not read the 0.6.6 daemon as a mismatch naming 0.6.6' }
Say '--- step 5 status (PowerShell block)'
Push-Location -LiteralPath $neutral
try { @(& powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $probeDir 'step5-1.ps1') 2>&1 | ForEach-Object { "$_" }) | ForEach-Object { Say $_ } } finally { Pop-Location }
# no group file: the hub the record names (a -Hub install) is fetched from with sotd topology sync. No ssh answers here, so the
# fetch must fail naming ssh, and the blocks must run as written.
Remove-Item Env:SOT_HOSTS -ErrorAction SilentlyContinue
Push-Location -LiteralPath $neutral
try {
    $nh = @(& powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $probeDir 'daemons-2.ps1') 2>&1 | ForEach-Object { "$_" })
    Say '--- step 2 with no hosts.toml'; $nh | ForEach-Object { Say $_ }
    if (-not ($nh | Where-Object { $_ -match 'no hosts.toml at' })) { Bad 'plan does not say no hosts.toml' }
    $alias = Join-Path $out 'daemons-3-probealias.ps1'
    [IO.File]::WriteAllText($alias, ((Get-Content -Raw -LiteralPath (Join-Path $probeDir 'daemons-3.ps1')) -creplace 'HUB', 'probealias'))
    $as = @(& powershell.exe -NoProfile -ExecutionPolicy Bypass -File $alias 2>&1 | ForEach-Object { "$_" })
    Say '--- the record lookup and sync from an alias no ssh reaches'; $as | ForEach-Object { Say $_ }
    if (-not ($as | Where-Object { $_ -match 'ssh probealias' })) { Bad 'the sync does not fail naming ssh' }
} finally { Pop-Location }
cmd /c "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$ld`" -Stop > `"$out\daemon-stop.out`" 2>&1 < NUL"
Say "RESULT $(if ($script:ok) { 'PASS' } else { 'FAIL' })"
if ($script:ok) { exit 0 } else { exit 1 }

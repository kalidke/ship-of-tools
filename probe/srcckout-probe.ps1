# srcckout-probe.ps1 -Checkout <tag|main> -OldBin <tag> [-NoPolicy] -- the update runbook on a Windows computer that
# ran from a source checkout: no install.json, no Rust toolchain, only an old sot.exe in %LOCALAPPDATA%\sot\bin, and a
# Desktop shortcut to the checkout's own launcher. Then the runbook's command (the newest release from the
# latest-release redirect, its own install.ps1 run with SOT_INSTALL_TAG and the tokens cleared), then the same again.
# Windows PowerShell 5.1, ASCII only. The runner's own profile is used (a hosted runner is thrown away after the job).
param(
    [string]$Checkout = 'v0.6.6',
    [string]$OldBin = 'v0.5.5',
    [switch]$NoPolicy
)
$ErrorActionPreference = 'Continue'
$ProgressPreference = 'SilentlyContinue'
$Repo = 'kalidke/ship-of-tools'
$prefix = Join-Path $env:LOCALAPPDATA 'sot'
$out = Join-Path $env:RUNNER_TEMP 'probe'
New-Item -ItemType Directory -Force -Path $out, (Join-Path $prefix 'bin'), "$env:USERPROFILE\Desktop" | Out-Null
$summary = Join-Path $out 'summary.txt'
function Say([string]$t) { Write-Host "probe: $t"; Add-Content -LiteralPath $summary -Value $t }
foreach ($n in @('GITHUB_TOKEN', 'GH_TOKEN', 'SOT_INSTALL_TAG', 'SOT_INSTALL_IMPLICIT', 'CARGO_HOME', 'RUSTUP_HOME')) { Remove-Item "Env:$n" -ErrorAction SilentlyContinue }

# ---- the start -------------------------------------------------------------------------------------------------------
# No Rust toolchain: the runner's cargo and rustup folders leave PATH.
$env:PATH = (($env:PATH -split ';') | Where-Object { $_ -and ($_ -notmatch 'cargo|rustup') }) -join ';'
Say "cargo on PATH: $([bool](Get-Command cargo -ErrorAction SilentlyContinue)); rustc on PATH: $([bool](Get-Command rustc -ErrorAction SilentlyContinue))"
# The source checkout.
$src = Join-Path $env:USERPROFILE 'src\ship-of-tools'
git clone --quiet "https://github.com/$Repo" $src 2>&1 | Out-Null
if ($Checkout -ne 'main') { git -C $src checkout --quiet $Checkout 2>&1 | Out-Null }
Say "source checkout: $src at $(git -C $src describe --tags --always)"
# An old sot.exe, alone in bin.
$zip = Join-Path $out 'old.zip'
curl.exe -fsSL -o $zip "https://github.com/$Repo/releases/download/$OldBin/sot-$($OldBin.TrimStart('v'))-windows-x86_64.zip"
Expand-Archive -LiteralPath $zip -DestinationPath (Join-Path $out 'old') -Force
$oldSot = @(Get-ChildItem -LiteralPath (Join-Path $out 'old') -Recurse -Filter 'sot.exe')[0].FullName
Copy-Item -LiteralPath $oldSot -Destination (Join-Path $prefix 'bin\sot.exe')
# The shortcut a source checkout's own setup leaves: the checkout's launcher.
$ws = New-Object -ComObject WScript.Shell
$lnkPath = "$env:USERPROFILE\Desktop\Ship of Tools.lnk"
$lnk = $ws.CreateShortcut($lnkPath)
$lnk.TargetPath = "$env:WINDIR\System32\WindowsPowerShell\v1.0\powershell.exe"
$lnk.Arguments = "-NoProfile -ExecutionPolicy Bypass -File `"$src\scripts\launch-sot.ps1`""
$lnk.WorkingDirectory = $src
$lnk.Save()
if (-not $NoPolicy) {
    New-Item -ItemType Directory -Force -Path (Join-Path $prefix 'config') | Out-Null
    [IO.File]::WriteAllText((Join-Path $prefix 'config\publish-list.txt'), "# synthetic probe policy`nprobe-denied-publication`n")
}
function Show-State([string]$label) {
    $ij = Join-Path $prefix 'install.json'
    $rec = $(if (Test-Path -LiteralPath $ij) { ([IO.File]::ReadAllText($ij) -replace '\s+', ' ').Substring(0, [Math]::Min(300, ([IO.File]::ReadAllText($ij) -replace '\s+', ' ').Length)) } else { 'absent' })
    Say "$label install.json: $rec"
    Say "$label bin: $((Get-ChildItem -LiteralPath (Join-Path $prefix 'bin') -Name -ErrorAction SilentlyContinue) -join ' ')"
    Say "$label sot.exe --version: $(& (Join-Path $prefix 'bin\sot.exe') --version 2>&1 | Select-Object -First 1)"
    if (Test-Path -LiteralPath (Join-Path $prefix 'bin\sotd.exe')) { Say "$label sotd.exe --version: $(& (Join-Path $prefix 'bin\sotd.exe') --version 2>&1 | Select-Object -First 1)" }
    $cur = ''
    try { $cur = [string]@((Get-Item -LiteralPath (Join-Path $prefix 'repo\current') -Force -ErrorAction Stop).Target)[0] } catch { }
    Say "$label repo\current: $cur"
    $s = $ws.CreateShortcut($lnkPath)
    Say "$label shortcut: $($s.TargetPath) $($s.Arguments) (in $($s.WorkingDirectory))"
}
Show-State 'start'
# What the checkout's own launcher meets: its plan needs a sotd.exe, and there is none.
$plan = Join-Path $src 'scripts\sot-hosts.ps1'
if (Test-Path -LiteralPath $plan) { Say "the checkout's launcher looks for sotd.exe at: $((Get-ChildItem -LiteralPath $src -Recurse -Filter sotd.exe -ErrorAction SilentlyContinue | Select-Object -First 1).FullName) (none: 'no sotd binary found')" }

# ---- the runbook's command ------------------------------------------------------------------------------------------
function Invoke-Runbook([string]$label) {
    $tag = (curl.exe -fsSLI -o NUL -w '%{url_effective}' "https://github.com/$Repo/releases/latest").Split('/')[-1]
    Say "newest release: $tag"
    $entryFile = Join-Path $env:TEMP 'sot-install.ps1'
    curl.exe -fsSL -o $entryFile "https://raw.githubusercontent.com/$Repo/$tag/scripts/install.ps1"
    Remove-Item Env:GITHUB_TOKEN, Env:GH_TOKEN -ErrorAction SilentlyContinue
    $env:SOT_INSTALL_TAG = $tag
    $log = Join-Path $out "$label.out"
    $t0 = Get-Date
    cmd /c "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$entryFile`" > `"$log`" 2>&1 < NUL"
    $rc = $LASTEXITCODE
    Remove-Item Env:SOT_INSTALL_TAG -ErrorAction SilentlyContinue
    Get-Content -LiteralPath $log | ForEach-Object { Write-Host "  $_" }
    $lines = @(Get-Content -LiteralPath $log | Where-Object { $_.Trim() -ne '' })
    $script:Last = $(if ($lines.Count) { $lines[-1] } else { '' })
    Say "${label}: exit $rc after $([int]((Get-Date) - $t0).TotalSeconds) s; last line: $($script:Last)"
}
Invoke-Runbook 'update'
$u = $script:Last
Show-State 'update'
Invoke-Runbook 'again'
$a = $script:Last
Show-State 'again'
$uk = $(if ($u.StartsWith('PASS: Ship of Tools 0.7.0 at ')) { 'PASS' } elseif ($u.StartsWith('ONE STEP LEFT')) { 'ONE-STEP-LEFT' } else { 'OTHER' })
$ak = $(if ($a.StartsWith('PASS: Ship of Tools 0.7.0 at ')) { 'PASS' } elseif ($a.StartsWith('ONE STEP LEFT')) { 'ONE-STEP-LEFT' } else { 'OTHER' })
Say "RESULT update=$uk again=$ak"
exit 0

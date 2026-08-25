# test-sot-apply.ps1 -- regression harness for scripts/sot-apply.ps1.
#
# Builds a synthetic staged update (pointer + ready dir + digests + a real git
# checkout) in a throwaway prefix and drives the applier through apply,
# damaged-stage, rollback, already-applied, wrong-target, and lock-held.
#
# Run under WINDOWS POWERSHELL 5.1 specifically -- that is the host the .lnk
# launcher uses, and 5.1 decodes BOM-less .ps1 files as cp1252 where pwsh 7
# decodes them as UTF-8, so a file that parses under 7 can still fail under 5.1.
#
# ASCII ONLY (see the same note in sot-apply.ps1).
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts	ests	est-sot-apply.ps1
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$apply = Join-Path $repo 'scripts\sot-apply.ps1'
$root = Join-Path $env:TEMP ("sot-apply-test-" + [guid]::NewGuid().ToString('N').Substring(0,8))
$pass = 0; $fail = 0
function Check([string]$name, [bool]$ok, [string]$detail) {
    if ($ok) { $script:pass++; Write-Host "  PASS  $name" -ForegroundColor Green }
    else { $script:fail++; Write-Host "  FAIL  $name -- $detail" -ForegroundColor Red }
}

function New-Fixture([string]$prefix, [string]$curTag, [string]$newTag, [switch]$CorruptDigest) {
    $TARGET = 'windows-x86_64'
    Remove-Item -LiteralPath $prefix -Recurse -Force -ErrorAction SilentlyContinue
    $bin = Join-Path $prefix 'bin'; New-Item -ItemType Directory -Force -Path $bin | Out-Null
    $updates = Join-Path $prefix 'updates'; New-Item -ItemType Directory -Force -Path $updates | Out-Null
    Set-Content -LiteralPath (Join-Path $bin 'sot.exe') -Value 'OLD-BINARY' -NoNewline
    Set-Content -LiteralPath (Join-Path $bin 'sotd.exe') -Value 'OLD-DAEMON' -NoNewline

    # install.json (schema 1) describing the CURRENT install.
    $ij = @"
{
  "schema": 1,
  "role": "remote",
  "prefix": "$($prefix -replace '\\','\\')",
  "config": "C:\\cfg",
  "service": "none",
  "version": "$($curTag -replace '^v','')",
  "tag": "$curTag",
  "commit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "installed_at": "2026-01-01T00:00:00Z",
  "future_field": "must-survive-the-rewrite"
}
"@
    [System.IO.File]::WriteAllText((Join-Path $prefix 'install.json'), $ij, (New-Object System.Text.UTF8Encoding($false)))

    # A real git worktree for the prepared checkout (the applier verifies
    # HEAD == pinned commit and that it is clean).
    $checkout = Join-Path $prefix "repo\versions\$newTag"
    New-Item -ItemType Directory -Force -Path $checkout | Out-Null
    Push-Location $checkout
    git init -q 2>$null | Out-Null
    git config user.email t@t 2>$null; git config user.name t 2>$null
    Set-Content -LiteralPath (Join-Path $checkout 'f.txt') -Value 'x'
    git add -A 2>$null | Out-Null
    git commit -qm init 2>$null | Out-Null
    $commit = (git rev-parse HEAD).Trim()
    Pop-Location

    # Staged release tree: <tag>-<target>/<top>/sot.exe + manifest + digests.
    $ready = Join-Path $updates "$newTag-$TARGET"
    $top = "sot-$($newTag -replace '^v','')-$TARGET"
    $staged = Join-Path $ready $top
    New-Item -ItemType Directory -Force -Path $staged | Out-Null
    Set-Content -LiteralPath (Join-Path $staged 'sot.exe') -Value 'NEW-BINARY' -NoNewline
    Set-Content -LiteralPath (Join-Path $staged 'sotd.exe') -Value 'NEW-DAEMON' -NoNewline
    Set-Content -LiteralPath (Join-Path $ready 'manifest.json') -Value '{"schema":1}'
    $asset = "$top.zip"
    Set-Content -LiteralPath (Join-Path $ready $asset) -Value 'ARCHIVE-BYTES' -NoNewline
    $assetSha = (Get-FileHash -LiteralPath (Join-Path $ready $asset) -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($CorruptDigest) { $assetSha = ('0' * 64) }
    # files.sha256 over the two staged binaries, relative to $ready.
    $lines = @()
    foreach ($rel in @("$top/sot.exe", "$top/sotd.exe")) {
        $h = (Get-FileHash -LiteralPath (Join-Path $ready ($rel -replace '/','\')) -Algorithm SHA256).Hash.ToLowerInvariant()
        $lines += "$h  $rel"
    }
    Set-Content -LiteralPath (Join-Path $ready 'files.sha256') -Value $lines

    $ptr = @"
{
  "tag": "$newTag",
  "target": "$TARGET",
  "checkout": "$($checkout -replace '\\','\\')",
  "commit": "$commit",
  "asset": "$asset",
  "asset_sha256": "$assetSha"
}
"@
    [System.IO.File]::WriteAllText((Join-Path $updates "pending-$TARGET.json"), $ptr, (New-Object System.Text.UTF8Encoding($false)))
    return @{ prefix = $prefix; checkout = $checkout; commit = $commit; ready = $ready; updates = $updates; bin = $bin }
}

Write-Host "`n=== 1. no pending pointer: clean no-op ===" -ForegroundColor Cyan
$p1 = Join-Path $root 'p1'
$f1 = New-Fixture $p1 'v0.5.0' 'v0.6.0'
Remove-Item -LiteralPath (Join-Path $f1.updates 'pending-windows-x86_64.json') -Force
& $apply -Prefix $p1 6>&1 2>&1 | Out-Null
Check 'exit code 0' ($LASTEXITCODE -eq 0) "got $LASTEXITCODE"
Check 'binary untouched' ((Get-Content (Join-Path $f1.bin 'sot.exe') -Raw) -eq 'OLD-BINARY') 'binary changed'
Check 'lock released' (-not (Test-Path (Join-Path $f1.updates '.lock'))) 'lock dir left behind'

Write-Host "`n=== 2. valid pending: applies ===" -ForegroundColor Cyan
$p2 = Join-Path $root 'p2'
$f2 = New-Fixture $p2 'v0.5.0' 'v0.6.0'
$out2 = & $apply -Prefix $p2 6>&1 2>&1
Write-Host ($out2 | ForEach-Object { "    $_" }) -ForegroundColor DarkGray
Check 'sot.exe swapped'  ((Get-Content (Join-Path $f2.bin 'sot.exe') -Raw) -eq 'NEW-BINARY') 'not swapped'
Check 'sotd.exe swapped' ((Get-Content (Join-Path $f2.bin 'sotd.exe') -Raw) -eq 'NEW-DAEMON') 'not swapped'
Check '.prev kept' ((Test-Path (Join-Path $f2.bin 'sot.exe.prev')) -and (Get-Content (Join-Path $f2.bin 'sot.exe.prev') -Raw) -eq 'OLD-BINARY') 'no .prev'
Check 'pointer cleared' (-not (Test-Path (Join-Path $f2.updates 'pending-windows-x86_64.json'))) 'pointer still there'
Check 'just-applied marker' (Test-Path (Join-Path $f2.updates 'just-applied-windows-x86_64')) 'no marker'
Check 'last-good recorded' (Test-Path (Join-Path $f2.updates 'last-good-windows-x86_64.json')) 'no last-good'
$ij2 = Get-Content (Join-Path $p2 'install.json') -Raw
Check 'install.json tag rewritten' ($ij2 -match '"tag":\s*"v0\.6\.0"') 'tag not updated'
Check 'install.json version rewritten' ($ij2 -match '"version":\s*"0\.6\.0"') 'version not updated'
Check 'unknown field preserved' ($ij2 -match 'must-survive-the-rewrite') 'additive field lost'
$jt = @((Get-Item (Join-Path $p2 'repo\current') -Force).Target)
Check 'repo\current junction' ([bool]($jt.Count -gt 0 -and $jt[0])) 'junction missing'
# install.json must stay BOM-less: serde_json::from_str rejects a leading BOM,
# so a BOM here makes InstallManifest::for_current_exe() return None and the
# frontend silently stops checking for updates after the FIRST successful
# apply. Set-Content -Encoding utf8 on 5.1 emits one; WriteAllText does not.
$b2 = [System.IO.File]::ReadAllBytes((Join-Path $p2 'install.json'))
Check 'install.json has no BOM after apply' `
    (-not ($b2[0] -eq 0xEF -and $b2[1] -eq 0xBB -and $b2[2] -eq 0xBF)) 'BOM written - Rust will refuse to parse it'
$lgb = [System.IO.File]::ReadAllBytes((Join-Path $f2.updates 'last-good-windows-x86_64.json'))
Check 'last-good has no BOM' `
    (-not ($lgb[0] -eq 0xEF -and $lgb[1] -eq 0xBB -and $lgb[2] -eq 0xBF)) 'BOM written'
Check 'lock released' (-not (Test-Path (Join-Path $f2.updates '.lock'))) 'lock dir left behind'

Write-Host "`n=== 3. corrupt archive digest: drops pointer + stage, no mutation ===" -ForegroundColor Cyan
$p3 = Join-Path $root 'p3'
$f3 = New-Fixture $p3 'v0.5.0' 'v0.6.0' -CorruptDigest
$out3 = & $apply -Prefix $p3 6>&1 2>&1
Write-Host ($out3 | ForEach-Object { "    $_" }) -ForegroundColor DarkGray
Check 'binary NOT swapped' ((Get-Content (Join-Path $f3.bin 'sot.exe') -Raw) -eq 'OLD-BINARY') 'mutated on a bad stage'
Check 'pointer dropped' (-not (Test-Path (Join-Path $f3.updates 'pending-windows-x86_64.json'))) 'pointer kept'
Check 'damaged stage removed' (-not (Test-Path $f3.ready)) 'stage kept'
Check 'no marker armed' (-not (Test-Path (Join-Path $f3.updates 'just-applied-windows-x86_64'))) 'marker armed on failure'

Write-Host "`n=== 4. rollback after apply ===" -ForegroundColor Cyan
$p4 = Join-Path $root 'p4'
$f4 = New-Fixture $p4 'v0.5.0' 'v0.6.0'
# Give last-good a real directory to point at (the pre-apply checkout).
$oldCheckout = Join-Path $p4 'repo\versions\v0.5.0'
New-Item -ItemType Directory -Force -Path $oldCheckout | Out-Null
New-Item -ItemType Junction -Path (Join-Path $p4 'repo\current') -Target $oldCheckout | Out-Null
& $apply -Prefix $p4 6>&1 2>&1 | Out-Null
$appliedOk = (Get-Content (Join-Path $f4.bin 'sot.exe') -Raw) -eq 'NEW-BINARY'
Check 'applied first' $appliedOk 'apply did not happen'
$out4 = & $apply -Prefix $p4 -Rollback 6>&1 2>&1
Write-Host ($out4 | ForEach-Object { "    $_" }) -ForegroundColor DarkGray
Check 'binary restored' ((Get-Content (Join-Path $f4.bin 'sot.exe') -Raw) -eq 'OLD-BINARY') 'not restored'
$ij4 = Get-Content (Join-Path $p4 'install.json') -Raw
Check 'install.json rolled back' ($ij4 -match '"tag":\s*"v0\.5\.0"') 'tag not rolled back'
Check 'bad marker written' (Test-Path (Join-Path $f4.updates 'bad-v0.6.0-windows-x86_64')) 'no bad marker'
$b4 = [System.IO.File]::ReadAllBytes((Join-Path $p4 'install.json'))
Check 'install.json has no BOM after rollback' `
    (-not ($b4[0] -eq 0xEF -and $b4[1] -eq 0xBB -and $b4[2] -eq 0xBF)) 'BOM written on the rollback path'
Check 'marker cleared' (-not (Test-Path (Join-Path $f4.updates 'just-applied-windows-x86_64'))) 'marker still armed'

Write-Host "`n=== 5. already at tag: clears stale pointer ===" -ForegroundColor Cyan
$p5 = Join-Path $root 'p5'
$f5 = New-Fixture $p5 'v0.6.0' 'v0.6.0'
& $apply -Prefix $p5 6>&1 2>&1 | Out-Null
Check 'stale pointer cleared' (-not (Test-Path (Join-Path $f5.updates 'pending-windows-x86_64.json'))) 'pointer kept'
Check 'binary untouched' ((Get-Content (Join-Path $f5.bin 'sot.exe') -Raw) -eq 'OLD-BINARY') 'mutated'

Write-Host "`n=== 6. wrong-target pointer is refused ===" -ForegroundColor Cyan
$p6 = Join-Path $root 'p6'
$f6 = New-Fixture $p6 'v0.5.0' 'v0.6.0'
$pp = Join-Path $f6.updates 'pending-windows-x86_64.json'
(Get-Content $pp -Raw) -replace '"windows-x86_64"', '"linux-x86_64"' | Set-Content -LiteralPath $pp -NoNewline
& $apply -Prefix $p6 6>&1 2>&1 | Out-Null
Check 'binary untouched' ((Get-Content (Join-Path $f6.bin 'sot.exe') -Raw) -eq 'OLD-BINARY') 'applied a foreign-target update'
Check 'pointer dropped' (-not (Test-Path $pp)) 'pointer kept'

Write-Host "`n=== 7. staging lock held: skips ===" -ForegroundColor Cyan
$p7 = Join-Path $root 'p7'
$f7 = New-Fixture $p7 'v0.5.0' 'v0.6.0'
New-Item -ItemType Directory -Force -Path (Join-Path $f7.updates '.lock') | Out-Null
$out7 = & $apply -Prefix $p7 6>&1 2>&1
Check 'skipped under lock' ((Get-Content (Join-Path $f7.bin 'sot.exe') -Raw) -eq 'OLD-BINARY') 'applied while locked'
Check 'said so' (($out7 -join ' ') -match 'lock held') "log was: $out7"

Write-Host "`n=== 8. prefix given as an 8.3 short path still applies ===" -ForegroundColor Cyan
# Regression: the post-flip check used to compare the junction target to the
# wanted checkout as raw strings. Windows reports the same directory under both
# the short (PROGRA~1) and long spelling, so a correct junction read as "did not
# flip" and the applier restored instead of completing -- i.e. that machine
# could never accept an update. The CI runner hit it for real: its TEMP is
# C:\Users\RUNNER~1\... while the junction's Target expands to ...\runneradmin\...
$longName = Join-Path $root 'a-deliberately-long-directory-name-for-8dot3'
New-Item -ItemType Directory -Force -Path $longName | Out-Null
$short = $longName
try {
    $fso = New-Object -ComObject Scripting.FileSystemObject
    $short = $fso.GetFolder($longName).ShortPath
} catch {}
if ($short -eq $longName -or $short -notmatch '~') {
    Write-Host "  SKIP  8.3 short names unavailable on this volume" -ForegroundColor Yellow
} else {
    Write-Host "  using short prefix: $short" -ForegroundColor DarkGray
    $p8 = Join-Path $short 'p8'
    $f8 = New-Fixture $p8 'v0.5.0' 'v0.6.0'
    $out8 = & $apply -Prefix $p8 6>&1 2>&1
    Write-Host ($out8 | ForEach-Object { "    $_" }) -ForegroundColor DarkGray
    Check '8.3 prefix: binary swapped' ((Get-Content (Join-Path $f8.bin 'sot.exe') -Raw) -eq 'NEW-BINARY') 'did not apply through a short path'
    Check '8.3 prefix: pointer cleared' (-not (Test-Path (Join-Path $f8.updates 'pending-windows-x86_64.json'))) 'pointer kept'
    Check '8.3 prefix: marker armed' (Test-Path (Join-Path $f8.updates 'just-applied-windows-x86_64')) 'no marker'
}

Write-Host "`n================ $pass passed, $fail failed ================" -ForegroundColor $(if ($fail) { 'Red' } else { 'Green' })
Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
if ($fail) { exit 1 }

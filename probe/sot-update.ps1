# sot-update.ps1: the update of docs/UPDATE-AGENT.md step 3. Options go in $opts, for example @('-Hub', 'myhub').
$opts = @()
$log = Join-Path $HOME 'sot-update.log'
[IO.File]::WriteAllText($log, '')
function Say($line) { "$line"; [IO.File]::AppendAllText($log, "$line`r`n") }
Set-Location $HOME
$tag = "$(curl.exe -fsSLI -o NUL -w '%{url_effective}' https://github.com/kalidke/ship-of-tools/releases/latest)".Split('/')[-1]
if ($tag -notmatch '^v[0-9]') { Say 'STOP: cannot reach GitHub'; exit 1 }
Say "newest release: $tag"
$f = Join-Path $env:TEMP "sot-install-$tag.ps1"
curl.exe -fsSL -o $f "https://raw.githubusercontent.com/kalidke/ship-of-tools/$tag/scripts/install.ps1"
if ($LASTEXITCODE -ne 0) { Say "STOP: could not fetch the installer of $tag"; exit 1 }
$env:SOT_INSTALL_TAG = $tag
$null | & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $f @opts 2>&1 | ForEach-Object { Say $_ }
exit $LASTEXITCODE

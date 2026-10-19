# scratch-profile.ps1 -- dot-source it to put this PowerShell session on a scratch profile (scratch-guard.ps1 says what
# that is): a fresh USERNAME `sot-scratch-<id>`, and APPDATA and LOCALAPPDATA in a new folder under TEMP. Only these
# three change; the suites' own fixtures do the rest. The folder is the caller's to remove. Windows PowerShell 5.1.
$sotScratchId = [guid]::NewGuid().ToString('N').Substring(0, 8)
$sotScratchHome = Join-Path $env:TEMP "sot-scratch-$sotScratchId"
New-Item -ItemType Directory -Force -Path (Join-Path $sotScratchHome 'AppData\Roaming'), (Join-Path $sotScratchHome 'AppData\Local') | Out-Null
$env:USERNAME = "sot-scratch-$sotScratchId"
$env:APPDATA = Join-Path $sotScratchHome 'AppData\Roaming'
$env:LOCALAPPDATA = Join-Path $sotScratchHome 'AppData\Local'

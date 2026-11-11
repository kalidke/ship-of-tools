  $f = Join-Path $env:TEMP 'sot-one-daemon.toml'
  Set-Content -LiteralPath $f -Encoding ascii -Value 'hub = "ALIAS"', '[host.ALIAS]', 'daemon = true'
  $env:SOT_HOSTS = $f; & "$env:LOCALAPPDATA\sot\bin\sotd.exe" status; Remove-Item Env:SOT_HOSTS
  
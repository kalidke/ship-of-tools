  Select-String '"hub"' "$env:LOCALAPPDATA\sot\install.json"
  & "$env:LOCALAPPDATA\sot\bin\sotd.exe" topology sync --hub HUB
  
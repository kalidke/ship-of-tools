Set-Location $HOME; $tag = (curl.exe -fsSLI -o NUL -w '%{url_effective}' https://github.com/kalidke/ship-of-tools/releases/latest).Split('/')[-1]; "newest release: $tag"
curl.exe -fsSL -o "$env:TEMP\sot-install.ps1" "https://raw.githubusercontent.com/kalidke/ship-of-tools/$tag/scripts/install.ps1"
Remove-Item Env:GITHUB_TOKEN, Env:GH_TOKEN -ErrorAction SilentlyContinue; $env:SOT_INSTALL_TAG = $tag
powershell -NoProfile -ExecutionPolicy Bypass -File "$env:TEMP\sot-install.ps1"

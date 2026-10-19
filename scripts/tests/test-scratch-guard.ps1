# test-scratch-guard.ps1 -- scratch-guard.ps1's rule, on values the test passes in (it reads no profile and starts nothing):
# each part of a real profile is refused, a scratch one passes, and scratch-profile.ps1 makes one the guard accepts.
# Windows PowerShell 5.1, ASCII only.
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\tests\test-scratch-guard.ps1
$ErrorActionPreference = 'Stop'
$pass = 0; $fail = 0
function Check([string]$name, [bool]$ok, [string]$detail) {
    if ($ok) { $script:pass++; Write-Host "  PASS  $name" -ForegroundColor Green }
    else { $script:fail++; Write-Host "  FAIL  $name -- $detail" -ForegroundColor Red }
}
# The guard file's function only: its last two lines act on the real environment.
$ast = [System.Management.Automation.Language.Parser]::ParseFile((Join-Path $PSScriptRoot 'scratch-guard.ps1'), [ref]$null, [ref]$null)
$fn = $ast.Find({ param($n) ($n -is [System.Management.Automation.Language.FunctionDefinitionAst]) -and $n.Name -eq 'Get-SotScratchRefusal' }, $true)
. ([scriptblock]::Create($fn.Extent.Text))

$tmp = 'C:\Users\someone\AppData\Local\Temp'
$sp = "$tmp\sot-scratch-1"
$cases = @(
    @{ N = 'a scratch profile passes'; U = 'sot-scratch-1'; L = "$sp\AppData\Local"; A = "$sp\AppData\Roaming"; Refused = $false },
    @{ N = 'the real USERNAME is refused'; U = 'someone'; L = "$sp\AppData\Local"; A = "$sp\AppData\Roaming"; Refused = $true },
    @{ N = 'the real LOCALAPPDATA is refused'; U = 'sot-scratch-1'; L = 'C:\Users\someone\AppData\Local'; A = "$sp\AppData\Roaming"; Refused = $true },
    @{ N = 'the real APPDATA is refused'; U = 'sot-scratch-1'; L = "$sp\AppData\Local"; A = 'C:\Users\someone\AppData\Roaming'; Refused = $true },
    @{ N = 'a path that only starts like TEMP is refused'; U = 'sot-scratch-1'; L = "${tmp}2\x"; A = "$sp\a"; Refused = $true },
    @{ N = 'TEMP itself is not inside a folder under TEMP'; U = 'sot-scratch-1'; L = $tmp; A = "$sp\a"; Refused = $true },
    @{ N = 'a path climbing out of TEMP is refused'; U = 'sot-scratch-1'; L = "$sp\..\..\Roaming"; A = "$sp\a"; Refused = $true },
    @{ N = 'a relative path is refused'; U = 'sot-scratch-1'; L = 'AppData\Local'; A = "$sp\a"; Refused = $true },
    @{ N = 'an empty USERNAME is refused'; U = ''; L = "$sp\l"; A = "$sp\a"; Refused = $true },
    @{ N = 'an unset LOCALAPPDATA is refused'; U = 'sot-scratch-1'; L = ''; A = "$sp\a"; Refused = $true },
    @{ N = 'the case of the folder names does not matter'; U = 'sot-scratch-1'; L = "$($sp.ToUpper())\l"; A = "$sp\a"; Refused = $false }
)
foreach ($c in $cases) {
    $r = Get-SotScratchRefusal $c.U $c.L $c.A @($tmp)
    Check $c.N (([bool]$r) -eq $c.Refused) "refusal: $r"
}
$r = Get-SotScratchRefusal 'someone' 'C:\x' 'C:\y' @($tmp)
Check 'the refusal is one line that names the real values' ($r -notmatch "[\r\n]" -and $r -like "*'someone'*" -and $r -like "*'C:\x'*" -and $r -like '*scratch-profile.ps1*') $r

# A child PowerShell running CODE (encoded, so no quote is lost on the way); its output lines, then its exit code last.
function Invoke-Child([string]$Code) {
    $enc = [Convert]::ToBase64String([System.Text.Encoding]::Unicode.GetBytes($Code))
    $saved = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
    $out = @(& powershell -NoProfile -ExecutionPolicy Bypass -EncodedCommand $enc 2>&1 | ForEach-Object { "$_" })
    $rc = $LASTEXITCODE
    $ErrorActionPreference = $saved
    return @{ Out = $out; Exit = $rc }
}

# The two scripts end to end, in a child, so this session's environment is untouched.
$env:SOT_T = $PSScriptRoot
$r = Invoke-Child '. (Join-Path $env:SOT_T "scratch-profile.ps1"); . (Join-Path $env:SOT_T "scratch-guard.ps1"); Write-Output ("made " + $env:USERNAME + " " + $env:LOCALAPPDATA)'
$line = [string]($r.Out | Where-Object { $_ -like 'made sot-scratch-*' } | Select-Object -First 1)
Check 'scratch-profile.ps1 makes a profile the guard accepts' ($r.Exit -eq 0 -and $line -ne '') "exit $($r.Exit), got: $($r.Out)"
if ($line -match 'made (\S+) (.+)$') { $made = Split-Path (Split-Path $Matches[2] -Parent) -Parent; if ($made -like '*\sot-scratch-*') { Remove-Item -LiteralPath $made -Recurse -Force -ErrorAction SilentlyContinue } }
$r = Invoke-Child '$env:USERNAME = "someone"; . (Join-Path $env:SOT_T "scratch-guard.ps1"); Write-Output "RAN"'
Remove-Item Env:\SOT_T
Check 'the guard ends the whole process under a real USERNAME at once, exit 2, one line' ($r.Exit -eq 2 -and @($r.Out | Where-Object { $_ -eq 'RAN' }).Count -eq 0 -and @($r.Out | Where-Object { $_ -like 'scratch-guard: refusing*' }).Count -eq 1) "exit $($r.Exit), got: $($r.Out)"
Write-Host "`n$pass passed, $fail failed"
if ($fail -gt 0) { exit 1 }

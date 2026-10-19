# test-native-stderr.ps1 -- a script under $ErrorActionPreference = 'Stop' must not depend on how its caller redirects
# (#49). Windows PowerShell 5.1 turns each line a native command writes to stderr into a terminating NativeCommandError
# when the script's output is redirected (`*>`, `2>&1`), whatever the exit code. sot-install-layout.ps1's
# Invoke-SotNative is the one way those scripts call a native command: stderr never ends the script, the exit code
# is the answer, a command that fails still stops the caller that tests it, and one that cannot start is Code 127.
# Then pack-build.ps1 and relaunch-sot.ps1, run redirected against a stand-in cargo first on the child's PATH: cargo's
# stderr warnings on a successful build do not end them, and a failed build still does, with their own message.
# Run under WINDOWS POWERSHELL 5.1: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\tests\test-native-stderr.ps1

$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$scripts = Join-Path $repoRoot 'scripts'
$work = Join-Path $env:TEMP ("sot-native-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Force -Path $work | Out-Null
$pass = 0; $fail = 0
function Check([string]$name, [bool]$ok, [string]$detail) {
    if ($ok) { $script:pass++; Write-Host "  PASS  $name" -ForegroundColor Green }
    else { $script:fail++; Write-Host "  FAIL  $name -- $detail" -ForegroundColor Red }
}

# Runs the script $Text in a child Windows PowerShell whose environment is this process's with $Env laid over it;
# returns @{ Code; Out } (stdout and stderr). A child still running after $Seconds is killed.
function Invoke-Child([string]$Name, [string]$Text, [hashtable]$Env = @{}, [int]$Seconds = 120) {
    $file = Join-Path $work "$Name.ps1"
    Set-Content -LiteralPath $file -Value $Text -Encoding ascii
    $psi = New-Object System.Diagnostics.ProcessStartInfo('powershell.exe', ('-NoProfile -ExecutionPolicy Bypass -File "{0}"' -f $file))
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    foreach ($k in $Env.Keys) { $psi.EnvironmentVariables[$k] = [string]$Env[$k] }
    $p = [System.Diagnostics.Process]::Start($psi)
    $out = $p.StandardOutput.ReadToEndAsync()
    $err = $p.StandardError.ReadToEndAsync()
    if (-not $p.WaitForExit($Seconds * 1000)) {
        try { $p.Kill() } catch { }
        throw "${Name}: the child had not ended after $Seconds s"
    }
    return @{ Code = $p.ExitCode; Out = ($out.Result + $err.Result) }
}

# A git repo with a remote-tracking branch holding HEAD (pack-build's two preconditions) beside copies of the scripts
# under test, and a bin folder for a stand-in cargo.
function Invoke-Setup([string]$Dir) {
    $root = Join-Path $work $Dir
    New-Item -ItemType Directory -Force -Path (Join-Path $root 'repo\scripts'), (Join-Path $root 'repo\rust'), (Join-Path $root 'bin') | Out-Null
    foreach ($f in 'pack-build.ps1', 'relaunch-sot.ps1', 'sot-install-layout.ps1') {
        $src = Join-Path $scripts $f
        Copy-Item -LiteralPath $src -Destination (Join-Path $root 'repo\scripts')
    }
    $setup = Join-Path $root 'setup.ps1'
    Set-Content -LiteralPath $setup -Encoding ascii -Value @"
`$ErrorActionPreference = 'Continue'
Set-Location '$root\repo'
git init -q 2>&1 | Out-Null
git config user.email t@t 2>&1 | Out-Null
git config user.name t 2>&1 | Out-Null
git add -A 2>&1 | Out-Null
git commit -qm one 2>&1 | Out-Null
git init -q --bare '$root\origin.git' 2>&1 | Out-Null
git remote add origin '$root\origin.git' 2>&1 | Out-Null
git push -q origin HEAD:refs/heads/main 2>&1 | Out-Null
git fetch -q origin 2>&1 | Out-Null
"@
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $setup 2>&1 | Out-Null
    return $root
}
function Set-Cargo([string]$Root, [int]$Exit) {
    Set-Content -LiteralPath (Join-Path $Root 'bin\cargo.cmd') -Encoding ascii -Value "@echo off`r`necho warning: stand-in cargo warns 1>&2`r`nexit /b $Exit"
}

try {
    Write-Host "`n=== 1. the helper, under Stop, with its output redirected ===" -ForegroundColor Cyan
    $layout = Join-Path $scripts 'sot-install-layout.ps1'
    $c = Invoke-Child 'helper' @"
`$ErrorActionPreference = 'Stop'
. '$layout'
`$a = Invoke-SotNative 'cmd' @('/c', 'echo out1& echo warn1 1>&2& exit 0')
"ok: code=`$(`$a.Code) out=`$(`$a.Out -join ',') err=`$(`$a.Err -join ',')"
`$b = Invoke-SotNative 'cmd' @('/c', 'echo warn2 1>&2& exit 3')
"fail: code=`$(`$b.Code)"
`$n = Invoke-SotNative 'sot-no-such-command-b44' @()
"missing: code=`$(`$n.Code)"
`$e = Invoke-SotNative 'cmd' @('/c', 'echo shown& exit 0') -Echo
"after"
"@
    Check 'a zero exit with stderr is Code 0, the lines apart' ($c.Out -match 'ok: code=0 out=out1\s*,?\s*err=warn1') $c.Out
    Check 'a non-zero exit is its Code and the script goes on' (($c.Out -match 'fail: code=3') -and ($c.Out -match 'after')) $c.Out
    Check 'a command that cannot start is Code 127' ($c.Out -match 'missing: code=127') $c.Out
    Check '-Echo writes the lines to the host' ($c.Out -match 'shown') $c.Out

    Write-Host "`n=== 2. the premise: bare, a native stderr line under Stop and a redirect ends the script ===" -ForegroundColor Cyan
    $c = Invoke-Child 'bare' @'
$ErrorActionPreference = 'Stop'
$o = & { cmd /c "echo warn 1>&2& exit 0"; 'survived' } 2>&1
"result: $($o -join '|')"
'@
    Check 'without the helper the redirected call ends the script (the defect the helper removes)' (($c.Out -match 'NativeCommandError|RemoteException') -and ($c.Out -notmatch 'result:.*survived')) $c.Out

    Write-Host "`n=== 3. pack-build.ps1, redirected (*> and 2>&1), against a cargo that warns on stderr ===" -ForegroundColor Cyan
    $root = Invoke-Setup 'pack'
    $pack = Join-Path $root 'repo\scripts\pack-build.ps1'
    $pathEnv = @{ PATH = "$root\bin;$env:PATH" }
    Set-Cargo $root 0
    foreach ($form in '*>&1', '2>&1') {
        $c = Invoke-Child 'pack-ok' "`$o = & '$pack' -Out '$root\out' $form | Out-String; `$o" $pathEnv
        Check "cargo warns and exits 0 ($form): the build is taken as done, the script goes on to the next step" (($c.Out -notmatch 'NativeCommandError') -and ($c.Out -notmatch 'cargo build failed')) $c.Out
    }
    Set-Cargo $root 1
    foreach ($form in '*>&1', '2>&1') {
        $c = Invoke-Child 'pack-fail' "`$o = & '$pack' -Out '$root\out' $form | Out-String; `$o" $pathEnv
        Check "cargo exits 1 ($form): the script stops with its own message" (($c.Out -match 'pack-build: cargo build failed \(1\)') -and ($c.Out -notmatch 'NativeCommandError')) $c.Out
    }

    Write-Host "`n=== 4. relaunch-sot.ps1, redirected, against a cargo that warns and fails ===" -ForegroundColor Cyan
    $c = Invoke-Child 'relaunch-fail' "`$o = & '$root\repo\scripts\relaunch-sot.ps1' *>&1 | Out-String; `$o; exit `$LASTEXITCODE" $pathEnv
    Check 'a failed build stops it with its own message and cargo exit code' (($c.Out -match 'Build failed \(exit 1\)') -and ($c.Out -notmatch 'NativeCommandError') -and ($c.Code -eq 1)) "code $($c.Code): $($c.Out)"
} finally {
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host ""
Write-Host "$pass passed, $fail failed"
if ($fail -gt 0) { exit 1 }
exit 0

# test-install-entry-julia.ps1 -- the Julia an install leaves meets the kernel's compat floor: the PATH julia (the refresh's)
# and juliaup's default (the daemon's, read from juliaup.json in a scratch JULIAUP_DEPOT_PATH). Dot-sourced by
# test-install-entry.ps1 inside its section 7, in the same scope (New-Home, Invoke-Entry, Invoke-Case, Check, $r98, ...),
# never run alone. Windows PowerShell 5.1, ASCII only.

    # 23b. The Julia the install leaves meets the kernel's compat floor: the PATH julia (the refresh's) and juliaup's default
    # (the daemon's, read from juliaup.json in a scratch JULIAUP_DEPOT_PATH) at or above it change nothing; below it, or
    # unaskable, is ONE STEP LEFT naming the version and the command that fixes it.
    Invoke-Case '7jf' {
        # juliaup's state in a scratch depot: the default channel at <Ver>, and a 1.12 channel when -Has12.
        function Set-JuliaupDefault([string]$Home_, [string]$Ver, [switch]$Has12) {
            $d = Join-Path $Home_ 'juliaup-depot\juliaup'; New-Item -ItemType Directory -Force -Path $d | Out-Null
            $extra = $(if ($Has12) { ',"1.12":{"Version":"1.12.7+0.x64.w64.mingw32"}' } else { '' })
            Set-Content -LiteralPath (Join-Path $d 'juliaup.json') -Value ('{"Default":"release","InstalledChannels":{"release":{"Version":"' + $Ver + '+0.x64.w64.mingw32"}' + $extra + '},"InstalledVersions":{}}')
            $env:JULIAUP_DEPOT_PATH = Join-Path $Home_ 'juliaup-depot'
        }
        # The daemon's Julia is juliaup's here, not SOT_JULIA_BIN's; a juliaup stand-in is on PATH only when asked for.
        $stubsUp = Join-Path $entryRoot 'stubs-juliaup'; New-Item -ItemType Directory -Force -Path $stubsUp | Out-Null
        Set-Content -LiteralPath (Join-Path $stubsUp 'juliaup.cmd') -Value "@echo off`r`nexit /b 0"
        function Start-JuliaCase([string]$Name, [string]$PathVer, [switch]$Juliaup) {
            $h = New-Home $Name
            [Environment]::SetEnvironmentVariable('SOT_JULIA_BIN', $null)
            if ($PathVer) { $env:SOT_STUB_JULIA_VER = $PathVer }
            # Whatever juliaup the box has is off PATH: the case owns the one it asks for.
            $env:PATH = (@($env:PATH -split ';' | Where-Object { $_ -and -not (Test-Path -LiteralPath (Join-Path $_ 'juliaup.exe')) -and -not (Test-Path -LiteralPath (Join-Path $_ 'juliaup.cmd')) }) -join ';')
            if ($Juliaup) { $env:PATH = "$stubsUp;$env:PATH" }
            return $h
        }
        $hj = Start-JuliaCase 'jf-ok' '' -Juliaup
        Set-JuliaupDefault $hj.Home '1.12.0'
        $e = Invoke-Entry @('-Archive', $r98.Pack)
        Check '7jf: a default at the floor and a PATH julia at it is PASS' ($e.Rc -eq 0 -and $e.Last -like 'PASS: *') "rc=$($e.Rc) last='$($e.Last)'"
        $hj = Start-JuliaCase 'jf-above' '1.13.2' -Juliaup
        Set-JuliaupDefault $hj.Home '1.13.2'
        $e = Invoke-Entry @('-Archive', $r98.Pack)
        Check '7jf: a default above the floor is PASS' ($e.Rc -eq 0 -and $e.Last -like 'PASS: *') "rc=$($e.Rc) last='$($e.Last)'"
        $hj = Start-JuliaCase 'jf-low' '1.11.7' -Juliaup
        Set-JuliaupDefault $hj.Home '1.11.7'
        $e = Invoke-Entry @('-Archive', $r98.Pack)
        Check '7jf: a default below the floor with no 1.12 channel stops before the layout and adds, then sets the default' ($e.Rc -eq 1 -and $e.Verdicts -eq 1 -and $e.Last -like 'FAIL: julia: the daemon runs Julia 1.11.7*, below the 1.12 the kernel needs: run  juliaup add 1.12; juliaup default 1.12*') "rc=$($e.Rc) last='$($e.Last)'"
        $hj = Start-JuliaCase 'jf-low-has' '1.11.7' -Juliaup
        Set-JuliaupDefault $hj.Home '1.11.7' -Has12
        $e = Invoke-Entry @('-Archive', $r98.Pack)
        Check '7jf: a default below the floor with the 1.12 channel installed names juliaup default alone' ($e.Rc -eq 1 -and $e.Last -like 'FAIL: julia: *run  juliaup default 1.12') "rc=$($e.Rc) last='$($e.Last)'"
        $hj = Start-JuliaCase 'jf-low-noup' '1.11.7'
        Set-JuliaupDefault $hj.Home '1.11.7'
        $e = Invoke-Entry @('-Archive', $r98.Pack)
        Check '7jf: juliaup.json present and no juliaup on PATH says to install Julia 1.12 or later' ($e.Rc -eq 1 -and $e.Last -like 'FAIL: julia: the daemon runs Julia 1.11.7*install Julia 1.12 or later*') "rc=$($e.Rc) last='$($e.Last)'"
        $hj = Start-JuliaCase 'jf-nojuliaup-low' '1.10.5'
        $e = Invoke-Entry @('-Archive', $r98.Pack)
        Check '7jf: no juliaup and a PATH julia below the floor stops, saying to install Julia 1.12 or later' ($e.Rc -eq 1 -and $e.Last -like 'FAIL: julia: the daemon runs Julia 1.10.5*install Julia 1.12 or later*') "rc=$($e.Rc) last='$($e.Last)'"
        $hj = Start-JuliaCase 'jf-nojuliaup-high' '1.12.4'
        $e = Invoke-Entry @('-Archive', $r98.Pack)
        Check '7jf: no juliaup and a PATH julia above the floor is PASS' ($e.Rc -eq 0 -and $e.Last -like 'PASS: *') "rc=$($e.Rc) last='$($e.Last)'"
        $hj = Start-JuliaCase 'jf-daemon-low' '1.12.4' -Juliaup
        Set-JuliaupDefault $hj.Home '1.11.7' -Has12
        $e = Invoke-Entry @('-Archive', $r98.Pack)
        Check '7jf: a default below the floor with a PATH julia above it is ONE STEP LEFT, the run goes on' ($e.Rc -eq 0 -and $e.Verdicts -eq 1 -and $e.Last -like 'ONE STEP LEFT: the daemon runs Julia 1.11.7*run  juliaup default 1.12*') "rc=$($e.Rc) last='$($e.Last)'"
        $hj = Start-JuliaCase 'jf-refresh-low' '1.11.7' -Juliaup
        Set-JuliaupDefault $hj.Home '1.12.0' -Has12
        $e = Invoke-Entry @('-Archive', $r98.Pack)
        Check '7jf: a default at the floor and a PATH julia below it stops, naming the refresh and the PATH julia' ($e.Rc -eq 1 -and $e.Last -like 'FAIL: julia: the comm refresh runs Julia 1.11.7*ahead of *' -and $e.Last -notlike '*the daemon runs*') "rc=$($e.Rc) last='$($e.Last)'"
        $hj = Start-JuliaCase 'jf-bin-low' '1.12.4' -Juliaup
        Set-JuliaupDefault $hj.Home '1.12.0' -Has12
        $env:SOT_JULIA_BIN = $juliaOk; $env:SOT_STUB_BIN_VER = '1.11.7'
        $e = Invoke-Entry @('-Archive', $r98.Pack)
        Check '7jf: SOT_JULIA_BIN below the floor with the default at it is ONE STEP LEFT, never PASS' ($e.Rc -eq 0 -and $e.Last -like 'ONE STEP LEFT: the daemon runs Julia 1.11.7*SOT_JULIA_BIN*') "rc=$($e.Rc) last='$($e.Last)'"
        $hj = Start-JuliaCase 'jf-unasked' '' -Juliaup
        Set-JuliaupDefault $hj.Home '1.12.0'; $env:SOT_STUB_JULIA_VER = 'none'
        $e = Invoke-Entry @('-Archive', $r98.Pack)
        Check '7jf: a PATH julia that cannot be asked is ONE STEP LEFT' ($e.Rc -eq 0 -and $e.Last -like 'ONE STEP LEFT: *could not be asked*') "rc=$($e.Rc) last='$($e.Last)'"
    }

# test-install-entry-julia.ps1 -- the Julia an install leaves meets the kernel's compat floor: the PATH julia (the refresh's)
# and juliaup's default (the daemon's, read from juliaup.json in a scratch JULIAUP_DEPOT_PATH). Dot-sourced by
# test-install-entry.ps1 inside its section 7, in the same scope (New-Home, Invoke-Entry, Invoke-Case, Check, $r98, ...),
# never run alone. Windows PowerShell 5.1, ASCII only.

    # 23b. The Julia the install leaves meets the kernel's compat floor: the PATH julia (the refresh's) and juliaup's default
    # (the daemon's, read from juliaup.json in a scratch JULIAUP_DEPOT_PATH) at or above it change nothing; below it, or
    # unaskable, is ONE STEP LEFT naming the version and the command that fixes it.
    Invoke-Case '7jf' {
        function Set-JuliaupDefault([string]$Home_, [string]$Ver) {
            $d = Join-Path $Home_ 'juliaup-depot\juliaup'; New-Item -ItemType Directory -Force -Path $d | Out-Null
            Set-Content -LiteralPath (Join-Path $d 'juliaup.json') -Value ('{"Default":"release","InstalledChannels":{"release":{"Version":"' + $Ver + '+0.x64.w64.mingw32"}},"InstalledVersions":{}}')
            $env:JULIAUP_DEPOT_PATH = Join-Path $Home_ 'juliaup-depot'
        }
        $hj = New-Home 'jf-ok'
        Set-JuliaupDefault $hj.Home '1.12.0'
        $e = Invoke-Entry @('-Archive', $r98.Pack)
        Check '7jf: a default at the floor and a PATH julia at it is PASS' ($e.Rc -eq 0 -and $e.Last -like 'PASS: *') "rc=$($e.Rc) last='$($e.Last)'"
        $hj = New-Home 'jf-above'
        Set-JuliaupDefault $hj.Home '1.13.2'; $env:SOT_STUB_JULIA_VER = '1.13.2'
        $e = Invoke-Entry @('-Archive', $r98.Pack)
        Check '7jf: a default above the floor is PASS' ($e.Rc -eq 0 -and $e.Last -like 'PASS: *') "rc=$($e.Rc) last='$($e.Last)'"
        $hj = New-Home 'jf-low'
        Set-JuliaupDefault $hj.Home '1.11.7'; $env:SOT_STUB_JULIA_VER = '1.11.7'
        $e = Invoke-Entry @('-Archive', $r98.Pack)
        Check '7jf: a default below the floor is ONE STEP LEFT naming the version and juliaup default' ($e.Rc -eq 0 -and $e.Verdicts -eq 1 -and $e.Last -like 'ONE STEP LEFT: the daemon runs Julia 1.11.7*, below the 1.12 the kernel needs: run  juliaup default 1.12*') "rc=$($e.Rc) last='$($e.Last)'"
        $hj = New-Home 'jf-nojuliaup-low'
        $env:SOT_STUB_JULIA_VER = '1.10.5'
        $e = Invoke-Entry @('-Archive', $r98.Pack)
        Check '7jf: no juliaup and a PATH julia below the floor says to install Julia 1.12 or later' ($e.Rc -eq 0 -and $e.Last -like 'ONE STEP LEFT: the daemon runs Julia 1.10.5*install Julia 1.12 or later*') "rc=$($e.Rc) last='$($e.Last)'"
        $hj = New-Home 'jf-nojuliaup-high'
        $env:SOT_STUB_JULIA_VER = '1.12.4'
        $e = Invoke-Entry @('-Archive', $r98.Pack)
        Check '7jf: no juliaup and a PATH julia above the floor is PASS' ($e.Rc -eq 0 -and $e.Last -like 'PASS: *') "rc=$($e.Rc) last='$($e.Last)'"
        $hj = New-Home 'jf-unasked'
        Set-JuliaupDefault $hj.Home '1.12.0'; $env:SOT_STUB_JULIA_VER = 'none'
        $e = Invoke-Entry @('-Archive', $r98.Pack)
        Check '7jf: a PATH julia that cannot be asked is ONE STEP LEFT' ($e.Rc -eq 0 -and $e.Last -like 'ONE STEP LEFT: *could not be asked*') "rc=$($e.Rc) last='$($e.Last)'"
    }

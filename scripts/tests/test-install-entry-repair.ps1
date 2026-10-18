# test-install-entry-repair.ps1 -- the repair cases of test-install-entry.ps1: scripts/install.ps1 over an install.json that is
# missing, holds a pin and no version, or is cut short, with and without a version named. Dot-sourced by
# test-install-entry.ps1 inside its section 7, in the same scope ($r98, $r99, New-Home, Invoke-Entry, Invoke-Case, Check, ...),
# never run alone. Windows PowerShell 5.1, ASCII only.

    # An install whose record is missing, holds a pin and no version, or is cut before its version is updated, never
    # copied over (a run of "fresh" would write bin\sotd.exe under a running daemon). Each run is beside a stand-in
    # sotd.exe that runs as another version; the installed checkout's sot-local-daemon.ps1 stand-in ends it, as in 7w. A run
    # that names a version pins; "update sot from the repo" names none, goes on over a record it cannot read, and does not.
    Invoke-Case '7z' {
        $n = 0
        foreach ($c in @(
            @{ Name = 'a missing record'; Record = $null; Named = $true },
            @{ Name = 'a record pinned with no version'; Record = '{"schema": 1, "prefix": "x", "pinned": true}'; Named = $true },
            @{ Name = 'a record cut before its version'; Record = '{"schema": 1, "ver'; Named = $false },
            @{ Name = 'a missing record, no version named'; Record = $null; Named = $false })) {
            $n++
            $hz = New-Home "z$n"
            [void](Invoke-Entry @('-Archive', $r98.Pack))
            $ijz = Join-Path $hz.Prefix 'install.json'
            if ($null -eq $c.Record) { Remove-Item -LiteralPath $ijz -Force } else { [System.IO.File]::WriteAllText($ijz, $c.Record, (New-Object System.Text.UTF8Encoding($false))) }
            $env:SOT_FAKE_SLEEP = '1'
            $heldz = Start-Process -FilePath (Join-Path $hz.Prefix 'bin\sotd.exe') -PassThru -WindowStyle Hidden
            [Environment]::SetEnvironmentVariable('SOT_FAKE_SLEEP', $null)
            $dfz = Join-Path $hz.Home 'daemon.code'
            Set-Content -LiteralPath $dfz -Value '3'
            $env:SOT_FAKE_DAEMON_FILE = $dfz
            Set-Content -LiteralPath (Join-Path $hz.Prefix 'repo\versions\v9.9.8\scripts\sot-local-daemon.ps1') -Value @"
Stop-Process -Id $($heldz.Id) -Force
Set-Content -LiteralPath '$dfz' -Value '2'
Write-Host 'sot-local-daemon: stopped'
"@
            try {
                Start-Sleep -Seconds 2
                if ($c.Named) { $e = Invoke-Entry @('-Archive', $r99.Pack) }
                else { $env:SOT_INSTALL_TAG = 'v9.9.9'; $env:SOT_INSTALL_IMPLICIT = '1'; $script:servePack = $r99.Pack; $e = Invoke-Entry @() }
                Check "7z: an update over $($c.Name) beside a running sotd.exe ends in PASS, never FAIL: binaries" ($e.Rc -eq 0 -and $e.Last -like 'PASS: *9.9.9*' -and $heldz.HasExited) "rc=$($e.Rc) last='$($e.Last)'; $(($e.Out | Select-Object -Last 4) -join ' / ')"
                $back = $null; $text = $(if (Test-Path -LiteralPath $ijz) { [System.IO.File]::ReadAllText($ijz) } else { '(no install.json)' }); try { $back = $text | ConvertFrom-Json } catch { }
                Check "7z: over $($c.Name) the run ends with a whole record at 9.9.9, pinned only when it named a version" (($null -ne $back) -and $back.version -eq '9.9.9' -and (($back.pinned -eq $true) -eq $c.Named)) $text
            } finally { Stop-Process -Id $heldz.Id -Force -ErrorAction SilentlyContinue; [Environment]::SetEnvironmentVariable('SOT_FAKE_DAEMON_FILE', $null) }
        }
    }
    # An install.json cut short by an interrupted write cannot be read, so no automatic writer moves the install; the
    # person's own run is the repair: the stage goes on as over no record, and the run ends PASS with a whole record.
    Invoke-Case '7y' {
        $hv = New-Home 'v1'
        [void](Invoke-Entry @('-Archive', $r98.Pack))
        $ijv = Join-Path $hv.Prefix 'install.json'
        $whole = [System.IO.File]::ReadAllText($ijv)
        $cut = $whole.Substring(0, $whole.IndexOf('"pinned"')) + '"pinned": tr'
        [System.IO.File]::WriteAllText($ijv, $cut, (New-Object System.Text.UTF8Encoding($false)))
        $reads = $true; try { [void]($cut | ConvertFrom-Json) } catch { $reads = $false }
        Check '7y: the fixture record is cut short and cannot be read' (-not $reads) $cut
        $e = Invoke-Entry @('-Archive', $r99.Pack)
        Check '7y: an update over a record that cannot be read ends in PASS, pinned at the version it named' ("$($e.Rc) $($e.Verdicts) $($e.Last)" -ceq "0 1 PASS: Ship of Tools 9.9.9 at $($r99.Short) on this computer$(Get-PinTail '9.9.9')") "rc=$($e.Rc) last='$($e.Last)'; $(($e.Out | Select-Object -Last 6) -join ' / ')"
        $back = $null; try { $back = [System.IO.File]::ReadAllText($ijv) | ConvertFrom-Json } catch { }
        Check '7y: and leaves a whole, readable record that names the new version and is pinned' (($null -ne $back) -and $back.version -eq '9.9.9' -and $back.pinned -eq $true) ([System.IO.File]::ReadAllText($ijv))
        # install.ps1's own writer (the pin edit) replaces the record by a rename: a handle opened before still reads the old bytes.
        $old = [System.IO.File]::ReadAllText($ijv)
        $held = [System.IO.File]::Open($ijv, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, ([System.IO.FileShare]'ReadWrite,Delete'))
        $still = ''
        try {
            Set-InstallPin $ijv $false
            $held.Position = 0
            $still = (New-Object System.IO.StreamReader($held)).ReadToEnd()
        } finally { $held.Dispose() }
        Check '7y: the pin edit changed the record, and a handle opened before it reads the old bytes (a rename, not an in-place write)' (($still -ceq $old) -and -not ([System.IO.File]::ReadAllText($ijv) -match 'pinned') -and @(Get-ChildItem -LiteralPath $hv.Prefix -Filter 'install.json*' | Where-Object { $_.Name -ne 'install.json' }).Count -eq 0) $still
    }

    # A record cut inside its version cannot say what is installed, but bin\sotd.exe can: "update sot from the repo" toward an
    # older release stops at the direction step and moves nothing (the guard holds whenever the installed version can be read).
    Invoke-Case '7z3' {
        $hb = New-Home 'b3'
        [void](Invoke-Entry @('-Archive', $r99.Pack))
        $ijb = Join-Path $hb.Prefix 'install.json'
        [System.IO.File]::WriteAllText($ijb, "{`n  `"schema`": 1,`n  `"version`": `"9.9.", (New-Object System.Text.UTF8Encoding($false)))
        $env:SOT_INSTALL_TAG = 'v9.9.8'; $env:SOT_INSTALL_IMPLICIT = '1'; $script:servePack = $r98.Pack
        $e = Invoke-Entry @()
        $binb = (Get-BinaryVersion (Join-Path $hb.Prefix 'bin\sotd.exe')).Version
        Check '7z3: an implicit run toward 9.9.8 over a record cut inside its version, beside a 9.9.9 binary, stops at the direction step' ($e.Rc -eq 2 -and $e.Last -like 'FAIL: direction: this computer runs 9.9.9, which is newer than 9.9.8*') "rc=$($e.Rc) last='$($e.Last)'"
        Check '7z3: and the install still runs 9.9.9' ($binb -eq '9.9.9') "bin\sotd.exe reports '$binb'"
    }

    # A staged sotd that predates `--installed` rejects it (exit 2, read as `unknown`): a whole record's own version is not
    # handed on, so a re-run of the installed pack at the same commit is still seen as the pin-only repair.
    Invoke-Case '7z4' {
        $hn = New-Home 'n10'
        [void](Invoke-Entry @('-Archive', $r98.Pack))
        $env:SOT_FAKE_NO_INSTALLED = '1'
        try { $e = Invoke-Entry @('-Archive', $r98.Pack) } finally { [Environment]::SetEnvironmentVariable('SOT_FAKE_NO_INSTALLED', $null) }
        Check '7z4: against a sotd that rejects --installed, a re-run of the installed pack over a whole record is still a repair run' ($e.Rc -eq 0 -and ($e.Out -match 'a repair run').Count -eq 1 -and $e.Last -like 'PASS: *9.9.8*') "rc=$($e.Rc) last='$($e.Last)'; $(($e.Out | Select-Object -First 6) -join ' / ')"
    }

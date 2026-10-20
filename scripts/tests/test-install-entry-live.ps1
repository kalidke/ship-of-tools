# test-install-entry-live.ps1 -- the ask judges "live" from the daemon's own children: an old daemon that cannot report its Pluto
# server and has no REPL or Pluto child installs in one plain run; a REPL or Pluto child is live (no console: the one-line
# stop; a console: the question in place). Dot-sourced by test-install-entry.ps1 inside its section 7, in the same scope
# (New-Home, Invoke-Entry, Invoke-Case, Check, $r98, $r99, ...), never run alone. Windows PowerShell 5.1, ASCII only.

    Invoke-Case '7xl' {
        $harnessX = @{}
        foreach ($f in @('Get-ProcessSnapshot', 'Test-Console', 'Read-Answer')) { $harnessX[$f] = (Get-Item -LiteralPath "Function:$f").ScriptBlock }
        $script:AnswerText = ''; $script:Console = $false; $script:Asked = @()
        Set-Item -LiteralPath 'Function:script:Test-Console' -Value { return $script:Console }
        Set-Item -LiteralPath 'Function:script:Read-Answer' -Value { param([string]$Question) $script:Asked += $Question; return $script:AnswerText }
        function Add-Kid($List, [int]$Id, [string]$Line) { $List.Add([pscustomobject]@{ ProcessId = $Id; ParentProcessId = 4242; ExecutablePath = 'C:\x\julia.exe'; CommandLine = $Line }) }
        try {
            $pure = [System.Collections.Generic.List[object]]::new()
            $bin = 'C:\p\bin\sotd.exe'
            $pure.Add([pscustomobject]@{ ProcessId = 4242; ParentProcessId = 1; ExecutablePath = $bin; CommandLine = $bin })
            Add-Kid $pure 4300 'julia.exe --startup-file=no -e using ShipToolsKernel; ShipToolsKernel.serve(stdin, stdout)'
            $k = Select-DaemonChildren $pure $bin
            Check '7xl: a kernel child of the daemon is nothing live' ($null -ne $k -and @($k).Count -eq 0) "kinds='$($k -join ',')'"
            Add-Kid $pure 4301 'julia.exe --project=C:\w -e using ShipToolsRepl; ShipToolsRepl.run_shim(stdin, stdout)'
            Add-Kid $pure 4302 'julia.exe --project=C:\p C:\r\julia\pluto\start.jl'
            $pure.Add([pscustomobject]@{ ProcessId = 4400; ParentProcessId = 9999; ExecutablePath = 'C:\x\julia.exe'; CommandLine = 'julia.exe -e ShipToolsRepl.run_shim' })
            $k = Select-DaemonChildren $pure $bin
            Check '7xl: a REPL child and the Pluto server of the daemon are found, another parent is not' (($k -join ',') -ceq 'repl,pluto') ($k -join ',')
            Check '7xl: no process of the install daemon: nothing can be ruled out ($null)' ($null -eq (Select-DaemonChildren $pure 'C:\q\bin\sotd.exe')) 'not null'

            $setup = {
                param([string]$Name, [string[]]$Lines)
                $hx = New-Home $Name
                [void](Invoke-Entry @('-Archive', $r98.Pack))
                # A daemon the stand-ins cannot restart stays on its version, so a run that goes on ends at the desk step.
                $script:DeskSession = $false
                $exe = Join-Path $hx.Prefix 'bin\sotd.exe'
                $list = [System.Collections.Generic.List[object]]::new()
                $list.Add([pscustomobject]@{ ProcessId = 4242; ParentProcessId = 1; ExecutablePath = $exe; CommandLine = $exe })
                $n = 4300; foreach ($l in $Lines) { Add-Kid $list $n $l; $n++ }
                $script:SnapList = $list
                Set-Item -LiteralPath 'Function:script:Get-ProcessSnapshot' -Value { return $script:SnapList.ToArray() }
                $env:SOT_FAKE_DAEMON = '3'; $env:SOT_FAKE_REPLS = ''; $env:SOT_FAKE_REPLS_RC = '1'
                return $hx
            }
            $kernelLine = 'julia.exe --startup-file=no -e using ShipToolsKernel; ShipToolsKernel.serve(stdin, stdout)'
            $replLine = 'julia.exe --project=C:\w -e using ShipToolsRepl; ShipToolsRepl.run_shim(stdin, stdout)'
            $hx = & $setup 'x1' @($kernelLine)
            $e = Invoke-Entry @('-Archive', $r99.Pack)
            Check '7xl: an old daemon that cannot report Pluto, no REPL or Pluto child: one plain run, no flag, no stop' ($e.Rc -eq 0 -and $e.Verdicts -eq 1 -and $e.Last -match '^(PASS|ONE STEP LEFT): ') "rc=$($e.Rc) last='$($e.Last)'"
            Check '7xl: and it said it found nothing live' (($e.Out -like '*it has no REPL or Pluto child, so restarting it ends nothing live*').Count -eq 1) ($e.Out -join ' / ')
            $hx = & $setup 'x2' @($replLine)
            $e = Invoke-Entry @('-Archive', $r99.Pack)
            Check '7xl: a REPL child and no console: the FAIL, exit 2, nothing asked' ($e.Rc -eq 2 -and $e.Last -like 'FAIL: daemon: could not ask*' -and $script:Asked.Count -eq 0) "rc=$($e.Rc) last='$($e.Last)'"
            $script:Console = $true; $script:AnswerText = 'n'
            $e = Invoke-Entry @('-Archive', $r99.Pack)
            Check '7xl: a console answering n: the FAIL, after the question' ($e.Rc -eq 2 -and $e.Last -like 'FAIL: daemon: *' -and $script:Asked.Count -eq 1 -and $script:Asked[0] -ceq 'Restart the daemon? This ends a live REPL (the daemon could not name its row), and any other the daemon could not list [y/N]') "rc=$($e.Rc) asked='$($script:Asked -join ' / ')'"
            $script:AnswerText = 'y'
            $e = Invoke-Entry @('-Archive', $r99.Pack)
            Check '7xl: a console answering y goes on' ($e.Rc -eq 0 -and $e.Verdicts -eq 1 -and $e.Last -match '^(PASS|ONE STEP LEFT): ') "rc=$($e.Rc) last='$($e.Last)'"
        } finally {
            foreach ($f in $harnessX.Keys) { Set-Item -LiteralPath "Function:script:$f" -Value $harnessX[$f] }
            $env:SOT_FAKE_DAEMON = '3'; $env:SOT_FAKE_REPLS = ''; $env:SOT_FAKE_REPLS_RC = '0'
            $script:DeskSession = $true
        }
    }

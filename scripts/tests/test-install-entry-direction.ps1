# test-install-entry-direction.ps1 -- a build of the line (a + in its version) run with no -Version stops and says which way
# the newest release moves it, by ancestry: the release's COMMIT against the build's commit in the install's base clone.
# Forward, older, or could not tell with the reason; it still stops and asks. Dot-sourced by test-install-entry.ps1 inside its
# section 7, in the same scope (New-Home, Invoke-Entry, Invoke-Case, Check, $textLog, ...), never run alone. Windows
# PowerShell 5.1, ASCII only.

    Invoke-Case '7e2' {
        $hd = New-Home 'dir1'
        $base = Join-Path $hd.Prefix 'repo\base'
        New-Item -ItemType Directory -Force -Path $base | Out-Null
        $sh = {
            param([string[]]$A)
            $eap = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
            try { $o = & git.exe -C $base -c user.name=t -c user.email=t@t @A 2>&1 | ForEach-Object { "$_" } } finally { $ErrorActionPreference = $eap }
            return ($o -join '')
        }
        [void](& $sh @('init', '-q'))
        [void](& $sh @('commit', '-q', '--allow-empty', '-m', 'one')); $c1 = (& $sh @('rev-parse', 'HEAD'))
        [void](& $sh @('commit', '-q', '--allow-empty', '-m', 'two')); $c2 = (& $sh @('rev-parse', 'HEAD'))
        [void](& $sh @('commit', '-q', '--allow-empty', '-m', 'three')); $c3 = (& $sh @('rev-parse', 'HEAD'))
        $script:relCommit = $c2
        $script:curlTextImpl = { param([string]$Url, [bool]$Head)
            if ($Url -like '*api.github.com*') { return [pscustomobject]@{ Ok = $true; Code = 0; Text = '[{"tag_name": "v9.9.10"}]' } }
            if ($Url -like '*/releases/download/v9.9.10/COMMIT') { return [pscustomobject]@{ Ok = $true; Code = 0; Text = "$($script:relCommit)`n" } }
            return [pscustomobject]@{ Ok = $false; Code = 22; Text = '' } }
        $stop = { param([string]$V, [string]$Rest)
            [System.IO.File]::WriteAllText((Join-Path $hd.Prefix 'install.json'), '{"schema":1,"prefix":"x","version":"' + $V + '","tag":"v' + $V + '","commit":"0"}')
            $e = Invoke-Entry @()
            return ($e.Rc -eq 2 -and $e.Verdicts -eq 1 -and $e.Last -ceq "FAIL: target: this computer runs $V, a build of the line, not a release: install a pack of the line with -Archive <folder>; $Rest; ask the person who runs it which they want") }
        Check '7e2: a build the newest release contains is told forward, never "usually older"' (& $stop "9.9.9-dev+$($c1.Substring(0, 9))" 'the newest release, v9.9.10, contains this build: naming it with -Version moves this computer forward') "last=$($e.Last)"
        Check '7e2: a build past the newest release is told it moves back' (& $stop "9.9.9-dev+$($c3.Substring(0, 9))" 'the newest release, v9.9.10, does not contain this build: naming it with -Version moves this computer back') "last=$($e.Last)"
        Check '7e2: a build whose commit the clone lacks says it could not tell, and why' (& $stop '9.9.9-dev+abc1234' "could not tell which way the newest release, v9.9.10, moves this computer (this build's commit abc1234 is not in $base)") "last=$($e.Last)"
        Check '7e2: a build that names no commit is not guessed' (& $stop '9.9.9-dev+src' 'could not tell which way the newest release, v9.9.10, moves this computer (this build names no commit)') "last=$($e.Last)"
    }

    # Every git call of install.ps1 goes through its one Invoke-Git, which carries the stall bound (a blobless clone's
    # `cat-file -e` and `worktree add` run a lazy fetch, and the pair reaches that child). Read as a syntax tree, whole file:
    # no command names git or git.exe outside the function, whatever its verb (Get-Command git.exe only looks the program up).
    Invoke-Case '7e3' {
        $ast = [System.Management.Automation.Language.Parser]::ParseFile($entryScript, [ref]$null, [ref]$null)
        $wrap = $ast.Find({ param($n) $n -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -eq 'Invoke-Git' }, $true)
        $wrapText = $(if ($wrap) { $wrap.Extent.Text } else { '' })
        Check '7e3: Invoke-Git prepends the stall bound and runs git.exe through Invoke-Native' ($wrapText -match "http\.lowSpeedLimit=1000" -and $wrapText -match "http\.lowSpeedTime=60" -and $wrapText -match "Invoke-Native 'git\.exe'") "wrapper=$wrapText"
        $outside = @($ast.FindAll({ param($n)
            if ($n -isnot [System.Management.Automation.Language.StringConstantExpressionAst] -or $n.Value -notmatch '^git(\.exe)?$') { return $false }
            for ($p = $n.Parent; $p; $p = $p.Parent) {
                if ($wrap -and [object]::ReferenceEquals($p, $wrap)) { return $false }
                if ($p -is [System.Management.Automation.Language.CommandAst] -and $p.GetCommandName() -eq 'Get-Command') { return $false }
            }
            return $true }, $true))
        Check '7e3: no command in install.ps1 names git or git.exe outside Invoke-Git' ($outside.Count -eq 0) "outside=$(@($outside | ForEach-Object { $_.Extent.StartLineNumber }) -join ',')"
        $calls = @($ast.FindAll({ param($n) $n -is [System.Management.Automation.Language.CommandAst] -and $n.GetCommandName() -eq 'Invoke-Git' }, $true))
        Check '7e3: install.ps1 runs git through Invoke-Git' ($calls.Count -ge 10) "calls=$($calls.Count)"
    }

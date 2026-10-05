# test-local-daemon-pipe.ps1 -- part of test-local-daemon.ps1, dot-sourced inside section 5c and run in that
# section's scope ($pipe5c, $new5c, $old5c, $request5c, $p3 and the support helpers): the session pipe under load,
# cases (iii)-(vii). Large requests behind an accepted and a refused hello, the inbound buffer's memory, and a peer
# the daemon gives up on.
#
# ASCII ONLY: Windows PowerShell 5.1 decodes a BOM-less .ps1 as cp1252.
        # (iii)-(v): requests longer than the pipe's default 512-byte buffer and the daemon's 4 KB read-ahead, the
        # last at the envelope cap (1 MiB, newline included); the transport writes the hello and the request before it
        # reads. After an accepted hello the daemon reads on, so (iii) and (v) are answered with any buffer. After a
        # refusal it reads no more and holds the pipe open until the refusal is read, so (iv) needs an inbound buffer
        # that holds the request: with less, the transport hangs in its write.
        $head5c = '{"v":3,"id":1,"kind":"req","op":"version.query","payload":{"pad":"'
        $tail5c = '"}}'
        foreach ($case5c in @(@(16384, $new5c, 'iii'), @(16384, $old5c, 'iv'), @(1048576, $new5c, 'v'))) {
            $len5c = $case5c[0]
            $big5c = $head5c + ('x' * ($len5c - 1 - $head5c.Length - $tail5c.Length)) + $tail5c
            $r5c = Invoke-PipeTransport $pipe5c version.query @($case5c[1], $big5c)
            $first5c = $null
            if ($r5c.Out.Count -eq 1) { $first5c = $r5c.Out[0] | ConvertFrom-Json }
            if ($case5c[2] -eq 'iv') {
                $ok5c = (-not $r5c.Hung) -and ($r5c.Exit -eq 1) -and ($null -ne $first5c) -and ($first5c.op -eq 'hello') -and ($first5c.payload.code -eq 'protocol_mismatch')
                $what5c = "5c ($($case5c[2])): a refused hello behind a $len5c-byte request line is printed, exit 1"
            } else {
                $ok5c = (-not $r5c.Hung) -and ($r5c.Exit -eq 0) -and ($null -ne $first5c) -and ($first5c.op -eq 'version.query')
                $what5c = "5c ($($case5c[2])): a $len5c-byte request line is answered, exit 0"
            }
            Check $what5c $ok5c "hung: $($r5c.Hung) exit: $($r5c.Exit) stdout: $($r5c.Out -join ' | ') stderr: $($r5c.Err)"
        }
        # (vi): a regression check of `bind_session` (section 5d is P18's test): eight connections held open after their
        # hellos grow the daemon's nonpaged pool by less than one buffer (2 MiB).
        $daemon5c = Get-Process -Id (@(Get-DaemonProcs (Get-PipePath $pipe5c))[0].ProcessId)
        $before5c = $daemon5c.NonpagedSystemMemorySize64
        $held5c = @()
        $answered5c = 0
        try {
            $helloBytes5c = (New-Object System.Text.UTF8Encoding($false)).GetBytes($new5c + "`n")
            for ($i5c = 0; $i5c -lt 8; $i5c++) {
                $c5c = New-Object System.IO.Pipes.NamedPipeClientStream('.', $pipe5c, [System.IO.Pipes.PipeDirection]::InOut)
                $held5c += $c5c
                $c5c.Connect(3000)
                $c5c.Write($helloBytes5c, 0, $helloBytes5c.Length)
                $reader5c = New-Object System.IO.StreamReader($c5c, (New-Object System.Text.UTF8Encoding($false)), $false, 1024, $true)
                if ($reader5c.ReadLineAsync().Wait(3000)) { $answered5c++ }
            }
            $daemon5c.Refresh()
            $grew5c = $daemon5c.NonpagedSystemMemorySize64 - $before5c
            Check '5c (vi): eight connections are admitted and answered' ($answered5c -eq 8) "answered: $answered5c"
            Check '5c (vi): eight open connections set no inbound buffer aside' ($grew5c -lt 2097152) "the daemon's nonpaged pool grew $grew5c bytes"
        } finally {
            foreach ($c in $held5c) { try { $c.Dispose() } catch { } }
        }
        # (vii): a peer the daemon gives up on does not hold the closes behind it. One connection says hello and never
        # reads; three broadcasts fill its pipe until the daemon's 10 s write deadline gives up on it. A refused hello
        # after that must still be answered and closed: its close waits in interprocess's one linger thread, which the
        # given-up peer's close held for as long as that peer kept its handle.
        # The running daemon's stdout log: the launcher stamps each start's, and the newest by name is the current one.
        $log5c = (Get-ChildItem -LiteralPath (Join-Path $p3 'logs') -Filter 'sotd-local.stdout.*.log' | Sort-Object Name | Select-Object -Last 1).FullName
        $seen5c = @(Get-Content -LiteralPath $log5c -ErrorAction SilentlyContinue).Count
        $wedged5c = New-Object System.IO.Pipes.NamedPipeClientStream('.', $pipe5c, [System.IO.Pipes.PipeDirection]::InOut)
        try {
            $wedged5c.Connect(3000)
            $hb5c = (New-Object System.Text.UTF8Encoding($false)).GetBytes($new5c + "`n")
            $wedged5c.Write($hb5c, 0, $hb5c.Length)
            $text5c = 'x' * 4096
            for ($k5c = 1; $k5c -le 3; $k5c++) {
                $send5c = '{"v":3,"id":2,"kind":"req","op":"agent.send","payload":{"from":"t-sender","to":"t-nobody","text":"' + $text5c + '","id":"t-' + $k5c + '"}}'
                $null = Invoke-PipeTransport $pipe5c agent.send @($new5c, $send5c)
            }
            $gaveUp5c = $false
            for ($w5c = 0; $w5c -lt 300 -and -not $gaveUp5c; $w5c++) {
                $gaveUp5c = @(Get-Content -LiteralPath $log5c -ErrorAction SilentlyContinue | Select-Object -Skip $seen5c | Where-Object { $_ -match 'peer not draining' }).Count -gt 0
                if (-not $gaveUp5c) { Start-Sleep -Milliseconds 100 }
            }
            Check '5c (vii): the daemon gives up on a peer that never reads' $gaveUp5c 'no "peer not draining" line in the daemon log within 30 s'
            $r5c = Invoke-PipeTransport $pipe5c version.query @($old5c, $request5c)
            Check '5c (vii): a refused hello after it is still answered, and the daemon closes it' ((-not $r5c.Hung) -and ($r5c.Exit -eq 1) -and ($r5c.Out.Count -eq 1) -and ($r5c.Err -match 'closed before a matching reply')) "hung: $($r5c.Hung) exit: $($r5c.Exit) stdout: $($r5c.Out -join ' | ') stderr: $($r5c.Err)"
        } finally {
            try { $wedged5c.Dispose() } catch { }
        }

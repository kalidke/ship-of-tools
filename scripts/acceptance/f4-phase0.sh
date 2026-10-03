#!/usr/bin/env bash
# Windows supervisor premises (git-bash on a Windows box): P1-P3 PowerShell call semantics, P5 the local daemon grants
# a lease to a PowerShell process (only while a window holds a lease; it checks first), P9 hash ids agree, W1-pre whether
# the running supervisor runs the installed launcher code. Read-only apart from its own temp folder and one lease it releases.
D="$(cygpath -u "$LOCALAPPDATA")/Temp/sot-f4"; mkdir -p "$D"
cat > "$D/premises.ps1" <<'PS'
$ErrorActionPreference = 'Stop'
$d = Join-Path $env:TEMP 'sot-f4'
New-Item -ItemType Directory -Force -Path $d | Out-Null
"host: PowerShell $($PSVersionTable.PSVersion) $($PSVersionTable.PSEdition); Get-FileHash: $([bool](Get-Command Get-FileHash -ErrorAction SilentlyContinue))"
$c = Join-Path $d 'child.ps1'
Set-Content -LiteralPath $c -Encoding ascii -Value '"child v1"; $global:L += @(New-Object System.IO.MemoryStream)'
$global:L = @(New-Object System.IO.MemoryStream); $global:L[0].WriteByte(1)
$o1 = & $c
Set-Content -LiteralPath $c -Encoding ascii -Value '"child v2"; foreach ($m in $global:L) { $m.WriteByte(2) }; $global:L = @()'
$keep = $global:L
$o2 = & $c
if ($o1 -ceq 'child v1' -and $o2 -ceq 'child v2') { 'P1 PASS: & re-reads the script file at each call' } else { "P1 FAIL: got '$o1' then '$o2'" }
if ($keep.Count -eq 2 -and $keep[0].Length -eq 2 -and $keep[1].Length -eq 1 -and $global:L.Count -eq 0) { 'P2 PASS: a script called with & shares $global: and its objects' } else { "P2 FAIL: count=$($keep.Count) len0=$($keep[0].Length) len1=$($keep[1].Length) after=$($global:L.Count)" }
$n = Join-Path $d 'nest.ps1'
Set-Content -LiteralPath $n -Encoding ascii -Value @'
param([int]$N, [int]$Max)
$ErrorActionPreference = 'Stop'
try {
    do {
        if ($N -ge 1) {
            if ($N -ge $Max) { "REACHED $N"; exit 0 }
            & $PSCommandPath -N ($N + 1) -Max $Max
            exit $LASTEXITCODE
        }
    } while ($false)
} catch {
    "STOPPED at depth $N : $($_.Exception.GetType().FullName): $($_.Exception.Message)"
    exit 3
} finally { }
'@
foreach ($max in 200, 1000) {
    $r = & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $n -N 1 -Max $max
    $code = $LASTEXITCODE
    $tag = if ($max -eq 200) { 'P3' } else { 'P3-info' }
    if ($code -eq 0 -and ($r -join ' ') -match "REACHED $max") { "$tag PASS: $max nested re-invocations" } else { "$tag FAIL (exit $code): $($r -join ' | ')" }
}
PS
cat > "$D/lease.ps1" <<'PS'
$ErrorActionPreference = 'Stop'
$sot = Join-Path $env:LOCALAPPDATA 'sot'
$held = Join-Path $sot 'held.json'
if (-not (Test-Path -LiteralPath $held)) { 'P5 SKIP: no held.json, so no window holds a lease - open the window and rerun'; exit 0 }
$h = Get-Content -Raw -LiteralPath $held | ConvertFrom-Json
if (@($h.holders).Count -lt 1 -or $h.closing -eq $true -or $null -ne $h.handover_until_ms) { "P5 SKIP: held.json is not 'a window holds a lease, nothing pending': $((Get-Content -Raw -LiteralPath $held).Trim())"; exit 0 }
$pipe = "$(& (Join-Path $sot 'bin\sotd.exe') session-socket-path local | Select-Object -First 1)".Trim()
try {
    $v = (Get-ItemProperty -Path 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Memory Management\PrefetchParameters' -Name BootId -ErrorAction Stop).BootId
    $boot = [string][BitConverter]::ToUInt32([BitConverter]::GetBytes([int32]$v), 0)
} catch { $boot = '' }
$created = [System.Diagnostics.Process]::GetCurrentProcess().StartTime.ToFileTimeUtc()
$enc = New-Object System.Text.UTF8Encoding($false)
$client = New-Object System.IO.Pipes.NamedPipeClientStream('.', ($pipe -replace '^\\\\\.\\pipe\\', ''), [System.IO.Pipes.PipeDirection]::InOut)
try {
    $client.Connect(5000)
    $b = $enc.GetBytes('{"v":2,"id":1,"kind":"req","op":"fe.lease","payload":{"boot":"' + $boot + '","created":' + $created + ',"pid":' + $PID + '}}' + "`n")
    $client.Write($b, 0, $b.Length); $client.Flush()
    $reader = New-Object System.IO.StreamReader($client, $enc, $false, 1024, $true)
    $t = $reader.ReadLineAsync()
    if (-not $t.Wait(5000)) { 'P5 FAIL: no reply to fe.lease within 5 s'; exit 1 }
    $outcome = (ConvertFrom-Json $t.Result).payload.outcome
    if ($outcome -eq 'granted') {
        $b = $enc.GetBytes('{"v":2,"id":2,"kind":"req","op":"fe.leaving","payload":{"intent":"handover"}}' + "`n")
        $client.Write($b, 0, $b.Length); $client.Flush()
        $t2 = $reader.ReadLineAsync(); [void]$t2.Wait(5000)
        "P5 PASS: granted to pid $PID (created $created, boot '$boot'); leaving reply: $($t2.Result)"
    } else { "P5 FAIL: outcome '$outcome' - reply $($t.Result)" }
} finally { $client.Dispose() }
PS
cat > "$D/w1pre.ps1" <<'PS'
$sot = Join-Path $env:LOCALAPPDATA 'sot'
$P = "$(Get-Content -LiteralPath (Join-Path $sot 'logs\launcher.pid') -ErrorAction SilentlyContinue | Select-Object -First 1)".Trim()
$sup = if ($P) { Get-CimInstance Win32_Process -Filter "ProcessId = $P" -ErrorAction SilentlyContinue } else { $null }
"supervisor pid=$P started=$(if ($sup) { $sup.CreationDate.ToString('o') } else { 'NOT RUNNING' })"
$inst = Get-Item -LiteralPath (Join-Path $sot 'install.json') -ErrorAction SilentlyContinue
"install.json written=$(if ($inst) { $inst.LastWriteTime.ToString('o') } else { 'absent' }) version=$(if ($inst) { (Get-Content -Raw -LiteralPath $inst.FullName | ConvertFrom-Json).version })"
$line = Get-Content -LiteralPath (Join-Path $sot 'logs\supervisor.log') | Where-Object { $_ -like "*pid=$P  supervisor start *" } | Select-Object -Last 1
"start line: $line"
$dir = Join-Path $sot 'repo\current\scripts'
$want = (@('launch-sot.ps1', 'sot-hosts.ps1', 'sot-install-layout.ps1') | ForEach-Object { (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $dir $_)).Hash }) -join '-'
"installed code id: $want"
$have = if ($line -match ' code=([0-9A-F-]+)$') { $Matches[1] } else { '' }
if ($have -and $have -ceq $want) { 'W1-pre PASS' } elseif ($have) { "W1-pre FAIL: the supervisor runs $have" } else { 'W1-pre FAIL: the supervisor start line has no code id (the supervisor predates the record)' }
if ($sup -and $inst -and $sup.CreationDate -lt $inst.LastWriteTime) { 'NOTE: the supervisor process predates the current install - do not converge this box before Ctrl+Q, Tab, Enter and a Start-menu start' }
PS
for s in premises lease w1pre; do
  echo "== $s"; powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$(cygpath -w "$D/$s.ps1")" > "$D/$s.out" 2>&1; echo "exit=$?"; cat "$D/$s.out"
done
d="$(cygpath -u "$LOCALAPPDATA")/sot/repo/current/scripts"
echo "P9 git-bash id: $(for f in launch-sot.ps1 sot-hosts.ps1 sot-install-layout.ps1; do sha256sum "$d/$f" | cut -c1-64; done | tr 'a-f' 'A-F' | paste -sd- -)"
echo "P9 PASS iff that line equals the 'installed code id' line of w1pre above"

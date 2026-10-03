#!/usr/bin/env bash
# Windows supervisor premise P3m (git-bash on a Windows box): each nested in-process re-invoke of a distinct,
# launcher-sized script keeps bounded memory. PASS iff both lines show exit=0 and REACHED, and on the max=60 line
# wsN minus ws1 is at most 300 (MB); the max=200 growth is recorded. Read-only apart from its own temp folder.
D="$(cygpath -u "$LOCALAPPDATA")/Temp/sot-f4"; mkdir -p "$D"
cat > "$D/nestm.template.ps1" <<'PS'
param([int]$N, [int]$Max)
$ErrorActionPreference = 'Stop'
function Get-WsMB {
    [GC]::Collect(); [GC]::WaitForPendingFinalizers(); [GC]::Collect()
    [int]([System.Diagnostics.Process]::GetCurrentProcess().WorkingSet64 / 1MB)
}
try {
    do {
        if ($N -ge 1) {
            if ($N -eq 1) { $env:SOT_F4_WS1 = "$(Get-WsMB)" }
            if ($N -ge $Max) { "REACHED $N ws1=$($env:SOT_F4_WS1) wsN=$(Get-WsMB)"; exit 0 }
            $t = [System.IO.File]::ReadAllText($PSCommandPath)
            [System.IO.File]::WriteAllText($PSCommandPath, ($t -replace '# copy \d+', "# copy $($N + 1)"))
            & $PSCommandPath -N ($N + 1) -Max $Max
            exit $LASTEXITCODE
        }
    } while ($false)
} catch {
    "STOPPED at depth $N : $($_.Exception.GetType().FullName): $($_.Exception.Message)"
    exit 3
}
# copy 1
PS
{ echo 'function Get-F4Pad {'; cat "$(cygpath -u "$LOCALAPPDATA")/sot/repo/current/scripts/launch-sot.ps1"; echo '}'; } >> "$D/nestm.template.ps1"
for max in 60 200; do
  cp "$D/nestm.template.ps1" "$D/nestm.ps1"
  powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$(cygpath -w "$D/nestm.ps1")" -N 1 -Max $max > "$D/p3m-$max.out" 2>&1
  echo "P3m max=$max exit=$?: $(tr -d '\r' < "$D/p3m-$max.out")"
done
echo "P3m PASS iff both lines show exit=0 and REACHED, and on the max=60 line wsN minus ws1 is at most 300 (MB); the max=200 growth is recorded"

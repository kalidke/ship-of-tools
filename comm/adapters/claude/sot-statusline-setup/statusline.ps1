# Claude Code statusLine (Windows) - 2 lines, single PowerShell pass.
$ErrorActionPreference = 'SilentlyContinue'
$esc = [char]27
function Col([string]$c,[string]$t){ "$esc[${c}m$t$esc[0m" }
function Fmt([int64]$n){ if($n -ge 1000){ "$([math]::Floor($n/1000))k" } else { "$n" } }
$j = $null; try { $j = [Console]::In.ReadToEnd() | ConvertFrom-Json } catch {}
$model = if ($j.model.display_name) { [string]$j.model.display_name } else { 'model?' }
$sid = [string]$j.session_id; $sess = if ($sid.Length -ge 8) { $sid.Substring(0,8) } else { $sid }
$effort = $j.effort.level
$think = if ($j.thinking.enabled -ne $true) { 'off' } elseif ($effort) { [string]$effort } else { 'on' }
$thinkSeg = if ($think -eq 'off') { Col '90' "think:$think" } else { Col '35' "think:$think" }
$ver = if ($j.version) { Col '33' "v$($j.version)" } else { '' }
$cur = if ($j.workspace.current_dir) { [string]$j.workspace.current_dir } else { '.' }
Push-Location $cur 2>$null
$branch = git branch --show-current 2>$null
if ($branch) { $repo = "$(Split-Path $cur -Leaf):$branch"; $unc = @(git status --porcelain 2>$null).Count }
else { $repo = 'no-git'; $unc = 0 }
Pop-Location 2>$null
$uncCol = if ($unc -eq 0) { '32' } else { '31' }
$l1 = @((Col '34' $model) + ' ' + (Col '90' "[$sess]"), $thinkSeg)
if ($ver) { $l1 += $ver }
$l1 += (Col '38;5;208' $repo); $l1 += (Col $uncCol "$unc uncommitted")
$inT = [int64]$j.context_window.total_input_tokens; $outT = [int64]$j.context_window.total_output_tokens
$cost = [double]$j.cost.total_cost_usd
$costCol = if ($cost -gt 0.50) { '31' } elseif ($cost -gt 0.10) { '33' } else { '32' }
$l2 = (Col '36' "Session: $(Fmt ($inT+$outT)) (in:$(Fmt $inT) out:$(Fmt $outT))") + ' | ' + (Col $costCol ('${0:N2}' -f $cost))
[Console]::Out.Write([string]::Join(' | ', $l1) + "`n" + $l2)

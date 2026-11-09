#!/usr/bin/env bash
# Claude Code statusLine (Linux/macOS) - mirrors the Windows statusline.ps1.
j="$(cat)"; e=$'\033'
col(){ printf '%s[%sm%s%s[0m' "$e" "$1" "$2" "$e"; }
get(){ printf '%s' "$j" | jq -r "$1 // empty" 2>/dev/null; }
fmt(){ local n=${1:-0}; if [ "$n" -ge 1000 ] 2>/dev/null; then echo "$((n/1000))k"; else echo "$n"; fi; }
model="$(get .model.display_name)"; [ -z "$model" ] && model='model?'
sid="$(get .session_id)"; sess="${sid:0:8}"; ver="$(get .version)"
# Which Claude account this session runs under - the config dir's basename
# (one per Claude account). Sessions for different accounts can share one home
# and nothing else on screen tells them apart. The transcript path carries the
# same name, so it covers a launcher that does not export the variable.
acct="$(basename "${CLAUDE_CONFIG_DIR:-}" 2>/dev/null)"; case "$acct" in ""|.|/) acct="" ;; esac
tp="$(get .transcript_path)"
[ -z "$acct" ] && [ -n "$tp" ] && acct="$(printf '%s' "$tp" | sed -n 's#.*/\.claude-auth/\([^/]*\)/.*#\1#p')"
eff="$(get .effort.level)"; th="$(get .thinking.enabled)"
if [ "$th" != "true" ]; then think=off; tc=90; elif [ -n "$eff" ]; then think="$eff"; tc=35; else think=on; tc=35; fi
cur="$(get .workspace.current_dir)"; [ -z "$cur" ] && cur=.
# A detached HEAD shows the short commit. --no-optional-locks: the count never takes the index lock.
if [ "$(git -C "$cur" rev-parse --is-inside-work-tree 2>/dev/null)" = true ]; then
  br="$(git -C "$cur" branch --show-current 2>/dev/null)"; [ -n "$br" ] || br="$(git -C "$cur" rev-parse --short HEAD 2>/dev/null)"
  repo="$(basename "$cur"):$br"; unc="$(git --no-optional-locks -C "$cur" status --porcelain 2>/dev/null | wc -l | tr -d ' ')"
else repo=no-git; unc=0; fi
[ "${unc:-0}" -eq 0 ] && uc=32 || uc=31
inT="$(get .context_window.total_input_tokens)"; outT="$(get .context_window.total_output_tokens)"
cost="$(get .cost.total_cost_usd)"; cost=${cost:-0}
cc=32; awk "BEGIN{exit !($cost>0.10)}" && cc=33; awk "BEGIN{exit !($cost>0.50)}" && cc=31
l1="$(col 34 "$model") $(col 90 "[$sess]")"
[ -n "$acct" ] && l1="$l1 $(col 36 "$acct")"
l1="$l1 | $(col "$tc" "think:$think")"
[ -n "$ver" ] && l1="$l1 | $(col 33 "v$ver")"
l1="$l1 | $(col '38;5;208' "$repo")"
# The lab server limits each user's use of /home and writes its figures to a file; warn from 90% of the limit, red from
# 95%. Silent when the file, the uid's row or a number is missing or bad (one small read, never a failure).
hm=""; hf="${SOT_HOME_USAGE_FILE:-/home/.home-usage.tsv}"
if [ -r "$hf" ]; then
  hp="$(awk -F'\t' -v u="$(id -u)" 'NR>1 && $1==u && NF==3 && $2 ~ /^[0-9]+$/ && $3 ~ /^[0-9]+$/ && $3+0>0 { printf "%d", $2*100/$3; exit }' "$hf" 2>/dev/null)"
  case "$hp" in ""|*[!0-9]*) ;; *)
    if [ "$hp" -ge 95 ]; then hm=" | $(col 31 "home $hp%")"; elif [ "$hp" -ge 90 ]; then hm=" | home $hp%"; fi ;;
  esac
fi
# The count sits on its own line so a narrow window never cuts it off the end of line 1.
printf '%s\n%s%s\n%s | %s\n' "$l1" "$(col "$uc" "$unc uncommitted")" "$hm" \
  "$(col 36 "Session: $(fmt $(( ${inT:-0} + ${outT:-0} ))) (in:$(fmt ${inT:-0}) out:$(fmt ${outT:-0}))")" \
  "$(col "$cc" "$(printf '$%.2f' "$cost")")"

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
br="$(git -C "$cur" branch --show-current 2>/dev/null)"
if [ -n "$br" ]; then repo="$(basename "$cur"):$br"; unc="$(git -C "$cur" status --porcelain 2>/dev/null | wc -l | tr -d ' ')"; else repo=no-git; unc=0; fi
[ "${unc:-0}" -eq 0 ] && uc=32 || uc=31
inT="$(get .context_window.total_input_tokens)"; outT="$(get .context_window.total_output_tokens)"
cost="$(get .cost.total_cost_usd)"; cost=${cost:-0}
cc=32; awk "BEGIN{exit !($cost>0.10)}" && cc=33; awk "BEGIN{exit !($cost>0.50)}" && cc=31
l1="$(col 34 "$model") $(col 90 "[$sess]")"
[ -n "$acct" ] && l1="$l1 $(col 36 "$acct")"
l1="$l1 | $(col "$tc" "think:$think")"
[ -n "$ver" ] && l1="$l1 | $(col 33 "v$ver")"
l1="$l1 | $(col '38;5;208' "$repo") | $(col "$uc" "$unc uncommitted")"
printf '%s\n%s | %s\n' "$l1" \
  "$(col 36 "Session: $(fmt $(( ${inT:-0} + ${outT:-0} ))) (in:$(fmt ${inT:-0}) out:$(fmt ${outT:-0}))")" \
  "$(col "$cc" "$(printf '$%.2f' "$cost")")"

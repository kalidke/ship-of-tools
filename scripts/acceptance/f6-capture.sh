#!/usr/bin/env bash
# Wake premise capture: what Claude Code draws in a row with agent view OFF while a background sub-agent runs,
# at rest and after focus keys. Run from the repo root of a joined row: bash <checkout>/scripts/acceptance/f6-capture.sh <outdir>
# Spawns one throwaway row and despawns it on exit.
set -u
OUT="${1:?usage: f6-capture.sh <outdir>}"; mkdir -p "$OUT"
BIN="$HOME/.sot-comm/bin"; SPAWN_BIN="${DT_SPAWN_BIN:-$BIN}"
ME="$("$BIN/comm-context.sh" 2>/dev/null | sed -n 's/^NAME=//p' | head -n1)"
TAG="f6cap$(date +%s | tail -c 6)"
DIR="$PWD/dev/output/done-test-rows/$TAG"; mkdir -p "$DIR/.claude"
printf '{\n  "disableAgentView": true\n}\n' > "$DIR/.claude/settings.local.json"
WS=""
trap '[ -n "$WS" ] && "$SPAWN_BIN/comm-despawn.sh" "$WS" >> "$OUT/run.log" 2>&1' EXIT
TASK='Start one background sub-agent (Agent tool, run_in_background) that runs `timeout 300 tail -f /dev/null` in Bash, then end your turn. Do nothing else.'
out="$("$SPAWN_BIN/comm-spawn.sh" "$DIR" --name "$TAG" --task "$TASK" 2>&1)"; echo "$out" >> "$OUT/run.log"
WS="$(printf '%s\n' "$out" | grep -o 'id=ws-[A-Za-z0-9_.-]*' | head -n1 | cut -d= -f2)"
[ -n "$WS" ] || { echo "no ws id" >> "$OUT/run.log"; exit 1; }
shot() { { printf -- '--- %s %s\n' "$(date -u +%H:%M:%S)" "$1"; "$BIN/sot-fe" screen "$WS" --timeout 3 2>&1; } > "$OUT/$1.txt"; }
key() { printf '%b' "$1" | "$BIN/sot-fe" type "$WS" --stdin --origin "${ME:-capture}" >> "$OUT/run.log" 2>&1; }
# wait (max 240 s) until the panel shows a running sub-agent and no spinner
t=0
while [ $t -lt 240 ]; do
    s="$("$BIN/sot-fe" screen "$WS" --timeout 3 2>/dev/null)"
    if printf '%s\n' "$s" | grep -q $'◯' && ! printf '%s\n' "$s" | grep -qE '…[[:space:]]*\([0-9]+(m [0-9]+)?s'; then break; fi
    sleep 5; t=$((t + 5))
done
echo "ready after ${t}s" >> "$OUT/run.log"
sleep 3
shot 1-rest
key '\e[B'; sleep 2; shot 2-down1
key '\e[B'; sleep 2; shot 3-down2
key '\e';   sleep 2; shot 4-esc
key '\e';   sleep 2; shot 5-esc2
echo done >> "$OUT/run.log"

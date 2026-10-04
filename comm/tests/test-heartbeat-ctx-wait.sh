#!/usr/bin/env bash
# test-heartbeat-ctx-wait.sh — hermetic suite for the heartbeat hook's
# comm-context.sh wait: it polls every 50 ms (not once a second), bounds the
# wait, kills a stalled child, removes its temp file, and keeps a finished
# child's output. A stub comm-context.sh sits in a scratch copy of the hooks dir.
#
# Usage: comm/tests/test-heartbeat-ctx-wait.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/../work_state/hooks" && pwd)"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-hbctx-test-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "mktemp failed" >&2; exit 1; }
STUB_PID=""
cleanup() {
    [ -n "$STUB_PID" ] && { kill "$STUB_PID" 2>/dev/null; wait "$STUB_PID" 2>/dev/null; }
    rm -rf "${WORK:?}"
}
trap cleanup EXIT

export SOT_COMM_HOME="$WORK/home"
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
SCRIPTS_DIR="$(guard_stage_bin "$WORK")" || exit 2
unset SOT_COMM_NAME SOT_COMM_SELF_FILE CLAUDE_CODE_SESSION_ID
mkdir -p "$SOT_COMM_HOME/state"
ln -s "$SCRIPTS_DIR" "$SOT_COMM_HOME/bin"
REGISTRY="$SOT_COMM_HOME/registry.json"
NAME="hbctx-test"

FLAT="$WORK/flat"; mkdir -p "$FLAT"
cp "$HOOKS_DIR/comm-status-heartbeat.sh" "$FLAT/"
ln -s "$SCRIPTS_DIR"/comm-lib*.sh "$FLAT/"
STUB="$FLAT/comm-context.sh"
HB_OUT="$WORK/hb.out"

pass=0; fail=0
ok()  { echo "PASS $1"; pass=$((pass+1)); }
bad() { echo "FAIL $1${2:+ — $2}"; fail=$((fail+1)); }

seed_row() {
    jq -n --arg n "$NAME" '{agents:{($n):{state:"working",floor:"working",summary:"x",status_at:"2026-09-08T00:00:00Z",repo:"x"}}}' > "$REGISTRY"
}
# HB: one heartbeat call past the 10 s throttle; prints elapsed ms.
HB() {
    local t0 t1
    rm -f "${SOT_COMM_HOME:?}"/state/hb-*.tick 2>/dev/null
    t0=$EPOCHREALTIME
    printf '{"tool_name":"Bash"}' | bash "$FLAT/comm-status-heartbeat.sh" >"$HB_OUT" 2>&1
    t1=$EPOCHREALTIME
    # Digits only, as comm-lib.sh reads its own clock: the decimal mark
    # follows the locale, so the raw string is never used in arithmetic.
    echo $(( (${t1//[!0-9]/} - ${t0//[!0-9]/}) / 1000 ))
}

# (a) fast context call: no whole-second dead time.
printf '#!/usr/bin/env bash\necho "NAME=%s"\n' "$NAME" > "$STUB"; chmod +x "$STUB"
seed_row
slow=0; times=""
for _ in 1 2 3 4 5; do
    ms="$(HB)"; times="$times $ms"
    [ "$ms" -lt 500 ] || slow=$((slow+1))
done
[ "$slow" = 0 ] && ok "(a) fast context: 5 runs under 500 ms (ms:$times)" || bad "(a) fast context" "ms:$times"

# (b) bound: a stalled context call is killed at the tick bound, no temp left.
cat > "$STUB" <<EOS
#!/usr/bin/env bash
echo \$\$ > "$WORK/stub.pid"
exec sleep 30
EOS
chmod +x "$STUB"
seed_row
ms="$(SOT_HB_CTX_TIMEOUT_TICKS=20 HB)"
STUB_PID="$(cat "$WORK/stub.pid" 2>/dev/null)"
gone=1; kill -0 "$STUB_PID" 2>/dev/null && gone=0
left="$(find "$SOT_COMM_HOME/state" -name '.hb-ctx-*' | wc -l)"
if [ "$ms" -lt 3000 ] && [ "$gone" = 1 ] && [ "$left" = 0 ]; then ok "(b) bound: returned in ${ms} ms, stub gone, no temp file"
else bad "(b) bound" "ms=$ms gone=$gone temp_left=$left"; fi
[ "$gone" = 0 ] && { kill "$STUB_PID" 2>/dev/null; wait "$STUB_PID" 2>/dev/null; }
STUB_PID=""

# (c) a context call that takes 0.2 s still has its output used.
printf '#!/usr/bin/env bash\nsleep 0.2\necho "NAME=%s"\n' "$NAME" > "$STUB"; chmod +x "$STUB"
seed_row
HB >/dev/null
at="$(jq -r --arg n "$NAME" '.agents[$n].status_at' "$REGISTRY")"
[ "$at" != "2026-09-08T00:00:00Z" ] && ok "(c) output kept: row stamped ($at)" || bad "(c) output kept" "status_at unchanged"

echo "$pass passed, $fail failed"
[ "$fail" = 0 ]

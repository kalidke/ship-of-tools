#!/usr/bin/env bash
# test-heartbeat-ctx-wait.sh — hermetic suite for the heartbeat hook's
# comm-context.sh wait: it polls every 50 ms (not once a second), bounds the
# wait, kills a stalled child, removes its temp file, and keeps a finished
# child's output. A stub comm-context.sh sits in a scratch copy of the hooks dir.
# The waits are counted, never timed: a logging `sleep` first on the hook's PATH
# records each wait the hook makes, so the verdict does not depend on host speed.
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
# The logging sleep: appends its arguments to SLEEPS, then runs the real sleep (its absolute path, resolved before the
# shim exists). A stub that must itself sleep calls $REAL_SLEEP, so the log holds only the hook's own waits.
REAL_SLEEP="$(command -v sleep)"
SHIM="$WORK/shim"; SLEEPS="$WORK/sleeps.log"; mkdir -p "$SHIM"
printf '#!/usr/bin/env bash\necho "$*" >> "%s"\nexec "%s" "$@"\n' "$SLEEPS" "$REAL_SLEEP" > "$SHIM/sleep"
chmod +x "$SHIM/sleep"
# HB: one heartbeat call past the 10 s throttle, with the logging sleep first on its PATH; SLEEPS holds its waits.
HB() {
    rm -f "${SOT_COMM_HOME:?}"/state/hb-*.tick 2>/dev/null
    : > "$SLEEPS"
    printf '{"tool_name":"Bash"}' | PATH="$SHIM:$PATH" bash "$FLAT/comm-status-heartbeat.sh" >"$HB_OUT" 2>&1
}

# (a) fast context call: every wait the hook makes is a 50 ms poll, never a whole second. An empty log passes too (the
# stub may end before the first poll); (b) shows the log sees the polls.
printf '#!/usr/bin/env bash\n"%s" 0.2\necho "NAME=%s"\n' "$REAL_SLEEP" "$NAME" > "$STUB"; chmod +x "$STUB"
seed_row
HB
other="$(grep -c -v -x '0.05' "$SLEEPS")"
[ "$other" = 0 ] && ok "(a) fast context: every wait is 0.05 ($(wc -l < "$SLEEPS") polls)" || bad "(a) fast context" "waits: $(tr '\n' ' ' < "$SLEEPS")"

# (b) bound: a stalled context call is killed at the tick bound, no temp left.
cat > "$STUB" <<EOS
#!/usr/bin/env bash
echo \$\$ > "$WORK/stub.pid"
exec "$REAL_SLEEP" 300
EOS
chmod +x "$STUB"
seed_row
SOT_HB_CTX_TIMEOUT_TICKS=20 HB
polls="$(grep -c -x '0.05' "$SLEEPS")"; total="$(wc -l < "$SLEEPS")"
STUB_PID="$(cat "$WORK/stub.pid" 2>/dev/null)"
gone=1; kill -0 "$STUB_PID" 2>/dev/null && gone=0
left="$(find "$SOT_COMM_HOME/state" -name '.hb-ctx-*' | wc -l)"
if [ "$polls" = 20 ] && [ "$total" = 20 ] && [ "$gone" = 1 ] && [ "$left" = 0 ]; then ok "(b) bound: 20 polls of 0.05, stub gone, no temp file"
else bad "(b) bound" "polls=$polls total=$total gone=$gone temp_left=$left"; fi
[ "$gone" = 0 ] && { kill "$STUB_PID" 2>/dev/null; wait "$STUB_PID" 2>/dev/null; }
STUB_PID=""

# (c) a context call that takes 0.2 s still has its output used.
printf '#!/usr/bin/env bash\n"%s" 0.2\necho "NAME=%s"\n' "$REAL_SLEEP" "$NAME" > "$STUB"; chmod +x "$STUB"
seed_row
HB
at="$(jq -r --arg n "$NAME" '.agents[$n].status_at' "$REGISTRY")"
[ "$at" != "2026-09-08T00:00:00Z" ] && ok "(c) output kept: row stamped ($at)" || bad "(c) output kept" "status_at unchanged"

echo "$pass passed, $fail failed"
[ "$fail" = 0 ]

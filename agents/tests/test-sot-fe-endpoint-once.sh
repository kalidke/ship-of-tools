#!/usr/bin/env bash
# test-sot-fe-endpoint-once.sh — `sot-fe` finds the daemon once per run, whichever verb: the dispatcher resolves the
# endpoint and every sender uses that answer, so `docs` and `repl run` (which need it before they send, for the path's
# spelling) ask the box's `sotd` for its socket one time, as `preview` does. The count is the stand-in `sotd`'s own: each
# `session-socket-path` it is asked. Nothing answers on the socket (each run is held to a 1 s bound); no real daemon.
#
# Usage: agents/tests/test-sot-fe-endpoint-once.sh     Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/../../comm/tests/lib-home-guard.sh" || exit 2   # never the live comm home

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-fe-endpoint-once-test-XXXXXX")"
if [ -z "$WORK" ] || [ ! -d "$WORK" ]; then
    echo "FATAL: mktemp did not produce a usable work directory (got: '$WORK')" >&2
    exit 1
fi
WORK="$(cd "$WORK" && pwd -P)" || exit 1
SOCK_PID=""
trap '[ -z "$SOCK_PID" ] || kill "$SOCK_PID" 2>/dev/null; rm -rf "${WORK:?}"' EXIT

export SOT_COMM_HOME="$WORK/home"
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
SCRIPTS_DIR="$(guard_stage_bin "$WORK")" || exit 2
BRIDGE="$(guard_bridge_stub "$WORK/bridge")"
[ -x "$BRIDGE" ] || exit 2
mkdir -p "$SOT_COMM_HOME"

# A socket nobody answers on, and a sotd that names it and counts being asked.
SOCK="$WORK/daemon.sock"
python3 -c '
import signal, socket, sys
signal.alarm(300)
s = socket.socket(socket.AF_UNIX)
s.bind(sys.argv[1])
s.listen(8)
while True:
    s.accept()[0].close()
' "$SOCK" &
SOCK_PID=$!
for _ in $(seq 1 50); do [ -S "$SOCK" ] && break; sleep 0.1; done
[ -S "$SOCK" ] || { echo "FATAL: the socket did not bind" >&2; exit 2; }
ASKED="$WORK/asked"
cat > "$WORK/sotd" <<EOF
#!/usr/bin/env bash
case "\${1:-}" in
    session-socket-path) echo x >> "$ASKED"; echo "$SOCK"; exit 0 ;;
    stdio-bridge) exec "$BRIDGE" "\$@" ;;
esac
exit 97
EOF
chmod +x "$WORK/sotd"
export SOTD_BIN="$WORK/sotd"

PASS=0
FAIL=0
check() {
    local desc="$1" fn="$2"
    if "$fn"; then echo "PASS: $desc"; PASS=$((PASS + 1)); else echo "FAIL: $desc"; FAIL=$((FAIL + 1)); fi
}

# asked_by ARG... — how many times one `sot-fe ARG...` run asked `sotd` for the daemon's socket.
asked_by() {
    : > "$ASKED"
    ( cd "$WORK" && bash "$SCRIPTS_DIR/sot-fe" "$@" --timeout 1 > /dev/null 2>&1 )
    wc -l < "$ASKED" | tr -d ' '
}
case_each_verb_asks_once() {
    local verb n bad=0
    for verb in "docs ws $WORK/a.html" "repl run ws $WORK/a.jl" "repl run ws $WORK/a.jl --fresh" "repl eval ws --code 1" \
                "repl status ws" "repl interrupt ws" "goto ws" "type ws x" "screen ws"; do
        # shellcheck disable=SC2086
        n="$(asked_by $verb)"
        [ "$n" = 1 ] || { echo "  'sot-fe $verb' asked $n times"; bad=1; }
    done
    return "$bad"
}
check "every verb that reaches the daemon asks sotd for its socket once" case_each_verb_asks_once

echo ""
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]

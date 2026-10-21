#!/usr/bin/env bash
# shapes.sh — the three shapes mail takes in use, on this tree's sotd in test homes, each sending and reading its
# reply: (a) a row; (b) a worktree session outside any row, whose handle comes from its folder (as a captain's does);
# (c) a server session with no daemon, on a second host that shares the comm folder, mailing a row on the daemon's
# host. Each sends `PING <nonce>` to an echo row and reads `PONG <nonce>` back; the echo row is a bash row whose session
# reads its mail and replies through the same commands, once per PING. A shape that loses its handle or its mail fails.
#
# Usage, run by hand on two build hosts that mount one network folder:
#   comm/tests/shapes.sh daemon WORK SHARED SOTD     the daemon's host: starts a test daemon (state in WORK, local),
#                                                    the echo row, shapes (a) and (b); then answers the server's
#                                                    shape, ends its rows and its daemon
#   comm/tests/shapes.sh server SHARED SOTD          the second host: shape (c), once SHARED/daemon-ready exists
# WORK must not exist or be empty; SHARED must be empty or new, on the network folder, outside any git checkout (or
# the checkout is every session's project). SOTD is each host's own build of this tree, with sot-capsule beside it.
# The daemon's comm folder is SHARED/comm; nothing touches a live comm folder, daemon or row: every session runs under
# `env -i` with its own home, and the only processes this ends are its daemon and the rows it made.
set -u

fail() { echo "FAIL: $*"; [ -z "${SHARED:-}" ] || : > "$SHARED/failed"; exit 1; }
pass() { echo "ok   $*"; }
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)" || exit 2
. "$ROOT/comm/tests/lib-wait.sh" || exit 2

# long_await ROUNDS CMD... — await CMD for up to ROUNDS of await's 30 s: the other host's side starts by hand.
long_await() {
    local n="$1" i
    shift
    for i in $(seq "$n"); do await "$@" && return 0; done
    return 1
}

# sess HOST HOME DIR SCRIPT ARGS... — a session outside any row on box HOST, working in DIR, with its own HOME.
sess() {
    local host="$1" home="$2" dir="$3" script="$4"
    shift 4
    (cd "$dir" && env -i PATH=/usr/local/bin:/usr/bin:/bin HOME="$home" SOT_COMM_HOME="$SHARED/comm" \
        SOT_SELF_HOST="$host" SOT_COMM_TEST_HOST="$host" SOT_SOCKET="${SESS_SOCKET:-$home/no-daemon.sock}" \
        SOT_HOSTS="$home/no-hosts.toml" XDG_RUNTIME_DIR="$home" SOTD_BIN="$SOTD" bash "$SHARED/bin/$script" "$@")
}

# handle_of HOST HOME DIR — the handle comm-context names for that session.
handle_of() {
    local ctx
    ctx="$(sess "$1" "$2" "$3" comm-context.sh 2>/dev/null)" || return 1
    ( eval "$ctx"; printf '%s' "${NAME:-}" )
}

# filed_ping HOST HOME DIR NONCE — the session sends PING NONCE to the echo row, and it is filed.
filed_ping() {
    local out
    out="$(sess "$1" "$2" "$3" comm-send.sh "@$ECHO" "PING $4" 2>&1)"
    case "$out" in *"filed -> @$ECHO"*) ;; *) echo "  the PING was not filed: $out"; return 1 ;; esac
}

# read_pong HOST HOME DIR NONCE — the session's poll shows PONG NONCE.
read_pong() {
    sess "$1" "$2" "$3" comm-poll.sh > "$2/poll.$4" 2>&1
    grep -q "] PONG $4$" "$2/poll.$4" || { echo "  no PONG $4 read: $(tail -3 "$2/poll.$4")"; return 1; }
}

ECHO="echo-dhost"

if [ "${1:-}" = server ]; then
    [ $# -eq 3 ] || { echo "usage: shapes.sh server SHARED SOTD" >&2; exit 2; }
    SHARED="$2" SOTD="$3"
    long_await 10 test -e "$SHARED/daemon-ready" || fail "the daemon's host never got ready"
    [ ! -e "$SHARED/failed" ] || fail "the daemon's host failed"
    home="$(mktemp -d)" && chmod 700 "$home" || exit 1
    mkdir -p "$SHARED/server-proj" || exit 1
    out="$(sess shost "$home" "$SHARED/server-proj" comm-join.sh 2>&1)"
    h="$(handle_of shost "$home" "$SHARED/server-proj")"
    [ "$h" = server-proj-shost ] || fail "(c) the server session's handle is '$h', not server-proj-shost: $out"
    pass "(c) the server session joined as @$h with no daemon on its host"
    filed_ping shost "$home" "$SHARED/server-proj" c1 || fail "(c) the server session's PING"
    : > "$SHARED/c.sent"
    replied() { [ -e "$SHARED/c.replied" ] || [ -e "$SHARED/failed" ]; }
    long_await 4 replied && [ ! -e "$SHARED/failed" ] || fail "(c) the echo row never answered"
    read_pong shost "$home" "$SHARED/server-proj" c1 || fail "(c) the server session's reply"
    pass "(c) the server session mailed the row on the daemon's host and read its reply"
    : > "$SHARED/server-done"
    rm -rf "${home:?}"
    exit 0
fi

[ "${1:-}" = daemon ] && [ $# -eq 4 ] || { echo "usage: shapes.sh daemon WORK SHARED SOTD | server SHARED SOTD" >&2; exit 2; }
W="$2" SHARED="$3" SOTD="$4"
for d in "$W" "$SHARED"; do
    if [ -e "$d" ]; then [ -z "$(ls -A "$d")" ] || { echo "not empty: $d" >&2; exit 2; }; fi
    mkdir -p "$d" && chmod 700 "$d" || exit 2
done
W="$(cd "$W" && pwd -P)" SHARED="$(cd "$SHARED" && pwd -P)"
(cd "$SHARED" && ! git rev-parse --show-toplevel >/dev/null 2>&1) || { echo "SHARED is inside a git checkout" >&2; exit 2; }
bash "$ROOT/comm/tests/stage-bin.sh" "$SHARED/bin" >/dev/null || fail "stage this tree's scripts"
mkdir -p "$W/home" "$W/run" "$W/state" "$W/config" "$W/project" "$SHARED/comm" && chmod 700 "$W/run" || exit 1

DAEMON="" ROWS=""
req() {  # OP PAYLOAD — one request to the test daemon; prints the reply
    env -i PATH=/usr/local/bin:/usr/bin:/bin HOME="$W/home" SOT_COMM_HOME="$SHARED/comm" SOTD_BIN="$SOTD" \
        SOT_SELF_HOST=dhost XDG_RUNTIME_DIR="$W/run" bash -c '
        . "$1/comm-lib.sh" || exit 2
        ENDPOINT="unix:$2"
        frame="$(jq -nc --arg op "$3" --argjson p "$4" "{v:1,id:1,kind:\"req\",op:\$op,payload:\$p}")"
        sot_oneshot_request "$frame" "$3"' _ "$SHARED/bin" "$W/run/sotd.sock" "$1" "$2"
}
end_all() {
    local r
    for r in $ROWS; do req workspace.destroy "$(jq -nc --arg w "$r" '{workspace_id:$w}')" >/dev/null 2>&1; done
    [ -z "$DAEMON" ] || { kill "$DAEMON" 2>/dev/null; wait "$DAEMON" 2>/dev/null; }
}
trap end_all EXIT

env -i PATH=/usr/local/bin:/usr/bin:/bin HOME="$W/home" USERPROFILE="$W/home" LOCALAPPDATA="$W/state" \
    XDG_STATE_HOME="$W/state" XDG_CONFIG_HOME="$W/config" SOT_SELF_HOST=dhost SOT_RUNTIME_DIR="$W/run" \
    SOT_COMM_HOME="$SHARED/comm" SOT_HOSTS="$W/no-hosts.toml" \
    "$SOTD" --socket "$W/run/sotd.sock" --project-root "$W/project" < /dev/null > "$W/sotd.out" 2>&1 &
DAEMON=$!
await test -S "$W/run/sotd.sock" || fail "no daemon socket; $(tail -3 "$W/sotd.out")"
await test -s "$SHARED/comm/inbox-lock-manager" || fail "the daemon wrote no inbox lock record"

# row VAR LABEL HANDLE — a bash row in SHARED/LABEL created with HANDLE; its id in VAR (and in ROWS, for the end) once
# it is ready.
row() {
    local out id
    mkdir -p "$SHARED/$2" || return 1
    out="$(req workspace.create "$(jq -nc --arg l "$2" --arg r "$SHARED/$2" --arg h "$3" \
        '{label:$l,project_root:$r,agent:"none",agent_name:$h,boot:true}')")" || { echo "create $2: $out" >&2; return 1; }
    id="$(printf '%s' "$out" | jq -r '.payload.workspace_id // empty')"
    [ -n "$id" ] || { echo "create $2: $out" >&2; return 1; }
    ROWS="$ROWS $id"
    printf -v "$1" '%s' "$id"
    ready() { [ "$(req workspace.list '{}' | jq -r --arg w "$id" '.payload.workspaces[]? | select(.workspace_id == $w) | .phase // empty')" = ready ]; }
    long_await 2 ready || { echo "row $2 never ready" >&2; return 1; }
}
# type_into ID LINE — LINE typed into the row's shell, then Enter.
type_into() {
    req pty.input "$(jq -nc --arg w "$1" --arg d "$(printf '%s' "$2" | base64 -w0)" '{workspace_id:$w,data_b64:$d,enter:true}')" >/dev/null
}
# live HANDLE ROW — the registry lists HANDLE, bound to ROW.
live() { jq -e --arg h "$1" --arg w "$2" '.agents[$h].workspace_id == $w' "$SHARED/comm/registry.json" >/dev/null 2>&1; }

# The echo row's one answer: its session reads its mail and answers each PING with a PONG, then writes DONE.
cat > "$SHARED/echo-once.sh" <<'ECHO'
#!/bin/bash
B="$1"
"$B/comm-poll.sh" 2>/dev/null | while IFS= read -r l; do
    case "$l" in
        *"] PING "*) from="${l#*] [}"; from="${from%%:*}"; "$B/comm-send.sh" "@$from" "PONG ${l##* PING }" >> "$2.log" 2>&1 ;;
    esac
done
: > "$2"
ECHO
# echo_once TAG — the echo row answers what it has; 0 once it has.
echo_once() {
    type_into "$ECHO_ROW" "bash '$SHARED/echo-once.sh' '$SHARED/bin' '$SHARED/echo.$1'" || return 1
    await test -e "$SHARED/echo.$1"
}

row ECHO_ROW echo "$ECHO" || fail "the echo row"
type_into "$ECHO_ROW" "cd '$SHARED/echo' && '$SHARED/bin/comm-join.sh' > '$SHARED/echo.join' 2>&1" || fail "type into the echo row"
await live "$ECHO" "$ECHO_ROW" || fail "the echo row never joined as @$ECHO: $(cat "$SHARED/echo.join" 2>/dev/null)"
pass "the echo row joined as @$ECHO, the handle it was created with (its self file, no pin)"

# (a) a row: a second bash row, created with its handle, sends and reads its reply from inside the row.
row SENDER_ROW sender sender-dhost || fail "(a) the sender row"
B="$SHARED/bin"
type_into "$SENDER_ROW" "cd '$SHARED/sender' && '$B/comm-join.sh' > '$SHARED/a.join' 2>&1 && '$B/comm-send.sh' @$ECHO 'PING a1' > '$SHARED/a.send' 2>&1" \
    || fail "(a) type into the sender row"
a_sent() { grep -q "filed -> @$ECHO" "$SHARED/a.send" 2>/dev/null; }
await a_sent || fail "(a) the row's PING: join $(cat "$SHARED/a.join" 2>/dev/null); send $(cat "$SHARED/a.send" 2>/dev/null)"
live sender-dhost "$SENDER_ROW" || fail "(a) the row's handle is not sender-dhost"
echo_once a1 || fail "(a) the echo row did not answer"
type_into "$SENDER_ROW" "'$B/comm-poll.sh' > '$SHARED/a.poll' 2>&1; : > '$SHARED/a.polled'" || fail "(a) type the poll"
await test -e "$SHARED/a.polled" && grep -q '] PONG a1$' "$SHARED/a.poll" || fail "(a) the row read no reply: $(tail -3 "$SHARED/a.poll" 2>/dev/null)"
pass "(a) a row joined as the handle it was created with, mailed the echo row and read its reply"

# (b) a worktree session outside any row, in a folder named as worktrees are: its handle is the folder's.
WT="$SHARED/ship-of-tools--shapes"
mkdir -p "$WT" "$W/wt-home" && chmod 700 "$W/wt-home" || exit 1
export SESS_SOCKET="$W/run/sotd.sock"
sess dhost "$W/wt-home" "$WT" comm-join.sh > "$W/wt.join" 2>&1
wt_h="$(handle_of dhost "$W/wt-home" "$WT")"
[ "$wt_h" = ship-of-tools-shapes-dhost ] || fail "(b) the worktree session's handle is '$wt_h': $(cat "$W/wt.join")"
pass "(b) the worktree session joined as @$wt_h, derived from its folder's name (ship-of-tools--shapes)"
filed_ping dhost "$W/wt-home" "$WT" b1 || fail "(b) the worktree session's PING"
echo_once b1 || fail "(b) the echo row did not answer"
read_pong dhost "$W/wt-home" "$WT" b1 || fail "(b) the worktree session's reply"
pass "(b) the worktree session mailed the echo row and read its reply"
unset SESS_SOCKET

# (c) the server's PING, answered here.
: > "$SHARED/daemon-ready"
echo "waiting for the server's shape (c) on the second host"
c_sent() { [ -e "$SHARED/c.sent" ] || [ -e "$SHARED/failed" ]; }
long_await 14 c_sent && [ ! -e "$SHARED/failed" ] || fail "the server's PING never came"
echo_once c1 || fail "(c) the echo row did not answer the server"
: > "$SHARED/c.replied"
c_done() { [ -e "$SHARED/server-done" ] || [ -e "$SHARED/failed" ]; }
long_await 4 c_done && [ ! -e "$SHARED/failed" ] || fail "the server's shape failed"
pass "(c) seen from the daemon's host: the echo row answered the server session"
echo "PASS shapes (a) a row, (b) a worktree session, (c) a server session with no daemon"

#!/usr/bin/env bash
# test-agent-layers.sh — a process acts as a comm handle only if at most one
# agent lies between it and its row's capsule (or the top of its process tree
# outside a row). A second agent started inside a session (`codex exec`,
# `claude -p`), and anything it starts, has no comm identity: it never reads
# the inbox, sends, joins, stamps, blocks the Stop hook or refreshes the
# heartbeat as the row's handle.
#
#   1. TABLE: _sot_agent_layers counts agent layers in a process chain
#      (comm-lib.sh), over the shapes the rule has to get right.
#   2. END TO END: fake processes, named by argv[0] only (`exec -a`), stand in
#      for a row's capsule and agents, so the result does not depend on what
#      runs this suite. Own first (one agent, no agent, an npm codex that is two
#      processes), then the child (a second agent between the first and the
#      tool shell).
#
# HERMETIC: a temp $SOT_COMM_HOME, a pinned $SOT_COMM_TEST_HOST, per-handle
# $SOT_COMM_SELF_FILEs — never the real ~/.sot-comm.
#
# Usage: comm/core/tests/test-agent-layers.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/../../adapters/claude/hooks" && pwd)"
JOIN="$SCRIPTS_DIR/comm-join.sh"
POLL="$SCRIPTS_DIR/comm-poll.sh"
SEND="$SCRIPTS_DIR/comm-send.sh"
STATUS="$SCRIPTS_DIR/comm-status.sh"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-agent-layers-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "FATAL: mktemp failed" >&2; exit 1; }
export SOT_COMM_HOME="$WORK/home"
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
mkdir -p "$SOT_COMM_HOME"
ln -s "$SCRIPTS_DIR" "$SOT_COMM_HOME/bin"
trap 'rm -rf "${WORK:?}"' EXIT
# A fake findmnt first on PATH reports a local filesystem, so the inbox lock is
# this box's own and every filing is local, whatever $WORK sits on and with no
# daemon involved (the same seam as test-relay-file-first.sh).
mkdir -p "$WORK/findmnt-bin" "$SOT_COMM_HOME/inbox"
printf '#!/bin/sh\necho "ext4 rw,relatime /dev/fake"\n' > "$WORK/findmnt-bin/findmnt"
chmod +x "$WORK/findmnt-bin/findmnt"
export PATH="$WORK/findmnt-bin:$PATH"
bash -c 'source "$1"; sot_inbox_lock_identity "$INBOX_DIR"' _ "$SCRIPTS_DIR/comm-lib.sh" > "$SOT_COMM_HOME/inbox-lock-manager"
export SOT_COMM_TEST_HOST="testhost"
unset SOT_COMM_NAME COMM_STATUS_ORIGIN CLAUDE_CODE_SESSION_ID SOT_WORKSPACE_ID SOT_COMM_SELF_FILE SOT_COMM_HOOKS

PASS=0; FAIL=0
ok()  { echo "PASS: $1"; PASS=$((PASS + 1)); }
bad() { echo "FAIL: $1"; FAIL=$((FAIL + 1)); }
has()   { case "$2" in *"$3"*) ok "$1" ;; *) bad "$1 (no '$3' in: $2)" ;; esac; }
hasnt() { case "$2" in *"$3"*) bad "$1 (found '$3' in: $2)" ;; *) ok "$1" ;; esac; }
eq()    { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (got '$2', want '$3')"; fi; }

# --- 1. the table -------------------------------------------------------------
# shellcheck source=../scripts/comm-lib.sh
source "$SCRIPTS_DIR/comm-lib.sh"
# A chain is one process per argument, caller first; the count is the number of
# layers _sot_agent_layers prints.
tbl() {  # WANT DESC LINE...
    local want="$1" desc="$2" got; shift 2
    got="$(printf '%s\n' "$@" | _sot_agent_layers 2>/dev/null | awk 'NF' | wc -l | tr -d ' ')"
    eq "table: $desc" "$got" "$want"
}
tbl 1 "native claude under the capsule"          bash "/h/.local/bin/claude --x" "sot-capsule run"
tbl 2 "codex exec under a claude row (the repro)" bash "bash -lc p" "/v/codex/codex exec" "node /n/bin/codex exec" "/bin/bash -c s" "/h/.local/bin/claude" "sot-capsule run"
tbl 1 "npm codex is a wrapper and its binary"     bash "/v/codex/codex" "node /n/bin/codex" "sot-capsule run"
tbl 1 "julia between is not an agent"             bash "julia s.jl" "bash -c s" claude "sot-capsule run"
tbl 1 "python and make between, no capsule"       bash "python3 x.py" make bash claude
tbl 0 "a bash row has no agent"                   bash -bash "sot-capsule run"
tbl 2 "claude -p under a claude row"              bash "claude -p" bash claude "sot-capsule run"
tbl 2 "two native codex"                          bash codex codex
tbl 1 "walk reaches the top outside a row"        bash claude bash tmux systemd
tbl 1 "the walk stops at the capsule"             bash claude "sot-capsule run" bash claude
tbl 2 "windows exe names"                         bash.exe codex.exe node.exe bash.exe claude.exe sot-capsule.exe
tbl 1 "node cli.js is a documented blind spot"    bash "node /x/claude-code/cli.js" claude

# --- 2. end to end --------------------------------------------------------------
printf '%s\n' 'n=$1; shift; exec -a "$n" bash "$@"' > "$WORK/fake.sh"
printf '%s\n' '"$@"; exit $?' > "$WORK/hold.sh"
mkdir -p "$WORK/npm"; cp "$WORK/hold.sh" "$WORK/npm/codex"
F() { bash "$WORK/fake.sh" "$@"; }
# chain KIND SELF CMD... : run CMD in a tool shell under a fake process chain,
# from $WORK, as the handle SELF names. Tool shell last, agents above it.
chain() {
    local kind="$1" self="$2"; shift 2
    # The tool shell writes CMD's status to $RCF: a shell that outlives its
    # command keeps the agent's own process shape, and its own status is not CMD's.
    local tool=(bash -c '"$@"; echo $? > "$RCF"' _ "$@") cap=(sot-capsule hold.sh)
    cd "$WORK" || return 1
    export SOT_COMM_SELF_FILE="$self"
    case "$kind" in
        own)   F "${cap[@]}" bash "$WORK/fake.sh" claude hold.sh "${tool[@]}" ;;
        child) F "${cap[@]}" bash "$WORK/fake.sh" claude hold.sh bash "$WORK/fake.sh" codex hold.sh "${tool[@]}" ;;
        npm)   F "${cap[@]}" bash "$WORK/fake.sh" node "$WORK/npm/codex" bash "$WORK/fake.sh" codex hold.sh "${tool[@]}" ;;
        bash)  F "${cap[@]}" bash hold.sh "${tool[@]}" ;;
    esac
}
RCF="$WORK/rc"; export RCF
rc_of() { RC="$(cat "$RCF" 2>/dev/null)"; rm -f "${RCF:?}"; }
run() { OUT="$(chain "$@" 2>&1)"; rc_of; }   # -> OUT / RC, in the caller's shell

REG="$SOT_COMM_HOME/registry.json"
ROW="row-agent"; PEER="peer"
SELF_ROW="$WORK/self-row.txt"; SELF_PEER="$WORK/self-peer.txt"
CURSOR="$SOT_COMM_HOME/read/$ROW.cursor"; INBOX="$SOT_COMM_HOME/inbox/$ROW.jsonl"
sum() { cat "$@" 2>/dev/null | cksum; }   # a missing file sums as empty, the same every time

run own "$SELF_ROW" "$JOIN" --name "$ROW"   || true; [ "$RC" -eq 0 ] || { echo "FATAL: setup join $ROW: $OUT" >&2; exit 1; }
run own "$SELF_PEER" "$JOIN" --name "$PEER" || true; [ "$RC" -eq 0 ] || { echo "FATAL: setup join $PEER: $OUT" >&2; exit 1; }
run own "$SELF_PEER" "$SEND" "@$ROW" "frame-one"; [ "$RC" -eq 0 ] && [ -s "$INBOX" ] || { echo "FATAL: setup send: $OUT" >&2; exit 1; }
run own "$SELF_ROW" "$STATUS" waiting x;          [ "$RC" -eq 0 ] || { echo "FATAL: setup status: $OUT" >&2; exit 1; }

# The heartbeat hook finds comm-context.sh and comm-lib.sh beside itself.
FLAT="$WORK/flat"; mkdir -p "$FLAT"
cp "$HOOKS_DIR/comm-status-heartbeat.sh" "$FLAT/"
ln -s "$SCRIPTS_DIR/comm-context.sh" "$FLAT/comm-context.sh"
ln -s "$SCRIPTS_DIR/comm-lib.sh" "$FLAT/comm-lib.sh"
# A turn is running (a floor) and the row's stamp is old: what the heartbeat refreshes.
backdate() { jq --arg n "$ROW" '.agents[$n] += {floor: "user", status_at: "2020-01-01T00:00:00Z"}' "$REG" > "$WORK/reg.tmp" && mv "$WORK/reg.tmp" "$REG"; }
state_of() { jq -r --arg n "$ROW" '.agents[$n].state' "$REG"; }
stop_hook() {  # KIND — the Stop hook, as a hook runs it: JSON on stdin
    OUT="$(printf '{}' | CLAUDE_CODE_SESSION_ID=agl-stop chain "$1" "$SELF_ROW" bash "$HOOKS_DIR/comm-status-idle.sh" 2>&1)"; rc_of
}
heartbeat() {  # KIND SESSION
    rm -f "${SOT_COMM_HOME:?}"/state/hb-*.tick 2>/dev/null
    OUT="$(printf '{"tool_name":"Bash"}' | CLAUDE_CODE_SESSION_ID="$2" chain "$1" "$SELF_ROW" bash "$FLAT/comm-status-heartbeat.sh" 2>&1)"; rc_of
}

# --- the child first: a codex exec under a claude row ---------------------------
cur0="$(sum "$CURSOR")"; inbox0="$(sum "$INBOX")"
run child "$SELF_ROW" "$POLL"
eq  "child: poll refuses" "$RC" 1
has "child: poll names the cause" "$OUT" "has no comm identity"
hasnt "child: poll shows no frame" "$OUT" "frame-one"
eq  "child: poll leaves the cursor alone" "$(sum "$CURSOR")" "$cur0"

run child "$SELF_PEER" "$SEND" "@$ROW" "from-child"
eq  "child: send refuses" "$RC" 1
has "child: send says FAILED" "$OUT" "FAILED -> @$ROW:"
has "child: send names the cause" "$OUT" "has no comm identity"
eq  "child: send leaves the inbox alone" "$(sum "$INBOX")" "$inbox0"

reg0="$(sum "$REG")"; selfrow0="$(sum "$SELF_ROW")"
run child "$SELF_ROW" "$JOIN" --name "$ROW"
eq  "child: join refuses" "$RC" 1
has "child: join names the cause" "$OUT" "has no comm identity"
eq  "child: join leaves the self file alone" "$(sum "$SELF_ROW")" "$selfrow0"
eq  "child: join leaves the registry alone" "$(sum "$REG")" "$reg0"

run child "$SELF_ROW" "$STATUS" working y
eq  "child: status working refuses" "$RC" 1
has "child: status working names the cause" "$OUT" "has no comm identity"
run child "$SELF_ROW" "$STATUS" prompt
eq  "child: status prompt is a silent no-op" "$RC" 0
eq  "child: status leaves the registry alone" "$(sum "$REG")" "$reg0"

stop_hook child
hasnt "child: the Stop hook does not block" "$OUT" '"decision"'
eq    "child: the Stop hook leaves the registry alone" "$(sum "$REG")" "$reg0"

backdate; reg1="$(sum "$REG")"
heartbeat child agl-hb-child
eq  "child: the heartbeat leaves the registry alone" "$(sum "$REG")" "$reg1"
eq  "child: the row is still waiting" "$(state_of)" waiting

# --- then the row's own agent ---------------------------------------------------
stop_hook own
has "own: the Stop hook blocks on unread mail" "$OUT" '"decision":"block"'
backdate; reg2="$(sum "$REG")"
heartbeat own agl-hb-own
if [ "$(sum "$REG")" != "$reg2" ]; then ok "own: the heartbeat refreshes the row"; else bad "own: the heartbeat refreshes the row"; fi
run own "$SELF_ROW" "$POLL"
eq  "own: poll succeeds" "$RC" 0
has "own: poll shows the frame" "$OUT" "frame-one"
run npm "$SELF_ROW" "$POLL"
eq  "own: poll under an npm codex row (two processes, one agent)" "$RC" 0
run bash "$SELF_ROW" "$POLL"
eq  "own: poll under a bash row (no agent)" "$RC" 0

echo "agent layers: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]

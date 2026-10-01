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
trap 'rm -rf "${WORK:?}"' EXIT
# Run against a copy of the scripts that can find no daemon and no hub, so a
# setup send can never reach a live one (the test-hub-files.sh seam); the
# append is checked, since a read-only source mode would silently skip it.
cp -r "$SCRIPTS_DIR" "$WORK/scripts" && chmod -R u+w "$WORK/scripts" || { echo "FATAL: cannot copy the scripts" >&2; exit 1; }
SCRIPTS_DIR="$WORK/scripts"
cat >> "$SCRIPTS_DIR/comm-lib.sh" <<'STUB'

# ---- no daemon, no hub (test only) ------------------------------------------
sot_daemon_endpoint() { return 1; }
sot_relay_endpoint() { return 1; }
STUB
grep -q '^sot_relay_endpoint() { return 1; }$' "$SCRIPTS_DIR/comm-lib.sh" || { echo "FATAL: the no-daemon stub did not land in the copy" >&2; exit 1; }
JOIN="$SCRIPTS_DIR/comm-join.sh"; POLL="$SCRIPTS_DIR/comm-poll.sh"
SEND="$SCRIPTS_DIR/comm-send.sh"; STATUS="$SCRIPTS_DIR/comm-status.sh"
ln -s "$SCRIPTS_DIR" "$SOT_COMM_HOME/bin"
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

# --- 0. the guard closed daemon discovery (lib-home-guard.sh, its self-test passed) ---
eq  "guard: sotd resolves to the refusing stub" "$(command -v sotd)" "$_GUARD_STUBS/sotd"
gout="$(sotd probe 2>&1)"; grc=$?
eq  "guard: the sotd stub exits 97" "$grc" 97
has "guard: the sotd stub says it refused" "$gout" "refused daemon discovery"
eq  "guard: pgrep resolves to the refusing stub" "$(command -v pgrep)" "$_GUARD_STUBS/pgrep"
gout="$( . "$SCRIPT_DIR/../scripts/comm-lib.sh" >/dev/null 2>&1; sot_daemon_endpoint 2>/dev/null; sot_relay_endpoint 2>/dev/null )"
eq  "guard: the tree's own comm-lib finds no daemon and no hub" "$gout" ""

# --- 1. the table -------------------------------------------------------------
# shellcheck source=../scripts/comm-lib.sh
source "$SCRIPTS_DIR/comm-lib.sh"
# A chain is one process per argument, caller first; the count is the number of
# layers _sot_agent_layers prints.
# A record is one process's argv, fields joined by US (comm-lib.sh reads
# /proc/<pid>/cmdline NUL-separated, so a space inside an argument survives).
# tbl splits each argument at its spaces; tblu takes fields joined by `|`, for
# an argument that holds a space.
US=$'\037'
tbl() {  # WANT DESC LINE...
    local want="$1" desc="$2" got l recs=(); shift 2
    for l in "$@"; do recs+=("${l// /$US}"); done
    got="$(printf '%s\n' "${recs[@]}" | _sot_agent_layers 2>/dev/null | awk 'NF' | wc -l | tr -d ' ')"
    eq "table: $desc" "$got" "$want"
}
tblu() {  # WANT DESC RECORD...   (fields joined by |)
    local want="$1" desc="$2" got l recs=(); shift 2
    for l in "$@"; do recs+=("${l//|/$US}"); done
    got="$(printf '%s\n' "${recs[@]}" | _sot_agent_layers 2>/dev/null | awk 'NF' | wc -l | tr -d ' ')"
    eq "table: $desc" "$got" "$want"
}
# req WANT_RC DESC TEXT RECORD... : sot_require_agent over a stubbed chain
# (fields joined by |); its status and one-line reason.
req() {
    local want="$1" desc="$2" text="$3" res; shift 3; CHAIN=("$@")
    res="$( _sot_ancestor_chain() { local r; [ "${#CHAIN[@]}" -gt 0 ] || return 1; for r in "${CHAIN[@]}"; do printf '%s\n' "${r//|/$US}"; done; }
            o="$(sot_require_agent)"; echo "rc=$?|$o" )"
    case "$res" in "rc=$want|"*"$text"*) ok "require: $desc" ;; *) bad "require: $desc (want rc=$want and '$text', got: $res)" ;; esac
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
tbl 1 "a node script is not an agent by its directory alone" bash "node /x/claude-code/cli.js" claude
tblu 2 "an agent path with spaces"                 bash "/tmp/agent dir/codex|exec" "/tmp/agent dir/claude" "sot-capsule|run"
tblu 2 "node, an option, then an agent script"     bash "node|--no-warnings|/n/bin/codex.js|exec" claude "sot-capsule|run"
tblu 2 "node running a script with a space in its path" bash "node|/tmp/x y/codex.js" claude "sot-capsule|run"
tblu 2 "node running the npm codex package"        bash "node|/n/node_modules/@openai/codex/bin/run.js" claude "sot-capsule|run"
tblu 2 "node running the npm claude package"       bash "node|/n/node_modules/@anthropic-ai/claude-code/cli.js" codex "sot-capsule|run"
tblu 2 "node running a package, windows separators" bash.exe 'node.exe|C:\n\node_modules\@openai\codex\bin\run.js' claude.exe sot-capsule.exe
tblu 1 "node with only options is not an agent"    bash "node|--inspect" claude "sot-capsule|run"
tblu 1 "npm codex over its native child, by package" bash "/v/codex/codex" "node|/n/node_modules/@openai/codex/bin/run.js" "sot-capsule|run"
tblu 2 "node after a native agent of a different name" bash "/v/claude" "node|/n/bin/codex.js" "sot-capsule|run"
tblu 1 "node after its own native agent, same name" bash codex "node|/n/bin/codex.js" "sot-capsule|run"
tblu 2 "an upper-case windows agent name"           bash.exe CLAUDE.EXE codex.exe sot-capsule.exe

# --- 1b. an unreadable ancestry is its own refusal (rc 2), not a child (rc 1) ----
R_TREE_TEXT="cannot read this process's ancestry"
req 0 "one agent, the walk reaches the capsule"      "" bash claude "sot-capsule|run"
req 1 "two agents is a child"                        "has no comm identity" bash codex bash claude "sot-capsule|run"
req 2 "a truncated record before the capsule"        "$R_TREE_TEXT" bash codex '!truncated'
req 2 "a truncated record hides the outer agent"     "$R_TREE_TEXT" bash claude bash '!truncated'
req 0 "a capsule reached before any truncation"      "" bash claude "sot-capsule|run" '!truncated'
req 2 "no chain at all"                              "$R_TREE_TEXT"

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
        # An npm agent is `node <script>`; the script runs CMD in its own shell.
        # NODE_ARGS is node's argv after `node`; "$@" is CMD, which the script runs.
        nodechild) F "${cap[@]}" bash "$WORK/fake.sh" claude hold.sh node "${NODE_ARGS[@]}" "$@" ;;
        nodeown)   F "${cap[@]}" node "${NODE_ARGS[@]}" "$@" ;;
        # DEPTH launchers between the tool shell and the agent, past the walk's cap.
        deep)  local a=(bash hold.sh "${tool[@]}") i
               for ((i = 0; i < 70; i++)); do a=(bash "$WORK/hold.sh" "${a[@]}"); done
               F "${cap[@]}" bash "$WORK/fake.sh" claude hold.sh "${a[@]}" ;;
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
# The shims npm installs: `node <package>/bin/codex.js`. Each runs its arguments
# in a shell with stdio inherited, as the real agents run their tools; the shell
# writes the command's status to $RCF.
SHIM='const r = require("child_process").spawnSync("bash", ["-c", "\"$@\"; echo $? > \"$RCF\"", "_"].concat(process.argv.slice(2)), { stdio: "inherit" }); process.exit(r.status === null ? 1 : r.status);'
mkdir -p "$WORK/nm/node_modules/@openai/codex/bin" "$WORK/nm/node_modules/@anthropic-ai/claude-code" "$WORK/x y"
printf '%s\n' "$SHIM" > "$WORK/nm/node_modules/@openai/codex/bin/codex.js"
printf '%s\n' "$SHIM" > "$WORK/nm/node_modules/@anthropic-ai/claude-code/cli.js"
printf '%s\n' "$SHIM" > "$WORK/x y/codex.js"
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

# --- the npm agents, with a real node: a node-run second agent is a child ---------
# A missing node FAILS this suite; the shims below are not skipped.
if command -v node >/dev/null 2>&1; then ok "node is present"; else bad "node is required for the npm agent cases"; fi
cur0="$(sum "$CURSOR")"
NODE_ARGS=("$WORK/nm/node_modules/@openai/codex/bin/codex.js")
run nodeown "$SELF_ROW" "$POLL"
eq  "node codex.js alone under the capsule is one agent: poll succeeds" "$RC" 0
run nodechild "$SELF_ROW" "$POLL"
eq  "child: poll under claude then node codex.js (npm package path) refuses" "$RC" 1
has "child: that refusal names the cause" "$OUT" "has no comm identity"
NODE_ARGS=("$WORK/nm/node_modules/@anthropic-ai/claude-code/cli.js")
run nodechild "$SELF_ROW" "$POLL"
eq  "child: poll under claude then node claude-code/cli.js refuses" "$RC" 1
NODE_ARGS=(--no-warnings "$WORK/x y/codex.js")
run nodechild "$SELF_ROW" "$POLL"
eq  "child: poll under claude then node --no-warnings 'x y/codex.js' refuses" "$RC" 1
eq  "child: none of those moved the cursor" "$(sum "$CURSOR")" "$cur0"

# --- a refused child changes nothing under the comm home --------------------------
# Every verb, both hooks: the files (by content), and the listing (no new file,
# no registry skeleton), before and after. A legacy self file (no root=) is one
# the context call would heal, and a refusal comes before that.
snap() { { find "$1" | sort; find "$1" -type f -exec sha256sum {} + | sort; } 2>&1; }
SELF_LEG="$WORK/self-legacy.txt"; head -2 "$SELF_ROW" > "$SELF_LEG"
NODE_ARGS=("$WORK/nm/node_modules/@openai/codex/bin/codex.js")
nochange() {  # DESC KIND CMD...
    local desc="$1" kind="$2"; shift 2
    local before after; head -2 "$SELF_ROW" > "$SELF_LEG"
    before="$(snap "$SOT_COMM_HOME"; cat "$SELF_LEG")"
    printf '%s' "${NC_IN:-{\}}" | "$@" >/dev/null 2>&1 || true
    after="$(snap "$SOT_COMM_HOME"; cat "$SELF_LEG")"
    if [ "$after" = "$before" ]; then ok "child: $desc changes nothing under the comm home, nor heals the legacy self file"
    else bad "child: $desc changed the comm home or healed the legacy self file: $(diff <(printf '%s\n' "$before") <(printf '%s\n' "$after") | tr '\n' ' ' | cut -c1-300)"; fi
}
for kind in child nodechild; do
    run_k() { chain "$kind" "$SELF_LEG" "$@" > /dev/null 2>&1; }
    nochange "poll ($kind)"   "$kind" run_k "$POLL"
    nochange "send ($kind)"   "$kind" run_k "$SEND" "@$ROW" "from-child"
    nochange "join ($kind)"   "$kind" run_k "$JOIN" --name "$ROW"
    nochange "status working ($kind)" "$kind" run_k "$STATUS" working z
    nochange "status waiting ($kind)" "$kind" run_k "$STATUS" waiting z
    nochange "leave ($kind)"  "$kind" run_k "$SCRIPTS_DIR/comm-leave.sh"
    nochange "leave --name ($kind)" "$kind" run_k "$SCRIPTS_DIR/comm-leave.sh" --name "$PEER"
    nochange "the Stop hook ($kind)" "$kind" run_k env CLAUDE_CODE_SESSION_ID="agl-nc-stop-$kind" bash "$HOOKS_DIR/comm-status-idle.sh"
    mkdir -p "$SOT_COMM_HOME/state"; : > "$SOT_COMM_HOME/state/hb-agl-nc-$kind.tick"; touch -d '2020-01-01' "$SOT_COMM_HOME/state/hb-agl-nc-$kind.tick"
    NC_IN='{"tool_name":"Bash"}' nochange "the heartbeat ($kind)" "$kind" run_k env CLAUDE_CODE_SESSION_ID="agl-nc-$kind" bash "$FLAT/comm-status-heartbeat.sh"
done
# A fresh comm home: a refused child makes no registry skeleton.
H2="$WORK/home2"; mkdir -p "$H2"
before="$(snap "$H2")"
for v in "$POLL" "$STATUS working q" "$SCRIPTS_DIR/comm-leave.sh"; do
    ( export SOT_COMM_HOME="$H2"; chain child "$SELF_LEG" $v ) >/dev/null 2>&1 || true
done
if [ "$(snap "$H2")" = "$before" ]; then ok "child: no registry skeleton in a fresh comm home"; else bad "child: a refused verb made files in a fresh comm home: $(snap "$H2" | tr '\n' ' ' | cut -c1-300)"; fi

# --- the leave gate: a child cannot remove a row, the row's own agent can --------
LV="leaver"
run own "$WORK/self-lv.txt" "$JOIN" --name "$LV"; eq "setup: join $LV" "$RC" 0
run child "$WORK/self-lv.txt" "$SCRIPTS_DIR/comm-leave.sh"
eq  "child: leave refuses" "$RC" 1
has "child: leave names the cause" "$OUT" "has no comm identity"
if jq -e --arg n "$LV" '.agents[$n]' "$REG" >/dev/null 2>&1; then ok "child: the row is still registered"; else bad "child: the row is still registered"; fi
run own "$WORK/self-lv.txt" "$SCRIPTS_DIR/comm-leave.sh"
eq  "own: leave succeeds" "$RC" 0
if jq -e --arg n "$LV" '.agents[$n]' "$REG" >/dev/null 2>&1; then bad "own: leave removed the row"; else ok "own: leave removed the row"; fi

# --- an ancestry that cannot be read is refused, not trusted ------------------------
# The library is a copy with the chain walk forced to fail (a box with no /proc
# and no ps); the scripts and hooks run from it through $SOT_COMM_HOME/bin.
# (An unreadable /proc cannot be simulated, so that route is covered by the
# table's truncated-record rows above only.)
BIN_T="$WORK/scripts-notree"; cp -r "$SCRIPTS_DIR" "$BIN_T" && chmod -R u+w "$BIN_T" \
    && printf '\n_sot_ancestor_chain() { return 1; }\n' >> "$BIN_T/comm-lib.sh" \
    && grep -q '^_sot_ancestor_chain() { return 1; }$' "$BIN_T/comm-lib.sh" || { echo "FATAL: cannot build the no-ancestry copy" >&2; exit 1; }
ln -sfn "$BIN_T" "$SOT_COMM_HOME/bin"
reg3="$(sum "$REG")"
run own "$SELF_ROW" "$BIN_T/comm-poll.sh"
eq  "no ancestry: poll exits 1" "$RC" 1
has "no ancestry: poll says why" "$OUT" "$R_TREE_TEXT"
run own "$SELF_ROW" "$BIN_T/comm-status.sh" working y
eq  "no ancestry: status working exits 1" "$RC" 1
has "no ancestry: status says why" "$OUT" "$R_TREE_TEXT"
run own "$SELF_ROW" "$BIN_T/comm-send.sh" "@$PEER" x
eq  "no ancestry: send exits 1" "$RC" 1
stop_hook own
has   "no ancestry: the Stop hook says why in a systemMessage" "$OUT" '"systemMessage":"sot-comm: '
hasnt "no ancestry: the Stop hook does not block" "$OUT" '"decision"'
backdate; reg4="$(sum "$REG")"
heartbeat own agl-hb-notree
eq  "no ancestry: the heartbeat leaves the registry alone" "$(sum "$REG")" "$reg4"
has "no ancestry: the heartbeat says why on stderr" "$OUT" "$R_TREE_TEXT"
ln -sfn "$SCRIPTS_DIR" "$SOT_COMM_HOME/bin"
# A chain of 70 launchers between the tool and its agent is past the cap.
run deep "$SELF_ROW" "$POLL"
eq  "a 70-deep chain of launchers is refused" "$RC" 1
has "a 70-deep chain says the ancestry cannot be read" "$OUT" "$R_TREE_TEXT"
# A library too old to hold the gate (rc 127) is not a child either.
BIN_O="$WORK/scripts-oldlib"; cp -r "$SCRIPTS_DIR" "$BIN_O" && chmod -R u+w "$BIN_O" \
    && printf '\nsot_require_agent() { return 127; }\n' >> "$BIN_O/comm-lib.sh" || { echo "FATAL: cannot build the old-lib copy" >&2; exit 1; }
ln -sfn "$BIN_O" "$SOT_COMM_HOME/bin"
stop_hook own
has   "an old lib (rc 127): the Stop hook says so in a systemMessage" "$OUT" '"systemMessage":"sot-comm: '
hasnt "an old lib (rc 127): the Stop hook does not block" "$OUT" '"decision"'
ln -sfn "$SCRIPTS_DIR" "$SOT_COMM_HOME/bin"

echo "agent layers: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]

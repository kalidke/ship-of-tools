# Part of ../test-agent-layers.sh: section 2 to the matrix runner's private copy; sourced where it stood.
# --- 2. end to end --------------------------------------------------------------
printf '%s\n' 'n=$1; shift; exec -a "$n" bash "$@"' > "$WORK/fake.sh"
printf '%s\n' '"$@"; exit $?' > "$WORK/hold.sh"
# An npm codex: the host script forwards its own arguments to the native process.
mkdir -p "$WORK/npm"; printf '%s\n' 'bash "'"$WORK"'/fake.sh" codex "$@"; exit $?' > "$WORK/npm/codex"
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
        npm)   F "${cap[@]}" bash "$WORK/fake.sh" node "$WORK/npm/codex" hold.sh "${tool[@]}" ;;
        bash)  F "${cap[@]}" bash hold.sh "${tool[@]}" ;;
        # One agent beneath the stand-in for row $ROWID's capsule (lib-home-guard.sh).
        rown)  in_row "$ROWID" bash "$WORK/fake.sh" claude hold.sh "${tool[@]}" ;;
        # A Windows row (SIMWIN): a native claude.exe beneath row $ROWID's native capsule.
        wown)  in_wrow "$ROWID" bash "$WORK/fake.sh" claude.exe hold.sh "${tool[@]}" ;;
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
: > "$WORK/x.cjs"
NODE_ARGS=(--require "$WORK/x.cjs" "$WORK/nm/node_modules/@anthropic-ai/claude-code/cli.js")
run nodechild "$SELF_ROW" "$POLL"
eq  "child: poll under claude then node --require x.cjs claude-code/cli.js refuses" "$RC" 1
has "child: that refusal names the cause" "$OUT" "has no comm identity"
eq  "child: none of those moved the cursor" "$(sum "$CURSOR")" "$cur0"

# --- a node host and a native child: one layer only when the child is the host's own forwarding ---
# A copy of bash named claude / codex is the native binary; the -c command is compound, so
# bash cannot exec its last command and drop the native process from the chain.
mkdir -p "$WORK/natbin" "$WORK/nat/node_modules/@anthropic-ai/claude-code" "$WORK/nat/node_modules/@openai/codex/bin"
cp "$(command -v bash)" "$WORK/natbin/claude"; cp "$(command -v bash)" "$WORK/natbin/codex"
# claude: a tool run by node's cli.js, started with its own arguments (they differ from the host's).
printf '%s\n' 'const r = require("child_process").spawnSync(process.env.NAT_BIN, ["-c", process.env.NAT_CMD], { stdio: "inherit" }); process.exit(r.status === null ? 1 : r.status);' > "$WORK/nat/node_modules/@anthropic-ai/claude-code/cli.js"
# codex: the npm launcher forwards its own arguments to the native binary.
printf '%s\n' 'const r = require("child_process").spawnSync(process.env.NAT_BIN, process.argv.slice(2), { stdio: "inherit" }); process.exit(r.status === null ? 1 : r.status);' > "$WORK/nat/node_modules/@openai/codex/bin/codex.js"
NATCHAIN="$WORK/natchain.txt"
NAT_CMD="$(printf '%q; rc=$?; bash -c %q _ %q > %q; exit $rc' "$POLL" '. "$1"; _sot_ancestor_chain' "$SCRIPTS_DIR/comm-lib.sh" "$NATCHAIN")"
natrun() {  # NATIVE_NAME HOST_ARGS...  (NAT_BIN, NAT_CMD in the environment)
    local nat="$1"; shift; rm -f "${NATCHAIN:?}"; cd "$WORK" || return 1
    OUT="$(SOT_COMM_SELF_FILE="$SELF_ROW" NAT_BIN="$WORK/natbin/$nat" NAT_CMD="$NAT_CMD" F sot-capsule hold.sh node "$@" 2>&1)"; RC=$?
}
natchain_has() { case "$(tr "$US" '|' < "$NATCHAIN" 2>/dev/null)" in *"$1"*) return 0 ;; *) return 1 ;; esac; }
cur0="$(sum "$CURSOR")"
natrun claude "$WORK/nat/node_modules/@anthropic-ai/claude-code/cli.js" --permission-mode auto
eq  "child: node cli.js whose tool runs native claude -c with other arguments is refused" "$RC" 1
has "child: that refusal names the cause" "$OUT" "has no comm identity"
if natchain_has "$WORK/natbin/claude|-c|"; then ok "child: the native claude is in the recorded chain"; else bad "child: the native claude is in the recorded chain"; fi
eq  "child: that refusal did not move the cursor" "$(sum "$CURSOR")" "$cur0"
natrun codex "$WORK/nat/node_modules/@openai/codex/bin/codex.js" -c "$NAT_CMD"
eq  "own: node codex.js forwarding its arguments to the native codex is one layer: poll succeeds" "$RC" 0
if natchain_has "$WORK/natbin/codex|-c|"; then ok "own: the native codex is in the recorded chain"; else bad "own: the native codex is in the recorded chain"; fi
# Exact arguments: the host's and the native child's differ only by a newline against a space
# (the command is the same either way): two layers, refused. Identical ones are one layer.
NAT_CMD_NL="${NAT_CMD/; /;$'\n'}"
[ "$NAT_CMD_NL" != "$NAT_CMD" ] || { echo "FATAL: the newline variant of the native command did not differ" >&2; exit 1; }
cur0="$(sum "$CURSOR")"
natrun claude "$WORK/nat/node_modules/@anthropic-ai/claude-code/cli.js" -c "$NAT_CMD_NL"
eq  "child: a node host and a native child whose arguments differ only by a newline against a space are two layers: refused" "$RC" 1
has "child: that refusal names the cause" "$OUT" "has no comm identity"
if natchain_has "$WORK/natbin/claude|-c|"; then ok "child: the native claude is in the recorded chain (newline vs space)"; else bad "child: the native claude is in the recorded chain (newline vs space)"; fi
eq  "child: that refusal did not move the cursor" "$(sum "$CURSOR")" "$cur0"
natrun claude "$WORK/nat/node_modules/@anthropic-ai/claude-code/cli.js" -c "$NAT_CMD"
eq  "own: a node host and a native child with identical arguments are one layer: poll succeeds" "$RC" 0

# --- a refused child changes nothing under the comm home --------------------------
# Every verb, both hooks: the files (by content, size and mtime), and the listing
# (no new file, no registry skeleton), before and after. A legacy self file (no root=) is one
# the context call would heal, and a refusal comes before that.
snap() { { find "$1" | sort; find "$1" -type f -exec sha256sum {} + | sort; find "$1" -type f -exec stat -c '%n %s %Y' {} + | sort; } 2>&1; }
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
    nochange "comm-context.sh ($kind)"       "$kind" run_k "$SCRIPTS_DIR/comm-context.sh"
    nochange "comm-session-start.sh ($kind)" "$kind" run_k "$SCRIPTS_DIR/comm-session-start.sh"
    nochange "comm-relay.sh send ($kind)"    "$kind" run_k "$SCRIPTS_DIR/comm-relay.sh" send "@$PEER" from-child
    nochange "comm-bootstrap.sh ($kind)"     "$kind" run_k "$SCRIPTS_DIR/comm-bootstrap.sh" some-row
    NC_IN='{"tool_name":"Bash"}' nochange "the heartbeat ($kind)" "$kind" run_k env CLAUDE_CODE_SESSION_ID="agl-nc-$kind" bash "$FLAT/comm-status-heartbeat.sh"
    # An AskUserQuestion answer marker is the owner's: a child's hook must not consume it.
    mkdir -p "$SOT_COMM_HOME/state"; ASKQ="$SOT_COMM_HOME/state/askq-agl-q-$kind.marker"; : > "$ASKQ"
    NC_IN='{"tool_name":"AskUserQuestion","tool_use_id":"agl-q-'"$kind"'"}' nochange "the heartbeat on an AskUserQuestion answer ($kind)" "$kind" run_k env CLAUDE_CODE_SESSION_ID="agl-nc-q-$kind" bash "$FLAT/comm-status-heartbeat.sh"
    if [ -f "$ASKQ" ]; then ok "child: the AskUserQuestion marker survives the heartbeat ($kind)"; else bad "child: the AskUserQuestion marker survives the heartbeat ($kind)"; fi
    # The PreToolUse hook of that dialog writes no marker for a child either.
    NC_IN='{"tool_name":"AskUserQuestion","tool_use_id":"agl-qb-'"$kind"'"}' nochange "the AskUserQuestion PreToolUse hook ($kind)" "$kind" run_k bash "$HOOKS_DIR/comm-status-blocked.sh"
    if [ ! -e "$SOT_COMM_HOME/state/askq-agl-qb-$kind.marker" ]; then ok "child: the AskUserQuestion PreToolUse hook writes no marker ($kind)"; else bad "child: the AskUserQuestion PreToolUse hook writes no marker ($kind)"; fi
done
# The row's own agent does consume it (the answer earns the prompt).
ASKQ="$SOT_COMM_HOME/state/askq-agl-q-own.marker"; : > "$ASKQ"
printf '%s' '{"tool_name":"AskUserQuestion","tool_use_id":"agl-q-own"}' | CLAUDE_CODE_SESSION_ID=agl-nc-q-own chain own "$SELF_ROW" bash "$FLAT/comm-status-heartbeat.sh" >/dev/null 2>&1 || true
if [ ! -f "$ASKQ" ]; then ok "own: the heartbeat consumes the AskUserQuestion marker"; else bad "own: the heartbeat consumes the AskUserQuestion marker"; fi
# The row's own agent: the PreToolUse hook stamps blocked and the marker appears (written by
# comm-status.sh, not by the hook); the heartbeat's PostToolUse consumes it. A manual
# `comm-status.sh blocked` carries no tool_use_id and writes no marker.
QM="$SOT_COMM_HOME/state/askq-agl-qb-own.marker"; rm -f "${QM:?}"
printf '%s' '{"tool_name":"AskUserQuestion","tool_use_id":"agl-qb-own"}' | CLAUDE_CODE_SESSION_ID=agl-qb-own chain own "$SELF_ROW" bash "$HOOKS_DIR/comm-status-blocked.sh" >/dev/null 2>&1 || true
if [ -f "$QM" ]; then ok "own: the AskUserQuestion PreToolUse hook leaves its marker"; else bad "own: the AskUserQuestion PreToolUse hook leaves its marker"; fi
printf '%s' '{"tool_name":"AskUserQuestion","tool_use_id":"agl-qb-own"}' | CLAUDE_CODE_SESSION_ID=agl-qb-own2 chain own "$SELF_ROW" bash "$FLAT/comm-status-heartbeat.sh" >/dev/null 2>&1 || true
if [ ! -e "$QM" ]; then ok "own: the heartbeat's PostToolUse consumes that marker"; else bad "own: the heartbeat's PostToolUse consumes that marker"; fi
nq0="$(ls "$SOT_COMM_HOME/state" | grep -c '^askq-' || true)"
run own "$SELF_ROW" "$STATUS" blocked "a manual question"; eq "own: a manual comm-status.sh blocked succeeds" "$RC" 0
eq  "own: a manual comm-status.sh blocked writes no marker" "$(ls "$SOT_COMM_HOME/state" | grep -c '^askq-' || true)" "$nq0"
run own "$SELF_ROW" "$STATUS" working y; run own "$SELF_ROW" "$STATUS" waiting x   # leave the row as it was
# A child neither spawns nor despawns rows: every form refuses, whatever --task says,
# and the comm home (registry, inboxes, the row's own entry) is byte for byte as it was.
for v in "--name h $WORK" "--name h $WORK --task t" "h $WORK" "h $WORK --task t" "$WORK" "--name h $WORK --endpoint unix:/nonexistent"; do
    for kind in child nodechild; do
        before="$(snap "$SOT_COMM_HOME"; cat "$SELF_ROW")"
        run "$kind" "$SELF_ROW" "$SCRIPTS_DIR/comm-spawn.sh" $v
        eq  "child: comm-spawn.sh $v ($kind) exits 1" "$RC" 1
        has "child: comm-spawn.sh $v ($kind) names the cause" "$OUT" "has no comm identity"
        if [ "$(snap "$SOT_COMM_HOME"; cat "$SELF_ROW")" = "$before" ]; then ok "child: comm-spawn.sh $v ($kind) changes nothing under the comm home"; else bad "child: comm-spawn.sh $v ($kind) changed the comm home"; fi
    done
done
for v in "h" "$ROW" "h --endpoint unix:/nonexistent" "--endpoint unix:/nonexistent h"; do
    for kind in child nodechild; do
        before="$(snap "$SOT_COMM_HOME"; cat "$SELF_ROW")"
        run "$kind" "$SELF_ROW" "$SCRIPTS_DIR/comm-despawn.sh" $v
        eq  "child: comm-despawn.sh $v ($kind) exits 1" "$RC" 1
        has "child: comm-despawn.sh $v ($kind) names the cause" "$OUT" "has no comm identity"
        if [ "$(snap "$SOT_COMM_HOME"; cat "$SELF_ROW")" = "$before" ]; then ok "child: comm-despawn.sh $v ($kind) changes nothing under the comm home"; else bad "child: comm-despawn.sh $v ($kind) changed the comm home"; fi
    done
done
# comm-probe.sh makes and stops rows: a child refuses it, before the home or any request.
for v in up down serve status "" --help bogus; do
    for kind in child nodechild; do
        before="$(snap "$SOT_COMM_HOME"; cat "$SELF_ROW")"
        run "$kind" "$SELF_ROW" "$SCRIPTS_DIR/comm-probe.sh" $v
        eq  "child: comm-probe.sh $v ($kind) exits 1" "$RC" 1
        has "child: comm-probe.sh $v ($kind) names the cause" "$OUT" "has no comm identity"
        if [ "$(snap "$SOT_COMM_HOME"; cat "$SELF_ROW")" = "$before" ]; then ok "child: comm-probe.sh $v ($kind) changes nothing under the comm home"; else bad "child: comm-probe.sh $v ($kind) changed the comm home"; fi
    done
done
# comm-worktree-new.sh gates before any git write: a child refuses it, and no worktree or branch is made.
WTR="$WORK/wtp/repo"; mkdir -p "$WTR"
{ git init -q "$WTR" && git -C "$WTR" config core.hooksPath /dev/null \
    && git -C "$WTR" -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false commit -q --allow-empty -m init; } >/dev/null 2>&1 \
    || { echo "FATAL: setup git repo for comm-worktree-new.sh" >&2; exit 1; }
for v in "c1" "c1 --no-spawn"; do
    for kind in child nodechild; do
        before="$(snap "$SOT_COMM_HOME"; cat "$SELF_ROW")"
        run "$kind" "$SELF_ROW" bash -c 'cd "$1" && shift && exec "$@"' _ "$WTR" "$SCRIPTS_DIR/comm-worktree-new.sh" $v
        eq  "child: comm-worktree-new.sh $v ($kind) exits 1" "$RC" 1
        has "child: comm-worktree-new.sh $v ($kind) names the cause" "$OUT" "comm-worktree-new.sh: this process runs under"
        if [ ! -e "$WORK/wtp/worktrees" ] && ! git -C "$WTR" show-ref --quiet --verify refs/heads/wt/c1; then ok "child: comm-worktree-new.sh $v ($kind) makes no worktree or branch"; else bad "child: comm-worktree-new.sh $v ($kind) made a worktree or branch"; fi
        if [ "$(snap "$SOT_COMM_HOME"; cat "$SELF_ROW")" = "$before" ]; then ok "child: comm-worktree-new.sh $v ($kind) changes nothing under the comm home"; else bad "child: comm-worktree-new.sh $v ($kind) changed the comm home"; fi
    done
done
run own "$SELF_ROW" bash -c 'cd "$1" && shift && exec "$@"' _ "$WTR" "$SCRIPTS_DIR/comm-worktree-new.sh" c2 --no-spawn
if [ "$RC" -eq 0 ] && [ -d "$WORK/wtp/worktrees/repo-wt-c2" ] && git -C "$WTR" show-ref --quiet --verify refs/heads/wt/c2; then ok "own: comm-worktree-new.sh c2 --no-spawn makes the worktree and branch"; else bad "own: comm-worktree-new.sh c2 --no-spawn (rc $RC: $OUT)"; fi
run own "$SELF_ROW" bash -c 'cd "$1" && shift && exec "$@"' _ "$WTR" "$SCRIPTS_DIR/comm-worktree-new.sh" --help
eq  "own: comm-worktree-new.sh --help exits 0" "$RC" 0
has "own: comm-worktree-new.sh --help describes --expertise" "$OUT" "--expertise \"...\" comma-separated"
has "own: comm-worktree-new.sh --help describes --display-prefix (the last option)" "$OUT" "--display-prefix L  override"
# A fresh comm home: a refused child makes no registry skeleton.
H2="$WORK/home2"; mkdir -p "$H2"
before="$(snap "$H2")"
for v in "$POLL" "$STATUS working q" "$SCRIPTS_DIR/comm-leave.sh" "$SCRIPTS_DIR/comm-list.sh"; do
    ( export SOT_COMM_HOME="$H2"; chain child "$SELF_LEG" $v ) >/dev/null 2>&1 || true
done
for v in "$SCRIPTS_DIR/comm-context.sh" "$SCRIPTS_DIR/comm-session-start.sh" "$SCRIPTS_DIR/comm-relay.sh send @$PEER x" "$SCRIPTS_DIR/comm-bootstrap.sh some-row" \
         "$SCRIPTS_DIR/comm-spawn.sh --name h $WORK" "$SCRIPTS_DIR/comm-despawn.sh h" "$SCRIPTS_DIR/comm-probe.sh up" "$SCRIPTS_DIR/comm-probe.sh down" \
         "$SCRIPTS_DIR/comm-probe.sh" "$SCRIPTS_DIR/comm-probe.sh --help" "$SCRIPTS_DIR/comm-probe.sh bogus"; do
    ( export SOT_COMM_HOME="$H2"; chain child "$SELF_LEG" $v ) >/dev/null 2>&1 || true
done
if [ "$(snap "$H2")" = "$before" ]; then ok "child: no registry skeleton in a fresh comm home"; else bad "child: a refused verb made files in a fresh comm home: $(snap "$H2" | tr '\n' ' ' | cut -c1-300)"; fi

# A comm home with a registry and no state directory: a refused child's heartbeat
# makes no state directory and no tick; the row's own agent makes both.
H3="$WORK/home3"; mkdir -p "$H3"; cp "$REG" "$H3/registry.json"
before="$(snap "$H3")"
printf '{"tool_name":"Bash"}' | ( export SOT_COMM_HOME="$H3" CLAUDE_CODE_SESSION_ID=agl-h3; chain child "$SELF_LEG" bash "$FLAT/comm-status-heartbeat.sh" ) >/dev/null 2>&1 || true
if [ "$(snap "$H3")" = "$before" ]; then ok "child: the heartbeat makes no state directory or tick in a comm home with a registry"; else bad "child: the heartbeat made files in a comm home with a registry: $(snap "$H3" | tr '\n' ' ' | cut -c1-300)"; fi
printf '{"tool_name":"Bash"}' | ( export SOT_COMM_HOME="$H3" CLAUDE_CODE_SESSION_ID=agl-h3; chain own "$SELF_LEG" bash "$FLAT/comm-status-heartbeat.sh" ) >/dev/null 2>&1 || true
if [ "$(snap "$H3")" != "$before" ]; then ok "own: the same heartbeat makes its tick"; else bad "own: the same heartbeat makes its tick"; fi

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

# --- the row rule, end to end: a process naming a row must run inside that row's capsule ---
mkdir -p "$WORK/self"
RA="row-a"; SELF_WA="$WORK/self/testhost__ws-a.txt"; CUR_A="$SOT_COMM_HOME/read/$RA.cursor"
ROWID=ws-a; run rown "$SELF_WA" "$JOIN" --name "$RA" || true; [ "$RC" -eq 0 ] || { echo "FATAL: setup join $RA in its row: $OUT" >&2; exit 1; }
run own "$SELF_PEER" "$SEND" "@$RA" "frame-row"; [ "$RC" -eq 0 ] || { echo "FATAL: setup send to $RA: $OUT" >&2; exit 1; }
cur0="$(sum "$CUR_A")"
ROWID=ws-b; run rown "$SELF_WA" "$POLL"
eq  "row rule: poll beneath ws-b's capsule, naming ws-a, exits 1 (the library's rc 2)" "$RC" 1
has "row rule: that refusal names the cause" "$OUT" "$R_ROW_TEXT"
hasnt "row rule: that poll shows no frame" "$OUT" "frame-row"
eq  "row rule: that poll leaves the cursor alone" "$(sum "$CUR_A")" "$cur0"
ROWID=ws-a; run rown "$SELF_WA" "$POLL"
eq  "row rule: poll beneath ws-a's capsule, naming ws-a, succeeds" "$RC" 0
# The Stop hook shows the refusal and stamps nothing.
reg0="$(sum "$REG")"
OUT="$(printf '{}' | CLAUDE_CODE_SESSION_ID=agl-stop-row ROWID=ws-b chain rown "$SELF_WA" bash "$HOOKS_DIR/comm-status-idle.sh" 2>&1)"; rc_of
eq  "row rule: the Stop hook beneath ws-b, naming ws-a, exits 0" "$RC" 0
has "row rule: the Stop hook shows a systemMessage" "$OUT" '"systemMessage"'
has "row rule: the Stop hook names the cause" "$OUT" "$R_ROW_TEXT"
eq  "row rule: the Stop hook leaves the registry alone" "$(sum "$REG")" "$reg0"
# An orphan: its shell exits at once, so no ws-a capsule is above it.
ORC="$WORK/orphan.rc"; rm -f "${ORC:?}"
( cd "$WORK" && export SOT_COMM_SELF_FILE="$SELF_WA" RCF && in_row ws-a bash -c '( sleep 1; "$@" > "$0.out" 2>&1; echo $? > "$0" ) < /dev/null > /dev/null 2>&1 & exit 0' "$ORC" "$POLL" ) > /dev/null 2>&1
for ((i = 0; i < 100; i++)); do [ -s "$ORC" ] && break; sleep 0.1; done
eq  "row rule: an orphan naming ws-a, reparented away from its capsule, exits 1 (the library's rc 2)" "$(cat "$ORC" 2>/dev/null)" 1
has "row rule: the orphan's refusal names the cause" "$(cat "$ORC.out" 2>/dev/null)" "$R_ROW_TEXT"

# --- the Windows walk, end to end: every comm script started as a script, as on the box ---
# SIMWIN turns on the stand-ins appended to the scripts' copy: an argv[0] ending in .exe
# is native, anything else MSYS, and an MSYS process started by an MSYS program other
# than a fork of itself has no live Windows parent (an MSYS exec).
SIMWIN="$WORK/simwin"; mkdir -p "$SIMWIN"
cat > "$SIMWIN/sotd" <<'SIMSOTD'
#!/usr/bin/env bash
# sotd.exe ancestors on a Linux "Windows": from the parent of --from's process
# (default: this one), parent first, one `<pid>\t<exe>\t<command line>` line each,
# stopping after an MSYS process whose parent is an MSYS process with another
# command line (an MSYS exec: its old Windows process has exited).
case "$#:${1:-}:${2:-}" in
    1:ancestors:) s=$$ ;;
    3:ancestors:--from) s=$3 ;;
    *) exit 2 ;;
esac
a0()   { local a=""; IFS= read -r -d '' a < "/proc/$1/cmdline" 2>/dev/null; printf '%s' "$a"; }
cl()   { local a out=""; while IFS= read -r -d '' a; do case "$a" in *[\ \"]*) a="\"${a//\"/\\\"}\"" ;; esac; out="$out${out:+ }$a"; done < "/proc/$1/cmdline" 2>/dev/null; printf '%s' "$out"; }
ppid() { local l; IFS= read -r l < "/proc/$1/stat" 2>/dev/null || return 1; l="${l##*) }"; l="${l#* }"; printf '%s' "${l%% *}"; }
p="$(ppid "$s")" || exit 1
while [ "$p" -gt 1 ] && [ -r "/proc/$p/cmdline" ]; do
    n0="$(a0 "$p")"; c="$(cl "$p")"
    printf '%s\t%s\t%s\n' "$p" "${n0##*/}" "$c"
    pp="$(ppid "$p")" || break
    case "$n0" in *.exe) ;; *)
        case "$(a0 "$pp")" in *.exe|'') ;; *) [ "$c" = "$(cl "$pp")" ] || break ;; esac ;;
    esac
    p="$pp"
done
exit 0
SIMSOTD
chmod +x "$SIMWIN/sotd"
# The 6b probe: run by a second agent's own process, as a script.
cat > "$SIMWIN/probe.sh" <<PROBE
export SOT_COMM_HOME="$SIMWIN/home-6b"
. "$SCRIPTS_DIR/comm-lib.sh"
why="\$(sot_require_agent)"; echo "require rc=\$? \$why"
bash "$SCRIPTS_DIR/comm-send.sh" @$PEER from-6b 2>&1; echo "send rc=\$?"
bash "$SCRIPTS_DIR/comm-context.sh" > /dev/null 2>&1; echo "context rc=\$?"
echo "home entries: \$(ls -A "$SIMWIN/home-6b" | wc -l | tr -d ' ')"
PROBE
in_wrow() {  # ID CMD... : CMD beneath a native stand-in for row ID's capsule
    local id="$1"; shift
    ( exec -a sot-capsule.exe bash -c 'shift; "$@"; exit $?' _ "/s/workspaces/$id/voyages/v0" "$@" )
}
RW="row-w"; SELF_WW="$WORK/self/testhost__ws-w.txt"
ROWID=ws-w; run rown "$SELF_WW" "$JOIN" --name "$RW" || true
eq  "windows walk: setup, $RW joins in its row (a Linux walk)" "$RC" 0
run own "$SELF_PEER" "$SEND" "@$RW" "frame-w"
eq  "windows walk: setup, the peer sends to $RW (a Linux walk)" "$RC" 0
export SIMWIN
run wown "$SELF_WW" "$POLL"
eq  "windows walk: the row's own agent polls from a script started as a script" "$RC" 0
has "windows walk: that poll shows the frame" "$OUT" "frame-w"
run wown "$SELF_WW" bash "$WORK/hold.sh" "$POLL"
eq  "windows walk: the row's own agent polls from a script started from a script" "$RC" 0
WCOD=(bash "$WORK/fake.sh" codex.exe hold.sh bash -c '"$@"; exit $?' _ "$POLL")
run wown "$SELF_WW" "${WCOD[@]}"
eq  "windows walk: a poll under a native codex.exe in the tool shell refuses" "$RC" 1
has "windows walk: that refusal names claude's session" "$OUT" "$WCX"
for v in "$SELF_WW" "$WORK/self/self-x.txt"; do
    case "$v" in "$SELF_WW") lbl="naming ws-w" ;; *) lbl="naming no row" ;; esac
    run wown "$v" bash "$WORK/hold.sh" "${WCOD[@]}"
    eq  "windows walk: codex.exe started by a script, $lbl: poll refuses" "$RC" 1
    has "windows walk: codex.exe started by a script, $lbl: the walk goes on past that script to claude" "$OUT" "$WCX"
    rm -rf "${SIMWIN:?}/home-6b"; mkdir -p "$SIMWIN/home-6b"
    run wown "$v" bash "$WORK/fake.sh" codex "$SIMWIN/probe.sh"
    has "windows walk: 6b, $lbl: the probe's own gate counts claude above codex" "$OUT" "require rc=1 this process $WCX"
    has "windows walk: 6b, $lbl: its send is refused at the gate" "$OUT" "FAILED -> @$PEER: this process $WCX"
    has "windows walk: 6b, $lbl: send rc=1" "$OUT" "send rc=1"
    has "windows walk: 6b, $lbl: neither the send nor comm-context.sh wrote to the empty comm home" "$OUT" "home entries: 0"
done
unset SIMWIN

# --- the matrix runner's private copy of the probe row's self file ---------------
SELF_P2="$WORK/self/testhost__ws-p2.txt"; PRIVD="$WORK/matrix-priv"; mkdir -p "$PRIVD"
ROWID=ws-p2; run rown "$SELF_P2" "$JOIN" --name probe2-testhost || true; [ "$RC" -eq 0 ] || { echo "FATAL: setup join probe2-testhost: $OUT" >&2; exit 1; }
PRIV="$( . "$SCRIPT_DIR/comm-matrix.sh" > /dev/null 2>&1; matrix_private_self "$SELF_P2" "$PRIVD" 2> /dev/null )"
PINBOX="$SOT_COMM_HOME/inbox/$PEER.jsonl"; pin0="$(sum "$PINBOX")"
ROWID=ws-dev; run rown "$SELF_P2" "$SEND" "@$PEER" "matrix-orig"
eq  "matrix: sending from the developer's row with the probe row's own self file exits 1 (the library's rc 2)" "$RC" 1
has "matrix: that refusal names the cause" "$OUT" "$R_ROW_TEXT"
eq  "matrix: that send filed nothing" "$(sum "$PINBOX")" "$pin0"
run rown "$PRIV" "$SEND" "@$PEER" "matrix-copy"
eq  "matrix: sending with the private copy exits 0" "$RC" 0
has "matrix: the private copy's send is filed" "$OUT" "filed -> @$PEER"
has "matrix: the peer's inbox line carries the probe row as sender" "$(tail -n 1 "$PINBOX" 2>/dev/null)" "probe2-testhost"

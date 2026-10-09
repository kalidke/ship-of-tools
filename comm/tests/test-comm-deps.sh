#!/usr/bin/env bash
# test-comm-deps.sh — a missing jq, flock or perl is said loudly, never passed
# silently: comm-poll.sh and comm-session-start.sh print one line naming the
# tool and exit nonzero; the Stop hook blocks ONCE per episode naming it,
# prefixes every later block, and clears when the tool is back. Runs against a
# temp $SOT_COMM_HOME; never touches the real ~/.sot-comm.
#
# Usage: comm/tests/test-comm-deps.sh     Exit: 0 if every case PASSes.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home
. "$(dirname "${BASH_SOURCE[0]}")/lib-wait.sh" || exit 2

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/../work_state/hooks" && pwd)"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-deps-test-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "mktemp failed" >&2; exit 1; }
trap 'rm -rf "${WORK:?}"' EXIT

export SOT_COMM_HOME="$WORK/home"
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
SCRIPTS_DIR="$(guard_stage_bin "$WORK")" || exit 2
export SOT_COMM_SELF_FILE="$WORK/self.txt"
export SOT_COMM_TEST_HOST="testhost"
unset SOT_COMM_NAME COMM_STATUS_ORIGIN CLAUDE_CODE_SESSION_ID SOT_WORKSPACE_ID
mkdir -p "$SOT_COMM_HOME"
ln -s "$SCRIPTS_DIR" "$SOT_COMM_HOME/bin"
# shellcheck source=../scripts/comm-lib.sh
source "$SCRIPTS_DIR/comm-lib.sh"
ensure_home
NAME="deps-test"
eval "$("$SCRIPTS_DIR/comm-context.sh" 2>/dev/null | grep -E '^(REPO|PROJECT_ROOT)=')"
sot_write_self_file "$SOT_COMM_SELF_FILE" "$NAME" "$REPO" "$PROJECT_ROOT" || { echo "self-file write failed" >&2; exit 1; }
jq -n --arg n "$NAME" '{agents:{($n):{state:"idle",summary:"x",status_at:"2026-09-08T00:00:00Z",repo:"x"}}}' > "$REGISTRY"

pass=0; fail=0
ok()  { pass=$((pass + 1)); echo "PASS: $1"; }
bad() { fail=$((fail + 1)); echo "FAIL: $1"; }
check() { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (got '$2', want '$3')"; fi; }
has() { case "$2" in *"$3"*) ok "$1" ;; *) bad "$1 (no '$3' in: $2)" ;; esac; }
hasnt() { case "$2" in *"$3"*) bad "$1 (found '$3' in: $2)" ;; *) ok "$1" ;; esac; }

# A PATH holding every tool the paths need except $1: everything in /usr/bin
# and /bin, plus jq, flock and perl wherever they live, behind the guard's
# refusing stubs (they stay first, so the rebuilt PATH cannot reach a daemon).
path_without() {  # TOOL
    local d="$WORK/path-no-$1" f t
    [ -d "$d" ] && { printf '%s' "$_GUARD_STUBS:$d"; return; }
    mkdir -p "$d"
    for f in /usr/bin/* /bin/*; do
        t="${f##*/}"; [ "$t" = "$1" ] || [ -e "$d/$t" ] || ln -s "$f" "$d/$t"
    done
    for t in jq flock perl; do
        [ "$t" = "$1" ] || [ -e "$d/$t" ] || ln -s "$(command -v "$t")" "$d/$t"
    done
    printf '%s' "$_GUARD_STUBS:$d"
}
BASH_BIN="$(command -v bash)"
POLL()  { PATH="$1" "$BASH_BIN" "$SCRIPTS_DIR/comm-poll.sh" 2>/dev/null; }
START() { PATH="$1" "$BASH_BIN" "$SCRIPTS_DIR/comm-session-start.sh" 2>/dev/null; }
STOP()  { printf '{}' | PATH="$1" CLAUDE_CODE_SESSION_ID=deps-sess "$BASH_BIN" "$HOOKS_DIR/comm-status-idle.sh" 2>/dev/null; }

for tool in jq flock perl; do
    P="$(path_without "$tool")"
    MSG="$tool is missing (install it)"
    out="$(POLL "$P")"; rc=$?
    has "poll names $tool on stdout" "$out" "$MSG"
    [ "$rc" -ne 0 ] && ok "poll exits nonzero without $tool" || bad "poll exits nonzero without $tool"
    out="$(START "$P")"; rc=$?
    has "session start names $tool on stdout" "$out" "$MSG"
    [ "$rc" -ne 0 ] && ok "session start exits nonzero without $tool" || bad "session start exits nonzero without $tool"

    rm -rf "${SOT_COMM_HOME:?}/state"
    out="$(STOP "$P")"
    has "stop hook blocks naming $tool" "$out" '"decision":"block"'
    has "stop hook block carries the message for $tool" "$out" "$MSG"
    out="$(STOP "$P")"
    hasnt "the second stop turn is not blocked again ($tool)" "$out" '"decision":"block"'
    # A block after the once-block carries the warning first: a lock-fault-free
    # pending message is the simplest later block, but it needs jq; with jq
    # present, the nudge-free path is enough to show the prefix via mail.
    if [ "$tool" != jq ]; then
        printf '%s\n' "$(jq -nc --arg n "$NAME" '{ts:"2026-09-30T00:00:00Z",from:"peer",to:$n,msg:"hi"}')" > "$INBOX_DIR/$NAME.jsonl"
        out="$(STOP "$P")"
        has "a later block is prefixed by the $tool warning" "$out" "$tool is missing"
        has "a later block still carries the mail notice" "$out" "New sot-comm mail"
        rm -f "${INBOX_DIR:?}/$NAME.jsonl" "${READ_DIR:?}/$NAME.cursor" "${SOT_COMM_HOME:?}"/state/mail-*.tick
    fi
    out="$(STOP "$PATH")"
    hasnt "the warning clears when $tool is back" "$out" "is missing"
    [ ! -e "$(ls "$SOT_COMM_HOME"/state/tool-fault-*.tick 2>/dev/null | head -n 1)" ] && ok "tick cleared for $tool" || bad "tick cleared for $tool"
done

out="$(POLL "$PATH")"; rc=$?
check "all tools: poll behaves as before" "$out" "No messages."
check "all tools: poll exits 0" "$rc" "0"
out="$(START "$PATH")"; rc=$?
hasnt "all tools: session start says nothing missing" "$out" "is missing"
# The 2026-09-19 cold-start field bug: a cold start must exit 0, print exactly
# one BOOTSTRAP-ARM line, and report the identity as ok.
check "all tools: cold session start exits 0" "$rc" "0"
check "all tools: exactly one BOOTSTRAP-ARM line" "$(printf '%s\n' "$out" | grep -c '^BOOTSTRAP-ARM ')" "1"
has "all tools: cold session start reports identity=ok" "$out" "identity=ok"
out="$(STOP "$PATH")"
check "all tools: stop hook prints no block" "$out" ""

# The handoff line (FRESH-LEG): a session start run in a project holding dev/output/handoff-<handle>.md names that
# file after BOOTSTRAP-ARM; without the file it prints no HANDOFF line. The project is its own git root, so the test
# decides the root sot_handoff_line reads.
HP="$WORK/handoff-project"
mkdir -p "$HP/dev/output" && (cd "$HP" && git init -q) && HP="$(cd "$HP" && pwd -P)"
printf 'state\n' > "$HP/dev/output/handoff-$NAME.md"
# The handle's self file is bound to its project, so this project gets one of its own.
HSF="$WORK/handoff-self.txt"
eval "$(cd "$HP" && "$SCRIPTS_DIR/comm-context.sh" 2>/dev/null | grep -E '^(REPO|PROJECT_ROOT)=' | sed 's/^/H_/')"
sot_write_self_file "$HSF" "$NAME" "$H_REPO" "$H_PROJECT_ROOT" || bad "handoff: self file for the handoff project"
out="$(cd "$HP" && SOT_COMM_SELF_FILE="$HSF" START "$PATH")"
has "handoff: session start names the handoff after BOOTSTRAP-ARM" "$(printf '%s\n' "$out" | sed -n '/^BOOTSTRAP-ARM /,$p')" "HANDOFF: read $HP/dev/output/handoff-$NAME.md before other work"
rm -f "${HP:?}/dev/output/handoff-$NAME.md"
out="$(cd "$HP" && SOT_COMM_SELF_FILE="$HSF" START "$PATH")"
has "handoff: the session start without a handoff still joins" "$out" "BOOTSTRAP-ARM handle=$NAME"
hasnt "handoff: no handoff file, no HANDOFF line" "$out" "HANDOFF:"

# D1, D2: a count that needs jq never reads 0 without it. With jq off the PATH,
# a timestamp cursor's offset and the unread count each exit nonzero, print
# nothing on stdout and name jq on stderr.
PJ="$(path_without jq)"
printf '%s\n' "$(jq -nc --arg n "$NAME" '{ts:"2026-09-30T00:00:00Z",from:"peer",to:$n,msg:"hi"}')" > "$INBOX_DIR/$NAME.jsonl"
printf '%s' "2026-01-01T00:00:00Z" > "$READ_DIR/$NAME.cursor"
for fn in sot_cursor_offset sot_unread; do
    out="$(PATH="$PJ" "$BASH_BIN" -c 'source "$1"; '"$fn"' "$2"' _ "$SCRIPTS_DIR/comm-lib.sh" "$NAME" 2>"$WORK/deps.err")"; rc=$?
    [ "$rc" -ne 0 ] && ok "D: $fn exits nonzero without jq" || bad "D: $fn exits nonzero without jq"
    check "D: $fn prints nothing on stdout without jq" "$out" ""
    has "D: $fn names jq on stderr" "$(cat "$WORK/deps.err")" "jq is missing"
done
rm -f "${INBOX_DIR:?}/$NAME.jsonl" "${READ_DIR:?}/$NAME.cursor"

# A mode this script no longer has (the retired --context) must fail loudly
# and write nothing, not fall through to the joining default.
before="$(cksum < "$REGISTRY")"
err="$(PATH="$PATH" "$BASH_BIN" "$SCRIPTS_DIR/comm-session-start.sh" --context 2>&1 >/dev/null)"; rc=$?
check "an unknown argument exits 2" "$rc" "2"
check "an unknown argument prints the usage line" "$err" "usage: comm-session-start.sh"
check "an unknown argument leaves registry.json byte-identical" "$(cksum < "$REGISTRY")" "$before"

# The tool-fault block records its feedback (fb_file is set up before it), so
# the next Stop knows it as the hook's own and not a new prompt: with jq
# present and flock missing, the feedback file is written.
rm -rf "${SOT_COMM_HOME:?}/state"; mkdir -p "$SOT_COMM_HOME/state"
printf '{"transcript_path":"%s"}' "$WORK/tp.jsonl" \
    | PATH="$(path_without flock)" CLAUDE_CODE_SESSION_ID=fb-sess "$BASH_BIN" "$HOOKS_DIR/comm-status-idle.sh" >/dev/null 2>&1
fbn="$(ls "$SOT_COMM_HOME"/state/stop-feedback-fb-sess.jsonl 2>/dev/null | wc -l)"
check "flock missing: the block's feedback is recorded" "$fbn" "1"
rm -rf "${SOT_COMM_HOME:?}/state"; mkdir -p "$SOT_COMM_HOME/state"

# With jq missing and stop_hook_active true, a Stop never blocks twice in a row.
out="$(printf '{"stop_hook_active":true}' | PATH="$(path_without jq)" "$BASH_BIN" "$HOOKS_DIR/comm-status-idle.sh" 2>/dev/null)"
hasnt "jq missing, stop_hook_active: no block" "$out" '"decision":"block"'

# --- the retired `bridge` verb: one log line, then it sleeps ----------------
# An old-form loop (the pre-0.6.6 comm-lib BRIDGE_LOOP text) still re-runs
# `comm-relay.sh bridge` every 2 s on every host; the verb must log once and idle.
OLD_LOOP='while :; do
    "$1" bridge --name "$2" & _c=$!
    while kill -0 "$_c" 2>/dev/null; do
        if [ -n "${3:-}" ] && ! kill -0 "$3" 2>/dev/null; then
            kill "$_c" 2>/dev/null; [ -z "${4:-}" ] || rm -f -- "${4:?}" 2>/dev/null; exit 0
        fi
        sleep 2
    done
    if [ -n "${3:-}" ] && ! kill -0 "$3" 2>/dev/null; then [ -z "${4:-}" ] || rm -f -- "${4:?}" 2>/dev/null; exit 0; fi
    sleep 2
done'
RETIRED='comm-relay: bridge retired in 0.6.6; this leftover loop now sleeps (a reboot clears it)'
mkdir -p "$WORK/bridge"

# (a) tethered: the line appears once; the loop and its child go when the tether dies.
sleep 30 & TETHER=$!
bash -c "$OLD_LOOP" sot-bridge "$SCRIPTS_DIR/comm-relay.sh" h "$TETHER" "" </dev/null >"$WORK/bridge/a.log" 2>&1 &
LOOPA=$!
logged_a() { grep -qF "$RETIRED" "$WORK/bridge/a.log"; }
await logged_a
check "retired bridge, tethered: the line is logged once" "$(grep -cF "$RETIRED" "$WORK/bridge/a.log")" "1"
CHILDA="$(command -p pgrep -P "$LOOPA" | head -n1)"
kill "$TETHER" 2>/dev/null; wait "$TETHER" 2>/dev/null
loop_a_gone() { ! kill -0 "$LOOPA" 2>/dev/null && { [ -z "$CHILDA" ] || ! kill -0 "$CHILDA" 2>/dev/null; }; }
gone=0; await loop_a_gone && gone=1
check "retired bridge, tethered: loop and child gone after the tether" "$gone" "1"
if [ "$gone" != 1 ]; then kill "$LOOPA" $CHILDA 2>/dev/null; fi

# (b) untethered: no retry, the child is a sleep; the test then cleans up its own fixtures.
bash -c "$OLD_LOOP" sot-bridge "$SCRIPTS_DIR/comm-relay.sh" h "" "" </dev/null >"$WORK/bridge/b.log" 2>&1 &
LOOPB=$!
sleep 6
check "retired bridge, untethered: still exactly one line after 6 s" "$(grep -cF "$RETIRED" "$WORK/bridge/b.log")" "1"
CHILDB="$(command -p pgrep -P "$LOOPB" | head -n1)"
check "retired bridge, untethered: the child is a sleep" "$(ps -o comm= -p "${CHILDB:-0}" 2>/dev/null)" "sleep"
kill "$LOOPB" 2>/dev/null; [ -z "$CHILDB" ] || kill "$CHILDB" 2>/dev/null
wait "$LOOPB" 2>/dev/null

# Before bash 5 the registry lock's clock is perl's Time::HiRes, so the mail
# tools list it there, and the check names the module when perl cannot load
# it. bash 5 cannot be made to take that branch (BASH_VERSINFO is read-only),
# so the check itself is tested here, with a perl that has no Time::HiRes.
REAL_PERL="$(command -v perl)"
mkdir -p "$WORK/nohires"
cat > "$WORK/nohires/perl" <<NOHIRES
#!/bin/sh
case "\$*" in *Time::HiRes*) echo "Can't locate Time/HiRes.pm in @INC" >&2; exit 2 ;; esac
exec "$REAL_PERL" "\$@"
NOHIRES
chmod +x "$WORK/nohires/perl"
out="$(PATH="$WORK/nohires:$PATH" bash -c ". '$SCRIPTS_DIR/comm-lib.sh'; sot_require_tools 'read mail' jq Time::HiRes" 2>&1)"; rc=$?
check "no Time::HiRes: the tool check fails" "$rc" "1"
check "no Time::HiRes: one line names it" "$out" "sot-comm: cannot read mail: perl's Time::HiRes is missing (install it)"
out="$(bash -c ". '$SCRIPTS_DIR/comm-lib.sh'; sot_require_tools 'read mail' jq Time::HiRes" 2>&1)"; rc=$?
check "Time::HiRes present: the tool check passes" "$rc:$out" "0:"
check "bash 5 on Linux: the mail tools need no Time::HiRes" "$(bash -c ". '$SCRIPTS_DIR/comm-lib.sh'; sot_mail_tools")" "jq flock perl"

echo "passed=$pass failed=$fail"
[ "$fail" -eq 0 ]

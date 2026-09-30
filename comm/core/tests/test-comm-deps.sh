#!/usr/bin/env bash
# test-comm-deps.sh — a missing jq, flock or perl is said loudly, never passed
# silently: comm-poll.sh and comm-session-start.sh print one line naming the
# tool and exit nonzero; the Stop hook blocks ONCE per episode naming it,
# prefixes every later block, and clears when the tool is back. Runs against a
# temp $SOT_COMM_HOME; never touches the real ~/.sot-comm.
#
# Usage: comm/core/tests/test-comm-deps.sh     Exit: 0 if every case PASSes.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/../../adapters/claude/hooks" && pwd)"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-deps-test-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "mktemp failed" >&2; exit 1; }
trap 'rm -rf "${WORK:?}"' EXIT

export SOT_COMM_HOME="$WORK/home"
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
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

# A PATH directory holding every tool the paths need except $1: everything in
# /usr/bin and /bin, plus jq, flock and perl wherever they live.
path_without() {  # TOOL
    local d="$WORK/path-no-$1" f t
    [ -d "$d" ] && { printf '%s' "$d"; return; }
    mkdir -p "$d"
    for f in /usr/bin/* /bin/*; do
        t="${f##*/}"; [ "$t" = "$1" ] || [ -e "$d/$t" ] || ln -s "$f" "$d/$t"
    done
    for t in jq flock perl; do
        [ "$t" = "$1" ] || [ -e "$d/$t" ] || ln -s "$(command -v "$t")" "$d/$t"
    done
    printf '%s' "$d"
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
        rm -f "$INBOX_DIR/$NAME.jsonl" "$READ_DIR/$NAME.cursor" "$SOT_COMM_HOME"/state/mail-*.tick
    fi
    out="$(STOP "$PATH")"
    hasnt "the warning clears when $tool is back" "$out" "is missing"
    [ ! -e "$(ls "$SOT_COMM_HOME"/state/tool-fault-*.tick 2>/dev/null | head -n 1)" ] && ok "tick cleared for $tool" || bad "tick cleared for $tool"
done

out="$(POLL "$PATH")"; rc=$?
check "all tools: poll behaves as before" "$out" "No messages."
check "all tools: poll exits 0" "$rc" "0"
out="$(START "$PATH")"
hasnt "all tools: session start says nothing missing" "$out" "is missing"
out="$(STOP "$PATH")"
check "all tools: stop hook prints no block" "$out" ""

echo "passed=$pass failed=$fail"
[ "$fail" -eq 0 ]

#!/usr/bin/env bash
# test-crlf-jq-output.sh — a native (non-MSYS) jq.exe on Windows opens stdout
# in text mode and rewrites every \n it writes to \r\n. Bash's own
# consumption idioms keep that \r glued to the value: command substitution
# strips only the FINAL \n, and mapfile/`read` split on \n only. Field
# report, 2026-09-27: a handle read back this way as "admiral-kitt\r" never
# compared equal to "admiral-kitt", so `comm-relay.sh`/`comm-send.sh`
# reported "no such handle" for a send that had already landed.
#
# This test does not assume jq is native-Windows; it makes ANY jq behave
# like one, by putting a wrapper jq first on PATH that captures the real
# jq's output and its exit status, then re-emits the output with \r before
# every \n — exactly the rewrite a Windows jq.exe performs, nothing else.
# The exit status is preserved so a boolean `jq -e` elsewhere in the setup
# path is not disturbed by this stub; only the bug under test is exercised.
#
# It runs `comm-send.sh --broadcast` against a hermetic two-agent registry
# (real comm-join.sh, real jq, for SETUP only) and then broadcasts with the
# CRLF-emitting jq on PATH. Confirmed by running this test against the
# unmodified code (2026-09-27): the FIRST site it trips is
# `sot_require_routable_identity` (comm-lib.sh), which reads its own
# registered project root back via `sot_registry_entry_status` — a trailing
# \r on that root means it never equals the clean $PROJECT_ROOT the sender
# just computed, so the sender's OWN identity is refused as "registered to
# a DIFFERENT project's root" before the broadcast is even attempted. With
# that helper fixed, the send proceeds into comm-send.sh's own broadcast
# fan-out, which reads its target list via
# `mapfile <(jq -r '.agents | keys[] | select(...)')` — the same class one
# layer up: a dirty key there misses the immediately-following
# `.agents[$n].host` lookup even though the row is right there, and the
# send is reported as "no such handle" for a peer that is registered and
# reachable. This test's assertion (a landed broadcast, not a refused
# send) is agnostic to which of the two sites is broken; either one alone
# is enough to fail it.
#
# No bats dependency. HERMETIC: a temp $SOT_COMM_HOME, per-case
# $SOT_COMM_SELF_FILE, a pinned $SOT_COMM_TEST_HOST, and the CRLF jq stub
# confined to a PATH prefix used only for the send under test — setup runs
# on the real jq so it is not itself a variable in this test's verdict.
#
# Usage: comm/core/tests/test-crlf-jq-output.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"

REAL_JQ="$(command -v jq)" || { echo "FATAL: jq not found on PATH" >&2; exit 1; }

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-comm-crlf-jq-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "FATAL: mktemp failed" >&2; exit 1; }
trap 'rm -rf "$WORK"' EXIT
export SOT_COMM_HOME="$WORK/home"
mkdir -p "$SOT_COMM_HOME"

# The CRLF-emitting jq stub: real jq's own stdout and exit status, unchanged,
# with a \r spliced in before every \n it wrote. `sed` runs on the CAPTURED
# text (not a live pipe), so the stub's own exit status is jq's, not sed's —
# a boolean `jq -e ... >/dev/null` elsewhere in this run still sees jq's real
# verdict, exactly as a real Windows jq.exe would report its own.
STUBBIN="$WORK/stubbin"
mkdir -p "$STUBBIN"
cat > "$STUBBIN/jq" <<STUB
#!/usr/bin/env bash
out="\$("$REAL_JQ" "\$@")"
rc=\$?
printf '%s' "\$out" | sed \$'s/\$/\r/'
exit "\$rc"
STUB
chmod +x "$STUBBIN/jq"

JOIN="$SCRIPTS_DIR/comm-join.sh"
SEND="$SCRIPTS_DIR/comm-send.sh"

SENDER="t-crlf-sender"
PEER="t-crlf-peer"
SELF_SENDER="$WORK/self-sender.txt"
SELF_PEER="$WORK/self-peer.txt"

PASS=0; FAIL=0
check() {
    local desc="$1" fn="$2" rc
    "$fn"; rc=$?
    case "$rc" in
        0) echo "PASS: $desc"; PASS=$((PASS + 1)) ;;
        *) echo "FAIL: $desc"; FAIL=$((FAIL + 1)) ;;
    esac
}
contains() { case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac; }

# Setup on the REAL jq only — this test's verdict must never turn on
# whether setup itself tripped the bug; that risk is closed by never
# putting the stub on PATH until the send under test.
setup_rows() {
    ( cd "$WORK" && SOT_COMM_SELF_FILE="$SELF_PEER" SOT_COMM_TEST_HOST="crlfbox" \
        "$JOIN" --name "$PEER" ) >/dev/null 2>&1 || return 1
    ( cd "$WORK" && SOT_COMM_SELF_FILE="$SELF_SENDER" SOT_COMM_TEST_HOST="crlfbox" \
        "$JOIN" --name "$SENDER" ) >/dev/null 2>&1 || return 1
}

SEND_OUT=""; SEND_ERR=""; SEND_RC=0
run_broadcast_with_crlf_jq() {
    SEND_OUT="$(cd "$WORK" && SOT_COMM_SELF_FILE="$SELF_SENDER" SOT_COMM_TEST_HOST="crlfbox" \
        PATH="$STUBBIN:$PATH" \
        "$SEND" --broadcast "landed?" 2>"$WORK/err.txt")"
    SEND_RC=$?
    SEND_ERR="$(cat "$WORK/err.txt" 2>/dev/null)"
    return 0
}

case_broadcast_survives_a_crlf_jq() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    run_broadcast_with_crlf_jq
    if contains "$SEND_ERR" "no such handle"; then
        echo "  stderr claimed 'no such handle' for a peer that IS registered: '$SEND_ERR'"
        return 1
    fi
    contains "$SEND_OUT" "filed -> @$PEER" \
        || { echo "  stdout was '$SEND_OUT', want 'filed -> @$PEER'"; return 1; }
    [ -f "$SOT_COMM_HOME/inbox/$PEER.jsonl" ] \
        || { echo "  no inbox file landed at '$SOT_COMM_HOME/inbox/$PEER.jsonl'"; return 1; }
    return 0
}

check "a broadcast reaches a registered peer even when jq emits CRLF" case_broadcast_survives_a_crlf_jq

# --- the three sites in code decision 0031 KEEPS ---------------------------
# The broadcast case above covers the two readers the 2026-09-27 field report
# tripped, both of them already on sot_jq. These three are the same class in
# code that survives 0031's delete list, each called DIRECTLY so a failure
# names the site instead of a symptom three layers up. Each one gets its own
# $SOT_COMM_HOME so no case depends on another having run first.

CURSOR_HOME="$WORK/cursor-home"
CURSOR_HANDLE="t-crlf-cursor"
seed_cursor_rows() {
    mkdir -p "$CURSOR_HOME/inbox" "$CURSOR_HOME/read" || return 1
    {
        printf '{"from":"a","to":"%s","msg":"one","ts":"2026-09-29T10:00:00Z"}\n'   "$CURSOR_HANDLE"
        printf '{"from":"a","to":"%s","msg":"two","ts":"2026-09-29T10:01:00Z"}\n'   "$CURSOR_HANDLE"
        printf '{"from":"a","to":"%s","msg":"three","ts":"2026-09-29T10:02:00Z"}\n' "$CURSOR_HANDLE"
    } > "$CURSOR_HOME/inbox/$CURSOR_HANDLE.jsonl" || return 1
    printf '2026-09-29T10:00:00Z\n' > "$CURSOR_HOME/read/$CURSOR_HANDLE.cursor"
}

# A cursor still holding a TIMESTAMP (the pre-count form comm-poll.sh
# converts once) is the one read path that asks jq for the offset. A \r glued
# to its answer fails the numeric test, the offset silently reads 0, and the
# session re-reads its whole inbox as unread.
case_cursor_offset_survives_a_crlf_jq() {
    seed_cursor_rows || { echo "  setup: could not seed the inbox and cursor"; return 1; }
    local got
    got="$(SOT_COMM_HOME="$CURSOR_HOME" PATH="$STUBBIN:$PATH" \
        bash -c 'source "$1/comm-lib.sh"; sot_cursor_offset "$2"' \
        _ "$SCRIPTS_DIR" "$CURSOR_HANDLE" 2>/dev/null)"
    [ "$got" = "1" ] \
        || { echo "  offset was '$got', want 1 (0 means the whole inbox reads as unread)"; return 1; }
    return 0
}

PROV_HOME="$WORK/prov-home"
PROV_ROW="t-crlf-prov"
PROV_ROOT="$WORK/prov-root"
PROV_NONCE="n-crlf-1"
seed_provisional_row() {
    mkdir -p "$PROV_HOME" || return 1
    "$REAL_JQ" -n --arg n "$PROV_ROW" --arg r "$PROV_ROOT" --arg o "$PROV_NONCE" \
        '{agents: {($n): {host:"crlfbox", root:$r, status:"spawning", nonce:$o}}}' \
        > "$PROV_HOME/registry.json" || return 1
}

# A spawn that fails rolls its provisional row back, and the rollback is
# CONDITIONAL on reading that row's own status, root and nonce back
# unchanged. A \r on any of the three reads as "this is somebody else's row
# now", the rollback declines, and a dead `spawning` row stays in the
# registry holding the handle.
case_provisional_rollback_survives_a_crlf_jq() {
    seed_provisional_row || { echo "  setup: could not write the provisional row"; return 1; }
    local rc=0
    SOT_COMM_HOME="$PROV_HOME" PATH="$STUBBIN:$PATH" \
        bash -c 'source "$1/comm-lib.sh"; registry_del_if_provisional "$2" "$3" "$4"' \
        _ "$SCRIPTS_DIR" "$PROV_ROW" "$PROV_ROOT" "$PROV_NONCE" >/dev/null 2>&1 || rc=$?
    [ "$rc" -eq 0 ] \
        || { echo "  registry_del_if_provisional returned $rc, want 0 (2 = it declined its own row)"; return 1; }
    "$REAL_JQ" -e --arg n "$PROV_ROW" '.agents | has($n) | not' "$PROV_HOME/registry.json" >/dev/null \
        || { echo "  the provisional row is still in the registry"; return 1; }
    return 0
}

# sot_json_escape's contract is ONE JSON string literal, interpolated into a
# hand-built frame. Text mode puts its \r AFTER the closing quote, outside
# the literal — which is also why stripping is safe HERE and never rewrites
# what a sender wrote: a carriage return the caller really typed comes back
# from jq as the two characters \r, which the strip cannot touch. The second
# assertion is that half, and it is the one that would catch a strip applied
# to free-text content by mistake.
case_json_escape_emits_no_carriage_return() {
    local got
    got="$(PATH="$STUBBIN:$PATH" bash -c 'source "$1/comm-lib.sh"; sot_json_escape "$2"' \
        _ "$SCRIPTS_DIR" "a-value" 2>/dev/null)"
    [ "$got" = '"a-value"' ] \
        || { echo "  escape produced '$got', want the literal \"a-value\""; return 1; }
    got="$(PATH="$STUBBIN:$PATH" bash -c 'source "$1/comm-lib.sh"; sot_json_escape "$2"' \
        _ "$SCRIPTS_DIR" "$(printf 'a\rb')" 2>/dev/null)"
    [ "$got" = '"a\rb"' ] \
        || { echo "  a carriage return the caller typed was not kept as \\r: '$got'"; return 1; }
    return 0
}

check "the timestamp cursor offset survives a CRLF jq" case_cursor_offset_survives_a_crlf_jq
check "a provisional row's rollback survives a CRLF jq" case_provisional_rollback_survives_a_crlf_jq
check "sot_json_escape emits one literal, with a real CR still escaped" case_json_escape_emits_no_carriage_return

echo "---"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]

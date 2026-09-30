#!/usr/bin/env bash
# test-crlf-jq-output.sh — a native (non-MSYS) jq.exe on Windows opens stdout
# in text mode and rewrites every \n it writes to \r\n. Bash's own
# consumption idioms keep that \r glued to the value: command substitution
# strips only the FINAL \n, and mapfile/`read` split on \n only. Field
# report, 2026-09-27: a handle read back this way as "admiral-kitt\r" never
# compared equal to "admiral-kitt", so `comm-relay.sh`/`comm-send.sh`
# reported "no such handle" for a send that had already landed.
#
# Most of this test does not assume jq is native-Windows; it makes ANY jq
# behave like one, by putting a wrapper jq first on PATH that re-emits the
# real jq's output with \r before
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
trap 'rm -rf "${WORK:?}"' EXIT
export SOT_COMM_HOME="$WORK/home"
# 0031 B1: the record a daemon writes at startup. Without it no script
# appends locally, and every filing here would go to a daemon instead.
mkdir -p "$SOT_COMM_HOME/inbox"
# A fake findmnt first on PATH reports a local filesystem, so the record and
# every script's identity are `local <machine-id>` whatever $WORK sits on (a
# function stub would not survive: comm-lib.sh defines _sot_findmnt itself).
mkdir -p "$WORK/findmnt-bin"
printf '#!/bin/sh\necho "ext4 rw,relatime /dev/fake"\n' > "$WORK/findmnt-bin/findmnt"
chmod +x "$WORK/findmnt-bin/findmnt"
export PATH="$WORK/findmnt-bin:$PATH"
bash -c 'source "$1"; sot_inbox_lock_identity "$INBOX_DIR"' _ "$SCRIPTS_DIR/comm-lib.sh" > "$SOT_COMM_HOME/inbox-lock-manager"
mkdir -p "$SOT_COMM_HOME"

# The CRLF-emitting jq stub: real jq's own stdout and exit status, unchanged,
# with a \r spliced in before every \n it writes. It STREAMS, so it adds the
# \r and nothing else -- a capturing stub would also drop jq's final newline,
# which a real Windows jq.exe does not do, and the stub would then be testing
# two bugs at once. Its exit status is jq's own, not sed's, so a boolean
# `jq -e ... >/dev/null` elsewhere in this run still sees jq's real verdict.
STUBBIN="$WORK/stubbin"
mkdir -p "$STUBBIN"
cat > "$STUBBIN/jq" <<STUB
#!/usr/bin/env bash
"$REAL_JQ" "\$@" | sed \$'s/\$/\r/'
exit "\${PIPESTATUS[0]}"
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

# --- the wrapper's own defect, which is NOT a CRLF case --------------------
# sot_jq once captured jq's output in a command substitution, which strips
# every trailing newline, so a `while read` consumer lost its LAST record --
# on every platform, carriage returns or not. Proved live on 2026-09-29:
# comm-list.sh printed 14 of the registry's 15 agents, and the missing one was
# always whichever entry stood last in registry.json. This case therefore runs
# on the REAL jq, with TWO rows so "last" means last and not "only".
#
# The roster assertion is the one that fails without the fix. The send that
# follows it passes at the base commit too, because the directed path reads
# `.agents[$n].host` through a command substitution and never through a
# stream -- it is here as the guard that membership must never MOVE onto a
# stream read, which is the road to a false "no such handle" the captain
# asked to close, not as proof of this fix.
LIST_HOME="$WORK/list-home"
LIST_FIRST="t-crlf-first-row"
LIST_LAST="t-crlf-last-row"
SELF_FIRST="$WORK/self-first-row.txt"
SELF_LAST="$WORK/self-last-row.txt"
seed_two_rows() {
    mkdir -p "$LIST_HOME/inbox" || return 1
    SOT_COMM_HOME="$LIST_HOME" bash -c 'source "$1"; sot_inbox_lock_identity "$INBOX_DIR"' _ "$SCRIPTS_DIR/comm-lib.sh" \
        > "$LIST_HOME/inbox-lock-manager"   # 0031 B1: the daemon's record
    ( cd "$WORK" && SOT_COMM_HOME="$LIST_HOME" SOT_COMM_SELF_FILE="$SELF_FIRST" \
        SOT_COMM_TEST_HOST="crlfbox" "$JOIN" --name "$LIST_FIRST" ) >/dev/null 2>&1 || return 1
    ( cd "$WORK" && SOT_COMM_HOME="$LIST_HOME" SOT_COMM_SELF_FILE="$SELF_LAST" \
        SOT_COMM_TEST_HOST="crlfbox" "$JOIN" --name "$LIST_LAST" ) >/dev/null 2>&1 || return 1
    # Assert the premise rather than assume it: this case is only about the
    # LAST entry if the row it names really is last in the file.
    local last
    last="$("$REAL_JQ" -r '.agents | to_entries | last | .key' "$LIST_HOME/registry.json" 2>/dev/null)"
    [ "$last" = "$LIST_LAST" ] || { echo "  setup: '$last' is last in the registry, wanted '$LIST_LAST'"; return 1; }
}

case_the_last_registry_row_is_not_dropped() {
    seed_two_rows || { echo "  setup: could not seed two rows"; return 1; }
    local out
    out="$(cd "$WORK" && SOT_COMM_HOME="$LIST_HOME" SOT_COMM_SELF_FILE="$SELF_FIRST" \
        SOT_COMM_TEST_HOST="crlfbox" "$SCRIPTS_DIR/comm-list.sh" 2>/dev/null)"
    contains "$out" "@$LIST_LAST" \
        || { echo "  the roster dropped the registry's last row: '$out'"; return 1; }
    local send_out send_err
    send_out="$(cd "$WORK" && SOT_COMM_HOME="$LIST_HOME" SOT_COMM_SELF_FILE="$SELF_FIRST" \
        SOT_COMM_TEST_HOST="crlfbox" "$SEND" "@$LIST_LAST" "to the last row" 2>"$WORK/last-err.txt")"
    send_err="$(cat "$WORK/last-err.txt" 2>/dev/null)"
    if contains "$send_err" "no such handle"; then
        echo "  a send to the registry's last row was refused: '$send_err'"
        return 1
    fi
    contains "$send_out" "filed -> @$LIST_LAST" \
        || { echo "  send said '$send_out', want 'filed -> @$LIST_LAST'"; return 1; }
    return 0
}

check "the timestamp cursor offset survives a CRLF jq" case_cursor_offset_survives_a_crlf_jq
check "a provisional row's rollback survives a CRLF jq" case_provisional_rollback_survives_a_crlf_jq
check "the registry's LAST row is listed and can be sent to" case_the_last_registry_row_is_not_dropped

echo "---"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]

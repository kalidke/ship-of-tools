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

echo "---"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]

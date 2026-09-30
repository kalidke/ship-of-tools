#!/usr/bin/env bash
# test-comm-matrix-verdict.sh — the acceptance matrix's decision logic, which is
# the whole reason the instrument exists: a sender's receipt and an actual
# delivery are different facts, and the two ways they disagree are the two
# faults every one-sided check has missed.
#
#   FALSE FAILURE — the echo arrived but the sender said NOT CONFIRMED. The
#                   path works; only the receipt is broken. A one-sided check
#                   reports an outage that is not there.
#   FALSE SUCCESS — the sender said filed and nothing ever arrived. A one-sided
#                   check reports a delivery that never happened, which is how a
#                   session goes deaf for an hour while its senders print
#                   success lines.
#
# The literal sender lines below are the real ones (comm-send.sh's two-space
# "  filed -> @x", comm-relay.sh's "FAILED -> @x: …" and, on the not-mine leg
# until B2, "NOT CONFIRMED" and "(by …, relay)"), and the
# inbox fixtures are real inbox lines. Nothing pre-computes the verdict's
# inputs: every case feeds raw sender output and a raw inbox file.
#
# No bats dependency. HERMETIC, same seams as test-comm-poll-cursor.sh: a temp
# $SOT_COMM_HOME, never the real ~/.sot-comm, no daemon, nothing sent.
#
# Usage: comm/core/tests/test-comm-matrix-verdict.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-comm-matrix-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "FATAL: mktemp failed" >&2; exit 1; }
export SOT_COMM_HOME="$WORK/home"
export SOT_COMM_SELF_FILE="$WORK/self.txt"
mkdir -p "$SOT_COMM_HOME/inbox"
trap 'rm -rf "${WORK:?}"' EXIT

# The runner, sourced for its decision helpers: it sends nothing when sourced.
# shellcheck source=comm-matrix.sh
source "$SCRIPT_DIR/comm-matrix.sh"

ME="probe2-kitt"
INBOX="$SOT_COMM_HOME/inbox/$ME.jsonl"
NONCE="a1b2c3d4e5f6"
OTHER="0f0f0f0f0f0f"

FILED="  filed -> @probe-kitt (woke)"
FILED_RELAY="filed -> @probe-asus2024 (by bridge@kitt, relay)"
NOT_CONFIRMED="NOT CONFIRMED: sent for @probe-kitt; nobody claimed it within 5s. Attached: fe@kitt."
NO_SUCH="FAILED -> @probe-nosuchrow-box: no box knows that handle: probe-nosuchrow-box"
NO_ANSWER="FAILED -> @probe-nosuchrow-box: the daemon did not answer at ssh:hub"

PASS=0; FAIL=0
check() {  # DESC EXPECTED ACTUAL
    if [ "$2" = "$3" ]; then echo "PASS: $1"; PASS=$((PASS + 1))
    else echo "FAIL: $1"; echo "      expected: $2"; echo "      actual:   $3"; FAIL=$((FAIL + 1)); fi
}

inbox_reset() { : > "$INBOX"; }
inbox_msg() {  # TEXT [TO]
    jq -nc --arg to "${2:-$ME}" --arg m "$1" \
        '{from:"probe-kitt",to:$to,repo:"ship-of-tools",msg:$m,ts:"2026-09-27T22:00:00Z"}' >> "$INBOX"
}
# The state word only — the note is prose and must not be what a test pins.
state_of() { printf '%s\n' "${1%%	*}"; }
note_of()  { printf '%s\n' "${1#*	}"; }

# --- the two faults the instrument exists for --------------------------------

inbox_reset; inbox_msg "ECHO $NONCE from @probe-kitt woke:typed"
v="$(matrix_line_verdict echo 1 "$NOT_CONFIRMED" probe-kitt "$INBOX" "$ME" "$NONCE")"
check "FALSE FAILURE is a FAIL (the echo arrived, the sender denied it)" "FAIL" "$(state_of "$v")"
case "$(note_of "$v")" in *"FALSE FAILURE"*) r=named ;; *) r="$(note_of "$v")" ;; esac
check "FALSE FAILURE is named in the note" "named" "$r"

inbox_reset
v="$(matrix_line_verdict echo 0 "$FILED" probe-kitt "$INBOX" "$ME" "$NONCE")"
check "FALSE SUCCESS is a FAIL (the sender claimed filed, nothing arrived)" "FAIL" "$(state_of "$v")"
case "$(note_of "$v")" in *"FALSE SUCCESS"*) r=named ;; *) r="$(note_of "$v")" ;; esac
check "FALSE SUCCESS is named in the note" "named" "$r"

# --- the honest outcomes -----------------------------------------------------

inbox_reset; inbox_msg "ECHO $NONCE from @probe-kitt woke:typed"
v="$(matrix_line_verdict echo 0 "$FILED" probe-kitt "$INBOX" "$ME" "$NONCE")"
check "claimed AND echoed is a PASS" "PASS" "$(state_of "$v")"

inbox_reset
v="$(matrix_line_verdict echo 1 "$NOT_CONFIRMED" probe-kitt "$INBOX" "$ME" "$NONCE")"
check "refused AND silent is a FAIL (a real outage)" "FAIL" "$(state_of "$v")"

inbox_reset
v="$(matrix_line_verdict nodelivery 1 "$NO_SUCH" probe-nosuchrow-box "$INBOX" "$ME" "$NONCE")"
check "a handle no row hosts, refused, is a PASS" "PASS" "$(state_of "$v")"

inbox_reset
v="$(matrix_line_verdict nodelivery 0 "  filed -> @probe-nosuchrow-kitt" probe-nosuchrow-kitt "$INBOX" "$ME" "$NONCE")"
check "a handle no row hosts, claimed filed, is a FAIL" "FAIL" "$(state_of "$v")"

# A send that never reached the question. The literal line is comm-lib.sh's own
# identity refusal, which is what the first live run of the matrix actually
# printed: the negative line went green while the runner could not send at all.
IDENT_ERR="ERROR: your sot-comm identity did not resolve — refusing to send with no verifiable from-handle (a reply would silently misroute)."
inbox_reset
v="$(matrix_line_verdict nodelivery 1 "$IDENT_ERR" probe-nosuchrow-kitt "$INBOX" "$ME" "$NONCE")"
check "a send that failed before the handle was in question is a SKIP, not a PASS" "SKIP" "$(state_of "$v")"

# The same for a daemon that never answered: its FAILED line is about this end.
inbox_reset
v="$(matrix_line_verdict nodelivery 1 "$NO_ANSWER" probe-nosuchrow-box "$INBOX" "$ME" "$NONCE")"
check "a daemon that did not answer is a SKIP, not a PASS" "SKIP" "$(state_of "$v")"

# --- what must never be mistaken for this line's echo ------------------------

inbox_reset; inbox_msg "ECHO $OTHER from @probe-kitt woke:poll"
v="$(matrix_line_verdict echo 0 "$FILED" probe-kitt "$INBOX" "$ME" "$NONCE")"
check "another line's echo does not satisfy this nonce" "FAIL" "$(state_of "$v")"

inbox_reset; inbox_msg "ECHO $NONCE from @probe-kitt woke:typed"
v="$(matrix_line_verdict echo 0 "$FILED_RELAY" probe-kitt "$INBOX" "$ME" "$NONCE")"
check "a receipt naming a DIFFERENT target is not this line's claim" "FAIL" "$(state_of "$v")"

inbox_reset
printf '{"from":"probe-kitt","to":"%s","msg":"ECHO %s par\n' "$ME" "$NONCE" >> "$INBOX"
inbox_msg "ECHO $NONCE from @probe-kitt woke:poll"
v="$(matrix_line_verdict echo 0 "$FILED" probe-kitt "$INBOX" "$ME" "$NONCE")"
check "a torn inbox line is skipped, not fatal, and the real echo still counts" "PASS" "$(state_of "$v")"

inbox_reset; inbox_msg "ECHO $NONCE from @probe-kitt woke:typed" "someone-else"
v="$(matrix_line_verdict echo 0 "$FILED" probe-kitt "$INBOX" "$ME" "$NONCE")"
check "the per-handle inbox is this handle's own, whatever a line's \`to\` says" "PASS" "$(state_of "$v")"

# --- the reverse direction, judged from the other box's VERDICT line ---------

inbox_reset; inbox_msg "ECHO $NONCE from @probe-asus2024 woke:typed"
inbox_msg "VERDICT $NONCE @probe-asus2024->@$ME rc=0 filed -> @$ME (by bridge@kitt, relay)"
check "reverse: echo plus rc=0 is a PASS" "PASS" \
    "$(state_of "$(matrix_reverse_verdict "$INBOX" "$ME" "$NONCE")")"

inbox_reset; inbox_msg "ECHO $NONCE from @probe-asus2024 woke:typed"
inbox_msg "VERDICT $NONCE @probe-asus2024->@$ME rc=1 NOT CONFIRMED: sent for @$ME; nobody claimed it within 5s."
v="$(matrix_reverse_verdict "$INBOX" "$ME" "$NONCE")"
check "reverse: echo plus a non-zero rc is a FALSE FAILURE on that box" "FAIL" "$(state_of "$v")"
case "$(note_of "$v")" in *"FALSE FAILURE"*) r=named ;; *) r="$(note_of "$v")" ;; esac
check "reverse: the FALSE FAILURE is named" "named" "$r"

inbox_reset
check "reverse: nothing back at all is a SKIP, not a verdict" "SKIP" \
    "$(state_of "$(matrix_reverse_verdict "$INBOX" "$ME" "$NONCE")")"

inbox_reset; inbox_msg "ECHO $NONCE from @probe-asus2024 woke:typed"
inbox_msg "VERDICT $NONCE @probe-asus2024->@$ME rc=0 filed -> @$ME (by fe@kitt, relay)"
if _sot_is_windows; then
    check "reverse: a frontend filer is unremarkable on Windows" "PASS" \
        "$(state_of "$(matrix_reverse_verdict "$INBOX" "$ME" "$NONCE")")"
else
    check "reverse: a frontend filed for a box with no frontend is a FAIL" "FAIL" \
        "$(state_of "$(matrix_reverse_verdict "$INBOX" "$ME" "$NONCE")")"
fi

echo
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]

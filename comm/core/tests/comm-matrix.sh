#!/usr/bin/env bash
# comm-matrix.sh — the cross-machine comm acceptance matrix: every delivery
# direction between the named boxes, proved end to end in about a minute, so no
# comm change SHIPS IN A CANDIDATE on hermetic tests alone.
#
# IT GATES THE CANDIDATE, NOT EACH MERGE. Nothing on the fixes branch is
# installed anywhere, so a merge is proven by the hermetic suites and this
# matrix runs before the rc is cut. Requiring it per merge would block every
# comm fix on a Windows frontend being up, which is a morning-hours resource;
# requiring it per candidate catches the same regressions before anyone runs
# the code. A candidate carrying a comm change with no matrix run is not ready.
#
# Usage:
#   comm/core/tests/comm-matrix.sh --boxes kitt,asus2024,quickbeam [--expect 0.6.6-rc9.4]
#                                  [--wait SECS] [--self HOST]
#
# It runs as `probe2-<self>` and reads that row's inbox FILE directly. It starts
# no processes, holds no connection and advances no read cursor — a session's
# own poll still sees everything this saw. The responders are comm-probe.sh,
# already serving in each box's probe rows (`comm-probe.sh up` there).
#
# THREE STEPS:
#   1. Preflight — every attached frontend's version plus this hub's own, from
#      `sot-fe version`. With --expect, a box not on that version exits 2 rather
#      than letting the matrix silently measure the wrong build.
#   2. One send per line, each with its own nonce, all before any waiting.
#   3. One line per direction: PASS|FAIL|SKIP, the sender's rc, its literal
#      receipt line, and the echo latency — plus, for a direction whose echo
#      arrived, how many copies of it reached this box's frontend inbox.
#
# WHAT IT IS FOR. A sender's receipt and an actual delivery are different facts,
# and every comm outage so far has been the gap between them. This instrument
# names that gap in both directions:
#   FALSE FAILURE — the echo arrived but the sender said NOT CONFIRMED.
#   FALSE SUCCESS — the sender said filed but no echo ever came.
#   DOUBLE DELIVERY — one frame reached the frontend inbox twice, so the
#     session is woken twice and reads the same message twice (ADR 0048
#     amendment 9: two frontends on one box must file a frame once).
# Any of them is a FAIL even though the classic one-sided check would pass.
#
# Exit: the number of FAILs (0 = the matrix is clean), or 2 from preflight.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"
# shellcheck source=../scripts/comm-lib.sh
source "$SCRIPTS_DIR/comm-lib.sh"   # sot_host / sot_fe_inbox_path / INBOX_DIR

MATRIX_WAIT="${SOT_MATRIX_WAIT:-25}"     # seconds to wait for the echoes
MATRIX_POLL="${SOT_MATRIX_POLL:-0.5}"    # inbox re-read interval

# --- the decision logic (the hermetic test's whole surface) ------------------

# matrix_claimed_filed RC OUT TARGET — did the SENDER claim delivery? The claim
# is the literal receipt line, never the exit code alone: comm-send.sh prints
# "  filed -> @x" and comm-relay.sh "filed -> @x (by …, relay)", while
# "NOT CONFIRMED", "no such handle" and "ERROR: unreachable" are the three
# honest negatives. Both facts must agree; an rc 0 with no receipt line is not
# a claim anyone made.
matrix_claimed_filed() {
    local rc="$1" out="$2" target="$3"
    [ "$rc" = "0" ] || return 1
    case "$out" in *"filed -> @$target"*) return 0 ;; esac
    return 1
}

# matrix_refused_for_handle OUT — is this refusal ABOUT THE TARGET HANDLE?
# comm-relay.sh's two handle verdicts are "no such handle" (nobody can file for
# it) and "NOT CONFIRMED" (somebody might have, nobody said so). Every other
# error — an unresolved sender identity, an unreachable daemon — is about this
# END of the send and settles nothing about the other.
matrix_refused_for_handle() {
    case "$1" in
        *"no such handle"*|*"NOT CONFIRMED"*) return 0 ;;
    esac
    return 1
}

# matrix_receipt_line RC OUT — the sender's literal first line, for the report.
matrix_receipt_line() {
    printf '%s\n' "$2" | sed -n '1p'
}

# matrix_inbox_texts FILE ME [FE_FILE] — every message text addressed to ME, from
# the per-handle inbox and (on Windows) the frontend's shared fe-inbox, one per
# line. CURSOR-FREE: this reads, it never marks anything read, so the row's own
# responder still sees its mail.
matrix_inbox_texts() {
    local file="$1" me="$2" fe="${3:-}"
    [ -f "$file" ] && jq -Rr '(fromjson? // empty) | select(type == "object") | (.msg // .text // "")' \
        < "$file" 2>/dev/null
    [ -n "$fe" ] && [ -f "$fe" ] && jq -Rr --arg me "$me" \
        '(fromjson? // empty) | select(type == "object") | select((.to // "") == $me) | (.msg // .text // "")' \
        < "$fe" 2>/dev/null
    return 0
}

# matrix_find_prefix FILE ME PREFIX — the first message text beginning PREFIX
# (e.g. "ECHO <nonce>" or "VERDICT <nonce>"). rc 1 when none.
matrix_find_prefix() {
    local file="$1" me="$2" prefix="$3" fe texts hit
    fe="$(sot_fe_inbox_path)"
    # A HERE-STRING, never a pipe: `grep -m1` closes its input on the match,
    # jq takes SIGPIPE, and under `pipefail` the whole pipeline then reports a
    # failure — a found line read as "nothing arrived", which is the exact
    # false negative this instrument exists to catch.
    texts="$(matrix_inbox_texts "$file" "$me" "$fe")"
    hit="$(grep -m1 -F "$prefix" <<< "$texts")" || return 1
    [ -n "$hit" ] || return 1
    printf '%s\n' "$hit"
}

# matrix_line_verdict EXPECT RC OUT TARGET FILE ME NONCE — one direction's
# verdict, as "<PASS|FAIL> <TAB> <note>".
#
# EXPECT is `echo` (this direction must deliver) or `nodelivery` (the handle no
# row hosts — the sender must refuse, and nothing may answer).
matrix_line_verdict() {
    local expect="$1" rc="$2" out="$3" target="$4" file="$5" me="$6" nonce="$7"
    local claimed=false echoed=false
    matrix_claimed_filed "$rc" "$out" "$target" && claimed=true
    matrix_find_prefix "$file" "$me" "ECHO $nonce" >/dev/null 2>&1 && echoed=true
    case "$expect" in
        echo)
            if [ "$claimed" = true ] && [ "$echoed" = true ]; then
                printf 'PASS\tdelivered\n'
            elif [ "$claimed" = false ] && [ "$echoed" = true ]; then
                printf 'FAIL\tFALSE FAILURE — the echo arrived but the sender did not claim it\n'
            elif [ "$claimed" = true ] && [ "$echoed" = false ]; then
                printf 'FAIL\tFALSE SUCCESS — the sender claimed filed but no echo came\n'
            else
                printf 'FAIL\tno delivery — the sender refused and nothing answered\n'
            fi
            ;;
        nodelivery)
            if [ "$echoed" = true ]; then
                printf 'FAIL\tFALSE FAILURE — a handle no row hosts answered\n'
            elif [ "$claimed" = true ]; then
                printf 'FAIL\tFALSE SUCCESS — the sender claimed filed for a handle no row hosts\n'
            elif matrix_refused_for_handle "$out"; then
                printf 'PASS\trefused, as it should be\n'
            else
                # A send that never got as far as asking about the handle (an
                # unresolved identity, an unreachable daemon) proves nothing
                # about the handle. Counting it a PASS is how this line would
                # go green on a runner that could not send at all — the one
                # way a negative test lies.
                printf 'SKIP\tthe send failed before the handle was in question\n'
            fi
            ;;
        *)  printf 'FAIL\tunknown expectation %s\n' "$expect" ;;
    esac
}

# matrix_reverse_verdict FILE ME NONCE — the direction the other box sent IN,
# judged from its own VERDICT line. No sender on this end can report it.
#   echo + rc 0        the reverse leg delivered and its sender knew it
#   echo + rc non-zero FALSE FAILURE on that box
#   no echo            SKIP: the forward line already reports the failure
# A receipt filed `by fe@…` on a box with no frontend of its own is the known
# mis-attribution, so it is called out here where the literal line is in hand.
matrix_reverse_verdict() {
    local file="$1" me="$2" nonce="$3" verdict vrc
    if ! matrix_find_prefix "$file" "$me" "ECHO $nonce" >/dev/null 2>&1; then
        printf 'SKIP\tnothing came back — see the forward line\n'
        return 0
    fi
    verdict="$(matrix_find_prefix "$file" "$me" "VERDICT $nonce")" || {
        printf 'PASS\tdelivered (no VERDICT line — that box predates it)\n'
        return 0
    }
    vrc="$(printf '%s\n' "$verdict" | sed -n 's/.*[[:space:]]rc=\([0-9]*\).*/\1/p')"
    if [ "${vrc:-1}" != "0" ]; then
        printf 'FAIL\tFALSE FAILURE on that box — its echo arrived, its sender said rc=%s\n' "${vrc:-?}"
        return 0
    fi
    case "$verdict" in
        *"by fe@"*) if ! _sot_is_windows; then
                        printf 'FAIL\tfiled by a frontend (fe@) for a handle on a box that has none\n'
                        return 0
                    fi ;;
    esac
    printf 'PASS\tdelivered, and its sender said so\n'
}

# --- the runner --------------------------------------------------------------

matrix_nonce() {
    local n
    n="$(od -An -tx1 -N6 /dev/urandom 2>/dev/null | tr -d ' \n')"
    [ "${#n}" -eq 12 ] || n="$(printf '%06x%06x' "$((RANDOM * RANDOM % 16777216))" "$((RANDOM * RANDOM % 16777216))")"
    printf '%s\n' "$n"
}

# matrix_fe_inbox_copies NONCE — how many copies of the ECHO frame carrying
# NONCE are in THIS box's frontend inbox. rc 1 when this platform has no
# frontend inbox at all, which is not the same fact as "none found" and must
# not be reported as one.
#
# It counts the FRAME, not the nonce, and the key is the one the forward leg
# already uses (`matrix_find_prefix ... "ECHO $nonce"`) — one key in both legs
# so the two cannot disagree about what they are counting. A nonce count would
# be wrong on every correct fleet: the responder answers `ECHO $nonce` AND then
# sends `VERDICT $nonce ...` to the same asker (comm-probe.sh's probe_reply),
# so a direction delivers two frames carrying the nonce and a hop line three.
# No VERDICT line contains this substring.
matrix_fe_inbox_copies() {
    local fe n
    fe="$(sot_fe_inbox_path)"
    [ -n "$fe" ] || return 1
    [ -r "$fe" ] || { printf '0\n'; return 0; }
    n="$(grep -c -F -- "ECHO $1" "$fe" 2>/dev/null)" || n=0
    [[ "$n" =~ ^[0-9]+$ ]] || n=0
    printf '%s\n' "$n"
}

# matrix_fe_dupe_verdict NONCE FRONTENDS — the two-frontend leg (ADR 0048
# amendment 9), as "<PASS|FAIL|SKIP> <TAB> <note>". Counts the ECHO frame
# carrying NONCE, never the nonce itself — see matrix_fe_inbox_copies.
#
# The daemon fans every relayed frame out to EVERY attached connection, and each
# attached frontend appends into one inbox whose path has no per-frontend
# component. One copy is the whole rule. Two is a failure in the same way a
# false success is, and for the same reason: this instrument exists to name the
# gap between what was claimed and what happened, and a session woken twice by
# one message is that gap.
#
# TWO SKIPS, and they are DIFFERENT. A platform whose readers never open that
# file has nothing to count — the leg is not applicable there, and reporting it
# as a pass would claim a proof nobody performed. A box with fewer than two
# frontends cannot produce the defect at all. Collapsing the two would let "this
# platform has no such file" masquerade as "this box has one frontend".
#
# FRONTENDS is the preflight's count of named frontend clients for this box. The
# roster prints no instance, so two frontends on one box are two identical
# `fe@<box>` lines and their COUNT is the only available signal; it is printed
# on the skip so that a future roster change folding duplicates into one row
# with a multiplier cannot make this leg skip silently.
matrix_fe_dupe_verdict() {
    local nonce="$1" frontends="${2:-0}" count
    if ! count="$(matrix_fe_inbox_copies "$nonce")"; then
        printf 'SKIP\tno frontend inbox on this platform — nothing here reads that file\n'
        return 0
    fi
    if [ "$frontends" -lt 2 ]; then
        printf 'SKIP\t%s frontend(s) counted for this box — one cannot produce the defect (copies seen: %s)\n' \
            "$frontends" "$count"
        return 0
    fi
    case "$count" in
        1) printf 'PASS\tone copy of the echo in the frontend inbox, from %s frontends\n' "$frontends" ;;
        0) printf 'SKIP\tnot delivered through the frontend inbox (a local registry hit files directly)\n' ;;
        *) printf 'FAIL\t%s copies of the echo in the frontend inbox — %s frontends each filed the same frame\n' \
               "$count" "$frontends" ;;
    esac
}

matrix_preflight() {
    local expect="$1" boxes="$2" self="${3:-}" out box line ver bad=0
    echo "== preflight: attached frontends and this hub =="
    out="$("$SCRIPTS_DIR/sot-fe" version 2>&1)" || {
        echo "FATAL: \`sot-fe version\` failed:" >&2
        printf '%s\n' "$out" >&2
        exit 2
    }
    printf '%s\n' "$out" | grep -E '^(fe@|unix:|tcp:|pipe:|npipe:)' || true
    # How many named frontends this box has, for the two-frontend leg below.
    # `local` is deliberately absent: the verdict helper reads it per line.
    MATRIX_FE_LOCAL="$(printf '%s\n' "$out" | grep -c "^fe@$self[[:space:]]")" || MATRIX_FE_LOCAL=0
    [[ "$MATRIX_FE_LOCAL" =~ ^[0-9]+$ ]] || MATRIX_FE_LOCAL=0
    echo "frontends attached for $self: $MATRIX_FE_LOCAL"
    [ -n "$expect" ] || { echo; return 0; }
    for box in ${boxes//,/ }; do
        line="$(printf '%s\n' "$out" | grep -m1 "^fe@$box[[:space:]]")" || line=""
        if [ -z "$line" ]; then
            echo "PREFLIGHT FAIL: no frontend attached for $box — nothing can say which build it runs" >&2
            bad=$((bad + 1)); continue
        fi
        ver="$(printf '%s\n' "$line" | awk '{print $2}')"
        [ "$ver" = "$expect" ] || {
            echo "PREFLIGHT FAIL: $box is on $ver, expected $expect" >&2
            bad=$((bad + 1))
        }
    done
    [ "$bad" -eq 0 ] || exit 2
    echo
}

# Runs only when executed, not sourced (the hermetic test sources this for the
# decision helpers above and must not send anything).
[ "${BASH_SOURCE[0]}" = "${0}" ] || return 0

BOXES=""; EXPECT=""; SELF=""
while [ $# -gt 0 ]; do
    case "$1" in
        --boxes)  BOXES="$2"; shift 2 ;;
        --expect) EXPECT="$2"; shift 2 ;;
        --wait)   MATRIX_WAIT="$2"; shift 2 ;;
        --self)   SELF="$2"; shift 2 ;;
        -h|--help) sed -n '2,30p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "usage: comm-matrix.sh --boxes a,b,c [--expect VERSION] [--wait SECS] [--self HOST]" >&2; exit 1 ;;
    esac
done

[ -n "$BOXES" ] || { echo "usage: comm-matrix.sh --boxes a,b,c [--expect VERSION]" >&2; exit 1; }
ensure_home
[ -n "$SELF" ] || SELF="$(sot_host)" || exit 1
ME="probe2-$SELF"
INBOX="$INBOX_DIR/$ME.jsonl"

# Every send goes out AS the probe row, never as whatever session ran this —
# and BEING that row is not a matter of setting a variable. comm-relay.sh
# refuses to send with an identity it cannot route (`sot_require_routable_identity`),
# which means a self file naming the handle AND a cwd matching the root that
# handle is registered to. So the runner adopts the row's own self file and
# moves into the row's own root. It has to be this row and no other: a handle
# with no declared row on this box has nothing to file a remote box's reply,
# so a private runner handle would make every inbound line fail for a reason
# that has nothing to do with the path under test.
matrix_self_file() {  # HANDLE -> path of the self file that names it
    local f
    for f in "$COMM_HOME/self"/*; do
        [ -f "$f" ] || continue
        [ "$(sed -n '1p' "$f" 2>/dev/null)" = "$1" ] || continue
        printf '%s\n' "$f"; return 0
    done
    return 1
}
SELF_FILE_PATH="$(matrix_self_file "$ME")" || {
    echo "FATAL: no joined identity for @$ME on this box — run \`comm-probe.sh up\` here first" >&2
    exit 2
}
MY_ROOT="$(sed -n '3p' "$SELF_FILE_PATH" 2>/dev/null)"; MY_ROOT="${MY_ROOT#root=}"
[ -n "$MY_ROOT" ] && [ -d "$MY_ROOT" ] || {
    echo "FATAL: @$ME's self file ($SELF_FILE_PATH) names no usable root" >&2
    exit 2
}
export SOT_COMM_SELF_FILE="$SELF_FILE_PATH"
cd "$MY_ROOT" || exit 2

MATRIX_FE_LOCAL=0
matrix_preflight "$EXPECT" "$BOXES" "$SELF"

# The lines. Each is: NAME | TARGET | HOP | EXPECT. A hop is how a box is made
# to send to a THIRD row: the responder forwards the probe, and the row it
# forwards to echoes home — which is the only way to measure a direction this
# box is not an endpoint of.
L_NAME=(); L_TARGET=(); L_HOP=(); L_EXPECT=(); L_REVERSE=()
add_line() { L_NAME+=("$1"); L_TARGET+=("$2"); L_HOP+=("$3"); L_EXPECT+=("$4"); L_REVERSE+=("$5"); }

REMOTES=()
for b in ${BOXES//,/ }; do [ "$b" = "$SELF" ] || REMOTES+=("$b"); done

add_line "$SELF->$SELF"            "probe-$SELF"  ""                 echo       ""
for b in "${REMOTES[@]}"; do
    add_line "$SELF->$b"           "probe-$b"     ""                 echo       "$b->$SELF"
    add_line "$b->$b (two inbox)"  "probe-$b"     "probe2-$b"        echo       ""
done
for a in "${REMOTES[@]}"; do
    for b in "${REMOTES[@]}"; do
        [ "$a" = "$b" ] && continue
        add_line "$a->$b"          "probe-$a"     "probe2-$b"        echo       ""
    done
done
# The handle no row hosts. It begins `probe`, so this can never poke a real one.
add_line "$SELF->nosuchrow"        "probe-nosuchrow-$SELF" ""        nodelivery ""

echo "== $((${#L_NAME[@]})) lines as @$ME =="
L_NONCE=(); L_RC=(); L_OUT=(); L_SENT=()
for i in "${!L_NAME[@]}"; do
    nonce="$(matrix_nonce)"
    if [ -n "${L_HOP[$i]}" ]; then
        payload="PROBE $nonce reply @$ME hop @${L_HOP[$i]}"
    else
        payload="PROBE $nonce reply @$ME"
    fi
    rc=0
    out="$("$SCRIPTS_DIR/comm-relay.sh" send "@${L_TARGET[$i]}" "$payload" 2>&1)" || rc=$?
    L_NONCE+=("$nonce"); L_RC+=("$rc"); L_OUT+=("$out"); L_SENT+=("$(date +%s%N)")
done

# One wait for all of them, not one per line: the lines are independent and the
# slowest is the bound.
deadline=$(( $(date +%s) + MATRIX_WAIT ))
L_SEEN=(); for i in "${!L_NONCE[@]}"; do L_SEEN+=(""); done
while [ "$(date +%s)" -lt "$deadline" ]; do
    missing=0
    for i in "${!L_NONCE[@]}"; do
        [ -z "${L_SEEN[$i]}" ] || continue
        if matrix_find_prefix "$INBOX" "$ME" "ECHO ${L_NONCE[$i]}" >/dev/null 2>&1; then
            L_SEEN[$i]="$(date +%s%N)"
        elif [ "${L_EXPECT[$i]}" = "echo" ]; then
            missing=$((missing + 1))
        fi
    done
    [ "$missing" -eq 0 ] && break
    sleep "$MATRIX_POLL"
done

FAILS=0
for i in "${!L_NAME[@]}"; do
    verdict="$(matrix_line_verdict "${L_EXPECT[$i]}" "${L_RC[$i]}" "${L_OUT[$i]}" "${L_TARGET[$i]}" \
        "$INBOX" "$ME" "${L_NONCE[$i]}")"
    state="${verdict%%	*}"; note="${verdict#*	}"
    lat="-"
    [ -z "${L_SEEN[$i]}" ] || lat="$(( ( ${L_SEEN[$i]} - ${L_SENT[$i]} ) / 1000000 ))ms"
    printf '%-4s %-26s rc=%s  %-8s %s\n' "$state" "${L_NAME[$i]}" "${L_RC[$i]}" "$lat" \
        "$(matrix_receipt_line "${L_RC[$i]}" "${L_OUT[$i]}")"
    printf '       %s\n' "$note"
    [ "$state" = "FAIL" ] && FAILS=$((FAILS + 1))
    # Once a direction's echo is found, ask the second question this box can
    # answer alone: did the frame land in the frontend inbox exactly once?
    if [ "${L_EXPECT[$i]}" = "echo" ] && [ -n "${L_SEEN[$i]}" ]; then
        verdict="$(matrix_fe_dupe_verdict "${L_NONCE[$i]}" "$MATRIX_FE_LOCAL")"
        state="${verdict%%	*}"; note="${verdict#*	}"
        printf '%-4s %-26s\n' "$state" "${L_NAME[$i]} (fe inbox)"
        printf '       %s\n' "$note"
        [ "$state" = "FAIL" ] && FAILS=$((FAILS + 1))
    fi
    if [ -n "${L_REVERSE[$i]}" ]; then
        verdict="$(matrix_reverse_verdict "$INBOX" "$ME" "${L_NONCE[$i]}")"
        state="${verdict%%	*}"; note="${verdict#*	}"
        printf '%-4s %-26s      %-8s %s\n' "$state" "${L_REVERSE[$i]}" "" \
            "$(matrix_find_prefix "$INBOX" "$ME" "VERDICT ${L_NONCE[$i]}" || echo '(no VERDICT line)')"
        printf '       %s\n' "$note"
        [ "$state" = "FAIL" ] && FAILS=$((FAILS + 1))
    fi
done

echo
echo "$FAILS FAIL(s)"
exit "$FAILS"

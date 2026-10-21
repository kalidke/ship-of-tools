#!/usr/bin/env bash
# comm-matrix.sh — the cross-machine comm acceptance matrix: every delivery
# direction between the named boxes, proved end to end in about a minute, so no
# comm change SHIPS IN A CANDIDATE on hermetic tests alone.
#
# IT GATES THE CANDIDATE, NOT EACH MERGE. Nothing on main is installed
# anywhere, so a merge is proven by the hermetic suites and this matrix runs
# before the rc is cut. Requiring it per merge would block every
# comm fix on a Windows frontend being up, which is a morning-hours resource;
# requiring it per candidate catches the same regressions before anyone runs
# the code. A candidate carrying a comm change with no matrix run is not ready.
#
# Usage:
#   comm/tests/comm-matrix.sh --boxes <a,b,c> [--expect 0.6.6-rc9.4]
#                              [--wait SECS] [--self HOST]
#
# It runs as a session of its own, `probe-matrix-<self>`, joined from outside any
# row and gone at exit, and reads that handle's inbox FILE directly. It reads no
# row's inbox and advances no row's read cursor. The responders are comm-probe.sh,
# already serving in each box's probe rows: `comm-probe.sh up` there exits 0
# only once both rows have joined again after its start line. A box whose `up`
# did not has a probe that may not answer, which says nothing about the route.
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
#   FALSE FAILURE — the echo arrived but the sender said FAILED.
#   FALSE SUCCESS — the sender said filed but no echo ever came.
# Any of them is a FAIL even though the classic one-sided check would pass.
#
# Exit: the number of FAILs (0 = the matrix is clean), or 2 from preflight.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../lib/comm-lib.sh" || exit 2   # sot_host / INBOX_DIR

MATRIX_WAIT="${SOT_MATRIX_WAIT:-25}"     # seconds to wait for the echoes
MATRIX_POLL="${SOT_MATRIX_POLL:-0.5}"    # inbox re-read interval

# --- the decision logic (the hermetic test's whole surface) ------------------

# matrix_claimed_filed RC OUT TARGET — did the SENDER claim delivery? The claim
# is the literal receipt line, never the exit code alone: comm-send.sh prints
# "  filed -> @x" and comm-relay.sh "filed -> @x", while "FAILED -> @x: …" is
# the honest negative. Both facts must agree; an rc 0 with no receipt line is
# not a claim anyone made.
matrix_claimed_filed() {
    local rc="$1" out="$2" target="$3"
    [ "$rc" = "0" ] || return 1
    case "$out" in *"filed -> @$target"*) return 0 ;; esac
    return 1
}

# matrix_refused_for_handle OUT — is this refusal ABOUT THE TARGET HANDLE?
# A "FAILED -> @x: …" line carries the daemon's own answer about the handle.
# The no-answer FAILED line, an unresolved sender identity (bare or as a FAILED
# line), and every other error are about this END of the send and settle nothing
# about the other.
matrix_refused_for_handle() {
    case "$1" in
        *"FAILED -> @"*": the daemon did not answer at "*) return 1 ;;
        *"FAILED -> @"*": your sot-comm identity did not resolve"*) return 1 ;;
        *"FAILED -> @"*) return 0 ;;
    esac
    return 1
}

# matrix_receipt_line RC OUT — the sender's literal first line, for the report.
matrix_receipt_line() {
    printf '%s\n' "$2" | sed -n '1p'
}

# matrix_inbox_texts FILE — every message text in the per-handle inbox, one per
# line. CURSOR-FREE: this reads, it never marks anything read, so the row's own
# responder still sees its mail.
matrix_inbox_texts() {
    local file="$1"
    [ -f "$file" ] && jq -Rr '(fromjson? // empty) | select(type == "object") | (.msg // .text // "")' \
        < "$file" 2>/dev/null
    return 0
}

# matrix_find_prefix FILE ME PREFIX — the first message text beginning PREFIX
# (e.g. "ECHO <nonce>" or "VERDICT <nonce>"). rc 1 when none.
matrix_find_prefix() {
    local file="$1" me="$2" prefix="$3" texts hit
    # A HERE-STRING, never a pipe: `grep -m1` closes its input on the match,
    # jq takes SIGPIPE, and under `pipefail` the whole pipeline then reports a
    # failure — a found line read as "nothing arrived", which is the exact
    # false negative this instrument exists to catch.
    texts="$(matrix_inbox_texts "$file")"
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
    printf 'PASS\tdelivered, and its sender said so\n'
}

# --- the runner --------------------------------------------------------------

matrix_nonce() {
    local n
    n="$(od -An -tx1 -N6 /dev/urandom 2>/dev/null | tr -d ' \n')"
    [ "${#n}" -eq 12 ] || n="$(printf '%06x%06x' "$((RANDOM * RANDOM % 16777216))" "$((RANDOM * RANDOM % 16777216))")"
    printf '%s\n' "$n"
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

# matrix_runner_join DIR — the runner is a session of its own, outside any row (PROTOCOL.md's row rule): it joins
# through the private self file the caller pinned (SOT_COMM_SELF_FILE, which names no row) from DIR/probe-matrix, so
# its derived handle, probe-matrix-<host>, starts with `probe` and the responders answer it, and the entry binds no
# row. A probe row's entry binds its row, so the runner cannot adopt that row's identity; it reads and sends as itself.
# Prints the handle; the join's own lines go to stderr. The caller moves into DIR/probe-matrix.
matrix_runner_join() {  # DIR
    mkdir -p "$1/probe-matrix" || return 1
    ( cd "$1/probe-matrix" && "$SCRIPTS_DIR/comm-join.sh" >&2 && eval "$("$SCRIPTS_DIR/comm-context.sh")" \
        && [ -n "${NAME:-}" ] && printf '%s\n' "$NAME" )
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
sot_comm ensure-home
[ -n "$SELF" ] || SELF="$(sot_host)" || exit 1

# Every send goes out as the runner's own handle, which starts with `probe` so the responders (comm-probe.sh's
# probe_handle_ok) reply and hop to it with no change. The runner is no row's session, so comm-relay.sh routes it
# (`sot_require_routable_identity`) from its own root, and a remote box files its replies to it by the hub's folder
# listing (`comm_file_verdict`): nothing here needs a row to be declared for it.
MATRIX_PRIV_DIR="$(mktemp -d)" || { echo "FATAL: mktemp failed" >&2; exit 2; }
SCRIPTS_DIR="$MATRIX_PRIV_DIR/bin"
# The leave is the runner's own, and only once its private self file is pinned: with no pin it would remove the
# identity of whatever session ran this.
trap '[ "${SOT_COMM_SELF_FILE:-}" != "$MATRIX_PRIV_DIR/matrix-self.txt" ] || "$SCRIPTS_DIR/comm-leave.sh" > /dev/null 2>&1; rm -rf "${MATRIX_PRIV_DIR:?}"' EXIT
bash "$SCRIPT_DIR/stage-bin.sh" "$SCRIPTS_DIR" || exit 2
export SOT_COMM_SELF_FILE="$MATRIX_PRIV_DIR/matrix-self.txt"
ME="$(matrix_runner_join "$MATRIX_PRIV_DIR" 2> "$MATRIX_PRIV_DIR/join.out")" || {
    echo "FATAL: the runner could not join: $(tail -n 1 "$MATRIX_PRIV_DIR/join.out")" >&2
    exit 2
}
INBOX="$INBOX_DIR/$ME.jsonl"
cd "$MATRIX_PRIV_DIR/probe-matrix" || exit 2

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
add_line "$SELF->$SELF (two inbox)" "probe-$SELF" "probe2-$SELF"     echo       ""
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

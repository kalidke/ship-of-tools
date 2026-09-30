#!/usr/bin/env bash
# test-registry-lock-twohost.sh — the registry lock (B1b) across boxes that
# mount one home. What a single box cannot show:
#
#   (m) every box's /etc/machine-id is distinct (the proof of death assumes a
#       machine-id is unique to one machine);
#   (a) the fresh read on the real mounts: per round the reader primes its
#       cache with a plain stat, the other box replaces the lock, the reader's
#       plain stat is the control, then its fresh read runs. Every fresh read
#       must show the new record, and the proof counts only if the control was
#       stale in at least one round. --rounds N (default 20) with the shell
#       arm and N with the Rust arm reading here; with --v3-host, N more with
#       the shell arm reading on that host (the Rust arm is not built there).
#       `--v3-host HOST --only a-v3` runs that last case alone;
#   (b) a holder here frozen: the peer FAILs "another machine" naming it; a
#       holder here killed: the peer FAILs, then a waiter here reclaims;
#   (k) a holder killed on the peer: a writer here FAILs with the exact line
#       naming it and the recovery, the next comm call on the peer clears it,
#       and the writer here then succeeds;
#   (c) 100 rounds of two waiters here and two on the other box against a
#       holder killed here: one holder at a time, and reclaim.<D> made. A
#       second marker is legitimate: a waiter that judges the reclaimer after
#       it has finished and exited takes reclaim.<reclaimer>, then declines to
#       act on its re-read. Every marker a round makes must be made by one of
#       its waiters and name D or one of them. Run against the peer, and again
#       against --v3-host.
#
# The working folder is fresh, under $HOME (every box sees it), holds a COPY of
# this tree's comm-lib.sh, and is removed on exit; it never touches ~/.sot-comm
# or a live daemon. Needs real boxes, so it runs in no workflow. Requires cargo.
#
# Usage: comm/core/tests/test-registry-lock-twohost.sh --peer HOST [--v3-host HOST] [--rounds N]
#        comm/core/tests/test-registry-lock-twohost.sh --v3-host HOST --only a-v3 [--rounds N]
# Exit: 0 all pass, 1 any fail, 2 usage.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home

PEER=""; V3=""; ROUNDS=20; ONLY=""
while [ $# -gt 0 ]; do
    case "$1" in
        --peer) PEER="${2:-}"; shift 2 || shift ;;
        --v3-host) V3="${2:-}"; shift 2 || shift ;;
        --rounds) ROUNDS="${2:-}"; shift 2 || shift ;;
        --only) ONLY="${2:-}"; shift 2 || shift ;;
        *) break ;;
    esac
done
if ! [[ "$ROUNDS" =~ ^[1-9][0-9]*$ ]] || { [ "$ONLY" != a-v3 ] && { [ -n "$ONLY" ] || [ -z "$PEER" ]; }; } \
    || { [ "$ONLY" = a-v3 ] && [ -z "$V3" ]; }; then
    echo "usage: test-registry-lock-twohost.sh --peer HOST [--v3-host HOST] [--rounds N]" >&2
    echo "       test-registry-lock-twohost.sh --v3-host HOST --only a-v3 [--rounds N]" >&2
    exit 2
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"
RUST_DIR="$(cd "$SCRIPT_DIR/../../../rust" && pwd)"
DIR="$(mktemp -d "$HOME/.sot-registry-lock-XXXXXX")" || { echo "FATAL: mktemp under \$HOME failed" >&2; exit 1; }
guard_refuse_live_home "$DIR"
LOCAL_PIDS=()
cleanup() {
    local p h
    for p in "${LOCAL_PIDS[@]}"; do kill -CONT "$p" 2>/dev/null; kill -9 "$p" 2>/dev/null; done
    # The pattern's first '.' is written [.], so it never matches its own shell.
    for h in $PEER $V3; do on "$h" "pkill -9 -f $(printf %q "${DIR/./[.]}/lib/")" 2>/dev/null; done
    rm -rf "${DIR:?}"
}
trap cleanup EXIT
mkdir -p "$DIR/lib"
cp "$SCRIPTS_DIR/comm-lib.sh" "$DIR/lib/comm-lib.sh"
LIB="$DIR/lib/comm-lib.sh"

on() { local h="$1"; shift; ssh -o BatchMode=yes -o ConnectTimeout=5 "$h" "$@"; }
# $1 = host ("" for here), $2 = comm home, $3 = bash body with the lib sourced.
sh_at() {
    local body=". $(printf %q "$LIB"); $3"
    if [ -z "$1" ]; then
        SOT_COMM_HOME="$2" SOT_COMM_TEST_LOCK_SETTLE="${SETTLE:-1}" bash -c "$body"
    else
        on "$1" "SOT_COMM_HOME=$(printf %q "$2") SOT_COMM_TEST_LOCK_SETTLE=${SETTLE:-1} bash -c $(printf %q "$body")"
    fi
}
new_case() { local c="$DIR/$1"; mkdir -p "$c"; printf '%s' "$c"; }
field() { cut -d: -f"$2" <<<"$1"; }

PASS=0; FAIL=0
verdict() {  # $1 = description, $2 = "" for pass or the reason
    if [ -z "$2" ]; then echo "PASS: $1"; PASS=$((PASS + 1)); else echo "FAIL: $1 — $2"; FAIL=$((FAIL + 1)); fi
}

# (m), skipped by --only
if [ -z "$ONLY" ]; then
    ids="$(cat /etc/machine-id)"
    for h in "$PEER" $V3; do ids="$ids"$'\n'"$(on "$h" cat /etc/machine-id)"; done
    n="$(printf '%s\n' "$ids" | grep -c .)"; u="$(printf '%s\n' "$ids" | sort -u | grep -c .)"
    verdict "(m) the $n machine-ids are distinct" "$([ "$n" = "$u" ] || echo "$u distinct of $n")"
    for h in "" "$PEER" $V3; do
        echo "  mount${h:+ on the other box}: $(sh_at "$h" "$DIR" 'findmnt -n -o FSTYPE,OPTIONS -T "$SOT_COMM_HOME" | cut -c1-60')"
    done
fi

# (a) $1 reader host ("" = here), $2 replacer host, $3 arm (shell|rust), $4 rounds.
cached_read() {
    local reader="$1" replacer="$2" arm="$3" rounds="$4" c i old new ino0 ino1 got stale=0 wrong=0
    c="$(new_case "a-$arm-${reader:-here}")"
    for i in $(seq "$rounds"); do
        old="r:-:-:-:1:old$i"; new="r:-:-:-:2:new$i"
        printf '%s\n' "$old" > "$c/t"; rm -f "${c:?}/.registry.lock"; link "$c/t" "$c/.registry.lock"; rm -f "${c:?}/t"
        ino0="$(sh_at "$reader" "$c" 'stat -c %i "$SOT_COMM_HOME/.registry.lock"; cat "$SOT_COMM_HOME/.registry.lock" >/dev/null')"
        sh_at "$replacer" "$c" "rm -f \"\${SOT_COMM_HOME:?}/.registry.lock\"; printf '%s\n' '$new' > \"\$SOT_COMM_HOME/t2\"; link \"\$SOT_COMM_HOME/t2\" \"\$SOT_COMM_HOME/.registry.lock\"; rm -f \"\${SOT_COMM_HOME:?}/t2\""
        ino1="$(sh_at "$reader" "$c" 'stat -c %i "$SOT_COMM_HOME/.registry.lock" 2>/dev/null')"
        [ "$ino0" = "$ino1" ] && stale=$((stale + 1))
        if [ "$arm" = shell ]; then
            got="$(sh_at "$reader" "$c" '_sot_lock_self_id; _sot_lock_fresh "$SOT_COMM_HOME/.registry.lock"; echo "$_SOT_LOCK_READ"')"
        else
            got="$( (cd "$RUST_DIR" && SOT_TEST_LOCK_PATH="$c/.registry.lock" cargo test -q -p sot-backend --bin sotd \
                comm_registry_lock::tests::print_fresh -- --ignored --exact --nocapture 2>/dev/null) | sed -n 's/^FRESH //p')"
        fi
        [ "$got" = "$new" ] || wrong=$((wrong + 1))
    done
    verdict "(a) $arm fresh read${reader:+ on the other box}: $rounds rounds, control stale in $stale" \
        "$([ "$wrong" = 0 ] || echo "$wrong stale fresh reads")$([ "$stale" -gt 0 ] || echo "cache never shown")"
}
if [ "$ONLY" = a-v3 ]; then
    cached_read "$V3" "" shell "$ROUNDS"
    echo "---"
    echo "$PASS passed, $FAIL failed"
    [ "$FAIL" = 0 ]
    exit
fi

cached_read "" "$PEER" shell "$ROUNDS"
cached_read "" "$PEER" rust "$ROUNDS"
[ -z "$V3" ] || cached_read "$V3" "" shell "$ROUNDS"

# (b) and (k)
SETTLE=0.2
c="$(new_case b)"
sh_at "" "$c" 'hold() { echo $BASHPID; kill -STOP $BASHPID; }; with_lock hold' > "$DIR/b.out" &
for i in $(seq 100); do [ -s "$DIR/b.out" ] && break; sleep 0.05; done
hp="$(cat "$DIR/b.out")"; LOCAL_PIDS+=("$hp")
err="$(sh_at "$PEER" "$c" 'SOT_LOCK_WAIT_SECS=0.2 with_lock true' 2>&1)"; rc=$?
verdict "(b) a frozen holder here: the peer FAILs naming it, another machine" \
    "$([ "$rc" != 0 ] && [[ "$err" == *"pid $hp start "*"it is on another machine"* ]] || echo "rc=$rc: $err")"
kill -9 "$hp"; wait 2>/dev/null
err="$(sh_at "$PEER" "$c" 'SOT_LOCK_WAIT_SECS=0.2 with_lock true' 2>&1)"; rc=$?
sh_at "" "$c" 'with_lock true'; rc2=$?
verdict "(b) a killed holder here: the peer FAILs, then a waiter here reclaims" \
    "$([ "$rc" != 0 ] && [ "$rc2" = 0 ] && [ ! -e "$c/.registry.lock" ] || echo "peer rc=$rc here rc=$rc2")"

c="$(new_case k)"
sh_at "$PEER" "$c" 'with_lock kill -9 $BASHPID' 2>/dev/null
rec="$(cat "$c/.registry.lock")"
err="$(sh_at "" "$c" 'SOT_LOCK_WAIT_SECS=0.2 with_lock true' 2>&1)"; rc=$?
want="ERROR: registry lock $c/.registry.lock is held by $(field "$rec" 1) pid $(field "$rec" 5) start $(field "$rec" 6) (AGE old): it is on another machine. If it is dead, run any comm command on $(field "$rec" 1), or run comm-registry-lock-clear.sh."
got="$(sed -E 's/\((-?[0-9]+s|unknown) old\)/(AGE old)/' <<<"$err")"
verdict "(k) a holder killed on the peer: the writer here FAILs with the exact line" "$([ "$rc" != 0 ] && [ "$got" = "$want" ] || printf 'rc=%s\n  got:  %s\n  want: %s' "$rc" "$got" "$want")"
echo "  $err"
sh_at "$PEER" "$c" 'SOT_LOCK_WAIT_SECS=1 with_lock true'; rc=$?
sh_at "" "$c" 'with_lock true'; rc2=$?
verdict "(k) the next comm call on the peer clears it, then the writer here succeeds" "$([ "$rc" = 0 ] && [ "$rc2" = 0 ] || echo "peer rc=$rc here rc=$rc2")"

# (c) $1 = the other box.
race() {
    local other="$1" c round w m ids bad="" crit
    c="$(new_case "c-$other")"
    # Each waiter writes its own ID to id.<n> in the process that then waits.
    crit="crit() { mkdir '$c/cs' || echo OVERLAP; sleep 0.02; rmdir '$c/cs'; echo held; }
        wait_as() { _sot_lock_self_id; echo \"\$_SOT_LOCK_ID\" > '$c/id.'\$1; with_lock crit; }"
    for round in $(seq 100); do
        sh_at "" "$c" 'with_lock kill -9 $BASHPID' 2>/dev/null; cp "$c/.registry.lock" "$c/d"
        ls -A "$c" | grep '^\.registry\.lock\.reclaim\.' | sort > "$DIR/c-before"
        rm -f "${c:?}"/id.*
        sh_at "$other" "$c" "$crit; wait_as r1 & wait_as r2 & wait" > "$DIR/c-remote" 2>&1 &
        local rp=$!
        for w in 1 2; do sh_at "" "$c" "$crit; wait_as $w" > "$DIR/c-$w" 2>&1 & done
        wait
        [ "$(cat "$DIR/c-1" "$DIR/c-2" "$DIR/c-remote" | sort | uniq -c | tr -s ' ')" = " 4 held" ] \
            || { bad="round $round: $(cat "$DIR/c-1" "$DIR/c-2" "$DIR/c-remote" | tr '\n' ' ')"; break; }
        [ -e "$c/.registry.lock.reclaim.$(tr : . < "$c/d")" ] || { bad="round $round: no marker for the dead holder"; break; }
        ids="$(cat "$c"/id.*)"
        for m in $(ls -A "$c" | grep '^\.registry\.lock\.reclaim\.' | sort | comm -13 "$DIR/c-before" -); do
            grep -qxF "$(cat "$c/$m")" <<<"$ids" || { bad="round $round: $m was made by no waiter of the round"; break 2; }
            [ "$m" = ".registry.lock.reclaim.$(tr : . < "$c/d")" ] || grep -qxF "${m#.registry.lock.reclaim.}" <<<"${ids//:/.}" \
                || { bad="round $round: $m names neither D nor a waiter of the round"; break 2; }
        done
        wait "$rp" 2>/dev/null
    done
    verdict "(c) 100 rounds, two waiters here and two on the other box, a holder killed here" "$bad"
}
race "$PEER"
[ -z "$V3" ] || race "$V3"

echo "---"
echo "$PASS passed, $FAIL failed"
[ "$FAIL" = 0 ]

#!/usr/bin/env bash
# test-registry-lock.sh — the registry lock (B1b): a file naming its holder,
# made by link(2), and reclaimed only when its holder is proved dead on this
# machine. One Linux box, hermetic.
#
#   1 a SIGKILLed holder is reclaimed: the waiter holds in under 3 s, the
#     marker reclaim.<D> holds the waiter's ID, registry.json still parses;
#   2 a live holder is never reclaimed: FAILED names it and "it is running",
#     the lock is byte-identical, there is no marker, and the record's pid is
#     the process that holds (the ID is computed there, never in a subshell);
#   3 a frozen holder gives FAILED and later releases; a foreign machine, and
#     the same machine-id under another host name, give FAILED "another
#     machine"; the same machine and name with another boot is reclaimed;
#   4 the race: 100 rounds, 3 waiters on one dead holder, one holder at a time,
#     reclaim.<D> made each round (a waiter that judges the reclaimer after it
#     exited may add reclaim.<reclaimer>, then declines to act), and every
#     marker a round makes was made by one of its waiters and names D or one;
#   5 a marker held by a frozen reclaimer gives FAILED naming the holder and
#     either true reason, a marker held by a dead one is reclaimed through
#     reclaim.<R>, both markers kept;
#   6 the mixed rollout: an older peer's mkdir lock gives FAILED "older
#     version" and is left as it was, with nothing linked into it; the older
#     mkdir waiter waits on a file lock and proceeds after its release;
#   7 a zero wait (the heartbeat) still reclaims a dead holder, and gives up
#     at once on a live one;
#   8 the touches: a send against a killed holder reclaims and files; against
#     a live holder it returns in about 1 s, exit 0, filed; comm-status
#     against the same live holder waits its full 10 s and FAILs;
#   9 the clear command removes an unprovable holder's lock through its
#     marker, refuses a holder this box proves alive, refuses a marker held by
#     a reclaimer it cannot prove dead (a frozen one "on another machine")
#     leaving lock and marker as they were, tells a person to remove the lock
#     by hand only when that reclaimer's record has no proof fields, and says
#     "free", exit 0, for no lock;
#  10 the bound is time: a touch with 0.5 s tries against a live holder ends
#     within its 1 s deadline plus one try;
#  11 a home without hard links FAILs naming the cause and leaves nothing;
#  12 a zero wait whose retake after a reclaim fails makes one step and that
#     retake, and FAILs at once: nothing chains (review SF2);
#  13 the clock is EPOCHREALTIME's digits under a comma decimal, and an unset
#     or non-numeric one is FAILED naming the clock, never the holder;
#  14 a dead holder D whose marker reclaim.<D> names D: the next writer and
#     the clear each stop at that marker within their bound, say to remove
#     the lock by hand, and leave lock and marker as they were (review B1);
#  15 an ID with no proof fields is never judged mine (review SF1), and no
#     call site compares a record to the own ID but through the one test;
#  16 a clear forcing a proof-less holder whose lock a new holder N takes
#     during the settle names N, and never says N has no proof fields or to
#     remove the lock by hand (review SF2);
#  17 a zero wait whose retake fails and whose fresh read finds the lock gone
#     FAILs a free lock, never one to remove by hand (review note 3).
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-registry-lock-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "FATAL: mktemp failed" >&2; exit 1; }
export SOT_COMM_HOME="$WORK/home"
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
SCRIPTS_DIR="$(guard_stage_bin "$WORK")" || exit 2
mkdir -p "$SOT_COMM_HOME/inbox"
PIDS=()
trap 'touch "$WORK/go" 2>/dev/null; for p in "${PIDS[@]}"; do kill -CONT "$p" 2>/dev/null; kill -9 "$p" 2>/dev/null; done; rm -rf "${WORK:?}"' EXIT

BIN="$WORK/bin"
cp -r "$SCRIPTS_DIR" "$BIN"
cat >> "$BIN/comm-lib.sh" <<'STUB'

# ---- no daemon, a fixture inbox mount (test only) ----------------------------
sot_daemon_endpoint() { return 1; }
sot_relay_endpoint() { [ -n "${1:-}" ] || return 1; printf '%s\n' "$1"; }
_sot_findmnt() { printf '%s\n' "nfs4 rw,vers=4.2,local_lock=none filer.example:/export/home"; }
_sot_machine_id() { printf '0123456789abcdef0123456789abcdef'; }
STUB
printf '%s\n' "nfs4 filer.example:/export/home" > "$SOT_COMM_HOME/inbox-lock-manager"
LIB="$BIN/comm-lib.sh"
P="$SOT_COMM_HOME/.registry.lock"
export SOT_COMM_TEST_LOCK_SETTLE=0.05

PASS=0; FAIL=0
check() {  # run in this shell, not a subshell, so PIDS reaches the EXIT trap
    local desc="$1" fn="$2"
    if "$fn" > "$WORK/why" 2>&1; then
        echo "PASS: $desc"; PASS=$((PASS + 1))
    else
        echo "FAIL: $desc — $(tail -n 3 "$WORK/why")"; FAIL=$((FAIL + 1))
    fi
}
contains() { case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac; }
hasnt_by_hand() { ! contains "$1" "by hand" || { echo "a provable record told to remove by hand: $1"; return 1; }; }
lib() { bash -c ". '$LIB'; $1"; }
field() { cut -d: -f"$2" <<<"$1"; }
marker() { printf '%s.reclaim.%s' "$P" "${1//:/.}"; }
markers() { find "$SOT_COMM_HOME" -maxdepth 1 -name '.registry.lock.reclaim.*' | wc -l; }
reset() {
    rm -f "${SOT_COMM_HOME:?}"/.registry.lock*
    rmdir "${P:?}" 2>/dev/null
    rm -f "${WORK:?}/go" "${WORK:?}/ready"
    printf '{"protocol_version": 1, "agents": {"x": {"last_seen": "t"}}}\n' > "$SOT_COMM_HOME/registry.json"
    return 0
}
SELF="$(lib '_sot_lock_self_id; echo "$_SOT_LOCK_SELF"')"   # name:machine:boot:pidns

# A holder SIGKILLed in its critical section; prints the record it left.
dead_holder() { lib 'with_lock kill -9 $BASHPID' 2>/dev/null; cat "$P"; }
# A holder that holds until $WORK/go exists; $1 = STOP freezes it in-lock.
live_holder() {
    local i
    bash -c ". '$LIB'; hold() { : > '$WORK/ready'; ${1:+kill -STOP \$BASHPID;} while [ ! -e '$WORK/go' ]; do sleep 0.05; done; }; with_lock hold" &
    HOLDER=$!; PIDS+=("$HOLDER")
    for i in $(seq 100); do [ -e "$WORK/ready" ] && return 0; sleep 0.05; done
    echo "the holder never took the lock"; return 1
}
end_holder() { touch "$WORK/go"; kill -CONT "$HOLDER" 2>/dev/null; wait "$HOLDER" 2>/dev/null; return 0; }
# A process that prints its own record to $WORK/r and then freezes (STOP) or dies (KILL).
record_then() {
    rm -f "${WORK:?}/r"
    bash -c ". '$LIB'; _sot_lock_self_id; echo \"\$_SOT_LOCK_ID\" > '$WORK/r.tmp'; mv '$WORK/r.tmp' '$WORK/r'; kill -$1 \$BASHPID" &
    R=$!; PIDS+=("$R")
    local i; for i in $(seq 100); do [ -s "$WORK/r" ] && break; sleep 0.05; done
    [ "$1" = KILL ] && wait "$R" 2>/dev/null
    return 0
}

t1() {
    reset; local d t0 ms me
    d="$(dead_holder)"
    t0=$(date +%s%N)
    lib '_sot_lock_self_id; echo "$_SOT_LOCK_ID" > "'"$WORK"'/me"; with_lock registry_touch x' || { echo "waiter failed"; return 1; }
    ms=$(( ($(date +%s%N) - t0) / 1000000 ))
    [ "$ms" -lt 3000 ] || { echo "took ${ms}ms"; return 1; }
    me="$(cat "$WORK/me")"
    [ "$(cat "$(marker "$d")")" = "$me" ] || { echo "marker does not hold the waiter's ID"; return 1; }
    [ ! -e "$P" ] || { echo "lock not released"; return 1; }
    jq -e '.agents.x.last_seen != "t"' "$SOT_COMM_HOME/registry.json" >/dev/null || { echo "registry.json not written"; return 1; }
}
check "1: a SIGKILLed holder is reclaimed, its marker holds the waiter, registry.json parses" t1

t2() {
    reset; local before err
    live_holder || return 1
    before="$(cat "$P")"
    [ "$(field "$before" 5)" = "$HOLDER" ] || { echo "record pid $(field "$before" 5) is not the holder $HOLDER"; end_holder; return 1; }
    err="$(lib 'SOT_LOCK_WAIT_SECS=0.2 with_lock true' 2>&1)" && { echo "waiter took a live lock"; end_holder; return 1; }
    [ "$(cat "$P")" = "$before" ] || { echo "the lock changed"; end_holder; return 1; }
    [ "$(markers)" = 0 ] || { echo "a marker was made"; end_holder; return 1; }
    contains "$err" "is held by $(field "$before" 1) pid $HOLDER start $(field "$before" 6) (" || { echo "$err"; end_holder; return 1; }
    contains "$err" "): it is running. If it is dead, run any comm command on $(field "$before" 1), or run comm-registry-lock-clear.sh." || { echo "$err"; end_holder; return 1; }
    end_holder
    [ ! -e "$P" ] || { echo "not released"; return 1; }
}
check "2: a live holder is named and never reclaimed; its record names the holding process" t2

t3() {
    reset; local err before name machine boot
    live_holder STOP || return 1
    before="$(cat "$P")"
    err="$(lib 'SOT_LOCK_WAIT_SECS=0.2 with_lock true' 2>&1)" && { echo "took a frozen lock"; end_holder; return 1; }
    contains "$err" "pid $HOLDER start" && contains "$err" "it is running" || { echo "$err"; end_holder; return 1; }
    [ "$(cat "$P")" = "$before" ] || { echo "the lock changed"; end_holder; return 1; }
    end_holder
    [ ! -e "$P" ] || { echo "the thawed holder did not release"; return 1; }
    IFS=: read -r name machine boot _ <<<"$SELF"
    printf 'elsewhere:%s:%s:1:4242:1\n' "ffffffffffffffffffffffffffffffff" "00000000-0000-0000-0000-000000000000" > "$P"
    err="$(lib 'SOT_LOCK_WAIT_SECS=0.1 with_lock true' 2>&1)" && { echo "took a foreign lock"; return 1; }
    contains "$err" "held by elsewhere pid 4242 start 1 (" && contains "$err" "it is on another machine" || { echo "$err"; return 1; }
    printf 'elsewhere:%s:%s:1:4242:1\n' "$machine" "00000000-0000-0000-0000-000000000000" > "$P"
    err="$(lib 'SOT_LOCK_WAIT_SECS=0.1 with_lock true' 2>&1)" && { echo "a shared machine-id under another name was taken as a reboot"; return 1; }
    contains "$err" "it is on another machine" || { echo "$err"; return 1; }
    printf '%s:%s:%s:1:4242:1\n' "$name" "$machine" "00000000-0000-0000-0000-000000000000" > "$P"
    lib 'with_lock true' || { echo "the reboot proof did not reclaim"; return 1; }
    [ "$(markers)" = 1 ] || { echo "no marker for the rebooted holder"; return 1; }
}
check "3: frozen -> FAILED then released; another machine -> FAILED; same machine, other boot -> reclaimed" t3

t4() {
    reset; local round d outs w m ids
    for round in $(seq 100); do
        d="$(dead_holder)"
        find "$SOT_COMM_HOME" -maxdepth 1 -name '.registry.lock.reclaim.*' | sort > "$WORK/before"
        rm -f "${WORK:?}"/id?
        outs=()
        for w in 1 2 3; do
            lib "crit() { mkdir '$WORK/cs' || echo OVERLAP; sleep 0.01; rmdir '${WORK:?}/cs'; echo held; }; _sot_lock_self_id; echo \"\$_SOT_LOCK_ID\" > '$WORK/id$w'; with_lock crit" > "$WORK/w$w" 2>&1 &
            outs+=($!)
        done
        wait "${outs[@]}"
        for w in 1 2 3; do
            [ "$(cat "$WORK/w$w")" = held ] || { echo "round $round waiter $w: $(cat "$WORK/w$w")"; return 1; }
        done
        [ "$(markers)" -ge "$round" ] || { echo "round $round: $(markers) markers"; return 1; }
        [ -e "$(marker "$d")" ] || { echo "round $round: no marker for the dead holder"; return 1; }
        ids="$(cat "$WORK"/id?)"
        for m in $(find "$SOT_COMM_HOME" -maxdepth 1 -name '.registry.lock.reclaim.*' | sort | comm -13 "$WORK/before" -); do
            grep -qxF "$(cat "$m")" <<<"$ids" || { echo "round $round: $m was made by no waiter of the round"; return 1; }
            [ "$m" = "$(marker "$d")" ] || grep -qxF "${m#"$P".reclaim.}" <<<"${ids//:/.}" \
                || { echo "round $round: $m names neither D nor a waiter of the round"; return 1; }
        done
    done
}
check "4: 100 rounds of 3 waiters on a dead holder: one holder at a time, reclaim.<D> made, every marker the round's" t4

t5() {
    reset; local d r err bad=""
    d="$(dead_holder)"
    record_then STOP; r="$(cat "$WORK/r")"
    printf '%s\n' "$r" > "$(marker "$d")"
    err="$(lib 'SOT_LOCK_WAIT_SECS=0.2 with_lock true' 2>&1)" && { echo "took past a frozen reclaimer"; return 1; }
    # Which true reason prints depends on the runner's speed: the last walk
    # reaches R before the deadline (it is running) or after it (still walked).
    contains "$err" "is held by $(field "$d" 1) pid $(field "$d" 5) " || { echo "$err"; bad=1; }
    contains "$err" "it is dead, but its reclaim by $(field "$r" 1) pid $(field "$r" 5) did not finish: it is running" \
        || contains "$err" "its reclaim chain was still being walked at the deadline" || { echo "$err"; bad=1; }
    [ "$(cat "$P")" = "$d" ] || { echo "the lock changed"; bad=1; }
    # The second half runs whatever the first half found.
    kill -9 "$R"; wait "$R" 2>/dev/null
    lib 'with_lock true' || { echo "the nested reclaim failed"; return 1; }
    [ -e "$(marker "$d")" ] && [ -e "$(marker "$r")" ] || { echo "a marker is gone"; return 1; }
    [ -z "$bad" ]
}
check "5: a frozen reclaimer is named with the holder; a dead one is reclaimed through reclaim.<R>" t5

t6() {
    reset; local err
    mkdir "$P"
    err="$(lib 'SOT_LOCK_WAIT_SECS=0.2 with_lock true' 2>&1)" && { echo "took an older peer's lock"; return 1; }
    contains "$err" "held by an older version that records no holder" || { echo "$err"; return 1; }
    [ -d "$P" ] && [ -z "$(ls -A "$P")" ] || { echo "the older lock was changed or linked into"; return 1; }
    rmdir "${P:?}"
    live_holder || return 1
    # f8a6a107's with_lock loop, verbatim but for the callee.
    bash -c 'LOCKDIR="'"$P"'"; tries=0
        while ! mkdir "$LOCKDIR" 2>/dev/null; do tries=$((tries + 1)); [ "$tries" -gt 200 ] && exit 1; sleep 0.05; done
        echo got; rmdir "$LOCKDIR"' > "$WORK/old" &
    local old=$!
    sleep 0.5
    [ ! -s "$WORK/old" ] || { echo "the older waiter entered a held lock"; end_holder; return 1; }
    end_holder
    wait "$old" || { echo "the older waiter never entered"; return 1; }
    [ "$(cat "$WORK/old")" = got ] && [ ! -e "$P" ] || { echo "older waiter: $(cat "$WORK/old")"; return 1; }
}
check "6: an older peer's mkdir lock is FAILED and untouched; an older waiter waits on a file lock" t6

t7() {
    reset; local t0 ms
    dead_holder >/dev/null
    lib 'SOT_LOCK_WAIT_SECS=0 with_lock registry_touch x' || { echo "zero tries did not reclaim"; return 1; }
    live_holder || return 1
    t0=$(date +%s%N)
    lib 'SOT_LOCK_WAIT_SECS=0 with_lock true' 2>/dev/null && { echo "took a live lock"; end_holder; return 1; }
    ms=$(( ($(date +%s%N) - t0) / 1000000 ))
    end_holder
    [ "$ms" -lt 1000 ] || { echo "zero tries waited ${ms}ms"; return 1; }
}
check "7: a zero wait reclaims a dead holder and gives up at once on a live one" t7

join_rows() {
    ( cd "$WORK" && SOT_COMM_SELF_FILE="$WORK/self-peer.txt" SOT_COMM_TEST_HOST=testhost "$BIN/comm-join.sh" --name t-peer ) >/dev/null 2>&1 &&
    ( cd "$WORK" && SOT_COMM_SELF_FILE="$WORK/self-sender.txt" SOT_COMM_TEST_HOST=testhost "$BIN/comm-join.sh" --name t-sender ) >/dev/null 2>&1 &&
    : > "$SOT_COMM_HOME/inbox/t-peer.jsonl"
}
send() {  # MSG — the send's stdout; SEND_RC, SEND_MS
    local t0; t0=$(date +%s%N)
    SEND_OUT="$(cd "$WORK" && SOT_COMM_SELF_FILE="$WORK/self-sender.txt" SOT_COMM_TEST_HOST=testhost \
        "$BIN/comm-send.sh" @t-peer "$1" 2>"$WORK/send.err")"
    SEND_RC=$?; SEND_MS=$(( ($(date +%s%N) - t0) / 1000000 ))
}
filed() { jq -e --arg m "$1" 'select(.msg == $m)' "$SOT_COMM_HOME/inbox/t-peer.jsonl" >/dev/null 2>&1; }

t8() {
    reset; rm -f "${SOT_COMM_HOME:?}/registry.json"; join_rows || { echo "join failed"; return 1; }
    local d err t0 ms
    d="$(dead_holder)"
    send "after a killed holder"
    [ "$SEND_RC" = 0 ] && filed "after a killed holder" || { echo "send rc=$SEND_RC: $SEND_OUT $(cat "$WORK/send.err")"; return 1; }
    [ -e "$(marker "$d")" ] && [ ! -e "$P" ] || { echo "the send did not reclaim"; return 1; }
    live_holder || return 1
    send "beside a live holder"
    [ "$SEND_RC" = 0 ] && filed "beside a live holder" || { echo "send rc=$SEND_RC: $SEND_OUT"; end_holder; return 1; }
    [ "$SEND_MS" -ge 900 ] && [ "$SEND_MS" -lt 4000 ] || { echo "the send took ${SEND_MS}ms"; end_holder; return 1; }
    t0=$(date +%s%N)
    err="$(cd "$WORK" && SOT_COMM_SELF_FILE="$WORK/self-peer.txt" SOT_COMM_TEST_HOST=testhost "$BIN/comm-status.sh" working 2>&1)" \
        && { echo "comm-status wrote under a live holder"; end_holder; return 1; }
    ms=$(( ($(date +%s%N) - t0) / 1000000 ))
    end_holder
    [ "$ms" -ge 10000 ] || { echo "comm-status gave up after ${ms}ms"; return 1; }
    contains "$err" "is held by" && contains "$err" "it is running" || { echo "$err"; return 1; }
}
check "8: send reclaims a killed holder and files; beside a live one ~1 s, exit 0, filed; comm-status waits 10 s and FAILs" t8

t9() {
    reset; local out far r
    printf 'elsewhere:ffffffffffffffffffffffffffffffff:00000000-0000-0000-0000-000000000000:1:4242:1\n' > "$P"
    out="$(SOT_COMM_TEST_LOCK_SETTLE=0.05 bash "$BIN/comm-registry-lock-clear.sh" 2>&1)" || { echo "$out"; return 1; }
    [ ! -e "$P" ] && [ -e "$(marker 'elsewhere:ffffffffffffffffffffffffffffffff:00000000-0000-0000-0000-000000000000:1:4242:1')" ] \
        || { echo "not cleared through its marker: $out"; return 1; }
    live_holder || return 1
    out="$(bash "$BIN/comm-registry-lock-clear.sh" 2>&1)" && { echo "cleared a live holder"; end_holder; return 1; }
    [ -e "$P" ] || { echo "a live holder's lock is gone"; end_holder; return 1; }
    end_holder
    contains "$out" "NOT cleared" && contains "$out" "it is running" || { echo "$out"; return 1; }
    # The person vouches for the holder only, never for a reclaimer holding
    # its marker: one this box cannot prove dead may be pending on its own
    # machine (review B1).
    reset
    far='elsewhere:ffffffffffffffffffffffffffffffff:00000000-0000-0000-0000-000000000000:1:4242:1'
    printf '%s\n' "$far" > "$P"
    r='elsewhere:ffffffffffffffffffffffffffffffff:00000000-0000-0000-0000-000000000000:1:4343:1'
    printf '%s\n' "$r" > "$(marker "$far")"
    out="$(SOT_COMM_TEST_LOCK_SETTLE=0.05 bash "$BIN/comm-registry-lock-clear.sh" 2>&1)"; local rc=$?
    [ "$rc" != 0 ] || { echo "passed a reclaimer it cannot prove dead: $out"; return 1; }
    [ "$(cat "$P")" = "$far" ] && [ "$(cat "$(marker "$far")")" = "$r" ] && [ ! -e "$(marker "$r")" ] \
        || { echo "the lock or its marker changed: $out"; return 1; }
    contains "$out" "its reclaim by elsewhere pid 4343 did not finish: it is on another machine" || { echo "$out"; return 1; }
    hasnt_by_hand "$out" || return 1
    # A reclaimer with no proof fields (a clear killed on macOS or git-bash):
    # no box can prove it dead, so the refusal says to remove the lock by hand.
    reset
    printf '%s\n' "$far" > "$P"
    printf '%s\n' 'mac:-:-:-:4343:-' > "$(marker "$far")"
    out="$(SOT_COMM_TEST_LOCK_SETTLE=0.05 bash "$BIN/comm-registry-lock-clear.sh" 2>&1)" && { echo "passed a proof-less reclaimer"; return 1; }
    contains "$out" "remove the lock by hand" && [ "$(cat "$P")" = "$far" ] || { echo "$out"; return 1; }
    reset
    out="$(bash "$BIN/comm-registry-lock-clear.sh" 2>&1)" || { echo "no lock, rc=$?: $out"; return 1; }
    contains "$out" "is free" || { echo "$out"; return 1; }
}
check "9: the clear clears an unprovable holder, refuses a live one and an unprovable reclaimer, and says free" t9

t10() {
    reset; local t0 ms
    live_holder || return 1
    t0=$(date +%s%N)
    lib 'SOT_LOCK_WAIT_SECS=1 SOT_COMM_TEST_LOCK_TRY_DELAY=0.5 with_lock true' 2>/dev/null && { echo "took a live lock"; end_holder; return 1; }
    ms=$(( ($(date +%s%N) - t0) / 1000000 ))
    end_holder
    [ "$ms" -ge 1000 ] && [ "$ms" -lt 2200 ] || { echo "a 1 s touch with 0.5 s tries took ${ms}ms"; return 1; }
}
check "10: the bound is time: 0.5 s tries against a live holder end within 1 s plus one try" t10

t11() {
    reset; local err
    mkdir -p "$WORK/nolink"
    cat > "$WORK/nolink/link" <<'LINK'
#!/bin/sh
echo "link: cannot create link '$2' to '$1': Operation not permitted" >&2
exit 1
LINK
    chmod +x "$WORK/nolink/link"
    err="$(PATH="$WORK/nolink:$PATH" lib 'with_lock true' 2>&1)" && { echo "took a lock with no hard links"; return 1; }
    contains "$err" "registry lock $P cannot be taken: cannot hard-link in $SOT_COMM_HOME: Operation not permitted" \
        || { echo "$err"; return 1; }
    [ ! -e "$P" ] && [ -z "$(find "$SOT_COMM_HOME" -maxdepth 1 -name '.registry.lock.tmp.*')" ] || { echo "a lock or temp was left"; return 1; }
}
check "11: a home without hard links FAILs naming the cause and leaves nothing" t11

# The step and take, counted: after the step removes the dead holder's lock,
# another dead holder takes it before the retake (review SF2).
cat > "$WORK/count.sh" <<'COUNT'
. "$LIB"
eval "orig_step() $(declare -f _sot_lock_step | tail -n +2)"
eval "orig_take() $(declare -f _sot_lock_take | tail -n +2)"
_sot_lock_step() { echo step >> "$WORK/count"; orig_step "$@" || return; printf '%s\n' "$D2" > "$P"; }
_sot_lock_take() { [ "$1" != "$P" ] || echo take >> "$WORK/count"; orig_take "$@"; }
SOT_LOCK_WAIT_SECS=0 with_lock true
COUNT
t12() {
    reset; local d2 out t0 ms
    record_then KILL; d2="$(cat "$WORK/r")"
    dead_holder >/dev/null; rm -f "${WORK:?}/count"
    t0=$(date +%s%N)
    out="$(LIB="$LIB" WORK="$WORK" P="$P" D2="$d2" bash "$WORK/count.sh" 2>&1)" && { echo "took the lock: $out"; return 1; }
    ms=$(( ($(date +%s%N) - t0) / 1000000 ))
    [ "$(grep -c step "$WORK/count")" = 1 ] && [ "$(grep -c take "$WORK/count")" = 2 ] \
        || { echo "not one step and one retake: $(tr '\n' ' ' < "$WORK/count")"; return 1; }
    [ "$(cat "$P")" = "$d2" ] || { echo "the second dead holder's lock was reclaimed"; return 1; }
    [ "$ms" -lt 1000 ] || { echo "a zero wait took ${ms}ms"; return 1; }
    contains "$out" "is held by $(field "$d2" 1) pid $(field "$d2" 5) start $(field "$d2" 6) (" \
        && contains "$out" "another process took it as soon as a dead holder's lock was removed" || { echo "$out"; return 1; }
}
check "12: a zero wait against a chain of dead holders makes one step and its retake, and fails at once" t12

# The clock: bash 5's EPOCHREALTIME, digits only in any locale (review SF3).
t13() {
    reset; local out loc
    out="$(lib 'unset EPOCHREALTIME; EPOCHREALTIME="1727712345,123456"; _sot_lock_now 0.5 && echo "$_SOT_LOCK_NOW"')"
    [ "$out" = 1727712345623456 ] || { echo "comma stub: $out"; return 1; }
    loc="$(locale -a 2>/dev/null | grep -m1 -iE '^(de_DE|fr_FR|nl_NL|ru_RU)\.utf-?8$')"
    if [ -n "$loc" ]; then
        out="$(LC_ALL="$loc" lib 'case "$EPOCHREALTIME" in *,*) ;; *) echo "no comma in $EPOCHREALTIME"; exit 1 ;; esac; _sot_lock_now 0 && echo "$_SOT_LOCK_NOW"')"
        [[ "$out" =~ ^[0-9]{16}$ ]] || { echo "$loc: $out"; return 1; }
    else
        echo "  no comma-decimal locale on this box: the stub alone"
    fi
    live_holder || return 1
    for v in unset '' abc; do
        if [ "$v" = unset ]; then out="$(lib 'unset EPOCHREALTIME; with_lock true' 2>&1)"
        else out="$(lib "unset EPOCHREALTIME; EPOCHREALTIME='$v'; with_lock true" 2>&1)"; fi && { echo "took a live lock ($v)"; end_holder; return 1; }
        contains "$out" "is held, and there is no clock to wait by: bash's EPOCHREALTIME is unset or not a number" \
            && ! contains "$out" "it is running" || { echo "$v: $out"; end_holder; return 1; }
    done
    end_holder
}
check "13: the clock is EPOCHREALTIME's digits in any locale, and no clock is FAILED naming it" t13

# A daemon that died during its own reclaim leaves reclaim.<D> naming D.
# `timeout` turns the endless walk this replaced into a failure, not a hang.
t14() {
    reset; local d out t0 ms
    d="$(dead_holder)"
    printf '%s\n' "$d" > "$(marker "$d")"
    t0=$(date +%s%N)
    out="$(timeout 20 bash -c ". '$LIB'; SOT_LOCK_WAIT_SECS=1 with_lock true" 2>&1)" && { echo "took the lock: $out"; return 1; }
    ms=$(( ($(date +%s%N) - t0) / 1000000 ))
    [ "$ms" -lt 2500 ] || { echo "a 1 s writer took ${ms}ms"; return 1; }
    contains "$out" "its reclaim marker $(marker "$d") names $d, which its reclaim chain already holds" \
        && contains "$out" "remove $P by hand" || { echo "$out"; return 1; }
    t0=$(date +%s%N)
    out="$(timeout 20 bash "$BIN/comm-registry-lock-clear.sh" 2>&1)" && { echo "cleared: $out"; return 1; }
    ms=$(( ($(date +%s%N) - t0) / 1000000 ))
    [ "$ms" -lt 2500 ] || { echo "the clear took ${ms}ms"; return 1; }
    contains "$out" "its reclaim marker $(marker "$d") names $d" && contains "$out" "remove the lock by hand" \
        || { echo "$out"; return 1; }
    [ "$(cat "$P")" = "$d" ] && [ "$(cat "$(marker "$d")")" = "$d" ] || { echo "the lock or its marker changed"; return 1; }
}
check "14: a marker naming its own dead holder stops the writer and the clear, which say by hand" t14

t15() {
    local out
    out="$(lib '_sot_lock_self_id; _sot_lock_is_me "$_SOT_LOCK_ID" && echo mine')"
    [ "$out" = mine ] || { echo "this process, with its proof: $out"; return 1; }
    out="$(lib '_sot_lock_self_id; _SOT_LOCK_SELF=""; _SOT_LOCK_ID="${_SOT_LOCK_ID%%:*}:-:-:-:$BASHPID:-"
        _sot_lock_is_me "$_SOT_LOCK_ID" && echo mine || echo not')"
    [ "$out" = not ] || { echo "the same host name and pid, with no proof: $out"; return 1; }
    # A call site put back to a bare comparison would pass the two above.
    local rs="$SCRIPT_DIR/../../rust/backend/src/comm/registry/lock.rs" hits
    local rs_tests="$SCRIPT_DIR/../../rust/backend/src/comm/registry/lock_tests.rs"
    local f libs=("$SCRIPTS_DIR"/comm-lib*.sh)
    for f in "$rs" "$rs_tests"; do [ -f "$f" ] || { echo "no $f"; return 1; }; done
    [ "${#libs[@]}" -ge 8 ] && [ -f "${libs[0]}" ] || { echo "fewer than eight comm-lib*.sh in $SCRIPTS_DIR"; return 1; }
    hits="$( { sed '/^_sot_lock_is_me() {/,/^}/s/.*//' "${libs[@]}" \
            | grep -nE ' (=|==|!=) +"?\$\{?_SOT_LOCK_ID([^A-Za-z0-9_]|$)|\$\{?_SOT_LOCK_ID\}?"? +(=|==|!=) |case +"?\$\{?_SOT_LOCK_ID|^ *"?\$\{?_SOT_LOCK_ID\}?"? *\)' \
            | sed 's/^/comm-lib*.sh:/'
        for f in "$rs" "$rs_tests"; do sed '/fn is_me(/,/^    }/s/.*//' "$f" \
            | grep -nE '(==|!=) *[&*]*[A-Za-z0-9_:().]*\.id([^A-Za-z0-9_]|$)|\.id *(==|!=)|contains\(&[A-Za-z0-9_:().]*\.id\)' \
            | sed "s|^|${f##*/}:|"; done; } )"
    [ -z "$hits" ] || { echo "the own ID compared but through _sot_lock_is_me / Me::is_me: $hits"; return 1; }
}
check "15: an ID with no proof fields is never judged mine, and nothing compares to it but the one test" t15

# The clear forces a proof-less D; during its settle (a `sleep` of 0.07 on
# PATH) a mac writer N takes the lock.
t16() {
    reset; local out n='mac:-:-:-:4343:-'
    mkdir -p "$WORK/settle"
    cat > "$WORK/settle/sleep" <<SLEEP
#!/bin/sh
[ "\$1" = 0.07 ] && printf '%s\n' '$n' > '$P'
exec '$(command -v sleep)' "\$@"
SLEEP
    chmod +x "$WORK/settle/sleep"
    printf '%s\n' 'far:-:-:-:4242:-' > "$P"
    out="$(PATH="$WORK/settle:$PATH" SOT_COMM_TEST_LOCK_SETTLE=0.07 bash "$BIN/comm-registry-lock-clear.sh" 2>&1)" \
        && { echo "cleared: $out"; return 1; }
    [ "$(cat "$P")" = "$n" ] || { echo "N's lock changed: $out"; return 1; }
    contains "$out" ", held by mac pid 4343: it was taken again during the reclaim" && ! contains "$out" "no proof fields" \
        && hasnt_by_hand "$out" || { echo "$out"; return 1; }
}
check "16: a clear whose proof-less holder's lock is taken during the settle names the new holder, not by hand" t16

# The retake after a reclaim fails, and the lock is gone by its fresh read.
cat > "$WORK/free.sh" <<'FREE'
. "$LIB"
eval "orig_step() $(declare -f _sot_lock_step | tail -n +2)"
eval "orig_take() $(declare -f _sot_lock_take | tail -n +2)"
_sot_lock_step() { orig_step "$@" || return; printf '%s\n' 'elsewhere:-:-:-:4242:-' > "$P"; RETOOK=1; }
_sot_lock_take() { orig_take "$@"; local rc=$?; [ -z "${RETOOK:-}" ] || [ "$1" != "$P" ] || rm -f "${P:?}"; return "$rc"; }
SOT_LOCK_WAIT_SECS=0 with_lock true
FREE
t17() {
    reset; local out
    dead_holder >/dev/null
    out="$(LIB="$LIB" P="$P" bash "$WORK/free.sh" 2>&1)" && { echo "took the lock: $out"; return 1; }
    [ ! -e "$P" ] || { echo "the lock is not free"; return 1; }
    contains "$out" "was not taken by the deadline: another process took it as soon as a dead holder's lock was removed" \
        && hasnt_by_hand "$out" || { echo "$out"; return 1; }
}
check "17: a free lock at the deadline is FAILED as not taken, never as one to remove by hand" t17

# A step that finds the lock already gone clears the previous step's by-hand
# flag, and says the holder it last read WAS held.
t18() {
    reset; local out
    mkdir "$P"
    out="$(lib "_SOT_REG_LOCK='$P'; _sot_lock_step; rmdir '$P'; _sot_lock_step; echo \"[\$_SOT_LOCK_BYHAND]\"; _sot_lock_fail_text")"
    contains "$out" "[]" && hasnt_by_hand "$out" || { echo "$out"; return 1; }
}
check "18: a lock gone at the step after a by-hand step is not FAILED as one to remove by hand" t18

t19() {
    reset; local out
    out="$(lib "_SOT_REG_LOCK='$P'; _SOT_LOCK_HOLDER='mac:-:-:-:4343:77'; _SOT_LOCK_WHY='it is alive'; _SOT_LOCK_GONE=''; _sot_lock_step; _sot_lock_fail_text")"
    contains "$out" "was held by mac pid 4343 start 77 when last read (it is alive)" && ! contains "$out" "is held" \
        && contains "$out" "when last read (it is alive); it may have been released since. Retry." \
        && ! contains "$out" "comm-registry-lock-clear" || { echo "$out"; return 1; }
}
check "19: a lock released since the last read is FAILED as was held, never as is held" t19

# A live record the step cannot read (a permission or I/O error, or a holder
# gone before a live writer linked the lock) is released, never by hand, in
# the FAILED line and in the clear's refusal; what was read whole, or a
# directory, still is by hand.
own_record() { lib "_sot_lock_start $$ && echo \"$SELF:$$:\$_SOT_LOCK_START\""; }
step_text() { lib "_sot_lock_self_id; _SOT_REG_LOCK='$P'; _sot_lock_step; echo \"[\$_SOT_LOCK_BYHAND][\$_SOT_LOCK_GONE]\"; _sot_lock_fail_text"; }
t20() {
    reset; local out
    own_record > "$P"; chmod 000 "$P"
    out="$(step_text)"
    chmod 644 "$P"
    contains "$out" "[][1]" && contains "$out" "could not be read" && hasnt_by_hand "$out" && [[ "$out" == *Retry. ]] \
        || { echo "$out"; return 1; }
}
check "20: a live record that cannot be read is released, never FAILED as one to remove by hand" t20

t21() {
    reset; local out rc rec
    rec="$(own_record)"; printf '%s\n' "$rec" > "$P"; chmod 000 "$P"
    out="$(bash "$BIN/comm-registry-lock-clear.sh" 2>&1 >/dev/null)"; rc=$?
    chmod 644 "$P"
    [ "$(cat "$P")" = "$rec" ] || { echo "the live lock changed: $out"; return 1; }
    [ "$rc" = 1 ] && hasnt_by_hand "$out" && ! contains "$out" "comm-registry-lock-clear" || { echo "rc=$rc: $out"; return 1; }
}
check "21: a clear whose lock cannot be read says neither to remove it by hand nor to clear it again" t21

t22() {
    reset; local out
    : > "$P"
    out="$(step_text)"
    contains "$out" "[1][]" && contains "$out" "its record names no holder (empty). If its holder is dead, remove $P by hand" \
        || { echo "$out"; return 1; }
}
check "22: an empty record read whole is still one to remove by hand" t22

t23() {
    reset; local out
    mkdir "$P"
    out="$(step_text)"
    contains "$out" "[1][]" && contains "$out" "held by an older version that records no holder. If its holder is dead" \
        || { echo "$out"; return 1; }
}
check "23: an older version's directory is still one to remove by hand" t23

echo "---"
echo "$PASS passed, $FAIL failed"
[ "$FAIL" = 0 ]

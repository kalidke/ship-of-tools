#!/usr/bin/env bash
# test-registry-twohost.sh — the registry's one writer and one reader (B1b)
# across boxes that mount one home. The proof a single box cannot give: a
# rename on one box and a read on the other, with the writer's fsync and the
# mkdir lock both enforced by the server.
#
# Two pairs, one after the other: V4 is this box with --peer (the same
# mount), and MIX is --v3-host (an NFSv3 mount of the same home) with this
# box. Per pair, on each box, for $SOT_TWOHOST_SECS (default 40) seconds:
#   - 2 writers, each owning 5 rows rt-<side>-<w>-<k>, loop: ensure_home, then
#     `with_lock registry_put <row> {"seq":N}` with a rising N, logging each N
#     whose put returned 0 and each put that did not as FAILED;
#   - 1 reader loops sot_registry_read over the other box's 10 rows (seeded
#     before the clock starts) and counts the answers 0, 1 and 2.
# Every helper logs its registry retries (SOT_COMM_TEST_RETRY_LOG), and each
# box's line gives the reads that took a retry and how many a retry resolved.
# A pair passes with, on each box: zero reads answering 1 or 2, zero FAILED
# puts, every read that took a retry resolved, and at the end every row
# present with its writer's last logged N (no lost update). A pass with no
# retry taken proves the read, not the retry: the race did not happen.
#
# Every helper is started by this script and ends by itself at the deadline
# (or at the stop file); the script waits for every one on every box, checks
# that none is left running there, and only then removes its folder. The
# folder is fresh, under $HOME (every box sees it), never ~/.sot-comm, and no
# daemon is ever dialled. The peers run a COPY of this tree's comm-lib.sh
# placed in that folder, so the worktree need not exist there.
#
# Needs real boxes, so it runs in no workflow. It keeps the real HOME by
# design (the scratch home must be on the shared mount); sourcing
# lib-home-guard.sh drops the host's comm identity and daemon route.
#
# Usage: comm/core/tests/test-registry-twohost.sh --peer HOST [--v3-host HOST]
# Exit: 0 every pair passes, 1 any fails, 2 usage.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home

PEER=""; V3=""
while [ $# -gt 0 ]; do
    case "$1" in
        --peer) PEER="${2:-}"; shift 2 || shift ;;
        --v3-host) V3="${2:-}"; shift 2 || shift ;;
        *) break ;;
    esac
done
[ -n "$PEER" ] || { echo "usage: test-registry-twohost.sh --peer HOST [--v3-host HOST]  (boxes that mount this home)" >&2; exit 2; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SECS="${SOT_TWOHOST_SECS:-40}"
SSH=(ssh -o BatchMode=yes -o ConnectTimeout=5)

DIR="$(mktemp -d "$HOME/.sot-registry-twohost-XXXXXX")" || { echo "FATAL: mktemp under \$HOME failed" >&2; exit 1; }
JOBS=()   # this box's helpers and ssh clients
BOXES=("" "$PEER" ${V3:+"$V3"})
on() {  # HOST CMD... — run on HOST ("" is this box)
    local h="$1"; shift
    if [ -z "$h" ]; then bash -c "$*"; else "${SSH[@]}" "$h" "$*"; fi
}
wait_all() { local p; for p in "${JOBS[@]}"; do wait "$p" 2>/dev/null; done; JOBS=(); }
stop_all() {  # ask every helper to stop, wait for each, then kill any left
    local h pids
    touch "$DIR/stop" 2>/dev/null
    wait_all
    for h in "${BOXES[@]}"; do
        pids="$(on "$h" "cat $(printf %q "$DIR")/*/pids/* 2>/dev/null" 2>/dev/null | tr '\n' ' ')"
        [ -z "${pids// }" ] || on "$h" "kill -9 $pids" 2>/dev/null
    done
}
cleanup() { stop_all; rm -rf "${DIR:?}"; }
trap cleanup EXIT
SCRIPTS_DIR="$(guard_stage_bin "$DIR")" || exit 2

mkdir -p "$DIR/lib"
cp "$SCRIPTS_DIR/comm-lib.sh" "$DIR/lib/comm-lib.sh"
cat > "$DIR/lib/helper.sh" <<'HELPER'
# helper.sh PAIR SIDE ROLE OTHER START END — a writer (ROLE w1|w2) or the
# reader (ROLE r, of side OTHER's rows) of one pair folder.
set -u
pair="$1" side="$2" role="$3" other="$4" start="$5" end="$6"
export SOT_COMM_HOME="$pair/home"
. "$(dirname "$0")/comm-lib.sh"
tag="$side-$role"; out="$pair/out/$tag"
export SOT_COMM_TEST_RETRY_LOG="$out.retry"
echo $$ > "$pair/pids/$tag"
trap 'rm -f "${pair:?}/pids/$tag"' EXIT
while [ "$(date +%s)" -lt "$start" ]; do sleep 0.1; done
running() { [ "$(date +%s)" -lt "$end" ] && [ ! -e "$pair/../stop" ]; }
case "$role" in
    w*) n=0
        while running; do
            n=$((n + 1)); row="rt-$side-$role-$((n % 5))"
            ensure_home
            if with_lock registry_put "$row" "{\"seq\":$n}" 2>>"$out.err"; then echo "$row $n" >> "$out"
            else echo "FAILED $row $n" >> "$out"; fi
        done ;;
    r)  c0=0 c1=0 c2=0
        while running; do
            for row in rt-"$other"-w1-{0..4} rt-"$other"-w2-{0..4}; do
                rc=0; sot_registry_read "$row" >/dev/null || rc=$?
                case "$rc" in 0) c0=$((c0 + 1)) ;; 1) c1=$((c1 + 1)) ;; *) c2=$((c2 + 1)) ;; esac
            done
        done
        echo "$c0 $c1 $c2" > "$out" ;;
esac
HELPER

count() { local n; n="$(grep -c "$@" 2>/dev/null)"; echo "${n:-0}"; }

FAIL=0
run_pair() {  # NAME HOST_A HOST_B — "" is this box
    local name="$1" pair="$DIR/$1" h side other role now start end row
    local -A host=([a]="$2" [b]="$3")
    mkdir -p "$pair/home" "$pair/pids" "$pair/out"
    # trap - EXIT: with_lock restores the EXIT trap it sees, and a subshell
    # sees this script's cleanup, which would remove the folder.
    ( trap - EXIT; export SOT_COMM_HOME="$pair/home"; . "$DIR/lib/comm-lib.sh"; ensure_home
      for side in a b; do for role in w1 w2; do for k in 0 1 2 3 4; do
          with_lock registry_put "rt-$side-$role-$k" '{"seq":0}' || exit 1
      done; done; done ) || { echo "FAIL $name: the seed could not be written"; FAIL=1; return; }
    now="$(date +%s)"; start=$((now + 5)); end=$((start + SECS))
    for side in a b; do
        h="${host[$side]}"; other=a; [ "$side" = a ] && other=b
        for role in w1 w2 r; do
            on "$h" "nice -n 10 bash $(printf %q "$DIR/lib/helper.sh") $(printf %q "$pair") $side $role $other $start $end" &
            JOBS+=($!)
        done
    done
    wait_all   # every helper ends at the deadline; this waits for each one
    local left base="${DIR##*/.}"
    for h in "${BOXES[@]}"; do
        # [.]: the pattern must not match the shell that runs pgrep.
        left="$(on "$h" "pgrep -f '[.]$base' | wc -l" 2>/dev/null)"
        echo "  ${h:-this box}: $left helper(s) still running after $name"
        [ "${left:-1}" -eq 0 ] || FAIL=1
    done
    local ok bad lost reads c0 c1 c2 want got retried resolved pass=1
    for side in a b; do
        h="${host[$side]}"
        ok=0 bad=0 lost=0 retried=0 resolved=0
        for role in w1 w2 r; do
            retried=$((retried + $(count -x 'retry 1' "$pair/out/$side-$role.retry")))
            resolved=$((resolved + $(count -x 'resolved' "$pair/out/$side-$role.retry")))
        done
        for role in w1 w2; do
            ok=$((ok + $(count -v '^FAILED' "$pair/out/$side-$role")))
            bad=$((bad + $(count '^FAILED' "$pair/out/$side-$role")))
            for k in 0 1 2 3 4; do
                row="rt-$side-$role-$k"
                want="$(awk -v r="$row" '$1 == r { n = $2 } END { print n + 0 }' "$pair/out/$side-$role" 2>/dev/null)"
                got="$(jq -r --arg r "$row" '.agents[$r].seq // "missing"' "$pair/home/registry.json" 2>/dev/null)"
                [ -n "$want" ] && [ "$got" = "$want" ] || { lost=$((lost + 1)); echo "  lost update: $row is ${got:-unreadable}, its writer's last logged N is $want"; }
            done
        done
        read -r c0 c1 c2 < "$pair/out/$side-r" 2>/dev/null || { c0=0; c1=0; c2=0; echo "  ${h:-this box}: the reader left no count"; pass=0; }
        echo "  $name ${h:-this box}: reads 0/1/2 = $c0/$c1/$c2; puts ok $ok, FAILED $bad; lost updates $lost; reads retried $retried, resolved $resolved"
        [ "$c1" -eq 0 ] && [ "$c2" -eq 0 ] && [ "$bad" -eq 0 ] && [ "$lost" -eq 0 ] && [ "$c0" -gt 0 ] && [ "$ok" -gt 0 ] \
            && [ "$resolved" -eq "$retried" ] || pass=0
        cat "$pair/out/$side"-w?.err 2>/dev/null | sort | uniq -c | sed 's/^/    writer stderr: /'
    done
    if [ "$pass" = 1 ]; then echo "PASS $name"; else echo "FAIL $name"; FAIL=1; fi
}

echo "tree: $(git -C "$SCRIPT_DIR" rev-parse HEAD 2>/dev/null)$(git -C "$SCRIPT_DIR" diff --quiet HEAD 2>/dev/null || echo ' (with uncommitted changes)'); ${SECS}s per pair"
echo "pair V4: this box (side a) with $PEER (side b)"
run_pair V4 "" "$PEER"
if [ -n "$V3" ]; then
    echo "pair MIX: $V3 (side a) with this box (side b)"
    run_pair MIX "$V3" ""
fi
[ "$FAIL" -eq 0 ]

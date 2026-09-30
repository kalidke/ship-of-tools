#!/usr/bin/env bash
# test-inbox-lock-twohost.sh — the inbox lock (0031 B1) across two boxes that
# mount one home. The proof a single box cannot give: flock on a network
# mount has to be enforced by the server, and both writers — the daemon's
# filer (Rust) and `sot_inbox_append` (shell) — have to take the same lock.
#
#   (a) concurrent appends to ONE inbox, 200 lines each side: the Rust arm
#       here against the shell arm there, and shell against shell;
#   (b) a holder on one box frozen (SIGSTOP) mid-append: the other box's
#       sender waits its full wait and reports FAILED, the holder resumes,
#       and no line is torn — both directions, both arms;
#   (c) a holder killed with -9: the OS releases the lock and the next send
#       from the other box files at once — both directions.
#   Plus the two-thread Rust case on the shared home: in-process exclusion
#   on a network mount is a fact to prove, not assume.
#
# The lock record: the Rust arm writes the daemon's `inbox-lock-manager` into
# the working folder, and a script appends locally only when its own identity
# for the inbox equals it. `--expect local` (a peer on the same lock manager)
# runs the cases above; `--expect wire` (a peer that must never append, e.g.
# an NFSv3 mount) asserts the peer's route is the wire in every case and that
# the inbox holds no line from it. The wire is a stub that records each frame
# and never answers, so no daemon is ever dialled.
#
# The proof rule. Content checks ALWAYS run, for every case whatever the
# overlap: no torn line, no lost line, no FAILED line. Overlap is reported on
# its own PASS/FAIL line. A concurrency case (a) counts as proof only if its
# overlap is nonzero, and the overall verdict needs, for each, content PASS and
# overlap > 0. The (a) writers are paced 25 ms between appends on both hosts so
# that a remote waiter's NFS lock retries (which back off from ~100 ms) meet a
# lock in use. The liveness case (a3) stays UNPACED: shell here, shell on the
# peer and the Rust filer here, 200 lines each, all at once; every send must be
# `filed`, and any FAILED fails it — a send that fails under ordinary
# two-host load is a working-comms failure. a3 has its own overlap line too
# (more than 3 writer runs), and the proof verdict needs it.
#
# The reader (B-1's proof). During a3 this tree's real comm-poll.sh polls the
# case's inbox HERE in a loop, under the nfs4 record that matches, and once
# more after the writers finish: every poll must exit 0 or 75 and print no
# lock WARNING (a refused shared lock is not an exit status: the poll names
# flock's code in a WARNING line, exits 0 and reads unlocked), and across all
# polls every line is shown exactly once, none skipped, none twice.
#
# Pass is zero torn or lost lines, each line one JSON object, and every
# `filed` matching exactly one line. The peer runs a COPY of this tree's
# comm-lib.sh placed beside the inbox, so the worktree need not exist there.
# Its working folder is fresh, under $HOME (both boxes see it), and removed on
# exit; it never touches ~/.sot-comm and never talks to a live daemon.
#
# Needs real boxes, so it runs in no workflow: CI has no second host on this
# home. Requires cargo (the Rust arm is tests/comm_file.rs's ignored cases).
#
# Usage: comm/core/tests/test-inbox-lock-twohost.sh --peer HOST --expect local|wire
# Exit: 0 all pass, 1 any fail, 2 usage.
set -uo pipefail

PEER=""; EXPECT=""
while [ $# -gt 0 ]; do
    case "$1" in
        --peer) PEER="${2:-}"; shift 2 || shift ;;
        --expect) EXPECT="${2:-}"; shift 2 || shift ;;
        *) break ;;
    esac
done
[ -n "$PEER" ] && { [ "$EXPECT" = local ] || [ "$EXPECT" = wire ]; } || {
    echo "usage: test-inbox-lock-twohost.sh --peer HOST --expect local|wire  (a second box that mounts this home)" >&2; exit 2; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"
RUST_DIR="$(cd "$SCRIPT_DIR/../../../rust" && pwd)"
WAIT=10

DIR="$(mktemp -d "$HOME/.sot-inbox-lock-XXXXXX")" || { echo "FATAL: mktemp under \$HOME failed" >&2; exit 1; }
LOCAL="$(mktemp -d "${TMPDIR:-/tmp}/sot-inbox-lock-XXXXXX")" || exit 1
PEER_PIDS=(); LOCAL_PIDS=()
cleanup() {
    local p
    for p in "${PEER_PIDS[@]}"; do rpeer "kill -9 $p" 2>/dev/null; done
    for p in "${LOCAL_PIDS[@]}"; do kill -9 "$p" 2>/dev/null; done
    rm -rf "${DIR:?}" "${LOCAL:?}"
}
trap cleanup EXIT
mkdir -p "$DIR/lib" "$DIR/inbox"
cp "$SCRIPTS_DIR/comm-lib.sh" "$DIR/lib/comm-lib.sh"
# The wire, stubbed on both sides: each frame is one line in the case's
# wire.log, and nothing answers, so a wire send is FAILED and files nothing.
cat > "$DIR/lib/wire-stub.sh" <<'STUB'
sot_daemon_endpoint() { printf 'unix:/t11-stub'; }
sot_relay_endpoint() { printf 'unix:/t11-stub'; }
sot_oneshot_request() { printf '%s\n' "$1" >> "$SOT_COMM_HOME/wire.log"; return 1; }
STUB

rpeer() { ssh -o BatchMode=yes "$PEER" "$@"; }
# $1 = case folder (its inbox/ is the one both sides use), $2 = bash body run
# with this tree's comm-lib.sh sourced and SOT_COMM_HOME on that folder.
PRELUDE="source $(printf %q "$DIR/lib/comm-lib.sh"); source $(printf %q "$DIR/lib/wire-stub.sh")"
peer_sh()  { rpeer "SOT_COMM_HOME=$(printf %q "$1") bash -c $(printf %q "$PRELUDE; $2")"; }
local_sh() { SOT_COMM_HOME="$1" bash -c "$PRELUDE; $2"; }
rust_arm() {  # $1 = case folder, $2 = ignored test name
    ( cd "$RUST_DIR" && SOT_TEST_INBOX_DIR="$1/inbox" \
        cargo test -q -p sot-backend --test comm_file "$2" -- --ignored --exact --nocapture 2>"$LOCAL/cargo.err" )
}
new_case() { local c="$DIR/$1"; mkdir -p "$c/inbox"; cp "$DIR/inbox-lock-manager" "$c/"; printf '%s' "$c"; }
wires() { [ -e "$1/wire.log" ] && wc -l < "$1/wire.log" || echo 0; }

# 200 appends as writer $W, each reported `filed W-i` or `FAILED W-i`.
APPEND='for i in $(seq 0 199); do
    [ -z "${PACE:-}" ] || sleep "$PACE"
    if printf "{\"from\":\"%s\",\"to\":\"t11\",\"repo\":\"r\",\"msg\":\"%s-%s\",\"ts\":\"t\"}\n" "$W" "$W" "$i" \
        | sot_inbox_append t11 >/dev/null; then echo "filed $W-$i"; else echo "FAILED $W-$i"; fi
done'
# One send; prints `filed|FAILED <reason> after <ms>ms`.
SEND_ONE='t0=$(date +%s%N)
if r="$(printf "{\"from\":\"sh\",\"to\":\"t11\",\"repo\":\"r\",\"msg\":\"%s\",\"ts\":\"t\"}\n" "$M" | sot_inbox_append t11)"
then v="filed $M"; else v="FAILED $M: $r"; fi
echo "$v after $(( ($(date +%s%N) - t0) / 1000000 ))ms"'
# A holder: takes the lock, reports its pid, then $HOLD.
HOLDER='exec 9>> "$SOT_COMM_HOME/inbox/t11.lock"; flock 9; exec 8>> "$SOT_COMM_HOME/inbox/t11.jsonl"; echo "held $$"'
FREEZE='printf "%s" "{\"from\":\"holder\"," >&8; kill -STOP $$; printf "%s\n" "\"to\":\"t11\",\"repo\":\"r\",\"msg\":\"holder-resumed\",\"ts\":\"t\"}" >&8'

PASS=0; FAIL=0
verdict() {  # $1 = description, $2 = "" for pass or the reason
    if [ -z "$2" ]; then echo "PASS: $1"; PASS=$((PASS + 1)); else echo "FAIL: $1 — $2"; FAIL=$((FAIL + 1)); fi
}

# Whole lines, and `filed` set == line set. $1 = inbox file, $2 = want count,
# rest = files holding `filed X` / `FAILED X` reports. Prints "" or a reason.
check_inbox() {
    local f="$1" want="$2"; shift 2
    local n parsed dups
    [ -s "$f" ] || { echo "no inbox"; return; }
    [ -z "$(tail -c1 "$f")" ] || { echo "a partial last line"; return; }
    n="$(wc -l < "$f")"
    parsed="$(jq -c 'select(type == "object")' "$f" 2>/dev/null | wc -l)"
    [ "$parsed" -eq "$n" ] || { echo "$((n - parsed)) of $n lines are not one JSON object (torn)"; return; }
    [ "$n" -eq "$want" ] || { echo "$n lines, want $want (lost or extra)"; return; }
    dups="$(jq -r .msg "$f" | sort | uniq -d | head -3)"
    [ -z "$dups" ] || { echo "doubled: $dups"; return; }
    if grep -h '^FAILED' "$@" 2>/dev/null | head -3 | grep -q .; then
        echo "refused: $(grep -h '^FAILED' "$@" | head -2 | tr '\n' ' ')"; return
    fi
    if ! diff <(grep -h '^filed ' "$@" | awk '{print $2}' | sort) <(jq -r .msg "$f" | sort) >/dev/null; then
        echo "a filed report and the lines disagree"; return
    fi
    echo ""
}
# Runs of one writer in a row: as many as there are writers means they never
# overlapped, and a concurrency case that did not run concurrently proves nothing.
# interleaved FILE — for each writer, how many of its lines lie strictly
# between the other writer's first and last line: 0 means one finished
# before the other began, and a large number on both sides means a real mix.
interleaved() {  # $1 = inbox file
    jq -rs '[.[].from] as $f | ($f | unique) as $w
        | if ($w | length) != 2 then "interleaved: n/a" else
          def inside($a; $b): [range(0; $f | length) as $i
              | select($f[$i] == $a and $i > ($f | index($b)) and $i < ($f | rindex($b)))] | length;
          "interleaved: \($w[0]) \(inside($w[0]; $w[1])) inside \($w[1])\u0027s span, \($w[1]) \(inside($w[1]; $w[0])) inside \($w[0])\u0027s"
          end' "$1" 2>/dev/null
}
overlap() {  # $1 = inbox file, $2 = its writers (default 2); prints "" or a reason
    local runs; runs="$(jq -r .from "$1" 2>/dev/null | uniq | wc -l)"
    [ "$runs" -gt "${2:-2}" ] || echo "the ${2:-2} writers did not overlap ($runs runs)"
}
# A proof case's two lines: content always, overlap on its own. PROOF_BAD
# counts the cases whose content failed or whose overlap was zero.
PROOF_BAD=0
proof_case() {  # $1 = description, $2 = inbox file, $3.. = check_inbox args (want, reports)
    local d="$1" f="$2" content ov; shift 2
    content="$(check_inbox "$f" "$@")"
    ov="$(overlap "$f")"
    verdict "$d: content ($(interleaved "$f"))" "$content"
    verdict "$d: overlap > 0 ($(jq -r .from "$f" 2>/dev/null | uniq | wc -l) writer runs)" "$ov"
    [ -z "$content$ov" ] || PROOF_BAD=$((PROOF_BAD + 1))
}

# The reader: this tree's real comm-poll.sh HERE, joined as t11 on a case
# folder with a pinned self file, host and SOT_COMM_HOME (as
# test-comm-poll-cursor.sh runs it). Each poll's exit status is one line of
# $LOCAL/reader.rc and its output goes to $LOCAL/reader.out.
reader_sh() {  # $1 = case folder, $2 = script, rest = its args
    local c="$1" s="$2"; shift 2
    SOT_COMM_HOME="$c" SOT_COMM_SELF_FILE="$c/self-reader.txt" SOT_COMM_TEST_HOST=t11-reader "$SCRIPTS_DIR/$s" "$@"
}
reader_loop() {  # $1 = case folder, $2 = stop file: poll until it exists, then once more
    local rc stop=""
    while [ -z "$stop" ]; do
        [ ! -e "$2" ] || stop=1
        rc=0; reader_sh "$1" comm-poll.sh >> "$LOCAL/reader.out" 2>&1 || rc=$?
        echo "$rc" >> "$LOCAL/reader.rc"
    done
}
# Every poll exited 0 or 75 and printed no lock WARNING (the NFS client
# refusing the shared lock shows as that WARNING, exit 0), and every line of
# the inbox was shown exactly once. $1 = inbox file; prints "" or a reason.
check_reader() {
    local bad skip twice extra warned
    bad="$(grep -v -x -E '0|75' "$LOCAL/reader.rc" | sort | uniq -c | awk '{printf "%s x exit %s ", $1, $2}')"
    warned="$(grep -c -F 'WARNING: the inbox lock' "$LOCAL/reader.out")"
    sed -n 's/^\[[^]]*\] \[[^]]*\] //p' "$LOCAL/reader.out" | sort > "$LOCAL/reader.shown"
    jq -r .msg "$1" | sort > "$LOCAL/reader.want"
    twice="$(uniq -d "$LOCAL/reader.shown" | wc -l)"
    skip="$(comm -13 <(uniq "$LOCAL/reader.shown") "$LOCAL/reader.want" | wc -l)"
    extra="$(comm -23 <(uniq "$LOCAL/reader.shown") "$LOCAL/reader.want" | wc -l)"
    [ -z "$bad" ] || echo "polls exited $bad"
    [ "$warned" -eq 0 ] || echo "$warned polls warned the lock failed: $(grep -m1 -F 'WARNING: the inbox lock' "$LOCAL/reader.out")"
    [ $((skip + twice + extra)) -eq 0 ] || echo "skipped $skip, shown twice $twice, shown but not in the inbox $extra"
}

echo "peer: $PEER; working folder on the shared home; wait ${WAIT}s"
echo "here:  $(findmnt -no FSTYPE,OPTIONS -T "$DIR" | tr , '\n' | grep -E '^(nfs|vers|local_lock)' | tr '\n' ' ')"
echo "there: $(rpeer "findmnt -no FSTYPE,OPTIONS -T $(printf %q "$DIR") | tr , '\n' | grep -E '^(nfs|vers|local_lock)' | tr '\n' ' '; flock --version | head -1")"
rpeer "test -r $(printf %q "$DIR/lib/comm-lib.sh")" || { echo "FATAL: $PEER cannot see the working folder" >&2; exit 1; }
( cd "$RUST_DIR" && cargo test -q -p sot-backend --test comm_file --no-run 2>"$LOCAL/cargo.err" ) \
    || { echo "FATAL: the Rust arm does not build:"; cat "$LOCAL/cargo.err"; exit 1; }
rust_arm "$DIR" t11_write_lock_record >/dev/null
[ -s "$DIR/inbox-lock-manager" ] || { echo "FATAL: the Rust arm wrote no lock record" >&2; exit 1; }
echo "record (Rust, here): $(cat "$DIR/inbox-lock-manager")"
echo "identity here:  $(local_sh "$DIR" 'sot_inbox_lock_identity "$INBOX_DIR"')"
echo "identity there: $(peer_sh "$DIR" 'sot_inbox_lock_identity "$INBOX_DIR"')"
echo "expect: $PEER appends $([ "$EXPECT" = local ] && echo locally || echo 'over the wire only')"

# ---- the route each side takes against the record --------------------------
c="$(new_case route)"
here_route="$(local_sh "$c" '[ -n "$(_sot_inbox_lock_ours "$INBOX_DIR")" ] && echo local || echo wire')"
there_route="$(peer_sh "$c" '[ -n "$(_sot_inbox_lock_ours "$INBOX_DIR")" ] && echo local || echo wire')"
verdict "the route: here $here_route, $PEER $there_route" \
    "$([ "$here_route" = local ] || echo "this box does not append locally")$([ "$there_route" = "$EXPECT" ] || echo "$PEER's route is $there_route, expected $EXPECT")"

# ---- T3 on the shared home: two threads of one process ------------------
c="$(new_case t3)"; t0=$SECONDS
out="$(rust_arm "$c" two_threads_on_the_env_dir)"; rc=$?
verdict "two threads of one process on the shared home give 400 whole lines ($((SECONDS - t0))s)" \
    "$([ "$rc" -eq 0 ] || { echo "rc $rc"; grep -m3 -E 'panicked|assert' "$LOCAL/cargo.err" <<<"$out"; })"

# Wait until $1 holds "held <pid>"; print the pid.
held_pid() {
    local i
    for i in $(seq 1 300); do
        grep -q '^held ' "$1" 2>/dev/null && { awk '/^held /{print $2; exit}' "$1"; return 0; }
        sleep 0.1
    done
    return 1
}

if [ "$EXPECT" = wire ]; then
    # Every case, the peer's sends go to the wire and the inbox holds no line
    # of theirs. $1 = inbox file, $2 = lines wanted, $3 = the one writer they
    # all come from, $4 = case folder, $5 = wire frames wanted.
    check_wire() {
        local n parsed who
        n="$(wc -l < "$1" 2>/dev/null || echo 0)"
        if [ "$n" -gt 0 ]; then
            parsed="$(jq -c 'select(type == "object")' "$1" 2>/dev/null | wc -l)"
            [ "$parsed" -eq "$n" ] || { echo "$((n - parsed)) of $n lines torn"; return; }
            who="$(jq -r .from "$1" | sort -u | tr '\n' ' ')"
            [ "$who" = "$3 " ] || { echo "lines from: $who"; return; }
        fi
        [ "$n" -eq "$2" ] || { echo "$n lines, want $2"; return; }
        [ "$(wires "$4")" -eq "$5" ] || { echo "$(wires "$4") wire frames, want $5"; return; }
        echo ""
    }
    c="$(new_case a1)"; t0=$SECONDS
    peer_sh "$c" "W=peer; $APPEND" > "$LOCAL/a1.peer" &
    touch "$c/inbox/go"; rust_arm "$c" t11_rust_appends_200 > "$LOCAL/a1.rust"; wait
    verdict "(a) Rust here and shell on $PEER: the peer's 200 go to the wire, the inbox holds the Rust 200 ($((SECONDS - t0))s)" \
        "$(check_wire "$c/inbox/t11.jsonl" 200 rust "$c" 200)"
    c="$(new_case a2)"; t0=$SECONDS
    peer_sh "$c" "W=peer; $APPEND" > "$LOCAL/a2.peer" &
    local_sh "$c" "W=here; $APPEND" > "$LOCAL/a2.here"; wait
    verdict "(a) shell here and shell on $PEER: here files 200 locally, the peer's 200 go to the wire ($((SECONDS - t0))s)" \
        "$(check_wire "$c/inbox/t11.jsonl" 200 here "$c" 200)"
    c="$(new_case b2)"
    local_sh "$c" "$HOLDER; $FREEZE" > "$LOCAL/b2.holder" &
    lpid=$!
    if p="$(held_pid "$LOCAL/b2.holder")"; then
        LOCAL_PIDS+=("$p")
        send="$(peer_sh "$c" "M=sh-one; $SEND_ONE")"
        kill -CONT "$p"; wait "$lpid"
        verdict "(b) holder frozen here: the send on $PEER goes to the wire without waiting [$send]" \
            "$(check_wire "$c/inbox/t11.jsonl" 1 holder "$c" 1)$(case "$send" in FAILED*"did not answer"*) ;; *) echo " send said: $send" ;; esac)"
    else
        verdict "(b) holder frozen here" "the holder never took the lock"
    fi
    c="$(new_case c2)"
    local_sh "$c" "$HOLDER; exec sleep 300" > "$LOCAL/c2.holder" &
    lpid=$!
    if p="$(held_pid "$LOCAL/c2.holder")"; then
        LOCAL_PIDS+=("$p")
        kill -9 "$p"; wait "$lpid" 2>/dev/null
        send="$(peer_sh "$c" "M=sh-one; $SEND_ONE")"
        verdict "(c) holder killed here: the send on $PEER goes to the wire [$send]" \
            "$(check_wire "$c/inbox/t11.jsonl" 0 - "$c" 1)"
    else
        verdict "(c) holder killed here" "the holder never took the lock"
    fi
    echo "n/a: (b), (c) with the holder on $PEER — a host that never appends never holds the lock"
    echo "---"
    echo "PASS=$PASS FAIL=$FAIL"
    [ "$FAIL" -eq 0 ]
    exit
fi

# ---- (a1) Rust here, shell there, one inbox --------------------------------
c="$(new_case a1)"; mkfifo "$LOCAL/go.a1"; t0=$SECONDS
peer_sh "$c" "echo ready; read -r _; PACE=0.025; W=peer; $APPEND" < "$LOCAL/go.a1" > "$LOCAL/a1.peer" &
exec 7> "$LOCAL/go.a1"
rust_arm "$c" t11_rust_appends_200 > "$LOCAL/a1.rust" &
rpid=$!
for _ in $(seq 1 1200); do
    grep -q ready "$LOCAL/a1.peer" 2>/dev/null && [ -e "$c/inbox/rust.ready" ] && break; sleep 0.1
done
echo go >&7; touch "$c/inbox/go"; exec 7>&-
wait
proof_case "(a) Rust here and shell on $PEER, 200 each, paced, one inbox ($((SECONDS - t0))s)" \
    "$c/inbox/t11.jsonl" 400 "$LOCAL/a1.peer" "$LOCAL/a1.rust"

# ---- (a2) shell here, shell there, one inbox --------------------------------
c="$(new_case a2)"; mkfifo "$LOCAL/go.a2"; t0=$SECONDS
peer_sh "$c" "echo ready; read -r _; PACE=0.025; W=peer; $APPEND" < "$LOCAL/go.a2" > "$LOCAL/a2.peer" &
exec 7> "$LOCAL/go.a2"
local_sh "$c" "while [ ! -e \"\$SOT_COMM_HOME/inbox/go\" ]; do sleep 0.01; done; PACE=0.025; W=here; $APPEND" > "$LOCAL/a2.here" &
for _ in $(seq 1 600); do grep -q ready "$LOCAL/a2.peer" 2>/dev/null && break; sleep 0.1; done
echo go >&7; touch "$c/inbox/go"; exec 7>&-
wait
proof_case "(a) shell here and shell on $PEER, 200 each, paced, one inbox ($((SECONDS - t0))s)" \
    "$c/inbox/t11.jsonl" 400 "$LOCAL/a2.peer" "$LOCAL/a2.here"

# ---- (a3) liveness: shell here, shell there and the Rust filer, unpaced ------
# Three writers, 200 each, all at once, no pacing. Content as always, and every
# send must be `filed`: FAILED counts are reported per writer, any FAILED fails.
# The reader polls here throughout (B-1's proof: a shared lock on the real
# mount, under the nfs4 record) and once more after the writers finish.
c="$(new_case a3)"; mkfifo "$LOCAL/go.a3"; t0=$SECONDS
reader_sh "$c" comm-join.sh --name t11 >/dev/null 2>&1 || echo "  the reader could not join"
peer_sh "$c" "echo ready; read -r _; W=peer; $APPEND" < "$LOCAL/go.a3" > "$LOCAL/a3.peer" &
wpids=($!)
exec 7> "$LOCAL/go.a3"
SOT_T11_PACE_MS=0 rust_arm "$c" t11_rust_appends_200 > "$LOCAL/a3.rust" &
wpids+=($!)
local_sh "$c" "while [ ! -e \"\$SOT_COMM_HOME/inbox/go\" ]; do sleep 0.01; done; W=here; $APPEND" > "$LOCAL/a3.here" &
wpids+=($!)
for _ in $(seq 1 1200); do
    grep -q ready "$LOCAL/a3.peer" 2>/dev/null && [ -e "$c/inbox/rust.ready" ] && break; sleep 0.1
done
reader_loop "$c" "$LOCAL/a3.done" &
rdpid=$!
echo go >&7; touch "$c/inbox/go"; exec 7>&-
wait "${wpids[@]}"
touch "$LOCAL/a3.done"; wait "$rdpid"
failed_of() { grep -c '^FAILED' "$1" 2>/dev/null; }
nf_peer="$(failed_of "$LOCAL/a3.peer")"; nf_here="$(failed_of "$LOCAL/a3.here")"; nf_rust="$(failed_of "$LOCAL/a3.rust")"
a3_content="$([ "$((nf_peer + nf_here + nf_rust))" -eq 0 ] || echo "sends were FAILED under ordinary load: $(grep -h '^FAILED' "$LOCAL/a3.peer" "$LOCAL/a3.here" "$LOCAL/a3.rust" | head -2 | tr '\n' ' ')")$(check_inbox "$c/inbox/t11.jsonl" 600 "$LOCAL/a3.peer" "$LOCAL/a3.here" "$LOCAL/a3.rust")"
a3_ov="$(overlap "$c/inbox/t11.jsonl" 3)"
verdict "(a3) liveness, unpaced, 600 sends: FAILED peer $nf_peer, here $nf_here, Rust $nf_rust; content ($((SECONDS - t0))s)" "$a3_content"
verdict "(a3) liveness: overlap > 0 ($(jq -r .from "$c/inbox/t11.jsonl" 2>/dev/null | uniq | wc -l) writer runs, 3 writers)" "$a3_ov"
[ -z "$a3_content$a3_ov" ] || PROOF_BAD=$((PROOF_BAD + 1))
verdict "(a3) the reader here, the real comm-poll.sh: $(wc -l < "$LOCAL/reader.rc") polls, $(grep -c -x 75 "$LOCAL/reader.rc") exited 75; every line shown exactly once" \
    "$(check_reader "$c/inbox/t11.jsonl")"

# The inbox after a freeze: the holder's line whole, the frozen-out send absent.
check_after_freeze() {  # $1 = inbox file, $2 = the refused msg, $3 = send report
    local r
    r="$(check_inbox "$1" 1 /dev/null)"
    case "$r" in "a filed report and the lines disagree"|"") ;; *) echo "$r"; return ;; esac
    [ "$(jq -r .msg "$1")" = "holder-resumed" ] || { echo "the holder's line is not the one line"; return; }
    case "$3" in
        FAILED*"the inbox lock for @t11 was held for ${WAIT}s — nothing was appended after "*) ;;
        *) echo "send said: $3"; return ;;
    esac
    local ms="${3##* after }"; ms="${ms%ms}"
    [ "$ms" -ge $((WAIT * 1000)) ] || { echo "gave up after ${ms}ms, before the wait"; return; }
    echo ""
}

# ---- (b1) frozen holder there, Rust sender here -----------------------------
c="$(new_case b1)"
peer_sh "$c" "$HOLDER; $FREEZE" > "$LOCAL/b1.holder" &
lpid=$!
if p="$(held_pid "$LOCAL/b1.holder")"; then
    PEER_PIDS+=("$p")
    send="$(rust_arm "$c" t11_rust_sends_one | grep -E '^(filed|FAILED) ')"
    rpeer "kill -CONT $p"; wait "$lpid"
    verdict "(b) holder frozen on $PEER: the Rust sender here waits ${WAIT}s and fails; no torn line [$send]" \
        "$(check_after_freeze "$c/inbox/t11.jsonl" rust-one "$send")"
else
    verdict "(b) holder frozen on $PEER" "the holder never took the lock"
fi

# ---- (b2) frozen holder here, shell sender there ----------------------------
c="$(new_case b2)"
local_sh "$c" "$HOLDER; $FREEZE" > "$LOCAL/b2.holder" &
lpid=$!
if p="$(held_pid "$LOCAL/b2.holder")"; then
    LOCAL_PIDS+=("$p")
    send="$(peer_sh "$c" "M=sh-one; $SEND_ONE")"
    kill -CONT "$p"; wait "$lpid"
    verdict "(b) holder frozen here: the shell sender on $PEER waits ${WAIT}s and fails; no torn line [$send]" \
        "$(check_after_freeze "$c/inbox/t11.jsonl" sh-one "$send")"
else
    verdict "(b) holder frozen here" "the holder never took the lock"
fi

# A send after a -9: filed, within 3s, one whole line.
check_after_kill() {  # $1 = inbox file, $2 = send report
    case "$2" in filed*) ;; *) echo "send said: $2"; return ;; esac
    local ms="${2##* after }"; ms="${ms%ms}"
    [ "$ms" -lt 3000 ] || { echo "waited ${ms}ms for a dead holder"; return; }
    [ "$(wc -l < "$1")" -eq 1 ] && jq -e . "$1" >/dev/null 2>&1 || { echo "not one whole line"; return; }
    echo ""
}

# ---- (c1) holder killed there, Rust sender here -----------------------------
c="$(new_case c1)"
peer_sh "$c" "$HOLDER; exec sleep 300" > "$LOCAL/c1.holder" &
lpid=$!
if p="$(held_pid "$LOCAL/c1.holder")"; then
    PEER_PIDS+=("$p")
    rpeer "kill -9 $p"; wait "$lpid" 2>/dev/null
    send="$(rust_arm "$c" t11_rust_sends_one | grep -E '^(filed|FAILED) ')"
    verdict "(c) holder killed on $PEER: the Rust sender here files at once [$send]" \
        "$(check_after_kill "$c/inbox/t11.jsonl" "$send")"
else
    verdict "(c) holder killed on $PEER" "the holder never took the lock"
fi

# ---- (c2) holder killed here, shell sender there ----------------------------
c="$(new_case c2)"
local_sh "$c" "$HOLDER; exec sleep 300" > "$LOCAL/c2.holder" &
lpid=$!
if p="$(held_pid "$LOCAL/c2.holder")"; then
    LOCAL_PIDS+=("$p")
    kill -9 "$p"; wait "$lpid" 2>/dev/null
    send="$(peer_sh "$c" "M=sh-one; $SEND_ONE")"
    verdict "(c) holder killed here: the shell sender on $PEER files at once [$send]" \
        "$(check_after_kill "$c/inbox/t11.jsonl" "$send")"
else
    verdict "(c) holder killed here" "the holder never took the lock"
fi

n=0; for w in "$DIR"/*/wire.log; do [ -e "$w" ] && n=$((n + $(wc -l < "$w"))); done
verdict "no send on either side went to the wire" "$([ "$n" -eq 0 ] || echo "$n wire frames")"
verdict "proof: every (a) case, a3 included, has content PASS and overlap > 0" "$([ "$PROOF_BAD" -eq 0 ] || echo "$PROOF_BAD proof case(s) failed content or had no overlap")"
echo "---"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]

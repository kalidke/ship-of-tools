#!/usr/bin/env bash
# test-inbox-lock-onehost.sh — the inbox lock (0031 B1) on ONE machine whose
# comm folder is on NFSv3 or another mount whose lock is unknown. There the
# record binds the folder to the machine that wrote it (`none@<machine-id>`),
# and the claim is that every writer on that machine — the daemon's filer
# threads (Rust) and many script senders (shell) — goes through that one
# machine's lock, so no filed line is torn. Everything runs ON --host, in a
# fresh folder on the shared home:
#
#   - the record, written by the Rust arm as a hub-less daemon, reads
#     `none@<the host's machine id>`, and both arms route locally there;
#   - 8 script senders (separate processes through `sot_inbox_append`, 50
#     lines each) and 4 filer threads in ONE process (50 each), every line
#     carrying writer and sequence and 2 KiB of padding, run together with a
#     helper killed with -9 while it holds the lock mid-line, and a frozen
#     holder (takes the lock, writes half its line, SIGSTOPped 3 s,
#     SIGCONTed, finishes it);
#   - verdict: every line whose writer reported `filed` is present exactly
#     once and parses with its padding intact, so no two lines interleave;
#     nothing is unparseable, because the next writer cut the killed helper's
#     partial line back to the last newline (a final send after every writer
#     guarantees a next writer); every FAILED line is absent;
#   - and from THIS machine, a script send against the same folder goes to
#     the wire, never local.
#   - the reader: this tree's real comm-poll.sh, joined as t1h, polls ON
#     --host in a loop while the writers run, under the `none@` record, and
#     once more after the final send; every poll exits 0 or 75 and prints no
#     lock WARNING (a lock error is a WARNING line and exit 0, never an exit
#     status), and every line is shown exactly once, none skipped, none twice,
#     the killed helper's partial never. It is NOT B-1's proof: under `none@`
#     the flock is the host's own kernel lock, so the open mode cannot matter.
#     The a3 reader in test-inbox-lock-twohost.sh, on the shared mount, is.
#
# The wire is a stub that records each frame and never answers, so no daemon
# is ever dialled; the folder is removed on exit and ~/.sot-comm is never
# touched. The Rust arm is tests/comm_file.rs's ignored cases, built here and
# copied beside the inbox: this machine's build first, a musl build when that
# one will not start on --host.
#
# Needs a real NFSv3 host on this home, so it runs in no workflow (like
# test-inbox-lock-twohost.sh). Requires cargo and jq here.
#
# It keeps the real HOME by design: its scratch home must live on the shared
# mount every host sees: a fresh mktemp folder beside ~/.sot-comm, never
# under it. Sourcing lib-home-guard.sh drops the host's comm identity and
# daemon route.
#
# Usage: comm/tests/test-inbox-lock-onehost.sh --host HOST
# Exit: 0 all pass, 1 any fail, 2 usage.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home

HOST=""
while [ $# -gt 0 ]; do
    case "$1" in
        --host) HOST="${2:-}"; shift 2 || shift ;;
        *) break ;;
    esac
done
[ -n "$HOST" ] || { echo "usage: test-inbox-lock-onehost.sh --host HOST  (a machine on this home whose mount is NFSv3)" >&2; exit 2; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUST_DIR="$(cd "$SCRIPT_DIR/../../rust" && pwd)"

DIR="$(mktemp -d "$HOME/.sot-inbox-lock-1h-XXXXXX")" || { echo "FATAL: mktemp under \$HOME failed" >&2; exit 1; }
LOCAL="$(mktemp -d "${TMPDIR:-/tmp}/sot-inbox-lock-1h-XXXXXX")" || exit 1
trap 'rhost "pkill -9 -f $(printf %q "$DIR")" 2>/dev/null; rm -rf "${DIR:?}" "${LOCAL:?}"' EXIT
SCRIPTS_DIR="$(guard_stage_bin "$DIR")" || exit 2
mkdir -p "$DIR/lib" "$DIR/bin" "$DIR/inbox"
cp "$SCRIPTS_DIR"/comm-lib*.sh "$DIR/lib/"
cp -r "$SCRIPTS_DIR" "$DIR/scripts"
cat > "$DIR/lib/wire-stub.sh" <<'STUB'
sot_daemon_endpoint() { printf 'unix:/onehost-stub'; }
sot_relay_endpoint() { printf 'unix:/onehost-stub'; }
sot_oneshot_request() { printf '%s\n' "$1" >> "$SOT_COMM_HOME/wire.log"; return 1; }
STUB

rhost() { ssh -o BatchMode=yes "$HOST" "$@"; }
PRELUDE="source $(printf %q "$DIR/lib/comm-lib.sh"); source $(printf %q "$DIR/lib/wire-stub.sh")"
host_sh()  { rhost "SOT_COMM_HOME=$(printf %q "$1") bash -c $(printf %q "$PRELUDE; $2")"; }
local_sh() { SOT_COMM_HOME="$1" bash -c "$PRELUDE; $2"; }
rust_on_host() {  # $1 = test name; the case folder is $DIR
    rhost "SOT_TEST_INBOX_DIR=$(printf %q "$DIR/inbox") $(printf %q "$DIR/bin/comm_file") $1 --ignored --exact --nocapture"
}
# The comm_file test binary for $1 ("" = this machine's target), copied to $DIR/bin.
build_rust() {
    local exe
    exe="$( cd "$RUST_DIR" && cargo test -q -p sot-backend --test comm_file --no-run ${1:+--target "$1"} \
        --message-format=json 2>"$LOCAL/cargo.err" \
        | jq -r 'select(.reason == "compiler-artifact" and .target.name == "comm_file") | .executable // empty' )"
    [ -n "$exe" ] && cp "$exe" "$DIR/bin/comm_file"
}

PASS=0; FAIL=0
verdict() {  # $1 = description, $2 = "" for pass or the reason
    if [ -z "$2" ]; then echo "PASS: $1"; PASS=$((PASS + 1)); else echo "FAIL: $1 — $2"; FAIL=$((FAIL + 1)); fi
}

echo "host: $HOST; working folder on the shared home"
echo "mount there: $(rhost "findmnt -no FSTYPE,OPTIONS -T $(printf %q "$DIR") | tr , '\n' | grep -E '^(nfs|vers|local_lock)' | tr '\n' ' '; flock --version | head -1; perl -e 'print \"perl \$^V\"'")"
rhost "test -r $(printf %q "$DIR/lib/comm-lib.sh")" || { echo "FATAL: $HOST cannot see the working folder" >&2; exit 1; }

# ---- the Rust arm on the host: this machine's build, else musl -------------
RUST_BUILD=""
if build_rust "" && rhost "$(printf %q "$DIR/bin/comm_file") --list >/dev/null 2>&1"; then
    RUST_BUILD="this machine's build"
elif build_rust x86_64-unknown-linux-musl && rhost "$(printf %q "$DIR/bin/comm_file") --list >/dev/null 2>&1"; then
    RUST_BUILD="the musl build (this machine's would not start there)"
else
    echo "FATAL: the Rust arm will not start on $HOST:"; cat "$LOCAL/cargo.err"; exit 1
fi
echo "rust arm on $HOST: $RUST_BUILD"

# ---- the record: a hub-less daemon on the host binds the folder to it ------
HOST_MID="$(rhost 'cat /etc/machine-id' | tr -d '[:space:]')"
rust_on_host t11_write_lock_record > "$LOCAL/record.out" 2>&1
REC1="$(sed -n 1p "$DIR/inbox-lock-manager" 2>/dev/null)"; REC2="$(sed -n 2p "$DIR/inbox-lock-manager" 2>/dev/null)"
echo "record (Rust, on $HOST): $REC1 / $REC2"
[ -n "$HOST_MID" ] && [ "$REC1" = "none@$HOST_MID" ] && [ "$REC2" = "$HOST_MID" ] || {
    echo "FATAL: the record is not none@<$HOST's machine id>, so this is not a one-host case:"; cat "$LOCAL/record.out"; exit 1; }
verdict "the record names $HOST's own machine: none@<its machine id>, then that id" ""

r="$(host_sh "$DIR" 'sot_inbox_lock_identity "$INBOX_DIR"; [ -n "$(_sot_inbox_lock_ours "$INBOX_DIR")" ] && echo local || echo wire')"
verdict "a script on $HOST computes the record's identity and routes local" \
    "$([ "$r" = "none@$HOST_MID"$'\n'"local" ] || echo "got: $r")"

# ---- many writers on the host at once --------------------------------------
PAD="$(printf '%02048d' 0 | tr 0 x)"
PARTIAL='{"from":"killed","to":"t1h","repo":"r","msg":"killed-half'
cat > "$DIR/lib/run.sh" <<'RUN'
# Runs ON the host. $1 = the case folder, $2 = the padding.
set -u
C="$1"; PAD="$2"; LIB="$(dirname "$0")"
export SOT_COMM_HOME="$C"
mkdir -p "$C/rep" "$C/reader"
WP=()
for k in 1 2 3 4 5 6 7 8; do
    W="sh$k" PAD="$PAD" bash -c 'source "$1/comm-lib.sh"; source "$1/wire-stub.sh"
        while [ ! -e "$SOT_COMM_HOME/inbox/go" ]; do sleep 0.01; done
        for i in $(seq 0 49); do
            if printf "{\"from\":\"%s\",\"to\":\"t1h\",\"repo\":\"r\",\"msg\":\"%s-%s %s\",\"ts\":\"t\"}\n" "$W" "$W" "$i" "$PAD" \
                | sot_inbox_append t1h >/dev/null; then echo "filed $W-$i"; else echo "FAILED $W-$i"; fi
        done' _ "$LIB" > "$C/rep/sh$k" 2>&1 &
    WP+=($!)
done
SOT_TEST_INBOX_DIR="$C/inbox" "$C/bin/comm_file" onehost_four_filer_threads --ignored --exact --nocapture > "$C/rep/rust" 2>&1 &
WP+=($!)
# The reader: the real comm-poll.sh with a pinned self file, host and
# SOT_COMM_HOME; each poll's exit status is one line of reader/rc.
rd() { SOT_COMM_HOME="$C" SOT_COMM_SELF_FILE="$C/reader/self.txt" SOT_COMM_TEST_HOST=t1h-reader "$C/scripts/$1" "${@:2}"; }
rd comm-join.sh --name t1h > /dev/null 2>&1 || echo "the reader could not join"
( stop=""
  while [ -z "$stop" ]; do
      [ ! -e "$C/reader/stop" ] || stop=1
      rc=0; rd comm-poll.sh >> "$C/reader/out" 2>&1 || rc=$?
      echo "$rc" >> "$C/reader/rc"
  done ) &
R=$!
sleep 1; touch "$C/inbox/go"; sleep 0.3
lines() { wc -l < "$C/inbox/t1h.jsonl" 2>/dev/null || echo 0; }
# A frozen holder: half its line, SIGSTOP, 3 s, SIGCONT, the rest. It writes
# raw, so it goes first: the tail it lands on is always a whole line.
bash -c 'exec 9>> "$1/inbox/t1h.lock"; flock 9; exec 8>> "$1/inbox/t1h.jsonl"
    printf "%s" "{\"from\":\"holder\",\"to\":\"t1h\",\"repo\":\"r\"," >&8
    kill -STOP $$
    printf "%s\n" "\"msg\":\"holder-frozen $2\",\"ts\":\"t\"}" >&8 && echo "filed holder-frozen"' _ "$C" "$PAD" > "$C/rep/holder" 2>&1 &
H=$!
for n in $(seq 1 3000); do [ "$(awk '{print $3}' "/proc/$H/stat" 2>/dev/null)" = T ] && break; sleep 0.01; done
echo "holder $H frozen holding the lock (state $(awk '{print $3}' "/proc/$H/stat" 2>/dev/null)) at line $(lines)"
sleep 3; kill -CONT "$H"; wait "$H"
sleep 0.2
# A helper killed with -9 while it holds the lock, half a line written; the
# writers still running end it with their newline. Its `touch` closes fds 8 and 9,
# so no child keeps the lock after the kill.
bash -c 'exec 9>> "$1/inbox/t1h.lock"; flock 9; exec 8>> "$1/inbox/t1h.jsonl"
    printf "%s" "{\"from\":\"killed\",\"to\":\"t1h\",\"repo\":\"r\",\"msg\":\"killed-half" >&8
    touch "$1/killed-ready" 8>&- 9>&-; exec sleep 60' _ "$C" &
K=$!
for n in $(seq 1 3000); do [ -e "$C/killed-ready" ] && break; sleep 0.01; done
kill -9 "$K"; wait "$K" 2>/dev/null
echo "helper $K killed holding the lock mid-line ($([ -e "$C/killed-ready" ] && echo ready || echo NOT ready)) at line $(lines)"
wait "${WP[@]}"
echo "all writers done"
# One more send after everything: the killed helper's partial line is cut by
# the next writer, and this one guarantees there is one.
bash -c 'source "$1/comm-lib.sh"; source "$1/wire-stub.sh"
    printf "{\"from\":\"final\",\"to\":\"t1h\",\"repo\":\"r\",\"msg\":\"final-0 %s\",\"ts\":\"t\"}\n" "$2" \
        | sot_inbox_append t1h >/dev/null && echo "filed final-0" || echo "FAILED final-0"' _ "$LIB" "$PAD" > "$C/rep/final" 2>&1
touch "$C/reader/stop"; wait "$R"
echo "the reader is done: $(wc -l < "$C/reader/rc") polls"
RUN
C="$DIR"
t0=$(date +%s)
rhost "bash $(printf %q "$DIR/lib/run.sh") $(printf %q "$C") $PAD" > "$LOCAL/run.out" 2>&1
sed 's/^/  /' "$LOCAL/run.out"
echo "  ($(( $(date +%s) - t0 ))s)"
grep -q '^route .*: Local$' "$C/rep/rust" || { echo "  rust arm: $(grep -v '^filed\|^FAILED' "$C/rep/rust" | head -5)"; }

# ---- the verdict -----------------------------------------------------------
F="$C/inbox/t1h.jsonl"
grep -h '^filed ' "$C"/rep/* | awk '{print $2}' | sort > "$LOCAL/filed"
grep -h '^FAILED ' "$C"/rep/* | awk '{sub(/:$/, "", $2); print $2}' | sort > "$LOCAL/failed"
jq -R -r --arg pad "$PAD" '. as $l | try (fromjson
        | if type == "object" and (.msg | split(" ") | length == 2 and .[1] == $pad)
          then "OK\t" + (.msg | split(" ") | .[0]) else "BAD\t" + $l end)
    catch ("BAD\t" + $l)' "$F" > "$LOCAL/lines"
grep '^OK' "$LOCAL/lines" | cut -f2 | sort > "$LOCAL/keys"
grep '^BAD' "$LOCAL/lines" | cut -f2- > "$LOCAL/bad"
N_FILED=$(wc -l < "$LOCAL/filed"); N_FAILED=$(wc -l < "$LOCAL/failed")
N_LINES=$(wc -l < "$F"); N_BAD=$(wc -l < "$LOCAL/bad")
# Runs of one writer in a row: near 13 would mean the writers took turns
# whole, and a concurrency case that did not run concurrently proves nothing.
RUNS="$(jq -R -r 'fromjson? | .from' "$F" | uniq | wc -l)"
MIXED="$(jq -R -r 'fromjson? | .from | sub("[0-9]+$"; "")' "$F" | uniq | wc -l)"
echo "counts: filed $N_FILED, FAILED $N_FAILED, lines $N_LINES, unparseable $N_BAD; writer runs $RUNS, script/filer alternations $MIXED"
echo "order by arm (first 12 runs): $(jq -R -r 'fromjson? | .from | sub("[0-9]+$"; "")' "$F" | uniq -c | head -n 12 | awk '{printf "%s x%s ", $2, $1}')"
verdict "every writer answered every line (8x50 script, 4x50 filer threads, the holder, the final send)" \
    "$([ $((N_FILED + N_FAILED)) -eq 602 ] || echo "$((N_FILED + N_FAILED)) answers, want 602")"
verdict "the inbox ends in a newline: no partial tail" "$([ -z "$(tail -c1 "$F")" ] || echo "a partial last line")"
verdict "nothing is unparseable: the killed helper's partial line was cut, never kept" \
    "$([ "$N_BAD" -eq 0 ] && ! grep -q 'killed-half' "$F" || { echo "$N_BAD unparseable:"; cut -c1-120 "$LOCAL/bad" | head -3; })"
verdict "every filed line is present exactly once, whole, and nothing else is" \
    "$(diff "$LOCAL/filed" "$LOCAL/keys" >/dev/null || { echo "filed vs lines:"; diff "$LOCAL/filed" "$LOCAL/keys" | head -5; })"
verdict "every FAILED line is absent" \
    "$(comm -12 "$LOCAL/failed" "$LOCAL/keys" | head -3)"
verdict "the frozen holder's line landed whole" "$(grep -qx 'holder-frozen' "$LOCAL/keys" || echo "absent or torn")"
echo "inbox tail after the final send (last three lines):"; tail -n 3 "$F" | cut -c1-150 | sed 's/^/  /'
verdict "the filer threads routed local on $HOST" "$(grep -q '^route .*: Local$' "$C/rep/rust" || echo "no local route")"

# ---- the reader on the host ------------------------------------------------
sed -n 's/^\[[^]]*\] \[[^]]*\] //p' "$C/reader/out" | awk '{print $1}' | sort > "$LOCAL/shown"
N_POLLS=$(wc -l < "$C/reader/rc"); N_75=$(grep -c -x 75 "$C/reader/rc")
BAD_RC="$(grep -v -x -E '0|75' "$C/reader/rc" | sort | uniq -c | awk '{printf "%s x exit %s ", $1, $2}')"
N_WARN=$(grep -c -F 'WARNING: the inbox lock' "$C/reader/out")
N_TWICE=$(uniq -d "$LOCAL/shown" | wc -l)
N_SKIP=$(comm -13 <(uniq "$LOCAL/shown") "$LOCAL/keys" | wc -l)
N_EXTRA=$(comm -23 <(uniq "$LOCAL/shown") "$LOCAL/keys" | wc -l)
verdict "the reader on $HOST, the real comm-poll.sh: $N_POLLS polls, $N_75 exited 75; every line shown exactly once" \
    "${BAD_RC:+polls exited $BAD_RC}$([ "$N_WARN" -eq 0 ] || echo "$N_WARN polls warned the lock failed; ")$([ $((N_SKIP + N_TWICE + N_EXTRA)) -eq 0 ] || echo "skipped $N_SKIP, shown twice $N_TWICE, shown but not in the inbox $N_EXTRA")"

# ---- another machine against the same folder -------------------------------
before="$(wc -l < "$F")"
r="$(local_sh "$C" '[ -n "$(_sot_inbox_lock_ours "$INBOX_DIR")" ] && echo local || echo wire
    printf "{\"from\":\"away\",\"to\":\"t1h\",\"repo\":\"r\",\"msg\":\"away-0\",\"ts\":\"t\"}\n" | sot_inbox_append t1h >/dev/null && echo filed || echo FAILED')"
verdict "a script send from this machine goes to the wire, never local" \
    "$([ "$r" = "wire"$'\n'"FAILED" ] && [ "$(wc -l < "$C/wire.log" 2>/dev/null)" = 1 ] && [ "$(wc -l < "$F")" = "$before" ] \
        || echo "route/verdict: $(echo $r), wire frames $(wc -l < "$C/wire.log" 2>/dev/null), lines $before -> $(wc -l < "$F")")"

echo "---"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]

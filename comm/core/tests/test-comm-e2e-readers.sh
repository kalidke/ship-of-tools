#!/usr/bin/env bash
# test-comm-e2e-readers.sh — an end-to-end run with the REAL readers on the
# real shared home (0031 B1's merge evidence). The reader path missed twice:
# a reader that opened the inbox lock write-only was refused by NFS, which
# would have been a total mail outage, and every harness test passed because
# none of them ran a real reader on a real mount.
#
# One scratch comm home under $HOME (shared by every host), the hub's record
# set to `nfs4`, and on each of three hosts the real readers for one handle:
# comm-watch.sh, comm-wake.sh in ping mode (only the three terminal seams the
# ping suite stubs are replaced: screen, input, row), comm-poll.sh in a loop
# and the Stop hook every 2-3 s. Two senders, this host and the v4 peer, each
# send to all three handles through the real comm-send.sh: 30 paced 200 ms,
# then 30 unpaced. The v3 host only reads (its sends would leave by the wire,
# and no daemon serves this scratch home): its identity differs from the
# record, so its reads run unlocked with the hashed cursor.
#
# It passes only if every send is `filed`; each handle's ids shown by
# comm-poll.sh are exactly the ids filed to it, once each; no poll exits
# anything but 0; every Stop-hook call returns within 5 s, blocks while mail
# is unread, and is silent after the final poll; in a strict phase (poll loops
# stopped, 60 sends 3 s apart, one at a time) each send gets its own wake ping
# before the next send to that handle, and the last poll shows exactly those
# messages; comm-watch.sh prints each directed line once; nothing pings or
# prints in the 60 s after the final polls (a reader that took the
# `<count> <crc>-<len>` cursor for a whole number would see every row unread
# forever); the wire stub saw 0 frames; every inbox is whole. In the concurrent
# phase a wake rightly skips a line a poll already read, so that phase only
# reports how many lines a ping covered first.
#
# Needs real boxes, so it runs in no workflow. Nothing here touches
# ~/.sot-comm or a live daemon; the scratch home is removed on exit.
#
# Usage: comm/core/tests/test-comm-e2e-readers.sh --peer HOST --v3-host HOST
# Exit: 0 all pass, 1 any fail, 2 usage.
set -uo pipefail

PEER=""; V3=""
while [ $# -gt 0 ]; do
    case "$1" in
        --peer) PEER="${2:-}"; shift 2 || shift ;;
        --v3-host) V3="${2:-}"; shift 2 || shift ;;
        *) break ;;
    esac
done
[ -n "$PEER" ] && [ -n "$V3" ] || {
    echo "usage: test-comm-e2e-readers.sh --peer HOST --v3-host HOST  (a v4 peer and a v3 host that mount this home)" >&2; exit 2; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"
HOOK="$(cd "$SCRIPT_DIR/../../adapters/claude/hooks" && pwd)/comm-status-idle.sh"
HERE="$(hostname -s)"
PACED="${E2E_PACED:-30}"; BURST="${E2E_BURST:-30}"; QUIET="${E2E_QUIET_SECS:-60}"
unset SOT_WORKSPACE_ID SOT_COMM_HOOKS SOT_COMM_NAME SOT_COMM_SELF_FILE SOT_COMM_TEST_HOST

D="$(mktemp -d -p "$HOME" .sot-e2e.XXXXXX)" || { echo "FATAL: mktemp under \$HOME failed" >&2; exit 1; }
E="$D/e2e"; L="$D/log"
export SOT_COMM_HOME="$D"
BG=()
cleanup() {
    local p t
    for t in here peer v3; do : > "$E/stop-$t" 2>/dev/null; done
    for p in "${BG[@]}"; do kill "$p" 2>/dev/null; done
    wait 2>/dev/null
    rm -rf -- "${D:?}"
}
trap cleanup EXIT
mkdir -p "$D/bin" "$D/proj" "$D/self" "$D/inbox" "$E" "$L"
cp -r "$SCRIPTS_DIR/." "$D/bin/"
cp "$HOOK" "$D/bin/comm-status-idle.sh"
# The wire, stubbed for every process: each frame is one line in wire.log and
# nothing answers, so a send that would leave by the wire is FAILED.
cat >> "$D/bin/comm-lib.sh" <<'STUB'

sot_daemon_endpoint() { printf 'unix:/e2e-stub'; }
sot_relay_endpoint() { printf 'unix:/e2e-stub'; }
sot_oneshot_request() { printf '%s\n' "$1" >> "$SOT_COMM_HOME/wire.log"; return 1; }
STUB

on() {  # HOST BODY — bash BODY there, in the scratch comm home
    local h="$1" body="export SOT_COMM_HOME=$(printf %q "$D"); $2"
    if [ "$h" = "$HERE" ]; then bash -c "$body"; else ssh -o BatchMode=yes "$h" "bash -c $(printf %q "$body")"; fi
}
now() { date +%s%3N; }
PASS=0; FAIL=0
verdict() {  # $1 = description, $2 = "" for pass or the reason
    if [ -z "$2" ]; then echo "PASS: $1"; PASS=$((PASS + 1)); else echo "FAIL: $1 — $2"; FAIL=$((FAIL + 1)); fi
}

# ---- setup ------------------------------------------------------------------
[ "$PEER" != "$HERE" ] && [ "$V3" != "$HERE" ] && [ "$PEER" != "$V3" ] || { echo "FATAL: the hosts must be three different machines" >&2; exit 2; }
ID='source "$SOT_COMM_HOME/bin/comm-lib.sh"; sot_inbox_lock_identity "$INBOX_DIR"'
for h in "$HERE" "$PEER" "$V3"; do
    on "$h" 'for c in jq flock perl findmnt awk; do command -v "$c" >/dev/null || { echo "FATAL: $c missing on $(hostname -s)" >&2; exit 1; }; done' || exit 1
done
id_here="$(on "$HERE" "$ID")"; id_peer="$(on "$PEER" "$ID")"; id_v3="$(on "$V3" "$ID")"
case "$id_here" in "nfs4 "*) ;; *) echo "STOP: the identity here is '$id_here', not nfs4 — this is not the mount the test is about" >&2; exit 1 ;; esac
printf '%s\n%s\n' "$id_here" "$(tr -d '\n' < /etc/machine-id)" > "$D/inbox-lock-manager"
verdict "the v4 peer computes the record's identity ($id_here)" "$([ "$id_peer" = "$id_here" ] || echo "peer computes '$id_peer'")"
verdict "the v3 host computes a different identity ($id_v3)" "$([ "$id_v3" != "$id_here" ] || echo "v3 host computes the same identity")"

# Clock skew of each remote against this host, for reading the timings.
skew_peer=0; skew_v3=0
for h in "$PEER" "$V3"; do
    a="$(now)"; r="$(on "$h" 'date +%s%3N')"; b="$(now)"
    sk=$((r - (a + b) / 2)); echo "clock skew $h: $sk ms"
    [ "$h" = "$PEER" ] && skew_peer=$sk || skew_v3=$sk
done

# Rows: readers on host e2e-reg, senders on host e2e-snd, so no send ever
# matches the recipient's host and pokes a daemon. Every process runs in
# $D/proj (env -C), so every identity shares one project root.
HANDLES=(e2e-here e2e-peer e2e-v3); SENDERS=(e2e-snd-here e2e-snd-peer)
join() {  # NAME HOST-TAG
    env -C "$D/proj" SOT_COMM_NAME="$1" SOT_COMM_SELF_FILE="$D/self/$2__ws-$1.txt" SOT_COMM_TEST_HOST="$2" \
        bash "$D/bin/comm-join.sh" --name "$1" >/dev/null 2>&1 || { echo "FATAL: join of $1 failed" >&2; exit 1; }
}
for h in "${HANDLES[@]}"; do join "$h" e2e-reg; done
for s in "${SENDERS[@]}"; do join "$s" e2e-snd; done
# The joins each announce themselves to the stubbed daemon; those are setup, not
# sends. Only agent.join frames may be there, and the count starts from zero.
[ "$(grep -c -v '"op":"agent.join"' "$D/wire.log" 2>/dev/null)" = 0 ] || { echo "FATAL: setup put a frame other than agent.join on the wire" >&2; exit 1; }
rm -f -- "${D:?}/wire.log"
[ "$(jq -r '.agents["e2e-v3"].host' "$D/registry.json")" = e2e-reg ] || { echo "FATAL: the rows are not registered as expected" >&2; exit 1; }

# ---- the helpers each host runs (copies from $E, visible on every host) ------
cat > "$E/wake.sh" <<'EOF'
#!/usr/bin/env bash
# The real comm-wake.sh, ping mode; only the terminal seams are replaced.
D="$1"; H="$2"; TAG="$3"
source "$D/bin/comm-wake.sh"
_comm_wake_row() { printf 'ws-e2e\n'; }
_comm_wake_pty_screen() { printf '%s' '{"payload":{"lines":["banner","❯"],"cursor":{"row":1,"col":2}}}'; }
_comm_wake_pty_input() { printf '%s ping\n' "$(date +%s%3N)" >> "$D/log/ping-$TAG.log"; printf '%s' '{"payload":{"ok":true,"enter_sent":true}}'; }
_comm_wake_main "$H" --deliver ping --owner $$
EOF
cat > "$E/reader.sh" <<'EOF'
#!/usr/bin/env bash
# The real readers for one handle on this host, until the stop file appears.
D="$1"; H="$2"; TAG="$3"
E="$D/e2e"; L="$D/log"
export SOT_COMM_HOME="$D" SOT_COMM_NAME="$H" SOT_COMM_SELF_FILE="$D/self/e2e-reg__ws-$H.txt" SOT_COMM_TEST_HOST=e2e-reg CLAUDE_CODE_SESSION_ID="e2e-$H"
unset SOT_WORKSPACE_ID SOT_COMM_HOOKS
now() { date +%s%3N; }
jitter() { local ms=$(( $1 + RANDOM % $2 )); sleep "$((ms / 1000)).$(printf %03d $((ms % 1000)))"; }
run() { env -C "$D/proj" "$@"; }
hook_once() {  # LABEL
    local s e rc out blk busy
    s="$(now)"; out="$(printf '{}' | run timeout 30 bash "$D/bin/comm-status-idle.sh" 2>&1)"; rc=$?; e="$(now)"
    blk=0; busy=0
    case "$out" in *'"decision":"block"'*) blk=1 ;; esac
    case "$out" in *"is being written"*) busy=1 ;; esac
    printf '%s rc=%s ms=%s block=%s busy=%s %s\n' "$e" "$rc" "$((e - s))" "$blk" "$busy" "$1" >> "$L/hook-$TAG.log"
}
poll_once() {  # LABEL
    local t rc out
    out="$(run bash "$D/bin/comm-poll.sh" 2>&1)"; rc=$?; t="$(now)"
    printf '%s rc=%s %s\n' "$t" "$rc" "$1" >> "$L/pollrc-$TAG.log"
    [ -z "$out" ] || printf '%s\n' "$out" | sed "s/^/$t /" >> "$L/pollout-$TAG.log"
    [ "$1" != final2 ] || [ -z "$out" ] || printf '%s\n' "$out" | sed "s/^/$t /" >> "$L/pollstrict-$TAG.log"
}
: > "$L/watch-$TAG.out"
run bash "$D/bin/comm-watch.sh" "$H" >> "$L/watch-$TAG.out" 2>&1 &
WATCH=$!
run bash "$E/wake.sh" "$D" "$H" "$TAG" >> "$L/wake-$TAG.err" 2>&1 &
WAKE=$!
( while [ ! -e "$E/pollstop-$TAG" ] && [ -d "$E" ]; do poll_once loop; jitter 500 1001; done ) &
POLLER=$!
( while [ ! -e "$E/stop-$TAG" ] && [ -d "$E" ]; do hook_once loop; jitter 2000 1001; done ) &
HOOKER=$!
sleep 3; : > "$E/ready-$TAG"
while [ ! -e "$E/pollstop-$TAG" ] && [ -d "$E" ]; do sleep 0.2; done
wait "$POLLER"
poll_once final1; : > "$E/finaldone1-$TAG"
while [ ! -e "$E/strictpoll-$TAG" ] && [ -d "$E" ]; do sleep 0.2; done
poll_once final2; now > "$L/final-$TAG"; hook_once final
wc -l < "$L/watch-$TAG.out" > "$L/watch-count-final-$TAG"; : > "$E/finaldone-$TAG"
while [ ! -e "$E/stop-$TAG" ] && [ -d "$E" ]; do sleep 0.2; done
wc -l < "$L/watch-$TAG.out" > "$L/watch-count-end-$TAG"
kill "$HOOKER" 2>/dev/null
for p in "$WAKE" "$WATCH"; do pkill -P "$p" 2>/dev/null; kill "$p" 2>/dev/null; done
wait 2>/dev/null
EOF
cat > "$E/sender.sh" <<'EOF'
#!/usr/bin/env bash
# One sender: PACED rounds 200 ms apart, then BURST unpaced, each to all three handles.
D="$1"; S="$2"; PACED="$3"; BURST="$4"
E="$D/e2e"
export SOT_COMM_HOME="$D" SOT_COMM_NAME="$S" SOT_COMM_SELF_FILE="$D/self/e2e-snd__ws-$S.txt" SOT_COMM_TEST_HOST=e2e-snd
unset SOT_WORKSPACE_ID SOT_COMM_HOOKS
send_round() {  # PREFIX ROUND
    local h id out rc
    for h in e2e-here e2e-peer e2e-v3; do
        id="m-$S-$h-$1$2"
        out="$(env -C "$D/proj" bash "$D/bin/comm-send.sh" "@$h" "$id" 2>&1)"; rc=$?
        case "$out" in *"filed -> @$h"*) [ "$rc" -eq 0 ] && st=filed || st=FAILED ;; *) st=FAILED ;; esac
        printf '%s %s %s %s %s\n' "$(date +%s%3N)" "$h" "$id" "$st" "$(printf '%s' "$out" | tr '\n' ' ')" >> "$D/log/send-$S.log"
    done
}
while [ ! -e "$E/go" ]; do sleep 0.05; done
for i in $(seq 1 "$PACED"); do send_round p "$i"; sleep 0.2; done
for i in $(seq 1 "$BURST"); do send_round b "$i"; done
: > "$E/senderdone-$S"
EOF
cat > "$E/send1.sh" <<'EOF'
#!/usr/bin/env bash
# One send, for the strict phase: START END HANDLE ID STATUS.
D="$1"; S="$2"; h="$3"; id="$4"
export SOT_COMM_HOME="$D" SOT_COMM_NAME="$S" SOT_COMM_SELF_FILE="$D/self/e2e-snd__ws-$S.txt" SOT_COMM_TEST_HOST=e2e-snd
unset SOT_WORKSPACE_ID SOT_COMM_HOOKS
st="$(date +%s%3N)"
out="$(env -C "$D/proj" bash "$D/bin/comm-send.sh" "@$h" "$id" 2>&1)"; rc=$?
case "$out" in *"filed -> @$h"*) [ "$rc" -eq 0 ] && r=filed || r=FAILED ;; *) r=FAILED ;; esac
printf '%s %s %s %s %s\n' "$st" "$(date +%s%3N)" "$h" "$id" "$r" >> "$D/log/strict-$S.log"
EOF

# ---- the run ----------------------------------------------------------------
hostof() { case "$1" in here) echo "$HERE" ;; peer) echo "$PEER" ;; v3) echo "$V3" ;; esac; }
launch() {  # HOST CMD...  — a helper of this script, reaped by it
    local h="$1"; shift
    if [ "$h" = "$HERE" ]; then bash "$@" >> "$L/helper-$h.out" 2>&1 &
    else ssh -o BatchMode=yes -o ServerAliveInterval=15 "$h" "bash $(printf '%q ' "$@")" >> "$L/helper-$h.out" 2>&1 &
    fi
    BG+=($!)
}
for t in here peer v3; do launch "$(hostof "$t")" "$E/reader.sh" "$D" "e2e-$t" "$t"; done
launch "$HERE" "$E/sender.sh" "$D" e2e-snd-here "$PACED" "$BURST"
launch "$PEER" "$E/sender.sh" "$D" e2e-snd-peer "$PACED" "$BURST"
for t in here peer v3; do
    for _ in $(seq 1 300); do [ -e "$E/ready-$t" ] && break; sleep 0.1; done
    [ -e "$E/ready-$t" ] || { echo "FATAL: the $t reader never came up" >&2; cat "$L"/helper-*.out >&2 2>/dev/null; exit 1; }
done
echo "readers up on $HERE $PEER $V3; sending"
: > "$E/go"
for s in "${SENDERS[@]}"; do
    for _ in $(seq 1 1800); do [ -e "$E/senderdone-$s" ] && break; sleep 0.2; done
    [ -e "$E/senderdone-$s" ] || { echo "FATAL: sender $s never finished" >&2; cat "$L"/helper-*.out >&2 2>/dev/null; exit 1; }
done
echo "senders done; waiting 10 s, then the final polls"
sleep 10
for t in here peer v3; do : > "$E/pollstop-$t"; done
for t in here peer v3; do
    for _ in $(seq 1 300); do [ -e "$E/finaldone1-$t" ] && break; sleep 0.1; done
    [ -e "$E/finaldone1-$t" ] || { echo "FATAL: the $t phase-1 final poll never finished" >&2; exit 1; }
done
echo "phase 1 final polls done; the strict wake phase: 10 per handle from each sender, 3 s apart, one at a time"
date +%s%3N > "$L/strict-start"
for i in $(seq 1 10); do
    for sn in here peer; do
        for h in "${HANDLES[@]}"; do
            on "$(hostof "$sn")" "bash $(printf %q "$E/send1.sh") $(printf %q "$D") e2e-snd-$sn $h m-e2e-snd-$sn-$h-s$i" >> "$L/helper-strict.out" 2>&1
            sleep 3
        done
    done
done
sleep 5
date +%s%3N > "$L/strict-end"
for t in here peer v3; do : > "$E/strictpoll-$t"; done
for t in here peer v3; do
    for _ in $(seq 1 300); do [ -e "$E/finaldone-$t" ] && break; sleep 0.1; done
    [ -e "$E/finaldone-$t" ] || { echo "FATAL: the $t final poll never finished" >&2; exit 1; }
done
echo "final polls done; ${QUIET} s of quiet"
sleep "$QUIET"
for t in here peer v3; do : > "$E/stop-$t"; done
wait "${BG[@]}" 2>/dev/null

# ---- the verdicts -----------------------------------------------------------
tag_of() { case "$1" in e2e-here) echo here ;; e2e-peer) echo peer ;; e2e-v3) echo v3 ;; esac; }
cat "$L"/send-*.log > "$L/send-all.log"
nsend="$(wc -l < "$L/send-all.log")"
want=$((2 * 3 * (PACED + BURST)))
bad="$(grep -c -v ' filed ' "$L/send-all.log")"
per=""; for s in "${SENDERS[@]}"; do for h in "${HANDLES[@]}"; do per="$per $s>$h:$(awk -v h="$h" '$2==h && $4=="filed"' "$L/send-$s.log" | wc -l)"; done; done
verdict "1. every send answered filed ($nsend of $want sends;$per)" \
    "$([ "$nsend" -eq "$want" ] && [ "$bad" -eq 0 ] || { echo "$nsend sends, $bad not filed"; grep -v ' filed ' "$L/send-all.log" | cut -c1-200 | awk 'NR<=5'; })"

cat "$L"/strict-*.log > "$L/strict-all.log"
nstrict="$(wc -l < "$L/strict-all.log")"; sbad="$(grep -c -v ' filed$' "$L/strict-all.log")"
verdict "1b. every strict-phase send answered filed ($nstrict of 60)" \
    "$([ "$nstrict" -eq 60 ] && [ "$sbad" -eq 0 ] || echo "$nstrict sends, $sbad not filed")"

for h in "${HANDLES[@]}"; do
    t="$(tag_of "$h")"
    { awk -v h="$h" '$2==h && $4=="filed" {print $3}' "$L/send-all.log"; awk -v h="$h" '$3==h && $5=="filed" {print $4}' "$L/strict-all.log"; } | sort > "$L/want-$t"
    grep -o ' m-[^ ]*$' "$L/pollout-$t.log" 2>/dev/null | tr -d ' ' | sort > "$L/shown-$t"
    nwant="$(wc -l < "$L/want-$t")"; nshown="$(wc -l < "$L/shown-$t")"
    miss="$(comm -23 "$L/want-$t" <(sort -u "$L/shown-$t") | awk 'NR<=3' | tr '\n' ' ')"
    dup="$(uniq -d "$L/shown-$t" | awk 'NR<=3' | tr '\n' ' ')"
    extra="$(comm -13 "$L/want-$t" <(sort -u "$L/shown-$t") | awk 'NR<=3' | tr '\n' ' ')"
    verdict "2. $h: $nshown shown of $nwant filed, each exactly once" \
        "$([ -z "$miss$dup$extra" ] || echo "missing: $miss; twice: $dup; unfiled: $extra")"
    sh2="$(grep -o ' m-[^ ]*$' "$L/pollstrict-$t.log" 2>/dev/null | tr -d ' ' | sort)"
    ws="$(grep -- '-s[0-9]*$' "$L/want-$t")"
    verdict "2b. $h: the last poll showed exactly the strict-phase messages ($(printf '%s' "$sh2" | grep -c .) of $(printf '%s' "$ws" | grep -c .))" \
        "$([ "$sh2" = "$ws" ] || echo "the last poll's ids differ from the strict-phase ids")"
done

allrc="$L/pollrc-all.log"; cat "$L"/pollrc-*.log > "$allrc"
npoll="$(wc -l < "$allrc")"; n75="$(grep -c ' rc=75 ' "$allrc")"; nnz="$(grep -c -v ' rc=0 ' "$allrc")"
verdict "3. no comm-poll.sh call exited anything but 0 ($npoll polls, $n75 exited 75, $nnz nonzero)" \
    "$([ "$nnz" -eq 0 ] || grep -v ' rc=0 ' "$allrc" | awk 'NR<=5')"

for h in "${HANDLES[@]}"; do
    t="$(tag_of "$h")"; hl="$L/hook-$t.log"
    nh="$(grep -c ' loop$' "$hl")"
    mx="$(sed -n 's/.* ms=\([0-9]*\) .*/\1/p' "$hl" | sort -n | tail -n 1)"
    nb="$(grep -c ' block=1 .* loop$' "$hl")"
    nbusy="$(grep -c ' busy=1 ' "$hl")"; nrc="$(grep -c -v ' rc=0 ' "$hl")"
    fin="$(grep ' final$' "$hl")"
    r=""
    [ "${mx:-99999}" -le 5000 ] || r="$r max ${mx}ms over 5 s;"
    [ "$nb" -gt 0 ] || r="$r never blocked while mail was unread;"
    [ "$nbusy" -eq 0 ] || r="$r $nbusy busy lines;"
    [ "$nrc" -eq 0 ] || r="$r $nrc nonzero exits;"
    case "$fin" in *" block=0 busy=0 final") ;; *) r="$r the last call after the final poll was: $fin;" ;; esac
    verdict "4. $h: Stop hook $nh calls, max ${mx} ms, $nb blocked while unread, 0 busy, silent after the final poll" "$r"
done

skew_of() { case "$1" in here|e2e-snd-here) echo 0 ;; peer|e2e-snd-peer) echo "$skew_peer" ;; v3) echo "$skew_v3" ;; esac; }
sst="$(cat "$L/strict-start")"
echo "wake, per handle (times normalised to this host's clock):"
for h in "${HANDLES[@]}"; do
    t="$(tag_of "$h")"; pl="$L/ping-$t.log"
    [ -e "$pl" ] || : > "$pl"
    rs="$(skew_of "$t")"
    awk -v s="$rs" '{print $1 - s}' "$pl" | sort -n > "$L/pings-norm-$t"
    # NOTE (concurrent phase): a line's first poll showing vs the first ping after its filing
    grep -E ' m-[^ ]*$' "$L/pollout-$t.log" | awk -v s="$rs" '{id=$NF; v=$1-s; if(!(id in f)||v<f[id])f[id]=v}END{for(i in f)print f[i],i}' > "$L/firstshown-$t"
    for sn in here peer; do
        awk -v h="$h" -v s="$(skew_of "e2e-snd-$sn")" '$2==h && $4=="filed"{print $1 - s, $3}' "$L/send-e2e-snd-$sn.log"
    done > "$L/filedat-$t"
    awk -v h="$h" -v ss="$sst" 'FILENAME==ARGV[1]{if($1<ss)p[++n]=$1;next} FILENAME==ARGV[2]{s[$2]=$1;next}
        {cov=0; for(i=1;i<=n;i++) if(p[i]>$1){fp=p[i];cov=1;break}
         if(cov && (!($2 in s) || s[$2]>=fp)) a++; else b++}
        END{printf "  NOTE %s: concurrent phase, %d lines covered by a ping first, %d shown by a poll first or never pinged\n", h, a+0, b+0}' "$L/pings-norm-$t" "$L/firstshown-$t" "$L/filedat-$t"
    # strict phase: every send has its own ping before the next send to that handle
    { for sn in here peer; do awk -v h="$h" -v s="$(skew_of "e2e-snd-$sn")" '$3==h{print $1 - s, $4}' "$L/strict-e2e-snd-$sn.log"; done; } | sort -n > "$L/strictsends-$t"
    nst="$(wc -l < "$L/strictsends-$t")"
    unp="$(awk 'FILENAME==ARGV[1]{p[++n]=$1;next} {st[++m]=$1; id[m]=$2}
        END{for(i=1;i<=m;i++){lo=st[i]; hi=(i<m)?st[i+1]:9e18; c=0; for(j=1;j<=n;j++) if(p[j]>=lo && p[j]<hi) c++; if(c<1) print id[i]}}' "$L/pings-norm-$t" "$L/strictsends-$t" | awk 'NR<=3' | tr '\n' ' ')"
    nsp="$(awk -v ss="$sst" '$1>=ss' "$L/pings-norm-$t" | wc -l)"
    verdict "5a. $h: strict phase, every one of its $nst sends got its own ping before the next send to it ($nsp pings since the phase began)" \
        "$([ "$nst" -eq 20 ] && [ -z "$unp" ] || echo "$nst sends; no ping before the next send after: $unp")"
    grep -o 'm-[^ ]*$' "$L/watch-$t.out" | sort > "$L/watched-$t"
    wdup="$(uniq -d "$L/watched-$t" | awk 'NR<=3' | tr '\n' ' ')"
    wmiss="$(comm -23 "$L/want-$t" <(sort -u "$L/watched-$t") | awk 'NR<=3' | tr '\n' ' ')"
    verdict "5b. $h: comm-watch.sh printed each directed line exactly once ($(wc -l < "$L/watched-$t") lines of $(wc -l < "$L/want-$t"))" \
        "$([ -z "$wdup$wmiss" ] || echo "missing: $wmiss; twice: $wdup")"
    fin="$(cat "$L/final-$t")"
    late="$(awk -v f="$fin" '$1>f' "$pl" | wc -l)"
    wl="$(( $(cat "$L/watch-count-end-$t") - $(cat "$L/watch-count-final-$t") ))"
    verdict "5c. $h: no wake storm ($late pings, $wl watch lines in the ${QUIET} s after the final poll)" \
        "$([ "$late" -eq 0 ] && [ "$wl" -eq 0 ] || echo "$late pings and $wl watch lines after the final poll")"
done

nwire=0; [ ! -e "$D/wire.log" ] || nwire="$(wc -l < "$D/wire.log")"
verdict "6. the wire stub recorded $nwire frames" "$([ "$nwire" -eq 0 ] || awk 'NR<=3' "$D/wire.log" | cut -c1-200)"

for h in "${HANDLES[@]}"; do
    f="$D/inbox/$h.jsonl"; r=""
    if [ ! -s "$f" ]; then r="no inbox"; else
        [ -z "$(tail -c1 "$f")" ] || r="an unterminated tail"
        n="$(wc -l < "$f")"; parsed="$(jq -c 'select(type == "object")' "$f" 2>/dev/null | wc -l)"
        [ "$parsed" -eq "$n" ] || r="$r $((n - parsed)) of $n lines are not one JSON object;"
        [ "$n" -eq "$(wc -l < "$L/want-$(tag_of "$h")")" ] || r="$r $n lines, want $(wc -l < "$L/want-$(tag_of "$h")");"
    fi
    verdict "7. inbox $h is whole ($(wc -l < "$f" 2>/dev/null) lines)" "$r"
done

echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]

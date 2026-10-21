#!/usr/bin/env bash
# interop-066.sh — the 0.7.0 install's interop check: a box on the 0.6.6 scripts and daemon beside one on this tree's
# `sotd comm`, with nothing on disk or on the wire converted (rust/backend/src/comm/CLAUDE.md, the install).
#
# Usage:
#   comm/tests/interop-066.sh WORK NEW_SOTD OLD_SOTD [OLD_TAG]
#       Parts 1 and 2 below, on this host, in WORK (made empty first; it must not exist or be empty).
#   comm/tests/interop-066.sh prepare SOTD COMM_HOME [OLD_TAG]
#       Part 3's folder: COMM_HOME (new or empty, on a network folder two hosts mount, outside any git checkout) with
#       @sink-shared joined and the inbox lock record the 0.6.6 library computes on this host.
#   comm/tests/interop-066.sh burst SOTD COMM_HOME N
#       Part 3's writer: N sends from @burst-<host> to @sink-shared through SOTD's own `sotd comm send`, as a session
#       outside any row. Run it on two hosts at once, then `count`.
#   comm/tests/interop-066.sh count COMM_HOME N HOSTS...
#       Part 3's check: @sink-shared's inbox holds exactly N whole lines from each @burst-<host>, each once.
#
# NEW_SOTD is this tree's build; OLD_SOTD is the 0.6.6 release's `sotd` (its Linux pack, checksum verified), and the
# 0.6.6 scripts come from OLD_TAG (default v0.6.6-rc9.15) by `git archive`, staged by that tree's own stager. Linux
# only (the 0.6.6 scripts need flock, perl and jq). Run by hand on a build host; it touches
# no live comm folder, daemon or row: every home, comm folder, socket and hosts.toml is under WORK, every session runs
# under `env -i`, and the only processes it ends are the two daemons it started.
#
# Part 1, one comm folder: a 0.6.6 session and a `sotd comm` session join, send to each other, and poll; each lists
#   the other from the one registry; each reads mail the other's poll left behind exactly once (one cursor format);
#   the inbox lock record the 0.6.6 library writes is the one `sotd comm send` agrees with, or it would not append.
# Part 2, a hub and a linked box on different versions, both ways round: the hub's session mails a handle only the
#   linked box lists, and the linked box's session mails one only the hub lists. Every ssh, the daemon's link and a
#   session's send alike, is a stub that runs the hub's own `sotd stdio-bridge` to the hub's socket, as a real login
#   to the hub would; `ssh -G` (the 0.6.6 library's configuration probe) prints nothing and reads no input.
# Part 3, two hosts on one network folder: concurrent `sotd comm send`s from both lose and tear no line, and each
#   host's lock manager is the record one host wrote (or its sends would go to a daemon there is none of).
set -u

fail() { echo "FAIL: $*"; exit 1; }
pass() { echo "ok   $*"; }

# bare SOTD COMM_HOME WHO HOST VERB ARGS... — `sotd comm VERB` as session WHO of HOST outside any row, no daemon. Its
# project folder is WHO, so its derived handle is WHO-HOST; COMM_HOME must lie outside any git checkout, or the
# checkout would be every session's project.
bare() {
    local sotd="$1" home="$2" who="$3" host="$4" w
    shift 4
    w="$home/../bare-$host-$who"
    mkdir -p "$w/p/$who" && chmod 700 "$w" || return 1
    (cd "$w/p/$who" && env -i PATH=/usr/bin:/bin HOME="$w" SOT_COMM_HOME="$home" SOT_COMM_SELF_FILE="$w/self.txt" \
        SOT_SELF_HOST="$host" SOT_COMM_TEST_HOST="$host" SOT_SOCKET="$w/no-daemon.sock" \
        SOT_HOSTS="$w/no-hosts.toml" "$sotd" comm "$@")
}

if [ "${1:-}" = prepare ]; then
    [ $# -ge 3 ] && [ $# -le 4 ] || { echo "usage: interop-066.sh prepare SOTD COMM_HOME [OLD_TAG]" >&2; exit 2; }
    sotd="$2" home="$3" tag="${4:-v0.6.6-rc9.15}"
    root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)" || exit 2
    if [ -e "$home" ]; then [ -z "$(ls -A "$home")" ] || fail "COMM_HOME is not empty: $home"; fi
    mkdir -p "$home/inbox" "$home/../old" || exit 1
    (cd "$root" && git archive "$tag" comm agents) | tar -x -C "$home/../old" || fail "git archive $tag"
    bash "$home/../old/comm/tests/stage-bin.sh" "$home/../old-bin" >/dev/null || fail "stage the $tag scripts"
    COMM_HOME="$home" bash -c '. "$1/comm-lib.sh" && sot_inbox_lock_identity "$2/inbox"' _ "$home/../old-bin" "$home" \
        > "$home/inbox-lock-manager" || fail "the 0.6.6 lock identity"
    out="$(bare "$sotd" "$home" sink shared join 2>&1)" || fail "join @sink-shared: $out"
    echo "ok   $home: @sink-shared joined, lock record $(head -1 "$home/inbox-lock-manager")"
    exit 0
fi

if [ "${1:-}" = burst ]; then
    [ $# -eq 4 ] || { echo "usage: interop-066.sh burst SOTD COMM_HOME N" >&2; exit 2; }
    sotd="$2" home="$3" n="$4"
    host="$(hostname -s)"
    out="$(bare "$sotd" "$home" burst "$host" join 2>&1)" || fail "join on $host: $out"
    i=0
    while [ "$i" -lt "$n" ]; do
        i=$((i + 1))
        out="$(bare "$sotd" "$home" burst "$host" send @sink-shared "line $i" 2>&1)" || fail "send $i on $host: $out"
    done
    echo "ok   $n sends from $host"
    exit 0
fi

if [ "${1:-}" = count ]; then
    [ $# -ge 4 ] || { echo "usage: interop-066.sh count COMM_HOME N HOSTS..." >&2; exit 2; }
    home="$2" n="$3"; shift 3
    f="$home/inbox/sink-shared.jsonl"
    total="$(wc -l < "$f")"
    whole="$(jq -c . < "$f" 2>/dev/null | wc -l)"
    [ "$total" = "$whole" ] || fail "$f: $total lines, $whole of them whole JSON"
    for h in "$@"; do
        got="$(jq -r --arg h "burst-$h" 'select(.from == $h) | .msg' < "$f" | sort -u | wc -l)"
        all="$(jq -r --arg h "burst-$h" 'select(.from == $h) | .msg' < "$f" | wc -l)"
        [ "$got" = "$all" ] || fail "@burst-$h: $all lines, $got distinct"
        [ "$got" = "$n" ] || fail "@burst-$h: $got distinct lines of $n"
        pass "@burst-$h: $n lines, each once"
    done
    exit 0
fi

[ $# -ge 3 ] && [ $# -le 4 ] || { echo "usage: interop-066.sh WORK NEW_SOTD OLD_SOTD [OLD_TAG]" >&2; exit 2; }
W="$1" NEW_SOTD="$2" OLD_SOTD="$3" TAG="${4:-v0.6.6-rc9.15}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)" || exit 2
. "$ROOT/comm/tests/lib-wait.sh" || exit 2
for b in "$NEW_SOTD" "$OLD_SOTD"; do [ -x "$b" ] || fail "not an executable: $b"; done
for t in jq flock perl findmnt; do command -v "$t" >/dev/null || fail "needs $t"; done
if [ -e "$W" ]; then [ -z "$(ls -A "$W")" ] || fail "WORK is not empty: $W"; fi
mkdir -p "$W" && chmod 700 "$W" && W="$(cd "$W" && pwd -P)" || exit 1

DAEMONS=""
end_daemons() {
    local p
    for p in $DAEMONS; do kill "$p" 2>/dev/null; done
    for p in $DAEMONS; do wait "$p" 2>/dev/null; done
    DAEMONS=""
}
trap end_daemons EXIT

mkdir -p "$W/old-tree" || exit 1
(cd "$ROOT" && git archive "$TAG" comm agents) | tar -x -C "$W/old-tree" || fail "git archive $TAG"
bash "$W/old-tree/comm/tests/stage-bin.sh" "$W/old-bin" >/dev/null || fail "stage the $TAG scripts"
bash "$ROOT/comm/tests/stage-bin.sh" "$W/new-bin" >/dev/null || fail "stage this tree's scripts"

# sess VERSION BOX WHO SCRIPT ARGS... — runs one wrapper as session WHO of box BOX (its home, comm folder, socket and
# hosts.toml under $W/BOX), with VERSION's scripts and sotd. Outside any row; its own self file.
sess() {
    local v="$1" box="$2" who="$3" script="$4" bin sotd
    shift 4
    case "$v" in old) bin="$W/old-bin" sotd="$OLD_SOTD" ;; new) bin="$W/new-bin" sotd="$NEW_SOTD" ;; esac
    mkdir -p "$W/$box/p/$who" "$W/$box/run" && chmod 700 "$W/$box/run"
    (cd "$W/$box/p/$who" && env -i PATH="$W/$box/stub:/usr/local/bin:/usr/bin:/bin" HOME="$W/$box/home" \
        SOT_COMM_HOME="$W/$box/home/.sot-comm" SOT_COMM_SELF_FILE="$W/$box/self-$who.txt" \
        SOT_SELF_HOST="$(cat "$W/$box/host")" SOT_COMM_TEST_HOST="$(cat "$W/$box/host")" \
        SOT_SOCKET="$W/$box/run/sotd.sock" SOT_HOSTS="$W/$box/hosts.toml" XDG_RUNTIME_DIR="$W/$box/run" \
        SOTD_BIN="$sotd" bash "$bin/$script" "$@")
}

# box NAME HOST — a box's folders; no daemon yet.
box() {
    mkdir -p "$W/$1/home/.sot-comm" "$W/$1/stub" "$W/$1/state" "$W/$1/config" "$W/$1/project" || exit 1
    printf '%s\n' "$2" > "$W/$1/host"
}

expect() {  # WHAT WANT GOT
    case "$3" in *"$2"*) pass "$1" ;; *) fail "$1: wanted '$2' in: $3" ;; esac
}

# ---- Part 1: one comm folder, both versions -------------------------------------------------------------------------
box one box1
out="$(sess old one alpha comm-join.sh 2>&1)"; expect "0.6.6 join" "Joined sot-comm as @alpha-box1" "$out"
out="$(sess new one beta comm-join.sh 2>&1)"; expect "sotd comm join" "Joined sot-comm as @beta-box1" "$out"
# The record a hub's daemon writes at its start, computed by the 0.6.6 library: `sotd comm send` appends itself only
# when its own lock manager is the same string.
COMM_HOME="$W/one/home/.sot-comm" bash -c '. "$1/comm-lib.sh" && sot_inbox_lock_identity "$2/inbox"' _ \
    "$W/old-bin" "$W/one/home/.sot-comm" > "$W/one/home/.sot-comm/inbox-lock-manager" || fail "the 0.6.6 lock identity"
[ -s "$W/one/home/.sot-comm/inbox-lock-manager" ] || fail "no lock identity for $W/one"

out="$(sess old one alpha comm-list.sh 2>&1)"; expect "0.6.6 list shows the sotd comm session" "beta-box1" "$out"
out="$(sess new one beta comm-list.sh 2>&1)"; expect "sotd comm list shows the 0.6.6 session" "alpha-box1" "$out"

out="$(sess old one alpha comm-send.sh @beta-box1 'from old one' 2>&1)"; expect "0.6.6 send to sotd comm" "filed -> @beta-box1" "$out"
out="$(sess new one beta comm-send.sh @alpha-box1 'from new one' 2>&1)"; expect "sotd comm send to 0.6.6" "filed -> @alpha-box1" "$out"
out="$(sess new one beta comm-poll.sh 2>&1)"; expect "sotd comm poll reads 0.6.6 mail" "[alpha-box1:alpha] from old one" "$out"
out="$(sess old one alpha comm-poll.sh 2>&1)"; expect "0.6.6 poll reads sotd comm mail" "[beta-box1:beta] from new one" "$out"

# One cursor: each version's poll leaves the cursor where the other's reads on from.
out="$(sess new one alpha comm-poll.sh 2>&1)"; expect "sotd comm poll after a 0.6.6 poll" "No new messages." "$out"
sess old one beta comm-send.sh @alpha-box1 'second' >/dev/null 2>&1 || fail "0.6.6 second send"
out="$(sess new one alpha comm-poll.sh 2>&1)"; expect "sotd comm poll reads on from the 0.6.6 cursor" "] second" "$out"
out="$(sess old one alpha comm-poll.sh 2>&1)"; expect "0.6.6 poll after a sotd comm poll" "No new messages." "$out"

out="$(sess old one alpha comm-context.sh 2>&1)"; expect "0.6.6 context" "NAME=alpha-box1" "$out"
out="$(sess new one alpha comm-context.sh 2>&1)"; expect "sotd comm context names the same session" "NAME=alpha-box1" "$out"
out="$(sess new one beta comm-leave.sh 2>&1)"
out="$(sess old one alpha comm-send.sh @beta-box1 'gone' 2>&1)"
case "$out" in *"filed -> "*) fail "0.6.6 send to a handle sotd comm left was filed: $out" ;; esac
pass "0.6.6 send after sotd comm leave is refused"

# ---- Part 2: a hub and a linked box on different versions -----------------------------------------------------------
# daemon BOX VERSION — starts BOX's daemon on VERSION's binary and waits for its socket.
daemon() {
    local b="$1" bin
    case "$2" in old) bin="$OLD_SOTD" ;; new) bin="$NEW_SOTD" ;; esac
    env -i PATH="$W/$b/stub:/usr/local/bin:/usr/bin:/bin" HOME="$W/$b/home" USERPROFILE="$W/$b/home" \
        LOCALAPPDATA="$W/$b/state" XDG_STATE_HOME="$W/$b/state" XDG_CONFIG_HOME="$W/$b/config" \
        SOT_SELF_HOST="$(cat "$W/$b/host")" SOT_RUNTIME_DIR="$W/$b/run" SOT_COMM_HOME="$W/$b/home/.sot-comm" \
        SOT_HOSTS="$W/$b/hosts.toml" \
        "$bin" --socket "$W/$b/run/sotd.sock" --project-root "$W/$b/project" < /dev/null > "$W/$b/sotd.log" 2>&1 &
    DAEMONS="$DAEMONS $!"
    await test -S "$W/$b/run/sotd.sock" && return 0
    fail "$b's daemon ($2) made no socket; its log: $(tail -5 "$W/$b/sotd.log")"
}

# mailed VERSION BOX TEXT — 0 when BOX's session @hubs files TEXT for @guests-box-b; the send's words in BOX/mailed.
mailed() {
    sess "$1" "$2" hubs comm-send.sh @guests-box-b "$3" > "$W/$2/mailed" 2>&1
    grep -q 'filed -> @guests-box-b' "$W/$2/mailed"
}

# pair HUBV GUESTV — a hub on HUBV and a linked box on GUESTV; one mail each way.
pair() {
    local hv="$1" gv="$2" k="$1-$2" hub guest out hubsotd
    hub="hub-$k" guest="guest-$k"
    case "$hv" in old) hubsotd="$OLD_SOTD" ;; new) hubsotd="$NEW_SOTD" ;; esac
    box "$hub" hub-a
    box "$guest" box-b
    for b in "$hub" "$guest"; do
        mkdir -p "$W/$b/run" && chmod 700 "$W/$b/run"
        printf 'hub = "hub-a"\n\n[host.hub-a]\ndaemon = true\n\n[host.box-b]\ndaemon = true\nfrontend = true\n' \
            > "$W/$b/hosts.toml"
        printf '#!/bin/sh\ncase " $* " in *" -G "*) exit 0 ;; esac\nexec %s stdio-bridge --endpoint unix:%s\n' \
            "$hubsotd" "$W/$hub/run/sotd.sock" > "$W/$b/stub/ssh" && chmod +x "$W/$b/stub/ssh"
    done
    daemon "$hub" "$hv"
    daemon "$guest" "$gv"
    out="$(sess "$hv" "$hub" hubs comm-join.sh 2>&1)"; expect "$k: hub session joins ($hv)" "Joined sot-comm as @hubs-hub-a" "$out"
    out="$(sess "$gv" "$guest" guests comm-join.sh 2>&1)"; expect "$k: linked session joins ($gv)" "Joined sot-comm as @guests-box-b" "$out"
    # The link comes up on the daemon's pace; the hub's answer is not_here until it does.
    await mailed "$hv" "$hub" "hub to box $k"
    out="$(cat "$W/$hub/mailed")"
    expect "$k: hub ($hv) mails the linked box ($gv)" "filed -> @guests-box-b" "$out"
    out="$(sess "$gv" "$guest" guests comm-poll.sh 2>&1)"; expect "$k: the linked box reads it" "hub to box $k" "$out"
    out="$(sess "$gv" "$guest" guests comm-send.sh @hubs-hub-a "box to hub $k" 2>&1)"
    expect "$k: the linked box ($gv) mails the hub ($hv)" "filed -> @hubs-hub-a" "$out"
    out="$(sess "$hv" "$hub" hubs comm-poll.sh 2>&1)"; expect "$k: the hub reads it" "box to hub $k" "$out"
    end_daemons
}

pair old new
pair new old
echo "PASS interop-066 ($TAG beside this tree)"

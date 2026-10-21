#!/usr/bin/env bash
# Suite bootstrap policy and behavior of the actual shared await and end_group.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)" || exit 1
T="$(mktemp -d "${TMPDIR:-/tmp}/sot-rm-guard-XXXXXX")" && [ -d "$T" ] || exit 1
trap 'rm -rf "${T:?}"' EXIT
guard_fresh_home "$T"
suites=()
while IFS= read -r f; do
    case "$f" in comm/*|agents/*) case "${f##*/}" in test-*.sh) suites+=("$REPO/$f") ;; esac ;; esac
done < <(git -C "$REPO" ls-files)
[ "${#suites[@]}" -ge 20 ] || { echo "FATAL: found ${#suites[@]} comm suites, expected at least 20"; exit 1; }
# unguarded SUITE... — each suite whose first command other than `set` is not
# the guard's source line.
unguarded() {
    local f
    for f in "$@"; do
        awk '/^[[:space:]]*#/ { next }
             !seen && !/^[[:space:]]*$/ && !/^[[:space:]]*set[[:space:]]/ {
                 seen = 1; first = ($0 ~ /^[[:space:]]*(\.|source)[[:space:]].*lib-home-guard\.sh/) }
             END { exit !(seen && !first) }' "$f" && printf '%s\n' "$f"
    done
}
printf 'set -u\necho x\n' > "$T/test-synthetic.sh"
[ "$(unguarded "$T/test-synthetic.sh")" = "$T/test-synthetic.sh" ] && [ -z "$(unguarded "$SCRIPT_DIR/test-status-floor.sh")" ] \
    || { echo "FAIL: the home-guard check does not tell test-status-floor.sh from a synthetic suite with no guard"; exit 1; }
bad="$(unguarded "${suites[@]}")"

# Two policies of the process-group rule (CLAUDE.md, timing): a hermetic suite runs no pkill or pgrep (Git Bash has
# neither), and a stub's pid (STUB_*_PID=$!) is taken between `set -m` and `set +m`, so end_group can end its group.
# pg_calls SUITE... — the lines that run pkill or pgrep as a command, in each suite that is hermetic.
pg_calls() {
    local f b
    for f in "$@"; do
        grep -nE '^[^#]*(^|[;&|(`]|\$\(|command -p )[[:space:]]*(pkill|pgrep)[[:space:]]+-' "$f" | sed "s#^#$f:#"
    done
}
# stub_pids_outside_m SUITE... — the lines that take a STUB_*_PID with `set -m` not in force.
stub_pids_outside_m() {
    local f
    for f in "$@"; do
        awk '/^[[:space:]]*#/ { next }
             /set -m/ { m = 1 } /set \+m/ { m = 0 }
             /STUB_[A-Z_]*_PID=\$!/ && !m { print FILENAME ":" FNR ": " $0 }' "$f"
    done
}
printf 'pkill -P 1\nx=$(command -p pgrep -P 2)\n# pkill -f no\n' > "$T/test-synthetic.sh"
printf 'sleep 1 &\nSTUB_NC_PID=$!\nset -m\nSTUB_X_PID=$!\nset +m\nSTUB_Y_PID=$!\n' > "$T/test-synthetic2.sh"
[ "$(pg_calls "$T/test-synthetic.sh" | wc -l)" -eq 2 ] && [ "$(stub_pids_outside_m "$T/test-synthetic2.sh" | wc -l)" -eq 2 ] \
    || { echo "FAIL: the pkill and set -m scans do not tell a synthetic offender from a clean suite"; exit 1; }
# A bare `sleep N` in a test is a fixed wait: it does not end when the thing it waits for happens, and it is too short
# on a slow host. A suite waits with lib-wait.sh's `await`. Every file under comm/tests and agents/tests (.sh, .py) but
# lib-wait.sh has the count of its non-comment lines with a `sleep` followed by a number pinned in the table below, with
# its reason; a file not in the table has none. The count can only fall: a fall also fails, so the table is lowered in
# the same commit. The pattern is at its command position or in text a suite writes, as `sleep 2 |` in a stub.
SLEEP_RX='(^|[^[:alnum:]_.$/-])sleep +[0-9]'
# sleep_counts FILE... — "FILE COUNT" for each file with a bare sleep.
sleep_counts() {
    local f n
    for f in "$@"; do
        n="$(awk -v rx="$SLEEP_RX" '/^[[:space:]]*#/ { next } $0 ~ rx { n++ } END { print n + 0 }' "$f")"
        [ "$n" -eq 0 ] || printf '%s %s\n' "${f#"$REPO"/}" "$n"
    done
}
sleep_files=()
while IFS= read -r f; do
    case "$f" in comm/tests/*|agents/tests/*) case "$f" in *.sh|*.py) [ "${f##*/}" = lib-wait.sh ] || sleep_files+=("$REPO/$f") ;; esac ;; esac
done < <(git -C "$REPO" ls-files)
# FILE COUNT REASON
read -r -d '' SLEEP_ALLOWED <<'ALLOW'
agents/tests/test-sot-fe-paths.sh 1 polls for the stand-in's socket, 100 tries of 0.1 s; moves to await when the file is next changed
comm/tests/lib-temp-files.sh 1 the stub jq that hangs until the signal under test ends it
comm/tests/test-endpoint-gate.sh 4 children that run until the case ends them, and a stub pgrep's printed text
comm/tests/test-heartbeat-ctx-wait.py 4 the generated bodies' own poll loops, which the suite's frozen observations time
comm/tests/test-rm-guard.sh 4 this file's synthetic offender text and the pipeline members end_group is run on
ALLOW
# sleep_unlisted COUNTS — the lines of COUNTS ("FILE COUNT") that differ from SLEEP_ALLOWED, and each allowed file missing.
sleep_unlisted() {
    awk 'NR == FNR { want[$1] = $2; next }
         { have[$1] = $2; if (!($1 in want)) print "unlisted: " $1 " has " $2 " bare sleep line(s)";
           else if (want[$1] != $2) print "changed: " $1 " has " $2 ", listed " want[$1] }
         END { for (f in want) if (!(f in have)) print "stale: " f " is listed with " want[f] " and has none" }' \
        <(printf '%s\n' "$SLEEP_ALLOWED") "$1"
}
printf 'x\n%s 5\n' "sle""ep" > "$T/test-synthetic3.sh"
[ "$(sleep_unlisted <(sleep_counts "$T/test-synthetic3.sh") | grep -c '^unlisted: ')" -eq 1 ] || { echo "FAIL: the bare sleep scan does not find a synthetic sleep"; exit 1; }
rc=0
sleep_hits="$(sleep_unlisted <(sleep_counts "${sleep_files[@]}"))"
if [ -z "$sleep_hits" ]; then
    echo "PASS: sleep_counts match the listed bare sleeps (${#sleep_files[@]} test files)"
else
    printf '%s\n' "$sleep_hits"
    echo "FAIL: sleep_counts; wait with await (lib-wait.sh), or list the sleep with its reason in this file"
    rc=1
fi
# this file holds the synthetic offenders as text
scanned=(); for f in "${suites[@]}"; do [ "${f##*/}" = test-rm-guard.sh ] || scanned+=("$f"); done
for scan in pg_calls stub_pids_outside_m; do
    hits="$($scan "${scanned[@]}")"
    if [ -z "$hits" ]; then
        echo "PASS: $scan finds nothing in the hermetic suites"
    else
        printf '%s\n' "$hits" | sed "s#$REPO/##"
        echo "FAIL: $scan"
        rc=1
    fi
done
if [ -z "$bad" ]; then
    echo "PASS: every comm suite sources the home guard first (${#suites[@]} suites)"
else
    printf '%s\n' "$bad" | sed "s#^$REPO/##" | while IFS= read -r f; do
        echo "FAIL: $f does not source lib-home-guard.sh before any command but set"
    done
    rc=1
fi

# Execute await itself in a child shell. The local sleep records waits without
# delaying the case; no fixture changes PATH, SHELL or the function under test.
for want in 1 3 0; do
    bash -c '
        . "$1/lib-wait.sh" || exit 2
        calls=0; sleeps=0
        sleep() { [ "$1" = 0.05 ] || exit 2; sleeps=$((sleeps + 1)); }
        ready() { calls=$((calls + 1)); [ "$2" -gt 0 ] && [ "$calls" -eq "$2" ]; }
        await ready x "$2"; rc=$?
        case "$2" in
            1) expected="1 0 0" ;;
            3) expected="3 2 0" ;;
            0) expected="600 600 1" ;;
        esac
        got="$calls $sleeps $rc"
        [ "$got" = "$expected" ] || { echo "FAIL await: $got expected $expected"; exit 1; }
        [ "$got" != "0 0 0" ] || { echo "FAIL await sensitivity"; exit 1; }
        echo "PASS await: $got; corrupted expectation rejected"
    ' _ "$SCRIPT_DIR" "$want" || rc=1
done

# Execute end_group itself on the stub daemons' watcher: a background subshell running a pipeline, started between
# `set -m` and `set +m`. A row is STOP BODY RC LEFT: the stop (end_group, or `leader`, the base's kill of the subshell
# alone, which Git Bash's missing pkill left as the whole stop), the body (`term`, or `ignore`: the members ignore
# TERM), end_group's status, and whether a process of the group is left after the stop. The `leader` row is the check's
# sensitivity: it must see the pipeline that stop leaves (then ended here). `ignore` runs await on a sleep that does
# not wait, so its 600 tries pass at once.
while read -r stop body want_rc want_left; do
    bash -c '
        . "$1/lib-wait.sh" || exit 2
        stop="$2" body="$3" want_rc="$4" want_left="$5" ready="$6/ready-$2-$3"
        rm -f -- "$ready"
        [ "$body" = noleader ] || set -m
        if [ "$body" = noleader ]; then
            ( sleep 2 | { : > "$ready"; while read -r _; do :; done; } ) &
        elif [ "$body" = ignore ]; then
            ( trap "" TERM; sleep 30 | { : > "$ready"; while read -r _; do :; done; } ) &
        else
            ( sleep 30 | { : > "$ready"; while read -r _; do :; done; } ) &
        fi
        g=$!
        set +m
        await test -e "$ready" || { echo "FAIL end_group $stop $body: the pipeline never started"; kill -KILL -- "-$g" "$g"; exit 1; }
        [ "$body" != ignore ] || sleep() { :; }
        if [ "$stop" = leader ]; then kill "$g"; wait "$g" 2>/dev/null; rc=-; else end_group "$g"; rc=$?; fi
        unset -f sleep
        left=gone; group_gone "$g" || left=left
        kill -KILL -- "-$g" 2>/dev/null; kill -KILL "$g" 2>/dev/null
        got="$rc $left"
        [ "$got" = "$want_rc $want_left" ] || { echo "FAIL end_group $stop $body: $got expected $want_rc $want_left"; exit 1; }
        echo "PASS end_group $stop $body: $got"
    ' _ "$SCRIPT_DIR" "$stop" "$body" "$want_rc" "$want_left" "$T" || rc=1
done <<'ROWS'
end_group term 0 gone
end_group ignore 1 gone
end_group noleader 2 gone
leader term - left
ROWS
exit "$rc"

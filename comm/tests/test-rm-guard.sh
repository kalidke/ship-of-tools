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
[ "${#suites[@]}" -ge 29 ] || { echo "FATAL: found ${#suites[@]} comm suites, expected at least 29"; exit 1; }
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
[ "$(unguarded "$T/test-synthetic.sh")" = "$T/test-synthetic.sh" ] && [ -z "$(unguarded "$SCRIPT_DIR/test-hub-files.sh")" ] \
    || { echo "FAIL: the home-guard check does not tell test-hub-files.sh from a synthetic suite with no guard"; exit 1; }
bad="$(unguarded "${suites[@]}")"

# Two policies of the process-group rule (CLAUDE.md, timing): a hermetic suite runs no pkill or pgrep (Git Bash has
# neither), and a stub's pid (STUB_*_PID=$!) is taken between `set -m` and `set +m`, so end_group can end its group. The
# five peer-host suites are outside both, and test-comm-deps.sh's untethered case names its loop's child with pgrep, a
# known failure on Git Bash.
peer=(test-comm-e2e-readers test-inbox-lock-onehost test-inbox-lock-twohost test-registry-twohost test-registry-lock-twohost)
# pg_calls SUITE... — the lines that run pkill or pgrep as a command, in each suite that is hermetic.
pg_calls() {
    local f b
    for f in "$@"; do
        b="${f##*/}"; b="${b%.sh}"
        case " ${peer[*]} " in *" $b "*) continue ;; esac
        [ "$b" != test-comm-deps ] || continue
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
rc=0
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

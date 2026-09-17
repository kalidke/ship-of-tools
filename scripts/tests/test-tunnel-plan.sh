#!/usr/bin/env bash
# test-tunnel-plan.sh -- regression harness for scripts/sot-hosts.sh's
# sot_topology_plan (topology plan, lane D: `sotd topology plan [--self
# <host>]` is the one parser now; this reads its plain-line stdout).
#
# Pure text processing against a FAKE sotd (a stub script on PATH that
# echoes fixed lines) -- no real sotd binary, no ssh, no network.
# rust/protocol/src/topology.rs's `plan` doc comment is the contract this
# stub imitates; a reviewer may still adjust that format, so this parses
# it in ONE function (sot_topology_plan) and nowhere else. Bash-side
# sibling of scripts/tests/test-tunnel-plan.ps1, which exercises the same
# fixtures through Get-SotTopologyPlan.
#
# Run: scripts/tests/test-tunnel-plan.sh

set -euo pipefail

# shellcheck source=../sot-hosts.sh
. "$(dirname "$0")/../sot-hosts.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
fails=0

check() {  # <description> <expected> <actual>
    if [ "$2" = "$3" ]; then
        printf '    ok   %s\n' "$1"
    else
        printf '    FAIL %s\n      expected: %s\n      actual:   %s\n' "$1" "$2" "$3"
        fails=$((fails + 1))
    fi
}
case_start() { printf '  %s\n' "$1"; }

# new_fake_sotd <name> <line>...
# A stub `sotd` executable that ignores its own argv (`topology plan
# [--self <host>]`) and just echoes the given fixed lines -- a pure
# parser test needs nothing else.
new_fake_sotd() {
    local name="$1"; shift
    local path="$WORK/$name"
    # A quoted heredoc (`cat <<'SOT_PLAN_EOF'`) inside the stub's OWN body:
    # the fixture lines are emitted verbatim when the stub runs, no shell
    # interpretation at all (dash's echo, unlike bash's, interprets
    # backslash escapes even inside single quotes -- this sidesteps that
    # entirely rather than trying to out-quote it), so a Windows pipe path
    # fixture's backslashes and embedded space survive untouched.
    {
        echo '#!/bin/sh'
        echo "cat <<'SOT_PLAN_EOF'"
        local line
        for line in "$@"; do
            printf '%s\n' "$line"
        done
        echo 'SOT_PLAN_EOF'
    } > "$path"
    chmod +x "$path"
    printf '%s\n' "$path"
}

echo "=== a well-formed plan ==="
good_sotd="$(new_fake_sotd good \
    'self myserver' \
    'hub hub-box' \
    'relay-endpoint tcp:127.0.0.1:18743' \
    'dial hub-box tcp:127.0.0.1:18743' \
    'dial otherbox tcp:127.0.0.1:18744' \
    'tunnel hub-box 18743' \
    'tunnel otherbox 18744')"
plan="$(sot_topology_plan "$good_sotd" myserver)"

case_start "scalar fields"
check "self" "myserver" "$(sot_topology_field "$plan" SELF)"
check "hub" "hub-box" "$(sot_topology_field "$plan" HUB)"
check "relay-endpoint" "tcp:127.0.0.1:18743" "$(sot_topology_field "$plan" RELAY)"

case_start "dial/tunnel records"
check "two dials, in order" "hub-box,otherbox" \
    "$(printf '%s\n' "$plan" | awk -F'|' '$1=="DIAL"{printf "%s%s", sep, $2; sep=","}')"
check "otherbox dial endpoint" "tcp:127.0.0.1:18744" \
    "$(printf '%s\n' "$plan" | awk -F'|' '$1=="DIAL" && $2=="otherbox"{print $3}')"
check "hub-box tunnel port" "18743" \
    "$(printf '%s\n' "$plan" | awk -F'|' '$1=="TUNNEL" && $2=="hub-box"{print $3}')"
check "otherbox tunnel port" "18744" \
    "$(printf '%s\n' "$plan" | awk -F'|' '$1=="TUNNEL" && $2=="otherbox"{print $3}')"

echo "=== an endpoint containing a space (Windows pipe path, verbatim username) ==="
space_sotd="$(new_fake_sotd space \
    'self myserver' \
    'hub myserver' \
    'relay-endpoint pipe:\\.\pipe\sot-My User-sot' \
    'dial myserver pipe:\\.\pipe\sot-My User-sot')"
space_plan="$(sot_topology_plan "$space_sotd" myserver)"
check "relay-endpoint keeps its embedded space intact" 'pipe:\\.\pipe\sot-My User-sot' \
    "$(sot_topology_field "$space_plan" RELAY)"
check "dial endpoint keeps its embedded space intact (host not swallowed into it)" \
    'DIAL|myserver|pipe:\\.\pipe\sot-My User-sot' \
    "$(printf '%s\n' "$space_plan" | awk -F'|' '$1=="DIAL"')"

echo "=== the fake sotd resolved via \$PATH (not just an absolute path) ==="
pathdir="$WORK/pathbin"
mkdir -p "$pathdir"
cp "$good_sotd" "$pathdir/sotd"
chmod +x "$pathdir/sotd"
resolved="$(PATH="$pathdir:$PATH" command -v sotd)"
check "resolved via PATH lands in the fake bin dir" "$pathdir/sotd" "$resolved"
plan_via_path="$(sot_topology_plan "$resolved" myserver)"
check "plan parses the same via the PATH-resolved binary" "myserver" \
    "$(sot_topology_field "$plan_via_path" SELF)"

echo "=== an unknown first word is ignored, not an error ==="
future_sotd="$(new_fake_sotd future \
    'self myserver' \
    'hub hub-box' \
    'a-future-fact something new here' \
    'dial hub-box tcp:127.0.0.1:18743')"
future_plan="$(sot_topology_plan "$future_sotd" myserver)"
check "self/hub still parsed around it" "myserver hub-box" \
    "$(sot_topology_field "$future_plan" SELF) $(sot_topology_field "$future_plan" HUB)"
check "dial still parsed around it" "1" \
    "$(printf '%s\n' "$future_plan" | awk -F'|' '$1=="DIAL"' | wc -l | tr -d ' ')"

echo "=== no sotd binary at all ==="
if sot_topology_plan "$WORK/does-not-exist" myserver >/dev/null; then
    check "missing binary must fail" "fail" "ok (WRONG -- should have failed)"
else
    check "missing binary fails cleanly (not a crash)" "fail" "fail"
fi

echo "=== sot_topology_sync: the one side-effecting call ==="
# A stub that records its own argv and answers with a chosen exit code, so
# this asserts the exact command line the launcher hands sotd without a
# real binary, ssh or network -- same hermetic rule as the sections above.
new_fake_sync_sotd() {  # <name> <arg-log-file> <exit-code> <message>
    local path="$WORK/$1" arglog="$2" code="$3" msg="$4"
    {
        echo '#!/bin/sh'
        echo "echo \"\$@\" > \"$arglog\""
        echo "echo '$msg'"
        echo "exit $code"
    } > "$path"
    chmod +x "$path"
    printf '%s\n' "$path"
}

# Every call below is guarded by an `if`, never a bare assignment -- this
# file runs under `set -e`, and a plain `x="$(failing_cmd)"` would abort
# the whole suite right there instead of letting `check` report it.
ok_log="$WORK/sync-ok-args.txt"
ok_sotd="$(new_fake_sync_sotd sync-ok "$ok_log" 0 synced)"
if ok_out="$(sot_topology_sync "$ok_sotd" hub-box)"; then ok_rc=0; else ok_rc=$?; fi
check "a zero exit reports success" "0" "$ok_rc"
check "sotd is asked for exactly 'topology sync --hub <alias>'" "topology sync --hub hub-box" "$(cat "$ok_log")"
check "the command output is carried back to the caller" "synced" "$ok_out"

fail_log="$WORK/sync-fail-args.txt"
fail_sotd="$(new_fake_sync_sotd sync-fail "$fail_log" 1 "hub unreachable")"
if fail_out="$(sot_topology_sync "$fail_sotd" hub-box)"; then fail_rc=0; else fail_rc=$?; fi
check "a non-zero exit is a reported failure, never a crash" "1" "$fail_rc"
check "the failure reason is carried back to the caller" "hub unreachable" "$fail_out"

# An empty hub (no env override) still runs the sync -- plain `topology
# sync`, no --hub -- so sotd derives the hub from the LOCAL copy itself.
# This is the fix: sync must not depend on a plan having already
# succeeded to learn a hub (that's exactly what a stale/unlisted-self file
# cannot provide).
nohub_log="$WORK/sync-nohub-args.txt"
nohub_sotd="$(new_fake_sync_sotd sync-nohub "$nohub_log" 0 "synced from local hub")"
if nohub_out="$(sot_topology_sync "$nohub_sotd" "")"; then nohub_rc=0; else nohub_rc=$?; fi
check "no env hub: still runs, with no --hub (sotd reads the local copy)" "topology sync" "$(cat "$nohub_log")"
check "no env hub: still reports success" "0" "$nohub_rc"

if sot_topology_sync "$WORK/does-not-exist" hub-box >/dev/null 2>&1; then
    check "no sotd binary must fail" "fail" "ok (WRONG -- should have failed)"
else
    check "no sotd binary fails cleanly (not a crash)" "fail" "fail"
fi

echo
if [ "$fails" -eq 0 ]; then
    echo "test-tunnel-plan: all checks passed"
else
    echo "test-tunnel-plan: $fails check(s) FAILED"
    exit 1
fi

#!/usr/bin/env bash
# test-statusline-home-quota.sh — statusline.sh shows `home N%` (N the whole percent, rounded down) only when the running
# uid's use in the usage file is 90% or more of its limit, in red at 95% or more. A missing or unreadable file, a uid
# with no row, a limit of 0 and a malformed line show nothing. The file is the one $SOT_HOME_USAGE_FILE names.
#
# HERMETIC: its own work folder and fixtures; no daemon, no agent.
#
# Usage: agents/tests/test-statusline-home-quota.sh     Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
SCRIPT="$ROOT/agents/claude/sot-statusline-setup/statusline.sh"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/statusline-home-test-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "FATAL: mktemp failed" >&2; exit 1; }
trap 'chmod -R u+rw "$WORK" 2> /dev/null; rm -rf "${WORK:?}"' EXIT
command -v jq > /dev/null || { echo "FATAL: jq missing" >&2; exit 2; }
UID_NOW="$(id -u)"
PASS=0
FAIL=0
RED=$'\033[31mhome '

# Runs the script with usage file $1 (the variable unset when $1 is -); output in $WORK/out.
run() {
    local json='{"model":{"display_name":"M"},"session_id":"abcdefgh","workspace":{"current_dir":"'"$WORK"'"}}'
    if [ "$1" = - ]; then unset SOT_HOME_USAGE_FILE; else export SOT_HOME_USAGE_FILE="$1"; fi
    printf '%s' "$json" | bash "$SCRIPT" > "$WORK/out" 2> /dev/null
}
# Writes fixture $1 whose own row is "$2" (a row line without the uid, or "-" for no row).
fixture() {
    { printf 'home/shared-home\t12345\n'; printf '%s\t%s\n' 999999 '1000	2000'
      [ "$2" = - ] || printf '%s\t%s\n' "$UID_NOW" "$2"; } > "$WORK/$1"
}
shows() { run "$WORK/$1" && grep -q "home $2%" "$WORK/out"; }
shows_red() { run "$WORK/$1" && grep -qF "${RED}$2%" "$WORK/out"; }
silent() { run "$1" && ! grep -q 'home' "$WORK/out"; }
check() {
    local desc="$1"; shift
    if "$@"; then echo "PASS: $desc"; PASS=$((PASS + 1)); else echo "FAIL: $desc"; FAIL=$((FAIL + 1)); fi
}

fixture f89 '89	100'; check "89% shows nothing" silent "$WORK/f89"
fixture f899 '8999999999	10000000000'; check "89.99% shows nothing" silent "$WORK/f899"
fixture f90 '90	100'; check "90% shows home 90%" shows f90 90
check "90% is not red" bash -c '! grep -qF "$1" "$2"' _ "$RED" "$WORK/out"
fixture f949 '949	1000'; check "94.9% shows home 94%" shows f949 94
check "94.9% is not red" bash -c '! grep -qF "$1" "$2"' _ "$RED" "$WORK/out"
fixture f95 '95	100'; check "95% shows home 95% in red" shows_red f95 95
fixture f120 '120	100'; check "over the limit shows home 120% in red" shows_red f120 120
fixture big '9500000000000	10000000000000'; check "byte counts of terabytes show home 95%" shows_red big 95
check "a missing file shows nothing" silent "$WORK/none"
fixture norow -; check "a missing uid row shows nothing" silent "$WORK/norow"
fixture zero '95	0'; check "a zero limit shows nothing" silent "$WORK/zero"
fixture bad1 'abc	100'; check "a non-number used shows nothing" silent "$WORK/bad1"
fixture bad2 '95	x'; check "a non-number limit shows nothing" silent "$WORK/bad2"
fixture bad3 '95'; check "a row with a missing field shows nothing" silent "$WORK/bad3"
fixture bad4 '-5	100'; check "a negative used shows nothing" silent "$WORK/bad4"
fixture lz '095	100'; check "leading zeros are decimal" shows_red lz 95
fixture unread '95	100'; chmod 000 "$WORK/unread"
if [ -r "$WORK/unread" ]; then echo "SKIP: unreadable file (running as a user that reads anything)"
else check "an unreadable file shows nothing" silent "$WORK/unread"; fi
run "$WORK/f89"; check "the status line still prints its three lines" test "$(wc -l < "$WORK/out")" -eq 3

echo "PASS: $PASS  FAIL: $FAIL"
exit $((FAIL > 0))

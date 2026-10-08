#!/usr/bin/env bash
# Suite bootstrap policy and behavior of the actual shared await.
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

rc=0
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
exit "$rc"

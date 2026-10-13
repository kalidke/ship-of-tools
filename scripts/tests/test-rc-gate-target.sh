#!/usr/bin/env bash
# test-rc-gate-target.sh: rc-gate.sh names its checkout in its CARGO_TARGET_DIR and refuses that target to any other
# checkout before it builds anything (cargo keys a workspace crate by its path inside the workspace and trusts file
# times, so a second tree built into one target can run the first tree's binaries). It runs outside a test container,
# so every run that passes the claim stops at rc-gate's container check and nothing is built. Exit: failed checks.
set -u
gate=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/rc-gate.sh
work=$(mktemp -d "${TMPDIR:-/tmp}/rc-gate-target-XXXXXX") || exit 2
trap 'rm -rf "${work:?}"' EXIT
mkdir -p "$work/parent" "$work/head"
fail=0
check() { if eval "$2"; then echo "PASS: $1"; else echo "FAIL: $1 (rc=$rc: $out)"; fail=$((fail + 1)); fi; }
run() { out=$(CARGO_TARGET_DIR=$work/target bash "$gate" "$work/$1" "$work/logs-$1" 2>&1); rc=$?; }

run parent
check "the first run names its checkout in the target" '[ "$(cat "$work/target/rc-gate-checkout" 2> /dev/null)" = "$work/parent" ]'
run head
check "another checkout is refused before anything is built" '[ "$rc" = 2 ] && [[ $out == *"holds the build of $work/parent"* ]]'
check "the refusal leaves the claim as it was" '[ "$(cat "$work/target/rc-gate-checkout")" = "$work/parent" ]'
run parent
check "the same checkout passes the claim" '[[ $out != *"holds the build of"* ]]'
echo "failed: $fail"
exit "$fail"

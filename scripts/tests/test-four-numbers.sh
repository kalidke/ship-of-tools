#!/usr/bin/env bash
# test-four-numbers.sh: scripts/four-numbers.jl prints a version line, every subsystem row and a total at two fixed
# revisions (a25f3c39f and its parent), and `--names` lists the names implemented twice. Read-only; needs julia.
set -u
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$repo" || exit 2
subsystems="topology wire server lifecycle rows agents platform capsule vt100 messaging files sidecars pages fe-ui fe-net distribution records"
fail=0
for rev in a25f3c39f a25f3c39f^; do
    out=$(julia --startup-file=no scripts/four-numbers.jl --names "$rev" 2>&1) || { echo "FAIL $rev: exit $?"; echo "$out" | tail -5; fail=1; continue; }
    echo "$out" | head -1 | grep -q '^four-numbers ' || { echo "FAIL $rev: no version line"; fail=1; }
    for s in $subsystems total; do
        echo "$out" | grep -Eq "^$s +[0-9]+ +[0-9]+ +[0-9]+ +[0-9]+" || { echo "FAIL $rev: no row for $s"; fail=1; }
    done
    echo "$out" | grep -q '^names implemented twice: [0-9]' || { echo "FAIL $rev: no names list"; fail=1; }
    echo "$out" | grep -q '^cross-refs (baseline; contract-named calls come with the crate)' || { echo "FAIL $rev: no cross-refs label"; fail=1; }
done
[ "$fail" = 0 ] && echo "test-four-numbers: ok"
exit "$fail"

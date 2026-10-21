#!/usr/bin/env bash
# user-manager-tests.sh <checkout> <logdir> -- the local gate's second container: the five tests that need a systemd user
# manager, each run by name with SOT_TEST_REQUIRE_USER_MANAGER=1, so a missing manager fails it rather than skips it.
#
#   CARGO_TARGET_DIR=... scripts/tests/in-container.sh --user-manager user-manager-tests -- \
#       bash scripts/tests/user-manager-tests.sh <checkout> <logdir>
#
# rc-gate.sh runs every test in a container that reaches no user manager, where these five print SKIPPED and pass, and
# no hosted runner has one; until the cut stopped running the workspace suite, it was the one place they ran. Each
# test's rows run in systemd scopes of their own, outside the container, and the test ends them. Linux only. Reads
# CARGO_TARGET_DIR (required: not rc-gate's while that runs, since these builds replace the `sotd` and `sot-capsule` in
# it), HOME, PATH and the user manager's address; every SOT_ variable of the caller is dropped. Each test's log is
# <logdir>/<test binary>__<name>.log, and <logdir>/summary.txt holds one `<binary> <name> rc=<n>` line per test, then
# ALLDONE or ALLDONE FAILED. A test passes only when its exact body ran and passed (lib-test-body.sh). Exit: 0 when
# every test passed, 1 when any failed, 2 on bad arguments or setup.

SELF=$(readlink -f "$0")
source "$(dirname "$SELF")/lib-test-body.sh" || exit 2
TO=(timeout -k 10 1200)
TESTS=(
    "capsule_workspaces platform::capsule_supervisor_survives_a_real_user_service_stop"
    "capsule_workspaces platform::destroy_ends_a_child_that_left_the_agents_process_group"
    "window_start killed_after_capture_next_start_ends_scope"
    "window_lease lease::shutdown_ends_a_child_that_left_the_agents_process_group"
    "contained_job a_killed_test_job_leaves_no_row"
)

if [ $# -ne 2 ] || [ -z "${CARGO_TARGET_DIR:-}" ]; then
    echo "usage: CARGO_TARGET_DIR=... user-manager-tests.sh <checkout> <logdir>" >&2
    exit 2
fi
D=$(cd "$1" && pwd) || exit 2
systemctl --user show-environment > /dev/null 2>&1 ||
    { echo "user-manager-tests: no user manager answers: run it under in-container.sh --user-manager" >&2; exit 2; }
if [ -d "$2" ] && [ -n "$(ls -A "$2")" ]; then
    echo "user-manager-tests: logdir $2 exists and is not empty" >&2
    exit 2
fi
mkdir -p "$2" || exit 2
L=$(cd "$2" && pwd)
for v in $(compgen -v SOT_); do unset "$v"; done
export SOT_TEST_REQUIRE_USER_MANAGER=1
M=$D/rust/Cargo.toml

echo "head $(git -C "$D" rev-parse HEAD) tree $(git -C "$D" rev-parse 'HEAD^{tree}')" > "$L/summary.txt"
# The tests start the sot-capsule built beside the sotd their own build makes.
cargo build --manifest-path "$M" -p sot-log --bin sot-capsule --locked > "$L/build.log" 2>&1
r=$?
echo "build rc=$r" >> "$L/summary.txt"
failed=$r
if [ "$r" -eq 0 ]; then
    for row in "${TESTS[@]}"; do
        bin=${row%% *} name=${row#* }
        log=$L/${bin}__${name//:/_}.log
        "${TO[@]}" cargo test --manifest-path "$M" -p sot-backend --test "$bin" --locked -- \
            --exact "$name" --format pretty --color never --show-output --test-threads=1 > "$log" 2>&1
        r=$?
        if test_body_check "$name" "$r" "$log" 2>> "$log"; then r=0; else r=$?; [ "$r" -ne 0 ] || r=1; fi
        echo "$bin $name rc=$r" >> "$L/summary.txt"
        [ "$r" -eq 0 ] || failed=1
    done
fi
if [ "$failed" -eq 0 ]; then echo ALLDONE >> "$L/summary.txt"; else echo "ALLDONE FAILED" >> "$L/summary.txt"; fi
cat "$L/summary.txt"
[ "$failed" -eq 0 ] || exit 1
exit 0

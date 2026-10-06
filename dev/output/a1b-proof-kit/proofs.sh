#!/usr/bin/env bash
# Run only archived test-owned fixtures. The CI owner runs these outside the restricted builder sandbox.
set -euo pipefail
umask 022
mode=${1:?linux, macos, windows or checkpoint}
proof_root=${2:?absolute proof-tree root}
proof_root=$(python3 -c 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).resolve().as_posix())' "$proof_root")
head_sha=${3:?A1b commit}
mkdir -p "${CARGO_TARGET_DIR:?}/logs"
run() {
    local label=$1 tree=$2 expected=$3 needle=$4
    shift 4
    local log="$CARGO_TARGET_DIR/logs/a1b-$label.log" rc
    set +e
    ( cd "$proof_root/$tree/rust" && nice -n 10 timeout 9m env CARGO_NET_OFFLINE=true CARGO_PROFILE_DEV_DEBUG=line-tables-only GIT_CEILING_DIRECTORIES="$proof_root" SOT_BUILD_ID="a1b-$head_sha-$tree" cargo "$@" -j 8 --locked ) > "$log" 2>&1
    rc=$?
    set -e
    printf '%s exit %s expected %s\n' "$label" "$rc" "$expected"
    if [[ "$rc" != "$expected" ]]; then tail -n 65 "$log"; return 1; fi
    if [[ -n "$needle" ]] && ! grep -Fq -- "$needle" "$log"; then
        printf '%s missing expected runtime line: %s\n' "$label" "$needle"; tail -n 65 "$log"; return 1
    fi
    if [[ "$expected" == 101 ]] && ! grep -Fq 'test result: FAILED' "$log"; then
        printf '%s compilation or prerequisite failure is not assertion-red\n' "$label"; return 1
    fi
    grep -E 'test result:|body-proof |fixture-proof |admission-proof |cutoff-proof |checkpoint-proof |stack backtrace:' "$log" || true
}
# Libtest arguments have to follow --. Keep compiler options before that delimiter.
test_run() {
    local label=$1 tree=$2 expected=$3 needle=$4 crate=$5 target_kind=$6 target=$7 selection=$8
    local log="$CARGO_TARGET_DIR/logs/a1b-$label.log" rc
    set +e
    ( cd "$proof_root/$tree/rust" && nice -n 10 timeout 9m env CARGO_NET_OFFLINE=true CARGO_PROFILE_DEV_DEBUG=line-tables-only GIT_CEILING_DIRECTORIES="$proof_root" SOT_BUILD_ID="a1b-$head_sha-$tree" cargo test -p "$crate" "$target_kind" "$target" -j 8 --locked -- "$selection" --exact --nocapture ) > "$log" 2>&1
    rc=$?
    set -e
    printf '%s exit %s expected %s\n' "$label" "$rc" "$expected"
    [[ "$rc" == "$expected" ]] || { tail -n 65 "$log"; return 1; }
    grep -Fq -- "$needle" "$log" || { tail -n 65 "$log"; return 1; }
    if [[ "$expected" == 101 ]]; then grep -Fq 'test result: FAILED' "$log" || return 1; fi
    grep -E 'test result:|body-proof |fixture-proof |admission-proof |cutoff-proof |checkpoint-proof |stack backtrace:' "$log" || true
}
# --lib has no value; use the dedicated wrapper to avoid a spurious empty positional filter.
lib_test() {
    local label=$1 tree=$2 expected=$3 needle=$4 selection=$5 log="$CARGO_TARGET_DIR/logs/a1b-$1.log" rc
    set +e
    ( cd "$proof_root/$tree/rust" && nice -n 10 timeout 9m env CARGO_NET_OFFLINE=true CARGO_PROFILE_DEV_DEBUG=line-tables-only GIT_CEILING_DIRECTORIES="$proof_root" SOT_BUILD_ID="a1b-$head_sha-$tree" cargo test -p sot-log --lib -j 8 --locked -- "$selection" --exact --nocapture ) > "$log" 2>&1
    rc=$?
    set -e
    printf '%s exit %s expected %s\n' "$label" "$rc" "$expected"
    [[ "$rc" == "$expected" ]] || { tail -n 65 "$log"; return 1; }
    grep -Fq -- "$needle" "$log" || { tail -n 65 "$log"; return 1; }
    if [[ "$expected" == 101 ]]; then grep -Fq 'test result: FAILED' "$log" || return 1; fi
    grep -E 'test result:|body-proof |fixture-proof |admission-proof |cutoff-proof |checkpoint-proof |stack backtrace:' "$log" || true
}
socket_test() { test_run "$1" "$2" "$3" "$4" sot-log --test socket_unix "$5"; }
backend_test() { test_run "$1" "$2" "$3" "$4" sot-backend --bin sotd "$5"; }
if [[ "$mode" == checkpoint ]]; then
    for iteration in $(seq 1 300); do
        socket_test "checkpoint-$iteration" head 0 'test result: ok. 1 passed;' close::server_close_yields_client_eof_and_client_drop_yields_server_closed
        printf 'checkpoint iteration=%s sha=%s status=0\n' "$iteration" "$head_sha"
    done
    run checkpoint-siblings head 0 'test result: ok.' test -p sot-log --test socket_unix
    printf 'checkpoint sha=%s completed=300 cause=open\n' "$head_sha"
    exit 0
fi
# Actual listener-owner preservation on identical native cases at A and A1b.
for tree in parent-admission head; do
    for case in a_connection_the_owner_check_refuses_reaches_no_handler foreign_or_unknown_owner_is_refused_before_handler; do
        lib_test "$tree-page-$case" "$tree" 0 'test result: ok. 1 passed;' "identity::peer_owner::tests::$case"
    done
    if [[ "$mode" == windows ]]; then
        for case in session_pipe_descriptor_is_protected_owner_only_with_no_container_inherit_flags session_pipe_denies_a_token_without_the_owner_grant; do
            backend_test "$tree-$case" "$tree" 0 'test result: ok. 1 passed;' "server::listen::tests::$case"
        done
        for case in pipe_descriptor_is_protected_owner_only_with_no_container_inherit_flags pipe_denies_a_token_without_the_owner_grant; do
            test_run "$tree-$case" "$tree" 0 'test result: ok. 1 passed;' sot-log --test pipe_win "connect::$case"
        done
    else
        backend_test "$tree-session" "$tree" 0 'test result: ok. 1 passed;' server::listen::tests::a_refused_os_peer_reaches_no_connection_handler
        backend_test "$tree-kernel-peer" "$tree" 0 'test result: ok. 1 passed;' server::listen::tests::admit_peer_admits_this_process_with_its_pid_and_creation_time
        for case in socket_is_owner_only_in_a_private_dir foreign_account_cannot_reach_the_lane; do
            socket_test "$tree-$case" "$tree" 0 'test result: ok. 1 passed;' "connect::$case"
        done
        test_run "$tree-page-native" "$tree" 0 'admission-proof test=' sot-log --test other_account a_client_of_another_account_gets_no_byte
    fi
done
lib_test revert-page revert-page 101 'refused owner reached handler' identity::peer_owner::tests::foreign_or_unknown_owner_is_refused_before_handler
if [[ "$mode" == windows ]]; then
    backend_test revert-session-pipe revert-pipe 101 'pipe opened for a token without the owner grant' server::listen::tests::session_pipe_denies_a_token_without_the_owner_grant
    test_run revert-capsule-pipe revert-pipe 101 'pipe opened for a token without the owner grant' sot-log --test pipe_win connect::pipe_denies_a_token_without_the_owner_grant
    run windows-warning-green head 0 'Finished' check -p sot-log --lib --features test-support
    if grep -E 'field `started` is never read|method `note` is never used' "$CARGO_TARGET_DIR/logs/a1b-windows-warning-green.log"; then exit 1; fi
    run windows-warning-reversal revert-windows-scope 0 'method `note` is never used' check -p sot-log --lib --features test-support
    grep -Fq 'field `started` is never read' "$CARGO_TARGET_DIR/logs/a1b-windows-warning-reversal.log"
    run windows-pipe-siblings head 0 'test result: ok.' test -p sot-log --test pipe_win
else
    backend_test revert-session revert-session 101 'refused OS peer reached a connection handler' server::listen::tests::a_refused_os_peer_reaches_no_connection_handler
    socket_test revert-private revert-private 101 'foreign account connected to private lane' connect::foreign_account_cannot_reach_the_lane
    for tree in parent-busy head revert-busy; do
        expected=101; needle='checkpoint admission waited for the busy recorder'
        if [[ "$tree" == head ]]; then expected=0; needle='test result: ok. 1 passed;'; fi
        lib_test "$tree-recorder" "$tree" "$expected" "$needle" lane::test_progress::tests::busy_checkpoint_admission_is_skipped_and_counted
        needle='transport stopped while recorder was busy'
        if [[ "$tree" == head ]]; then needle='test result: ok. 1 passed;'; fi
        lib_test "$tree-transport" "$tree" "$expected" "$needle" lane::socket_unix::server::tests::transport_progresses_while_recorder_is_busy
    done
    for tree in parent-deadline head revert-deadline; do
        expected=101; needle='child deadline was recomputed'
        if [[ "$tree" == head ]]; then expected=0; needle='test result: ok. 1 passed;'; fi
        socket_test "$tree-deadline" "$tree" "$expected" "$needle" diagnostics::deadline_adapters_preserve_origin_and_outcome
    done
    for tree in parent-io head revert-io; do
        expected=101; needle='I/O timeout missing emitted progress snapshot'
        if [[ "$tree" == head ]]; then expected=0; needle='test result: ok. 1 passed;'; fi
        socket_test "$tree-io" "$tree" "$expected" "$needle" diagnostics::io_timeout_with_server_reports_snapshot
    done
    for tree in parent-retention head revert-retention; do
        expected=101; needle='snapshot unavailable after Closed'
        if [[ "$tree" == head ]]; then expected=0; needle='test result: ok. 1 passed;'; fi
        for case in progress_survives_connection_removal snapshot_poll_waits_for_available_history; do
            socket_test "$tree-$case" "$tree" "$expected" "$needle" "diagnostics::$case"
        done
    done
    for tree in parent-output head; do
        for case in begin_is_visible_before_blocking absent_event_reports_named_wait unrelated_event_reports_named_wait outcomes_distinguish_disconnect_success_and_io_error child_cutoff_preserves_last_begin; do
            socket_test "$tree-$case" "$tree" 0 'test result: ok. 1 passed;' "diagnostics::$case"
        done
    done
    socket_test emission-reversal revert-emission 101 'child begin was not visible before release' diagnostics::begin_is_visible_before_blocking
    # Deadline assertion-red occurs in the actual ISO child; capture prints its stderr before parent status failure.
    grep -Fq 'child deadline was recomputed' "$CARGO_TARGET_DIR/logs/a1b-parent-deadline-deadline.log"
    grep -Fq 'stack backtrace:' "$CARGO_TARGET_DIR/logs/a1b-parent-deadline-deadline.log"
    grep -Fq 'body-proof test=diagnostics::deadline_adapters_preserve_origin_and_outcome' "$CARGO_TARGET_DIR/logs/a1b-parent-deadline-deadline.log"
    printf 'backtrace-proof sha=%s failing-child-stderr=observed\n' "$head_sha"
    socket_test busy-snapshot head 0 'test result: ok. 1 passed;' diagnostics::snapshot_poll_times_out_when_busy
    run recorder-siblings head 0 'test result: ok.' test -p sot-log --lib lane::test_progress::tests
    run socket-siblings head 0 'test result: ok.' test -p sot-log --test socket_unix
fi
# R15 preservation and its independent entry-enforcement negative control.
for tree in parent-entry head; do
    for case in a_real_body_enters_exactly_once an_unqualified_name_fails_as_a_body_that_did_not_enter a_misspelled_name_fails_as_a_body_that_did_not_enter a_role_that_selects_no_test_fails_as_a_body_that_did_not_enter an_entry_dropped_unchecked_fails a_child_that_fills_its_pipe_is_drained_and_finishes a_stalled_child_fails_at_its_bound output_a_descendant_holds_fails_at_the_bound; do
        lib_test "$tree-$case" "$tree" 0 'test result: ok. 1 passed;' "test_isolated::tests::$case"
    done
done
lib_test entry-reversal revert-entry 101 'a check that must fail passed' test_isolated::tests::a_role_that_selects_no_test_fails_as_a_body_that_did_not_enter
for tree in parent-iso-deadline head revert-iso-deadline; do
    expected=101; needle='child deadline was recomputed'
    if [[ "$tree" == head ]]; then expected=0; needle='test result: ok. 1 passed;'; fi
    lib_test "$tree-origin" "$tree" "$expected" "$needle" test_isolated::tests::wait_until_retains_a_spent_deadline_and_confirms_expiry
done
run iso-siblings head 0 'test result: ok.' test -p sot-log --lib test_isolated::tests
# The existing native challenge suites retain all ordinary sibling controls.
if [[ "$mode" == linux ]]; then
    run challenge-unix head 0 'test result: ok.' test -p sot-log --test challenge_unix
elif [[ "$mode" == macos ]]; then
    run challenge-macos head 0 'test result: ok.' test -p sot-log --test challenge_macos
fi
run macos-kernel head 0 'test result: ok.' test -p sot-log --test macos_kernel_facts
printf 'matrix sha=%s platform=%s complete\n' "$head_sha" "$mode"
